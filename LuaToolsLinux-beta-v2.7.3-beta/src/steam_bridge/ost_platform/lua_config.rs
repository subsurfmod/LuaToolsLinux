//! LuaConfig implementation for Linux.
//!
//! Port of usefulsteamtools `LuaConfig.cpp` + `LuaFileWatcher.cpp` + `ConfigFileWatcher.cpp`.
//! Parses `.lua` config files from `stplug-in/` directory, provides depot/appid mapping,
//! decryption keys, access tokens, manifest overrides, and file watching.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::steam_bridge::ost_platform::directory_watch::{Watch, wait_any, ChangeAction};
use crate::steam_bridge::ticket_store::TicketStore;

// ============================================================================
// LuaConfig data structures
// ============================================================================

#[derive(Debug, Clone, Default)]
pub struct LuaConfigEntry {
    pub id: u32,
    pub key: Option<String>,           // Depot decryption key
    pub access_token: Option<String>,  // Access token for app
    pub stat_steam_id: Option<u64>,    // Stat SteamID for achievements
    pub manifest_id: Option<String>,   // Manifest override GID
    pub manifest_size: Option<u64>,    // Manifest override size
    pub purchase_time: Option<u32>,    // Purchase timestamp
    pub process_name: Option<String>,  // Process name -> appid mapping
}

#[derive(Debug, Clone, Default)]
pub struct LuaConfigRegistry {
    pub depots: BTreeMap<u32, LuaConfigEntry>,    // depot_id -> entry
    pub apps: BTreeMap<u32, LuaConfigEntry>,      // app_id -> entry
    pub owned_apps: BTreeSet<u32>,                 // Owned app IDs
    pub pending_removals: Vec<u32>,                // Pending depot removals
    pub pending_additions: Vec<u32>,               // Pending depot additions
    pub eticket_url: Option<String>,               // Backend eticket URL
}

// ============================================================================
// Global registry
// ============================================================================

static LUA_CONFIG: OnceLock<Mutex<LuaConfigRegistry>> = OnceLock::new();

fn lua_config() -> &'static Mutex<LuaConfigRegistry> {
    LUA_CONFIG.get_or_init(|| Mutex::new(LuaConfigRegistry::default()))
}

// ============================================================================
// Public API (matching usefulsteamtools)
// ============================================================================

/// Check if a depot is in the config
pub fn has_depot(app_or_depot_id: u32, include_apps: bool) -> bool {
    let config = lua_config().lock().unwrap();
    config.depots.contains_key(&app_or_depot_id) ||
    (include_apps && config.apps.contains_key(&app_or_depot_id))
}

/// Get decryption key for a depot
pub fn get_decryption_key(depot_id: u32) -> Option<String> {
    let config = lua_config().lock().unwrap();
    config.depots.get(&depot_id).and_then(|e| e.key.clone())
}

/// Get access token for an app
pub fn get_access_token(app_id: u32) -> Option<String> {
    let config = lua_config().lock().unwrap();
    config.apps.get(&app_id).and_then(|e| e.access_token.clone())
}

/// Get stat SteamID for an app
pub fn get_stat_steam_id(app_id: u32) -> Option<u64> {
    let config = lua_config().lock().unwrap();
    config.apps.get(&app_id).and_then(|e| e.stat_steam_id)
}

/// Get all manifest overrides (depot_id -> (gid, size))
pub fn get_manifest_overrides() -> BTreeMap<u32, (String, u64)> {
    let config = lua_config().lock().unwrap();
    config.depots.iter()
        .filter_map(|(id, e)| {
            e.manifest_id.as_ref().map(|gid| {
                (*id, (gid.clone(), e.manifest_size.unwrap_or(0)))
            })
        })
        .collect()
}

/// Get all depot IDs in config
pub fn get_all_depot_ids() -> Vec<u32> {
    let config = lua_config().lock().unwrap();
    config.depots.keys().copied().collect()
}

/// Check if app is owned (in config)
pub fn is_owned(app_id: u32) -> bool {
    let config = lua_config().lock().unwrap();
    config.owned_apps.contains(&app_id) || config.apps.contains_key(&app_id)
}

/// Get purchase time for an app
pub fn get_purchase_time(app_id: u32) -> Option<u32> {
    let config = lua_config().lock().unwrap();
    config.apps.get(&app_id).and_then(|e| e.purchase_time)
}

/// Map process name to appid
pub fn get_app_id_for_process(image_name: &str) -> Option<u32> {
    let config = lua_config().lock().unwrap();
    for (app_id, entry) in &config.apps {
        if entry.process_name.as_deref() == Some(image_name) {
            return Some(*app_id);
        }
    }
    None
}

/// Get eticket backend URL
pub fn get_eticket_url() -> Option<String> {
    let config = lua_config().lock().unwrap();
    config.eticket_url.clone()
}

/// Mark an app as owned
pub fn mark_owned(app_id: u32) {
    let mut config = lua_config().lock().unwrap();
    config.owned_apps.insert(app_id);
    // Also ensure app entry exists
    config.apps.entry(app_id).or_default().id = app_id;
}

/// Get and clear pending depot removals
pub fn take_pending_removals() -> Vec<u32> {
    let mut config = lua_config().lock().unwrap();
    std::mem::take(&mut config.pending_removals)
}

/// Get and clear pending depot additions
pub fn take_pending_additions() -> Vec<u32> {
    let mut config = lua_config().lock().unwrap();
    std::mem::take(&mut config.pending_additions)
}

// ============================================================================
// Lua file parsing
// ============================================================================

/// Parse a single .lua file using mlua
pub fn parse_lua_file(path: &Path, base_app_id: u32) -> Result<Vec<LuaConfigEntry>, String> {
    // Try to use mlua if available, otherwise use simple text parsing
    #[cfg(feature = "mlua")]
    {
        return parse_lua_file_mlua(path, base_app_id);
    }
    #[cfg(not(feature = "mlua"))]
    {
        parse_lua_file_simple(path, base_app_id)
    }
}

/// Simple text-based Lua parser (fallback when mlua not available)
fn parse_lua_file_simple(path: &Path, base_app_id: u32) -> Result<Vec<LuaConfigEntry>, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;

    let mut entries = Vec::new();
    let mut current_entry = LuaConfigEntry {
        id: base_app_id,
        ..Default::default()
    };
    let mut in_table = false;

    for line in content.lines() {
        let line = line.trim();

        // Skip comments
        if line.starts_with("--") {
            continue;
        }

        // Detect table start: `config = {` or `depot = {`
        if line.contains("={") || (line.ends_with('{') && !line.starts_with("--")) {
            in_table = true;
            continue;
        }

        // Detect table end
        if line == "}" {
            in_table = false;
            if current_entry.id != 0 {
                entries.push(std::mem::take(&mut current_entry));
                current_entry = LuaConfigEntry::default();
            }
            continue;
        }

        if !in_table {
            continue;
        }

        // Parse key = value
        if let Some((key, value)) = parse_lua_assignment(line) {
            match key.as_str() {
                "id" => current_entry.id = value.parse().unwrap_or(0),
                "key" => current_entry.key = Some(value.trim_matches('"').to_string()),
                "access_token" => current_entry.access_token = Some(value.trim_matches('"').to_string()),
                "stat_steam_id" => current_entry.stat_steam_id = value.parse().ok(),
                "manifest_id" => current_entry.manifest_id = Some(value.trim_matches('"').to_string()),
                "manifest_size" => current_entry.manifest_size = value.parse().ok(),
                "purchase_time" => current_entry.purchase_time = value.parse().ok(),
                "process_name" => current_entry.process_name = Some(value.trim_matches('"').to_string()),
                "eticket_url" => {
                    // Global config
                    let mut config = lua_config().lock().unwrap();
                    config.eticket_url = Some(value.trim_matches('"').to_string());
                }
                _ => {}
            }
        }
    }

    Ok(entries)
}

fn parse_lua_assignment(line: &str) -> Option<(String, String)> {
    let line = line.trim_end_matches(',').trim();
    if let Some((key, value)) = line.split_once('=') {
        Some((key.trim().to_string(), value.trim().to_string()))
    } else {
        None
    }
}

/// Parse all .lua files in a directory
pub fn parse_lua_directory(dir: &Path) -> Result<(), String> {
    let mut config = lua_config().lock().unwrap();

    for entry in fs::read_dir(dir).map_err(|e| format!("Failed to read dir: {}", e))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) != Some("lua") {
            continue;
        }

        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let base_id = stem.split('_').next().and_then(|s| s.parse().ok()).unwrap_or(0);

        let entries = parse_lua_file(&path, base_id)?;

        for entry in entries {
            if entry.id != 0 {
                if entry.key.is_some() || entry.manifest_id.is_some() {
                    // Depot entry
                    config.depots.insert(entry.id, entry.clone());
                } else {
                    // App entry
                    config.apps.insert(entry.id, entry.clone());
                    config.owned_apps.insert(entry.id);
                }
            }
        }
    }

    Ok(())
}

// ============================================================================
// File watchers (LuaFileWatcher + ConfigFileWatcher)
// ============================================================================

/// Watch .lua files for changes
pub struct LuaFileWatcher {
    watchers: Vec<Watch>,
    watch_dirs: Vec<PathBuf>,
}

impl LuaFileWatcher {
    pub fn new() -> Self {
        Self { watchers: Vec::new(), watch_dirs: Vec::new() }
    }

    pub fn start(&mut self, watch_dirs: Vec<PathBuf>) -> Result<(), String> {
        self.watch_dirs = watch_dirs.clone();

        for dir in watch_dirs {
            if !dir.exists() {
                continue;
            }

            let mut watch = Watch::new();
            if watch.open(dir.to_str().unwrap(), 8192) {
                self.watchers.push(watch);
            }
        }

        Ok(())
    }

    pub fn stop(&mut self) {
        for watch in &mut self.watchers {
            watch.cancel();
        }
        self.watchers.clear();
    }

    /// Check for changes and reload affected .lua files
    pub fn check_and_reload(&mut self) -> Result<bool, String> {
        let mut changed = false;

        for watch in &mut self.watchers {
            let changes = watch.drain();
            for change in changes {
                if change.action == ChangeAction::Added ||
                   change.action == ChangeAction::Modified ||
                   change.action == ChangeAction::Removed {
                    // Reload the .lua file
                    if let Some(dir) = self.watch_dirs.iter().find(|d| change.relative_path.starts_with(d.to_str().unwrap_or(""))) {
                        let path = dir.join(&change.relative_path);
                        if path.extension().and_then(|s| s.to_str()) == Some("lua") {
                            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                            let base_id = stem.split('_').next().and_then(|s| s.parse().ok()).unwrap_or(0);

                            if change.action == ChangeAction::Removed {
                                // Remove from config
                                let mut cfg = lua_config().lock().unwrap();
                                if let Some(id) = stem.split('_').next().and_then(|s| s.parse().ok()) {
                                    cfg.depots.remove(&id);
                                    cfg.apps.remove(&id);
                                    cfg.owned_apps.remove(&id);
                                }
                            } else if path.exists() {
                                let _ = parse_lua_file(&path, base_id).map(|entries| {
                                    let mut cfg = lua_config().lock().unwrap();
                                    for entry in entries {
                                        if entry.id != 0 {
                                            if entry.key.is_some() || entry.manifest_id.is_some() {
                                                cfg.depots.insert(entry.id, entry.clone());
                                            } else {
                                                cfg.apps.insert(entry.id, entry.clone());
                                                cfg.owned_apps.insert(entry.id);
                                            }
                                        }
                                    }
                                });
                            }
                            changed = true;
                        }
                    }
                }
            }
        }

        Ok(changed)
    }
}

/// Watch opensteamtool.toml (or equivalent) for config changes
pub struct ConfigFileWatcher {
    watch: Option<Watch>,
    config_path: PathBuf,
    lua_dir: PathBuf,
}

impl ConfigFileWatcher {
    pub fn new(config_path: PathBuf, lua_dir: PathBuf) -> Self {
        Self { watch: None, config_path, lua_dir }
    }

    pub fn start(&mut self) -> Result<(), String> {
        if let Some(parent) = self.config_path.parent() {
            let mut watch = Watch::new();
            if watch.open(parent.to_str().unwrap(), 8192) {
                self.watch = Some(watch);
            }
        }
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(watch) = &mut self.watch {
            watch.cancel();
        }
        self.watch = None;
    }

    pub fn check_and_reload(&mut self) -> Result<bool, String> {
        if let Some(watch) = &mut self.watch {
            let changes = watch.drain();
            for change in changes {
                if change.relative_path == self.config_path.file_name().and_then(|s| s.to_str()).unwrap_or("") {
                    // Config file changed - reload entire config
                    let _ = parse_lua_directory(&self.lua_dir);
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_parse_simple_lua() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("123456_config.lua");
        fs::write(&file, r#"
config = {
    id = 123456,
    key = "0123456789abcdef0123456789abcdef",
    access_token = "token123",
    stat_steam_id = 76561198028121353,
    purchase_time = 1234567890,
}
"#).unwrap();

        let entries = parse_lua_file(&file, 123456).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, 123456);
        assert_eq!(entries[0].key, Some("0123456789abcdef0123456789abcdef".to_string()));
    }

    #[test]
    fn test_has_depot() {
        let mut config = lua_config().lock().unwrap();
        config.depots.insert(123, LuaConfigEntry { id: 123, ..Default::default() });
        assert!(has_depot(123, false));
        assert!(!has_depot(456, false));
    }
}