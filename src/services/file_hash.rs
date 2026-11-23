//! FileHash - Cryptographic hashing of individual files
//!
//! Provides SHA-256, SHA-1, and MD5 hashing for individual files
//! with caching and verification support.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use md5::compute as md5_compute;

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
    pub fn new(file_path: &Path, file_name: &str) -> Self {
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
            .unwrap()
            .insert(file_path.to_string_lossy().to_string(), hash.clone());
        hash
    }

    pub fn get_hash(&self, file_path: &Path) -> Option<FileHash> {
        self.cache
            .lock()
            .unwrap()
            .get(&file_path.to_string_lossy().to_string())
            .cloned()
    }

    pub fn verify_file(&self, file_path: &Path) -> bool {
        self.cache
            .lock()
            .unwrap()
            .get(&file_path.to_string_lossy().to_string())
            .map(|h| h.verify(file_path))
            .unwrap_or(false)
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
        let file_path = dir.path().join("test.txt");
        let mut f = std::fs::File::create(&file_path).unwrap();
        f.write_all(b"hello world").unwrap();

        let service = FileHashService::new();
        let hash = service.hash_file(&file_path, "test.txt");

        assert_eq!(hash.file_name, "test.txt");
        assert!(!hash.sha256.is_empty());
        assert!(service.verify_file(&file_path));
    }
}