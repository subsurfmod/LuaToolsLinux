//! Streaming binary reader & writer for Steam's `appcache/appinfo.vdf` (v27, v28, and v29).

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};

pub const MAGIC_27: u32 = 0x0756_4427;
pub const MAGIC_28: u32 = 0x0756_4428;
pub const MAGIC_29: u32 = 0x0756_4429;

pub const TYPE_TABLE: u8 = 0x00;
pub const TYPE_STRING: u8 = 0x01;
pub const TYPE_INT32: u8 = 0x02;
pub const TYPE_FLOAT: u8 = 0x03;
pub const TYPE_POINTER: u8 = 0x04;
pub const TYPE_WSTRING: u8 = 0x05;
pub const TYPE_COLOR: u8 = 0x06;
pub const TYPE_UINT64: u8 = 0x07;
pub const TYPE_END: u8 = 0x08;
pub const TYPE_INT64: u8 = 0x0A;
pub const TYPE_END_ALT: u8 = 0x0B;

#[derive(Debug, Clone)]
pub enum VdfValue {
    Table(VdfTable),
    Bytes(Vec<u8>),
    WBytes(Vec<u8>),
    Int32(i32),
    Float(f32),
    UInt64(u64),
    Int64(i64),
}

#[derive(Debug, Clone)]
pub struct VdfProperty {
    pub name: String,
    pub raw_name: Option<Vec<u8>>,
    pub vtype: u8,
    pub value: VdfValue,
}

impl VdfProperty {
    pub fn raw_name_bytes(&self) -> &[u8] {
        self.raw_name
            .as_deref()
            .unwrap_or_else(|| self.name.as_bytes())
    }
}

#[derive(Debug, Clone, Default)]
pub struct VdfTable {
    pub items: Vec<VdfProperty>,
}

impl VdfTable {
    pub fn find(&self, name: &str) -> Option<&VdfProperty> {
        self.items
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }

    pub fn find_mut(&mut self, name: &str) -> Option<&mut VdfProperty> {
        self.items
            .iter_mut()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }

    pub fn get_table(&self, name: &str) -> Option<&VdfTable> {
        match &self.find(name)?.value {
            VdfValue::Table(t) => Some(t),
            _ => None,
        }
    }

    pub fn get_table_mut(&mut self, name: &str) -> Option<&mut VdfTable> {
        match &mut self.find_mut(name)?.value {
            VdfValue::Table(t) => Some(t),
            _ => None,
        }
    }

    pub fn ensure_table(&mut self, name: &str) -> &mut VdfTable {
        if let Some(idx) = self.items.iter().position(|p| {
            p.name.eq_ignore_ascii_case(name) && matches!(p.value, VdfValue::Table(_))
        }) {
            match &mut self.items[idx].value {
                VdfValue::Table(t) => return t,
                _ => unreachable!(),
            }
        }
        self.items.push(VdfProperty {
            name: name.to_string(),
            raw_name: None,
            vtype: TYPE_TABLE,
            value: VdfValue::Table(VdfTable::default()),
        });
        match &mut self.items.last_mut().unwrap().value {
            VdfValue::Table(t) => t,
            _ => unreachable!(),
        }
    }

    pub fn get_text(&self, name: &str) -> String {
        let Some(p) = self.find(name) else {
            return String::new();
        };
        match &p.value {
            VdfValue::Bytes(b) => String::from_utf8_lossy(b).to_string(),
            VdfValue::Int32(v) => v.to_string(),
            VdfValue::UInt64(v) => v.to_string(),
            VdfValue::Int64(v) => v.to_string(),
            _ => String::new(),
        }
    }

    pub fn set_text(&mut self, name: &str, value: &str) {
        if value.is_empty() {
            self.remove(name);
            return;
        }
        let raw = value.as_bytes().to_vec();
        if let Some(prop) = self.find_mut(name) {
            prop.vtype = TYPE_STRING;
            prop.value = VdfValue::Bytes(raw);
        } else {
            self.items.push(VdfProperty {
                name: name.to_string(),
                raw_name: None,
                vtype: TYPE_STRING,
                value: VdfValue::Bytes(raw),
            });
        }
    }

    pub fn remove(&mut self, name: &str) {
        self.items.retain(|p| !p.name.eq_ignore_ascii_case(name));
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchEntry {
    pub index: String,
    pub executable: String,
    pub arguments: String,
    pub workingdir: String,
    pub description: String,
    pub launch_type: String,
    pub oslist: String,
    pub osarch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beta_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owns_dlc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_index: Option<usize>,
}

/// Alias matching canonical LT-55 `LaunchOption` naming.
pub type LaunchOption = LaunchEntry;

fn read_u32_le<R: Read>(r: &mut R) -> Result<u32, String> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).map_err(|e| e.to_string())?;
    Ok(u32::from_le_bytes(b))
}

fn read_i32_le<R: Read>(r: &mut R) -> Result<i32, String> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).map_err(|e| e.to_string())?;
    Ok(i32::from_le_bytes(b))
}

fn read_u64_le<R: Read>(r: &mut R) -> Result<u64, String> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b).map_err(|e| e.to_string())?;
    Ok(u64::from_le_bytes(b))
}

fn read_i64_le<R: Read>(r: &mut R) -> Result<i64, String> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b).map_err(|e| e.to_string())?;
    Ok(i64::from_le_bytes(b))
}

fn read_f32_le<R: Read>(r: &mut R) -> Result<f32, String> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).map_err(|e| e.to_string())?;
    Ok(f32::from_le_bytes(b))
}

fn read_cstring_bytes<R: Read>(r: &mut R) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match r.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                if byte[0] == 0 {
                    break;
                }
                out.push(byte[0]);
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(out)
}

fn read_wstring_bytes<R: Read>(r: &mut R) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut pair = [0u8; 2];
    loop {
        if r.read_exact(&mut pair).is_err() || pair == [0, 0] {
            break;
        }
        out.extend_from_slice(&pair);
    }
    Ok(out)
}

fn read_vdf<R: Read>(
    r: &mut R,
    strings: &[String],
    string_bytes: &[Vec<u8>],
    indexed_keys: bool,
) -> Result<VdfTable, String> {
    let mut table = VdfTable::default();
    let mut marker_buf = [0u8; 1];

    loop {
        match r.read(&mut marker_buf) {
            Ok(0) => return Ok(table),
            Ok(_) => {}
            Err(e) => return Err(e.to_string()),
        }
        let marker = marker_buf[0];
        if marker == TYPE_END || marker == TYPE_END_ALT {
            return Ok(table);
        }

        let (name, raw_name) = if indexed_keys {
            let idx = read_u32_le(r)? as usize;
            let s = strings
                .get(idx)
                .cloned()
                .ok_or_else(|| format!("VDF string index {} out of bounds", idx))?;
            let raw = string_bytes
                .get(idx)
                .cloned()
                .ok_or_else(|| format!("VDF string bytes index {} out of bounds", idx))?;
            (s, Some(raw))
        } else {
            let raw_key = read_cstring_bytes(r)?;
            let s = String::from_utf8_lossy(&raw_key).into_owned();
            (s, Some(raw_key))
        };

        let value = match marker {
            TYPE_TABLE => VdfValue::Table(read_vdf(r, strings, string_bytes, indexed_keys)?),
            TYPE_STRING => VdfValue::Bytes(read_cstring_bytes(r)?),
            TYPE_WSTRING => VdfValue::WBytes(read_wstring_bytes(r)?),
            TYPE_INT32 | TYPE_POINTER | TYPE_COLOR => VdfValue::Int32(read_i32_le(r)?),
            TYPE_UINT64 => VdfValue::UInt64(read_u64_le(r)?),
            TYPE_INT64 => VdfValue::Int64(read_i64_le(r)?),
            TYPE_FLOAT => VdfValue::Float(read_f32_le(r)?),
            other => return Err(format!("Unsupported VDF type 0x{:02X}", other)),
        };

        table.items.push(VdfProperty {
            name,
            raw_name,
            vtype: marker,
            value,
        });
    }
}

fn write_vdf<F>(buf: &mut Vec<u8>, table: &VdfTable, intern_fn: &mut F, indexed_keys: bool)
where
    F: FnMut(&[u8]) -> u32,
{
    for p in &table.items {
        buf.push(p.vtype);
        let raw_key = p.raw_name_bytes();
        if indexed_keys {
            buf.extend_from_slice(&intern_fn(raw_key).to_le_bytes());
        } else {
            buf.extend_from_slice(raw_key);
            buf.push(0);
        }

        match &p.value {
            VdfValue::Table(sub) => write_vdf(buf, sub, intern_fn, indexed_keys),
            VdfValue::Bytes(b) => {
                buf.extend_from_slice(b);
                buf.push(0);
            }
            VdfValue::WBytes(b) => {
                buf.extend_from_slice(b);
                buf.extend_from_slice(&[0, 0]);
            }
            VdfValue::Int32(v) => buf.extend_from_slice(&v.to_le_bytes()),
            VdfValue::UInt64(v) => buf.extend_from_slice(&v.to_le_bytes()),
            VdfValue::Int64(v) => buf.extend_from_slice(&v.to_le_bytes()),
            VdfValue::Float(v) => buf.extend_from_slice(&v.to_le_bytes()),
        }
    }
    buf.push(TYPE_END);
}

fn non_empty_trimmed(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn extract_launch_entries_from_table(launch: &VdfTable) -> Vec<LaunchEntry> {
    let mut out = Vec::new();
    for (slot_pos, prop) in launch.items.iter().enumerate() {
        if let VdfValue::Table(t) = &prop.value {
            let cfg = t.get_table("config");
            let lt = t.get_text("type");
            let beta_key = cfg
                .and_then(|c| non_empty_trimmed(c.get_text("betakey")))
                .or_else(|| non_empty_trimmed(t.get_text("BetaKey")));
            let owns_dlc = cfg
                .and_then(|c| non_empty_trimmed(c.get_text("ownsdlc")))
                .or_else(|| non_empty_trimmed(t.get_text("ownsdlc")));
            let source_index = prop.name.parse::<usize>().ok().or(Some(slot_pos));
            out.push(LaunchEntry {
                index: prop.name.clone(),
                executable: t.get_text("executable"),
                arguments: t.get_text("arguments"),
                workingdir: t.get_text("workingdir"),
                description: t.get_text("description"),
                launch_type: if lt.is_empty() {
                    "default".to_string()
                } else {
                    lt
                },
                oslist: cfg.map(|c| c.get_text("oslist")).unwrap_or_default(),
                osarch: cfg.map(|c| c.get_text("osarch")).unwrap_or_default(),
                beta_key,
                owns_dlc,
                source_index,
            });
        }
    }
    out
}

pub struct AppInfoFile {
    pub path: PathBuf,
    pub magic: u32,
    pub universe: u32,
    pub strings: Vec<String>,
    string_bytes: Vec<Vec<u8>>,
    index_order: Vec<u32>,
    index_map: HashMap<u32, (u64, u32)>,
}

impl AppInfoFile {
    pub fn open(path: &Path) -> Result<Self, String> {
        let f = File::open(path).map_err(|e| e.to_string())?;
        let mut r = BufReader::new(f);

        let magic = read_u32_le(&mut r)?;
        let universe = read_u32_le(&mut r)?;
        if magic != MAGIC_27 && magic != MAGIC_28 && magic != MAGIC_29 {
            return Err(format!("Unsupported appinfo.vdf magic 0x{:08X}", magic));
        }

        let indexed_keys = magic >= MAGIC_29;
        let meta_len = if magic >= MAGIC_28 { 60u32 } else { 40u32 };
        let mut strings = Vec::new();
        let mut string_bytes = Vec::new();

        if indexed_keys {
            let table_offset_raw = read_i64_le(&mut r)?;
            if table_offset_raw < 0 {
                return Err(format!(
                    "Corrupt appinfo.vdf negative string table offset: {}",
                    table_offset_raw
                ));
            }
            let table_offset = table_offset_raw as u64;
            let resume = r.stream_position().map_err(|e| e.to_string())?;
            r.seek(SeekFrom::Start(table_offset))
                .map_err(|e| e.to_string())?;
            let count = read_u32_le(&mut r)? as usize;
            if count > 2_000_000 {
                return Err(format!(
                    "Corrupt appinfo.vdf string table count ({}) exceeds safety limit",
                    count
                ));
            }
            strings.reserve(count.min(65_536));
            string_bytes.reserve(count.min(65_536));
            for _ in 0..count {
                let raw = read_cstring_bytes(&mut r)?;
                strings.push(String::from_utf8_lossy(&raw).to_string());
                string_bytes.push(raw);
            }
            r.seek(SeekFrom::Start(resume)).map_err(|e| e.to_string())?;
        }

        let mut index_order = Vec::new();
        let mut index_map = HashMap::new();
        loop {
            let offset = r.stream_position().map_err(|e| e.to_string())?;
            let Ok(app_id) = read_u32_le(&mut r) else {
                break;
            };
            if app_id == 0 {
                break;
            }
            let size = read_u32_le(&mut r)?;
            if size < meta_len {
                return Err(format!(
                    "Corrupt appinfo.vdf entry for AppID {} (size {} < {})",
                    app_id, size, meta_len
                ));
            }
            index_order.push(app_id);
            index_map.insert(app_id, (offset, size));
            r.seek(SeekFrom::Start(offset + 8 + (size as u64)))
                .map_err(|e| e.to_string())?;
        }

        Ok(Self {
            path: path.to_path_buf(),
            magic,
            universe,
            strings,
            string_bytes,
            index_order,
            index_map,
        })
    }

    pub fn indexed_keys(&self) -> bool {
        self.magic >= MAGIC_29
    }

    pub fn meta_length(&self) -> usize {
        if self.magic >= MAGIC_28 {
            60
        } else {
            40
        }
    }

    /// Extracts the PICS change number for a specific app from appinfo.vdf.
    /// Per-app metadata layout (v27 = 40 bytes, v28/v29 = 60 bytes):
    ///   [0..4]   info_state (u32)
    ///   [4..8]   last_updated (u32)
    ///   [8..16]  pics_token (u64)
    ///   [16..36] sha1_text ([u8; 20])
    ///   [36..40] change_number (u32)
    ///   [40..60] sha1_binary ([u8; 20], v28/v29)
    pub fn extract_app_change_number(path: &Path, app_id: u32) -> Option<u64> {
        let af = Self::open(path).ok()?;
        let &(offset, _size) = af.index_map.get(&app_id)?;
        let meta_len = af.meta_length();
        if meta_len < 40 {
            return None;
        }
        let mut f = File::open(path).ok()?;
        f.seek(SeekFrom::Start(offset + 8)).ok()?;
        let mut meta = vec![0u8; meta_len];
        f.read_exact(&mut meta).ok()?;
        Some(u32::from_le_bytes(meta[36..40].try_into().ok()?) as u64)
    }

    /// Extracts launch options for a specific app from appinfo.vdf.
    /// Returns `None` if `appinfo.vdf` cannot be read or `app_id` is absent,
    /// and `Some(Vec::new())` if the app exists without a `config/launch` table.
    pub fn extract_launch_options(path: &Path, app_id: u32) -> Option<Vec<LaunchEntry>> {
        let af = Self::open(path).ok()?;
        let root = af.read_app(app_id).ok()??;
        let body = root.get_table("appinfo").unwrap_or(&root);
        let Some(config) = body.get_table("config") else {
            return Some(Vec::new());
        };
        let Some(launch) = config.get_table("launch") else {
            return Some(Vec::new());
        };
        Some(extract_launch_entries_from_table(launch))
    }

    pub fn read_app(&self, app_id: u32) -> Result<Option<VdfTable>, String> {
        let Some(&(offset, size)) = self.index_map.get(&app_id) else {
            return Ok(None);
        };
        let meta_len = self.meta_length();
        let payload_len = (size as usize).saturating_sub(meta_len);
        let mut f = File::open(&self.path).map_err(|e| e.to_string())?;
        f.seek(SeekFrom::Start(offset + 8 + (meta_len as u64)))
            .map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; payload_len];
        f.read_exact(&mut buf).map_err(|e| e.to_string())?;
        let mut cur = Cursor::new(buf);
        let tbl = read_vdf(&mut cur, &self.strings, &self.string_bytes, self.indexed_keys())?;
        Ok(Some(tbl))
    }

    pub fn save_with_edits(
        &self,
        dest: &Path,
        edits: &HashMap<u32, VdfTable>,
    ) -> Result<(), String> {
        let mut strings_bytes = self.string_bytes.clone();
        // LT-54: Key v29 string-pool lookup by exact raw bytes so distinct non-UTF-8 keys never
        // collide or get lossily replaced with UTF-8 U+FFFD bytes on re-serialization.
        let mut lookup: HashMap<Vec<u8>, u32> = HashMap::with_capacity(self.string_bytes.len());
        for (i, raw) in self.string_bytes.iter().enumerate() {
            lookup.entry(raw.clone()).or_insert(i as u32);
        }

        let mut intern = |key_bytes: &[u8]| -> u32 {
            if let Some(&idx) = lookup.get(key_bytes) {
                return idx;
            }
            let idx = strings_bytes.len() as u32;
            let owned = key_bytes.to_vec();
            lookup.insert(owned.clone(), idx);
            strings_bytes.push(owned);
            idx
        };

        let mut src = BufReader::new(File::open(&self.path).map_err(|e| e.to_string())?);
        let mut out = BufWriter::new(File::create(dest).map_err(|e| e.to_string())?);

        out.write_all(&self.magic.to_le_bytes())
            .map_err(|e| e.to_string())?;
        out.write_all(&self.universe.to_le_bytes())
            .map_err(|e| e.to_string())?;

        let table_ptr_pos = out.stream_position().map_err(|e| e.to_string())?;
        if self.indexed_keys() {
            out.write_all(&0i64.to_le_bytes())
                .map_err(|e| e.to_string())?;
        }

        let meta_len = self.meta_length();
        for &app_id in &self.index_order {
            let (offset, size) = self.index_map[&app_id];
            if let Some(edited_table) = edits.get(&app_id) {
                src.seek(SeekFrom::Start(offset + 8))
                    .map_err(|e| e.to_string())?;
                let mut meta = vec![0u8; meta_len];
                src.read_exact(&mut meta).map_err(|e| e.to_string())?;

                let mut blob = Vec::new();
                write_vdf(&mut blob, edited_table, &mut intern, self.indexed_keys());

                if meta.len() >= 60 {
                    let mut hasher = Sha1::new();
                    hasher.update(&blob);
                    let digest = hasher.finalize();
                    meta[40..60].copy_from_slice(&digest);
                }

                let new_size = (meta.len() + blob.len()) as u32;
                out.write_all(&app_id.to_le_bytes())
                    .map_err(|e| e.to_string())?;
                out.write_all(&new_size.to_le_bytes())
                    .map_err(|e| e.to_string())?;
                out.write_all(&meta).map_err(|e| e.to_string())?;
                out.write_all(&blob).map_err(|e| e.to_string())?;
            } else {
                src.seek(SeekFrom::Start(offset))
                    .map_err(|e| e.to_string())?;
                let mut raw = vec![0u8; 8 + (size as usize)];
                src.read_exact(&mut raw).map_err(|e| e.to_string())?;
                out.write_all(&raw).map_err(|e| e.to_string())?;
            }
        }

        out.write_all(&0u32.to_le_bytes())
            .map_err(|e| e.to_string())?;

        if self.indexed_keys() {
            let table_pos = out.stream_position().map_err(|e| e.to_string())? as i64;
            out.write_all(&(strings_bytes.len() as u32).to_le_bytes())
                .map_err(|e| e.to_string())?;
            for s in &strings_bytes {
                out.write_all(s).map_err(|e| e.to_string())?;
                out.write_all(&[0]).map_err(|e| e.to_string())?;
            }
            out.seek(SeekFrom::Start(table_ptr_pos))
                .map_err(|e| e.to_string())?;
            out.write_all(&table_pos.to_le_bytes())
                .map_err(|e| e.to_string())?;
        }

        out.flush().map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct LaunchOptionsService {
    pub steam_root: PathBuf,
}

impl LaunchOptionsService {
    pub fn new(steam_root: PathBuf) -> Self {
        Self { steam_root }
    }

    pub fn appinfo_path(&self) -> PathBuf {
        self.steam_root.join("appcache").join("appinfo.vdf")
    }

    pub fn backup_path(&self) -> PathBuf {
        self.steam_root
            .join("appcache")
            .join("appinfo.vdf.luatools.bak")
    }

    pub fn get_launch_entries(&self, app_id: u32) -> Vec<LaunchEntry> {
        let p = self.appinfo_path();
        if !p.exists() {
            return Vec::new();
        }
        let Ok(af) = AppInfoFile::open(&p) else {
            return Vec::new();
        };
        let Ok(Some(root)) = af.read_app(app_id) else {
            return Vec::new();
        };
        let body = root.get_table("appinfo").unwrap_or(&root);
        let Some(config) = body.get_table("config") else {
            return Vec::new();
        };
        let Some(launch) = config.get_table("launch") else {
            return Vec::new();
        };

        extract_launch_entries_from_table(launch)
    }

    pub fn save_launch_entries(
        &self,
        app_id: u32,
        entries: &[LaunchEntry],
    ) -> Result<String, String> {
        let appinfo = self.appinfo_path();
        if !appinfo.exists() {
            return Err("appinfo.vdf not found.".to_string());
        }
        let af = AppInfoFile::open(&appinfo)?;
        let mut root = af
            .read_app(app_id)?
            .ok_or_else(|| format!("AppID {} not found in appinfo.vdf.", app_id))?;

        {
            let has_appinfo_sub = root.get_table("appinfo").is_some();
            let body = if has_appinfo_sub {
                root.ensure_table("appinfo")
            } else {
                &mut root
            };
            let config = body.ensure_table("config");
            let launch = config.ensure_table("launch");

            // LT-55: Snapshot existing launch slot tables keyed by original slot index so
            // matching by `source_index` preserves unmodelled keys (`vacmodulefilename`,
            // `description_loc`, etc.) accurately even across reorders and deletions without
            // cross-contaminating adjacent slots.
            let mut existing_tables: HashMap<usize, VdfTable> = HashMap::new();
            for (pos, prop) in launch.items.iter().enumerate() {
                if let VdfValue::Table(t) = &prop.value {
                    let slot_idx = prop.name.parse::<usize>().unwrap_or(pos);
                    existing_tables.insert(slot_idx, t.clone());
                }
            }

            let mut new_items: Vec<VdfProperty> = Vec::with_capacity(entries.len());
            for (idx, e) in entries.iter().enumerate() {
                let mut slot_table = e
                    .source_index
                    .and_then(|src| existing_tables.get(&src).cloned())
                    .unwrap_or_default();

                slot_table.set_text("executable", e.executable.trim());
                slot_table.set_text("arguments", e.arguments.trim());
                slot_table.set_text("workingdir", e.workingdir.trim());
                slot_table.set_text("description", e.description.trim());
                let ltype = if e.launch_type.trim().is_empty() {
                    "default"
                } else {
                    e.launch_type.trim()
                };
                slot_table.set_text("type", ltype);

                // Explicitly update OR remove oslist / osarch when cleared
                let oslist = e.oslist.trim();
                let osarch = e.osarch.trim();
                if !oslist.is_empty() || !osarch.is_empty() {
                    let cfg = slot_table.ensure_table("config");
                    cfg.set_text("oslist", oslist);
                    cfg.set_text("osarch", osarch);
                } else if let Some(cfg) = slot_table.get_table_mut("config") {
                    cfg.remove("oslist");
                    cfg.remove("osarch");
                }

                // BetaKey: preserve existing location (root vs config) or store in config; remove if cleared
                let beta_key = e.beta_key.as_deref().map(str::trim).unwrap_or("");
                if beta_key.is_empty() {
                    slot_table.remove("BetaKey");
                    if let Some(cfg) = slot_table.get_table_mut("config") {
                        cfg.remove("betakey");
                    }
                } else if slot_table.find("BetaKey").is_some() {
                    slot_table.set_text("BetaKey", beta_key);
                } else {
                    slot_table.ensure_table("config").set_text("betakey", beta_key);
                }

                // ownsdlc: preserve existing location & Int32 representation when applicable; remove if cleared
                let owns_dlc = e.owns_dlc.as_deref().map(str::trim).unwrap_or("");
                if owns_dlc.is_empty() {
                    slot_table.remove("ownsdlc");
                    if let Some(cfg) = slot_table.get_table_mut("config") {
                        cfg.remove("ownsdlc");
                    }
                } else if let Some(prop) = slot_table.find_mut("ownsdlc") {
                    if prop.vtype == TYPE_INT32 {
                        if let Ok(v) = owns_dlc.parse::<i32>() {
                            prop.value = VdfValue::Int32(v);
                        } else {
                            prop.vtype = TYPE_STRING;
                            prop.value = VdfValue::Bytes(owns_dlc.as_bytes().to_vec());
                        }
                    } else {
                        prop.vtype = TYPE_STRING;
                        prop.value = VdfValue::Bytes(owns_dlc.as_bytes().to_vec());
                    }
                } else {
                    let cfg = slot_table.ensure_table("config");
                    if let Some(prop) = cfg.find_mut("ownsdlc") {
                        if prop.vtype == TYPE_INT32 {
                            if let Ok(v) = owns_dlc.parse::<i32>() {
                                prop.value = VdfValue::Int32(v);
                            } else {
                                prop.vtype = TYPE_STRING;
                                prop.value = VdfValue::Bytes(owns_dlc.as_bytes().to_vec());
                            }
                        } else {
                            prop.vtype = TYPE_STRING;
                            prop.value = VdfValue::Bytes(owns_dlc.as_bytes().to_vec());
                        }
                    } else {
                        cfg.set_text("ownsdlc", owns_dlc);
                    }
                }

                // Remove empty `config` subtable if all its properties were cleared
                if slot_table.get_table("config").is_some_and(|c| c.items.is_empty()) {
                    slot_table.remove("config");
                }

                new_items.push(VdfProperty {
                    name: idx.to_string(),
                    raw_name: None,
                    vtype: TYPE_TABLE,
                    value: VdfValue::Table(slot_table),
                });
            }
            launch.items = new_items;
        }

        // LT-56: Check 2x free disk space before writing
        let appinfo_len = fs::metadata(&appinfo).map(|m| m.len()).unwrap_or(0);
        let required_bytes = appinfo_len.max(1024 * 1024) * 2;
        let c_path = std::ffi::CString::new(appinfo.to_str().unwrap_or("/tmp")).unwrap_or_default();
        let mut stat: std::mem::MaybeUninit<libc::statvfs> = std::mem::MaybeUninit::uninit();
        if unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) } == 0 {
            let stat = unsafe { stat.assume_init() };
            let free_space = (stat.f_bavail as u64) * (stat.f_frsize as u64);
            if free_space < required_bytes {
                return Err(format!(
                    "Insufficient disk space for appinfo.vdf: {} bytes free, {} required (2x).",
                    free_space, required_bytes
                ));
            }
        }

        let bkp = self.backup_path();
        if !bkp.exists() {
            fs::copy(&appinfo, &bkp)
                .map_err(|e| format!("Failed to create initial appinfo.vdf backup: {}", e))?;
        } else {
            // Keep timestamped rotating backup (retain last 3, excluding original .bak) before overwriting appinfo.vdf
            let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S-%3f");
            let rot = appinfo.with_extension(format!("vdf.luatools.{}.bak", ts));
            fs::copy(&appinfo, &rot)
                .map_err(|e| format!("Failed to create rotating appinfo.vdf backup: {}", e))?;
            if let Some(parent) = appinfo.parent() {
                if let Ok(rd) = fs::read_dir(parent) {
                    let mut backups: Vec<PathBuf> = rd
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| {
                            p.file_name()
                                .and_then(|n| n.to_str())
                                .map(|n| {
                                    n != "appinfo.vdf.luatools.bak"
                                        && n.starts_with("appinfo.vdf.luatools.")
                                        && n.ends_with(".bak")
                                })
                                .unwrap_or(false)
                        })
                        .collect();
                    backups.sort();
                    while backups.len() > 3 {
                        if let Some(oldest) = backups.first().cloned() {
                            let _ = fs::remove_file(&oldest);
                            backups.remove(0);
                        }
                    }
                }
            }
        }

        let tmp = appinfo.with_extension("vdf.tmp");
        let mut edits = HashMap::new();
        edits.insert(app_id, root);
        af.save_with_edits(&tmp, &edits)?;
        fs::rename(&tmp, &appinfo).map_err(|e| e.to_string())?;
        Ok("Launch options written to appinfo.vdf.".to_string())
    }

    /// Atomically restores appinfo.vdf from the luatools backup (LT-56).
    pub fn restore_backup(&self) -> Result<String, String> {
        let bkp = self.backup_path();
        if !bkp.exists() {
            return Err("No appinfo.vdf.luatools.bak backup found.".to_string());
        }
        let target = self.appinfo_path();
        let tmp = target.with_extension("restore.tmp");
        fs::copy(&bkp, &tmp).map_err(|e| format!("Failed to stage backup restoration: {}", e))?;
        fs::rename(&tmp, &target).map_err(|e| format!("Failed atomic backup restoration: {}", e))?;
        Ok("Restored appinfo.vdf from backup.".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_binary_vdf_v29_launch_entries() {
        let dir = tempfile::tempdir().unwrap();
        let appcache = dir.path().join("appcache");
        fs::create_dir_all(&appcache).unwrap();
        let vdf_path = appcache.join("appinfo.vdf");

        let strings = ["appinfo", "config", "launch", "0", "executable", "type"];
        let mut blob = Vec::new();
        blob.extend_from_slice(&[TYPE_TABLE]);
        blob.extend_from_slice(&0u32.to_le_bytes());
        blob.extend_from_slice(&[TYPE_TABLE]);
        blob.extend_from_slice(&1u32.to_le_bytes());
        blob.extend_from_slice(&[TYPE_TABLE]);
        blob.extend_from_slice(&2u32.to_le_bytes());
        blob.extend_from_slice(&[TYPE_TABLE]);
        blob.extend_from_slice(&3u32.to_le_bytes());
        blob.extend_from_slice(&[TYPE_STRING]);
        blob.extend_from_slice(&4u32.to_le_bytes());
        blob.extend_from_slice(b"eldenring.exe\0");
        blob.extend_from_slice(&[TYPE_STRING]);
        blob.extend_from_slice(&5u32.to_le_bytes());
        blob.extend_from_slice(b"default\0");
        blob.extend_from_slice(&[TYPE_END, TYPE_END, TYPE_END, TYPE_END, TYPE_END]);

        let meta = vec![0u8; 60];
        let entry_size = (meta.len() + blob.len()) as i32;

        let mut file_bytes = Vec::new();
        file_bytes.extend_from_slice(&MAGIC_29.to_le_bytes());
        file_bytes.extend_from_slice(&1u32.to_le_bytes());
        file_bytes.extend_from_slice(&0i64.to_le_bytes()); // table ptr placeholder at offset 8
        file_bytes.extend_from_slice(&1245620u32.to_le_bytes());
        file_bytes.extend_from_slice(&entry_size.to_le_bytes());
        file_bytes.extend_from_slice(&meta);
        file_bytes.extend_from_slice(&blob);
        file_bytes.extend_from_slice(&0u32.to_le_bytes());

        let table_pos = file_bytes.len() as i64;
        file_bytes.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        for s in &strings {
            file_bytes.extend_from_slice(s.as_bytes());
            file_bytes.push(0);
        }
        file_bytes[8..16].copy_from_slice(&table_pos.to_le_bytes());
        fs::write(&vdf_path, &file_bytes).unwrap();

        let svc = LaunchOptionsService::new(dir.path().to_path_buf());
        let mut entries = svc.get_launch_entries(1245620);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].executable, "eldenring.exe");

        entries.push(LaunchEntry {
            index: "1".into(),
            executable: "start_protected_game.exe".into(),
            arguments: "-eac-nop-loaded".into(),
            workingdir: String::new(),
            description: "Offline Bypass".into(),
            launch_type: "option1".into(),
            oslist: "windows".into(),
            osarch: String::new(),
            beta_key: Some("beta_branch".into()),
            owns_dlc: Some("2778580".into()),
            source_index: None,
        });
        svc.save_launch_entries(1245620, &entries).unwrap();

        let reloaded = svc.get_launch_entries(1245620);
        assert_eq!(reloaded.len(), 2);
        assert_eq!(reloaded[1].arguments, "-eac-nop-loaded");
        assert_eq!(reloaded[1].beta_key.as_deref(), Some("beta_branch"));
        assert_eq!(reloaded[1].owns_dlc.as_deref(), Some("2778580"));
        assert_eq!(reloaded[0].source_index, Some(0));
        assert_eq!(reloaded[1].source_index, Some(1));
    }

    #[test]
    fn test_lt54_lt55_preserves_unmodelled_keys_and_property_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let appcache = dir.path().join("appcache");
        fs::create_dir_all(&appcache).unwrap();
        let vdf_path = appcache.join("appinfo.vdf");

        // String table includes unmodelled keys: BetaKey, ownsdlc, vacmodulefilename, description_loc
        let strings = [
            "appinfo",
            "config",
            "launch",
            "0",
            "executable",
            "type",
            "BetaKey",
            "ownsdlc",
            "vacmodulefilename",
            "description_loc",
        ];
        let mut blob = Vec::new();
        blob.extend_from_slice(&[TYPE_TABLE]); // appinfo
        blob.extend_from_slice(&0u32.to_le_bytes());
        blob.extend_from_slice(&[TYPE_TABLE]); // config
        blob.extend_from_slice(&1u32.to_le_bytes());
        blob.extend_from_slice(&[TYPE_TABLE]); // launch
        blob.extend_from_slice(&2u32.to_le_bytes());
        blob.extend_from_slice(&[TYPE_TABLE]); // slot "0"
        blob.extend_from_slice(&3u32.to_le_bytes());

        // Modelled keys
        blob.extend_from_slice(&[TYPE_STRING]);
        blob.extend_from_slice(&4u32.to_le_bytes()); // executable
        blob.extend_from_slice(b"game.exe\0");

        blob.extend_from_slice(&[TYPE_STRING]);
        blob.extend_from_slice(&5u32.to_le_bytes()); // type
        blob.extend_from_slice(b"default\0");

        // Unmodelled keys: BetaKey (string), ownsdlc (int32), vacmodulefilename (string), description_loc (table/token)
        blob.extend_from_slice(&[TYPE_STRING]);
        blob.extend_from_slice(&6u32.to_le_bytes()); // BetaKey
        blob.extend_from_slice(b"public_beta\0");

        blob.extend_from_slice(&[TYPE_INT32]);
        blob.extend_from_slice(&7u32.to_le_bytes()); // ownsdlc
        blob.extend_from_slice(&12345i32.to_le_bytes());

        blob.extend_from_slice(&[TYPE_STRING]);
        blob.extend_from_slice(&8u32.to_le_bytes()); // vacmodulefilename
        blob.extend_from_slice(b"vac_game.dll\0");

        blob.extend_from_slice(&[TYPE_STRING]);
        blob.extend_from_slice(&9u32.to_le_bytes()); // description_loc
        blob.extend_from_slice(b"#Launch_Description\0");

        blob.extend_from_slice(&[TYPE_END, TYPE_END, TYPE_END, TYPE_END, TYPE_END]);

        let meta = vec![0u8; 60];
        let entry_size = (meta.len() + blob.len()) as i32;

        let mut file_bytes = Vec::new();
        file_bytes.extend_from_slice(&MAGIC_29.to_le_bytes());
        file_bytes.extend_from_slice(&1u32.to_le_bytes());
        file_bytes.extend_from_slice(&0i64.to_le_bytes());
        file_bytes.extend_from_slice(&480u32.to_le_bytes()); // AppID 480
        file_bytes.extend_from_slice(&entry_size.to_le_bytes());
        file_bytes.extend_from_slice(&meta);
        file_bytes.extend_from_slice(&blob);
        file_bytes.extend_from_slice(&0u32.to_le_bytes());

        let table_pos = file_bytes.len() as i64;
        file_bytes.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        for s in &strings {
            file_bytes.extend_from_slice(s.as_bytes());
            file_bytes.push(0);
        }
        file_bytes[8..16].copy_from_slice(&table_pos.to_le_bytes());
        fs::write(&vdf_path, &file_bytes).unwrap();

        let svc = LaunchOptionsService::new(dir.path().to_path_buf());
        let mut entries = svc.get_launch_entries(480);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].executable, "game.exe");

        // Update modelled executable and arguments in slot 0
        entries[0].executable = "game_modded.exe".to_string();
        entries[0].arguments = "-novid -freq 144".to_string();

        svc.save_launch_entries(480, &entries).unwrap();

        // Verify unmodelled keys are preserved in appinfo.vdf
        let af = AppInfoFile::open(&vdf_path).unwrap();
        let root = af.read_app(480).unwrap().unwrap();
        let body = root.get_table("appinfo").unwrap();
        let launch = body.get_table("config").unwrap().get_table("launch").unwrap();
        let slot0 = launch.get_table("0").unwrap();

        assert_eq!(slot0.get_text("executable"), "game_modded.exe");
        assert_eq!(slot0.get_text("arguments"), "-novid -freq 144");
        assert_eq!(slot0.get_text("BetaKey"), "public_beta");
        assert_eq!(slot0.get_text("ownsdlc"), "12345");
        assert_eq!(slot0.get_text("vacmodulefilename"), "vac_game.dll");
        assert_eq!(slot0.get_text("description_loc"), "#Launch_Description");
    }

    #[test]
    fn test_lt54_roundtrips_non_utf8_keys_losslessly_in_v29_and_v27() {
        // 1. v29 string pool with two distinct non-UTF-8 byte sequences that both decode to U+FFFD
        let dir = tempfile::tempdir().unwrap();
        let vdf_path = dir.path().join("appinfo_v29.vdf");
        let raw_strings: Vec<Vec<u8>> = vec![
            b"appinfo".to_vec(),
            b"bad_\xFE_key".to_vec(),
            b"bad_\xFF_key".to_vec(),
        ];
        let mut blob = Vec::new();
        blob.push(TYPE_TABLE);
        blob.extend_from_slice(&0u32.to_le_bytes());
        blob.push(TYPE_STRING);
        blob.extend_from_slice(&1u32.to_le_bytes());
        blob.extend_from_slice(b"val_fe\0");
        blob.push(TYPE_STRING);
        blob.extend_from_slice(&2u32.to_le_bytes());
        blob.extend_from_slice(b"val_ff\0");
        blob.extend_from_slice(&[TYPE_END, TYPE_END]);

        let meta = vec![0u8; 60];
        let entry_size = (meta.len() + blob.len()) as u32;
        let mut file_bytes = Vec::new();
        file_bytes.extend_from_slice(&MAGIC_29.to_le_bytes());
        file_bytes.extend_from_slice(&1u32.to_le_bytes());
        file_bytes.extend_from_slice(&0i64.to_le_bytes());
        file_bytes.extend_from_slice(&480u32.to_le_bytes());
        file_bytes.extend_from_slice(&entry_size.to_le_bytes());
        file_bytes.extend_from_slice(&meta);
        file_bytes.extend_from_slice(&blob);
        file_bytes.extend_from_slice(&0u32.to_le_bytes());
        let table_pos = file_bytes.len() as i64;
        file_bytes.extend_from_slice(&(raw_strings.len() as u32).to_le_bytes());
        for s in &raw_strings {
            file_bytes.extend_from_slice(s);
            file_bytes.push(0);
        }
        file_bytes[8..16].copy_from_slice(&table_pos.to_le_bytes());
        fs::write(&vdf_path, &file_bytes).unwrap();

        let af = AppInfoFile::open(&vdf_path).unwrap();
        let root = af.read_app(480).unwrap().unwrap();
        let out_path = dir.path().join("appinfo_v29_out.vdf");
        let mut edits = HashMap::new();
        edits.insert(480, root);
        af.save_with_edits(&out_path, &edits).unwrap();

        let af2 = AppInfoFile::open(&out_path).unwrap();
        let root2 = af2.read_app(480).unwrap().unwrap();
        let body2 = root2.get_table("appinfo").unwrap();
        assert_eq!(body2.items.len(), 2);
        assert_eq!(body2.items[0].raw_name_bytes(), b"bad_\xFE_key");
        assert_eq!(body2.items[1].raw_name_bytes(), b"bad_\xFF_key");

        // 2. v27 inline non-UTF-8 key round-trip
        let v27_path = dir.path().join("appinfo_v27.vdf");
        let mut blob27 = Vec::new();
        blob27.push(TYPE_STRING);
        blob27.extend_from_slice(b"legacy_\xFF_key\0");
        blob27.extend_from_slice(b"hello\0");
        blob27.push(TYPE_END);
        let meta27 = vec![0u8; 40];
        let sz27 = (meta27.len() + blob27.len()) as u32;
        let mut bytes27 = Vec::new();
        bytes27.extend_from_slice(&MAGIC_27.to_le_bytes());
        bytes27.extend_from_slice(&1u32.to_le_bytes());
        bytes27.extend_from_slice(&480u32.to_le_bytes());
        bytes27.extend_from_slice(&sz27.to_le_bytes());
        bytes27.extend_from_slice(&meta27);
        bytes27.extend_from_slice(&blob27);
        bytes27.extend_from_slice(&0u32.to_le_bytes());
        fs::write(&v27_path, &bytes27).unwrap();

        let af27 = AppInfoFile::open(&v27_path).unwrap();
        let root27 = af27.read_app(480).unwrap().unwrap();
        let out27 = dir.path().join("appinfo_v27_out.vdf");
        let mut edits27 = HashMap::new();
        edits27.insert(480, root27);
        af27.save_with_edits(&out27, &edits27).unwrap();

        let af27_re = AppInfoFile::open(&out27).unwrap();
        let root27_re = af27_re.read_app(480).unwrap().unwrap();
        assert_eq!(root27_re.items[0].raw_name_bytes(), b"legacy_\xFF_key");
    }

    #[test]
    fn test_lt55_lt56_source_index_reorder_oslist_clear_and_backup_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let appcache = dir.path().join("appcache");
        fs::create_dir_all(&appcache).unwrap();
        let vdf_path = appcache.join("appinfo.vdf");

        let strings = [
            "appinfo",
            "config",
            "launch",
            "0",
            "1",
            "executable",
            "type",
            "oslist",
            "osarch",
            "vacmodulefilename",
            "description_loc",
        ];
        let mut blob = Vec::new();
        blob.extend_from_slice(&[TYPE_TABLE]);
        blob.extend_from_slice(&0u32.to_le_bytes()); // appinfo
        blob.extend_from_slice(&[TYPE_TABLE]);
        blob.extend_from_slice(&1u32.to_le_bytes()); // config
        blob.extend_from_slice(&[TYPE_TABLE]);
        blob.extend_from_slice(&2u32.to_le_bytes()); // launch

        // Slot 0 with vacmodulefilename + config/oslist + config/osarch
        blob.push(TYPE_TABLE);
        blob.extend_from_slice(&3u32.to_le_bytes());
        blob.push(TYPE_STRING);
        blob.extend_from_slice(&5u32.to_le_bytes());
        blob.extend_from_slice(b"slot0.exe\0");
        blob.push(TYPE_STRING);
        blob.extend_from_slice(&9u32.to_le_bytes());
        blob.extend_from_slice(b"vac_slot0.dll\0");
        blob.push(TYPE_TABLE);
        blob.extend_from_slice(&1u32.to_le_bytes()); // config
        blob.push(TYPE_STRING);
        blob.extend_from_slice(&7u32.to_le_bytes());
        blob.extend_from_slice(b"windows\0");
        blob.push(TYPE_STRING);
        blob.extend_from_slice(&8u32.to_le_bytes());
        blob.extend_from_slice(b"64\0");
        blob.push(TYPE_END); // end config
        blob.push(TYPE_END); // end slot 0

        // Slot 1 with description_loc (no vacmodulefilename)
        blob.push(TYPE_TABLE);
        blob.extend_from_slice(&4u32.to_le_bytes());
        blob.push(TYPE_STRING);
        blob.extend_from_slice(&5u32.to_le_bytes());
        blob.extend_from_slice(b"slot1.exe\0");
        blob.push(TYPE_STRING);
        blob.extend_from_slice(&10u32.to_le_bytes());
        blob.extend_from_slice(b"#Slot1_Loc\0");
        blob.push(TYPE_END); // end slot 1

        blob.extend_from_slice(&[TYPE_END, TYPE_END, TYPE_END, TYPE_END]);

        let meta = vec![0u8; 60];
        let entry_size = (meta.len() + blob.len()) as u32;
        let mut file_bytes = Vec::new();
        file_bytes.extend_from_slice(&MAGIC_29.to_le_bytes());
        file_bytes.extend_from_slice(&1u32.to_le_bytes());
        file_bytes.extend_from_slice(&0i64.to_le_bytes());
        file_bytes.extend_from_slice(&480u32.to_le_bytes());
        file_bytes.extend_from_slice(&entry_size.to_le_bytes());
        file_bytes.extend_from_slice(&meta);
        file_bytes.extend_from_slice(&blob);
        file_bytes.extend_from_slice(&0u32.to_le_bytes());
        let table_pos = file_bytes.len() as i64;
        file_bytes.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        for s in &strings {
            file_bytes.extend_from_slice(s.as_bytes());
            file_bytes.push(0);
        }
        file_bytes[8..16].copy_from_slice(&table_pos.to_le_bytes());
        fs::write(&vdf_path, &file_bytes).unwrap();

        let svc = LaunchOptionsService::new(dir.path().to_path_buf());
        let mut entries = svc.get_launch_entries(480);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].source_index, Some(0));
        assert_eq!(entries[1].source_index, Some(1));

        // Clear oslist/osarch on slot 0, then swap slot 0 and slot 1
        entries[0].oslist.clear();
        entries[0].osarch.clear();
        entries.swap(0, 1);
        svc.save_launch_entries(480, &entries).unwrap();

        let af = AppInfoFile::open(&vdf_path).unwrap();
        let root = af.read_app(480).unwrap().unwrap();
        let launch = root
            .get_table("appinfo")
            .unwrap()
            .get_table("config")
            .unwrap()
            .get_table("launch")
            .unwrap();
        // New slot "0" came from source_index 1 -> has description_loc, NOT vac_slot0.dll
        let new_slot0 = launch.get_table("0").unwrap();
        assert_eq!(new_slot0.get_text("executable"), "slot1.exe");
        assert_eq!(new_slot0.get_text("description_loc"), "#Slot1_Loc");
        assert!(new_slot0.find("vacmodulefilename").is_none());

        // New slot "1" came from source_index 0 -> kept vac_slot0.dll, cleared oslist/osarch
        let new_slot1 = launch.get_table("1").unwrap();
        assert_eq!(new_slot1.get_text("executable"), "slot0.exe");
        assert_eq!(new_slot1.get_text("vacmodulefilename"), "vac_slot0.dll");
        assert!(new_slot1.get_table("config").is_none());

        // LT-56: Trigger 5 saves and verify base .bak is preserved plus at most 3 rotated backups
        for _ in 0..4 {
            std::thread::sleep(std::time::Duration::from_millis(5));
            let cur = svc.get_launch_entries(480);
            svc.save_launch_entries(480, &cur).unwrap();
        }
        assert!(svc.backup_path().is_file());
        let rotated_count = fs::read_dir(&appcache)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| {
                        n != "appinfo.vdf.luatools.bak"
                            && n.starts_with("appinfo.vdf.luatools.")
                            && n.ends_with(".bak")
                    })
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(rotated_count, 3);
    }
}

// ============================================================================
// LT-58 & LT-59: LaunchModStore, PICS Snapshot Rebase & Drift Detection
// ============================================================================

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchMod {
    pub app_id: u32,
    pub change_number: u32,
    pub original: Vec<LaunchEntry>,
    pub desired: Vec<LaunchEntry>,
    pub saved_at: String,
}

// Re-export LaunchModStore from canonical service module
pub use crate::services::launch_mod_store::LaunchModStore;
// Re-export ManifestFile and manifest types from canonical manifest parser module
pub use crate::parsers::manifest_binary::{
    ManifestFile, ManifestMetadata, MANIFEST_EOF_MAGIC, MANIFEST_METADATA_MAGIC,
    MANIFEST_PAYLOAD_MAGIC,
};

#[cfg(test)]
mod tests_vdf_extended {
    use super::*;

    #[test]
    fn test_manifest_metadata_decode_and_unwrapping() {
        // Tag 1 (depot_id), wire 0: 0x08; 1245620 varint: 0xb4, 0x83, 0x4c
        let meta_payload = vec![0x08, 0xb4, 0x83, 0x4c, 0x10, 0xa0, 0x88, 0x01];
        let mut framed = Vec::new();
        framed.extend(MANIFEST_METADATA_MAGIC.to_le_bytes());
        framed.extend((meta_payload.len() as u32).to_le_bytes());
        framed.extend(&meta_payload);
        framed.extend(MANIFEST_EOF_MAGIC.to_le_bytes());
        framed.extend(0u32.to_le_bytes());

        let section = ManifestFile::find_section(&framed, MANIFEST_METADATA_MAGIC).unwrap();
        assert_eq!(section.len(), meta_payload.len());
        let meta = ManifestFile::parse_metadata(section).unwrap();
        assert_eq!(meta.depot_id, 1245620);
    }
}
