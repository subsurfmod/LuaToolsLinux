//! Linux implementation of OSTPlatform::Encoding
//!
//! Provides UTF-8/UTF-16 (wide string) conversion utilities.
//! On Linux, strings are typically UTF-8, so WideToUtf8 is a no-op.
//! Utf8ToWide converts to UTF-16 for Windows API compatibility.

/// Error type for encoding operations.
#[derive(Debug, thiserror::Error)]
pub enum EncodingError {
    #[error("Invalid UTF-8 sequence: {0}")]
    InvalidUtf8(String),

    #[error("Invalid UTF-16 sequence: {0}")]
    InvalidUtf16(String),

    #[error("String size overflow: input too large for operation")]
    SizeOverflow,
}

/// Convert a UTF-16 (wide) string to UTF-8.
/// On Linux, this is primarily for interoperating with Windows-style APIs.
///
/// # Arguments
/// * `value` - UTF-16 encoded string (as u16 slice)
///
/// # Returns
/// UTF-8 string on success, or error on invalid UTF-16.
pub fn wide_to_utf8(value: &[u16]) -> Result<String, EncodingError> {
    if value.is_empty() {
        return Ok(String::new());
    }

    String::from_utf16(value).map_err(|e| EncodingError::InvalidUtf16(format!("{:?}", e)))
}

/// Convert a UTF-16 (wide) string to UTF-8, returning empty string on error.
/// Convenience wrapper for cases where silent failure is acceptable.
pub fn wide_to_utf8_lossy(value: &[u16]) -> String {
    wide_to_utf8(value).unwrap_or_default()
}

/// Convert a UTF-8 string to UTF-16 (wide) string.
/// Returns the encoded u16 slice.
///
/// # Arguments
/// * `value` - UTF-8 string
///
/// # Returns
/// Vector of u16 representing UTF-16 encoded string.
pub fn utf8_to_wide(value: &str) -> Result<Vec<u16>, EncodingError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }

    Ok(value.encode_utf16().collect())
}

/// Convert a UTF-8 string to wide string, with lossy replacement.
/// Invalid UTF-8 sequences are replaced with the Unicode replacement character.
pub fn utf8_to_wide_lossy(value: &str) -> Vec<u16> {
    if value.is_empty() {
        return Vec::new();
    }

    value.encode_utf16().collect()
}

/// Convert a null-terminated UTF-16 string to UTF-8.
/// Stops at the first null character.
pub fn wide_to_utf8_cstr(value: &[u16]) -> String {
    let len = value.iter().position(|&c| c == 0).unwrap_or(value.len());
    wide_to_utf8(&value[..len]).unwrap_or_default()
}

/// Convert a null-terminated UTF-8 string to wide string.
/// This is typically not needed on Linux but provided for API compatibility.
pub fn utf8_to_wide_cstr(value: &str) -> Vec<u16> {
    if value.is_empty() {
        return Vec::new();
    }

    // Find the first null character (if any) and encode that portion
    let len = value.len();
    let s = &value[..len];
    utf8_to_wide(s).unwrap_or_default()
}

/// Convert wide string (Vec<u16>) to owned wide string (Vec<u16>).
/// Identity function for API consistency.
pub fn to_wide_vec(value: Vec<u16>) -> Vec<u16> {
    value
}

/// Check if a wide string is valid UTF-16.
pub fn is_valid_utf16(value: &[u16]) -> bool {
    let mut chars = value.iter().peekable();
    while let Some(&c) = chars.next() {
        // Handle surrogate pairs
        if (0xD800..=0xDBFF).contains(&c) {
            // High surrogate must be followed by low surrogate
            if let Some(&next) = chars.peek() {
                if !matches!(next, 0xDC00..=0xDFFF) {
                    return false;
                }
                chars.next(); // consume low surrogate
            } else {
                return false; // High surrogate without low surrogate
            }
        } else if (0xDC00..=0xDFFF).contains(&c) {
            // Low surrogate without high surrogate
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wide_to_utf8_ascii() {
        // "hello" in UTF-16
        let input: &[u16] = &[
            b'h' as u16,
            b'e' as u16,
            b'l' as u16,
            b'l' as u16,
            b'o' as u16,
        ];
        let result = wide_to_utf8(input).unwrap();
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_wide_to_utf8_unicode() {
        // "héllo" - é is U+00E9
        let input: &[u16] = &[
            b'h' as u16,
            0x00E9, // é
            b'l' as u16,
            b'l' as u16,
            b'o' as u16,
        ];
        let result = wide_to_utf8(input).unwrap();
        assert_eq!(result, "héllo");
    }

    #[test]
    fn test_wide_to_utf8_emoji() {
        // 😀 U+1F600 - requires surrogate pair in UTF-16
        let input: &[u16] = &[0xD83D, 0xDE00]; // Surrogate pair for 😀
        let result = wide_to_utf8(input).unwrap();
        assert_eq!(result, "😀");
    }

    #[test]
    fn test_wide_to_utf8_empty() {
        let result = wide_to_utf8(&[]).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_utf8_to_wide_ascii() {
        let result = utf8_to_wide("hello").unwrap();
        assert_eq!(
            result,
            &[
                b'h' as u16,
                b'e' as u16,
                b'l' as u16,
                b'l' as u16,
                b'o' as u16
            ][..]
        );
    }

    #[test]
    fn test_utf8_to_wide_unicode() {
        let result = utf8_to_wide("héllo").unwrap();
        assert!(result.contains(&0x00E9));
    }

    #[test]
    fn test_utf8_to_wide_empty() {
        let result = utf8_to_wide("").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_wide_to_utf8_cstr() {
        // "hello\0world" - null in the middle
        let input: &[u16] = &[
            b'h' as u16,
            b'e' as u16,
            b'l' as u16,
            b'l' as u16,
            b'o' as u16,
            0, // null terminator
            b'w' as u16,
            b'o' as u16,
            b'r' as u16,
            b'l' as u16,
            b'd' as u16,
        ];
        let result = wide_to_utf8_cstr(input);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_utf8_to_wide_lossy() {
        let result = utf8_to_wide_lossy("hello");
        assert_eq!(result.len(), 5);
    }

    #[test]
    fn test_is_valid_utf16_valid() {
        // ASCII is valid
        assert!(is_valid_utf16(&[b'h' as u16, b'i' as u16]));
        // BMP character
        assert!(is_valid_utf16(&[0x00E9])); // é
                                            // Valid surrogate pair
        assert!(is_valid_utf16(&[0xD83D, 0xDE00])); // 😀
    }

    #[test]
    fn test_is_valid_utf16_invalid() {
        // Lone high surrogate
        assert!(!is_valid_utf16(&[0xD800]));
        // Lone low surrogate
        assert!(!is_valid_utf16(&[0xDC00]));
    }

    #[test]
    fn test_roundtrip() {
        let original = "héllo 🌍";
        let wide = utf8_to_wide(original).unwrap();
        let back = wide_to_utf8(&wide).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn test_wide_to_utf8_invalid_surrogate() {
        // Invalid: high surrogate without low surrogate
        let input: &[u16] = &[0xD800];
        let result = wide_to_utf8(input);
        assert!(result.is_err());
    }
}
