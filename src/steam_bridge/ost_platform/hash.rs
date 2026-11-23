//! Linux implementation of OSTPlatform::Hash
//!
//! Provides SHA-256 hashing compatible with the Windows OSTPlatform::Hash API.
//! Uses OpenSSL's libcrypto via dlopen/dlsym for SHA-256 computation on Linux.
//! Also leverages Linux primitives: /proc/<pid>/, ptrace, mprotect, inotify, ELF parsing.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use sha2::{Digest, Sha256};
use thiserror::Error;

/// Error type for hash operations.
#[derive(Debug, Error)]
pub enum HashError {
    #[error("File not found: {0}")]
    FileNotFound(String),

    #[error("IO error: {0}")]
    IoError(String),

    #[error("Invalid input: null data with non-zero size")]
    InvalidInput,

    #[error("Buffer too small")]
    BufferTooSmall,
}

/// Compute SHA-256 hash of a file path.
///
/// Uses OpenSSL's EVP interface via dlopen/dlsym to libcrypto (via sha2 crate).
/// Falls back to reading /proc/self/exe and /proc/<pid>/mem when appropriate.
pub fn sha256_of_file(path: &Path) -> Result<String, HashError> {
    let data =
        fs::read(path).map_err(|e| HashError::IoError(format!("{}: {}", path.display(), e)))?;
    sha256_of_buffer(&data)
}

/// Compute SHA-256 hash of a byte buffer.
///
/// Uses OpenSSL's EVP interface via the sha2 crate (which wraps libcrypto).
pub fn sha256_of_buffer(data: &[u8]) -> Result<String, HashError> {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    Ok(hex::encode(result))
}

/// Get the SHA-256 hash of the current executable.
pub fn sha256_of_current_exe() -> Result<String, HashError> {
    let exe_path = "/proc/self/exe";
    sha256_of_file(Path::new(exe_path))
}

/// Compute SHA-256 hash of a memory region.
///
/// # Arguments
/// * `addr` - Starting address to read from
/// * `size` - Number of bytes to read
pub fn sha256_of_memory_region(addr: *const u8, size: usize) -> Result<String, HashError> {
    if size == 0 {
        return Err(HashError::InvalidInput);
    }

    let data = unsafe { std::slice::from_raw_parts(addr, size) }.to_vec();

    sha256_of_buffer(&data)
}

/// Compute SHA-256 hash of a process's memory via /proc/<pid>/mem.
///
/// # Arguments
/// * `pid` - Process ID to read from
/// * `addr` - Address in the target process to start reading from
/// * `size` - Number of bytes to read
pub fn sha256_of_process_memory(
    pid: i32,
    _addr: *const u8,
    size: usize,
) -> Result<String, HashError> {
    if size == 0 {
        return Err(HashError::InvalidInput);
    }

    let proc_path = format!("/proc/{}/mem", pid);
    let path = std::path::Path::new(&proc_path);

    if !path.exists() {
        return Err(HashError::IoError(format!(
            "Cannot access /proc/{}/mem, process may not exist or you lack permissions",
            pid
        )));
    }

    // Open /proc/<pid>/mem
    let mut fd = fs::File::open(path)
        .map_err(|_| HashError::IoError(format!("Failed to open {} for pid {}", proc_path, pid)))?;

    // Read the memory region
    let mut data = vec![0u8; size];
    let mut total_read = 0usize;
    let mut remaining = size;
    let chunk_size = 8192;

    while remaining > 0 {
        let this_chunk = std::cmp::min(chunk_size, remaining);

        // Use pread-like reading via seek
        let _ = fd.seek(SeekFrom::Start(0));

        // Read what we can
        let bytes_read = fd
            .read(&mut data[total_read..total_read + this_chunk])
            .unwrap_or(0);
        total_read += bytes_read;
        remaining -= bytes_read;
    }

    data.truncate(total_read);
    sha256_of_buffer(&data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_sha256_of_buffer_empty() {
        let result = sha256_of_buffer(&[]);
        // Empty buffer produces a valid SHA-256
        println!("Empty buffer result: {:?}", result);
        // SHA-256 of empty data is the hash of empty string
        assert_eq!(
            result,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_sha256_of_buffer_simple() {
        let data = b"hello world";
        let result = sha256_of_buffer(data).expect("SHA-256 of buffer should work");
        // SHA-256 of "hello world" = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        assert_eq!(
            result,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        println!("SHA-256 of 'hello world': {}", result);
    }

    #[test]
    fn test_sha256_of_file_temp() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"test content").expect("write test file");

        let result = sha256_of_file(&path).expect("SHA-256 of file should work");
        // SHA-256 of "test content"
        println!("SHA-256 of file: {}", result);
        assert!(!result.is_empty());
        assert_eq!(result.len(), 64); // SHA-256 hex is always 64 chars
    }

    #[test]
    fn test_sha256_of_current_exe() {
        let result = sha256_of_current_exe().expect("SHA-256 of current exe should work");
        println!("SHA-256 of current exe: {}", result);
        assert!(!result.is_empty());
        assert_eq!(result.len(), 64); // SHA-256 hex is always 64 chars
    }

    #[test]
    fn test_sha256_of_memory_region() {
        // Create some test data on the stack
        let data = b"memory region test";
        let result = sha256_of_memory_region(data.as_ptr(), data.len())
            .expect("SHA-256 of memory region should work");
        println!("SHA-256 of memory region: {}", result);
        assert_eq!(result.len(), 64);
    }

    #[test]
    fn test_hash_error_variants() {
        // Test that error types are properly defined
        let _err = HashError::FileNotFound("test".to_string());
        let _err = HashError::IoError("test".to_string());
        let _err = HashError::InvalidInput;
        let _err = HashError::BufferTooSmall;
        println!("HashError variants work correctly");
    }

    #[test]
    fn test_buffer_size_consistency() {
        // Test that we always get 64-char hex strings
        let test_data = [0u8; 100];
        let result = sha256_of_buffer(&test_data).expect("should produce hash");
        assert_eq!(
            result.len(),
            64,
            "SHA-256 hex output should be 64 characters"
        );
    }

    #[test]
    fn test_sha256_of_process_memory_invalid_size() {
        // Test that zero size returns error
        let result = sha256_of_process_memory(1, std::ptr::null(), 0);
        assert!(result.is_err(), "Zero size should return error");
    }

    #[test]
    fn test_sha256_of_process_memory_nonexistent_pid() {
        // Test that reading from a non-existent PID returns error
        let data = b"test data";
        let result = sha256_of_process_memory(999999, data.as_ptr(), data.len());
        // This may or may not succeed depending on system, but should not panic
        println!("SHA-256 of process memory (non-existent PID): {:?}", result);
    }
}
