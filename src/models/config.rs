//! Configuration models for LuaTools Linux (`settings.json`) and SLSsteam (`config.yaml`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub fn default_slssteam_config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("SLSsteam")
        .join("config.yaml")
}

pub fn default_app_settings_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("luatools")
        .join("settings.json")
}

pub fn default_luatools_data_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local")
        .join("share")
        .join("luatools")
}

pub fn default_luatools_cache_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".cache")
        .join("luatools")
}

#[derive(Debug, Clone)]
pub struct SlssteamConfig {
    pub path: PathBuf,
    // Scalar settings required by AceSLS/SLSsteam CConfig::loadSettings
    pub log_levels: u32,
    pub api: bool,
    pub disable_family_share_lock: bool,
    pub use_whitelist: bool,
    pub max_schema_tries: u32,
    pub smart_tickets: u32,
    pub safe_mode: bool,
    pub warn_hash_missmatch: bool,
    pub notify_init: bool,
    pub fake_name: String,
    pub fake_email: String,
    pub fake_wallet_balance: i32,
    pub plugins: bool,
    pub disable_cloud: bool,
    pub disable_updates: bool,
    pub dump_client_interfaces: bool,
    pub extended_logging: bool,
    // Sequence settings required by AceSLS/SLSsteam
    pub app_ids: Vec<u32>,
    pub fake_offline: Vec<u32>,
    pub depot_blacklist: Vec<u32>,
    pub additional_apps: Vec<u32>,
    // Map & structured settings required by AceSLS/SLSsteam
    pub fake_app_ids: BTreeMap<u32, u32>,
    pub manifest_overrides: BTreeMap<u32, String>,
    pub app_tokens: BTreeMap<u32, String>,
    pub cd_keys: BTreeMap<u32, String>,
    pub game_titles: BTreeMap<u32, String>,
    pub subscription_timestamps: BTreeMap<u32, u32>,
    pub steam_id_overrides: BTreeMap<u32, u64>,
    pub steam_id_override: Option<u64>,
    pub launch_options: BTreeMap<u32, String>,
    pub idle_status_app_id: u32,
    pub idle_status_title: String,
    pub dlc_data: BTreeMap<u32, BTreeMap<u32, String>>,
    pub denuvo_games: BTreeMap<u64, Vec<u32>>,
    // Companion ACCELA / LuaTools extensions (ignored by SLSsteam)
    pub depot_keys: BTreeMap<u32, String>,
    pub process_map: BTreeMap<u32, String>,
    pub force_denuvo: Vec<u32>,
    pub pinned_apps: Vec<u32>,
    pub play_not_owned: bool,
    pub limit_downloads: bool,
    pub workshop_path: String,
    pub extra_fields: BTreeMap<String, serde_yml::Value>,
}

impl Default for SlssteamConfig {
    fn default() -> Self {
        Self {
            path: default_slssteam_config_path(),
            log_levels: 0xff,
            api: false,
            disable_family_share_lock: false,
            use_whitelist: false,
            max_schema_tries: 10,
            smart_tickets: 0x1,
            safe_mode: false,
            warn_hash_missmatch: false,
            notify_init: true,
            fake_name: String::new(),
            fake_email: String::new(),
            fake_wallet_balance: 0,
            plugins: false,
            disable_cloud: true,
            disable_updates: true,
            dump_client_interfaces: false,
            extended_logging: false,
            app_ids: Vec::new(),
            fake_offline: Vec::new(),
            depot_blacklist: Vec::new(),
            additional_apps: Vec::new(),
            fake_app_ids: BTreeMap::new(),
            manifest_overrides: BTreeMap::new(),
            app_tokens: BTreeMap::new(),
            cd_keys: BTreeMap::new(),
            game_titles: BTreeMap::new(),
            subscription_timestamps: BTreeMap::new(),
            steam_id_overrides: BTreeMap::new(),
            steam_id_override: None,
            launch_options: BTreeMap::new(),
            idle_status_app_id: 0,
            idle_status_title: String::new(),
            dlc_data: BTreeMap::new(),
            denuvo_games: BTreeMap::new(),
            depot_keys: BTreeMap::new(),
            process_map: BTreeMap::new(),
            force_denuvo: Vec::new(),
            pinned_apps: Vec::new(),
            play_not_owned: true,
            limit_downloads: false,
            workshop_path: String::new(),
            extra_fields: BTreeMap::new(),
        }
    }
}

impl SlssteamConfig {
    pub fn exists(&self) -> bool {
        self.path.exists()
    }
}

fn default_true() -> bool {
    true
}

fn default_http_port() -> u16 {
    6767
}

fn default_lang() -> String {
    "en".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub steam_path_override: String,
    #[serde(default = "default_true")]
    pub auto_update_apps: bool,
    #[serde(default = "default_true")]
    pub fast_fetch: bool,
    #[serde(default = "default_true")]
    pub donate_keys: bool,
    #[serde(default)]
    pub hubcap_api_key: String,
    #[serde(default, alias = "cef_injector_enabled")]
    pub cef_bridge_enabled: bool,
    #[serde(default = "default_true")]
    pub http_server_enabled: bool,
    #[serde(default = "default_http_port")]
    pub http_server_port: u16,
    #[serde(default = "default_lang")]
    pub language: String,
    #[serde(default)]
    pub loaded_app_ids: Vec<u32>,
    #[serde(default = "default_true")]
    pub check_updates_on_startup: bool,
    #[serde(default)]
    pub git_remote_url: String,
    #[serde(default)]
    pub update_repo_dir: String,
    #[serde(default)]
    pub steam_username: String,
    #[serde(default = "default_true")]
    pub telemetry_enabled: bool,
    #[serde(default)]
    pub telemetry_consent_seen: bool,
    #[serde(default)]
    pub telemetry_endpoint_url: String,
    #[serde(default)]
    pub telemetry_install_id: String,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub close_to_tray: bool,
    #[serde(default)]
    pub minimize_to_tray: bool,
    #[serde(default)]
    pub doh_enabled: bool,
    #[serde(default)]
    pub doh_endpoint: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            steam_path_override: String::new(),
            auto_update_apps: true,
            fast_fetch: true,
            donate_keys: true,
            hubcap_api_key: String::new(),
            cef_bridge_enabled: false,
            http_server_enabled: true,
            http_server_port: 6767,
            language: "en".to_string(),
            loaded_app_ids: Vec::new(),
            check_updates_on_startup: true,
            git_remote_url: String::new(),
            update_repo_dir: String::new(),
            steam_username: String::new(),
            telemetry_enabled: true,
            telemetry_consent_seen: false,
            telemetry_endpoint_url: String::new(),
            telemetry_install_id: String::new(),
            autostart: false,
            close_to_tray: false,
            minimize_to_tray: false,
            doh_enabled: false,
            doh_endpoint: String::new(),
        }
    }
}
