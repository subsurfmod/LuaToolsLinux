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

    /// Resolves the Millennium plugins directory following Millennium v3.6+ env precedence:
    /// `MILLENNIUM__PLUGINS_PATH` -> `MILLENNIUM__DATA_PATH/plugins` ->
    /// `XDG_DATA_HOME/millennium/plugins` -> `~/.local/share/millennium/plugins`.
    pub fn resolve_plugins_dir() -> PathBuf {
        let plugins_env = std::env::var("MILLENNIUM__PLUGINS_PATH").ok();
        let data_env = std::env::var("MILLENNIUM__DATA_PATH").ok();
        let xdg_data_env = std::env::var("XDG_DATA_HOME").ok();
        let home = dirs::home_dir();
        Self::resolve_plugins_dir_from(
            plugins_env.as_deref(),
            data_env.as_deref(),
            xdg_data_env.as_deref(),
            home.as_deref(),
        )
    }

    fn resolve_plugins_dir_from(
        plugins_env: Option<&str>,
        data_env: Option<&str>,
        xdg_data_env: Option<&str>,
        home: Option<&Path>,
    ) -> PathBuf {
        if let Some(p) = plugins_env.map(str::trim).filter(|s| !s.is_empty()) {
            return PathBuf::from(p);
        }
        if let Some(d) = data_env.map(str::trim).filter(|s| !s.is_empty()) {
            return PathBuf::from(d).join("plugins");
        }
        if let Some(xdg) = xdg_data_env.map(str::trim).filter(|s| !s.is_empty()) {
            return PathBuf::from(xdg).join("millennium/plugins");
        }
        home.map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".local/share/millennium/plugins")
    }

    /// Candidate Millennium data directories on Linux (native + Flatpak Steam).
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
            // Flatpak Steam Millennium data directories
            dirs_list.push(h.join(".var/app/com.valvesoftware.Steam/.local/share/millennium"));
            dirs_list.push(h.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/ext"));
            dirs_list.push(h.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/millennium"));
        }
        if let Some(sr) = steam_root {
            dirs_list.push(sr.join("ext"));
            dirs_list.push(sr.join("millennium"));
        }
        dirs_list.push(PathBuf::from("/usr/share/millennium"));
        dirs_list.push(PathBuf::from("/usr/lib/millennium"));
        dedup_paths(dirs_list)
    }

    /// Candidate Millennium configuration files (`plugins.json` and `config.json`) on Linux
    /// (native + Flatpak Steam).
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
            // Flatpak Steam Millennium config paths
            paths.push(h.join(".var/app/com.valvesoftware.Steam/.config/millennium/plugins.json"));
            paths.push(h.join(".var/app/com.valvesoftware.Steam/.config/millennium/config.json"));
            paths.push(h.join(
                ".var/app/com.valvesoftware.Steam/.local/share/millennium/config/plugins.json",
            ));
            paths.push(h.join(
                ".var/app/com.valvesoftware.Steam/.local/share/millennium/config/config.json",
            ));
            paths.push(
                h.join(".var/app/com.valvesoftware.Steam/.local/share/millennium/plugins.json"),
            );
            paths.push(h.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/ext/config.json"));
            paths.push(
                h.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/ext/plugins.json"),
            );
            paths.push(h.join(
                ".var/app/com.valvesoftware.Steam/.local/share/Steam/millennium/config/config.json",
            ));
            paths.push(h.join(
                ".var/app/com.valvesoftware.Steam/.local/share/Steam/millennium/config/plugins.json",
            ));
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
        if lower == "luatoolslinux" {
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
                let tmp = atomic_tmp_path(path);
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

    /// Default sentinel file tracking whether LuaToolsLinux was auto-enabled and whether
    /// the user manually opted out in Steam's Millennium UI (`~/.config/luatools/millennium_plugin_state.json`).
    pub fn default_plugin_state_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config")
            .join("luatools")
            .join("millennium_plugin_state.json")
    }

    fn is_millennium_installed_on_host(steam_root: Option<&Path>) -> bool {
        if Self::detect_with_root(steam_root).installed {
            return true;
        }
        if Path::new("/usr/lib/millennium").is_dir() {
            return true;
        }
        if let Ok(plugins_env) = std::env::var("MILLENNIUM__PLUGINS_PATH") {
            let trimmed = plugins_env.trim();
            if !trimmed.is_empty() && Path::new(trimmed).is_dir() {
                return true;
            }
        }
        if let Some(h) = dirs::home_dir() {
            if h.join(".local/share/millennium").is_dir()
                || h.join(".config/millennium").is_dir()
                || h.join(".var/app/com.valvesoftware.Steam/.local/share/millennium")
                    .is_dir()
                || h.join(".var/app/com.valvesoftware.Steam/.config/millennium")
                    .is_dir()
            {
                return true;
            }
        }
        false
    }

    fn load_plugin_state(state_path: &Path) -> MillenniumPluginState {
        fs::read_to_string(state_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<MillenniumPluginState>(&raw).ok())
            .unwrap_or_default()
    }

    fn save_plugin_state(state_path: &Path, state: &MillenniumPluginState) {
        if let Some(parent) = state_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(pretty) = serde_json::to_string_pretty(state) {
            let tmp = atomic_tmp_path(state_path);
            if fs::write(&tmp, pretty).is_ok() {
                let _ = fs::rename(&tmp, state_path);
            }
        }
    }

    fn is_config_json_path(path: &Path) -> bool {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.eq_ignore_ascii_case("config.json"))
            .unwrap_or(false)
    }

    /// Inspects existing `config.json` files to check whether any defines
    /// `plugins.enabledPlugins` (`saw_enabled_list`) and whether `"LuaToolsLinux"` is enabled.
    fn inspect_config_jsons_for_luatools(configs: &[&PathBuf]) -> (bool, bool) {
        let mut saw_enabled_list = false;
        let mut is_enabled = false;
        for cfg in configs {
            if let Ok(raw) = fs::read_to_string(cfg) {
                if let Ok(val) = serde_json::from_str::<Value>(&raw) {
                    if let Some(arr) = val
                        .get("plugins")
                        .and_then(|p| p.get("enabledPlugins"))
                        .and_then(|e| e.as_array())
                    {
                        saw_enabled_list = true;
                        if arr.iter().any(|v| v.as_str() == Some("LuaToolsLinux")) {
                            is_enabled = true;
                            break;
                        }
                    }
                }
            }
        }
        (saw_enabled_list, is_enabled)
    }

    fn ensure_luatoolslinux_plugin_installed_in(
        millennium_installed: bool,
        plugins_base_dir: Option<&Path>,
        config_files: &[PathBuf],
        state_sentinel_path: &Path,
    ) -> Result<PathBuf, String> {
        if !millennium_installed {
            let msg = "Millennium is not installed; skipping LuaToolsLinux plugin deployment";
            PluginLog::global().info(Some("Millennium"), msg);
            return Err(msg.into());
        }

        let resolved_base_dir = plugins_base_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(Self::resolve_plugins_dir);
        let target_plugin_dir = resolved_base_dir.join("LuaToolsLinux");

        let existing_config_jsons: Vec<&PathBuf> = config_files
            .iter()
            .filter(|p| p.is_file() && Self::is_config_json_path(p))
            .collect();

        let mut state = Self::load_plugin_state(state_sentinel_path);
        if state.user_opted_out {
            // Still inspect config.json: if the user manually re-enabled "LuaToolsLinux"
            // inside Steam's Millennium UI, clear the opt-out sentinel and upgrade plugin files.
            let (_, manually_reenabled) =
                Self::inspect_config_jsons_for_luatools(&existing_config_jsons);
            if manually_reenabled {
                state.user_opted_out = false;
                state.auto_enabled_once = true;
                Self::save_plugin_state(state_sentinel_path, &state);
                PluginLog::global().info(
                    Some("Millennium"),
                    "Detected manual re-enable of LuaToolsLinux in Millennium config; clearing opt-out sentinel and upgrading plugin files",
                );
            } else {
                PluginLog::global().info(
                    Some("Millennium"),
                    "Respecting user opt-out sentinel; skipping LuaToolsLinux Millennium plugin auto-enable",
                );
                return Ok(target_plugin_dir);
            }
        } else if state.auto_enabled_once && !existing_config_jsons.is_empty() {
            // If we previously auto-enabled LuaToolsLinux and the user later removed it from
            // Millennium's config.json enabledPlugins, record the opt-out sentinel and respect it.
            let (saw_enabled_list, still_enabled) =
                Self::inspect_config_jsons_for_luatools(&existing_config_jsons);
            if saw_enabled_list && !still_enabled {
                state.user_opted_out = true;
                Self::save_plugin_state(state_sentinel_path, &state);
                PluginLog::global().info(
                    Some("Millennium"),
                    "Detected manual disable of LuaToolsLinux in Millennium config; recorded opt-out sentinel",
                );
                return Ok(target_plugin_dir);
            }
        }

        let installer =
            crate::services::plugin_installer_service::PluginInstallerService::default();
        let plugin_path = installer.install_embedded_millennium_plugin(Some(&resolved_base_dir))?;

        // Ensure config.json has general.injectJavascript, general.injectCSS, and
        // plugins.enabledPlugins containing "LuaToolsLinux" (creating missing objects).
        let mut confirmed_in_config = false;
        for cfg in existing_config_jsons {
            if let Ok(raw) = fs::read_to_string(cfg) {
                if let Ok(mut val) = serde_json::from_str::<Value>(&raw) {
                    let mut modified = false;

                    if !val.is_object() {
                        val = json!({});
                        modified = true;
                    }
                    if let Some(root_obj) = val.as_object_mut() {
                        if !root_obj.get("general").map_or(false, Value::is_object) {
                            root_obj.insert("general".into(), json!({}));
                            modified = true;
                        }
                        if let Some(gen) = root_obj.get_mut("general").and_then(Value::as_object_mut)
                        {
                            if gen.get("injectJavascript").and_then(Value::as_bool) != Some(true) {
                                gen.insert("injectJavascript".into(), json!(true));
                                modified = true;
                            }
                            if gen.get("injectCSS").and_then(Value::as_bool) != Some(true) {
                                gen.insert("injectCSS".into(), json!(true));
                                modified = true;
                            }
                        }

                        if !root_obj.get("plugins").map_or(false, Value::is_object) {
                            root_obj.insert("plugins".into(), json!({}));
                            modified = true;
                        }
                        if let Some(plugins_obj) =
                            root_obj.get_mut("plugins").and_then(Value::as_object_mut)
                        {
                            if !plugins_obj
                                .get("enabledPlugins")
                                .map_or(false, Value::is_array)
                            {
                                plugins_obj.insert("enabledPlugins".into(), json!([]));
                                modified = true;
                            }
                            if let Some(arr) = plugins_obj
                                .get_mut("enabledPlugins")
                                .and_then(Value::as_array_mut)
                            {
                                if !arr.iter().any(|v| v.as_str() == Some("LuaToolsLinux")) {
                                    arr.push(json!("LuaToolsLinux"));
                                    modified = true;
                                }
                            }
                        }
                    }

                    if modified {
                        if let Ok(pretty) = serde_json::to_string_pretty(&val) {
                            let tmp = atomic_tmp_path(cfg);
                            if fs::write(&tmp, pretty).is_ok() && fs::rename(&tmp, cfg).is_ok() {
                                confirmed_in_config = true;
                                PluginLog::global().info(
                                    Some("Millennium"),
                                    &format!("Enabled LuaToolsLinux plugin in {}", cfg.display()),
                                );
                            }
                        }
                    } else if val
                        .get("plugins")
                        .and_then(|p| p.get("enabledPlugins"))
                        .and_then(Value::as_array)
                        .map_or(false, |arr| {
                            arr.iter().any(|v| v.as_str() == Some("LuaToolsLinux"))
                        })
                    {
                        confirmed_in_config = true;
                    }
                }
            }
        }

        // Only mark auto_enabled_once after at least one config.json actually contains
        // "LuaToolsLinux" so fresh Millennium installs (before Steam's first launch creates
        // config.json) never trigger a false permanent opt-out later.
        if confirmed_in_config && !state.auto_enabled_once {
            state.auto_enabled_once = true;
            Self::save_plugin_state(state_sentinel_path, &state);
        }

        Ok(plugin_path)
    }

    /// Ensures that the native LuaToolsLinux plugin is installed in Millennium's plugins
    /// directory and enabled in `~/.config/millennium/config.json` non-destructively,
    /// only when Millennium is installed and the user has not opted out.
    pub fn ensure_luatoolslinux_plugin_installed(
        steam_root: Option<&Path>,
    ) -> Result<PathBuf, String> {
        let installed = Self::is_millennium_installed_on_host(steam_root);
        let cfgs = Self::config_paths(steam_root);
        let state_path = Self::default_plugin_state_path();
        Self::ensure_luatoolslinux_plugin_installed_in(installed, None, &cfgs, &state_path)
    }
}

fn atomic_tmp_path(target: &Path) -> PathBuf {
    let mut tmp_name = target
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("file"));
    tmp_name.push(format!(".tmp.{}", std::process::id()));
    target.with_file_name(tmp_name)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MillenniumPluginState {
    #[serde(default)]
    pub auto_enabled_once: bool,
    #[serde(default, alias = "opted_out", alias = "disabled")]
    pub user_opted_out: bool,
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

    #[test]
    fn ensure_luatoolslinux_skips_when_millennium_not_installed() {
        let tmp = tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        let cfg_json = tmp.path().join("config.json");
        let state_json = tmp.path().join("millennium_plugin_state.json");

        let res = MillenniumService::ensure_luatoolslinux_plugin_installed_in(
            false,
            Some(&plugins_dir),
            &[cfg_json],
            &state_json,
        );
        assert!(res.is_err());
        assert!(!plugins_dir.join("LuaToolsLinux").exists());
        assert!(!state_json.exists());
    }

    #[test]
    fn ensure_luatoolslinux_creates_missing_general_and_plugins_objects_and_preserves_plugins_json() {
        let tmp = tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        let cfg_json = tmp.path().join("config.json");
        let plugins_json = tmp.path().join("plugins.json");
        let state_json = tmp.path().join("millennium_plugin_state.json");

        // Minimal config.json missing both "general" and "plugins" objects
        fs::write(&cfg_json, r#"{"themes":{"activeTheme":"dark"}}"#).unwrap();
        // Sibling plugins.json array must NOT be clobbered
        let orig_plugins_json = r#"[{"name":"protondb","enabled":true}]"#;
        fs::write(&plugins_json, orig_plugins_json).unwrap();

        let deployed = MillenniumService::ensure_luatoolslinux_plugin_installed_in(
            true,
            Some(&plugins_dir),
            &[cfg_json.clone(), plugins_json.clone()],
            &state_json,
        )
        .unwrap();
        assert_eq!(deployed, plugins_dir.join("LuaToolsLinux"));
        assert!(deployed.join("plugin.json").is_file());

        let after_cfg: Value =
            serde_json::from_str(&fs::read_to_string(&cfg_json).unwrap()).unwrap();
        assert_eq!(after_cfg["themes"]["activeTheme"], json!("dark"));
        assert_eq!(after_cfg["general"]["injectJavascript"], json!(true));
        assert_eq!(after_cfg["general"]["injectCSS"], json!(true));
        assert_eq!(
            after_cfg["plugins"]["enabledPlugins"],
            json!(["LuaToolsLinux"])
        );

        // plugins.json must remain untouched
        assert_eq!(fs::read_to_string(&plugins_json).unwrap(), orig_plugins_json);
    }

    #[test]
    fn ensure_luatoolslinux_respects_user_opt_out_sentinel_and_manual_disable() {
        let tmp = tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        let cfg_json = tmp.path().join("config.json");
        let state_json = tmp.path().join("millennium_plugin_state.json");

        fs::write(&cfg_json, r#"{"plugins":{"enabledPlugins":["protondb"]}}"#).unwrap();

        // 1. First run auto-enables LuaToolsLinux and records auto_enabled_once
        MillenniumService::ensure_luatoolslinux_plugin_installed_in(
            true,
            Some(&plugins_dir),
            &[cfg_json.clone()],
            &state_json,
        )
        .unwrap();
        let after_first: Value =
            serde_json::from_str(&fs::read_to_string(&cfg_json).unwrap()).unwrap();
        assert_eq!(
            after_first["plugins"]["enabledPlugins"],
            json!(["protondb", "LuaToolsLinux"])
        );

        // 2. Simulate user disabling LuaToolsLinux in Steam's Millennium UI
        fs::write(&cfg_json, r#"{"plugins":{"enabledPlugins":["protondb"]}}"#).unwrap();

        // 3. Startup self-heal detects manual removal, writes opt-out sentinel, and does NOT re-enable
        MillenniumService::ensure_luatoolslinux_plugin_installed_in(
            true,
            Some(&plugins_dir),
            &[cfg_json.clone()],
            &state_json,
        )
        .unwrap();
        let after_disable: Value =
            serde_json::from_str(&fs::read_to_string(&cfg_json).unwrap()).unwrap();
        assert_eq!(after_disable["plugins"]["enabledPlugins"], json!(["protondb"]));
        let state: MillenniumPluginState =
            serde_json::from_str(&fs::read_to_string(&state_json).unwrap()).unwrap();
        assert!(state.user_opted_out);

        // 4. Subsequent runs also respect explicit user_opted_out sentinel without modifying config
        let plugins_dir2 = tmp.path().join("plugins2");
        MillenniumService::ensure_luatoolslinux_plugin_installed_in(
            true,
            Some(&plugins_dir2),
            &[cfg_json.clone()],
            &state_json,
        )
        .unwrap();
        assert!(!plugins_dir2.join("LuaToolsLinux").exists());
        let after_optout: Value =
            serde_json::from_str(&fs::read_to_string(&cfg_json).unwrap()).unwrap();
        assert_eq!(after_optout["plugins"]["enabledPlugins"], json!(["protondb"]));

        // 5. If user manually re-enables "LuaToolsLinux" in config.json, clear opt-out sentinel
        //    and deploy/upgrade plugin files.
        fs::write(
            &cfg_json,
            r#"{"plugins":{"enabledPlugins":["protondb","LuaToolsLinux"]}}"#,
        )
        .unwrap();
        MillenniumService::ensure_luatoolslinux_plugin_installed_in(
            true,
            Some(&plugins_dir2),
            &[cfg_json.clone()],
            &state_json,
        )
        .unwrap();
        assert!(plugins_dir2
            .join("LuaToolsLinux/.millennium/Dist/webkit.js")
            .is_file());
        let state_after_reenable: MillenniumPluginState =
            serde_json::from_str(&fs::read_to_string(&state_json).unwrap()).unwrap();
        assert!(!state_after_reenable.user_opted_out);
        assert!(state_after_reenable.auto_enabled_once);
    }

    #[test]
    fn ensure_luatoolslinux_waits_for_config_json_before_marking_auto_enabled_once() {
        let tmp = tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        let cfg_json = tmp.path().join("config.json"); // does NOT exist yet (fresh Millennium)
        let state_json = tmp.path().join("millennium_plugin_state.json");

        // 1. First run before Steam creates config.json deploys plugin files,
        //    but must NOT mark auto_enabled_once = true yet.
        MillenniumService::ensure_luatoolslinux_plugin_installed_in(
            true,
            Some(&plugins_dir),
            &[cfg_json.clone()],
            &state_json,
        )
        .unwrap();
        assert!(plugins_dir.join("LuaToolsLinux/plugin.json").is_file());
        assert!(!state_json.exists());

        // 2. Steam launches for the first time and creates config.json with empty enabledPlugins.
        fs::write(&cfg_json, r#"{"plugins":{"enabledPlugins":[]}}"#).unwrap();

        // 3. Next run must auto-enable LuaToolsLinux (not falsely conclude user opted out).
        MillenniumService::ensure_luatoolslinux_plugin_installed_in(
            true,
            Some(&plugins_dir),
            &[cfg_json.clone()],
            &state_json,
        )
        .unwrap();
        let after: Value = serde_json::from_str(&fs::read_to_string(&cfg_json).unwrap()).unwrap();
        assert_eq!(after["plugins"]["enabledPlugins"], json!(["LuaToolsLinux"]));
        let state: MillenniumPluginState =
            serde_json::from_str(&fs::read_to_string(&state_json).unwrap()).unwrap();
        assert!(state.auto_enabled_once);
        assert!(!state.user_opted_out);
    }

    #[test]
    fn resolve_plugins_dir_precedence_and_flatpak_paths() {
        let home = Path::new("/home/tester");
        assert_eq!(
            MillenniumService::resolve_plugins_dir_from(
                Some("/custom/plugins"),
                Some("/custom/data"),
                Some("/custom/xdg"),
                Some(home),
            ),
            PathBuf::from("/custom/plugins")
        );
        assert_eq!(
            MillenniumService::resolve_plugins_dir_from(
                Some("   "),
                Some("/custom/data"),
                Some("/custom/xdg"),
                Some(home),
            ),
            PathBuf::from("/custom/data/plugins")
        );
        assert_eq!(
            MillenniumService::resolve_plugins_dir_from(None, None, Some("/custom/xdg"), Some(home)),
            PathBuf::from("/custom/xdg/millennium/plugins")
        );
        assert_eq!(
            MillenniumService::resolve_plugins_dir_from(None, None, None, Some(home)),
            PathBuf::from("/home/tester/.local/share/millennium/plugins")
        );

        let data_dirs = MillenniumService::data_dirs(None);
        let config_paths = MillenniumService::config_paths(None);
        if dirs::home_dir().is_some() {
            assert!(data_dirs.iter().any(|p| p
                .to_string_lossy()
                .contains(".var/app/com.valvesoftware.Steam/.local/share/millennium")));
            assert!(config_paths.iter().any(|p| p
                .to_string_lossy()
                .contains(".var/app/com.valvesoftware.Steam/.config/millennium/config.json")));
        }
    }
}
