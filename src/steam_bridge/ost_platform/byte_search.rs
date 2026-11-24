//! Linux implementation of OSTPlatform::ByteSearch
//!
//! Provides byte pattern searching in memory buffers and files.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Find a byte pattern in a memory buffer.
/// Returns the offset of the first match, or None if not found.
pub fn find(bytes: &[u8], pattern: &[u8]) -> Option<usize> {
    if pattern.is_empty() || bytes.len() < pattern.len() {
        return None;
    }

    bytes
        .windows(pattern.len())
        .position(|window| window == pattern)
}

/// Find a byte pattern with a mask in a memory buffer.
/// Mask bytes: 0xFF = exact match, 0x00 = wildcard (don't care).
pub fn find_with_mask(bytes: &[u8], pattern: &[u8], mask: &[u8]) -> Option<usize> {
    if pattern.is_empty() || bytes.len() < pattern.len() || mask.len() < pattern.len() {
        return None;
    }

    for i in 0..=bytes.len() - pattern.len() {
        let mut matched = true;
        for j in 0..pattern.len() {
            if (bytes[i + j] & mask[j]) != (pattern[j] & mask[j]) {
                matched = false;
                break;
            }
        }
        if matched {
            return Some(i);
        }
    }
    None
}

/// Find a byte pattern in a file.
/// Reads the file in chunks to handle large files.
/// Returns the file offset of the first match, or None if not found.
pub fn find_in_file(path: &Path, pattern: &[u8], chunk_bytes: usize) -> Option<u64> {
    let file = File::open(path).ok()?;
    let file_size = file.metadata().ok()?.len();
    find_in_open_file(file, 0, file_size, pattern, chunk_bytes)
}

/// Find a byte pattern in a file within a specific range `[offset, offset + size)`.
/// Returns the absolute file offset of the first match, or None if not found.
pub fn find_in_file_range(
    path: &Path,
    offset: u64,
    size: u64,
    pattern: &[u8],
    chunk_bytes: usize,
) -> Option<u64> {
    if pattern.is_empty() || size < pattern.len() as u64 {
        return None;
    }

    let file = File::open(path).ok()?;
    find_in_open_file(file, offset, size, pattern, chunk_bytes)
}

fn find_in_open_file(
    mut file: File,
    offset: u64,
    size: u64,
    pattern: &[u8],
    chunk_bytes: usize,
) -> Option<u64> {
    const DEFAULT_CHUNK_BYTES: usize = 64 * 1024;
    const MAX_CHUNK_BYTES: usize = 16 * 1024 * 1024;

    if pattern.is_empty() || size < pattern.len() as u64 {
        return None;
    }

    file.seek(SeekFrom::Start(offset)).ok()?;

    let requested_chunk = if chunk_bytes == 0 {
        DEFAULT_CHUNK_BYTES
    } else {
        chunk_bytes
    };
    let size_usize = usize::try_from(size).unwrap_or(usize::MAX);
    let chunk_size = requested_chunk
        .max(pattern.len().saturating_mul(2))
        .min(MAX_CHUNK_BYTES)
        .min(size_usize);
    let max_overlap = pattern.len().saturating_sub(1);

    // Single reusable buffer: [0..overlap_len] holds tail from previous chunk,
    // [overlap_len..overlap_len + bytes_read] receives newly read bytes.
    let buf_len = max_overlap.checked_add(chunk_size)?;
    let mut buffer = vec![0u8; buf_len];
    let mut overlap_len = 0usize;
    let mut bytes_remaining = size;
    let mut file_offset = offset;

    while bytes_remaining > 0 {
        let to_read = chunk_size.min(usize::try_from(bytes_remaining).unwrap_or(usize::MAX));
        let bytes_read = loop {
            match file.read(&mut buffer[overlap_len..overlap_len + to_read]) {
                Ok(n) => break n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return None,
            }
        };
        if bytes_read == 0 {
            break;
        }

        let valid_len = overlap_len + bytes_read;
        if let Some(pos) = find(&buffer[..valid_len], pattern) {
            let match_offset = file_offset
                .checked_sub(overlap_len as u64)?
                .checked_add(pos as u64)?;
            return Some(match_offset);
        }

        if max_overlap > 0 {
            let keep = valid_len.min(max_overlap);
            buffer.copy_within(valid_len - keep..valid_len, 0);
            overlap_len = keep;
        }

        file_offset = file_offset.checked_add(bytes_read as u64)?;
        bytes_remaining -= bytes_read as u64;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_find_simple() {
        let data = b"hello world hello";
        let pattern = b"world";
        assert_eq!(find(data, pattern), Some(6));
    }

    #[test]
    fn test_find_not_found() {
        let data = b"hello world";
        let pattern = b"xyz";
        assert_eq!(find(data, pattern), None);
    }

    #[test]
    fn test_find_empty_pattern() {
        let data = b"hello";
        let pattern = b"";
        assert_eq!(find(data, pattern), None);
    }

    #[test]
    fn test_find_with_mask() {
        // Pattern: 0x48 0x89 0x?? 0x??
        let data = &[0x48, 0x89, 0xC7, 0x90, 0x48, 0x89, 0xF0, 0xC3];
        let pattern = &[0x48, 0x89, 0x00, 0x00];
        let mask = &[0xFF, 0xFF, 0x00, 0x00];
        assert_eq!(find_with_mask(data, pattern, mask), Some(0));
        assert_eq!(find_with_mask(&data[4..], pattern, mask), Some(0));
    }

    #[test]
    fn test_find_in_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.bin");
        let mut file = File::create(&file_path).unwrap();
        file.write_all(b"hello world test pattern here").unwrap();
        file.flush().unwrap();

        // "pattern" starts at byte 17 in "hello world test pattern here"
        assert_eq!(find_in_file(&file_path, b"pattern", 4096), Some(17));
        // Also verify small chunk boundary crossing
        assert_eq!(find_in_file(&file_path, b"pattern", 8), Some(17));
    }

    #[test]
    fn test_find_in_file_range() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.bin");
        let mut file = File::create(&file_path).unwrap();
        file.write_all(b"0123456789abcdef").unwrap();
        file.flush().unwrap();

        assert_eq!(
            find_in_file_range(&file_path, 5, 5, b"56789", 4096),
            Some(5)
        );
        assert_eq!(find_in_file_range(&file_path, 0, 5, b"56789", 4096), None);
    }

    #[test]
    fn test_find_in_file_zero_chunk_and_boundary_cases() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("boundary.bin");
        let mut file = File::create(&file_path).unwrap();
        file.write_all(b"aaaaaabbbbbTARGETccccc").unwrap();
        file.flush().unwrap();

        // chunk_bytes = 0 should fall back to default chunk size cleanly
        assert_eq!(find_in_file(&file_path, b"TARGET", 0), Some(11));
        // Single-byte pattern with tiny chunk
        assert_eq!(find_in_file(&file_path, b"T", 1), Some(11));
        // Pattern spanning exact chunk boundary
        assert_eq!(find_in_file(&file_path, b"TARGET", 6), Some(11));
        // Short mask should return None safely
        assert_eq!(find_with_mask(b"abcd", b"ab", &[0xFF]), None);
    }
}
