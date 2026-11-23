//! Linux implementation of OSTPlatform::ByteSearch
//!
//! Provides byte pattern searching in memory buffers and files.
//! Uses the existing binary_reader pattern scanning implementation.

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
    if pattern.is_empty() {
        return None;
    }

    let mut file = File::open(path).ok()?;
    let file_size = file.metadata().ok()?.len();

    if file_size < pattern.len() as u64 {
        return None;
    }

    let chunk_size = chunk_bytes.max(pattern.len() * 2);
    let mut buffer = vec![0u8; chunk_size];
    let mut file_offset: u64 = 0;
    let mut overlap = vec![0u8; pattern.len() - 1];

    loop {
        let bytes_read = file.read(&mut buffer).ok()?;
        if bytes_read == 0 {
            break;
        }

        // Prepend overlap from previous chunk
        let mut search_buffer = Vec::with_capacity(overlap.len() + bytes_read);
        search_buffer.extend_from_slice(&overlap);
        search_buffer.extend_from_slice(&buffer[..bytes_read]);

        // Search in this chunk (with overlap)
        if let Some(pos) = search_buffer.windows(pattern.len()).position(|w| w == pattern) {
            let match_offset = file_offset.saturating_sub(overlap.len() as u64) + pos as u64;
            return Some(match_offset);
        }

        // Save overlap for next iteration
        if bytes_read >= pattern.len() {
            let start = bytes_read.saturating_sub(pattern.len() - 1);
            overlap.copy_from_slice(&buffer[start..bytes_read]);
        } else {
            // Not enough data for full overlap
            overlap[..bytes_read].copy_from_slice(&buffer[..bytes_read]);
            for i in bytes_read..overlap.len() {
                overlap[i] = 0;
            }
        }

        file_offset += bytes_read as u64;

        if file_offset >= file_size {
            break;
        }
    }

    None
}

/// Find a byte pattern in a file within a specific range.
/// Returns the file offset of the first match, or None if not found.
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

    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::Start(offset)).ok()?;

    let chunk_size = chunk_bytes.max(pattern.len() * 2).min(size as usize);
    let mut buffer = vec![0u8; chunk_size];
    let mut bytes_remaining = size;
    let mut file_offset = offset;
    let mut overlap = vec![0u8; pattern.len() - 1];

    while bytes_remaining > 0 {
        let to_read = chunk_size.min(bytes_remaining as usize);
        let bytes_read = file.read(&mut buffer[..to_read]).ok()?;
        if bytes_read == 0 {
            break;
        }

        // Prepend overlap from previous chunk
        let mut search_buffer = Vec::with_capacity(overlap.len() + bytes_read);
        search_buffer.extend_from_slice(&overlap);
        search_buffer.extend_from_slice(&buffer[..bytes_read]);

        // Search in this chunk (with overlap)
        if let Some(pos) = search_buffer.windows(pattern.len()).position(|w| w == pattern) {
            let match_offset = file_offset.saturating_sub(overlap.len() as u64) + pos as u64;
            return Some(match_offset);
        }

        // Save overlap for next iteration
        if bytes_read >= pattern.len() {
            let start = bytes_read.saturating_sub(pattern.len() - 1);
            overlap.copy_from_slice(&buffer[start..bytes_read]);
        } else {
            overlap[..bytes_read].copy_from_slice(&buffer[..bytes_read]);
            for i in bytes_read..overlap.len() {
                overlap[i] = 0;
            }
        }

        file_offset += bytes_read as u64;
        bytes_remaining -= bytes_read as u64;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
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
        // Pattern: 0x48 0x89 0x?? 0x?? (mov r8, r9 with wildcard)
        let data = &[0x48, 0x89, 0xC7, 0x48, 0x89, 0xF0];
        let pattern = &[0x48, 0x89, 0x00, 0x00];
        let mask = &[0xFF, 0xFF, 0x00, 0x00];
        assert_eq!(find_with_mask(data, pattern, mask), Some(0));

        // Second match
        assert_eq!(find_with_mask(&data[4..], pattern, mask), Some(0));
    }

    #[test]
    fn test_find_in_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.bin");
        let mut file = File::create(&file_path).unwrap();
        file.write_all(b"hello world test pattern here").unwrap();
        file.flush().unwrap();

        let result = find_in_file(&file_path, b"pattern", 4096);
        assert_eq!(result, Some(18));
    }

    #[test]
    fn test_find_in_file_range() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.bin");
        let mut file = File::create(&file_path).unwrap();
        file.write_all(b"0123456789abcdef").unwrap();
        file.flush().unwrap();

        // Search in range 5..10 (should find "56789")
        let result = find_in_file_range(&file_path, 5, 5, b"56789", 4096);
        assert_eq!(result, Some(5));

        // Search in range that doesn't contain pattern
        let result = find_in_file_range(&file_path, 0, 5, b"56789", 4096);
        assert_eq!(result, None);
    }
}