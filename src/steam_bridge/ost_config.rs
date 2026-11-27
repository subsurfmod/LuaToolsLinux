//! opensteamtool.toml engine configuration parser and state store (BST-18).
//!
//! Ported directly from usefulsteamtools `Config.cpp` / `Config.h`.
//! Supports runtime loading, defaulting, serialization, and dynamic querying of:
//! - `[manifest]` provider, timeouts
//! - `[log]` level (trace, debug, info, warn, error)
//! - `[lua]` custom search directories
//! - `[remote]` mirror templates
//! - `[stats]` enable_api flag
//! - `[update]` auto-update flag
//! - `[donate]` ManifestDonor settings and rate-limit clamps
//! - `[[inject]]` game process injection configurations
//! - `[cloud]` Steam Cloud save redirection settings

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Mutex, RwLock};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Default for LogLevel {
    fn default() -> Self {
        LogLevel::Debug
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogLevel::Trace => write!(f, "trace"),
            LogLevel::Debug => write!(f, "debug"),
            LogLevel::Info => write!(f, "info"),
            LogLevel::Warn => write!(f, "warn"),
            LogLevel::Error => write!(f, "error"),
        }
    }
}

impl FromStr for LogLevel {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "trace" => Ok(LogLevel::Trace),
            "debug" => Ok(LogLevel::Debug),
            "info" => Ok(LogLevel::Info),
            "warn" | "warning" => Ok(LogLevel::Warn),
            "error" => Ok(LogLevel::Error),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestTimeouts {
    #[serde(default = "default_timeout_resolve")]
    pub resolve: u32,
    #[serde(default = "default_timeout_connect")]
    pub connect: u32,
    #[serde(default = "default_timeout_send")]
    pub send: u32,
    #[serde(default = "default_timeout_recv")]
    pub recv: u32,
}

fn default_timeout_resolve() -> u32 {
    5000
}
fn default_timeout_connect() -> u32 {
    5000
}
fn default_timeout_send() -> u32 {
    10000
}
fn default_timeout_recv() -> u32 {
    10000
}

impl Default for ManifestTimeouts {
    fn default() -> Self {
        Self {
            resolve: 5000,
            connect: 5000,
            send: 10000,
            recv: 10000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DonateSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_donate_url")]
    pub url: String,
    #[serde(default = "default_donate_interval_secs")]
    pub interval_secs: u32,
    #[serde(default = "default_donate_max_mints_per_cycle")]
    pub max_mints_per_cycle: u32,
    #[serde(default = "default_donate_min_mint_interval_ms")]
    pub min_mint_interval_ms: u32,
    #[serde(default)]
    pub max_mints_per_session: u32,
    #[serde(default = "default_donate_wanted_refresh_secs")]
    pub wanted_refresh_secs: u32,
}

fn default_true() -> bool {
    true
}
fn default_donate_url() -> String {
    "https://manifest.luastools.xyz".to_string()
}
fn default_donate_interval_secs() -> u32 {
    30
}
fn default_donate_max_mints_per_cycle() -> u32 {
    25
}
fn default_donate_min_mint_interval_ms() -> u32 {
    2000
}
fn default_donate_wanted_refresh_secs() -> u32 {
    300
}

impl Default for DonateSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            url: default_donate_url(),
            interval_secs: 30,
            max_mints_per_cycle: 25,
            min_mint_interval_ms: 2000,
            max_mints_per_session: 0,
            wanted_refresh_secs: 300,
        }
    }
}

impl DonateSettings {
    /// Clamps numeric settings to safe ranges matching Windows Config.cpp.
    pub fn sanitize(&mut self) {
        if self.url.trim().is_empty()
            || (!self.url.starts_with("http://") && !self.url.starts_with("https://"))
        {
            self.url = default_donate_url();
        }
        if self.interval_secs < 5 || self.interval_secs > 86400 {
            log::warn!(
                "[donate] interval_secs={} out of range [5, 86400], resetting to 30",
                self.interval_secs
            );
            self.interval_secs = 30;
        }
        if self.max_mints_per_cycle < 1 || self.max_mints_per_cycle > 500 {
            log::warn!(
                "[donate] max_mints_per_cycle={} out of range [1, 500], resetting to 25",
                self.max_mints_per_cycle
            );
            self.max_mints_per_cycle = 25;
        }
        if self.min_mint_interval_ms > 60000 {
            log::warn!(
                "[donate] min_mint_interval_ms={} out of range [0, 60000], resetting to 2000",
                self.min_mint_interval_ms
            );
            self.min_mint_interval_ms = 2000;
        }
        if self.max_mints_per_session > 100000 {
            log::warn!(
                "[donate] max_mints_per_session={} out of range [0, 100000], resetting to 0",
                self.max_mints_per_session
            );
            self.max_mints_per_session = 0;
        }
        if self.wanted_refresh_secs < 30 || self.wanted_refresh_secs > 86400 {
            log::warn!(
                "[donate] wanted_refresh_secs={} out of range [30, 86400], resetting to 300",
                self.wanted_refresh_secs
            );
            self.wanted_refresh_secs = 300;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InjectDll {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub when_cmdline: String,
    #[serde(default)]
    pub when_appids: Vec<u32>,
    #[serde(default)]
    pub all_games: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub library: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OstConfig {
    pub manifest_provider: String,
    pub manifest_timeouts: ManifestTimeouts,
    pub log_level: LogLevel,
    pub log_dir: PathBuf,
    pub lua_paths: Vec<PathBuf>,
    pub remote_url_templates: Vec<String>,
    pub stats_enable_api: bool,
    pub update_enabled: bool,
    pub donate: DonateSettings,
    pub inject_dlls: Vec<InjectDll>,
    pub cloud: CloudSettings,
}

impl Default for OstConfig {
    fn default() -> Self {
        Self {
            manifest_provider: "opensteamtool".to_string(),
            manifest_timeouts: ManifestTimeouts::default(),
            log_level: LogLevel::Debug,
            log_dir: PathBuf::from("opensteamtool"),
            lua_paths: Vec::new(),
            remote_url_templates: Vec::new(),
            stats_enable_api: true,
            update_enabled: true,
            donate: DonateSettings::default(),
            inject_dlls: Vec::new(),
            cloud: CloudSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadResult {
    pub applied: bool,
    pub lua_paths_changed: bool,
}

static ACTIVE_CONFIG: RwLock<Option<OstConfig>> = RwLock::new(None);
static CONFIG_MUTEX: Mutex<()> = Mutex::new(());

/// Helper raw TOML representation for parsing `opensteamtool.toml`.
#[derive(Debug, Deserialize)]
struct TomlConfigRaw {
    #[serde(default)]
    manifest: Option<TomlManifestRaw>,
    #[serde(default)]
    log: Option<TomlLogRaw>,
    #[serde(default)]
    lua: Option<TomlLuaRaw>,
    #[serde(default)]
    remote: Option<TomlRemoteRaw>,
    #[serde(default)]
    stats: Option<TomlStatsRaw>,
    #[serde(default)]
    update: Option<TomlUpdateRaw>,
    #[serde(default)]
    donate: Option<DonateSettings>,
    #[serde(default)]
    inject: Option<Vec<InjectDll>>,
    #[serde(default)]
    cloud: Option<CloudSettings>,
}

#[derive(Debug, Deserialize)]
struct TomlManifestRaw {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    timeout_resolve_ms: Option<u32>,
    #[serde(default)]
    timeout_connect_ms: Option<u32>,
    #[serde(default)]
    timeout_send_ms: Option<u32>,
    #[serde(default)]
    timeout_recv_ms: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct TomlLogRaw {
    #[serde(default)]
    level: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TomlLuaRaw {
    #[serde(default)]
    paths: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum TomlRemoteUrlTemplate {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Debug, Deserialize)]
struct TomlRemoteRaw {
    #[serde(default)]
    url_template: Option<TomlRemoteUrlTemplate>,
}

#[derive(Debug, Deserialize)]
struct TomlStatsRaw {
    #[serde(default)]
    enable_api: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct TomlUpdateRaw {
    #[serde(default)]
    enabled: Option<bool>,
}

/// Parse TOML content into an `OstConfig` snapshot with proper directory defaults.
pub fn parse_toml_content(content: &str, base_path: &Path) -> Result<OstConfig, String> {
    let raw: TomlConfigRaw = toml::from_str(content).map_err(|e| e.to_string())?;

    let mut config = OstConfig::default();
    let parent = base_path.parent().unwrap_or_else(|| Path::new("."));
    config.log_dir = parent.join("opensteamtool");

    if let Some(m) = raw.manifest {
        if let Some(url) = m.url {
            config.manifest_provider = url;
        }
        if let Some(t) = m.timeout_resolve_ms {
            config.manifest_timeouts.resolve = t;
        }
        if let Some(t) = m.timeout_connect_ms {
            config.manifest_timeouts.connect = t;
        }
        if let Some(t) = m.timeout_send_ms {
            config.manifest_timeouts.send = t;
        }
        if let Some(t) = m.timeout_recv_ms {
            config.manifest_timeouts.recv = t;
        }
    }

    if let Some(l) = raw.log {
        if let Some(level_str) = l.level {
            if let Ok(lvl) = LogLevel::from_str(&level_str) {
                config.log_level = lvl;
            }
        }
    }

    if let Some(lua) = raw.lua {
        if let Some(paths) = lua.paths {
            config.lua_paths = paths.into_iter().map(PathBuf::from).collect();
        }
    }

    if let Some(r) = raw.remote {
        if let Some(tmpl) = r.url_template {
            match tmpl {
                TomlRemoteUrlTemplate::Single(s) => config.remote_url_templates = vec![s],
                TomlRemoteUrlTemplate::Multiple(v) => config.remote_url_templates = v,
            }
        }
    }

    if let Some(s) = raw.stats {
        if let Some(api) = s.enable_api {
            config.stats_enable_api = api;
        }
    }

    if let Some(u) = raw.update {
        if let Some(en) = u.enabled {
            config.update_enabled = en;
        }
    }

    if let Some(mut d) = raw.donate {
        d.sanitize();
        config.donate = d;
    }

    if let Some(inj) = raw.inject {
        config.inject_dlls = inj;
    }

    if let Some(c) = raw.cloud {
        config.cloud = c;
    }

    Ok(config)
}

/// Load configuration from `opensteamtool.toml` at `config_path`.
/// If the file does not exist, defaults are used.
/// If the file exists but has syntax errors, previous configuration is retained.
pub fn load(config_path: &Path) -> LoadResult {
    let _guard = CONFIG_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

    if !config_path.exists() {
        log::info!(
            "[OstConfig] Config file {:?} not found, applying defaults",
            config_path
        );
        let mut snapshot = OstConfig::default();
        let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
        snapshot.log_dir = parent.join("opensteamtool");

        return apply_snapshot_locked(snapshot);
    }

    let content = match fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(e) => {
            log::warn!(
                "[OstConfig] Failed to read {:?}: {}, keeping previous valid config",
                config_path,
                e
            );
            return LoadResult {
                applied: false,
                lua_paths_changed: false,
            };
        }
    };

    match parse_toml_content(&content, config_path) {
        Ok(snapshot) => {
            log::info!(
                "[OstConfig] Loaded config from {:?}: manifest.url={} log.level={} lua.paths={} donate.enabled={}",
                config_path,
                snapshot.manifest_provider,
                snapshot.log_level,
                snapshot.lua_paths.len(),
                snapshot.donate.enabled
            );
            apply_snapshot_locked(snapshot)
        }
        Err(e) => {
            log::warn!(
                "[OstConfig] Config parse error in {:?}: {}, keeping previous valid config",
                config_path,
                e
            );
            LoadResult {
                applied: false,
                lua_paths_changed: false,
            }
        }
    }
}

fn apply_snapshot_locked(snapshot: OstConfig) -> LoadResult {
    let mut current = ACTIVE_CONFIG.write().unwrap_or_else(|e| e.into_inner());
    let lua_paths_changed = match current.as_ref() {
        Some(prev) => prev.lua_paths != snapshot.lua_paths,
        None => true,
    };

    *current = Some(snapshot);
    LoadResult {
        applied: true,
        lua_paths_changed,
    }
}

impl OstConfig {
    /// Get a clone of the currently active engine configuration.
    pub fn active() -> Self {
        get_active_config()
    }
}

/// Get a clone of the currently active engine configuration.
pub fn get_active_config() -> OstConfig {
    let reader = ACTIVE_CONFIG.read().unwrap_or_else(|e| e.into_inner());
    reader.clone().unwrap_or_default()
}

/// Get active donate settings.
pub fn get_donate_settings() -> DonateSettings {
    get_active_config().donate
}

/// Update donate settings dynamically at runtime.
pub fn set_donate_settings(settings: DonateSettings) {
    let mut writer = ACTIVE_CONFIG.write().unwrap_or_else(|e| e.into_inner());
    if let Some(ref mut cfg) = *writer {
        let mut sanitized = settings;
        sanitized.sanitize();
        cfg.donate = sanitized;
    } else {
        let mut cfg = OstConfig::default();
        let mut sanitized = settings;
        sanitized.sanitize();
        cfg.donate = sanitized;
        *writer = Some(cfg);
    }
}

/// Get manifest timeouts.
pub fn get_manifest_timeouts() -> ManifestTimeouts {
    get_active_config().manifest_timeouts
}

/// Get current log level.
pub fn get_log_level() -> LogLevel {
    get_active_config().log_level
}

/// Get active custom Lua search paths.
pub fn get_lua_paths() -> Vec<PathBuf> {
    get_active_config().lua_paths
}

/// Get remote url mirror templates.
pub fn get_remote_url_templates() -> Vec<String> {
    get_active_config().remote_url_templates
}

/// Get stats enable_api flag.
pub fn get_stats_enable_api() -> bool {
    get_active_config().stats_enable_api
}

/// Get auto-update enabled flag.
pub fn get_update_enabled() -> bool {
    get_active_config().update_enabled
}

/// Get Cloud save redirection settings.
pub fn get_cloud_settings() -> CloudSettings {
    get_active_config().cloud
}

/// Get list of DLL/SO process inject directives.
pub fn get_inject_dlls() -> Vec<InjectDll> {
    get_active_config().inject_dlls
}

/// Save an `OstConfig` snapshot to a TOML file.
pub fn save(config_path: &Path, config: &OstConfig) -> Result<(), String> {
    let mut toml_str = String::new();

    toml_str.push_str("[manifest]\n");
    toml_str.push_str(&format!("url = \"{}\"\n", config.manifest_provider));
    toml_str.push_str(&format!(
        "timeout_resolve_ms = {}\n",
        config.manifest_timeouts.resolve
    ));
    toml_str.push_str(&format!(
        "timeout_connect_ms = {}\n",
        config.manifest_timeouts.connect
    ));
    toml_str.push_str(&format!(
        "timeout_send_ms = {}\n",
        config.manifest_timeouts.send
    ));
    toml_str.push_str(&format!(
        "timeout_recv_ms = {}\n\n",
        config.manifest_timeouts.recv
    ));

    toml_str.push_str("[log]\n");
    toml_str.push_str(&format!("level = \"{}\"\n\n", config.log_level));

    toml_str.push_str("[lua]\n");
    let paths_str: Vec<String> = config
        .lua_paths
        .iter()
        .map(|p| format!("\"{}\"", p.display()))
        .collect();
    toml_str.push_str(&format!("paths = [{}]\n\n", paths_str.join(", ")));

    toml_str.push_str("[remote]\n");
    let remotes_str: Vec<String> = config
        .remote_url_templates
        .iter()
        .map(|r| format!("\"{}\"", r))
        .collect();
    toml_str.push_str(&format!(
        "url_template = [{}]\n\n",
        remotes_str.join(", ")
    ));

    toml_str.push_str("[stats]\n");
    toml_str.push_str(&format!("enable_api = {}\n\n", config.stats_enable_api));

    toml_str.push_str("[update]\n");
    toml_str.push_str(&format!("enabled = {}\n\n", config.update_enabled));

    toml_str.push_str("[donate]\n");
    toml_str.push_str(&format!("enabled = {}\n", config.donate.enabled));
    toml_str.push_str(&format!("url = \"{}\"\n", config.donate.url));
    toml_str.push_str(&format!(
        "interval_secs = {}\n",
        config.donate.interval_secs
    ));
    toml_str.push_str(&format!(
        "max_mints_per_cycle = {}\n",
        config.donate.max_mints_per_cycle
    ));
    toml_str.push_str(&format!(
        "min_mint_interval_ms = {}\n",
        config.donate.min_mint_interval_ms
    ));
    toml_str.push_str(&format!(
        "max_mints_per_session = {}\n",
        config.donate.max_mints_per_session
    ));
    toml_str.push_str(&format!(
        "wanted_refresh_secs = {}\n\n",
        config.donate.wanted_refresh_secs
    ));

    toml_str.push_str("[cloud]\n");
    toml_str.push_str(&format!("enabled = {}\n", config.cloud.enabled));
    toml_str.push_str(&format!("library = \"{}\"\n\n", config.cloud.library));

    for inj in &config.inject_dlls {
        toml_str.push_str("[[inject]]\n");
        toml_str.push_str(&format!("path = \"{}\"\n", inj.path));
        toml_str.push_str(&format!("when_cmdline = \"{}\"\n", inj.when_cmdline));
        let aids: Vec<String> = inj.when_appids.iter().map(|a| a.to_string()).collect();
        toml_str.push_str(&format!("when_appids = [{}]\n", aids.join(", ")));
        toml_str.push_str(&format!("all_games = {}\n\n", inj.all_games));
    }

    if let Some(parent) = config_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(config_path, toml_str).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ost_config_parse_full() {
        let toml_data = r#"
[manifest]
url = "gmrc.wudrm.com"
timeout_resolve_ms = 4000
timeout_connect_ms = 4500
timeout_send_ms = 8000
timeout_recv_ms = 9000

[log]
level = "trace"

[lua]
paths = ["/tmp/stplugin1", "/tmp/stplugin2"]

[remote]
url_template = ["https://mirror1.example.com/{depot}/{gid}"]

[stats]
enable_api = false

[update]
enabled = false

[donate]
enabled = true
url = "https://custom.manifest.donor"
interval_secs = 45
max_mints_per_cycle = 30
min_mint_interval_ms = 2500
max_mints_per_session = 100
wanted_refresh_secs = 600

[cloud]
enabled = true
library = "custom_cloud.so"

[[inject]]
path = "/opt/hook.so"
when_cmdline = "-inject"
when_appids = [480, 730]
all_games = false
"#;

        let base = PathBuf::from("/tmp/opensteamtool.toml");
        let cfg = parse_toml_content(toml_data, &base).expect("Failed to parse TOML");

        assert_eq!(cfg.manifest_provider, "gmrc.wudrm.com");
        assert_eq!(cfg.manifest_timeouts.resolve, 4000);
        assert_eq!(cfg.log_level, LogLevel::Trace);
        assert_eq!(cfg.lua_paths.len(), 2);
        assert!(!cfg.stats_enable_api);
        assert!(!cfg.update_enabled);
        assert!(cfg.donate.enabled);
        assert_eq!(cfg.donate.url, "https://custom.manifest.donor");
        assert_eq!(cfg.donate.interval_secs, 45);
        assert!(cfg.cloud.enabled);
        assert_eq!(cfg.inject_dlls.len(), 1);
        assert_eq!(cfg.inject_dlls[0].path, "/opt/hook.so");
    }

    #[test]
    fn test_ost_config_defaults_and_clamps() {
        let mut donate = DonateSettings {
            enabled: true,
            url: "".to_string(),
            interval_secs: 1,
            max_mints_per_cycle: 9999,
            min_mint_interval_ms: 999999,
            max_mints_per_session: 9999999,
            wanted_refresh_secs: 10,
        };
        donate.sanitize();
        assert_eq!(donate.url, "https://manifest.luastools.xyz");
        assert_eq!(donate.interval_secs, 30);
        assert_eq!(donate.max_mints_per_cycle, 25);
        assert_eq!(donate.min_mint_interval_ms, 2000);
        assert_eq!(donate.wanted_refresh_secs, 300);
    }
}
