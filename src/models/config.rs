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
    pub fake_app_ids: BTreeMap<u32, u32>,
    pub app_tokens: BTreeMap<u32, String>,
    pub dlc_data: BTreeMap<u32, BTreeMap<u32, String>>,
    pub additional_apps: Vec<u32>,
    pub depot_keys: BTreeMap<u32, String>,
    pub play_not_owned: bool,
    pub limit_downloads: bool,
    pub workshop_path: String,
    pub extra_fields: BTreeMap<String, serde_yml::Value>,
}

impl Default for SlssteamConfig {
    fn default() -> Self {
        Self {
            path: default_slssteam_config_path(),
            fake_app_ids: BTreeMap::new(),
            app_tokens: BTreeMap::new(),
            dlc_data: BTreeMap::new(),
            additional_apps: Vec::new(),
            depot_keys: BTreeMap::new(),
            play_not_owned: false,
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
        }
    }
}
