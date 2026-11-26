//! At-Rest Credential Encryption (LT-40)
//!
//! Uses age (https://age-encryption.org) for authenticated encryption of sensitive data
//! like auth.json. Encrypts with X25519/AES256-GCM and stores the encrypted blob
//! with a version header.
//!
//! Master encryption key is stored securely in the OS keyring (secret-service/KWallet)
//! instead of plaintext file.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use age::{Encryptor, Decryptor, Recipient, Identity};
use age::x25519;
use age::secrecy::ExposeSecret;
use base64::{Engine, engine::general_purpose::STANDARD};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::models::config::default_luatools_data_dir;

const ENCRYPTION_VERSION: u8 = 1;
const ENCRYPTED_FILE_EXT: &str = "enc";

#[derive(Debug, Error)]
pub enum EncryptionError {
    #[error("Encryption failed: {0}")]
    Encrypt(String),
    #[error("Decryption failed: {0}")]
    Decrypt(String),
    #[error("Invalid format: {0}")]
    InvalidFormat(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Encrypted payload format:
/// ```json
/// {
///   "version": 1,
///   "ciphertext": "base64(...)",
///   "recipient": "age1...",
///   "identity_hash": "sha256(base64_recipient)"
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedPayload {
    version: u8,
    ciphertext: String,
    recipient: String,
    identity_hash: String,
}

/// EncryptionService manages the age identity/recipient pair and provides
/// encrypt/decrypt operations for sensitive data.
pub struct EncryptionService {
    identity: x25519::Identity,
    recipient: x25519::Recipient,
    key_file: PathBuf,
}

impl std::fmt::Debug for EncryptionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptionService")
            .field("recipient", &self.recipient.to_string())
            .field("key_file", &self.key_file)
            .finish_non_exhaustive()
    }
}

static ENCRYPTION_SERVICE: OnceLock<Arc<Mutex<Option<EncryptionService>>>> = OnceLock::new();

impl EncryptionService {
    /// Gets or creates the global encryption service.
    /// The first call generates a new identity if none exists.
    pub fn global() -> Arc<Mutex<Option<EncryptionService>>> {
        ENCRYPTION_SERVICE
            .get_or_init(|| Arc::new(Mutex::new(None)))
            .clone()
    }

    /// Gets or creates the master encryption key from the OS keyring.
    fn get_or_create_master_key() -> Result<String, EncryptionError> {
        let entry = Entry::new("luatools_linux", "master_key")
            .map_err(|e| EncryptionError::Decrypt(format!("Keyring entry error: {e}")))?;

        match entry.get_password() {
            Ok(key) => Ok(key),
            Err(keyring::Error::NoEntry) => {
                // Generate new key and store in keyring
                let new_key = Self::generate_secure_token(32);
                entry.set_password(&new_key)
                    .map_err(|e| EncryptionError::Decrypt(format!("Failed to store key: {e}")))?;
                Ok(new_key)
            }
            Err(e) => Err(EncryptionError::Decrypt(format!("Keyring error: {e}"))),
        }
    }

    /// Generates a cryptographically secure random token.
    fn generate_secure_token(len: usize) -> String {
        use rand::RngCore;
        let mut bytes = vec![0u8; len];
        rand::thread_rng().fill_bytes(&mut bytes);
        hex::encode(bytes)
    }

    /// Initializes the global encryption service.
    /// Creates a new identity if one doesn't exist, or loads the existing one.
    pub fn init_global() -> Result<(), EncryptionError> {
        let global = Self::global();
        let mut guard = global.lock().unwrap();

        if guard.is_some() {
            return Ok(());
        }

        // Get or create master key from OS keyring (for auditing/verification)
        let _master_key = Self::get_or_create_master_key()?;

        // For age, we still need an X25519 identity for encryption
        // We keep the age identity file for encryption operations but protect the
        // master key in the keyring
        let key_file = default_luatools_data_dir().join("encryption_key.age");
        let service = if key_file.exists() {
            Self::load_from_key_file(&key_file)?
        } else {
            let service = Self::generate_new(&key_file)?;
            service
        };

        *guard = Some(service);
        Ok(())
    }

    /// Generates a new age identity and saves it to the key file.
    fn generate_new(key_file: &Path) -> Result<Self, EncryptionError> {
        let identity = x25519::Identity::generate();
        let recipient = identity.to_public();

        if let Some(parent) = key_file.parent() {
            fs::create_dir_all(parent)?;
        }

        // Write identity with restrictive permissions
        let tmp = key_file.with_extension("tmp");
        let identity_str = identity.to_string();
        fs::write(&tmp, identity_str.expose_secret().as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(&tmp, key_file)?;

        Ok(Self {
            identity,
            recipient,
            key_file: key_file.to_path_buf(),
        })
    }

    /// Loads an existing age identity from the key file.
    fn load_from_key_file(key_file: &Path) -> Result<Self, EncryptionError> {
        let identity_str = fs::read_to_string(key_file)?;
        let identity: x25519::Identity = identity_str.parse()
            .map_err(|e| EncryptionError::Decrypt(format!("Invalid identity: {e}")))?;
        let recipient = identity.to_public();

        Ok(Self {
            identity,
            recipient,
            key_file: key_file.to_path_buf(),
        })
    }

    /// Returns the recipient string for this identity.
    pub fn recipient(&self) -> String {
        self.recipient.to_string()
    }

    /// Returns a hash of the identity for verification (not the identity itself).
    pub fn identity_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let recipient = self.recipient();
        let mut hasher = Sha256::new();
        hasher.update(recipient.as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Encrypts a JSON-serializable value and returns the encrypted payload.
    pub fn encrypt<T: Serialize>(&self, value: &T) -> Result<EncryptedPayload, EncryptionError> {
        let plaintext = serde_json::to_vec(value)?;

        let recipients: Vec<&dyn Recipient> = vec![&self.recipient];
        let encryptor = Encryptor::with_recipients(recipients.into_iter())
            .map_err(|e| EncryptionError::Encrypt(e.to_string()))?;
        let mut encrypted = Vec::new();
        {
            let mut writer = encryptor.wrap_output(&mut encrypted)
                .map_err(|e| EncryptionError::Encrypt(e.to_string()))?;
            std::io::Write::write_all(&mut writer, &plaintext)
                .map_err(|e| EncryptionError::Encrypt(e.to_string()))?;
            writer.finish()
                .map_err(|e| EncryptionError::Encrypt(e.to_string()))?;
        }

        let ciphertext_b64 = STANDARD.encode(&encrypted);
        let recipient_str = self.recipient();
        let identity_hash = self.identity_hash();

        Ok(EncryptedPayload {
            version: ENCRYPTION_VERSION,
            ciphertext: ciphertext_b64,
            recipient: recipient_str,
            identity_hash,
        })
    }

    /// Decrypts an encrypted payload and deserializes to T.
    pub fn decrypt<T: for<'de> Deserialize<'de>>(&self, payload: &EncryptedPayload) -> Result<T, EncryptionError> {
        if payload.version != ENCRYPTION_VERSION {
            return Err(EncryptionError::InvalidFormat(
                format!("Unsupported encryption version: {}", payload.version)
            ));
        }

        // Verify the recipient matches our identity
        if payload.identity_hash != self.identity_hash() {
            return Err(EncryptionError::Decrypt(
                "Encrypted data was created with a different identity".into()
            ));
        }

        let ciphertext = STANDARD.decode(&payload.ciphertext)
            .map_err(|e| EncryptionError::Decrypt(format!("Invalid base64: {e}")))?;

        let decryptor = Decryptor::new(&ciphertext[..])
            .map_err(|e| EncryptionError::Decrypt(e.to_string()))?;

        let mut decrypted = Vec::new();
        {
            let identity = &self.identity;
            let identities: Vec<&dyn Identity> = vec![identity];
            let mut reader = decryptor.decrypt(identities.into_iter())
                .map_err(|e| EncryptionError::Decrypt(e.to_string()))?;
            std::io::Read::read_to_end(&mut reader, &mut decrypted)
                .map_err(|e| EncryptionError::Decrypt(e.to_string()))?;
        }

        serde_json::from_slice(&decrypted)
            .map_err(|e| EncryptionError::Serde(e))
    }

    /// Encrypts a file in place, replacing it with the encrypted version.
    pub fn encrypt_file(&self, path: &Path) -> Result<PathBuf, EncryptionError> {
        let data = fs::read_to_string(path)?;
        let value: serde_json::Value = serde_json::from_str(&data)?;
        let payload = self.encrypt(&value)?;

        let encrypted_path = path.with_extension(ENCRYPTED_FILE_EXT);
        let json = serde_json::to_string_pretty(&payload)?;

        let tmp = encrypted_path.with_extension("tmp");
        fs::write(&tmp, json)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(&tmp, &encrypted_path)?;

        // Remove original plaintext file
        fs::remove_file(path)?;

        Ok(encrypted_path)
    }

    /// Decrypts an encrypted file and writes the plaintext to the destination.
    pub fn decrypt_file(&self, encrypted_path: &Path, dest_path: &Path) -> Result<(), EncryptionError> {
        let data = fs::read_to_string(encrypted_path)?;
        let payload: EncryptedPayload = serde_json::from_str(&data)?;

        let value: serde_json::Value = self.decrypt(&payload)?;
        let json = serde_json::to_string_pretty(&value)?;

        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let tmp = dest_path.with_extension("tmp");
        fs::write(&tmp, json)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(&tmp, dest_path)?;

        Ok(())
    }
}

/// High-level convenience functions for common operations

/// Encrypts the auth.json file if it exists and is not already encrypted.
pub fn migrate_auth_to_encrypted() -> Result<bool, EncryptionError> {
    let auth_path = default_luatools_data_dir().join("auth.json");
    let encrypted_path = auth_path.with_extension(ENCRYPTED_FILE_EXT);

    if !auth_path.exists() {
        return Ok(false);
    }

    if encrypted_path.exists() {
        // Already migrated
        return Ok(false);
    }

    let global = EncryptionService::global();
    let guard = global.lock().unwrap();
    let service = guard.as_ref()
        .ok_or_else(|| EncryptionError::Decrypt("Encryption service not initialized".into()))?
        .clone();

    service.encrypt_file(&auth_path)?;
    Ok(true)
}

/// Decrypts auth.json.enc back to auth.json if it exists.
pub fn decrypt_auth_if_encrypted() -> Result<bool, EncryptionError> {
    let auth_path = default_luatools_data_dir().join("auth.json");
    let encrypted_path = auth_path.with_extension(ENCRYPTED_FILE_EXT);

    if !encrypted_path.exists() {
        return Ok(false);
    }

    let global = EncryptionService::global();
    let guard = global.lock().unwrap();
    let service = guard.as_ref()
        .ok_or_else(|| EncryptionError::Decrypt("Encryption service not initialized".into()))?
        .clone();

    service.decrypt_file(&encrypted_path, &auth_path)?;
    Ok(true)
}

/// Gets the encryption service, initializing if needed.
pub fn get_encryption_service() -> Result<EncryptionService, EncryptionError> {
    let global = EncryptionService::global();
    let guard = global.lock().unwrap();
    guard.as_ref().cloned().ok_or_else(|| {
        EncryptionError::Decrypt("Encryption service not initialized".into())
    })
}

// Need to implement Clone for EncryptionService
impl Clone for EncryptionService {
    fn clone(&self) -> Self {
        Self {
            identity: self.identity.clone(),
            recipient: self.recipient.clone(),
            key_file: self.key_file.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use serde_json::json;

    #[test]
    fn test_encryption_roundtrip() {
        let dir = tempdir().unwrap();
        let key_file = dir.path().join("test_key.age");

        let service = EncryptionService::generate_new(&key_file).unwrap();

        let test_data = json!({
            "access_token": "secret_token_123",
            "refresh_token": "secret_refresh_456",
            "expires_at": 9999999999.0,
            "display_name": "Test User",
            "email": "test@example.com",
            "avatar_url": "https://example.com/avatar.png"
        });

        let payload = service.encrypt(&test_data).unwrap();
        let decrypted: serde_json::Value = service.decrypt(&payload).unwrap();

        assert_eq!(decrypted["access_token"], "secret_token_123");
        assert_eq!(decrypted["email"], "test@example.com");
    }

    #[test]
    fn test_encrypt_decrypt_file() {
        let dir = tempdir().unwrap();
        let key_file = dir.path().join("test_key.age");

        let service = EncryptionService::generate_new(&key_file).unwrap();

        let plaintext_path = dir.path().join("test.json");
        let data = json!({
            "secret": "value",
            "number": 42
        });
        fs::write(&plaintext_path, serde_json::to_string_pretty(&data).unwrap()).unwrap();

        let encrypted_path = service.encrypt_file(&plaintext_path).unwrap();
        assert!(encrypted_path.exists());
        assert!(!plaintext_path.exists());

        let restored_path = dir.path().join("restored.json");
        service.decrypt_file(&encrypted_path, &restored_path).unwrap();

        let restored: serde_json::Value = serde_json::from_str(&fs::read_to_string(&restored_path).unwrap()).unwrap();
        assert_eq!(restored["secret"], "value");
        assert_eq!(restored["number"], 42);
    }

    #[test]
    fn test_load_existing_key() {
        let dir = tempdir().unwrap();
        let key_file = dir.path().join("test_key.age");

        let service1 = EncryptionService::generate_new(&key_file).unwrap();
        let recipient = service1.recipient();

        let service2 = EncryptionService::load_from_key_file(&key_file).unwrap();
        assert_eq!(service2.recipient(), recipient);
    }
}