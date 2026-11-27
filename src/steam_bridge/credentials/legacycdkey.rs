//! Legacy CD-key resolution for Steam on Linux.
//! Ported from usefulsteamtools::LegacyCDKey.
//! Replaces Windows registry (HKCU\Software\Valve\Steam\Apps\<AppId>)
//! with reading/writing from active user's `userdata/<account_id>/config/localconfig.vdf`
//! plus a `0o600` JSON sidecar (`legacy_cdkeys.json`) so overrides survive Steam rewrites.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::steam_bridge::ticket_store::{TicketError, TicketStore};

pub const LEGACY_CDKEYS_SIDECAR_FILENAME: &str = "legacy_cdkeys.json";
const CROCKFORD_ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Error type for legacy CD-key operations.
#[derive(Debug, thiserror::Error)]
pub enum LegacyCdKeyError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Ticket store error: {0}")]
    TicketStore(#[from] TicketError),
    #[error("App {0} not found in ticket store")]
    NotFound(u32),
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("No override configured for app {0}; synthesizing deterministic key")]
    NoOverride(u32),
}

/// Represents the source of a legacy CD key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyCdKeySource {
    /// User-configured override stored in localconfig.vdf / legacy_cdkeys.json.
    Override,
    /// Deterministically synthesized key from appId+accountId.
    Synthesized,
}

/// Data stored for a legacy CD key entry in the store.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyCdKeyData {
    /// User-configured override key, if any.
    #[serde(default)]
    pub override_key: Option<String>,
    /// Whether a key has been set (override or synthesized).
    #[serde(default)]
    pub has_key: bool,
}

/// Store for legacy CD keys backed by active user's `userdata/<account_id>/config/localconfig.vdf`
/// and companion `0o600` JSON sidecar `legacy_cdkeys.json`.
#[derive(Debug, Clone)]
pub struct LegacyCdKeyStore {
    config_dir: PathBuf,
    cache: BTreeMap<u32, LegacyCdKeyData>,
}

impl Default for LegacyCdKeyStore {
    fn default() -> Self {
        Self::new(&Self::default_path())
    }
}

impl LegacyCdKeyStore {
    /// Create a store rooted at `config_dir`.
    ///
    /// Automatically resolves `<steam_root>` or `<steam_root>/config` to the active user's
    /// `<steam_root>/userdata/<account_id>/config` via [`TicketStore::resolve_config_dir`],
    /// falling back to `config_dir` in isolated directories without `userdata/`.
    pub fn new(config_dir: &Path) -> Self {
        Self {
            config_dir: TicketStore::resolve_config_dir(config_dir),
            cache: BTreeMap::new(),
        }
    }

    pub fn new_for_steam_root(steam_root: &Path) -> Self {
        Self::new(steam_root)
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn sidecar_path(&self) -> PathBuf {
        self.config_dir.join(LEGACY_CDKEYS_SIDECAR_FILENAME)
    }

    pub fn default_path() -> PathBuf {
        TicketStore::default_path()
    }

    pub fn load(&mut self) -> Result<(), TicketError> {
        let vdf_path = self.config_dir.join("localconfig.vdf");
        if vdf_path.exists() {
            self._parse_vdf(&vdf_path)?;
        } else {
            let alt = self.config_dir.join("localconfig.vdf.bak");
            if alt.exists() {
                self._parse_vdf(&alt)?;
            } else {
                self.cache.clear();
            }
        }

        // Merge companion `0o600` JSON sidecar (`legacy_cdkeys.json`) so user overrides
        // survive Steam client rewrites that strip unknown `"LegacyCDKey"` keys from VDF.
        let sidecar = self._load_sidecar();
        for (app_id, sc) in sidecar {
            if sc.override_key.as_deref().is_some_and(|k| !k.is_empty()) {
                self.cache.entry(app_id).or_insert(LegacyCdKeyData {
                    override_key: sc.override_key,
                    has_key: true,
                });
            }
        }

        Ok(())
    }

    fn is_apps_section_key(trimmed: &str) -> bool {
        trimmed.eq_ignore_ascii_case("\"apps\"")
    }

    fn write_atomic(path: &Path, content: &str) -> Result<(), TicketError> {
        static TMP_CDKEY_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let count = TMP_CDKEY_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("localconfig.vdf");
        let tmp_path =
            path.with_file_name(format!(".{}.cdkey.tmp.{}.{}", file_name, std::process::id(), count));
        fs::write(&tmp_path, content)?;
        fs::rename(&tmp_path, path)?;
        Ok(())
    }

    fn write_atomic_0600(path: &Path, content: &str) -> Result<(), TicketError> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        static TMP_CDKEY_0600_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let count = TMP_CDKEY_0600_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(LEGACY_CDKEYS_SIDECAR_FILENAME);
        let tmp_path =
            path.with_file_name(format!(".{}.cdkey.tmp.{}.{}", file_name, std::process::id(), count));
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)?;
        f.write_all(content.as_bytes())?;
        f.flush()?;
        drop(f);
        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600))?;
        fs::rename(&tmp_path, path)?;
        Ok(())
    }

    fn _load_sidecar(&self) -> BTreeMap<u32, LegacyCdKeyData> {
        let path = self.sidecar_path();
        let Ok(raw) = fs::read_to_string(&path) else {
            return BTreeMap::new();
        };
        serde_json::from_str::<BTreeMap<u32, LegacyCdKeyData>>(&raw).unwrap_or_default()
    }

    fn _save_sidecar(&self) -> Result<(), TicketError> {
        let persisted: BTreeMap<u32, &LegacyCdKeyData> = self
            .cache
            .iter()
            .filter(|(_, d)| d.override_key.as_deref().is_some_and(|k| !k.is_empty()))
            .map(|(&k, v)| (k, v))
            .collect();
        let json = serde_json::to_string_pretty(&persisted)
            .map_err(|e| TicketError::Parse(e.to_string()))?;
        Self::write_atomic_0600(&self.sidecar_path(), &json)
    }

    fn _parse_vdf(&mut self, path: &Path) -> Result<(), TicketError> {
        let content = fs::read_to_string(path)?;
        let app_blocks = TicketStore::extract_app_blocks(&content);
        let mut parsed = BTreeMap::new();
        for (appid_str, block) in app_blocks {
            if let Ok(app_id) = appid_str.parse::<u32>() {
                let mut data = LegacyCdKeyData::default();
                for line in block.lines() {
                    let line = line.trim();
                    if let Some((key, val)) = TicketStore::split_kv(line) {
                        if val.is_empty() {
                            continue;
                        }
                        if key == "LegacyCDKey" {
                            data.override_key = Some(val);
                            data.has_key = true;
                        }
                    }
                }
                if data.has_key {
                    parsed.insert(app_id, data);
                }
            }
        }
        self.cache = parsed;
        Ok(())
    }

    pub fn get(&mut self, app_id: u32) -> Result<&LegacyCdKeyData, TicketError> {
        if !self.cache.contains_key(&app_id) {
            self.load()?;
        }
        self.cache.get(&app_id).ok_or(TicketError::NotFound(app_id))
    }

    /// Check whether an explicit user override exists for `app_id`.
    pub fn has_override(&mut self, app_id: u32) -> bool {
        self.get(app_id)
            .map(|d| d.override_key.as_deref().is_some_and(|k| !k.is_empty()))
            .unwrap_or(false)
    }

    /// Look up the effective CD key and its source (`Override` vs `Synthesized`).
    pub fn lookup(&mut self, app_id: u32, account_id: u32) -> (String, LegacyCdKeySource) {
        if let Ok(data) = self.get(app_id) {
            if let Some(ref key) = data.override_key {
                if !key.is_empty() {
                    return (key.clone(), LegacyCdKeySource::Override);
                }
            }
        }
        (synthesize(app_id, account_id), LegacyCdKeySource::Synthesized)
    }

    /// List all stored `(app_id -> override_key)` mappings across VDF + JSON sidecar.
    pub fn list_overrides(&mut self) -> Result<BTreeMap<u32, String>, TicketError> {
        self.load()?;
        Ok(self
            .cache
            .iter()
            .filter_map(|(&aid, data)| {
                data.override_key
                    .as_ref()
                    .filter(|k| !k.is_empty())
                    .map(|k| (aid, k.clone()))
            })
            .collect())
    }

    pub fn set_override(&mut self, app_id: u32, key: String) -> Result<(), TicketError> {
        let trimmed = key.trim().to_string();
        if trimmed.is_empty() {
            return Err(TicketError::Parse(
                "Legacy CD key override cannot be empty".into(),
            ));
        }
        if self.cache.is_empty() {
            let _ = self.load();
        }
        let data = LegacyCdKeyData {
            override_key: Some(trimmed),
            has_key: true,
        };
        self._persist_override(app_id, &data)?;
        self.cache.insert(app_id, data);
        self._save_sidecar()?;
        Ok(())
    }

    pub fn remove_override(&mut self, app_id: u32) -> Result<(), TicketError> {
        if self.cache.is_empty() {
            let _ = self.load();
        }
        self.cache.remove(&app_id);
        self._remove_app_from_vdf(app_id)?;
        if self.sidecar_path().exists() || !self.cache.is_empty() {
            self._save_sidecar()?;
        }
        Ok(())
    }

    fn _persist_override(&self, app_id: u32, data: &LegacyCdKeyData) -> Result<(), TicketError> {
        fs::create_dir_all(&self.config_dir)?;
        let vdf_path = self.config_dir.join("localconfig.vdf");
        let content = if vdf_path.exists() {
            fs::read_to_string(&vdf_path)?
        } else {
            String::from("\"UserLocalConfigStore\"\n{\n    \"Software\"\n    {\n        \"Valve\"\n        {\n            \"Steam\"\n            {\n                \"apps\"\n                {\n                }\n            }\n        }\n    }\n}\n")
        };
        let new_content = Self::upsert_legacy_cd_key_block(&content, app_id, data);
        Self::write_atomic(&vdf_path, &new_content)
    }

    fn upsert_legacy_cd_key_block(content: &str, app_id: u32, data: &LegacyCdKeyData) -> String {
        let appid_str = app_id.to_string();
        let Some(ref raw_key) = data.override_key else {
            return Self::remove_legacy_cd_key_block(content, app_id);
        };
        let escaped_key = TicketStore::escape_vdf_value(raw_key);
        let new_block = format!(
            "        \"{}\"\n        {{\n            \"LegacyCDKey\" \"{}\"\n        }}\n",
            appid_str, escaped_key
        );

        let lines: Vec<&str> = content.lines().collect();
        let mut result = String::with_capacity(content.len() + 128);
        let mut in_apps = false;
        let mut seen_apps_open_brace = false;
        let mut depth = 0u32;
        let mut inserted = false;
        let mut in_target_app = false;
        let mut replaced_key_in_place = false;

        for line in &lines {
            let trimmed = line.trim();

            if !in_apps && !inserted && Self::is_apps_section_key(trimmed) {
                in_apps = true;
                seen_apps_open_brace = false;
                depth = 0;
                result.push_str(line);
                result.push('\n');
                continue;
            }

            if !in_apps {
                result.push_str(line);
                result.push('\n');
                continue;
            }

            let prev_depth = depth;
            let (opens, closes) = TicketStore::count_unquoted_braces(trimmed);
            if opens > 0 {
                seen_apps_open_brace = true;
            }
            depth = depth.saturating_add(opens).saturating_sub(closes);

            if in_target_app {
                if prev_depth >= 2 && depth == 1 {
                    if !replaced_key_in_place {
                        result.push_str(&format!(
                            "            \"LegacyCDKey\" \"{}\"\n",
                            escaped_key
                        ));
                    }
                    result.push_str(line);
                    result.push('\n');
                    in_target_app = false;
                    continue;
                }

                if prev_depth == 2 && depth == 2 {
                    if let Some((key, _)) = TicketStore::split_kv(trimmed) {
                        if key == "LegacyCDKey" {
                            let indent_len = line.len() - line.trim_start().len();
                            let indent = &line[..indent_len];
                            result.push_str(&format!(
                                "{}\"LegacyCDKey\" \"{}\"\n",
                                indent, escaped_key
                            ));
                            replaced_key_in_place = true;
                            continue;
                        }
                    }
                }

                result.push_str(line);
                result.push('\n');
                continue;
            }

            if prev_depth == 1 && depth == 1 {
                if let Some(aid) = TicketStore::extract_quoted_number(trimmed) {
                    if aid == appid_str {
                        in_target_app = true;
                        inserted = true;
                        replaced_key_in_place = false;
                        result.push_str(line);
                        result.push('\n');
                        continue;
                    }
                }
            }

            if seen_apps_open_brace && depth == 0 {
                if !inserted {
                    result.push_str(&new_block);
                    inserted = true;
                }
                result.push_str(line);
                result.push('\n');
                in_apps = false;
                seen_apps_open_brace = false;
                continue;
            }

            result.push_str(line);
            result.push('\n');
        }

        if !inserted {
            result.push_str("\"apps\"\n{\n");
            result.push_str(&new_block);
            result.push_str("}\n");
        }

        result
    }

    fn _remove_app_from_vdf(&self, app_id: u32) -> Result<(), TicketError> {
        let vdf_path = self.config_dir.join("localconfig.vdf");
        if !vdf_path.exists() {
            return Ok(());
        }
        let content = fs::read_to_string(&vdf_path)?;
        let new_content = Self::remove_legacy_cd_key_block(&content, app_id);
        Self::write_atomic(&vdf_path, &new_content)
    }

    /// Remove only the `"LegacyCDKey"` key from `<app_id>`'s VDF block, preserving all
    /// other Steam fields and tickets. Drops the `<app_id>` block itself only if it becomes empty.
    fn remove_legacy_cd_key_block(content: &str, app_id: u32) -> String {
        let appid_str = app_id.to_string();
        let lines: Vec<&str> = content.lines().collect();
        let mut result = String::with_capacity(content.len());
        let mut in_apps = false;
        let mut seen_apps_open_brace = false;
        let mut depth = 0u32;
        let mut in_target_app = false;
        let mut target_buf: Vec<&str> = Vec::new();
        let mut target_has_other_content = false;

        for line in &lines {
            let trimmed = line.trim();

            if !in_apps && Self::is_apps_section_key(trimmed) {
                in_apps = true;
                seen_apps_open_brace = false;
                depth = 0;
                result.push_str(line);
                result.push('\n');
                continue;
            }

            if !in_apps {
                result.push_str(line);
                result.push('\n');
                continue;
            }

            let prev_depth = depth;
            let (opens, closes) = TicketStore::count_unquoted_braces(trimmed);
            if opens > 0 {
                seen_apps_open_brace = true;
            }
            depth = depth.saturating_add(opens).saturating_sub(closes);

            if in_target_app {
                if prev_depth == 2 && depth == 2 {
                    if let Some((key, _)) = TicketStore::split_kv(trimmed) {
                        if key == "LegacyCDKey" {
                            continue;
                        }
                    }
                }

                target_buf.push(line);

                if prev_depth >= 2 && depth == 1 {
                    if target_has_other_content {
                        for kept in &target_buf {
                            result.push_str(kept);
                            result.push('\n');
                        }
                    }
                    target_buf.clear();
                    in_target_app = false;
                    continue;
                }

                if depth >= 2 && trimmed != "{" && !trimmed.is_empty() {
                    target_has_other_content = true;
                }
                continue;
            }

            if prev_depth == 1 && depth == 1 {
                if let Some(aid) = TicketStore::extract_quoted_number(trimmed) {
                    if aid == appid_str {
                        in_target_app = true;
                        target_has_other_content = false;
                        target_buf.clear();
                        target_buf.push(line);
                        continue;
                    }
                }
            }

            result.push_str(line);
            result.push('\n');

            if seen_apps_open_brace && depth == 0 {
                in_apps = false;
                seen_apps_open_brace = false;
            }
        }

        if in_target_app && target_has_other_content {
            for kept in &target_buf {
                result.push_str(kept);
                result.push('\n');
            }
        }

        result
    }

    pub fn list_app_ids(&mut self) -> Result<Vec<u32>, TicketError> {
        self.load()?;
        Ok(self.cache.keys().copied().collect())
    }

    pub fn resolve(
        store: &mut LegacyCdKeyStore,
        app_id: u32,
        account_id: u32,
    ) -> Result<Option<String>, LegacyCdKeyError> {
        resolve(store, app_id, account_id)
    }
}

// ============================================================================
// Crockford Base32 Encoding, Normalization & Deterministic Key Synthesis
// ============================================================================

fn crockford_decode_char(ch: char) -> Option<u8> {
    let upper = match ch.to_ascii_uppercase() {
        'O' => '0',
        'I' | 'L' => '1',
        c => c,
    };
    CROCKFORD_ALPHABET
        .iter()
        .position(|&b| b as char == upper)
        .map(|i| i as u8)
}

/// Encode arbitrary bytes into Crockford Base32 (`0-9A-HJKMNP-TV-Z`, unpadded).
pub fn encode_crockford_base32(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut buffer: u32 = 0;
    let mut bits_left: u32 = 0;

    for &b in bytes {
        buffer = (buffer << 8) | u32::from(b);
        bits_left += 8;
        while bits_left >= 5 {
            bits_left -= 5;
            let idx = ((buffer >> bits_left) & 0x1F) as usize;
            out.push(CROCKFORD_ALPHABET[idx] as char);
        }
    }
    if bits_left > 0 {
        let idx = ((buffer << (5 - bits_left)) & 0x1F) as usize;
        out.push(CROCKFORD_ALPHABET[idx] as char);
    }
    out
}

/// Decode a Crockford Base32 string (case-insensitive, tolerant of `-` / spaces and `O/I/L`).
pub fn decode_crockford_base32(encoded: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buffer: u32 = 0;
    let mut bits_left: u32 = 0;

    for ch in encoded.chars() {
        if ch == '-' || ch.is_ascii_whitespace() {
            continue;
        }
        let val = crockford_decode_char(ch)?;
        buffer = (buffer << 5) | u32::from(val);
        bits_left += 5;
        if bits_left >= 8 {
            bits_left -= 8;
            out.push(((buffer >> bits_left) & 0xFF) as u8);
        }
    }
    Some(out)
}

/// Normalize a 16-symbol Crockford Base32 CD key into canonical `XXXX-XXXX-XXXX-XXXX` form.
pub fn normalize_crockford_key(input: &str) -> Option<String> {
    let mut symbols = Vec::with_capacity(16);
    for ch in input.chars() {
        if ch == '-' || ch.is_ascii_whitespace() {
            continue;
        }
        let idx = crockford_decode_char(ch)?;
        symbols.push(CROCKFORD_ALPHABET[idx as usize] as char);
    }
    if symbols.len() != 16 {
        return None;
    }
    let mut formatted = String::with_capacity(19);
    for (i, ch) in symbols.into_iter().enumerate() {
        if i > 0 && (i % 4) == 0 {
            formatted.push('-');
        }
        formatted.push(ch);
    }
    Some(formatted)
}

/// Verify whether `key` is already in strict canonical `XXXX-XXXX-XXXX-XXXX` Crockford Base32 format.
pub fn is_valid_crockford_cdkey(key: &str) -> bool {
    let bytes = key.as_bytes();
    if bytes.len() != 19 {
        return false;
    }
    for (i, &b) in bytes.iter().enumerate() {
        if i == 4 || i == 9 || i == 14 {
            if b != b'-' {
                return false;
            }
        } else if !CROCKFORD_ALPHABET.contains(&b) {
            return false;
        }
    }
    true
}

/// Synthesize a deterministic legacy CD key from appId and accountId.
fn fnv1a_hash32(input: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in input.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

pub fn synthesize(app_id: u32, account_id: u32) -> String {
    let seed_a = format!("{}:{}", app_id, account_id);
    let seed_b = format!("OST:{}:{}", account_id, app_id);

    let mut bits = (u64::from(fnv1a_hash32(&seed_a)) << 32) | u64::from(fnv1a_hash32(&seed_b));

    let mut key = String::with_capacity(19);
    for i in 0..16 {
        if i > 0 && (i % 4) == 0 {
            key.push('-');
        }
        let idx = (bits & 31) as usize;
        key.push(CROCKFORD_ALPHABET[idx] as char);
        bits = bits.rotate_right(4);
    }
    key
}

/// Resolve the legacy CD key for a managed app.
pub fn resolve(
    store: &mut LegacyCdKeyStore,
    app_id: u32,
    account_id: u32,
) -> Result<Option<String>, LegacyCdKeyError> {
    let (key, _source) = store.lookup(app_id, account_id);
    Ok(Some(key))
}

/// Generate a deterministic legacy CD key given app_id and a 64-bit Steam ID.
pub fn generate_legacy_cdkey(app_id: u32, steam_id: u64) -> String {
    let account_id = (steam_id & 0xFFFF_FFFF) as u32;
    synthesize(app_id, account_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steam_bridge::ticket_store::AppTicketData;

    #[test]
    fn test_synthesize_deterministic_key() {
        let key1 = synthesize(1245620, 28121353);
        let key2 = synthesize(1245620, 28121353);
        assert_eq!(key1, key2);
        assert!(key1.contains('-'));
        assert_eq!(key1.len(), 19);
        assert!(is_valid_crockford_cdkey(&key1));
    }

    #[test]
    fn test_synthesize_different_accounts() {
        let key1 = synthesize(1245620, 28121353);
        let key2 = synthesize(1245620, 28121354);
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_set_and_get_override() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path();

        let mut store = LegacyCdKeyStore::new(store_path);
        store.load().unwrap();

        assert!(store.set_override(1245620, "   ".into()).is_err());
        store.set_override(1245620, "USERKEY1234".into()).unwrap();

        let data = store.get(1245620).unwrap();
        assert!(data.override_key.is_some());
        assert_eq!(data.override_key.as_deref(), Some("USERKEY1234"));

        let mut store2 = LegacyCdKeyStore::new(store_path);
        store2.load().unwrap();
        let result = LegacyCdKeyStore::resolve(&mut store2, 1245620, 28121353)
            .unwrap()
            .unwrap();
        assert_eq!(result, "USERKEY1234");
    }

    #[test]
    fn test_resolve_no_override_synthesizes_key() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path();

        let mut store = LegacyCdKeyStore::new(store_path);
        store.load().unwrap();

        let result = LegacyCdKeyStore::resolve(&mut store, 1245620, 28121353)
            .unwrap()
            .unwrap();
        assert!(result.contains('-'));
        assert_eq!(result.len(), 19);
    }

    #[test]
    fn test_remove_override() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path();

        let mut store = LegacyCdKeyStore::new(store_path);
        store.load().unwrap();

        store.set_override(1245620, "TEMPKEY".into()).unwrap();
        assert!(store.get(1245620).unwrap().override_key.is_some());

        store.remove_override(1245620).unwrap();
        assert!(store.get(1245620).is_err());

        let mut store2 = LegacyCdKeyStore::new(store_path);
        store2.load().unwrap();
        let result = LegacyCdKeyStore::resolve(&mut store2, 1245620, 28121353)
            .unwrap()
            .unwrap();
        assert!(result.contains('-'));
        assert_eq!(result.len(), 19);
    }

    #[test]
    fn test_set_and_remove_override_preserves_tickets_and_steam_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path();

        // Write an AppTicket first
        let mut tickets = TicketStore::new(store_path);
        tickets
            .set(AppTicketData {
                app_id: 1245620,
                steam_id: Some("76561198028121353".into()),
                app_ticket: Some("deadbeef".into()),
                e_ticket: Some("cafebabe".into()),
                last_update: None,
            })
            .unwrap();

        let mut cdkeys = LegacyCdKeyStore::new(store_path);
        // Apps without LegacyCDKey must NOT appear in list_app_ids
        assert!(cdkeys.list_app_ids().unwrap().is_empty());

        cdkeys
            .set_override(1245620, "KEY-1111-2222-3333".into())
            .unwrap();
        assert_eq!(cdkeys.list_app_ids().unwrap(), vec![1245620]);

        // Ticket fields must still be intact!
        let mut tickets2 = TicketStore::new(store_path);
        let t = tickets2.get(1245620).unwrap();
        assert_eq!(t.app_ticket.as_deref(), Some("deadbeef"));
        assert_eq!(t.e_ticket.as_deref(), Some("cafebabe"));

        // Removing the LegacyCDKey must NOT delete the app's TicketStore block!
        cdkeys.remove_override(1245620).unwrap();
        assert!(cdkeys.list_app_ids().unwrap().is_empty());

        let mut tickets3 = TicketStore::new(store_path);
        let t3 = tickets3.get(1245620).unwrap();
        assert_eq!(t3.app_ticket.as_deref(), Some("deadbeef"));
        assert_eq!(t3.e_ticket.as_deref(), Some("cafebabe"));
    }

    #[test]
    fn test_roundtrip_with_localconfig_vdf() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path();

        let mut store = LegacyCdKeyStore::new(store_path);
        store.load().unwrap();

        store.set_override(1245620, "ROUNDTRIPKEY".into()).unwrap();
        let data = store.get(1245620).unwrap();
        assert_eq!(data.override_key.as_deref(), Some("ROUNDTRIPKEY"));

        store.remove_override(1245620).unwrap();

        let mut store2 = LegacyCdKeyStore::new(store_path);
        store2.load().unwrap();
        assert!(store2.get(1245620).is_err());
    }

    #[test]
    fn test_resolves_active_userdata_config_dir_and_sidecar_0600_survives_vdf_rewrite() {
        let tmp = tempfile::tempdir().unwrap();
        let steam_root = tmp.path().join("Steam");
        let cfg_dir = steam_root.join("config");
        let acc_id = 424242u32;
        let steam64 = 76_561_197_960_265_728u64 + u64::from(acc_id);
        let user_cfg = steam_root
            .join("userdata")
            .join(acc_id.to_string())
            .join("config");

        fs::create_dir_all(&cfg_dir).unwrap();
        fs::create_dir_all(&user_cfg).unwrap();
        fs::write(
            cfg_dir.join("loginusers.vdf"),
            format!(
                "\"users\"\n{{\n    \"{}\"\n    {{\n        \"MostRecent\" \"1\"\n    }}\n}}\n",
                steam64
            ),
        )
        .unwrap();
        fs::write(
            user_cfg.join("localconfig.vdf"),
            "\"UserLocalConfigStore\"\n{\n    \"apps\"\n    {\n    }\n}\n",
        )
        .unwrap();

        // Both `<steam_root>` and `<steam_root>/config` must resolve to active userdata/<id>/config
        let mut store = LegacyCdKeyStore::new(&cfg_dir);
        assert_eq!(store.config_dir(), user_cfg.as_path());
        assert_eq!(
            LegacyCdKeyStore::new_for_steam_root(&steam_root).config_dir(),
            user_cfg.as_path()
        );

        store
            .set_override(1245620, "ABCD-EFGH-JKMN-PQRS".into())
            .unwrap();

        // Sidecar must exist with 0o600 permissions inside active userdata/<id>/config
        let sidecar = store.sidecar_path();
        assert_eq!(sidecar, user_cfg.join(LEGACY_CDKEYS_SIDECAR_FILENAME));
        let mode = fs::metadata(&sidecar).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        // Simulate Steam rewriting localconfig.vdf and stripping unknown `"LegacyCDKey"` keys
        fs::write(
            user_cfg.join("localconfig.vdf"),
            "\"UserLocalConfigStore\"\n{\n    \"apps\"\n    {\n    }\n}\n",
        )
        .unwrap();

        // Reload must still recover the override from the 0o600 JSON sidecar
        let mut reloaded = LegacyCdKeyStore::new(&cfg_dir);
        let (key, source) = reloaded.lookup(1245620, acc_id);
        assert_eq!(source, LegacyCdKeySource::Override);
        assert_eq!(key, "ABCD-EFGH-JKMN-PQRS");
        assert!(reloaded.has_override(1245620));
        assert_eq!(
            reloaded.list_overrides().unwrap(),
            BTreeMap::from([(1245620, "ABCD-EFGH-JKMN-PQRS".to_string())])
        );
    }

    #[test]
    fn test_crockford_base32_roundtrip_and_normalization() {
        let raw = b"SteamLegacyKey!";
        let encoded = encode_crockford_base32(raw);
        assert!(!encoded.contains('I'));
        assert!(!encoded.contains('L'));
        assert!(!encoded.contains('O'));
        assert!(!encoded.contains('U'));
        let decoded = decode_crockford_base32(&encoded).unwrap();
        assert_eq!(decoded, raw);

        // Synthesized keys must be valid canonical Crockford Base32 and idempotent under normalization
        let synth = synthesize(480, 12345678);
        assert!(is_valid_crockford_cdkey(&synth));
        assert_eq!(normalize_crockford_key(&synth).as_deref(), Some(synth.as_str()));

        // Ambiguous Crockford input (`o` -> `0`, `i`/`l` -> `1`) normalizes cleanly
        assert_eq!(
            normalize_crockford_key("oil0 abcd efgh jkmn").as_deref(),
            Some("0110-ABCD-EFGH-JKMN")
        );
        assert!(normalize_crockford_key("INVALID-U-CHAR-1234").is_none());
        assert!(!is_valid_crockford_cdkey("0110-ABCD-EFGH-UUUU"));
    }

    #[test]
    fn test_lookup_distinguishes_override_and_synthesized() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = LegacyCdKeyStore::new(tmp.path());

        let (synth_key, synth_src) = store.lookup(730, 555);
        assert_eq!(synth_src, LegacyCdKeySource::Synthesized);
        assert!(is_valid_crockford_cdkey(&synth_key));
        assert!(!store.has_override(730));

        store
            .set_override(730, "9999-8888-7777-6666".into())
            .unwrap();
        let (ov_key, ov_src) = store.lookup(730, 555);
        assert_eq!(ov_src, LegacyCdKeySource::Override);
        assert_eq!(ov_key, "9999-8888-7777-6666");
        assert!(store.has_override(730));
    }
}
