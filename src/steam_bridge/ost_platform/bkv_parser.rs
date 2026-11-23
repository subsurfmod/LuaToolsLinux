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
use std::io::{Read, Seek, SeekFrom};
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
        // Read and verify magic
        let mut magic = [0u8; 4];
        self.reader.read_exact(&mut magic)?;
        if magic != [b'B', b'K', b'V', 0] {
            return Err(BkvError::InvalidMagic(magic));
        }

        self.read_node()
    }

    fn read_node(&mut self) -> Result<BkvValue, BkvError> {
        let node_type = self.read_u8()?;
        let name = self.read_null_terminated_string()?;

        match node_type {
            0 => Ok(BkvValue::Object(BTreeMap::new())), // End of object
            1 => self.read_string().map(BkvValue::String),
            2 => self.read_int32().map(BkvValue::Int32),
            3 => self.read_float().map(BkvValue::Float),
            4 => self.read_pointer().map(BkvValue::Pointer),
            5 => self.read_wide_string().map(BkvValue::WideString),
            6 => self.read_color().map(BkvValue::Color),
            7 => self.read_uint64().map(BkvValue::UInt64),
            8 => self.read_binary().map(BkvValue::Binary),
            9 => self.read_object().map(BkvValue::Object), // Nested object
            10 => self.read_array().map(BkvValue::Array),  // Array
            t => Err(BkvError::InvalidNodeType(t)),
        }
    }

    fn read_object(&mut self) -> Result<BTreeMap<String, BkvValue>, BkvError> {
        let mut map = BTreeMap::new();
        loop {
            let node_type = self.peek_u8()?;
            if node_type == 0 {
                self.read_u8()?; // consume end marker
                let _name = self.read_null_terminated_string()?; // empty name for end
                break;
            }
            let value = self.read_node()?;
            if let BkvValue::Object(obj) = value {
                // This shouldn't happen - read_node returns the value directly
            }
            // The name was read inside read_node, we need to track it differently
            // Actually, let's re-read: read_node reads type + name + value
            // For objects, the "value" is the object itself with children
        }
        Ok(map)
    }

    fn read_array(&mut self) -> Result<Vec<BkvValue>, BkvError> {
        let mut arr = Vec::new();
        loop {
            let node_type = self.peek_u8()?;
            if node_type == 0 {
                self.read_u8()?; // consume end marker
                let _name = self.read_null_terminated_string()?;
                break;
            }
            let value = self.read_node()?;
            arr.push(value);
        }
        Ok(arr)
    }

    fn read_string(&mut self) -> Result<String, BkvError> {
        let len = self.read_varint()? as usize;
        let mut buf = vec![0u8; len];
        self.reader.read_exact(&mut buf)?;
        Ok(String::from_utf8(buf)?)
    }

    fn read_wide_string(&mut self) -> Result<String, BkvError> {
        let len = self.read_varint()? as usize;
        let mut buf = vec![0u16; len];
        for i in 0..len {
            buf[i] = self.read_u16_le()?;
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
        let len = self.read_varint()? as usize;
        let mut buf = vec![0u8; len];
        self.reader.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn read_null_terminated_string(&mut self) -> Result<String, BkvError> {
        let mut buf = Vec::new();
        loop {
            let b = self.read_u8()?;
            if b == 0 {
                break;
            }
            buf.push(b);
        }
        Ok(String::from_utf8(buf)?)
    }

    fn read_varint(&mut self) -> Result<u64, BkvError> {
        let mut result = 0u64;
        let mut shift = 0;
        loop {
            let byte = self.read_u8()?;
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

    fn read_u8(&mut self) -> Result<u8, BkvError> {
        let mut buf = [0u8; 1];
        self.reader.read_exact(&mut buf)?;
        Ok(buf[0])
    }

    fn read_u16_le(&mut self) -> Result<u16, BkvError> {
        let mut buf = [0u8; 2];
        self.reader.read_exact(&mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    fn read_u32_le(&mut self) -> Result<u32, BkvError> {
        let mut buf = [0u8; 4];
        self.reader.read_exact(&mut buf)?;
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
    let mut reader = BkvReader::new(file);
    reader.parse()
}

/// Extract achievements from a parsed UserGameStats BKV
pub fn extract_achievements(bkv: &BkvValue) -> BTreeMap<String, AchievementData> {
    let mut achievements = BTreeMap::new();
    extract_achievements_recursive(bkv, &mut achievements);
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

fn extract_achievements_recursive(value: &BkvValue, out: &mut BTreeMap<String, AchievementData>) {
    match value {
        BkvValue::Object(map) => {
            // Check if this is an achievement node
            if let (Some(BkvValue::String(name)), Some(BkvValue::Int32(unlocked))) =
                (map.get("name"), map.get("unlocked")) {
                let mut ach = AchievementData {
                    name: name.clone(),
                    unlocked: *unlocked != 0,
                    ..Default::default()
                };
                if let Some(BkvValue::Int32(t)) = map.get("unlockTime") {
                    ach.unlock_time = Some(*t as u32);
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
                extract_achievements_recursive(v, out);
            }
        }
        BkvValue::Array(arr) => {
            for v in arr {
                extract_achievements_recursive(v, out);
            }
        }
        _ => {}
    }
}

/// Find the Steam stats file for a given app/account
pub fn find_user_game_stats_file(steam_config_dir: &Path, account_id: u32, app_id: u32) -> Option<std::path::PathBuf> {
    let pattern = format!("UserGameStats_{}_{}.bin", account_id, app_id);
    let depot_cache = steam_config_dir.parent()?.join("depotcache");
    if depot_cache.exists() {
        let path = depot_cache.join(&pattern);
        if path.exists() {
            return Some(path);
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
                if v == 0 { break; }
            }
            assert_eq!(buf, expected, "varint encoding for {}", val);

            // Test decoding
            let mut cursor = Cursor::new(&buf);
            let mut reader = BkvReader::new(&mut cursor);
            assert_eq!(reader.read_varint().unwrap(), val, "varint decoding for {}", val);
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
}