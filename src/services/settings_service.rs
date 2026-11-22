//! Persistent application settings (`~/.config/luatools/settings.json`).

use std::fs;
use std::path::PathBuf;

use crate::models::config::{default_app_settings_path, AppSettings};

#[derive(Debug, Clone)]
pub struct SettingsService {
    pub config_path: PathBuf,
    pub settings: AppSettings,
}

impl Default for SettingsService {
    fn default() -> Self {
        Self::new(None)
    }
}

impl SettingsService {
    pub fn new(config_path: Option<PathBuf>) -> Self {
        let path = config_path.unwrap_or_else(default_app_settings_path);
        let settings = Self::load_from(&path);
        Self {
            config_path: path,
            settings,
        }
    }

    fn load_from(path: &PathBuf) -> AppSettings {
        if let Ok(raw) = fs::read_to_string(path) {
            if let Ok(parsed) = serde_json::from_str::<AppSettings>(&raw) {
                return parsed;
            }
        }
        AppSettings::default()
    }

    pub fn reload(&mut self) {
        self.settings = Self::load_from(&self.config_path);
    }

    pub fn save(&self) -> bool {
        if let Some(parent) = self.config_path.parent() {
            if fs::create_dir_all(parent).is_err() {
                return false;
            }
        }
        let Ok(json) = serde_json::to_string_pretty(&self.settings) else {
            return false;
        };
        let tmp = self.config_path.with_extension("tmp");
        if fs::write(&tmp, json).is_err() {
            return false;
        }
        fs::rename(&tmp, &self.config_path).is_ok()
    }

    pub fn record_loaded_app(&mut self, app_id: u32) {
        self.settings.loaded_app_ids.retain(|&id| id != app_id);
        self.settings.loaded_app_ids.insert(0, app_id);
        self.settings.loaded_app_ids.truncate(20);
        let _ = self.save();
    }
}
