//! Valve Binary KeyValues (KV1) parser for Steam's UserGameStats binary format.
//!
//! Based on Valve's binary KeyValues format used in UserGameStats_*.bin files.
//! This is the ACTUAL format used by Steam, NOT the custom "BKV" format.
//!
//! Format (Valve's binary KeyValues v1):
//! - Byte 0x00: Subtable start (read null-terminated string key, recurse)
//! - Byte 0x01: String value (read null-terminated key, read null-terminated string value)
//! - Byte 0x02: Int32 value (read null-terminated key, read 4-byte LE int)
//! - Byte 0x03: Float32 value (read null-terminated key, read 4-byte LE float)
//! - Byte 0x04: Pointer value (read null-terminated key, read varint/uint64)
//! - Byte 0x05: WideString value (read null-terminated key, read length-prefixed UTF-16)
//! - Byte 0x06: Color value (read null-terminated key, read 4-byte uint32)
//! - Byte 0x07: UInt64 value (read null-terminated key, read 8-byte LE uint64)
//! - Byte 0x08: Subtable end (marks end of current table)

use std::collections::BTreeMap;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum Kv1Value {
    String(String),
    Int32(i32),
    Float(f32),
    Pointer(u64),
    WideString(String),
    Color(u32),
    UInt64(u64),
    Binary(Vec<u8>),
    Object(BTreeMap<String, Kv1Value>),
    Array(Vec<Kv1Value>),
}

#[derive(Debug, Clone)]
pub struct Kv1Node {
    pub name: String,
    pub value: Kv1Value,
}

const MAX_KV1_ALLOC_LEN: usize = 64 * 1024 * 1024;
const MAX_KV1_NAME_LEN: usize = 64 * 1024;
const MAX_KV1_DEPTH: usize = 64;
const READ_CHUNK_SIZE: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Kv1Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Unexpected EOF")]
    UnexpectedEof,
    #[error("Invalid node type: {0}")]
    InvalidNodeType(u8),
    #[error("UTF-8 decode error: {0}")]
    Utf8Error(#[from] std::string::FromUtf8Error),
    #[error("UTF-16 decode error: {0}")]
    Utf16Error(#[from] std::string::FromUtf16Error),
    #[error("Parse error: {0}")]
    Parse(String),
}

pub struct Kv1Reader<R: Read + Seek> {
    reader: R,
}

impl<R: Read + Seek> Kv1Reader<R> {
    pub fn new(reader: R) -> Self {
        Self { reader }
    }

    pub fn parse(&mut self) -> Result<Kv1Value, Kv1Error> {
        // Valve KV1 doesn't have a magic header - it starts directly with the root object
        // The root is typically an Object (type 0x00) with an empty name or a specific name
        let (_name, value) = self.read_node(0)?;
        Ok(value)
    }

    fn read_node(&mut self, depth: usize) -> Result<(String, Kv1Value), Kv1Error> {
        if depth > MAX_KV1_DEPTH {
            return Err(Kv1Error::Parse(format!(
                "Maximum KV1 nesting depth ({}) exceeded",
                MAX_KV1_DEPTH
            )));
        }

        let node_type = self.read_u8()?;

        // Type 0x08 is end-of-table marker (no key)
        if node_type == 0x08 {
            return Err(Kv1Error::Parse("Unexpected end-of-table marker".into()));
        }

        let name = self.read_null_terminated_string()?;

        let value = match node_type {
            0x00 => Kv1Value::Object(self.read_object(depth + 1)?),
            0x01 => Kv1Value::String(self.read_null_terminated_string()?),
            0x02 => Kv1Value::Int32(self.read_i32_le()?),
            0x03 => Kv1Value::Float(self.read_f32_le()?),
            0x04 => Kv1Value::Pointer(self.read_varint()?),
            0x05 => Kv1Value::WideString(self.read_wide_string()?),
            0x06 => Kv1Value::Color(self.read_u32_le()?),
            0x07 => Kv1Value::UInt64(self.read_u64_le()?),
            0x08 => {
                // End marker - shouldn't reach here as we check at start
                return Err(Kv1Error::Parse("Unexpected end-of-table marker".into()));
            }
            0x09 => Kv1Value::Binary(self.read_binary()?),
            0x0A => Kv1Value::Array(self.read_array(depth + 1)?),
            t => return Err(Kv1Error::InvalidNodeType(t)),
        };
        Ok((name, value))
    }

    fn read_object(&mut self, depth: usize) -> Result<BTreeMap<String, Kv1Value>, Kv1Error> {
        if depth > MAX_KV1_DEPTH {
            return Err(Kv1Error::Parse(format!(
                "Maximum KV1 nesting depth ({}) exceeded",
                MAX_KV1_DEPTH
            )));
        }
        let mut map = BTreeMap::new();
        loop {
            let node_type = self.peek_u8()?;
            if node_type == 0x08 {
                // End of object
                self.read_u8()?; // consume the 0x08
                let _empty_name = self.read_null_terminated_string()?; // empty name for end marker
                break;
            }
            let (child_name, child_value) = self.read_node(depth)?;
            map.insert(child_name, child_value);
        }
        Ok(map)
    }

    fn read_array(&mut self, depth: usize) -> Result<Vec<Kv1Value>, Kv1Error> {
        if depth > MAX_KV1_DEPTH {
            return Err(Kv1Error::Parse(format!(
                "Maximum KV1 nesting depth ({}) exceeded",
                MAX_KV1_DEPTH
            )));
        }
        let mut arr = Vec::new();
        loop {
            let node_type = self.peek_u8()?;
            if node_type == 0x08 {
                // End of array
                self.read_u8()?; // consume the 0x08
                let _empty_name = self.read_null_terminated_string()?; // empty name for end marker
                break;
            }
            // Array elements have empty names in Valve format
            let (_child_name, value) = self.read_node(depth)?;
            arr.push(value);
        }
        Ok(arr)
    }

    fn read_bounded_bytes(&mut self, len: usize) -> Result<Vec<u8>, Kv1Error> {
        let mut buf = Vec::with_capacity(len.min(READ_CHUNK_SIZE));
        let mut remaining = len;
        let mut chunk = [0u8; 4096];
        while remaining > 0 {
            let to_read = remaining.min(chunk.len());
            self.read_exact_checked(&mut chunk[..to_read])?;
            buf.extend_from_slice(&chunk[..to_read]);
            remaining -= to_read;
        }
        Ok(buf)
    }

    fn read_null_terminated_string(&mut self) -> Result<String, Kv1Error> {
        let mut buf = Vec::new();
        loop {
            let b = self.read_u8()?;
            if b == 0 {
                break;
            }
            if buf.len() >= MAX_KV1_NAME_LEN {
                return Err(Kv1Error::Parse(format!(
                    "String length exceeds limit ({})",
                    MAX_KV1_NAME_LEN
                )));
            }
            buf.push(b);
        }
        Ok(String::from_utf8(buf)?)
    }

    fn read_wide_string(&mut self) -> Result<String, Kv1Error> {
        // WideString is length-prefixed UTF-16
        let len = self.read_u32_le()? as usize;
        if len > (MAX_KV1_ALLOC_LEN / 2) {
            return Err(Kv1Error::Parse(format!(
                "WideString length {} exceeds limit",
                len
            )));
        }
        let mut buf = Vec::with_capacity(len.min(READ_CHUNK_SIZE / 2));
        for _ in 0..len {
            buf.push(self.read_u16_le()?);
        }
        Ok(String::from_utf16(&buf)?)
    }

    fn read_binary(&mut self) -> Result<Vec<u8>, Kv1Error> {
        // Binary data is length-prefixed (uint32 LE)
        let len = self.read_u32_le()? as usize;
        if len > MAX_KV1_ALLOC_LEN {
            return Err(Kv1Error::Parse(format!(
                "Binary length {} exceeds limit",
                len
            )));
        }
        self.read_bounded_bytes(len)
    }

    fn read_varint(&mut self) -> Result<u64, Kv1Error> {
        let mut result = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = self.read_u8()?;
            if shift == 63 && (byte & 0x7F) > 1 {
                return Err(Kv1Error::Parse("VarInt overflow".into()));
            }
            result |= ((byte & 0x7F) as u64) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 63 {
                return Err(Kv1Error::Parse("VarInt too large".into()));
            }
        }
        Ok(result)
    }

    fn read_exact_checked(&mut self, buf: &mut [u8]) -> Result<(), Kv1Error> {
        self.reader.read_exact(buf).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                Kv1Error::UnexpectedEof
            } else {
                Kv1Error::Io(e)
            }
        })
    }

    fn read_u8(&mut self) -> Result<u8, Kv1Error> {
        let mut buf = [0u8; 1];
        self.read_exact_checked(&mut buf)?;
        Ok(buf[0])
    }

    fn read_u16_le(&mut self) -> Result<u16, Kv1Error> {
        let mut buf = [0u8; 2];
        self.read_exact_checked(&mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    fn read_u32_le(&mut self) -> Result<u32, Kv1Error> {
        let mut buf = [0u8; 4];
        self.read_exact_checked(&mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    fn read_u64_le(&mut self) -> Result<u64, Kv1Error> {
        let mut buf = [0u8; 8];
        self.read_exact_checked(&mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    fn read_i32_le(&mut self) -> Result<i32, Kv1Error> {
        let mut buf = [0u8; 4];
        self.read_exact_checked(&mut buf)?;
        Ok(i32::from_le_bytes(buf))
    }

    fn read_f32_le(&mut self) -> Result<f32, Kv1Error> {
        let mut buf = [0u8; 4];
        self.read_exact_checked(&mut buf)?;
        Ok(f32::from_le_bytes(buf))
    }

    fn peek_u8(&mut self) -> Result<u8, Kv1Error> {
        let pos = self.reader.stream_position()?;
        let b = self.read_u8()?;
        self.reader.seek(SeekFrom::Start(pos))?;
        Ok(b)
    }
}

/// Parse a Valve KV1 file from disk
pub fn parse_kv1_file<P: AsRef<Path>>(path: P) -> Result<Kv1Value, Kv1Error> {
    let file = std::fs::File::open(path)?;
    let mut reader = Kv1Reader::new(BufReader::new(file));
    reader.parse()
}

/// Extract achievements from a parsed UserGameStats KV1
pub fn extract_achievements_kv1(kv1: &Kv1Value) -> BTreeMap<String, AchievementData> {
    let mut achievements = BTreeMap::new();
    extract_achievements_recursive(kv1, &mut achievements, 0);
    achievements
}

#[derive(Debug, Clone, Default)]
pub struct AchievementData {
    pub name: String,
    pub unlocked: bool,
    pub unlock_time: Option<u32>,
    pub progress: Option<i32>,
    pub max_progress: Option<i32>,
}

fn extract_achievements_recursive(
    value: &Kv1Value,
    out: &mut BTreeMap<String, AchievementData>,
    depth: usize,
) {
    if depth > MAX_KV1_DEPTH {
        return;
    }
    match value {
        Kv1Value::Object(map) => {
            // Check if this is an achievement node
            if let (Some(Kv1Value::String(name)), Some(Kv1Value::Int32(unlocked))) =
                (map.get("name"), map.get("unlocked"))
            {
                let mut ach = AchievementData {
                    name: name.clone(),
                    unlocked: *unlocked != 0,
                    ..Default::default()
                };
                match map.get("unlockTime") {
                    Some(Kv1Value::Int32(t)) => ach.unlock_time = Some(*t as u32),
                    Some(Kv1Value::UInt64(t)) => ach.unlock_time = u32::try_from(*t).ok(),
                    _ => {}
                }
                if let Some(Kv1Value::Int32(p)) = map.get("progress") {
                    ach.progress = Some(*p);
                }
                if let Some(Kv1Value::Int32(mp)) = map.get("maxProgress") {
                    ach.max_progress = Some(*mp);
                }
                out.insert(name.clone(), ach);
            }
            // Recurse into children
            for v in map.values() {
                extract_achievements_recursive(v, out, depth + 1);
            }
        }
        Kv1Value::Array(arr) => {
            for v in arr {
                extract_achievements_recursive(v, out, depth + 1);
            }
        }
        _ => {}
    }
}

/// Find the Steam stats file for a given app/account
pub fn find_user_game_stats_file(
    steam_config_dir: &Path,
    account_id: u32,
    app_id: u32,
) -> Option<std::path::PathBuf> {
    let pattern = format!("UserGameStats_{}_{}.bin", account_id, app_id);
    if let Some(parent) = steam_config_dir.parent() {
        let appcache_stats = parent.join("appcache").join("stats").join(&pattern);
        if appcache_stats.exists() {
            return Some(appcache_stats);
        }
        let depot_cache = parent.join("depotcache").join(&pattern);
        if depot_cache.exists() {
            return Some(depot_cache);
        }
    }
    // Also check config directory
    let path = steam_config_dir.join(&pattern);
    if path.exists() {
        return Some(path);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_parse_simple_string() {
        // Type 0x01 (string) + key "test" + null + value "data" + null
        let mut data = vec![0x01];
        data.extend_from_slice(b"test\0");
        data.extend_from_slice(b"data\0");

        let mut cursor = Cursor::new(data);
        let mut reader = Kv1Reader::new(&mut cursor);
        let result = reader.read_node(0).unwrap();
        assert_eq!(result.0, "test");
        assert_eq!(result.1, Kv1Value::String("data".into()));
    }

    #[test]
    fn test_parse_simple_int32() {
        // Type 0x02 (int32) + key "count" + null + value 42 (LE)
        let mut data = vec![0x02];
        data.extend_from_slice(b"count\0");
        data.extend_from_slice(&42i32.to_le_bytes());

        let mut cursor = Cursor::new(data);
        let mut reader = Kv1Reader::new(&mut cursor);
        let result = reader.read_node(0).unwrap();
        assert_eq!(result.0, "count");
        assert_eq!(result.1, Kv1Value::Int32(42));
    }

    #[test]
    fn test_parse_nested_object() {
        // Type 0x00 (object) + key "root" + null
        //   Type 0x01 (string) + key "child" + null + value "value" + null
        //   Type 0x08 (end) + empty key
        let mut data = vec![0x00];
        data.extend_from_slice(b"root\0");
        data.push(0x01);
        data.extend_from_slice(b"child\0");
        data.extend_from_slice(b"value\0");
        data.push(0x08);
        data.push(0); // empty name for end marker

        let mut cursor = Cursor::new(data);
        let mut reader = Kv1Reader::new(&mut cursor);
        let result = reader.read_node(0).unwrap();
        assert_eq!(result.0, "root");
        if let Kv1Value::Object(map) = result.1 {
            assert!(map.contains_key("child"));
            assert_eq!(map.get("child"), Some(&Kv1Value::String("value".into())));
        } else {
            panic!("Expected object");
        }
    }

    #[test]
    fn test_parse_root_object() {
        // Root object with multiple children
        let mut data = vec![0x00];
        data.push(0); // empty root name
        // Child 1: string
        data.push(0x01);
        data.extend_from_slice(b"version\0");
        data.extend_from_slice(b"1\0");
        // Child 2: int32
        data.push(0x02);
        data.extend_from_slice(b"appid\0");
        data.extend_from_slice(&730i32.to_le_bytes());
        // End
        data.push(0x08);
        data.push(0);

        let mut cursor = Cursor::new(data);
        let mut reader = Kv1Reader::new(&mut cursor);
        let result = reader.parse().unwrap();
        if let Kv1Value::Object(map) = result {
            assert_eq!(map.get("version"), Some(&Kv1Value::String("1".into())));
            assert_eq!(map.get("appid"), Some(&Kv1Value::Int32(730)));
        } else {
            panic!("Expected object");
        }
    }

    #[test]
    fn test_rejects_excessive_nesting_depth() {
        let mut data = vec![0x00];
        data.push(0); // empty root name
        for _ in 0..(MAX_KV1_DEPTH + 5) {
            data.push(0x00); // Object
            data.push(0); // empty name
        }
        let mut cursor = Cursor::new(data);
        let mut reader = Kv1Reader::new(&mut cursor);
        let err = reader.parse().unwrap_err();
        assert!(matches!(err, Kv1Error::Parse(_)));
    }

    #[test]
    fn test_varint_overflow_on_10th_byte() {
        // 9 continuation bytes + 10th byte with value 2 (overflows u64)
        let data = vec![0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02];
        let mut cursor = Cursor::new(data);
        let mut reader = Kv1Reader::new(&mut cursor);
        assert!(matches!(reader.read_varint(), Err(Kv1Error::Parse(_))));

        // u64::MAX should still decode cleanly
        let max_bytes = vec![0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01];
        let mut cursor_max = Cursor::new(max_bytes);
        let mut reader_max = Kv1Reader::new(&mut cursor_max);
        assert_eq!(reader_max.read_varint().unwrap(), u64::MAX);
    }

    #[test]
    fn test_truncated_large_length_returns_unexpected_eof_without_oom() {
        // Object with string claiming large length but truncated
        let mut data = vec![0x00, 0]; // root object
        data.push(0x01); // string type
        data.extend_from_slice(b"s\0"); // key
        // varint for large length: 0x80, 0x80, 0x80, 0x10 (claims 33MB)
        data.extend_from_slice(&[0x80, 0x80, 0x80, 0x10]);
        data.extend_from_slice(b"abc"); // only 3 bytes

        let mut cursor = Cursor::new(data);
        let mut reader = Kv1Reader::new(&mut cursor);
        assert!(matches!(reader.parse(), Err(Kv1Error::UnexpectedEof)));
    }

    #[test]
    fn test_extract_achievements_and_find_stats_file() {
        let mut ach = BTreeMap::new();
        ach.insert("name".into(), Kv1Value::String("ACH_FIRST_BLOOD".into()));
        ach.insert("unlocked".into(), Kv1Value::Int32(1));
        ach.insert("unlockTime".into(), Kv1Value::UInt64(1_700_000_000));
        ach.insert("progress".into(), Kv1Value::Int32(5));
        ach.insert("maxProgress".into(), Kv1Value::Int32(10));

        let root = Kv1Value::Array(vec![Kv1Value::Object(ach)]);
        let extracted = extract_achievements_kv1(&root);
        let item = extracted.get("ACH_FIRST_BLOOD").expect("achievement");
        assert!(item.unlocked);
        assert_eq!(item.unlock_time, Some(1_700_000_000));
        assert_eq!(item.progress, Some(5));
        assert_eq!(item.max_progress, Some(10));

        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join("config");
        let appcache_stats = dir.path().join("appcache").join("stats");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&appcache_stats).unwrap();

        let stats_file = appcache_stats.join("UserGameStats_123_456.bin");
        std::fs::write(&stats_file, b"test").unwrap();
        assert_eq!(
            find_user_game_stats_file(&config_dir, 123, 456),
            Some(stats_file)
        );
    }
}
