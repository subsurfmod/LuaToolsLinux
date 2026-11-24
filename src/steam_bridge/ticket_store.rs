//! Ticket store for the Steam client on Linux.
//!
//! On Windows the integration reads/writes `AppTicket` and `ETicket` from
//! `HKCU\Software\Valve\Steam\Apps\<AppId>`. On Linux, Steam stores the
//! equivalent data in `localconfig.vdf` under the Steam config directory.
//! This module reads/writes those values so ticket-based features work on Linux.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Error type for ticket store operations.
#[derive(Debug, thiserror::Error)]
pub enum TicketError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("App {0} not found in ticket store")]
    NotFound(u32),
    #[error("Parse error: {0}")]
    Parse(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppTicketData {
    pub app_id: u32,
    pub steam_id: Option<String>,
    pub app_ticket: Option<String>,
    pub e_ticket: Option<String>,
    pub last_update: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct TicketStore {
    config_dir: PathBuf,
    cache: BTreeMap<u32, AppTicketData>,
    async_calls: BTreeMap<u64, u32>,
}

impl TicketStore {
    pub fn global() -> &'static Mutex<TicketStore> {
        static GLOBAL_TICKET_STORE: OnceLock<Mutex<TicketStore>> = OnceLock::new();
        GLOBAL_TICKET_STORE.get_or_init(|| Mutex::new(TicketStore::default()))
    }

    pub fn new(config_dir: &Path) -> Self {
        Self {
            config_dir: config_dir.to_path_buf(),
            cache: BTreeMap::new(),
            async_calls: BTreeMap::new(),
        }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
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
        let content = fs::read_to_string(path)?;
        let app_blocks = Self::extract_app_blocks(&content);
        for (appid_str, block) in app_blocks {
            if let Ok(app_id) = appid_str.parse::<u32>() {
                let mut data = AppTicketData {
                    app_id,
                    ..Default::default()
                };
                for line in block.lines() {
                    let line = line.trim();
                    if let Some((key, val)) = Self::split_kv(line) {
                        if val.is_empty() {
                            continue;
                        }
                        match key.as_str() {
                            "SteamID" => data.steam_id = Some(val),
                            "AppTicket" => data.app_ticket = Some(val),
                            "ETicket" => data.e_ticket = Some(val),
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

    pub fn get(&mut self, app_id: u32) -> Result<AppTicketData, TicketError> {
        if let Some(data) = self.cache.get(&app_id) {
            return Ok(data.clone());
        }
        self.load()?;
        self.cache
            .get(&app_id)
            .cloned()
            .ok_or(TicketError::NotFound(app_id))
    }

    pub fn set(&mut self, data: AppTicketData) -> Result<(), TicketError> {
        self.cache.insert(data.app_id, data.clone());
        self._persist_app(data)
    }

    pub fn remove(&mut self, app_id: u32) -> Result<(), TicketError> {
        self.cache.remove(&app_id);
        self._remove_app_from_vdf(app_id)
    }

    pub fn has_app_ticket(&mut self, app_id: u32) -> bool {
        self.get(app_id)
            .map(|d| d.app_ticket.is_some())
            .unwrap_or(false)
    }

    pub fn has_e_ticket(&mut self, app_id: u32) -> bool {
        self.get(app_id)
            .map(|d| d.e_ticket.is_some())
            .unwrap_or(false)
    }

    fn _persist_app(&self, data: AppTicketData) -> Result<(), TicketError> {
        fs::create_dir_all(&self.config_dir)?;
        let vdf_path = self.config_dir.join("localconfig.vdf");
        let content = if vdf_path.exists() {
            fs::read_to_string(&vdf_path)?
        } else {
            String::from("\"UserLocalConfigStore\"\n{\n    \"Software\"\n    {\n        \"Valve\"\n        {\n            \"Steam\"\n            {\n                \"apps\"\n                {\n                }\n            }\n        }\n    }\n}\n")
        };
        let new_content = Self::upsert_app_block(&content, data.app_id, &data);
        fs::write(&vdf_path, new_content)?;
        Ok(())
    }

    fn _remove_app_from_vdf(&self, app_id: u32) -> Result<(), TicketError> {
        let vdf_path = self.config_dir.join("localconfig.vdf");
        if !vdf_path.exists() {
            return Ok(());
        }
        let content = fs::read_to_string(&vdf_path)?;
        let new_content = Self::remove_app_block(&content, app_id);
        fs::write(&vdf_path, new_content)?;
        Ok(())
    }

    fn upsert_app_block(content: &str, app_id: u32, data: &AppTicketData) -> String {
        let appid_str = app_id.to_string();
        let block = format!(
            "        \"{}\"\n        {{\n            \"appid\" \"{}\"\n            \"SteamID\" \"{}\"\n            \"AppTicket\" \"{}\"\n            \"ETicket\" \"{}\"\n        }}\n",
            appid_str,
            appid_str,
            data.steam_id.as_deref().unwrap_or(""),
            data.app_ticket.as_deref().unwrap_or(""),
            data.e_ticket.as_deref().unwrap_or(""),
        );

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
                // Keep skipping until we leave the target app's `{ ... }` block back to depth == 1
                if prev_depth >= 2 && depth == 1 {
                    skipping_target = false;
                }
                continue;
            }

            if prev_depth == 1 && depth == 1 {
                if let Some(aid) = Self::extract_quoted_number(trimmed) {
                    if aid == appid_str {
                        // Replace existing app block in place
                        result.push_str(&block);
                        inserted = true;
                        skipping_target = true;
                        continue;
                    }
                }
            }

            // Closing brace of `"apps"` — insert new app block BEFORE writing `}`
            if seen_apps_open_brace && depth == 0 {
                if !inserted {
                    result.push_str(&block);
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
            result.push_str(&block);
            result.push_str("}\n");
        }

        result
    }

    fn remove_app_block(content: &str, app_id: u32) -> String {
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

    /// Record an async call handle for encrypted ticket tracking.
    pub fn record_async_call(&mut self, async_call: u64, app_id: u32) {
        self.async_calls.insert(async_call, app_id);
    }

    /// Consume an async call handle and return the associated app_id.
    pub fn take_async_call(&mut self, async_call: u64) -> Option<u32> {
        self.async_calls.remove(&async_call)
    }

    /// Get encrypted ticket (ETicket) bytes for an app.
    pub fn get_encrypted_ticket(&mut self, app_id: u32) -> Option<Vec<u8>> {
        self.load().ok()?;
        self.cache
            .get(&app_id)
            .and_then(|d| d.e_ticket.as_ref())
            .and_then(|s| hex::decode(s).ok())
    }

    /// Get spoofed SteamID for an app (returns 0 when unset or invalid).
    pub fn get_spoof_steam_id(&mut self, app_id: u32) -> u64 {
        if self.load().is_err() {
            return 0;
        }
        self.cache
            .get(&app_id)
            .and_then(|d| d.steam_id.as_ref())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
    }
}

impl Default for TicketStore {
    fn default() -> Self {
        Self::new(&Self::default_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn extract_app_blocks_parses_simple_vdf() {
        let vdf = r#""apps"
        {
            "1245620"
            {
                "appid" "1245620"
                "SteamID" "76561198028121353"
                "AppTicket" "deadbeef"
                "ETicket" "cafebabe"
            }
            "2778580"
            {
                "appid" "2778580"
                "AppTicket" "0000"
            }
        }"#;
        let blocks = TicketStore::extract_app_blocks(vdf);
        assert_eq!(blocks.len(), 2);
        let map: BTreeMap<String, String> = blocks.into_iter().collect();
        assert!(map.get("1245620").unwrap().contains("76561198028121353"));
        assert!(map.get("1245620").unwrap().contains("deadbeef"));
        assert!(map.get("2778580").unwrap().contains("0000"));
    }

    #[test]
    fn split_kv_extracts_real_quoted_values() {
        assert_eq!(
            TicketStore::split_kv(r#""SteamID" "76561198028121353""#),
            Some(("SteamID".into(), "76561198028121353".into()))
        );
        assert_eq!(
            TicketStore::split_kv("\"AppTicket\"\t\t\"deadbeef\""),
            Some(("AppTicket".into(), "deadbeef".into()))
        );
        assert_eq!(
            TicketStore::split_kv("\"ETicket\" \"\""),
            Some(("ETicket".into(), "".into()))
        );
    }

    #[test]
    fn set_update_load_and_remove_roundtrip_preserves_vdf_structure() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = TicketStore::new(tmp.path());

        store
            .set(AppTicketData {
                app_id: 1245620,
                steam_id: Some("76561198028121353".into()),
                app_ticket: Some("deadbeef".into()),
                e_ticket: Some("cafebabe".into()),
                last_update: None,
            })
            .unwrap();

        store
            .set(AppTicketData {
                app_id: 2778580,
                steam_id: Some("76561198028121353".into()),
                app_ticket: Some("11223344".into()),
                e_ticket: None,
                last_update: None,
            })
            .unwrap();

        // Update existing app 1245620 — must NOT delete it!
        store
            .set(AppTicketData {
                app_id: 1245620,
                steam_id: Some("76561198028121353".into()),
                app_ticket: Some("feedface".into()),
                e_ticket: Some("01020304".into()),
                last_update: None,
            })
            .unwrap();

        let mut reloaded = TicketStore::new(tmp.path());
        reloaded.load().unwrap();
        let d1 = reloaded.get(1245620).unwrap();
        assert_eq!(d1.steam_id.as_deref(), Some("76561198028121353"));
        assert_eq!(d1.app_ticket.as_deref(), Some("feedface"));
        assert_eq!(d1.e_ticket.as_deref(), Some("01020304"));

        let d2 = reloaded.get(2778580).unwrap();
        assert_eq!(d2.app_ticket.as_deref(), Some("11223344"));

        reloaded.remove(1245620).unwrap();
        let mut reloaded2 = TicketStore::new(tmp.path());
        reloaded2.load().unwrap();
        assert!(reloaded2.get(1245620).is_err());
        assert_eq!(
            reloaded2.get(2778580).unwrap().app_ticket.as_deref(),
            Some("11223344")
        );
    }

    #[test]
    fn ignores_non_apps_keys_containing_apps_substring() {
        let vdf = r#""UserLocalConfigStore"
{
    "steamapps" "C:/Steam/steamapps"
    "apps"
    {
        "480"
        {
            "SteamID" "76561198000000001"
        }
    }
}
"#;
        let blocks = TicketStore::extract_app_blocks(vdf);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].0, "480");
    }
}
