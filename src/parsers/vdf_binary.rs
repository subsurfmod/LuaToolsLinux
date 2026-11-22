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

#[derive(Debug, Clone, Default)]
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
            String::from_utf8_lossy(&read_cstring_bytes(r)?).to_string()
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
            buf.extend_from_slice(p.name.as_bytes());
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
    index_map: HashMap<u32, (u64, i32)>,
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
        let mut strings = Vec::new();
        let mut string_bytes = Vec::new();

        if indexed_keys {
            let table_offset = read_i64_le(&mut r)? as u64;
            let resume = r.stream_position().map_err(|e| e.to_string())?;
            r.seek(SeekFrom::Start(table_offset))
                .map_err(|e| e.to_string())?;
            let count = read_u32_le(&mut r)? as usize;
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
            let mut hdr = [0u8; 8];
            if r.read_exact(&mut hdr).is_err() {
                break;
            }
            let app_id = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
            let size = i32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
            if app_id == 0 {
                break;
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

                let new_size = (meta.len() + blob.len()) as i32;
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
            launch.items.clear();

            for (idx, e) in entries.iter().enumerate() {
                let mut t = VdfTable::default();
                t.set_text("executable", e.executable.trim());
                t.set_text("arguments", e.arguments.trim());
                t.set_text("workingdir", e.workingdir.trim());
                t.set_text("description", e.description.trim());
                let ltype = if e.launch_type.trim().is_empty() {
                    "default"
                } else {
                    e.launch_type.trim()
                };
                t.set_text("type", ltype);
                if !e.oslist.trim().is_empty() || !e.osarch.trim().is_empty() {
                    let cfg = t.ensure_table("config");
                    cfg.set_text("oslist", e.oslist.trim());
                    cfg.set_text("osarch", e.osarch.trim());
                }
                launch.items.push(VdfProperty {
                    name: idx.to_string(),
                    vtype: TYPE_TABLE,
                    value: VdfValue::Table(t),
                });
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

    pub fn restore_backup(&self) -> Result<String, String> {
        let bkp = self.backup_path();
        if !bkp.exists() {
            return Err("No appinfo.vdf.luatools.bak backup found.".to_string());
        }
        fs::copy(&bkp, self.appinfo_path()).map_err(|e| e.to_string())?;
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
