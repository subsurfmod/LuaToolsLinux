//! Linux implementation of `OSTPlatform::Numbers`.
//!
//! Provides strict numeric string parsing utilities compatible with the Windows
//! `OSTPlatform::Numbers` API (`std::from_chars`-equivalent semantics).

use std::ffi::{c_char, CStr};

use thiserror::Error;

/// A slice of a string matching `OSTPlatform::Numbers::TextSlice`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSlice {
    pub offset: usize,
    pub count: usize,
}

/// Error type for numeric parsing operations.
#[derive(Debug, Error)]
pub enum NumbersError {
    #[error("Invalid numeric format: {0}")]
    InvalidFormat(String),

    #[error("Value out of range: {0}")]
    OutOfRange(String),

    #[error("Invalid hexadecimal format: {0}")]
    InvalidHex(String),

    #[error("Slice out of bounds")]
    SliceOutOfBounds,

    #[error("Null pointer")]
    NullPointer,
}

/// Result type for numeric parsing operations.
pub type NumbersResult<T> = std::result::Result<T, NumbersError>;

fn apply_slice(value: &str, slice: Option<TextSlice>) -> NumbersResult<&str> {
    let Some(s) = slice else {
        return Ok(value);
    };
    if s.offset > value.len() || !value.is_char_boundary(s.offset) {
        return Err(NumbersError::SliceOutOfBounds);
    }
    if s.count == 0 {
        return Ok(&value[s.offset..]);
    }
    let end = s
        .offset
        .checked_add(s.count)
        .ok_or(NumbersError::SliceOutOfBounds)?;
    if end > value.len() || !value.is_char_boundary(end) {
        return Err(NumbersError::SliceOutOfBounds);
    }
    Ok(&value[s.offset..end])
}

fn with_cstr<T>(
    value: *const c_char,
    f: impl FnOnce(&str) -> NumbersResult<T>,
) -> NumbersResult<T> {
    if value.is_null() {
        return Err(NumbersError::NullPointer);
    }
    let s = unsafe { CStr::from_ptr(value) }
        .to_str()
        .map_err(|e| NumbersError::InvalidFormat(e.to_string()))?;
    f(s)
}

fn strip_hex_prefix(value: &str) -> &str {
    value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value)
}

fn wide_to_utf8(value: &[u16]) -> NumbersResult<String> {
    String::from_utf16(value).map_err(|e| NumbersError::InvalidFormat(e.to_string()))
}

fn map_int_err(e: std::num::ParseIntError, raw: &str, is_hex: bool) -> NumbersError {
    match e.kind() {
        std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow => {
            NumbersError::OutOfRange(raw.to_string())
        }
        _ if is_hex => NumbersError::InvalidHex(raw.to_string()),
        _ => NumbersError::InvalidFormat(raw.to_string()),
    }
}

/// Strict decimal parsing for `u64`.
pub fn parse_uint64(value: &str) -> NumbersResult<u64> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(NumbersError::InvalidFormat(value.to_string()));
    }
    value
        .parse::<u64>()
        .map_err(|e| map_int_err(e, value, false))
}

pub fn parse_uint64_wide(value: &[u16]) -> NumbersResult<u64> {
    let utf8 = wide_to_utf8(value)?;
    parse_uint64(&utf8)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn parse_uint64_cstr(value: *const c_char) -> NumbersResult<u64> {
    with_cstr(value, parse_uint64)
}

pub fn parse_uint64_with_slice(value: &str, slice: TextSlice) -> NumbersResult<u64> {
    parse_uint64(apply_slice(value, Some(slice))?)
}

/// Strict decimal parsing for `u32`.
pub fn parse_uint32(value: &str) -> NumbersResult<u32> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(NumbersError::InvalidFormat(value.to_string()));
    }
    value
        .parse::<u32>()
        .map_err(|e| map_int_err(e, value, false))
}

pub fn parse_uint32_wide(value: &[u16]) -> NumbersResult<u32> {
    let utf8 = wide_to_utf8(value)?;
    parse_uint32(&utf8)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn parse_uint32_cstr(value: *const c_char) -> NumbersResult<u32> {
    with_cstr(value, parse_uint32)
}

pub fn parse_uint32_with_slice(value: &str, slice: TextSlice) -> NumbersResult<u32> {
    parse_uint32(apply_slice(value, Some(slice))?)
}

/// Strict decimal parsing for `i64`.
pub fn parse_int64(value: &str) -> NumbersResult<i64> {
    let digits = value.strip_prefix('-').unwrap_or(value);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(NumbersError::InvalidFormat(value.to_string()));
    }
    value
        .parse::<i64>()
        .map_err(|e| map_int_err(e, value, false))
}

pub fn parse_int64_wide(value: &[u16]) -> NumbersResult<i64> {
    let utf8 = wide_to_utf8(value)?;
    parse_int64(&utf8)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn parse_int64_cstr(value: *const c_char) -> NumbersResult<i64> {
    with_cstr(value, parse_int64)
}

pub fn parse_int64_with_slice(value: &str, slice: TextSlice) -> NumbersResult<i64> {
    parse_int64(apply_slice(value, Some(slice))?)
}

/// Strict decimal parsing for `i32`.
pub fn parse_int32(value: &str) -> NumbersResult<i32> {
    let digits = value.strip_prefix('-').unwrap_or(value);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(NumbersError::InvalidFormat(value.to_string()));
    }
    value
        .parse::<i32>()
        .map_err(|e| map_int_err(e, value, false))
}

pub fn parse_int32_wide(value: &[u16]) -> NumbersResult<i32> {
    let utf8 = wide_to_utf8(value)?;
    parse_int32(&utf8)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn parse_int32_cstr(value: *const c_char) -> NumbersResult<i32> {
    with_cstr(value, parse_int32)
}

pub fn parse_int32_with_slice(value: &str, slice: TextSlice) -> NumbersResult<i32> {
    parse_int32(apply_slice(value, Some(slice))?)
}

/// Strict hexadecimal parsing for `u64` (optional `0x` / `0X` prefix).
pub fn parse_hex_uint64(value: &str) -> NumbersResult<u64> {
    let hex = strip_hex_prefix(value);
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(NumbersError::InvalidHex(value.to_string()));
    }
    u64::from_str_radix(hex, 16).map_err(|e| map_int_err(e, value, true))
}

pub fn parse_hex_uint64_wide(value: &[u16]) -> NumbersResult<u64> {
    let utf8 = wide_to_utf8(value)?;
    parse_hex_uint64(&utf8)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn parse_hex_uint64_cstr(value: *const c_char) -> NumbersResult<u64> {
    with_cstr(value, parse_hex_uint64)
}

pub fn parse_hex_uint64_with_slice(value: &str, slice: TextSlice) -> NumbersResult<u64> {
    parse_hex_uint64(apply_slice(value, Some(slice))?)
}

/// Strict hexadecimal parsing for `u32`.
pub fn parse_hex_uint32(value: &str) -> NumbersResult<u32> {
    let hex = strip_hex_prefix(value);
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(NumbersError::InvalidHex(value.to_string()));
    }
    u32::from_str_radix(hex, 16).map_err(|e| map_int_err(e, value, true))
}

pub fn parse_hex_uint32_wide(value: &[u16]) -> NumbersResult<u32> {
    let utf8 = wide_to_utf8(value)?;
    parse_hex_uint32(&utf8)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn parse_hex_uint32_cstr(value: *const c_char) -> NumbersResult<u32> {
    with_cstr(value, parse_hex_uint32)
}

pub fn parse_hex_uint32_with_slice(value: &str, slice: TextSlice) -> NumbersResult<u32> {
    parse_hex_uint32(apply_slice(value, Some(slice))?)
}

/// Strict hexadecimal parsing for `u8`.
pub fn parse_hex_uint8(value: &str) -> NumbersResult<u8> {
    let hex = strip_hex_prefix(value);
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(NumbersError::InvalidHex(value.to_string()));
    }
    u8::from_str_radix(hex, 16).map_err(|e| map_int_err(e, value, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_uint64_simple() {
        assert_eq!(parse_uint64("12345").unwrap(), 12345);
        assert_eq!(parse_uint64("0").unwrap(), 0);
        assert_eq!(parse_uint64("000123").unwrap(), 123);
        assert!(parse_uint64("123abc").is_err());
        assert!(parse_uint64("").is_err());
    }

    #[test]
    fn test_parse_uint64_with_slice() {
        assert_eq!(
            parse_uint64_with_slice(
                "1234567890",
                TextSlice {
                    offset: 2,
                    count: 3
                }
            )
            .unwrap(),
            345
        );
        assert!(parse_uint64_with_slice(
            "123",
            TextSlice {
                offset: 2,
                count: 5
            }
        )
        .is_err());
        assert_eq!(
            parse_uint64_with_slice(
                "12345",
                TextSlice {
                    offset: 0,
                    count: 0
                }
            )
            .unwrap(),
            12345
        );
    }

    #[test]
    fn test_parse_uint32() {
        assert_eq!(parse_uint32("42").unwrap(), 42);
        assert_eq!(parse_uint32("0").unwrap(), 0);
        assert!(parse_uint32("99999999999").is_err());
    }

    #[test]
    fn test_parse_int64_and_int32() {
        assert_eq!(parse_int64("12345").unwrap(), 12345);
        assert_eq!(parse_int64("-12345").unwrap(), -12345);
        assert_eq!(parse_int64("0").unwrap(), 0);
        assert_eq!(parse_int32("42").unwrap(), 42);
        assert_eq!(parse_int32("-42").unwrap(), -42);
        assert_eq!(
            parse_int64_with_slice(
                "x-234y",
                TextSlice {
                    offset: 1,
                    count: 4
                }
            )
            .unwrap(),
            -234
        );
    }

    #[test]
    fn test_parse_hex() {
        assert_eq!(parse_hex_uint64("FF").unwrap(), 255);
        assert_eq!(parse_hex_uint64("0xFF").unwrap(), 255);
        assert_eq!(parse_hex_uint64("0Xff").unwrap(), 255);
        assert_eq!(parse_hex_uint64("FF00AA").unwrap(), 0xFF00AA);
        assert!(parse_hex_uint64("GG").is_err());
        assert_eq!(parse_hex_uint32("0x100").unwrap(), 256);
        assert_eq!(parse_hex_uint8("0xAB").unwrap(), 0xAB);
    }

    #[test]
    fn test_parse_wide_and_cstr() {
        let wide: Vec<u16> = "98765".encode_utf16().collect();
        assert_eq!(parse_uint64_wide(&wide).unwrap(), 98765);

        let c = std::ffi::CString::new("0x2A").unwrap();
        assert_eq!(parse_hex_uint32_cstr(c.as_ptr()).unwrap(), 42);
        assert!(parse_uint64_cstr(std::ptr::null()).is_err());
    }

    #[test]
    fn test_slice_edge_cases_and_bounds() {
        // Non-zero offset with count == 0 parses suffix from offset
        assert_eq!(
            parse_uint64_with_slice(
                "ab345",
                TextSlice {
                    offset: 2,
                    count: 0
                }
            )
            .unwrap(),
            345
        );
        // Out-of-bounds offset with count == 0 must error
        assert!(matches!(
            parse_uint64_with_slice(
                "123",
                TextSlice {
                    offset: 99,
                    count: 0
                }
            ),
            Err(NumbersError::SliceOutOfBounds)
        ));
        // Mid-UTF-8 char boundary must return SliceOutOfBounds, never panic
        assert!(matches!(
            parse_uint64_with_slice(
                "é123",
                TextSlice {
                    offset: 1,
                    count: 2
                }
            ),
            Err(NumbersError::SliceOutOfBounds)
        ));
        // Signed extremes & overflow classification
        assert_eq!(parse_int64("-9223372036854775808").unwrap(), i64::MIN);
        assert!(matches!(
            parse_int64("-9223372036854775809"),
            Err(NumbersError::OutOfRange(_))
        ));
        assert_eq!(parse_int32("-2147483648").unwrap(), i32::MIN);
        assert!(matches!(
            parse_hex_uint64("0x10000000000000000"),
            Err(NumbersError::OutOfRange(_))
        ));
        assert!(matches!(
            parse_hex_uint64("0x"),
            Err(NumbersError::InvalidHex(_))
        ));
    }
}
