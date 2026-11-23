//! AssetHash - Cryptographic hashing of game assets for integrity verification
//!
//! Provides SHA-256, SHA-1, and MD5 hashing of game files to verify integrity
//! and detect tampering or corruption.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use md5::{compute as md5_compute};

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
    pub fn new(app_id: u32, file_path: &Path, file_name: &str) -> Self {
        let file_size = fs::metadata(file_path).map(|m| m.len()).unwrap_or(0);
        let bytes = fs::read(file_path).unwrap_or_default();

        let mut sha256_hasher = Sha256::new();
        sha256_hasher.update(&bytes);
        let sha256 = format!("{:x}", sha256_hasher.finalize());

        let mut sha1_hasher = sha1::Sha1::new();
        sha1_hasher.update(&bytes);
        let sha1 = format!("{:x}", sha1_hasher.finalize());

        let md5 = format!("{:x}", md5_compute(&bytes));

        Self {
            app_id,
            file_path: file_path.to_string_lossy().to_string(),
            file_name: file_name.to_string(),
            file_size,
            sha256,
            sha1,
            md5,
            last_verified: Some(chrono::Utc::now().to_rfc3339()),
        }
    }

    pub fn verify(&self, file_path: &Path) -> bool {
        let bytes = match fs::read(file_path) {
            Ok(b) => b,
            Err(_) => return false,
        };

        let mut hasher = Sha256::new();
        hasher.update(&bytes);
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
        self.cache
            .lock()
            .unwrap()
            .entry(app_id)
            .or_default()
            .push(hash.clone());
        hash
    }

    pub fn get_hashes(&self, app_id: u32) -> Vec<AssetHash> {
        self.cache
            .lock()
            .unwrap()
            .get(&app_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn verify_file(&self, app_id: u32, file_path: &Path) -> bool {
        let hashes = self.cache.lock().unwrap().get(&app_id).cloned().unwrap_or_default();
        hashes.iter().any(|h| h.file_path == file_path.to_string_lossy() && h.verify(file_path))
    }

    pub fn clear_cache(&mut self) {
        self.cache.lock().unwrap().clear();
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
        assert!(!hash.sha256.is_empty());
        assert!(!hash.sha1.is_empty());
        assert!(!hash.md5.is_empty());
    }

    #[test]
    fn test_verify_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.bin");
        std::fs::write(&file_path, b"test data").unwrap();

        let service = AssetHashService::new();
        let hash = service.hash_file(1245620, &file_path, "test.bin");

        assert!(service.verify_file(1245620, &file_path));
    }
}