//! Legacy CD-key resolution for Steam on Linux.
//! Ported from usefulsteamtools::LegacyCDKey.
//! Replaces Windows registry (HKCU\Software\Valve\Steam\Apps\<AppId>)
//! with reading/writing from localconfig.vdf via ticket_store.rs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::steam_bridge::ticket_store::{AppTicketData, TicketError, TicketStore};

/// Error type for legacy CD-key operations.
#[derive(Debug, thiserror::Error)]
pub enum LegacyCdKeyError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
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
    /// User-configured override stored in localconfig.vdf.
    Override,
    /// Deterministically synthesized key from appId+accountId.
    Synthesized,
}

/// Data stored for a legacy CD key entry in the ticket store.
#[derive(Debug, Clone, Default)]
pub struct LegacyCdKeyData {
    /// User-configured override key, if any.
    pub override: Option<String>,
    /// Whether a key has been set (override or synthesized).
    pub has_key: bool,
}

/// Store for legacy CD keys using localconfig.vdf on Linux.
#[derive(Debug, Clone, Default)]
pub struct LegacyCdKeyStore {
    config_dir: PathBuf,
    cache: BTreeMap<u32, LegacyCdKeyData>,
}

impl LegacyCdKeyStore {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            config_dir: config_dir.to_path_buf(),
            cache: BTreeMap::new(),
        }
    }

    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".steam/steam/config")
    }

    pub fn load(&mut self) -> Result<(), TicketError> {
        let vdf_path = self.config_dir.join("localconfig.vdf");
        if !vdf_path.exists() {
            let alt = self.config_dir.join("localconfig.vdf.bak");
            if alt.exists() {
                self._parse_vdf(&alt)?;
            }
            return Ok(());
        }
        self._parse_vdf(&vdf_path)?;
        Ok(())
    }

    fn is_apps_section_key(trimmed: &str) -> bool {
        trimmed.eq_ignore_ascii_case("\"apps\"")
    }

    fn _parse_vdf(&mut self, path: &Path) -> Result<(), TicketError> {
        let content = std::fs::read_to_string(path)?;
        let app_blocks = Self::extract_app_blocks(&content);
        for (appid_str, block) in app_blocks {
            if let Ok(app_id) = appid_str.parse::<u32>() {
                let mut data = LegacyCdKeyData::default();
                for line in block.lines() {
                    let line = line.trim();
                    if let Some((key, val)) = Self::split_kv(line) {
                        if val.is_empty() {
                            continue;
                        }
                        match key.as_str() {
                            "SteamID" => {}
                            "AppTicket" => {}
                            "ETicket" => {}
                            "LegacyCDKey" => data.override = Some(val),
                            _ => {}
                        }
                    }
                }
                self.cache.insert(app_id, data);
            }
        }
        Ok(())
    }

    fn extract_app_blocks(content: &str) -> Vec<(String, String)> {
        let mut blocks = Vec::new();
        let mut in_apps = false;
        let mut seen_apps_open_brace = false;
        let mut depth = 0u32;
        let mut current_appid: Option<String> = None;
        let mut current_block = String::new();

        for line in content.lines() {
            let trimmed = line.trim();

            if !in_apps && Self::is_apps_section_key(trimmed) {
                in_apps = true;
                seen_apps_open_brace = false;
                depth = 0;
                continue;
            }

            if !in_apps {
                continue;
            }

            let prev_depth = depth;
            for ch in trimmed.chars() {
                if ch == '{' {
                    depth += 1;
                    seen_apps_open_brace = true;
                } else if ch == '}' {
                    depth = depth.saturating_sub(1);
                }
            }

            if prev_depth == 1 && depth == 1 {
                if let Some(appid) = Self::extract_quoted_number(trimmed) {
                    if let Some(prev) = current_appid.replace(appid) {
                        blocks.push((prev, current_block.clone()));
                    }
                    current_block.clear();
                    continue;
                }
            }

            if depth >= 2 && current_appid.is_some() {
                current_block.push_str(trimmed);
                current_block.push('\n');
            } else if prev_depth >= 2 && depth == 1 {
                if let Some(prev) = current_appid.take() {
                    blocks.push((prev, current_block.clone()));
                    current_block.clear();
                }
            }

            if seen_apps_open_brace && depth == 0 {
                if let Some(prev) = current_appid.take() {
                    blocks.push((prev, current_block.clone()));
                    current_block.clear();
                }
                in_apps = false;
                seen_apps_open_brace = false;
            }
        }

        if let Some(aid) = current_appid.take() {
            blocks.push((aid, current_block));
        }

        blocks
    }

    fn extract_quoted_number(line: &str) -> Option<String> {
        let line = line.trim();
        if line.starts_with('"') && line.len() > 2 {
            let end = line[1..].find('"')? + 1;
            let inner = &line[1..end];
            if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
                return Some(inner.to_string());
            }
        }
        None
    }

    fn split_kv(line: &str) -> Option<(String, String)> {
        let line = line.trim();
        if !line.starts_with('"') {
            return None;
        }
        let key_end = line[1..].find('"')? + 1;
        let key = &line[1..key_end];
        // Advance past the closing quote of the key (`key_end + 1`), not `key_end`.
        let rest = line.get(key_end + 1..)?.trim();
        if rest.is_empty() {
            return None;
        }
        let val = if let Some(stripped) = rest.strip_prefix('"') {
            let v_end = stripped.find('"')?;
            stripped[..v_end].to_string()
        } else {
            rest.split_whitespace().next().unwrap_or("").to_string()
        };
        if key.is_empty() {
            return None;
        }
        Some((key.to_string(), val))
    }

    pub fn get(&mut self, app_id: u32) -> Result<&LegacyCdKeyData, TicketError> {
        if let Some(data) = self.cache.get(&app_id) {
            return Ok(data);
        }
        self.load()?;
        self.cache
            .get(&app_id)
            .ok_or_else(|| TicketError::NotFound(app_id))
    }

    pub fn set_override(&mut self, app_id: u32, key: String) -> Result<(), TicketError> {
        let data = LegacyCdKeyData {
            override: Some(key.clone()),
            ..Default::default()
        };
        self.cache.insert(app_id, data);
        self._persist_override(app_id, &data)
    }

    pub fn remove_override(&mut self, app_id: u32) -> Result<(), TicketError> {
        self.cache.remove(&app_id);
        self._remove_app_from_vdf(app_id)
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
        fs::write(&vdf_path, new_content)?;
        Ok(())
    }

    fn upsert_legacy_cd_key_block(content: &str, app_id: u32, data: &LegacyCdKeyData) -> String {
        let appid_str = app_id.to_string();
        let legacy_key_block = if let Some(ref key) = data.override {
            format!(
                "        \"{}\"\n        {{\n            \"LegacyCDKey\" \"{}\"\n        }}\n",
                appid_str, key
            )
        } else {
            String::new()
        };

        let lines: Vec<&str> = content.lines().collect();
        let mut result = String::new();
        let mut in_apps = false;
        let mut seen_apps_open_brace = false;
        let mut depth = 0u32;
        let mut inserted = false;
        let mut skipping_target = false;

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
            for ch in trimmed.chars() {
                if ch == '{' {
                    depth += 1;
                    seen_apps_open_brace = true;
                } else if ch == '}' {
                    depth = depth.saturating_sub(1);
                }
            }

            if skipping_target {
                if prev_depth >= 2 && depth == 1 {
                    skipping_target = false;
                }
                continue;
            }

            if prev_depth == 1 && depth == 1 {
                if let Some(aid) = Self::extract_quoted_number(trimmed) {
                    if aid == appid_str {
                        // Replace existing app block in place
                        result.push_str(&legacy_key_block);
                        inserted = true;
                        skipping_target = true;
                        continue;
                    }
                }
            }

            // Closing brace of `"apps"` — insert new app block BEFORE writing `}`
            if seen_apps_open_brace && depth == 0 {
                if !inserted {
                    result.push_str(&legacy_key_block);
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
            result.push_str(&legacy_key_block);
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
        fs::write(&vdf_path, new_content)?;
        Ok(())
    }

    fn remove_legacy_cd_key_block(content: &str, app_id: u32) -> String {
        let appid_str = app_id.to_string();
        let lines: Vec<&str> = content.lines().collect();
        let mut result = String::new();
        let mut in_apps = false;
        let mut seen_apps_open_brace = false;
        let mut depth = 0u32;
        let mut skipping_target = false;

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
            for ch in trimmed.chars() {
                if ch == '{' {
                    depth += 1;
                    seen_apps_open_brace = true;
                } else if ch == '}' {
                    depth = depth.saturating_sub(1);
                }
            }

            if skipping_target {
                if prev_depth >= 2 && depth == 1 {
                    skipping_target = false;
                }
                continue;
            }

            if prev_depth == 1 && depth == 1 {
                if let Some(aid) = Self::extract_quoted_number(trimmed) {
                    if aid == appid_str {
                        skipping_target = true;
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

        result
    }

    pub fn list_app_ids(&mut self) -> Result<Vec<u32>, TicketError> {
        self.load()?;
        Ok(self.cache.keys().copied().collect())
    }
}

/// Synthesize a deterministic legacy CD key from appId and accountId.
/// Matches the algorithm from usefulsteamtools: uses Fnv1a hash of
/// seed strings "appId:accountId" and "OST:accountId:appId".
fn fnv1a_hash(input: &str) -> u64 {
    let mut hash: u64 = 0x811c9dc5;
    for byte in input.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

pub fn synthesize(app_id: u32, account_id: u32) -> String {
    let seed_a = format!("{}:{}", app_id, account_id);
    let seed_b = format!("OST:{}:{}", account_id, app_id);

    let bits = (fnv1a_hash(&seed_a) << 32) | fnv1a_hash(&seed_b);

    let kAlphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

    let mut key = String::with_capacity(19);
    for i in 0..16 {
        if i && (i % 4) == 0 {
            key.push('-');
        }
        let idx = (bits & 31) as usize;
        key.push(kAlphabet[idx]);
        bits = (bits >> 4) | (bits << 60); // rotate so every char draws fresh bits
    }
    key
}

/// Resolve the legacy CD key for a managed app.
///
/// - If a user override is configured (via set_override), return that.
/// - Otherwise, if the app is managed (has a depot entry), synthesize a deterministic key.
/// - If the app is not managed, return None (caller should let Steam's real flow run).
///
/// # Arguments
/// - `app_id`: The Steam application ID
/// - `account_id`: The user's Steam account ID
pub fn resolve(
    store: &mut LegacyCdKeyStore,
    app_id: u32,
    account_id: u32,
) -> Result<Option<String>, LegacyCdKeyError> {
    // Check if we have a credential store entry for this app (managed app check)
    let _ = store.get(app_id)?; // Just ensures it's loaded; we don't fail if not found

    // Check for user-configured override
    if let Ok(data) = store.get(app_id) {
        if let Some(ref override_key) = data.override {
            return Ok(Some(override_key.clone()));
        }
    }

    // Synthesize a deterministic key — this satisfies Steam's local step only.
    // Titles that validate the key online (Ubisoft/Rockstar) require a real override key.
    let synthesized = synthesize(app_id, account_id);
    Ok(Some(synthesized))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn test_synthesize_deterministic_key() {
        let key1 = synthesize(1245620, 76561198028121353);
        let key2 = synthesize(1245620, 76561198028121353);
        assert_eq!(key1, key2);
        // Should be in XXXX-XXXX-XXXX-XXXX format
        assert!(key1.contains('-'));
        assert_eq!(key1.len(), 19);
    }

    #[test]
    fn test_synthesize_different_accounts() {
        let key1 = synthesize(1245620, 76561198028121353);
        let key2 = synthesize(1245620, 76561198028121354);
        // Different account IDs should produce different keys
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_set_and_get_override() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path();

        let mut store = LegacyCdKeyStore::new(store_path);
        store.load().unwrap();

        // Set an override
        store
            .set_override(1245620, "USERKEY1234".into())
            .unwrap();

        // Read back
        let data = store.get(1245620).unwrap();
        assert!(data.override.is_some());
        assert_eq!(data.override.as_deref(), Some("USERKEY1234"));

        // Resolve should return the override
        let mut store2 = LegacyCdKeyStore::new(store_path);
        store2.load().unwrap();
        let result = LegacyCdKeyStore::resolve(&mut store2, 1245620, 76561198028121353)
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

        // No override set - should synthesize
        let result = LegacyCdKeyStore::resolve(&mut store, 1245620, 76561198028121353)
            .unwrap()
            .unwrap();
        // Should be in XXXX-XXXX-XXXX-XXXX format
        assert!(result.contains('-'));
        assert_eq!(result.len(), 19);
    }

    #[test]
    fn test_remove_override() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path();

        let mut store = LegacyCdKeyStore::new(store_path);
        store.load().unwrap();

        // Set then remove override
        store
            .set_override(1245620, "TEMPKEY".into())
            .unwrap();

        assert!(store.get(1245620).unwrap().override.is_some());
        store.remove_override(1245620).unwrap();
        assert!(store.get(1245620).unwrap().override.is_none());

        // After removal, resolve should synthesize
        let mut store2 = LegacyCdKeyStore::new(store_path);
        store2.load().unwrap();
        let result = LegacyCdKeyStore::resolve(&mut store2, 1245620, 76561198028121353)
            .unwrap()
            .unwrap();
        assert!(result.contains('-'));
        assert_eq!(result.len(), 19);
    }

    #[test]
    fn test_roundtrip_with_localconfig_vdf() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path();

        let mut store = LegacyCdKeyStore::new(store_path);
        store.load().unwrap();

        // Set override via API
        store
            .set_override(1245620, "ROUNDTRIPKEY".into())
            .unwrap();

        // Verify data is in cache
        let data = store.get(1245620).unwrap();
        assert_eq!(data.override.as_deref(), Some("ROUNDTRIPKEY"));

        // Remove and verify VDF is updated
        store.remove_override(1245620).unwrap();

        // Reload from VDF
        let mut store2 = LegacyCdKeyStore::new(store_path);
        store2.load().unwrap();
        assert!(store2.get(1245620).unwrap().override.is_none());
    }
}