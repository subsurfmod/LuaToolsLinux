//! Configuration management for SteamAutoPatch

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Base data directory
    pub data_dir: PathBuf,
    /// Temporary directory
    pub temp_dir: PathBuf,
    /// Steam installation root
    pub steam_root: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let base = home.join(".local/share/luatools");
        Self {
            data_dir: base.clone(),
            temp_dir: base.join("temp"),
            steam_root: None,
        }
    }
}

impl Config {
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn temp_dir(&self) -> &Path {
        &self.temp_dir
    }

    pub fn steam_root(&self) -> Option<&Path> {
        self.steam_root.as_deref()
    }
}

lazy_static::lazy_static! {
    static ref GLOBAL_CONFIG: std::sync::RwLock<Config> = std::sync::RwLock::new(Config::default());
}

pub fn get_config() -> Config {
    GLOBAL_CONFIG.read().unwrap().clone()
}

pub fn set_config(config: Config) {
    *GLOBAL_CONFIG.write().unwrap() = config;
}
