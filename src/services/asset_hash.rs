//! AssetHash - Cryptographic hashing of game assets for integrity verification
//!
//! Provides SHA-256, SHA-1, and MD5 hashing of game files to verify integrity
//! and detect tampering or corruption.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

const HASH_BUF_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct AssetHash {
    pub app_id: u32,
    pub file_path: String,
    pub file_name: String,
    pub file_size: u64,
    pub sha256: String,
    pub sha1: String,
    pub md5: String,
    pub last_verified: Option<String>,
}

impl AssetHash {
    pub fn try_new(app_id: u32, file_path: &Path, file_name: &str) -> io::Result<Self> {
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
            app_id,
            file_path: file_path.to_string_lossy().to_string(),
            file_name: file_name.to_string(),
            file_size,
            sha256: format!("{:x}", sha256_hasher.finalize()),
            sha1: format!("{:x}", sha1_hasher.finalize()),
            md5: format!("{:x}", md5_ctx.finalize()),
            last_verified: Some(chrono::Utc::now().to_rfc3339()),
        })
    }

    pub fn new(app_id: u32, file_path: &Path, file_name: &str) -> Self {
        Self::try_new(app_id, file_path, file_name).unwrap_or_else(|_| Self {
            app_id,
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
pub struct AssetHashService {
    cache: Arc<Mutex<HashMap<u32, Vec<AssetHash>>>>,
}

impl AssetHashService {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn hash_file(&self, app_id: u32, file_path: &Path, file_name: &str) -> AssetHash {
        let hash = AssetHash::new(app_id, file_path, file_name);
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let list = cache.entry(app_id).or_default();
        if let Some(existing) = list.iter_mut().find(|h| h.file_path == hash.file_path) {
            *existing = hash.clone();
        } else {
            list.push(hash.clone());
        }
        hash
    }

    pub fn get_hashes(&self, app_id: u32) -> Vec<AssetHash> {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&app_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn verify_file(&self, app_id: u32, file_path: &Path) -> bool {
        let hashes = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&app_id)
            .cloned()
            .unwrap_or_default();
        let target = file_path.to_string_lossy();
        hashes
            .iter()
            .any(|h| h.file_path == target && h.verify(file_path))
    }

    pub fn clear_cache(&mut self) {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
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
        let file_path = dir.path().join("test.bin");
        let mut f = std::fs::File::create(&file_path).unwrap();
        f.write_all(b"test data").unwrap();

        let service = AssetHashService::new();
        let hash = service.hash_file(1245620, &file_path, "test.bin");

        assert_eq!(hash.app_id, 1245620);
        assert_eq!(hash.file_name, "test.bin");
        assert_eq!(hash.file_size, 9);
        assert_eq!(
            hash.sha256,
            "916f0027a575074ce72a331777c3478d6513f786a591bd892da1a577bf2335f9"
        );
        assert_eq!(hash.sha1, "f48dd853820860816c75d54d0f584dc863327a7c");
        assert_eq!(hash.md5, "eb733a00c0c9d336e65691a37ab54293");
        assert!(hash.last_verified.is_some());
    }

    #[test]
    fn test_verify_file_and_rehash_replaces_stale_entry() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.bin");
        std::fs::write(&file_path, b"version 1").unwrap();

        let service = AssetHashService::new();
        let _h1 = service.hash_file(1245620, &file_path, "test.bin");
        assert!(service.verify_file(1245620, &file_path));

        std::fs::write(&file_path, b"version 2").unwrap();
        assert!(!service.verify_file(1245620, &file_path));

        let _h2 = service.hash_file(1245620, &file_path, "test.bin");
        assert_eq!(service.get_hashes(1245620).len(), 1);
        assert!(service.verify_file(1245620, &file_path));

        // Reverting to version 1 must fail now that version 2 replaced it
        std::fs::write(&file_path, b"version 1").unwrap();
        assert!(!service.verify_file(1245620, &file_path));
    }

    #[test]
    fn test_missing_file_does_not_verify_as_empty() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("missing.bin");
        let hash = AssetHash::new(1245620, &missing, "missing.bin");
        assert!(hash.sha256.is_empty());
        assert!(hash.last_verified.is_none());

        // Creating an empty file at the same path must NOT match the failed hash
        std::fs::write(&missing, b"").unwrap();
        assert!(!hash.verify(&missing));
    }
}
