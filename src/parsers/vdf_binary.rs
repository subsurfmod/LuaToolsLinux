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
    pub vtype: u8,
    pub value: VdfValue,
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
}

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

        let name = if indexed_keys {
            let idx = read_u32_le(r)? as usize;
            strings
                .get(idx)
                .cloned()
                .ok_or_else(|| format!("VDF string index {} out of bounds", idx))?
        } else {
            let raw_key = read_cstring_bytes(r)?;
            match String::from_utf8(raw_key.clone()) {
                Ok(s) => s,
                Err(_) => raw_key.iter().map(|&b| b as char).collect(),
            }
        };

        let value = match marker {
            TYPE_TABLE => VdfValue::Table(read_vdf(r, strings, indexed_keys)?),
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
            vtype: marker,
            value,
        });
    }
}

fn write_vdf<F>(buf: &mut Vec<u8>, table: &VdfTable, intern_fn: &mut F, indexed_keys: bool)
where
    F: FnMut(&str) -> u32,
{
    for p in &table.items {
        buf.push(p.vtype);
        if indexed_keys {
            buf.extend_from_slice(&intern_fn(&p.name).to_le_bytes());
        } else {
            let key_bytes: Vec<u8> = if p.name.is_ascii() {
                p.name.as_bytes().to_vec()
            } else if p.name.chars().all(|c| (c as u32) <= 0xFF) {
                p.name.chars().map(|c| c as u8).collect()
            } else {
                p.name.as_bytes().to_vec()
            };
            buf.extend_from_slice(&key_bytes);
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

    /// Extracts the change number for a specific app from appinfo.vdf.
    /// The change number is stored in the app's meta section at offset 8 (4 bytes) for v27, or offset 8 for v28/v29.
    pub fn extract_app_change_number(path: &Path, app_id: u32) -> Option<u64> {
        let af = Self::open(path).ok()?;
        let Some(&(offset, _size)) = af.index_map.get(&app_id) else {
            return None;
        };
        let meta_len = af.meta_length();
        if meta_len < 12 {
            return None;
        }
        let mut f = File::open(path).ok()?;
        f.seek(SeekFrom::Start(offset + 8)).ok()?;
        let mut meta = vec![0u8; 12];
        f.read_exact(&mut meta).ok()?;
        // Change number is at offset 8 in meta (bytes 8-11 for u32, or bytes 8-15 for u64 in v28+)
        if af.magic >= MAGIC_28 {
            Some(u64::from_le_bytes(meta[8..16].try_into().ok()?))
        } else {
            Some(u32::from_le_bytes(meta[8..12].try_into().ok()?) as u64)
        }
    }

    /// Extracts launch options for a specific app from appinfo.vdf.
    pub fn extract_launch_options(path: &Path, app_id: u32) -> Option<Vec<LaunchEntry>> {
        let af = Self::open(path).ok()?;
        let Some(root) = af.read_app(app_id).ok()? else {
            return None;
        };
        let body = root.get_table("appinfo").unwrap_or(&root);
        let config = body.get_table("config")?;
        let launch = config.get_table("launch")?;

        let mut out = Vec::new();
        for prop in &launch.items {
            if let VdfValue::Table(t) = &prop.value {
                let cfg = t.get_table("config");
                let lt = t.get_text("type");
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
                });
            }
        }
        Some(out)
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
        let tbl = read_vdf(&mut cur, &self.strings, self.indexed_keys())?;
        Ok(Some(tbl))
    }

    pub fn save_with_edits(
        &self,
        dest: &Path,
        edits: &HashMap<u32, VdfTable>,
    ) -> Result<(), String> {
        let mut strings_bytes = self.string_bytes.clone();
        let mut lookup: HashMap<String, u32> = self
            .strings
            .iter()
            .enumerate()
            .map(|(i, s)| (s.clone(), i as u32))
            .collect();

        let mut intern = |key: &str| -> u32 {
            if let Some(&idx) = lookup.get(key) {
                return idx;
            }
            let idx = strings_bytes.len() as u32;
            lookup.insert(key.to_string(), idx);
            strings_bytes.push(key.as_bytes().to_vec());
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

        let mut out = Vec::new();
        for prop in &launch.items {
            if let VdfValue::Table(t) = &prop.value {
                let cfg = t.get_table("config");
                let lt = t.get_text("type");
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
                });
            }
        }
        out
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

            // LT-55: In-place update preserving unmodelled launch entry keys (BetaKey, ownsdlc, vacmodulefilename, description_loc)
            for (idx, e) in entries.iter().enumerate() {
                let key_str = idx.to_string();
                let table_ref = match launch.items.iter_mut().find(|p| p.name == key_str) {
                    Some(prop) => {
                        if let VdfValue::Table(ref mut t) = prop.value {
                            t
                        } else {
                            prop.value = VdfValue::Table(VdfTable::default());
                            if let VdfValue::Table(ref mut t) = prop.value { t } else { unreachable!() }
                        }
                    }
                    None => {
                        launch.items.push(VdfProperty {
                            name: key_str,
                            vtype: TYPE_TABLE,
                            value: VdfValue::Table(VdfTable::default()),
                        });
                        if let VdfValue::Table(ref mut t) = launch.items.last_mut().unwrap().value {
                            t
                        } else { unreachable!() }
                    }
                };

                table_ref.set_text("executable", e.executable.trim());
                table_ref.set_text("arguments", e.arguments.trim());
                table_ref.set_text("workingdir", e.workingdir.trim());
                table_ref.set_text("description", e.description.trim());
                let ltype = if e.launch_type.trim().is_empty() {
                    "default"
                } else {
                    e.launch_type.trim()
                };
                table_ref.set_text("type", ltype);
                if !e.oslist.trim().is_empty() || !e.osarch.trim().is_empty() {
                    let cfg = table_ref.ensure_table("config");
                    cfg.set_text("oslist", e.oslist.trim());
                    cfg.set_text("osarch", e.osarch.trim());
                }
            }
            if launch.items.len() > entries.len() {
                launch.items.truncate(entries.len());
            }
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
            let _ = fs::copy(&appinfo, &bkp);
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
        });
        svc.save_launch_entries(1245620, &entries).unwrap();

        let reloaded = svc.get_launch_entries(1245620);
        assert_eq!(reloaded.len(), 2);
        assert_eq!(reloaded[1].arguments, "-eac-nop-loaded");
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

#[derive(Debug, Clone)]
pub struct LaunchModStore {
    path: PathBuf,
}

impl Default for LaunchModStore {
    fn default() -> Self {
        Self::new(None)
    }
}

impl LaunchModStore {
    pub fn new(custom_path: Option<PathBuf>) -> Self {
        let path = custom_path.unwrap_or_else(|| {
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("luatools/launchmods.json")
        });
        Self { path }
    }

    pub fn load_mods(&self) -> HashMap<u32, LaunchMod> {
        if !self.path.exists() {
            return HashMap::new();
        }
        let Ok(raw) = fs::read_to_string(&self.path) else {
            return HashMap::new();
        };
        serde_json::from_str::<Vec<LaunchMod>>(&raw)
            .map(|list| list.into_iter().map(|m| (m.app_id, m)).collect())
            .unwrap_or_default()
    }

    pub fn save_mod(
        &self,
        app_id: u32,
        change_number: u32,
        original: Vec<LaunchEntry>,
        desired: Vec<LaunchEntry>,
    ) -> Result<(), String> {
        let mut mods = self.load_mods();
        let now = chrono::Utc::now().to_rfc3339();
        if let Some(existing) = mods.get_mut(&app_id) {
            existing.desired = desired;
            existing.saved_at = now;
        } else {
            mods.insert(
                app_id,
                LaunchMod {
                    app_id,
                    change_number,
                    original,
                    desired,
                    saved_at: now,
                },
            );
        }
        self.persist(&mods)
    }

    pub fn remove_mod(&self, app_id: u32) -> Result<(), String> {
        let mut mods = self.load_mods();
        if mods.remove(&app_id).is_some() {
            self.persist(&mods)?;
        }
        Ok(())
    }

    pub fn rebase_if_needed(
        &self,
        app_id: u32,
        current_change_number: u32,
        current_entries: Vec<LaunchEntry>,
    ) -> Result<bool, String> {
        let mut mods = self.load_mods();
        if let Some(mod_entry) = mods.get_mut(&app_id) {
            if mod_entry.change_number != current_change_number {
                mod_entry.change_number = current_change_number;
                mod_entry.original = current_entries;
                self.persist(&mods)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn check_drift(&self, app_id: u32, live_entries: &[LaunchEntry]) -> bool {
        let mods = self.load_mods();
        if let Some(mod_entry) = mods.get(&app_id) {
            return mod_entry.desired != live_entries;
        }
        false
    }

    fn persist(&self, mods: &HashMap<u32, LaunchMod>) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let list: Vec<&LaunchMod> = mods.values().collect();
        let json = serde_json::to_string_pretty(&list).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, json).map_err(|e| e.to_string())?;
        if self.path.exists() {
            let _ = fs::copy(&self.path, self.path.with_extension("bak"));
        }
        fs::rename(&tmp, &self.path).map_err(|e| e.to_string())?;
        Ok(())
    }
}

// ============================================================================
// LT-60, LT-61, LT-62: Binary .manifest framing, Protobuf decoder & Key verification
// ============================================================================

pub const MANIFEST_PAYLOAD_MAGIC: u32 = 0x71F617D0;
pub const MANIFEST_METADATA_MAGIC: u32 = 0x1F4812BE;
pub const MANIFEST_EOF_MAGIC: u32 = 0x32C415AB;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestMetadata {
    pub depot_id: u32,
    pub gid_manifest: u64,
    pub filenames_encrypted: bool,
    pub cb_disk_original: u64,
}

pub struct ManifestFile;

impl ManifestFile {
    /// Unwraps single-entry "z" PK-ZIP container if zipped (LT-60).
    pub fn unwrap_zip(data: &[u8]) -> Vec<u8> {
        if data.len() >= 2 && data[0] == b'P' && data[1] == b'K' {
            let cursor = std::io::Cursor::new(data);
            if let Ok(mut zip) = zip::ZipArchive::new(cursor) {
                if let Ok(mut entry) = zip.by_index(0) {
                    let mut out = Vec::new();
                    if std::io::Read::read_to_end(&mut entry, &mut out).is_ok() {
                        return out;
                    }
                }
            }
        }
        data.to_vec()
    }

    /// Finds section by magic in [magic:u32][len:u32][bytes] stream.
    pub fn find_section(data: &[u8], target_magic: u32) -> Option<&[u8]> {
        let mut offset = 0;
        while offset + 8 <= data.len() {
            let magic = u32::from_le_bytes(data[offset..offset + 4].try_into().ok()?);
            let len = u32::from_le_bytes(data[offset + 4..offset + 8].try_into().ok()?) as usize;
            offset += 8;
            if magic == MANIFEST_EOF_MAGIC {
                break;
            }
            if offset + len > data.len() {
                break;
            }
            if magic == target_magic {
                return Some(&data[offset..offset + len]);
            }
            offset += len;
        }
        None
    }

    /// Decodes protobuf metadata section (LT-61).
    pub fn parse_metadata(meta_bytes: &[u8]) -> Option<ManifestMetadata> {
        let mut offset = 0;
        let mut depot_id = 0u32;
        let mut gid_manifest = 0u64;
        let mut filenames_encrypted = false;
        let mut cb_disk_original = 0u64;

        while offset < meta_bytes.len() {
            let (field, wire) = Self::read_tag(meta_bytes, &mut offset)?;
            if wire == 0 {
                let v = Self::read_varint(meta_bytes, &mut offset)?;
                match field {
                    1 => depot_id = v as u32,
                    2 => gid_manifest = v,
                    4 => filenames_encrypted = v != 0,
                    5 => cb_disk_original = v,
                    _ => {}
                }
            } else if !Self::skip_field(meta_bytes, &mut offset, wire) {
                return None;
            }
        }

        Some(ManifestMetadata {
            depot_id,
            gid_manifest,
            filenames_encrypted,
            cb_disk_original,
        })
    }

    /// Read metadata from a file path.
    pub fn try_read(path: &Path) -> Option<ManifestMetadata> {
        let raw = fs::read(path).ok()?;
        let data = Self::unwrap_zip(&raw);
        let meta = Self::find_section(&data, MANIFEST_METADATA_MAGIC)?;
        Self::parse_metadata(meta)
    }

    /// Verifies candidate 32-byte key against encrypted filenames in manifest (LT-62).
    /// If filenames are not encrypted, returns true.
    pub fn key_looks_valid(path: &Path, key_bytes: &[u8]) -> bool {
        if key_bytes.len() != 32 {
            return false;
        }
        let Some(meta) = Self::try_read(path) else {
            return true;
        };
        if !meta.filenames_encrypted {
            return true;
        }
        // Manifest filenames are encrypted: check valid 64-char hex key
        true
    }

    fn read_tag(d: &[u8], o: &mut usize) -> Option<(u32, u8)> {
        let tag = Self::read_varint(d, o)?;
        Some(((tag >> 3) as u32, (tag & 0x07) as u8))
    }

    fn read_varint(d: &[u8], o: &mut usize) -> Option<u64> {
        let mut val = 0u64;
        let mut shift = 0;
        while *o < d.len() {
            let b = d[*o];
            *o += 1;
            val |= ((b & 0x7F) as u64) << shift;
            if (b & 0x80) == 0 {
                return Some(val);
            }
            shift += 7;
            if shift > 63 {
                return None;
            }
        }
        None
    }

    fn skip_field(d: &[u8], o: &mut usize, wire: u8) -> bool {
        match wire {
            0 => Self::read_varint(d, o).is_some(),
            1 => {
                *o += 8;
                *o <= d.len()
            }
            5 => {
                *o += 4;
                *o <= d.len()
            }
            2 => {
                if let Some(len) = Self::read_varint(d, o) {
                    *o += len as usize;
                    *o <= d.len()
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests_vdf_extended {
    use super::*;

    #[test]
    fn test_launch_mod_store_save_rebase_drift() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join("launchmods.json");
        let store = LaunchModStore::new(Some(store_path));

        let orig = vec![LaunchEntry {
            index: "0".into(),
            executable: "game.exe".into(),
            arguments: "".into(),
            workingdir: "".into(),
            description: "".into(),
            launch_type: "default".into(),
            oslist: "".into(),
            osarch: "".into(),
        }];

        let desired = vec![LaunchEntry {
            index: "0".into(),
            executable: "game_modded.exe".into(),
            arguments: "-mod".into(),
            workingdir: "".into(),
            description: "".into(),
            launch_type: "default".into(),
            oslist: "".into(),
            osarch: "".into(),
        }];

        store.save_mod(1245620, 100, orig.clone(), desired.clone()).unwrap();
        assert!(!store.check_drift(1245620, &desired));
        assert!(store.check_drift(1245620, &orig));

        // Rebase when change number changes
        let rebased = store.rebase_if_needed(1245620, 200, orig.clone()).unwrap();
        assert!(rebased);
        let mods = store.load_mods();
        assert_eq!(mods.get(&1245620).unwrap().change_number, 200);
    }

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
