//! FileHash - Cryptographic hashing of individual files
//!
//! Provides SHA-256, SHA-1, and MD5 hashing for individual files
//! with caching and verification support.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

const HASH_BUF_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct FileHash {
    pub file_path: String,
    pub file_name: String,
    pub file_size: u64,
    pub sha256: String,
    pub sha1: String,
    pub md5: String,
    pub last_verified: Option<String>,
}

impl FileHash {
    pub fn try_new(file_path: &Path, file_name: &str) -> io::Result<Self> {
        let mut file = File::open(file_path)?;
        let mut sha256_hasher = Sha256::new();
        let mut sha1_hasher = sha1::Sha1::new();
        let mut md5_ctx = md5::Context::new();
        let mut buf = [0u8; HASH_BUF_SIZE];
        let mut total_bytes: u64 = 0;

        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let chunk = &buf[..n];
            sha256_hasher.update(chunk);
            sha1_hasher.update(chunk);
            md5_ctx.consume(chunk);
            total_bytes += n as u64;
        }

        let file_size = fs::metadata(file_path)
            .map(|m| m.len())
            .unwrap_or(total_bytes);

        Ok(Self {
            file_path: file_path.to_string_lossy().to_string(),
            file_name: file_name.to_string(),
            file_size,
            sha256: format!("{:x}", sha256_hasher.finalize()),
            sha1: format!("{:x}", sha1_hasher.finalize()),
            md5: format!("{:x}", md5_ctx.finalize()),
            last_verified: Some(chrono::Utc::now().to_rfc3339()),
        })
    }

    pub fn new(file_path: &Path, file_name: &str) -> Self {
        Self::try_new(file_path, file_name).unwrap_or_else(|_| Self {
            file_path: file_path.to_string_lossy().to_string(),
            file_name: file_name.to_string(),
            file_size: 0,
            sha256: String::new(),
            sha1: String::new(),
            md5: String::new(),
            last_verified: None,
        })
    }

    pub fn verify(&self, file_path: &Path) -> bool {
        if self.sha256.is_empty() {
            return false;
        }
        let Ok(mut file) = File::open(file_path) else {
            return false;
        };

        let mut hasher = Sha256::new();
        let mut buf = [0u8; HASH_BUF_SIZE];
        loop {
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => hasher.update(&buf[..n]),
                Err(_) => return false,
            }
        }
        let computed = format!("{:x}", hasher.finalize());
        computed == self.sha256
    }
}

#[derive(Debug, Clone, Default)]
pub struct FileHashService {
    cache: Arc<Mutex<HashMap<String, FileHash>>>,
}

impl FileHashService {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn hash_file(&self, file_path: &Path, file_name: &str) -> FileHash {
        let hash = FileHash::new(file_path, file_name);
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(file_path.to_string_lossy().to_string(), hash.clone());
        hash
    }

    pub fn get_hash(&self, file_path: &Path) -> Option<FileHash> {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&file_path.to_string_lossy().to_string())
            .cloned()
    }

    pub fn verify_file(&self, file_path: &Path) -> bool {
        let cached = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&file_path.to_string_lossy().to_string())
            .cloned();
        cached.map(|h| h.verify(file_path)).unwrap_or(false)
    }

    pub fn clear_cache(&mut self) {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// Parses a GitHub release asset `digest` string (e.g. `"sha256:abcd..."` or raw 64-char hex)
    /// into a normalized lowercase 64-char SHA-256 hex digest (LT-49).
    pub fn parse_github_sha256_digest(digest: &str) -> Option<String> {
        let trimmed = digest.trim();
        let hex_part = trimmed
            .strip_prefix("sha256:")
            .or_else(|| trimmed.strip_prefix("SHA256:"))
            .unwrap_or(trimmed)
            .trim();
        if hex_part.len() == 64 && hex_part.chars().all(|c| c.is_ascii_hexdigit()) {
            Some(hex_part.to_ascii_lowercase())
        } else {
            None
        }
    }

    /// Computes and caches the SHA-256 hash of `file_path` and verifies it against `expected_digest`
    /// (`"sha256:..."` or raw 64-char hex). Returns `Err` on unreadable file or digest mismatch (LT-49).
    pub fn verify_sha256_digest(
        &self,
        file_path: &Path,
        file_name: &str,
        expected_digest: &str,
    ) -> Result<FileHash, String> {
        let expected = Self::parse_github_sha256_digest(expected_digest).ok_or_else(|| {
            format!(
                "Invalid SHA-256 digest format for {}: '{}'",
                file_name, expected_digest
            )
        })?;
        let computed = self.hash_file(file_path, file_name);
        if computed.sha256.is_empty() {
            return Err(format!(
                "Failed to hash {} at {}",
                file_name,
                file_path.display()
            ));
        }
        if !computed.sha256.eq_ignore_ascii_case(&expected) {
            return Err(format!(
                "SHA-256 mismatch for {}: expected {}, got {}",
                file_name, expected, computed.sha256
            ));
        }
        Ok(computed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_hash_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        let mut f = std::fs::File::create(&file_path).unwrap();
        f.write_all(b"hello world").unwrap();

        let service = FileHashService::new();
        let hash = service.hash_file(&file_path, "test.txt");

        assert_eq!(hash.file_name, "test.txt");
        assert_eq!(hash.file_size, 11);
        assert_eq!(
            hash.sha256,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        assert_eq!(hash.sha1, "2aae6c35c94fcfb415dbe95f408b9ce91ee846ed");
        assert_eq!(hash.md5, "5eb63bbbe01eeed093cb22bb8f5acdc3");
        assert!(service.verify_file(&file_path));

        std::fs::write(&file_path, b"tampered").unwrap();
        assert!(!service.verify_file(&file_path));
    }

    #[test]
    fn test_missing_file_returns_unverified_and_fails_verify() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("nope.txt");
        let service = FileHashService::new();
        let hash = service.hash_file(&missing, "nope.txt");
        assert!(hash.sha256.is_empty());
        assert!(hash.last_verified.is_none());

        std::fs::write(&missing, b"").unwrap();
        assert!(!service.verify_file(&missing));
    }

    #[test]
    fn test_verify_github_sha256_digest_succeeds_and_rejects_mismatch() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("asset.tar.gz");
        std::fs::write(&file_path, b"hello world").unwrap();

        let service = FileHashService::new();
        let ok = service.verify_sha256_digest(
            &file_path,
            "asset.tar.gz",
            "sha256:b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9",
        );
        assert!(ok.is_ok());

        let bad = service.verify_sha256_digest(
            &file_path,
            "asset.tar.gz",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        );
        assert!(bad.is_err());
    }
}
