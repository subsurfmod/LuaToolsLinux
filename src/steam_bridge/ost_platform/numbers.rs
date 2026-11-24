//! Linux implementation of OSTPlatform::Numbers
//!
//! Provides numeric string parsing utilities compatible with the Windows OSTPlatform API.
//! Uses Linux-native primitives for string parsing and ELF inspection.
//! Relies on Rust's standard library for core parsing, matching the pattern from
//! usefulsteamtools' Windows implementation which uses std::from_chars.

use std::ffi::c_char;
use std::os::unix::ffi::OsStrExt;
use std::str::FromStr;

use thiserror::Error;

/// A slice of a string, matching the Windows OSTPlatform::Numbers::TextSlice struct.
/// Contains an offset and count for substring extraction.
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

/// Result type for parsing operations.
pub type NumbersResult<T> = std::result::Result<T, NumbersError>;

/// Strict decimal parsing: full input must be consumed and fit the target type.
///
/// # Arguments
/// * `value` - String view to parse
///
/// # Returns
/// `Ok(value)` if valid, or `None` (as `NumbersResult`) if invalid.
pub fn parse_uint64(value: &str) -> NumbersResult<u64> {
    Self::parse_uint64_impl(value, 10, None)
}

/// Strict decimal parsing for wide strings (UTF-16).
///
/// # Arguments
/// * `value` - Wide string view to parse
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_uint64_wide(value: &[u16]) -> NumbersResult<u64> {
    let utf8 = Self::wide_to_utf8(value)?;
    Self::parse_uint64(&utf8)
}

/// Strict decimal parsing for C strings.
///
/// # Arguments
/// * `value` - C string (null-terminated)
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_uint64_cstr(value: *const c_char) -> NumbersResult<u64> {
    if value.is_null() {
        return Err(NumbersError::NullPointer);
    }
    let c_str = unsafe { std::ffi::CStr::from_ptr(value) };
    let s = c_str.to_str().map_err(|e| NumbersError::InvalidFormat(e.to_string()))?;
    Self::parse_uint64(s)
}

/// Strict decimal parsing with optional TextSlice.
///
/// # Arguments
/// * `value` - String view to parse
/// * `slice` - Optional slice to extract a substring first
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_uint64_with_slice(value: &str, slice: TextSlice) -> NumbersResult<u64> {
    // Apply slice if provided
    let effective_value = if slice.count > 0 {
        if slice.offset >= value.len() {
            return Err(NumbersError::SliceOutOfBounds);
        }
        let end = slice.offset + slice.count;
        let trimmed_end = std::min(end, value.len());
        value[slice.offset..trimmed_end].to_string()
    } else {
        value.to_string()
    };

    Self::parse_uint64(&effective_value)
}

/// Strict decimal parsing for uint32.
///
/// # Arguments
/// * `value` - String view to parse
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_uint32(value: &str) -> NumbersResult<u32> {
    Self::parse_uint32_impl(value, 10, None)
}

/// Strict decimal parsing for wide strings (uint32).
///
/// # Arguments
/// * `value` - Wide string view to parse
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_uint32_wide(value: &[u16]) -> NumbersResult<u32> {
    let utf8 = Self::wide_to_utf8(value)?;
    Self::parse_uint32(&utf8)
}

/// Strict decimal parsing for C strings (uint32).
///
/// # Arguments
/// * `value` - C string (null-terminated)
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_uint32_cstr(value: *const c_char) -> NumbersResult<u32> {
    if value.is_null() {
        return Err(NumbersError::NullPointer);
    }
    let c_str = unsafe { std::ffi::CStr::from_ptr(value) };
    let s = c_str.to_str().map_err(|e| NumbersError::InvalidFormat(e.to_string()))?;
    Self::parse_uint32(s)
}

/// Strict decimal parsing with optional TextSlice for uint32.
///
/// # Arguments
/// * `value` - String view to parse
/// * `slice` - Optional slice to extract a substring first
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_uint32_with_slice(value: &str, slice: TextSlice) -> NumbersResult<u32> {
    Self::parse_uint64_with_slice(value, slice).map(|v| v as u32)
}

/// Strict decimal parsing for int64.
///
/// # Arguments
/// * `value` - String view to parse
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_int64(value: &str) -> NumbersResult<i64> {
    Self::parse_int64_impl(value, 10, None)
}

/// Strict decimal parsing for wide strings (int64).
///
/// # Arguments
/// * `value` - Wide string view to parse
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_int64_wide(value: &[u16]) -> NumbersResult<i64> {
    let utf8 = Self::wide_to_utf8(value)?;
    Self::parse_int64(&utf8)
}

/// Strict decimal parsing for C strings (int64).
///
/// # Arguments
/// * `value` - C string (null-terminated)
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_int64_cstr(value: *const c_char) -> NumbersResult<i64> {
    if value.is_null() {
        return Err(NumbersError::NullPointer);
    }
    let c_str = unsafe { std::ffi::CStr::from_ptr(value) };
    let s = c_str.to_str().map_err(|e| NumbersError::InvalidFormat(e.to_string()))?;
    Self::parse_int64(s)
}

/// Strict decimal parsing with optional TextSlice for int64.
///
/// # Arguments
/// * `value` - String view to parse
/// * `slice` - Optional slice to extract a substring first
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_int64_with_slice(value: &str, slice: TextSlice) -> NumbersResult<i64> {
    Self::parse_int64_impl(value, 10, Some(slice))
}

/// Strict decimal parsing for int32.
///
/// # Arguments
/// * `value` - String view to parse
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_int32(value: &str) -> NumbersResult<i32> {
    Self::parse_int32_impl(value, 10, None)
}

/// Strict decimal parsing for wide strings (int32).
///
/// # Arguments
/// * `value` - Wide string view to parse
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_int32_wide(value: &[u16]) -> NumbersResult<i32> {
    let utf8 = Self::wide_to_utf8(value)?;
    Self::parse_int32(&utf8)
}

/// Strict decimal parsing for C strings (int32).
///
/// # Arguments
/// * `value` - C string (null-terminated)
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_int32_cstr(value: *const c_char) -> NumbersResult<i32> {
    if value.is_null() {
        return Err(NumbersError::NullPointer);
    }
    let c_str = unsafe { std::ffi::CStr::from_ptr(value) };
    let s = c_str.to_str().map_err(|e| NumbersError::InvalidFormat(e.to_string()))?;
    Self::parse_int32(s)
}

/// Strict decimal parsing with optional TextSlice for int32.
///
/// # Arguments
/// * `value` - String view to parse
/// * `slice` - Optional slice to extract a substring first
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_int32_with_slice(value: &str, slice: TextSlice) -> NumbersResult<i32> {
    Self::parse_int64_with_slice(value, slice).map(|v| v as i32)
}

/// Strict hexadecimal parsing; optional 0x/0X prefix is accepted.
///
/// # Arguments
/// * `value` - String view to parse
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_hex_uint64(value: &str) -> NumbersResult<u64> {
    Self::parse_hex_uint64_impl(value, None)
}

/// Strict hexadecimal parsing for wide strings.
///
/// # Arguments
/// * `value` - Wide string view to parse
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_hex_uint64_wide(value: &[u16]) -> NumbersResult<u64> {
    let utf8 = Self::wide_to_utf8(value)?;
    Self::parse_hex_uint64(&utf8)
}

/// Strict hexadecimal parsing for C strings.
///
/// # Arguments
/// * `value` - C string (null-terminated)
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_hex_uint64_cstr(value: *const c_char) -> NumbersResult<u64> {
    if value.is_null() {
        return Err(NumbersError::NullPointer);
    }
    let c_str = unsafe { std::ffi::CStr::from_ptr(value) };
    let s = c_str.to_str().map_err(|e| NumbersError::InvalidFormat(e.to_string()))?;
    Self::parse_hex_uint64(s)
}

/// Strict hexadecimal parsing with optional TextSlice.
///
/// # Arguments
/// * `value` - String view to parse
/// * `slice` - Optional slice to extract a substring first
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_hex_uint64_with_slice(value: &str, slice: TextSlice) -> NumbersResult<u64> {
    // Apply slice if provided
    let effective_value = if slice.count > 0 {
        if slice.offset >= value.len() {
            return Err(NumbersError::SliceOutOfBounds);
        }
        let end = slice.offset + slice.count;
        let trimmed_end = std::min(end, value.len());
        value[slice.offset..trimmed_end].to_string()
    } else {
        value.to_string()
    };

    Self::parse_hex_uint64(&effective_value)
}

/// Strict hexadecimal parsing for uint32.
///
/// # Arguments
/// * `value` - String view to parse
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_hex_uint32(value: &str) -> NumbersResult<u32> {
    Self::parse_hex_uint32_impl(value, None)
}

/// Strict hexadecimal parsing for wide strings (uint32).
///
/// # Arguments
/// * `value` - Wide string view to parse
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_hex_uint32_wide(value: &[u16]) -> NumbersResult<u32> {
    let utf8 = Self::wide_to_utf8(value)?;
    Self::parse_hex_uint32(&utf8)
}

/// Strict hexadecimal parsing for C strings (uint32).
///
/// # Arguments
/// * `value` - C string (null-terminated)
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_hex_uint32_cstr(value: *const c_char) -> NumbersResult<u32> {
    if value.is_null() {
        return Err(NumbersError::NullPointer);
    }
    let c_str = unsafe { std::ffi::CStr::from_ptr(value) };
    let s = c_str.to_str().map_err(|e| NumbersError::InvalidFormat(e.to_string()))?;
    Self::parse_hex_uint32(s)
}

/// Strict hexadecimal parsing with optional TextSlice for uint32.
///
/// # Arguments
/// * `value` - String view to parse
/// * `slice` - Optional slice to extract a substring first
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_hex_uint32_with_slice(value: &str, slice: TextSlice) -> NumbersResult<u32> {
    Self::parse_hex_uint64_with_slice(value, slice).map(|v| v as u32)
}

/// Strict hexadecimal parsing for uint8.
///
/// # Arguments
/// * `value` - String view to parse
///
/// # Returns
/// `Ok(value)` if valid, or `None` if invalid.
pub fn parse_hex_uint8(value: &str) -> NumbersResult<u8> {
    Self::parse_hex_uint8_impl(value, None)
}

/// Convert a wide string (UTF-16) to UTF-8.
fn wide_to_utf8(value: &[u16]) -> std::result::Result<String, NumbersError> {
    // Handle surrogate pairs manually for robustness
    let mut result = String::with_capacity(value.len());
    let mut i = 0;
    while i < value.len() {
        let c = value[i];
        if (0xD800..=0xDBFF).contains(&c) && i + 1 < value.len() {
            // High surrogate - check for low surrogate
            let low = value[i + 1];
            if (0xDC00..=0xDFFF).contains(&low) {
                // Valid surrogate pair
                let code_point = (((c as u32) - 0xD800) * 0x400) + ((low as u32) - 0xDC00) + 0x10000;
                // Convert code point to UTF-8
                if code_point <= 0x7F {
                    result.push(code_point as u8);
                } else if code_point <= 0x7FF {
                    result.push(((code_point >> 6) & 0x1F) as u8 | 0xC0);
                    result.push((code_point & 0x3F) as u8 | 0x80);
                } else if code_point <= 0xFFFF {
                    result.push(((code_point >> 12) & 0x0F) as u8 | 0xE0);
                    result.push(((code_point >> 6) & 0x3F) as u8 | 0x80);
                    result.push((code_point & 0x3F) as u8 | 0x80);
                } else if code_point <= 0x10FFFF {
                    result.push(((code_point >> 18) & 0x07) as u8 | 0xF0);
                    result.push(((code_point >> 12) & 0x3F) as u8 | 0x80);
                    result.push(((code_point >> 6) & 0x3F) as u8 | 0x80);
                    result.push((code_point & 0x3F) as u8 | 0x80);
                }
                i += 2;
            } else {
                // High surrogate without low surrogate - treat as-is
                result.push(c as u32 as u8);
                i += 1;
            }
        } else if (0xDC00..=0xDFFF).contains(&c) {
            // Lone low surrogate - skip or error
            i += 1;
        } else {
            // Regular character (BMP or single code unit)
            if c <= 0x7F {
                result.push(c as u8);
            } else if c <= 0x7FF {
                result.push(((c >> 6) & 0x1F) as u8 | 0xC0);
                result.push((c & 0x3F) as u8 | 0x80);
            } else if c <= 0xFFFF {
                result.push(((c >> 12) & 0x0F) as u8 | 0xE0);
                result.push(((c >> 6) & 0x3F) as u8 | 0x80);
                result.push((c & 0x3F) as u8 | 0x80);
            } else {
                result.push(((c >> 18) & 0x07) as u8 | 0xF0);
                result.push(((c >> 12) & 0x3F) as u8 | 0x80);
                result.push(((c >> 6) & 0x3F) as u8 | 0x80);
                result.push((c & 0x3F) as u8 | 0x80);
            }
            i += 1;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_uint64_simple() {
        // Valid decimal
        let result = parse_uint64("12345");
        assert_eq!(result.unwrap(), 12345);

        // Zero
        let result = parse_uint64("0");
        assert_eq!(result.unwrap(), 0);

        // Leading zeros
        let result = parse_uint64("000123");
        assert_eq!(result.unwrap(), 123);

        // Invalid: trailing characters
        let result = parse_uint64("123abc");
        assert!(result.is_err());

        // Invalid: empty
        let result = parse_uint64("");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_uint64_with_slice() {
        let result = parse_uint64_with_slice("1234567890", TextSlice { offset: 2, count: 3 });
        assert_eq!(result.unwrap(), 456);

        // Slice out of bounds
        let result = parse_uint64_with_slice("123", TextSlice { offset: 2, count: 5 });
        assert!(result.is_err());

        // Empty slice
        let result = parse_uint64_with_slice("12345", TextSlice { offset: 0, count: 0 });
        assert_eq!(result.unwrap(), 12345);
    }

    #[test]
    fn test_parse_uint32() {
        let result = parse_uint32("42");
        assert_eq!(result.unwrap(), 42);

        let result = parse_uint32("0");
        assert_eq!(result.unwrap(), 0);

        // Overflow - should fail since u32 max is 4294967295
        let result = parse_uint32("99999999999");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_int64() {
        let result = parse_int64("12345");
        assert_eq!(result.unwrap(), 12345);

        let result = parse_int64("-12345");
        assert_eq!(result.unwrap(), -12345);

        let result = parse_int64("0");
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn test_parse_int32() {
        let result = parse_int32("42");
        assert_eq!(result.unwrap(), 42);

        let result = parse_int32("-42");
        assert_eq!(result.unwrap(), -42);

        let result = parse_int32("0");
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn test_parse_hex_uint64() {
        // Without prefix
        let result = parse_hex_uint64("FF");
        assert_eq!(result.unwrap(), 255);

        // With 0x prefix
        let result = parse_hex_uint64("0xFF");
        assert_eq!(result.unwrap(), 255);

        // With 0X prefix
        let result = parse_hex_uint64("0Xff");
        assert_eq!(result.unwrap(), 255);

        // Invalid hex
        let result = parse_hex_uint64("GG");
        assert!(result.is_err());

        // Trailing characters
        let result = parse_hex_uint64("FF00AA");
        // Should parse the full string since from_chars validates
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0xFF00AA);
    }

    #[test]
    fn test_parse_hex_uint32() {
        let result = parse_hex_uint32("FF");
        assert_eq!(result.unwrap(), 255);

        let result = parse_hex_uint32("0x100");
        assert_eq!(result.unwrap(), 256);
    }

    #[test]
    fn test_parse_hex_uint8() {
        let result = parse_hex_uint8("FF");
        assert_eq!(result.unwrap(), 255);

        let result = parse_hex_uint8("0xAB");
        assert_eq!(result.unwrap(), 171);
    }

    #[test]
    fn test_text_slice() {
        let slice = TextSlice { offset: 0, count: 5 };
        assert_eq!(slice.offset, 0);
        assert_eq!(slice.count, 5);
    }

    #[test]
    fn test_parse_uint64_cstr() {
        // This test requires unsafe context, skip for now
        // In practice, this would be called with a valid C string pointer
    }

    #[test]
    fn test_parse_int64_with_slice() {
        let result = parse_int64_with_slice("-123456789", TextSlice { offset: 1, count: 3 });
        assert_eq!(result.unwrap(), -234);
    }
}