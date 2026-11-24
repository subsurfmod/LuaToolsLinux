//! Linux implementation of OSTPlatform::Hash
//!
//! Provides streaming SHA-256 hashing compatible with the Windows OSTPlatform::Hash API
//! using the pure-Rust `sha2` crate and `/proc/<pid>/mem` for remote process memory.

use std::fs;
use std::io::Read;
use std::os::unix::fs::FileExt;
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

/// Compute SHA-256 hash of a file path using bounded streaming I/O.
pub fn sha256_of_file(path: &Path) -> Result<String, HashError> {
    let mut file = fs::File::open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            HashError::FileNotFound(path.display().to_string())
        } else {
            HashError::IoError(format!("{}: {}", path.display(), e))
        }
    })?;

    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(HashError::IoError(format!("{}: {}", path.display(), e)));
            }
        }
    }

    Ok(hex::encode(hasher.finalize()))
}

/// Compute SHA-256 hash of a byte buffer.
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

/// Compute SHA-256 hash of a memory region in the current process.
///
/// Validates readability via `/proc/self/maps` before dereferencing.
///
/// # Arguments
/// * `addr` - Starting address to read from
/// * `size` - Number of bytes to read
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn sha256_of_memory_region(addr: *const u8, size: usize) -> Result<String, HashError> {
    if addr.is_null() || size == 0 || size > isize::MAX as usize {
        return Err(HashError::InvalidInput);
    }

    if !super::memory::is_readable(addr.cast(), size) {
        return Err(HashError::IoError(format!(
            "Memory region {:p}..+{size} is not readable",
            addr
        )));
    }

    let data = unsafe { std::slice::from_raw_parts(addr, size) };
    sha256_of_buffer(data)
}

/// Compute SHA-256 hash of a process's memory via `/proc/<pid>/mem` using streaming `pread`.
pub fn sha256_of_process_memory(
    pid: i32,
    addr: *const u8,
    size: usize,
) -> Result<String, HashError> {
    if pid <= 0 || addr.is_null() || size == 0 {
        return Err(HashError::InvalidInput);
    }

    let proc_path = format!("/proc/{}/mem", pid);
    let fd = fs::File::open(&proc_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            HashError::FileNotFound(proc_path.clone())
        } else {
            HashError::IoError(format!(
                "Failed to open {} for pid {}: {}",
                proc_path, pid, e
            ))
        }
    })?;

    let mut hasher = Sha256::new();
    let mut chunk = [0u8; 8192];
    let mut total_read = 0usize;
    let base_addr = addr as u64;

    while total_read < size {
        let to_read = (size - total_read).min(chunk.len());
        let read_addr = base_addr
            .checked_add(total_read as u64)
            .ok_or(HashError::InvalidInput)?;

        let bytes_read = loop {
            match fd.read_at(&mut chunk[..to_read], read_addr) {
                Ok(n) => break n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    return Err(HashError::IoError(format!(
                        "Failed reading {} at {:#x}: {}",
                        proc_path, read_addr, e
                    )));
                }
            }
        };

        if bytes_read == 0 {
            return Err(HashError::IoError(format!(
                "Unexpected EOF reading {} after {}/{} bytes",
                proc_path, total_read, size
            )));
        }

        hasher.update(&chunk[..bytes_read]);
        total_read += bytes_read;
    }

    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_of_buffer_empty() {
        let result = sha256_of_buffer(&[]).expect("SHA-256 of empty buffer should succeed");
        assert_eq!(
            result,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_sha256_of_buffer_simple() {
        let data = b"hello world";
        let result = sha256_of_buffer(data).expect("SHA-256 of buffer should work");
        assert_eq!(
            result,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
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
        let data = b"test data";
        let result = sha256_of_process_memory(999_999_999, data.as_ptr(), data.len());
        assert!(result.is_err());
    }

    #[test]
    fn test_sha256_of_file_not_found() {
        let err = sha256_of_file(Path::new("/nonexistent/ost_hash_test_file.bin")).unwrap_err();
        assert!(matches!(err, HashError::FileNotFound(_)));
    }

    #[test]
    fn test_sha256_of_process_memory_self_and_unreadable_error() {
        let data = b"process memory hash test payload";
        let expected = sha256_of_buffer(data).unwrap();
        let pid = std::process::id() as i32;
        let actual = sha256_of_process_memory(pid, data.as_ptr(), data.len())
            .expect("should hash own process memory");
        assert_eq!(actual, expected);

        // Non-positive PID must return InvalidInput
        assert!(matches!(
            sha256_of_process_memory(0, data.as_ptr(), data.len()),
            Err(HashError::InvalidInput)
        ));
        assert!(matches!(
            sha256_of_process_memory(-1, data.as_ptr(), data.len()),
            Err(HashError::InvalidInput)
        ));

        // Unreadable address must error instead of returning hash of empty buffer
        assert!(sha256_of_process_memory(pid, 0x1000 as *const u8, 16).is_err());
        assert!(sha256_of_memory_region(0x1000 as *const u8, 16).is_err());
    }
}
