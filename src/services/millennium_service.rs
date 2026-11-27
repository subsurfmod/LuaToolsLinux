//! MillenniumService — real Linux Millennium installation/process detection and
//! clean plugin coexistence / conflict management (LT-21).
//!
//! Upstream Millennium on Linux does NOT host an RPC server on `127.0.0.1:38495`.
//! Detection checks Linux filesystem locations (`~/.local/share/millennium`,
//! `~/.config/millennium/plugins.json`, Steam root Millennium directories/configs,
//! and system installs) as well as running Millennium / Steam processes via `/proc`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::services::plugin_log::PluginLog;

/// Known Millennium plugin identifiers that conflict with LuaTools Linux's
/// CEF bridge / Store plugin or SLSsteam integration.
const CONFLICTING_PLUGIN_TOKENS: &[&str] = &[
    "luatools",
    "luatools-plugin",
    "luatools_plugin",
    "luatoolssteamplugin",
    "ltsp",
    "steamtools",
    "stplugin",
    "st-plugin",
    "manifesthub",
    "manifest-hub",
    "morrenus",
    "greenluma",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MillenniumDetection {
    pub installed: bool,
    pub running: bool,
    pub version: Option<String>,
    pub data_dir: Option<PathBuf>,
    pub config_files: Vec<PathBuf>,
    pub conflicting_plugins: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct MillenniumService {
    pub steam_root: Option<PathBuf>,
    pub millennium_data_dir: Option<PathBuf>,
    pub millennium_config_dir: Option<PathBuf>,
}

impl MillenniumService {
    pub fn new(steam_root: Option<PathBuf>) -> Self {
        Self {
            steam_root,
            millennium_data_dir: None,
            millennium_config_dir: None,
        }
    }

    pub fn with_dirs(
        steam_root: Option<PathBuf>,
        data_dir: Option<PathBuf>,
        config_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            steam_root,
            millennium_data_dir: data_dir,
            millennium_config_dir: config_dir,
        }
    }

    /// Candidate Millennium data directories on Linux.
    pub fn data_dirs(steam_root: Option<&Path>) -> Vec<PathBuf> {
        let mut dirs_list = Vec::new();
        if let Ok(env_data) = std::env::var("MILLENNIUM__DATA_PATH") {
            let trimmed = env_data.trim();
            if !trimmed.is_empty() {
                dirs_list.push(PathBuf::from(trimmed));
            }
        }
        if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME") {
            let trimmed = xdg_data.trim();
            if !trimmed.is_empty() {
                dirs_list.push(PathBuf::from(trimmed).join("millennium"));
            }
        }
        if let Some(h) = dirs::home_dir() {
            dirs_list.push(h.join(".local/share/millennium"));
            dirs_list.push(h.join(".local/share/Steam/ext"));
            dirs_list.push(h.join(".local/share/Steam/millennium"));
        }
        if let Some(sr) = steam_root {
            dirs_list.push(sr.join("ext"));
            dirs_list.push(sr.join("millennium"));
        }
        dirs_list.push(PathBuf::from("/usr/share/millennium"));
        dirs_list.push(PathBuf::from("/usr/lib/millennium"));
        dedup_paths(dirs_list)
    }

    /// Candidate Millennium configuration files (`plugins.json` and `config.json`) on Linux.
    pub fn config_paths(steam_root: Option<&Path>) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Ok(env_cfg) = std::env::var("MILLENNIUM__CONFIG_PATH") {
            let trimmed = env_cfg.trim();
            if !trimmed.is_empty() {
                let p = PathBuf::from(trimmed);
                paths.push(p.join("plugins.json"));
                paths.push(p.join("config.json"));
            }
        }
        if let Ok(xdg_cfg) = std::env::var("XDG_CONFIG_HOME") {
            let trimmed = xdg_cfg.trim();
            if !trimmed.is_empty() {
                let p = PathBuf::from(trimmed).join("millennium");
                paths.push(p.join("plugins.json"));
                paths.push(p.join("config.json"));
            }
        }
        if let Some(h) = dirs::home_dir() {
            paths.push(h.join(".config/millennium/plugins.json"));
            paths.push(h.join(".config/millennium/config.json"));
            paths.push(h.join(".local/share/millennium/config/plugins.json"));
            paths.push(h.join(".local/share/millennium/config/config.json"));
            paths.push(h.join(".local/share/millennium/plugins.json"));
            paths.push(h.join(".local/share/Steam/ext/config.json"));
            paths.push(h.join(".local/share/Steam/ext/plugins.json"));
            paths.push(h.join(".local/share/Steam/millennium/config/config.json"));
            paths.push(h.join(".local/share/Steam/millennium/config/plugins.json"));
        }
        if let Some(sr) = steam_root {
            paths.push(sr.join("ext/config.json"));
            paths.push(sr.join("ext/plugins.json"));
            paths.push(sr.join("millennium/config/config.json"));
            paths.push(sr.join("millennium/config/plugins.json"));
            paths.push(sr.join("millennium/config.json"));
            paths.push(sr.join("millennium/plugins.json"));
        }
        dedup_paths(paths)
    }

    fn effective_data_dirs(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(ref d) = self.millennium_data_dir {
            out.push(d.clone());
        } else {
            out.extend(Self::data_dirs(self.steam_root.as_deref()));
        }
        dedup_paths(out)
    }

    fn effective_config_paths(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(ref c) = self.millennium_config_dir {
            out.push(c.join("plugins.json"));
            out.push(c.join("config.json"));
        } else {
            out.extend(Self::config_paths(self.steam_root.as_deref()));
        }
        dedup_paths(out)
    }

    /// Checks Linux `/proc` for a running Millennium process or `libmillennium`
    /// loaded inside a running `steam` / `steamwebhelper` process.
    pub fn is_millennium_process_running() -> bool {
        let Ok(entries) = fs::read_dir("/proc") else {
            return false;
        };
        for entry in entries.flatten() {
            let fname = entry.file_name();
            let Some(pid_str) = fname.to_str() else {
                continue;
            };
            if !pid_str.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let proc_dir = entry.path();
            if let Ok(comm) = fs::read_to_string(proc_dir.join("comm")) {
                let lc = comm.trim().to_ascii_lowercase();
                if lc.contains("millennium") {
                    return true;
                }
                if lc == "steam" || lc.starts_with("steamwebhelper") {
                    if let Ok(maps) = fs::read_to_string(proc_dir.join("maps")) {
                        let maps_lc = maps.to_ascii_lowercase();
                        if maps_lc.contains("libmillennium") || maps_lc.contains("/millennium/") {
                            return true;
                        }
                    }
                    if let Ok(env_bytes) = fs::read(proc_dir.join("environ")) {
                        let env_str = String::from_utf8_lossy(&env_bytes);
                        if env_str.contains("MILLENNIUM__") || env_str.contains("libmillennium") {
                            return true;
                        }
                    }
                }
            }
            if let Ok(cmdline_bytes) = fs::read(proc_dir.join("cmdline")) {
                let cmdline = String::from_utf8_lossy(&cmdline_bytes).to_ascii_lowercase();
                if cmdline.contains("millennium") && !cmdline.contains("luatools") {
                    return true;
                }
            }
        }
        false
    }

    fn detect_shared_libs(steam_root: Option<&Path>) -> bool {
        let mut candidates = vec![
            PathBuf::from("/usr/lib/millennium/libmillennium_x86.so"),
            PathBuf::from("/usr/lib/millennium/libmillennium_x64.so"),
            PathBuf::from("/usr/lib32/millennium/libmillennium_x86.so"),
            PathBuf::from("/usr/lib64/millennium/libmillennium_x64.so"),
        ];
        if let Some(sr) = steam_root {
            candidates.push(sr.join("ubuntu12_32/libmillennium_x86.so"));
            candidates.push(sr.join("ubuntu12_64/libmillennium_x64.so"));
            candidates.push(sr.join("ubuntu12_32/libmillennium.so"));
        }
        if let Some(h) = dirs::home_dir() {
            candidates.push(h.join(".local/share/Steam/ubuntu12_32/libmillennium_x86.so"));
            candidates.push(h.join(".local/share/Steam/ubuntu12_64/libmillennium_x64.so"));
        }
        candidates.into_iter().any(|p| p.is_file())
    }

    fn read_version_from_dirs_and_configs(
        data_dirs: &[PathBuf],
        config_files: &[PathBuf],
    ) -> Option<String> {
        // 1. Inspect metadata files in data dirs
        for dir in data_dirs {
            for fname in ["package.json", "millennium.json", "manifest.json"] {
                let p = dir.join(fname);
                if let Ok(raw) = fs::read_to_string(&p) {
                    if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                        if let Some(ver) = v
                            .get("version")
                            .and_then(|x| x.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                        {
                            return Some(ver.to_string());
                        }
                    }
                }
            }
            let ver_file = dir.join("version");
            if let Ok(raw) = fs::read_to_string(&ver_file) {
                let trimmed = raw.trim();
                if !trimmed.is_empty() && trimmed.len() <= 64 {
                    return Some(trimmed.to_string());
                }
            }
        }

        // 2. Inspect config files for version field
        for cfg in config_files {
            if let Ok(raw) = fs::read_to_string(cfg) {
                if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                    let ver = v
                        .get("version")
                        .or_else(|| v.get("general").and_then(|g| g.get("version")))
                        .and_then(|x| x.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty());
                    if let Some(s) = ver {
                        return Some(s.to_string());
                    }
                }
            }
        }
        None
    }

    /// Runs full Linux filesystem + `/proc` detection for Millennium.
    pub fn detect(&self) -> MillenniumDetection {
        let data_dirs = self.effective_data_dirs();
        let existing_data_dir = data_dirs.iter().find(|d| d.is_dir()).cloned();
        let config_files: Vec<PathBuf> = self
            .effective_config_paths()
            .into_iter()
            .filter(|p| p.is_file())
            .collect();

        // Only inspect system shared libs / running processes when using default detection
        let using_custom_dirs =
            self.millennium_data_dir.is_some() || self.millennium_config_dir.is_some();
        let has_libs = !using_custom_dirs && Self::detect_shared_libs(self.steam_root.as_deref());
        let running = !using_custom_dirs && Self::is_millennium_process_running();

        let installed = existing_data_dir.is_some() || !config_files.is_empty() || has_libs;
        let version = Self::read_version_from_dirs_and_configs(&data_dirs, &config_files).or_else(
            || {
                if running {
                    Some("active (Linux)".to_string())
                } else if installed {
                    Some("installed (Linux)".to_string())
                } else {
                    None
                }
            },
        );
        let conflicting_plugins = Self::detect_conflicts_in_paths(&config_files, &data_dirs);

        MillenniumDetection {
            installed,
            running,
            version,
            data_dir: existing_data_dir,
            config_files,
            conflicting_plugins,
        }
    }

    pub fn detect_with_root(steam_root: Option<&Path>) -> MillenniumDetection {
        Self::new(steam_root.map(Path::to_path_buf)).detect()
    }

    /// Returns true if Millennium is installed or running on this Linux system.
    pub fn is_available(&self) -> bool {
        let d = self.detect();
        d.installed || d.running
    }

    /// Returns true if Millennium is currently active in a running process.
    pub fn is_running(&self) -> bool {
        self.detect().running
    }

    pub fn get_version(&self) -> Option<String> {
        self.detect().version
    }

    pub fn get_steam_path(&self) -> Option<String> {
        for cfg in self.effective_config_paths() {
            if let Ok(raw) = fs::read_to_string(&cfg) {
                if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                    if let Some(sp) = v
                        .get("steam_path")
                        .or_else(|| v.get("steamPath"))
                        .or_else(|| v.get("general").and_then(|g| g.get("steamPath")))
                        .and_then(|x| x.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        return Some(sp.to_string());
                    }
                }
            }
        }
        if let Some(ref sr) = self.steam_root {
            return Some(sr.to_string_lossy().to_string());
        }
        crate::services::steam_service::detect_steam_install(None)
            .map(|inst| inst.root.to_string_lossy().to_string())
    }

    /// Millennium on Linux does not expose a local HTTP restart RPC endpoint;
    /// callers should fall back to direct graceful Steam restart (`steam -shutdown`).
    pub fn restart_steam(&self) -> Result<(), String> {
        Err(
            "Millennium on Linux does not expose a restart RPC; use direct Steam restart".into(),
        )
    }

    /// Returns true if a Millennium plugin identifier/name conflicts with LuaTools Linux.
    pub fn is_conflicting_plugin_name(name: &str) -> bool {
        let lower = name.trim().to_ascii_lowercase();
        if lower.is_empty() {
            return false;
        }
        let first_tok = lower.split_whitespace().next().unwrap_or(&lower);
        CONFLICTING_PLUGIN_TOKENS.iter().any(|&tok| {
            first_tok == tok
                || lower == tok
                || lower.contains("luatools")
                || lower.contains("steamtools")
        })
    }

    /// Detects conflicting plugins across existing Millennium configs and plugin folders.
    pub fn detect_conflicts(steam_root: Option<&Path>) -> Vec<String> {
        let cfgs: Vec<PathBuf> = Self::config_paths(steam_root)
            .into_iter()
            .filter(|p| p.is_file())
            .collect();
        let data_dirs = Self::data_dirs(steam_root);
        Self::detect_conflicts_in_paths(&cfgs, &data_dirs)
    }

    fn detect_conflicts_in_paths(config_files: &[PathBuf], data_dirs: &[PathBuf]) -> Vec<String> {
        let mut found = BTreeSet::new();

        for path in config_files {
            let Ok(raw) = fs::read_to_string(path) else {
                continue;
            };
            let Ok(val) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };

            // Shape 1: config.json { "plugins": { "enabledPlugins": ["luatools", ...] } }
            if let Some(list) = val
                .get("plugins")
                .and_then(|p| p.get("enabledPlugins"))
                .and_then(|e| e.as_array())
            {
                for item in list {
                    if let Some(name) = item.as_str() {
                        if Self::is_conflicting_plugin_name(name) {
                            found.insert(name.to_string());
                        }
                    }
                }
            }

            // Shape 2: plugins.json top-level array [ { "name": "...", "enabled": true } ]
            if let Some(arr) = val.as_array() {
                for item in arr {
                    let enabled = item
                        .get("enabled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    if !enabled {
                        continue;
                    }
                    let name = item
                        .get("name")
                        .or_else(|| item.get("common_name"))
                        .or_else(|| item.get("id"))
                        .or_else(|| item.get("path"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if Self::is_conflicting_plugin_name(name) {
                        found.insert(name.to_string());
                    }
                }
            }

            // Shape 3: plugins.json { "plugins": [ { "name": "...", "enabled": true } ] }
            if let Some(arr) = val.get("plugins").and_then(|p| p.as_array()) {
                for item in arr {
                    let enabled = item
                        .get("enabled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    if !enabled {
                        continue;
                    }
                    let name = item
                        .get("name")
                        .or_else(|| item.get("common_name"))
                        .or_else(|| item.get("id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if Self::is_conflicting_plugin_name(name) {
                        found.insert(name.to_string());
                    }
                }
            }

            // Shape 4: plugins.json { "enabled_plugins": ["..."] } or { "enabledPlugins": ["..."] }
            for key in ["enabled_plugins", "enabledPlugins"] {
                if let Some(arr) = val.get(key).and_then(|v| v.as_array()) {
                    for item in arr {
                        if let Some(name) = item.as_str() {
                            if Self::is_conflicting_plugin_name(name) {
                                found.insert(name.to_string());
                            }
                        }
                    }
                }
            }
        }

        // Also scan Millennium's plugins/ subdirectories for conflicting plugin folders
        for dir in data_dirs {
            let plugins_sub = dir.join("plugins");
            if let Ok(entries) = fs::read_dir(&plugins_sub) {
                for entry in entries.flatten() {
                    if !entry.path().is_dir() {
                        continue;
                    }
                    let dir_name = entry.file_name().to_string_lossy().to_string();
                    if Self::is_conflicting_plugin_name(&dir_name) {
                        // Only flag if not already marked disabled via marker
                        if !entry.path().join(".disabled_by_luatools").exists() {
                            found.insert(dir_name);
                        }
                    }
                }
            }
        }

        found.into_iter().collect()
    }

    /// Cleanly disables conflicting plugins inside the specified Millennium config files
    /// without touching non-conflicting user plugins and without blindly adding `"luatools"`
    /// to `enabledPlugins`.
    pub fn disable_conflicting_plugins_in_paths(paths: &[PathBuf]) -> Result<Vec<String>, String> {
        let mut disabled = BTreeSet::new();

        for path in paths {
            if !path.is_file() {
                continue;
            }
            let Ok(raw) = fs::read_to_string(path) else {
                continue;
            };
            let Ok(mut val) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            let mut modified = false;

            // Shape 1: { "plugins": { "enabledPlugins": [...] } }
            if let Some(plugins_obj) = val.get_mut("plugins").and_then(|p| p.as_object_mut()) {
                if let Some(list) = plugins_obj
                    .get_mut("enabledPlugins")
                    .and_then(|e| e.as_array_mut())
                {
                    let before = list.len();
                    list.retain(|entry| {
                        if let Some(name) = entry.as_str() {
                            if Self::is_conflicting_plugin_name(name) {
                                disabled.insert(name.to_string());
                                return false;
                            }
                        }
                        true
                    });
                    if list.len() != before {
                        modified = true;
                    }
                }
            }

            // Shape 2: top-level array of plugin objects
            if let Some(arr) = val.as_array_mut() {
                for item in arr.iter_mut() {
                    if let Some(obj) = item.as_object_mut() {
                        let name = obj
                            .get("name")
                            .or_else(|| obj.get("common_name"))
                            .or_else(|| obj.get("id"))
                            .or_else(|| obj.get("path"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let is_enabled = obj
                            .get("enabled")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true);
                        if is_enabled && Self::is_conflicting_plugin_name(&name) {
                            obj.insert("enabled".into(), json!(false));
                            disabled.insert(name);
                            modified = true;
                        }
                    }
                }
            }

            // Shape 3: { "plugins": [ { "name": "...", "enabled": true } ] }
            if let Some(arr) = val.get_mut("plugins").and_then(|p| p.as_array_mut()) {
                for item in arr.iter_mut() {
                    if let Some(obj) = item.as_object_mut() {
                        let name = obj
                            .get("name")
                            .or_else(|| obj.get("common_name"))
                            .or_else(|| obj.get("id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let is_enabled = obj
                            .get("enabled")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true);
                        if is_enabled && Self::is_conflicting_plugin_name(&name) {
                            obj.insert("enabled".into(), json!(false));
                            disabled.insert(name);
                            modified = true;
                        }
                    }
                }
            }

            // Shape 4: top-level enabled_plugins / enabledPlugins array
            for key in ["enabled_plugins", "enabledPlugins"] {
                if let Some(list) = val.get_mut(key).and_then(|v| v.as_array_mut()) {
                    let before = list.len();
                    list.retain(|entry| {
                        if let Some(name) = entry.as_str() {
                            if Self::is_conflicting_plugin_name(name) {
                                disabled.insert(name.to_string());
                                return false;
                            }
                        }
                        true
                    });
                    if list.len() != before {
                        modified = true;
                    }
                }
            }

            if modified {
                let pretty = serde_json::to_string_pretty(&val)
                    .map_err(|e| format!("Failed to serialize {}: {}", path.display(), e))?;
                let tmp = path.with_extension("json.tmp");
                fs::write(&tmp, pretty)
                    .map_err(|e| format!("Failed to write {}: {}", tmp.display(), e))?;
                fs::rename(&tmp, path)
                    .map_err(|e| format!("Failed to replace {}: {}", path.display(), e))?;
                PluginLog::global().info(
                    Some("Millennium"),
                    &format!(
                        "Disabled conflicting plugin entries in {}",
                        path.display()
                    ),
                );
            }
        }

        Ok(disabled.into_iter().collect())
    }

    /// Detects and disables conflicting plugins across all discovered Millennium config files.
    pub fn disable_conflicting_plugins(steam_root: Option<&Path>) -> Result<Vec<String>, String> {
        let paths = Self::config_paths(steam_root);
        Self::disable_conflicting_plugins_in_paths(&paths)
    }

    /// Reconciles Millennium coexistence for LuaTools Linux:
    /// detects existing Millennium configuration files and cleanly disables conflicting
    /// plugins so LuaTools' CEF bridge + `:6767` backend operate without duplicate injection.
    /// Does NOT fabricate configs when Millennium is absent and does NOT blindly add
    /// `"luatools"` into Millennium's `enabledPlugins`.
    pub fn register_luatools_plugin(steam_root: Option<&Path>) -> bool {
        let existing_configs: Vec<PathBuf> = Self::config_paths(steam_root)
            .into_iter()
            .filter(|p| p.is_file())
            .collect();
        if existing_configs.is_empty() {
            return false;
        }
        match Self::disable_conflicting_plugins_in_paths(&existing_configs) {
            Ok(disabled) => {
                if !disabled.is_empty() {
                    PluginLog::global().info(
                        Some("Millennium"),
                        &format!(
                            "Coexistence reconciled: disabled conflicting Millennium plugin(s): {}",
                            disabled.join(", ")
                        ),
                    );
                } else {
                    PluginLog::global().info(
                        Some("Millennium"),
                        "Coexistence verified: no conflicting Millennium plugins enabled",
                    );
                }
                true
            }
            Err(e) => {
                PluginLog::global().warn(
                    Some("Millennium"),
                    &format!("Failed to reconcile Millennium conflicts: {}", e),
                );
                false
            }
        }
    }
}

fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn detects_linux_millennium_from_data_dir_and_config_without_network() {
        let tmp = tempdir().unwrap();
        let data_dir = tmp.path().join("share/millennium");
        let cfg_dir = tmp.path().join("config/millennium");
        fs::create_dir_all(&data_dir).unwrap();
        fs::create_dir_all(&cfg_dir).unwrap();

        fs::write(
            data_dir.join("package.json"),
            r#"{"name":"millennium","version":"2.18.0"}"#,
        )
        .unwrap();
        fs::write(
            cfg_dir.join("plugins.json"),
            r#"[{"name":"protondb-badges","enabled":true}]"#,
        )
        .unwrap();

        let svc = MillenniumService::with_dirs(None, Some(data_dir.clone()), Some(cfg_dir));
        let det = svc.detect();
        assert!(det.installed);
        assert!(svc.is_available());
        assert_eq!(det.version.as_deref(), Some("2.18.0"));
        assert_eq!(det.data_dir.as_deref(), Some(data_dir.as_path()));
        assert!(det.conflicting_plugins.is_empty());

        // Missing directories cleanly report unavailable
        let empty = MillenniumService::with_dirs(
            None,
            Some(tmp.path().join("nope_data")),
            Some(tmp.path().join("nope_cfg")),
        );
        assert!(!empty.is_available());
        assert!(empty.get_version().is_none());
    }

    #[test]
    fn disables_conflicting_plugins_and_preserves_other_plugins() {
        let tmp = tempdir().unwrap();
        let cfg_json = tmp.path().join("config.json");
        let plugins_json = tmp.path().join("plugins.json");

        fs::write(
            &cfg_json,
            serde_json::to_string_pretty(&json!({
                "plugins": {
                    "enabledPlugins": ["steamdb", "luatools", "SteamTools"]
                },
                "themes": { "activeTheme": "default" }
            }))
            .unwrap(),
        )
        .unwrap();

        fs::write(
            &plugins_json,
            serde_json::to_string_pretty(&json!([
                { "name": "protondb", "enabled": true },
                { "name": "LuaToolsSteamPlugin", "enabled": true },
                { "name": "manifest-hub", "enabled": true }
            ]))
            .unwrap(),
        )
        .unwrap();

        let disabled = MillenniumService::disable_conflicting_plugins_in_paths(&[
            cfg_json.clone(),
            plugins_json.clone(),
        ])
        .unwrap();

        assert!(disabled.iter().any(|s| s == "luatools"));
        assert!(disabled.iter().any(|s| s == "SteamTools"));
        assert!(disabled.iter().any(|s| s == "LuaToolsSteamPlugin"));
        assert!(disabled.iter().any(|s| s == "manifest-hub"));

        let after_cfg: Value =
            serde_json::from_str(&fs::read_to_string(&cfg_json).unwrap()).unwrap();
        let enabled = after_cfg["plugins"]["enabledPlugins"].as_array().unwrap();
        assert_eq!(enabled, &vec![json!("steamdb")]);
        assert_eq!(after_cfg["themes"]["activeTheme"], json!("default"));

        let after_plugins: Value =
            serde_json::from_str(&fs::read_to_string(&plugins_json).unwrap()).unwrap();
        let arr = after_plugins.as_array().unwrap();
        assert_eq!(arr[0]["name"], json!("protondb"));
        assert_eq!(arr[0]["enabled"], json!(true));
        assert_eq!(arr[1]["enabled"], json!(false));
        assert_eq!(arr[2]["enabled"], json!(false));
    }
}
