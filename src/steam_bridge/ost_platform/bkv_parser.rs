//! Binary KeyValues (BKV) parser for Steam's UserGameStats binary format.
//!
//! Ported from usefulsteamtools `LocalAchievements::BkvRead`.
//! Used to parse `UserGameStats_<accountId>_<appId>.bin` files.
//!
//! Format (reverse-engineered):
//! - Magic: "BKV\0" (4 bytes)
//! - Root node: type + name + children
//! - Node types: 0=End, 1=String, 2=Int32, 3=Float, 4=Pointer, 5=WideString, 6=Color, 7=UInt64, 8=End (alt)
//! - Variable-length encoding for integers

use std::collections::BTreeMap;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum BkvValue {
    String(String),
    Int32(i32),
    Float(f32),
    Pointer(u64),
    WideString(String),
    Color(u32),
    UInt64(u64),
    Binary(Vec<u8>),
    Object(BTreeMap<String, BkvValue>),
    Array(Vec<BkvValue>),
}

#[derive(Debug, Clone)]
pub struct BkvNode {
    pub name: String,
    pub value: BkvValue,
}

const MAX_BKV_ALLOC_LEN: usize = 64 * 1024 * 1024;
const MAX_BKV_NAME_LEN: usize = 64 * 1024;
const MAX_BKV_DEPTH: usize = 64;
const READ_CHUNK_SIZE: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum BkvError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Invalid magic: expected BKV\\0, got {0:?}")]
    InvalidMagic([u8; 4]),
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

pub struct BkvReader<R: Read + Seek> {
    reader: R,
}

impl<R: Read + Seek> BkvReader<R> {
    pub fn new(reader: R) -> Self {
        Self { reader }
    }

    pub fn parse(&mut self) -> Result<BkvValue, BkvError> {
        let mut magic = [0u8; 4];
        self.read_exact_checked(&mut magic)?;
        if magic != [b'B', b'K', b'V', 0] {
            return Err(BkvError::InvalidMagic(magic));
        }

        let (_name, value) = self.read_node(0)?;
        Ok(value)
    }

    fn read_node(&mut self, depth: usize) -> Result<(String, BkvValue), BkvError> {
        if depth > MAX_BKV_DEPTH {
            return Err(BkvError::Parse(format!(
                "Maximum BKV nesting depth ({}) exceeded",
                MAX_BKV_DEPTH
            )));
        }

        let node_type = self.read_u8()?;
        let name = self.read_null_terminated_string()?;

        let value = match node_type {
            0 => BkvValue::Object(BTreeMap::new()),
            1 => BkvValue::String(self.read_string()?),
            2 => BkvValue::Int32(self.read_int32()?),
            3 => BkvValue::Float(self.read_float()?),
            4 => BkvValue::Pointer(self.read_pointer()?),
            5 => BkvValue::WideString(self.read_wide_string()?),
            6 => BkvValue::Color(self.read_color()?),
            7 => BkvValue::UInt64(self.read_uint64()?),
            8 => BkvValue::Binary(self.read_binary()?),
            9 => BkvValue::Object(self.read_object(depth + 1)?),
            10 => BkvValue::Array(self.read_array(depth + 1)?),
            t => return Err(BkvError::InvalidNodeType(t)),
        };
        Ok((name, value))
    }

    fn read_object(&mut self, depth: usize) -> Result<BTreeMap<String, BkvValue>, BkvError> {
        if depth > MAX_BKV_DEPTH {
            return Err(BkvError::Parse(format!(
                "Maximum BKV nesting depth ({}) exceeded",
                MAX_BKV_DEPTH
            )));
        }
        let mut map = BTreeMap::new();
        loop {
            let node_type = self.peek_u8()?;
            if node_type == 0 {
                self.read_u8()?;
                let _name = self.read_null_terminated_string()?;
                break;
            }
            let (child_name, child_value) = self.read_node(depth)?;
            map.insert(child_name, child_value);
        }
        Ok(map)
    }

    fn read_array(&mut self, depth: usize) -> Result<Vec<BkvValue>, BkvError> {
        if depth > MAX_BKV_DEPTH {
            return Err(BkvError::Parse(format!(
                "Maximum BKV nesting depth ({}) exceeded",
                MAX_BKV_DEPTH
            )));
        }
        let mut arr = Vec::new();
        loop {
            let node_type = self.peek_u8()?;
            if node_type == 0 {
                self.read_u8()?;
                let _name = self.read_null_terminated_string()?;
                break;
            }
            let (_child_name, value) = self.read_node(depth)?;
            arr.push(value);
        }
        Ok(arr)
    }

    fn read_bounded_bytes(&mut self, len: usize) -> Result<Vec<u8>, BkvError> {
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

    fn read_string(&mut self) -> Result<String, BkvError> {
        let raw_len = self.read_varint()?;
        if raw_len > MAX_BKV_ALLOC_LEN as u64 {
            return Err(BkvError::Parse(format!(
                "String length {} exceeds limit",
                raw_len
            )));
        }
        let buf = self.read_bounded_bytes(raw_len as usize)?;
        Ok(String::from_utf8(buf)?)
    }

    fn read_wide_string(&mut self) -> Result<String, BkvError> {
        let raw_len = self.read_varint()?;
        if raw_len > (MAX_BKV_ALLOC_LEN / 2) as u64 {
            return Err(BkvError::Parse(format!(
                "WideString length {} exceeds limit",
                raw_len
            )));
        }
        let len = raw_len as usize;
        let mut buf = Vec::with_capacity(len.min(READ_CHUNK_SIZE / 2));
        for _ in 0..len {
            buf.push(self.read_u16_le()?);
        }
        Ok(String::from_utf16(&buf)?)
    }

    fn read_int32(&mut self) -> Result<i32, BkvError> {
        Ok(self.read_varint()? as i32)
    }

    fn read_float(&mut self) -> Result<f32, BkvError> {
        let bits = self.read_u32_le()?;
        Ok(f32::from_bits(bits))
    }

    fn read_pointer(&mut self) -> Result<u64, BkvError> {
        self.read_varint()
    }

    fn read_color(&mut self) -> Result<u32, BkvError> {
        self.read_u32_le()
    }

    fn read_uint64(&mut self) -> Result<u64, BkvError> {
        self.read_varint()
    }

    fn read_binary(&mut self) -> Result<Vec<u8>, BkvError> {
        let raw_len = self.read_varint()?;
        if raw_len > MAX_BKV_ALLOC_LEN as u64 {
            return Err(BkvError::Parse(format!(
                "Binary length {} exceeds limit",
                raw_len
            )));
        }
        self.read_bounded_bytes(raw_len as usize)
    }

    fn read_null_terminated_string(&mut self) -> Result<String, BkvError> {
        let mut buf = Vec::new();
        loop {
            let b = self.read_u8()?;
            if b == 0 {
                break;
            }
            if buf.len() >= MAX_BKV_NAME_LEN {
                return Err(BkvError::Parse(format!(
                    "Node name length exceeds limit ({})",
                    MAX_BKV_NAME_LEN
                )));
            }
            buf.push(b);
        }
        Ok(String::from_utf8(buf)?)
    }

    fn read_varint(&mut self) -> Result<u64, BkvError> {
        let mut result = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = self.read_u8()?;
            if shift == 63 && (byte & 0x7F) > 1 {
                return Err(BkvError::Parse("VarInt overflow".into()));
            }
            result |= ((byte & 0x7F) as u64) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 63 {
                return Err(BkvError::Parse("VarInt too large".into()));
            }
        }
        Ok(result)
    }

    fn read_exact_checked(&mut self, buf: &mut [u8]) -> Result<(), BkvError> {
        self.reader.read_exact(buf).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                BkvError::UnexpectedEof
            } else {
                BkvError::Io(e)
            }
        })
    }

    fn read_u8(&mut self) -> Result<u8, BkvError> {
        let mut buf = [0u8; 1];
        self.read_exact_checked(&mut buf)?;
        Ok(buf[0])
    }

    fn read_u16_le(&mut self) -> Result<u16, BkvError> {
        let mut buf = [0u8; 2];
        self.read_exact_checked(&mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    fn read_u32_le(&mut self) -> Result<u32, BkvError> {
        let mut buf = [0u8; 4];
        self.read_exact_checked(&mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    fn peek_u8(&mut self) -> Result<u8, BkvError> {
        let pos = self.reader.stream_position()?;
        let b = self.read_u8()?;
        self.reader.seek(SeekFrom::Start(pos))?;
        Ok(b)
    }
}

/// Parse a BKV file from disk
pub fn parse_bkv_file<P: AsRef<Path>>(path: P) -> Result<BkvValue, BkvError> {
    let file = std::fs::File::open(path)?;
    let mut reader = BkvReader::new(BufReader::new(file));
    reader.parse()
}

/// Extract achievements from a parsed UserGameStats BKV
pub fn extract_achievements(bkv: &BkvValue) -> BTreeMap<String, AchievementData> {
    let mut achievements = BTreeMap::new();
    extract_achievements_recursive(bkv, &mut achievements, 0);
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
    value: &BkvValue,
    out: &mut BTreeMap<String, AchievementData>,
    depth: usize,
) {
    if depth > MAX_BKV_DEPTH {
        return;
    }
    match value {
        BkvValue::Object(map) => {
            // Check if this is an achievement node
            if let (Some(BkvValue::String(name)), Some(BkvValue::Int32(unlocked))) =
                (map.get("name"), map.get("unlocked"))
            {
                let mut ach = AchievementData {
                    name: name.clone(),
                    unlocked: *unlocked != 0,
                    ..Default::default()
                };
                match map.get("unlockTime") {
                    Some(BkvValue::Int32(t)) => ach.unlock_time = Some(*t as u32),
                    Some(BkvValue::UInt64(t)) => ach.unlock_time = u32::try_from(*t).ok(),
                    _ => {}
                }
                if let Some(BkvValue::Int32(p)) = map.get("progress") {
                    ach.progress = Some(*p);
                }
                if let Some(BkvValue::Int32(mp)) = map.get("maxProgress") {
                    ach.max_progress = Some(*mp);
                }
                out.insert(name.clone(), ach);
            }
            // Recurse into children
            for v in map.values() {
                extract_achievements_recursive(v, out, depth + 1);
            }
        }
        BkvValue::Array(arr) => {
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
    fn test_varint_encoding() {
        let test_cases = vec![
            (0u64, vec![0x00]),
            (1u64, vec![0x01]),
            (127u64, vec![0x7F]),
            (128u64, vec![0x80, 0x01]),
            (16383u64, vec![0xFF, 0x7F]),
            (16384u64, vec![0x80, 0x80, 0x01]),
        ];

        for (val, expected) in test_cases {
            let mut buf = Vec::new();
            let mut v = val;
            loop {
                let mut byte = (v & 0x7F) as u8;
                v >>= 7;
                if v != 0 {
                    byte |= 0x80;
                }
                buf.push(byte);
                if v == 0 {
                    break;
                }
            }
            assert_eq!(buf, expected, "varint encoding for {}", val);

            // Test decoding
            let mut cursor = Cursor::new(&buf);
            let mut reader = BkvReader::new(&mut cursor);
            assert_eq!(
                reader.read_varint().unwrap(),
                val,
                "varint decoding for {}",
                val
            );
        }
    }

    #[test]
    fn test_parse_simple_string() {
        // BKV: magic + type(1=string) + name("test") + len(4) + "data"
        let mut data = vec![b'B', b'K', b'V', 0];
        data.push(1); // type = string
        data.extend_from_slice(b"test\0"); // name
        data.push(4); // length = 4
        data.extend_from_slice(b"data"); // value

        let mut cursor = Cursor::new(data);
        let mut reader = BkvReader::new(&mut cursor);
        let result = reader.parse().unwrap();
        assert_eq!(result, BkvValue::String("data".into()));
    }

    #[test]
    fn test_parse_simple_int32() {
        // BKV: magic + type(2=int32) + name("count") + varint(42)
        let mut data = vec![b'B', b'K', b'V', 0];
        data.push(2); // type = int32
        data.extend_from_slice(b"count\0");
        data.push(42); // varint for 42

        let mut cursor = Cursor::new(data);
        let mut reader = BkvReader::new(&mut cursor);
        let result = reader.parse().unwrap();
        assert_eq!(result, BkvValue::Int32(42));
    }

    #[test]
    fn test_parse_nested_object() {
        // BKV: magic + type(9=object) + name("root") + child(type=1,name="child",value="value") + end(0,name="")
        let mut data = vec![b'B', b'K', b'V', 0];
        data.push(9); // type = object
        data.extend_from_slice(b"root\0");
        // Child: string
        data.push(1); // type = string
        data.extend_from_slice(b"child\0");
        data.push(5); // len = 5
        data.extend_from_slice(b"value");
        // End marker
        data.push(0); // type = end
        data.push(0); // empty name

        let mut cursor = Cursor::new(data);
        let mut reader = BkvReader::new(&mut cursor);
        let result = reader.parse().unwrap();
        if let BkvValue::Object(map) = result {
            assert!(map.contains_key("child"));
            assert_eq!(map.get("child"), Some(&BkvValue::String("value".into())));
        } else {
            panic!("Expected object");
        }
    }

    #[test]
    fn test_rejects_excessive_nesting_depth() {
        let mut data = vec![b'B', b'K', b'V', 0];
        for _ in 0..(MAX_BKV_DEPTH + 5) {
            data.push(9); // Object
            data.push(0); // empty name
        }
        let mut cursor = Cursor::new(data);
        let mut reader = BkvReader::new(&mut cursor);
        let err = reader.parse().unwrap_err();
        assert!(matches!(err, BkvError::Parse(_)));
    }

    #[test]
    fn test_varint_overflow_on_10th_byte() {
        // 9 continuation bytes + 10th byte with value 2 (overflows u64)
        let data = vec![0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02];
        let mut cursor = Cursor::new(data);
        let mut reader = BkvReader::new(&mut cursor);
        assert!(matches!(reader.read_varint(), Err(BkvError::Parse(_))));

        // u64::MAX should still decode cleanly
        let max_bytes = vec![0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01];
        let mut cursor_max = Cursor::new(max_bytes);
        let mut reader_max = BkvReader::new(&mut cursor_max);
        assert_eq!(reader_max.read_varint().unwrap(), u64::MAX);
    }

    #[test]
    fn test_truncated_large_length_returns_unexpected_eof_without_oom() {
        // Claims a 32 MiB string in a 12-byte buffer
        let mut data = vec![b'B', b'K', b'V', 0, 1, b's', 0];
        // varint for 33_554_432 (0x0200_0000): 0x80, 0x80, 0x80, 0x10
        data.extend_from_slice(&[0x80, 0x80, 0x80, 0x10]);
        data.extend_from_slice(b"abc");

        let mut cursor = Cursor::new(data);
        let mut reader = BkvReader::new(&mut cursor);
        assert!(matches!(reader.parse(), Err(BkvError::UnexpectedEof)));
    }

    #[test]
    fn test_extract_achievements_and_find_stats_file() {
        let mut ach = BTreeMap::new();
        ach.insert("name".into(), BkvValue::String("ACH_FIRST_BLOOD".into()));
        ach.insert("unlocked".into(), BkvValue::Int32(1));
        ach.insert("unlockTime".into(), BkvValue::UInt64(1_700_000_000));
        ach.insert("progress".into(), BkvValue::Int32(5));
        ach.insert("maxProgress".into(), BkvValue::Int32(10));

        let root = BkvValue::Array(vec![BkvValue::Object(ach)]);
        let extracted = extract_achievements(&root);
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
        std::fs::write(&stats_file, b"BKV\0").unwrap();
        assert_eq!(
            find_user_game_stats_file(&config_dir, 123, 456),
            Some(stats_file)
        );
    }
}
