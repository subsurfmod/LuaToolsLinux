//! Persistent application settings (`~/.config/luatools/settings.json`).

use std::fs;
use std::path::{Path, PathBuf};

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

    fn load_from(path: &Path) -> AppSettings {
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

    /// Path to user login autostart desktop entry: `~/.config/autostart/luatools.desktop`
    pub fn autostart_desktop_path() -> Option<PathBuf> {
        let home = dirs::home_dir()?;
        Some(home.join(".config/autostart/luatools.desktop"))
    }

    /// Checks if autostart desktop entry exists on the filesystem.
    pub fn is_autostart_enabled() -> bool {
        Self::autostart_desktop_path()
            .map(|p| p.is_file())
            .unwrap_or(false)
    }

    /// Enables or disables login autostart by writing or removing `~/.config/autostart/luatools.desktop` with `--minimized`.
    pub fn set_autostart(&mut self, enable: bool) -> Result<(), String> {
        self.settings.autostart = enable;
        let _ = self.save();

        let path = Self::autostart_desktop_path()
            .ok_or_else(|| "Could not determine home directory".to_string())?;

        if enable {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let exe = std::env::current_exe()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "luatools".to_string());
            let content = format!(
                "[Desktop Entry]\n\
                 Type=Application\n\
                 Version=1.0\n\
                 Name=LuaTools Linux\n\
                 Comment=Steam manifest and lua configuration manager\n\
                 Exec=\"{}\" --minimized\n\
                 Icon=luatools\n\
                 Terminal=false\n\
                 Categories=Game;Utility;\n\
                 X-GNOME-Autostart-enabled=true\n",
                exe
            );
            fs::write(&path, content).map_err(|e| e.to_string())?;
        } else if path.exists() {
            let _ = fs::remove_file(&path);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_autostart_desktop_path_resolution() {
        let p = SettingsService::autostart_desktop_path();
        assert!(p.is_some());
        let path = p.unwrap();
        assert!(path.ends_with(".config/autostart/luatools.desktop"));
    }
}
