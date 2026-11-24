//! Legacy CD-key resolution for Steam on Linux.
//! Ported from usefulsteamtools::LegacyCDKey.
//! Replaces Windows registry (HKCU\Software\Valve\Steam\Apps\<AppId>)
//! with reading/writing from localconfig.vdf via ticket_store.rs.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::steam_bridge::ticket_store::{TicketError, TicketStore};

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
    /// User-configured override stored in localconfig.vdf.
    Override,
    /// Deterministically synthesized key from appId+accountId.
    Synthesized,
}

/// Data stored for a legacy CD key entry in the ticket store.
#[derive(Debug, Clone, Default)]
pub struct LegacyCdKeyData {
    /// User-configured override key, if any.
    pub override_key: Option<String>,
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

    fn write_atomic(path: &Path, content: &str) -> Result<(), TicketError> {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("localconfig.vdf");
        let tmp_path =
            path.with_file_name(format!(".{}.cdkey.tmp.{}", file_name, std::process::id()));
        fs::write(&tmp_path, content)?;
        fs::rename(&tmp_path, path)?;
        Ok(())
    }

    fn _parse_vdf(&mut self, path: &Path) -> Result<(), TicketError> {
        let content = std::fs::read_to_string(path)?;
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

    pub fn set_override(&mut self, app_id: u32, key: String) -> Result<(), TicketError> {
        let trimmed = key.trim().to_string();
        if trimmed.is_empty() {
            return Err(TicketError::Parse(
                "Legacy CD key override cannot be empty".into(),
            ));
        }
        let data = LegacyCdKeyData {
            override_key: Some(trimmed),
            has_key: true,
        };
        self._persist_override(app_id, &data)?;
        self.cache.insert(app_id, data);
        Ok(())
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
                // Closing brace of the target app block: append `"LegacyCDKey"` if not already present.
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

                // Update `"LegacyCDKey"` in place while preserving all other Steam/ticket keys.
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

            // Closing brace of `"apps"` — insert new app block BEFORE writing `}`
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
                    // End of target app block
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

    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

    let mut key = String::with_capacity(19);
    for i in 0..16 {
        if i > 0 && (i % 4) == 0 {
            key.push('-');
        }
        let idx = (bits & 31) as usize;
        key.push(ALPHABET[idx] as char);
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
    if let Ok(data) = store.get(app_id) {
        if let Some(ref override_key) = data.override_key {
            return Ok(Some(override_key.clone()));
        }
    }

    let synthesized = synthesize(app_id, account_id);
    Ok(Some(synthesized))
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
}
