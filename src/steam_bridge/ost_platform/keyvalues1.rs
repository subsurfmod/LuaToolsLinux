//! Valve Binary KeyValues (KV1) parser & serializer for Steam's `UserGameStats_*.bin` format.
//!
//! Wire format (Valve's binary KeyValues v1):
//! - `0x00`: Subtable start (NUL-terminated UTF-8 key, followed by child entries until `0x08` or `0x0B`)
//! - `0x01`: String (NUL-terminated UTF-8 key, NUL-terminated UTF-8 value)
//! - `0x02`: Int32 (NUL-terminated UTF-8 key, 4-byte LE `i32`)
//! - `0x03`: Float32 (NUL-terminated UTF-8 key, 4-byte LE `f32`)
//! - `0x04`: Pointer (NUL-terminated UTF-8 key, 4-byte LE `u32`)
//! - `0x05`: WideString (NUL-terminated UTF-8 key, NUL-terminated UTF-16LE string)
//! - `0x06`: Color (NUL-terminated UTF-8 key, 4-byte LE `u32` RGBA)
//! - `0x07`: UInt64 (NUL-terminated UTF-8 key, 8-byte LE `u64`)
//! - `0x08`: End of subtable (bare byte, NO key)
//! - `0x0A`: Int64 (NUL-terminated UTF-8 key, 8-byte LE `i64`)
//! - `0x0B`: Alternate end of subtable `TYPE_ALTEND` (bare byte, NO key)

use std::collections::BTreeMap;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
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
    Int64(i64),
    Object(BTreeMap<String, Kv1Value>),
    Array(Vec<Kv1Value>),
}

#[derive(Debug, Clone)]
pub struct Kv1Node {
    pub name: String,
    pub value: Kv1Value,
}

const MAX_KV1_NAME_LEN: usize = 64 * 1024;
const MAX_KV1_WIDE_CHARS: usize = 64 * 1024;
const MAX_KV1_DEPTH: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum Kv1Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Unexpected EOF")]
    UnexpectedEof,
    #[error("Invalid node type: 0x{0:02x}")]
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
        // Valve KV1 starts directly with the root node (typically 0x00 Object)
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

        // 0x08 (TYPE_NUMTYPES) and 0x0B (TYPE_ALTEND) are bare end-of-table markers (no key)
        if node_type == 0x08 || node_type == 0x0B {
            return Err(Kv1Error::Parse("Unexpected end-of-table marker".into()));
        }

        let name = self.read_null_terminated_string()?;

        let value = match node_type {
            0x00 => Kv1Value::Object(self.read_object(depth + 1)?),
            0x01 => Kv1Value::String(self.read_null_terminated_string()?),
            0x02 => Kv1Value::Int32(self.read_i32_le()?),
            0x03 => Kv1Value::Float(self.read_f32_le()?),
            0x04 => Kv1Value::Pointer(self.read_u32_le()? as u64),
            0x05 => Kv1Value::WideString(self.read_wide_string_nul()?),
            0x06 => Kv1Value::Color(self.read_u32_le()?),
            0x07 => Kv1Value::UInt64(self.read_u64_le()?),
            0x0A => Kv1Value::Int64(self.read_i64_le()?),
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
            if node_type == 0x08 || node_type == 0x0B {
                // Real Valve binary KV1: bare end-of-table marker byte, NO trailing key string
                self.read_u8()?;
                break;
            }
            let (child_name, child_value) = self.read_node(depth)?;
            map.insert(child_name, child_value);
        }
        Ok(map)
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

    /// Reads a NUL-terminated UTF-16LE string (`0x05` WideString in Valve binary KV1).
    fn read_wide_string_nul(&mut self) -> Result<String, Kv1Error> {
        let mut units = Vec::new();
        loop {
            let code_unit = self.read_u16_le()?;
            if code_unit == 0 {
                break;
            }
            if units.len() >= MAX_KV1_WIDE_CHARS {
                return Err(Kv1Error::Parse(format!(
                    "WideString length exceeds limit ({})",
                    MAX_KV1_WIDE_CHARS
                )));
            }
            units.push(code_unit);
        }
        Ok(String::from_utf16(&units)?)
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

    fn read_i64_le(&mut self) -> Result<i64, Kv1Error> {
        let mut buf = [0u8; 8];
        self.read_exact_checked(&mut buf)?;
        Ok(i64::from_le_bytes(buf))
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

/// Serialize a `Kv1Value` back into Valve binary KV1 wire format (round-trip compatible).
pub fn write_kv1(root_name: &str, value: &Kv1Value) -> Result<Vec<u8>, Kv1Error> {
    let mut out = Vec::new();
    write_kv1_node(root_name, value, &mut out, 0)?;
    out.push(0x08); // root-level terminator
    Ok(out)
}

fn write_kv1_node(
    name: &str,
    value: &Kv1Value,
    out: &mut Vec<u8>,
    depth: usize,
) -> Result<(), Kv1Error> {
    if depth > MAX_KV1_DEPTH {
        return Err(Kv1Error::Parse(format!(
            "Maximum KV1 nesting depth ({}) exceeded",
            MAX_KV1_DEPTH
        )));
    }
    match value {
        Kv1Value::Object(map) => {
            out.push(0x00);
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            for (k, v) in map {
                write_kv1_node(k, v, out, depth + 1)?;
            }
            out.push(0x08);
        }
        Kv1Value::Array(arr) => {
            out.push(0x00);
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            for (idx, v) in arr.iter().enumerate() {
                write_kv1_node(&idx.to_string(), v, out, depth + 1)?;
            }
            out.push(0x08);
        }
        Kv1Value::String(s) => {
            out.push(0x01);
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            out.extend_from_slice(s.as_bytes());
            out.push(0);
        }
        Kv1Value::Int32(n) => {
            out.push(0x02);
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            out.extend_from_slice(&n.to_le_bytes());
        }
        Kv1Value::Float(f) => {
            out.push(0x03);
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            out.extend_from_slice(&f.to_le_bytes());
        }
        Kv1Value::Pointer(p) => {
            out.push(0x04);
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            out.extend_from_slice(&(*p as u32).to_le_bytes());
        }
        Kv1Value::WideString(ws) => {
            out.push(0x05);
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            for u in ws.encode_utf16() {
                out.extend_from_slice(&u.to_le_bytes());
            }
            out.extend_from_slice(&0u16.to_le_bytes());
        }
        Kv1Value::Color(c) => {
            out.push(0x06);
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            out.extend_from_slice(&c.to_le_bytes());
        }
        Kv1Value::UInt64(n) => {
            out.push(0x07);
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            out.extend_from_slice(&n.to_le_bytes());
        }
        Kv1Value::Int64(n) => {
            out.push(0x0A);
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            out.extend_from_slice(&n.to_le_bytes());
        }
    }
    Ok(())
}

/// Parse a Valve KV1 buffer in memory.
pub fn parse_kv1_bytes(data: &[u8]) -> Result<Kv1Value, Kv1Error> {
    let mut reader = Kv1Reader::new(Cursor::new(data));
    reader.parse()
}

/// Parse a Valve KV1 file from disk.
pub fn parse_kv1_file<P: AsRef<Path>>(path: P) -> Result<Kv1Value, Kv1Error> {
    let file = std::fs::File::open(path)?;
    let mut reader = Kv1Reader::new(BufReader::new(file));
    reader.parse()
}

#[derive(Debug, Clone, Default)]
pub struct AchievementData {
    pub name: String,
    pub unlocked: bool,
    pub unlock_time: Option<u32>,
    pub progress: Option<i32>,
    pub max_progress: Option<i32>,
}

/// Extract achievements from a parsed Steam `UserGameStats` KV1 tree.
///
/// Supports both:
/// 1. Real Steam `UserGameStats_<account>_<app>.bin` layout (`AchievementTimes` subtable + stat-ID bitmasks)
/// 2. Named `{ "name", "unlocked", "unlockTime", "progress", "maxProgress" }` objects
pub fn extract_achievements_kv1(kv1: &Kv1Value) -> BTreeMap<String, AchievementData> {
    extract_achievements_with_schema(kv1, None)
}

/// Extract achievements from `UserGameStats` KV1, optionally resolving `(stat_id, bit)` pairs
/// to canonical API names using a parsed `UserGameStatsSchema_<app>.bin` KV1 tree.
pub fn extract_achievements_with_schema(
    kv1: &Kv1Value,
    schema: Option<&Kv1Value>,
) -> BTreeMap<String, AchievementData> {
    let mut achievements = BTreeMap::new();
    let schema_names = schema
        .map(extract_schema_bit_names)
        .unwrap_or_default();

    // 1. Parse real Steam `AchievementTimes` + numeric stat-ID bitmasks
    if let Kv1Value::Object(root) = kv1 {
        extract_steam_bitmask_achievements(root, &schema_names, &mut achievements);
    }

    // 2. Also support named `{ name, unlocked, ... }` objects
    extract_achievements_recursive(kv1, &mut achievements, 0);
    achievements
}

/// Extracts `(stat_id, bit_index) -> achievement_api_name` from `UserGameStatsSchema_<app>.bin`.
fn extract_schema_bit_names(schema: &Kv1Value) -> BTreeMap<(u32, u32), String> {
    let mut map = BTreeMap::new();
    let Kv1Value::Object(root) = schema else {
        return map;
    };
    let stats_obj = root
        .get("stats")
        .and_then(|v| match v {
            Kv1Value::Object(o) => Some(o),
            _ => None,
        })
        .unwrap_or(root);

    for (stat_key, stat_val) in stats_obj {
        let Ok(stat_id) = stat_key.parse::<u32>() else {
            continue;
        };
        let Kv1Value::Object(stat_map) = stat_val else {
            continue;
        };
        if let Some(Kv1Value::Object(bits)) = stat_map.get("bits") {
            for (bit_key, bit_val) in bits {
                let Ok(bit_idx) = bit_key.parse::<u32>() else {
                    continue;
                };
                if let Kv1Value::Object(bit_obj) = bit_val {
                    if let Some(Kv1Value::String(api_name)) = bit_obj.get("name") {
                        map.insert((stat_id, bit_idx), api_name.clone());
                    }
                }
            }
        }
    }
    map
}

fn extract_steam_bitmask_achievements(
    root: &BTreeMap<String, Kv1Value>,
    schema_names: &BTreeMap<(u32, u32), String>,
    out: &mut BTreeMap<String, AchievementData>,
) {
    // Steam stores unlock timestamps under `AchievementTimes -> <stat_id> -> <bit_idx> = <unix_ts>`
    if let Some(Kv1Value::Object(times_root)) = root.get("AchievementTimes") {
        for (stat_key, stat_val) in times_root {
            let Ok(stat_id) = stat_key.parse::<u32>() else {
                continue;
            };
            let Kv1Value::Object(bits_map) = stat_val else {
                continue;
            };
            for (bit_key, ts_val) in bits_map {
                let Ok(bit_idx) = bit_key.parse::<u32>() else {
                    continue;
                };
                let unlock_ts = match ts_val {
                    Kv1Value::Int32(t) if *t > 0 => Some(*t as u32),
                    Kv1Value::UInt64(t) if *t > 0 => u32::try_from(*t).ok(),
                    Kv1Value::Int64(t) if *t > 0 => u32::try_from(*t).ok(),
                    _ => None,
                };
                let name = schema_names
                    .get(&(stat_id, bit_idx))
                    .cloned()
                    .unwrap_or_else(|| format!("STAT_{}_BIT_{}", stat_id, bit_idx));
                out.insert(
                    name.clone(),
                    AchievementData {
                        name,
                        unlocked: unlock_ts.is_some(),
                        unlock_time: unlock_ts,
                        progress: None,
                        max_progress: None,
                    },
                );
            }
        }
    }

    // Also inspect numeric stat-ID bitmasks when schema identifies which stat IDs are achievement bitfields
    for (&(stat_id, bit_idx), api_name) in schema_names {
        let stat_key = stat_id.to_string();
        let bitmask = root.get(&stat_key).and_then(|v| match v {
            Kv1Value::Int32(n) => Some(*n as u32),
            Kv1Value::UInt64(n) => Some(*n as u32),
            Kv1Value::Int64(n) => Some(*n as u32),
            _ => None,
        });
        if let Some(mask) = bitmask {
            let unlocked = bit_idx < 32 && ((mask >> bit_idx) & 1) != 0;
            out.entry(api_name.clone())
                .and_modify(|e| {
                    if unlocked {
                        e.unlocked = true;
                    }
                })
                .or_insert_with(|| AchievementData {
                    name: api_name.clone(),
                    unlocked,
                    unlock_time: None,
                    progress: None,
                    max_progress: None,
                });
        }
    }
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
                    Some(Kv1Value::Int64(t)) => ach.unlock_time = u32::try_from(*t).ok(),
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

/// Find the Steam stats file for a given app/account.
pub fn find_user_game_stats_file(
    steam_config_dir: &Path,
    account_id: u32,
    app_id: u32,
) -> Option<std::path::PathBuf> {
    let pattern = format!("UserGameStats_{}_{}.bin", account_id, app_id);
    if let Some(parent) = steam_config_dir.parent() {
        let appcache_stats = parent.join("appcache").join("stats").join(&pattern);
        if appcache_stats.is_file() {
            return Some(appcache_stats);
        }
    }
    let stats_dir = steam_config_dir.join("stats");
    if stats_dir.is_dir() {
        let path = stats_dir.join(&pattern);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_string() {
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
    fn test_parse_simple_int32_pointer_widestring_int64_and_altend() {
        // Real Valve binary KV1: bare 0x08 / 0x0B terminators, 4-byte LE Pointer, NUL-terminated UTF-16LE WideString, 8-byte LE Int64
        let mut data = vec![0x00];
        data.extend_from_slice(b"root\0");
        // Int32 (0x02)
        data.push(0x02);
        data.extend_from_slice(b"count\0");
        data.extend_from_slice(&42i32.to_le_bytes());
        // Pointer (0x04) = 4-byte LE u32
        data.push(0x04);
        data.extend_from_slice(b"ptr\0");
        data.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        // WideString (0x05) = NUL-terminated UTF-16LE "Hi"
        data.push(0x05);
        data.extend_from_slice(b"wide\0");
        for u in "Hi".encode_utf16() {
            data.extend_from_slice(&u.to_le_bytes());
        }
        data.extend_from_slice(&0u16.to_le_bytes());
        // Int64 (0x0A) = 8-byte LE i64
        data.push(0x0A);
        data.extend_from_slice(b"signed64\0");
        data.extend_from_slice(&(-987_654_321_012i64).to_le_bytes());
        // Alternate end marker 0x0B (bare byte, NO trailing NUL)
        data.push(0x0B);

        let parsed = parse_kv1_bytes(&data).unwrap();
        let Kv1Value::Object(map) = parsed else {
            panic!("expected object");
        };
        assert_eq!(map.get("count"), Some(&Kv1Value::Int32(42)));
        assert_eq!(map.get("ptr"), Some(&Kv1Value::Pointer(0xDEAD_BEEF)));
        assert_eq!(map.get("wide"), Some(&Kv1Value::WideString("Hi".into())));
        assert_eq!(
            map.get("signed64"),
            Some(&Kv1Value::Int64(-987_654_321_012))
        );
    }

    #[test]
    fn test_parse_nested_objects_with_bare_0x08_sibling_preservation() {
        // Nested sibling objects terminated with bare 0x08 (proving we do NOT consume sibling type bytes)
        let mut data = vec![0x00];
        data.extend_from_slice(b"root\0");
        // Child object 1
        data.push(0x00);
        data.extend_from_slice(b"first\0");
        data.push(0x01);
        data.extend_from_slice(b"k1\0");
        data.extend_from_slice(b"v1\0");
        data.push(0x08); // bare end of "first"
        // Child object 2 immediately follows 0x08!
        data.push(0x00);
        data.extend_from_slice(b"second\0");
        data.push(0x02);
        data.extend_from_slice(b"k2\0");
        data.extend_from_slice(&99i32.to_le_bytes());
        data.push(0x08); // bare end of "second"
        data.push(0x08); // bare end of "root"

        let parsed = parse_kv1_bytes(&data).unwrap();
        let Kv1Value::Object(root) = &parsed else {
            panic!("expected root object");
        };
        assert!(root.contains_key("first"));
        assert!(root.contains_key("second"));

        // Verify round-trip serialization -> parse
        let encoded = write_kv1("root", &parsed).unwrap();
        let reparsed = parse_kv1_bytes(&encoded).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn test_rejects_excessive_nesting_depth() {
        let mut data = vec![0x00, 0];
        for _ in 0..(MAX_KV1_DEPTH + 5) {
            data.push(0x00);
            data.push(0);
        }
        let err = parse_kv1_bytes(&data).unwrap_err();
        assert!(matches!(err, Kv1Error::Parse(_)));
    }

    #[test]
    fn test_truncated_string_returns_unexpected_eof() {
        let mut data = vec![0x00, 0];
        data.push(0x01);
        data.extend_from_slice(b"s\0");
        data.extend_from_slice(b"unterminated");
        assert!(matches!(parse_kv1_bytes(&data), Err(Kv1Error::UnexpectedEof)));
    }

    #[test]
    fn test_real_steam_usergamestats_bitmask_and_schema_extraction() {
        // Real Steam UserGameStats layout: stat "1" has bitmask 0b101 (bits 0 and 2 unlocked)
        // plus AchievementTimes -> "1" -> "0" = 1700000100, "2" = 1700000200
        let mut times_stat1 = BTreeMap::new();
        times_stat1.insert("0".into(), Kv1Value::Int32(1_700_000_100));
        times_stat1.insert("2".into(), Kv1Value::Int32(1_700_000_200));
        let mut ach_times = BTreeMap::new();
        ach_times.insert("1".into(), Kv1Value::Object(times_stat1));

        let mut stats_root = BTreeMap::new();
        stats_root.insert("1".into(), Kv1Value::Int32(0b101));
        stats_root.insert("AchievementTimes".into(), Kv1Value::Object(ach_times));
        let stats_kv = Kv1Value::Object(stats_root);

        // Round-trip through real binary KV1 bytes
        let raw_bin = write_kv1("UserGameStats", &stats_kv).unwrap();
        let parsed_stats = parse_kv1_bytes(&raw_bin).unwrap();

        // Schema mapping (stat 1, bit 0) -> "ACH_WIN_ONE", (stat 1, bit 1) -> "ACH_LOCKED", (stat 1, bit 2) -> "ACH_BOSS"
        let mut bit0 = BTreeMap::new();
        bit0.insert("name".into(), Kv1Value::String("ACH_WIN_ONE".into()));
        let mut bit1 = BTreeMap::new();
        bit1.insert("name".into(), Kv1Value::String("ACH_LOCKED".into()));
        let mut bit2 = BTreeMap::new();
        bit2.insert("name".into(), Kv1Value::String("ACH_BOSS".into()));
        let mut bits = BTreeMap::new();
        bits.insert("0".into(), Kv1Value::Object(bit0));
        bits.insert("1".into(), Kv1Value::Object(bit1));
        bits.insert("2".into(), Kv1Value::Object(bit2));
        let mut stat1_schema = BTreeMap::new();
        stat1_schema.insert("bits".into(), Kv1Value::Object(bits));
        let mut schema_stats = BTreeMap::new();
        schema_stats.insert("1".into(), Kv1Value::Object(stat1_schema));
        let mut schema_root = BTreeMap::new();
        schema_root.insert("stats".into(), Kv1Value::Object(schema_stats));
        let schema_kv = Kv1Value::Object(schema_root);

        let extracted = extract_achievements_with_schema(&parsed_stats, Some(&schema_kv));
        assert_eq!(extracted.len(), 3);
        assert!(extracted["ACH_WIN_ONE"].unlocked);
        assert_eq!(extracted["ACH_WIN_ONE"].unlock_time, Some(1_700_000_100));
        assert!(!extracted["ACH_LOCKED"].unlocked);
        assert!(extracted["ACH_BOSS"].unlocked);
        assert_eq!(extracted["ACH_BOSS"].unlock_time, Some(1_700_000_200));
    }

    #[test]
    fn test_extract_named_achievements_and_find_stats_file() {
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
