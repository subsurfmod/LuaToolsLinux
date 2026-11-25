//! Persistent application settings (`~/.config/luatools/settings.json`) and XDG OS login autostart (`~/.config/autostart/luatools.desktop`).

use std::fs;
use std::path::{Path, PathBuf};

use crate::models::config::{default_app_settings_path, AppSettings};

#[derive(Debug, Clone)]
pub struct SettingsService {
    pub config_path: PathBuf,
    pub autostart_path: Option<PathBuf>,
    pub settings: AppSettings,
}

impl Default for SettingsService {
    fn default() -> Self {
        Self::new(None)
    }
}

impl SettingsService {
    pub fn new(config_path: Option<PathBuf>) -> Self {
        let is_default_config = config_path.is_none();
        let path = config_path.unwrap_or_else(default_app_settings_path);
        let autostart_path = if is_default_config {
            Self::autostart_desktop_path()
        } else {
            path.parent().map(|p| p.join("autostart/luatools.desktop"))
        };
        let mut settings = Self::load_from(&path);
        if let Some(ref ap) = autostart_path {
            if is_default_config || ap.exists() {
                settings.autostart = ap.is_file();
            }
        }
        Self {
            config_path: path,
            autostart_path,
            settings,
        }
    }

    pub fn with_autostart_path(config_path: PathBuf, autostart_path: PathBuf) -> Self {
        let mut settings = Self::load_from(&config_path);
        settings.autostart = autostart_path.is_file();
        Self {
            config_path,
            autostart_path: Some(autostart_path),
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
        let _ = self.sync_autostart_from_disk();
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
        let tmp_path = self.config_path.with_extension("tmp");
        if fs::write(&tmp_path, json).is_err() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600));
        }
        fs::rename(&tmp_path, &self.config_path).is_ok()
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

    /// Effective autostart desktop path for this service instance.
    pub fn effective_autostart_path(&self) -> Option<PathBuf> {
        self.autostart_path
            .clone()
            .or_else(Self::autostart_desktop_path)
    }

    /// Checks if the global `~/.config/autostart/luatools.desktop` entry exists on the filesystem.
    pub fn is_autostart_enabled() -> bool {
        Self::autostart_desktop_path()
            .map(|p| p.is_file())
            .unwrap_or(false)
    }

    /// Checks if the autostart desktop entry for this instance exists on disk.
    pub fn has_autostart_entry(&self) -> bool {
        self.effective_autostart_path()
            .map(|p| p.is_file())
            .unwrap_or(false)
    }

    /// Reconciles `self.settings.autostart` with the actual `.desktop` file on disk.
    pub fn sync_autostart_from_disk(&mut self) -> bool {
        let on_disk = self.has_autostart_entry();
        if self.settings.autostart != on_disk {
            self.settings.autostart = on_disk;
        }
        on_disk
    }

    fn resolve_autostart_executable() -> String {
        let raw = crate::steam_bridge::ost_platform::trap::get_main_executable_path()
            .ok()
            .or_else(|| std::env::current_exe().ok())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "luatools".to_string());
        let cleaned = raw.strip_suffix(" (deleted)").unwrap_or(&raw).trim();
        if cleaned.is_empty() {
            "luatools".to_string()
        } else {
            cleaned.replace('\\', "\\\\").replace('"', "\\\"")
        }
    }

    pub fn build_autostart_desktop_entry(escaped_exe: &str) -> String {
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Version=1.0\n\
             Name=LuaTools Linux\n\
             Comment=Steam manifest and lua configuration manager\n\
             Exec=\"{}\" --minimized --tray-locked\n\
             Icon=luatools\n\
             Terminal=false\n\
             StartupNotify=false\n\
             Categories=Game;Utility;\n\
             X-GNOME-Autostart-enabled=true\n",
            escaped_exe
        )
    }

    /// Enables or disables login autostart by writing or removing `luatools.desktop` with `--minimized --tray-locked`.
    pub fn set_autostart(&mut self, enable: bool) -> Result<(), String> {
        let path = self
            .effective_autostart_path()
            .ok_or_else(|| "Could not determine autostart directory".to_string())?;

        if enable {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let exe = Self::resolve_autostart_executable();
            let content = Self::build_autostart_desktop_entry(&exe);
            let tmp_path = path.with_extension("desktop.tmp");
            fs::write(&tmp_path, content).map_err(|e| e.to_string())?;
            fs::rename(&tmp_path, &path).map_err(|e| e.to_string())?;
        } else if path.exists() {
            fs::remove_file(&path).map_err(|e| e.to_string())?;
        }

        self.settings.autostart = enable;
        let _ = self.save();
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

    #[test]
    fn test_set_autostart_enable_disable_and_disk_sync() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("settings.json");
        let desktop_path = tmp.path().join("autostart/luatools.desktop");

        let mut svc = SettingsService::with_autostart_path(cfg_path.clone(), desktop_path.clone());
        assert!(!svc.settings.autostart);
        assert!(!svc.has_autostart_entry());

        svc.set_autostart(true).expect("enable autostart");
        assert!(svc.settings.autostart);
        assert!(svc.has_autostart_entry());
        let text = fs::read_to_string(&desktop_path).expect("read desktop file");
        assert!(text.contains("[Desktop Entry]"));
        assert!(text.contains("--minimized --tray-locked"));
        assert!(text.contains("X-GNOME-Autostart-enabled=true"));

        // Re-opening from disk preserves synced autostart state
        let mut svc2 = SettingsService::with_autostart_path(cfg_path.clone(), desktop_path.clone());
        assert!(svc2.settings.autostart);

        // External removal (simulating CLI --autostart-disable or manual delete) reconciles cleanly
        svc2.set_autostart(false).expect("disable autostart");
        assert!(!svc2.settings.autostart);
        assert!(!desktop_path.exists());
        assert!(!svc.sync_autostart_from_disk());
        assert!(!svc.settings.autostart);
    }

    #[test]
    #[cfg(unix)]
    fn test_save_enforces_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("settings.json");
        let svc = SettingsService::new(Some(cfg_path.clone()));
        assert!(svc.save());
        let mode = fs::metadata(&cfg_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
