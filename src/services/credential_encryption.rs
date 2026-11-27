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

/// Prefix used to mark encrypted string fields persisted inside JSON configs such as `settings.json` (LT-40).
pub const SECRET_PREFIX: &str = "enc:v1:";

fn write_private_tmp_and_rename(dest: &Path, bytes: &[u8]) -> Result<(), EncryptionError> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = dest.with_extension("tmp");
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        fs::write(&tmp, bytes)?;
    }
    fs::rename(&tmp, dest)?;
    Ok(())
}

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

    /// Initializes the global encryption service.
    /// Stores/loads the X25519 identity in the OS keyring when available, removing any
    /// migrated plaintext `encryption_key.age`, and falls back cleanly to a `0600` local
    /// key file on headless/non-D-Bus Linux environments.
    pub fn init_global() -> Result<(), EncryptionError> {
        let global = Self::global();
        let mut guard = global.lock().unwrap_or_else(|e| e.into_inner());

        if guard.is_some() {
            return Ok(());
        }

        let key_file = default_luatools_data_dir().join("encryption_key.age");

        // Try OS keyring first for the X25519 identity secret; fall back cleanly on headless systems.
        let keyring_service = Entry::new("luatools_linux", "age_identity").ok().and_then(|entry| {
            match entry.get_password() {
                Ok(secret) => secret.parse::<x25519::Identity>().ok().map(|identity| {
                    // Clean up leftover plaintext file if keyring already holds the identity
                    if key_file.exists() {
                        let _ = fs::remove_file(&key_file);
                    }
                    let recipient = identity.to_public();
                    Self {
                        identity,
                        recipient,
                        key_file: key_file.clone(),
                    }
                }),
                Err(keyring::Error::NoEntry) => {
                    // Migrate existing file key into keyring if present, else generate fresh identity
                    let migrated_from_file = key_file.exists();
                    let svc = if migrated_from_file {
                        Self::load_from_key_file(&key_file).ok()
                    } else {
                        let identity = x25519::Identity::generate();
                        let recipient = identity.to_public();
                        Some(Self {
                            identity,
                            recipient,
                            key_file: key_file.clone(),
                        })
                    }?;
                    use age::secrecy::ExposeSecret;
                    let secret_str = svc.identity.to_string();
                    if entry.set_password(secret_str.expose_secret()).is_ok() {
                        if migrated_from_file {
                            let _ = fs::remove_file(&key_file);
                        }
                        Some(svc)
                    } else {
                        None
                    }
                }
                Err(e) => {
                    log::debug!(
                        "OS keyring unavailable ({}); falling back to 0600 local age key file",
                        e
                    );
                    None
                }
            }
        });

        let service = if let Some(svc) = keyring_service {
            svc
        } else if key_file.exists() {
            Self::load_from_key_file(&key_file)?
        } else {
            Self::generate_new(&key_file)?
        };

        *guard = Some(service);
        Ok(())
    }

    /// Generates a new age identity and saves it to the key file with `0o600` set at creation time.
    fn generate_new(key_file: &Path) -> Result<Self, EncryptionError> {
        let identity = x25519::Identity::generate();
        let recipient = identity.to_public();

        if let Some(parent) = key_file.parent() {
            fs::create_dir_all(parent)?;
        }

        let tmp = key_file.with_extension("tmp");
        let identity_str = identity.to_string();
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(identity_str.expose_secret().as_bytes())?;
            file.sync_all()?;
        }
        #[cfg(not(unix))]
        {
            fs::write(&tmp, identity_str.expose_secret().as_bytes())?;
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

    /// Encrypts a sensitive string (e.g. `hubcap_api_key`) into an `enc:v1:<base64>` token (LT-40).
    pub fn encrypt_string(&self, plaintext: &str) -> Result<String, EncryptionError> {
        if plaintext.is_empty() {
            return Ok(String::new());
        }
        let payload = self.encrypt(&plaintext)?;
        let json = serde_json::to_vec(&payload)?;
        Ok(format!("{}{}", SECRET_PREFIX, STANDARD.encode(&json)))
    }

    /// Decrypts an `enc:v1:<base64>` token back into plaintext. Passes non-prefixed strings through unchanged.
    pub fn decrypt_string(&self, encoded: &str) -> Result<String, EncryptionError> {
        let Some(b64) = encoded.strip_prefix(SECRET_PREFIX) else {
            return Ok(encoded.to_string());
        };
        let raw = STANDARD
            .decode(b64)
            .map_err(|e| EncryptionError::Decrypt(format!("Invalid secret base64: {e}")))?;
        let payload: EncryptedPayload = serde_json::from_slice(&raw)?;
        self.decrypt::<String>(&payload)
    }

    /// Encrypts a file in place, replacing it with the encrypted version (`0o600` at creation time).
    pub fn encrypt_file(&self, path: &Path) -> Result<PathBuf, EncryptionError> {
        let data = fs::read_to_string(path)?;
        let value: serde_json::Value = serde_json::from_str(&data)?;
        let payload = self.encrypt(&value)?;

        let encrypted_path = path.with_extension(ENCRYPTED_FILE_EXT);
        let json = serde_json::to_string_pretty(&payload)?;
        write_private_tmp_and_rename(&encrypted_path, json.as_bytes())?;

        // Remove original plaintext file
        fs::remove_file(path)?;

        Ok(encrypted_path)
    }

    /// Decrypts an encrypted file and writes the plaintext to the destination (`0o600` at creation time).
    pub fn decrypt_file(&self, encrypted_path: &Path, dest_path: &Path) -> Result<(), EncryptionError> {
        let data = fs::read_to_string(encrypted_path)?;
        let payload: EncryptedPayload = serde_json::from_str(&data)?;

        let value: serde_json::Value = self.decrypt(&payload)?;
        let json = serde_json::to_string_pretty(&value)?;
        write_private_tmp_and_rename(dest_path, json.as_bytes())?;

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
    let guard = global.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().cloned().ok_or_else(|| {
        EncryptionError::Decrypt("Encryption service not initialized".into())
    })
}

/// Encrypts a string using the global `EncryptionService` (auto-initializing on demand) (LT-40).
pub fn encrypt_secret_string(plaintext: &str) -> Result<String, EncryptionError> {
    if plaintext.is_empty() || plaintext.starts_with(SECRET_PREFIX) {
        return Ok(plaintext.to_string());
    }
    let _ = EncryptionService::init_global();
    let svc = get_encryption_service()?;
    svc.encrypt_string(plaintext)
}

/// Decrypts an `enc:v1:...` string using the global `EncryptionService` (auto-initializing on demand) (LT-40).
pub fn decrypt_secret_string(encoded: &str) -> Result<String, EncryptionError> {
    if !encoded.starts_with(SECRET_PREFIX) {
        return Ok(encoded.to_string());
    }
    let _ = EncryptionService::init_global();
    let svc = get_encryption_service()?;
    svc.decrypt_string(encoded)
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

        let token = service1.encrypt_string("smm_secret_api_key_12345").unwrap();
        assert!(token.starts_with(SECRET_PREFIX));
        assert!(!token.contains("smm_secret_api_key_12345"));
        assert_eq!(
            service2.decrypt_string(&token).unwrap(),
            "smm_secret_api_key_12345"
        );
    }
}