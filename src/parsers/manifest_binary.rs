//! Binary .manifest Magic/Protobuf Decoder (LT-60, LT-61, LT-62)
//!
//! Parses Steam depot manifest binary format:
//! - Magic headers: 0x71F617D0 (v1), 0x1F4812BE (v2), 0x32C415AB (v3)
//! - 'z' compressed wrapper (single-entry ZIP)
//! - Protobuf payload decoding
//! - AES key validation for encrypted filenames

use std::fs;
use std::io::{Cursor, Read};
use std::path::Path;

use bytes::Buf;
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::Sha256;
use thiserror::Error;
use zip::read::ZipArchive;

pub const MAGIC_V1: u32 = 0x71F617D0;
pub const MAGIC_V2: u32 = 0x1F4812BE;
pub const MAGIC_V3: u32 = 0x32C415AB;
pub const MAGIC_Z_WRAPPER: u32 = 0x7A; // 'z' character

pub const MANIFEST_PAYLOAD_MAGIC: u32 = MAGIC_V1;
pub const MANIFEST_METADATA_MAGIC: u32 = MAGIC_V2;
pub const MANIFEST_EOF_MAGIC: u32 = MAGIC_V3;

/// Legacy compatibility wrapper for binary manifest parsing (LT-60, LT-61, LT-62)
pub struct ManifestFile;

impl ManifestFile {
    /// Unwraps single-entry "z" PK-ZIP container if zipped (LT-60).
    pub fn unwrap_zip(data: &[u8]) -> Vec<u8> {
        if data.len() >= 2 && data[0] == b'P' && data[1] == b'K' {
            let cursor = Cursor::new(data);
            if let Ok(mut zip) = ZipArchive::new(cursor) {
                if let Ok(mut entry) = zip.by_index(0) {
                    let mut out = Vec::new();
                    if entry.read_to_end(&mut out).is_ok() {
                        return out;
                    }
                }
            }
        }
        data.to_vec()
    }

    /// Finds section by magic in [magic:u32][len:u32][bytes] stream.
    pub fn find_section(data: &[u8], target_magic: u32) -> Option<&[u8]> {
        let mut offset = 0;
        while offset + 8 <= data.len() {
            let magic = u32::from_le_bytes(data[offset..offset + 4].try_into().ok()?);
            let len = u32::from_le_bytes(data[offset + 4..offset + 8].try_into().ok()?) as usize;
            offset += 8;
            if magic == MANIFEST_EOF_MAGIC {
                break;
            }
            if offset + len > data.len() {
                break;
            }
            if magic == target_magic {
                return Some(&data[offset..offset + len]);
            }
            offset += len;
        }
        None
    }

    /// Decodes protobuf metadata section (LT-61).
    pub fn parse_metadata(meta_bytes: &[u8]) -> Option<ManifestMetadata> {
        let mut offset = 0;
        let mut depot_id = 0u32;
        let mut gid_manifest = 0u64;
        let mut filenames_encrypted = false;
        let mut cb_disk_original = 0u64;

        while offset < meta_bytes.len() {
            let (field, wire) = Self::read_tag(meta_bytes, &mut offset)?;
            if wire == 0 {
                let v = Self::read_varint(meta_bytes, &mut offset)?;
                match field {
                    1 => depot_id = v as u32,
                    2 => gid_manifest = v,
                    4 => filenames_encrypted = v != 0,
                    5 => cb_disk_original = v,
                    _ => {}
                }
            } else if !Self::skip_field(meta_bytes, &mut offset, wire) {
                return None;
            }
        }

        Some(ManifestMetadata {
            depot_id,
            manifest_gid: gid_manifest,
            version: ManifestVersion::V2,
            filenames_encrypted,
            file_count: 0,
            total_size: cb_disk_original,
            file_flags: 0,
            creation_time: 0,
            sha_content: Vec::new(),
            sha_metadata: Vec::new(),
        })
    }

    /// Read metadata from a file path.
    pub fn try_read(path: &Path) -> Option<ManifestMetadata> {
        let raw = fs::read(path).ok()?;
        let data = Self::unwrap_zip(&raw);
        let meta = Self::find_section(&data, MANIFEST_METADATA_MAGIC)?;
        Self::parse_metadata(meta)
    }

    /// Verifies candidate 32-byte key against encrypted filenames in manifest (LT-62).
    /// If filenames are not encrypted, returns true.
    /// Returns false for corrupted/unreadable manifests.
    pub fn key_looks_valid(path: &Path, key_bytes: &[u8]) -> bool {
        if key_bytes.len() != 32 {
            return false;
        }
        let Some(meta) = Self::try_read(path) else {
            // Corrupted manifest - reject
            return false;
        };
        if !meta.filenames_encrypted {
            return true;
        }
        // Manifest filenames are encrypted: try to decrypt at least one filename
        let mut key_arr = [0u8; 32];
        key_arr.copy_from_slice(key_bytes);
        crate::parsers::manifest_binary::validate_aes_key(path, &key_arr).unwrap_or(false)
    }

    fn read_tag(d: &[u8], o: &mut usize) -> Option<(u32, u8)> {
        let tag = Self::read_varint(d, o)?;
        Some(((tag >> 3) as u32, (tag & 0x07) as u8))
    }

    fn read_varint(d: &[u8], o: &mut usize) -> Option<u64> {
        let mut val = 0u64;
        let mut shift = 0;
        while *o < d.len() {
            let b = d[*o];
            *o += 1;
            val |= ((b & 0x7F) as u64) << shift;
            if (b & 0x80) == 0 {
                return Some(val);
            }
            shift += 7;
            if shift > 63 {
                return None;
            }
        }
        None
    }

    fn skip_field(d: &[u8], o: &mut usize, wire: u8) -> bool {
        match wire {
            0 => Self::read_varint(d, o).is_some(),
            1 => {
                *o += 8;
                *o <= d.len()
            }
            5 => {
                *o += 4;
                *o <= d.len()
            }
            2 => {
                if let Some(len) = Self::read_varint(d, o) {
                    let len_usize = match usize::try_from(len) {
                        Ok(l) => l,
                        Err(_) => return false,
                    };
                    match o.checked_add(len_usize) {
                        Some(new_o) if new_o <= d.len() => {
                            *o = new_o;
                            true
                        }
                        _ => false,
                    }
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

/// Steam manifest format version
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ManifestVersion {
    #[default]
    V1,
    V2,
    V3,
}

/// Decoded manifest metadata (LT-61)
#[derive(Debug, Clone, Default)]
pub struct ManifestMetadata {
    pub depot_id: u32,
    pub manifest_gid: u64,
    pub version: ManifestVersion,
    pub filenames_encrypted: bool,
    pub file_count: u32,
    pub total_size: u64,
    pub file_flags: u32,
    pub creation_time: u64,
    pub sha_content: Vec<u8>,
    pub sha_metadata: Vec<u8>,
}

/// Encrypted filename entry
#[derive(Debug, Clone)]
pub struct EncryptedFilenameEntry {
    pub offset: u64,
    pub size: u64,
    pub encrypted_name: Vec<u8>,
}

/// Binary manifest container header (after unwrap)
#[derive(Debug, Clone)]
struct ManifestHeader {
    version: ManifestVersion,
    depot_id: u32,
    manifest_gid: u64,
    file_count: u32,
    total_size: u64,
    file_flags: u32,
    creation_time: u64,
    sha_content: Vec<u8>,
    sha_metadata: Vec<u8>,
}

/// Errors for manifest parsing
#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("Invalid magic header: expected one of 0x71F617D0, 0x1F4812BE, 0x32C415AB, got 0x{0:08X}")]
    InvalidMagic(u32),
    #[error("File too small for manifest header: {0} bytes")]
    FileTooSmall(usize),
    #[error("ZIP unwrap failed: {0}")]
    ZipError(String),
    #[error("Zlib decompression failed: {0}")]
    ZlibError(String),
    #[error("Protobuf parsing error: {0}")]
    ProtobufError(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Checksum mismatch: expected {expected:02x?}, got {actual:02x?}")]
    ChecksumMismatch { expected: Vec<u8>, actual: Vec<u8> },
    #[error("AES key validation failed: {0}")]
    AesKeyInvalid(String),
}

/// Reads and parses a Steam depot manifest file (LT-60, LT-61)
pub fn read_manifest_file(path: &Path) -> Result<ManifestMetadata, ManifestError> {
    let data = fs::read(path)?;
    parse_manifest_bytes(&data)
}

/// Parses manifest from byte slice (handles magic, ZIP unwrap, protobuf)
fn parse_manifest_bytes(data: &[u8]) -> Result<ManifestMetadata, ManifestError> {
    if data.len() < 4 {
        return Err(ManifestError::FileTooSmall(data.len()));
    }

    let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);

    // Handle 'z' wrapper (compressed single-entry ZIP)
    let payload = if magic == MAGIC_Z_WRAPPER {
        unwrap_z_wrapper(data)?
    } else {
        data.to_vec()
    };

    // Now parse the unwrapped payload
    parse_manifest_payload(&payload)
}

/// Unwraps 'z' single-entry ZIP wrapper
fn unwrap_z_wrapper(data: &[u8]) -> Result<Vec<u8>, ManifestError> {
    let cursor = Cursor::new(data);
    let mut archive = ZipArchive::new(cursor)
        .map_err(|e| ManifestError::ZipError(e.to_string()))?;

    if archive.len() != 1 {
        return Err(ManifestError::ZipError(
            format!("Expected 1 entry in z-wrapper, found {}", archive.len())
        ));
    }

    let mut file = archive.by_index(0)
        .map_err(|e| ManifestError::ZipError(e.to_string()))?;

    let mut contents = Vec::new();
    file.read_to_end(&mut contents)
        .map_err(|e| ManifestError::ZipError(e.to_string()))?;

    Ok(contents)
}

/// Parses the actual manifest payload after any unwrapping
fn parse_manifest_payload(data: &[u8]) -> Result<ManifestMetadata, ManifestError> {
    if data.len() < 4 {
        return Err(ManifestError::FileTooSmall(data.len()));
    }

    let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);

    let version = match magic {
        MAGIC_V1 => ManifestVersion::V1,
        MAGIC_V2 => ManifestVersion::V2,
        MAGIC_V3 => ManifestVersion::V3,
        _ => return Err(ManifestError::InvalidMagic(magic)),
    };

    // Skip magic and parse header
    let mut cursor = Cursor::new(&data[4..]);
    let header = parse_manifest_header(&mut cursor, version)?;

    // Read file entries to compute sizes
    let mut file_entries = Vec::new();
    for _ in 0..header.file_count {
        let entry = parse_file_entry(&mut cursor)?;
        file_entries.push(entry);
    }

    // Build metadata
    Ok(ManifestMetadata {
        depot_id: header.depot_id,
        manifest_gid: header.manifest_gid,
        version: header.version,
        filenames_encrypted: !file_entries.is_empty() && file_entries[0].encrypted_name.len() > 0,
        file_count: header.file_count,
        total_size: header.total_size,
        file_flags: header.file_flags,
        creation_time: header.creation_time,
        sha_content: header.sha_content,
        sha_metadata: header.sha_metadata,
    })
}

/// Parses the manifest header (protobuf-like wire format)
fn parse_manifest_header(cursor: &mut Cursor<&[u8]>, version: ManifestVersion) -> Result<ManifestHeader, ManifestError> {
    let mut header = ManifestHeader {
        version,
        depot_id: 0,
        manifest_gid: 0,
        file_count: 0,
        total_size: 0,
        file_flags: 0,
        creation_time: 0,
        sha_content: Vec::new(),
        sha_metadata: Vec::new(),
    };

    // Steam manifest uses a simple TLV-like format after the magic
    // Fields are varint-encoded (similar to protobuf)
    while cursor.has_remaining() {
        let tag = read_varint(cursor)?;
        let wire_type = tag & 0x7;
        let field_num = tag >> 3;

        match (field_num, wire_type) {
            (1, 0) => { // depot_id (varint)
                header.depot_id = read_varint(cursor)? as u32;
            }
            (2, 0) => { // manifest_gid (varint)
                header.manifest_gid = read_varint(cursor)?;
            }
            (3, 0) => { // file_count (varint)
                header.file_count = read_varint(cursor)? as u32;
            }
            (4, 0) => { // total_size (varint)
                header.total_size = read_varint(cursor)?;
            }
            (5, 0) => { // file_flags (varint)
                header.file_flags = read_varint(cursor)? as u32;
            }
            (6, 0) => { // creation_time (varint)
                header.creation_time = read_varint(cursor)?;
            }
            (7, 2) => { // sha_content (length-delimited)
                header.sha_content = read_length_delimited(cursor)?;
            }
            (8, 2) => { // sha_metadata (length-delimited)
                header.sha_metadata = read_length_delimited(cursor)?;
            }
            (_, 0) | (_, 2) => {
                // Skip unknown fields
                if wire_type == 0 {
                    let _ = read_varint(cursor)?;
                } else if wire_type == 2 {
                    let _ = read_length_delimited(cursor)?;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }

    Ok(header)
}

/// Parses a single file entry
fn parse_file_entry(cursor: &mut Cursor<&[u8]>) -> Result<EncryptedFilenameEntry, ManifestError> {
    let offset = read_varint(cursor)?;
    let size = read_varint(cursor)?;
    let name_len = read_varint(cursor)?;

    if name_len > 1024 {
        return Err(ManifestError::ProtobufError(
            format!("Suspicious filename length: {}", name_len)
        ));
    }

    let mut encrypted_name = vec![0u8; name_len as usize];
    cursor.read_exact(&mut encrypted_name)?;

    Ok(EncryptedFilenameEntry {
        offset,
        size,
        encrypted_name,
    })
}

/// Reads a varint (protobuf encoding)
fn read_varint(cursor: &mut Cursor<&[u8]>) -> Result<u64, ManifestError> {
    let mut result = 0u64;
    let mut shift = 0;
    loop {
        if !cursor.has_remaining() {
            return Err(ManifestError::ProtobufError("Unexpected EOF reading varint".into()));
        }
        let byte = cursor.get_u8();
        result |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift >= 64 {
            return Err(ManifestError::ProtobufError("Varint too long".into()));
        }
    }
    Ok(result)
}

/// Reads a length-delimited field (protobuf wire type 2)
fn read_length_delimited(cursor: &mut Cursor<&[u8]>) -> Result<Vec<u8>, ManifestError> {
    let len = read_varint(cursor)? as usize;
    if len > cursor.remaining() {
        return Err(ManifestError::ProtobufError(
            format!("Length-delimited field too long: {} > {}", len, cursor.remaining())
        ));
    }
    let mut buf = vec![0u8; len];
    cursor.read_exact(&mut buf)?;
    Ok(buf)
}

/// Validates an AES key against encrypted manifest filenames (LT-62)
/// Returns true if the key can successfully decrypt at least one filename
/// Returns true for unencrypted manifests (no key required)
pub fn validate_aes_key(manifest_path: &Path, candidate_key: &[u8; 32]) -> Result<bool, ManifestError> {
    let metadata = read_manifest_file(manifest_path)?;
    if !metadata.filenames_encrypted {
        // No encryption = no key needed = valid
        return Ok(true);
    }

    // Re-read the file to get encrypted names
    let data = fs::read(manifest_path)?;
    let payload = if data.len() >= 4 && u32::from_le_bytes([data[0], data[1], data[2], data[3]]) == MAGIC_Z_WRAPPER {
        unwrap_z_wrapper(&data)?
    } else {
        data
    };

    // Skip header and iterate entries
    let mut cursor = Cursor::new(&payload[4..]);
    let _header = parse_manifest_header(&mut cursor, metadata.version)?;

    for _ in 0..metadata.file_count {
        let entry = parse_file_entry(&mut cursor)?;
        if !entry.encrypted_name.is_empty() {
            if try_decrypt_filename(&entry.encrypted_name, candidate_key).is_ok() {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

/// Attempts to decrypt a filename with AES-256-ECB
fn try_decrypt_filename(encrypted: &[u8], key: &[u8; 32]) -> Result<String, ManifestError> {
    use aes::Aes256;
    use cipher::{BlockDecrypt, KeyInit};

    if encrypted.len() % 16 != 0 {
        return Err(ManifestError::AesKeyInvalid("Not a multiple of block size".into()));
    }

    let cipher = Aes256::new(key.into());
    let mut blocks = encrypted.to_vec();

    // Decrypt in-place (ECB mode)
    for chunk in blocks.chunks_mut(16) {
        let mut block = [0u8; 16];
        block.copy_from_slice(chunk);
        cipher.decrypt_block(&mut block.into());
        chunk.copy_from_slice(&block);
    }

    // Remove PKCS#7 padding
    if let Some(last) = blocks.last() {
        let pad_len = *last as usize;
        if pad_len > 0 && pad_len <= 16 {
            let start = blocks.len().saturating_sub(pad_len);
            if blocks[start..].iter().all(|&b| b == *last) {
                blocks.truncate(start);
            }
        }
    }

    String::from_utf8(blocks).map_err(|_| ManifestError::AesKeyInvalid("Invalid UTF-8 after decryption".into()))
}

/// Validates an AES key using AES-256-CBC mode (alternative)
pub fn validate_aes_key_cbc(manifest_path: &Path, candidate_key: &[u8; 32], iv: &[u8; 16]) -> Result<bool, ManifestError> {
    let metadata = read_manifest_file(manifest_path)?;
    if !metadata.filenames_encrypted {
        return Ok(false);
    }

    let data = fs::read(manifest_path)?;
    let payload = if data.len() >= 4 && u32::from_le_bytes([data[0], data[1], data[2], data[3]]) == MAGIC_Z_WRAPPER {
        unwrap_z_wrapper(&data)?
    } else {
        data
    };

    let mut cursor = Cursor::new(&payload[4..]);
    let _header = parse_manifest_header(&mut cursor, metadata.version)?;

    for _ in 0..metadata.file_count {
        let entry = parse_file_entry(&mut cursor)?;
        if !entry.encrypted_name.is_empty() {
            if try_decrypt_filename_cbc(&entry.encrypted_name, candidate_key, iv).is_ok() {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

/// Attempts to decrypt a filename with AES-256-CBC (simplified implementation)
fn try_decrypt_filename_cbc(encrypted: &[u8], key: &[u8; 32], iv: &[u8; 16]) -> Result<String, ManifestError> {
    use aes::Aes256;
    use cipher::{BlockDecrypt, KeyInit};

    if encrypted.len() % 16 != 0 {
        return Err(ManifestError::AesKeyInvalid("Not a multiple of block size".into()));
    }

    // Manual CBC decryption to avoid crypto_common version conflicts
    let cipher = Aes256::new(key.into());
    let mut prev_block = *iv;
    let mut decrypted = Vec::with_capacity(encrypted.len());

    for chunk in encrypted.chunks(16) {
        let mut block = [0u8; 16];
        block.copy_from_slice(chunk);
        cipher.decrypt_block(&mut block.into());

        // XOR with previous ciphertext block (or IV for first block)
        for i in 0..16 {
            block[i] ^= prev_block[i];
        }

        decrypted.extend_from_slice(&block);
        prev_block.copy_from_slice(chunk);
    }

    // Remove PKCS#7 padding
    if let Some(&last) = decrypted.last() {
        let pad_len = last as usize;
        if pad_len > 0 && pad_len <= 16 {
            let start = decrypted.len().saturating_sub(pad_len);
            if decrypted[start..].iter().all(|&b| b == last) {
                decrypted.truncate(start);
            }
        }
    }

    String::from_utf8(decrypted).map_err(|_| ManifestError::AesKeyInvalid("Invalid UTF-8 after decryption".into()))
}

/// Computes SHA-1 hash of manifest content (for integrity verification)
pub fn compute_manifest_sha1(manifest_path: &Path) -> Result<Vec<u8>, ManifestError> {
    let data = fs::read(manifest_path)?;
    let payload = if data.len() >= 4 && u32::from_le_bytes([data[0], data[1], data[2], data[3]]) == MAGIC_Z_WRAPPER {
        unwrap_z_wrapper(&data)?
    } else {
        data
    };

    let mut hasher = Sha1::new();
    hasher.update(&payload);
    Ok(hasher.finalize().to_vec())
}

/// Computes SHA-256 hash of manifest content
pub fn compute_manifest_sha256(manifest_path: &Path) -> Result<Vec<u8>, ManifestError> {
    let data = fs::read(manifest_path)?;
    let payload = if data.len() >= 4 && u32::from_le_bytes([data[0], data[1], data[2], data[3]]) == MAGIC_Z_WRAPPER {
        unwrap_z_wrapper(&data)?
    } else {
        data
    };

    let mut hasher = Sha256::new();
    hasher.update(&payload);
    Ok(hasher.finalize().to_vec())
}

/// Computes MD5 hash of manifest content
pub fn compute_manifest_md5(manifest_path: &Path) -> Result<Vec<u8>, ManifestError> {
    let data = fs::read(manifest_path)?;
    let payload = if data.len() >= 4 && u32::from_le_bytes([data[0], data[1], data[2], data[3]]) == MAGIC_Z_WRAPPER {
        unwrap_z_wrapper(&data)?
    } else {
        data
    };

    let digest = md5::compute(&payload);
    Ok(digest.0.to_vec())
}

/// Extracts depot ID and GID from manifest filename pattern: `<depotId>_<gid>.manifest`
pub fn parse_manifest_filename(path: &Path) -> Option<(u32, u64)> {
    let name = path.file_name()?.to_str()?;
    if !name.ends_with(".manifest") {
        return None;
    }
    let name = name.trim_end_matches(".manifest");
    let parts: Vec<&str> = name.split('_').collect();
    if parts.len() != 2 {
        return None;
    }
    let depot_id = parts[0].parse().ok()?;
    let gid = parts[1].parse().ok()?;
    Some((depot_id, gid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_manifest_filename() {
        assert_eq!(parse_manifest_filename(Path::new("123_456.manifest")), Some((123, 456)));
        assert_eq!(parse_manifest_filename(Path::new("730_1234567890123456.manifest")), Some((730, 1234567890123456)));
        assert_eq!(parse_manifest_filename(Path::new("no_underscore.manifest")), None);
        assert_eq!(parse_manifest_filename(Path::new("not_a_manifest.txt")), None);
    }

    #[test]
    fn test_read_varint() {
        // Single byte
        let data: &[u8] = &[0x05];
        let mut cursor = Cursor::new(data);
        assert_eq!(read_varint(&mut cursor).unwrap(), 5);

        // Multi-byte varint: 300 = 0xAC 0x02
        let data: &[u8] = &[0xAC, 0x02];
        let mut cursor = Cursor::new(data);
        assert_eq!(read_varint(&mut cursor).unwrap(), 300);

        // Max uint64
        let data: &[u8] = &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01];
        let mut cursor = Cursor::new(data);
        assert_eq!(read_varint(&mut cursor).unwrap(), u64::MAX);
    }

    #[test]
    fn test_read_length_delimited() {
        // Length 5 + "hello"
        let data: &[u8] = &[0x05, b'h', b'e', b'l', b'l', b'o'];
        let mut cursor = Cursor::new(data);
        let result = read_length_delimited(&mut cursor).unwrap();
        assert_eq!(result, b"hello");
    }

    #[test]
    fn test_magic_constants() {
        assert_eq!(MAGIC_V1, 0x71F617D0);
        assert_eq!(MAGIC_V2, 0x1F4812BE);
        assert_eq!(MAGIC_V3, 0x32C415AB);
        assert_eq!(MAGIC_Z_WRAPPER, 0x7A);
    }
}