//! Persistent application settings (`~/.config/luatools/settings.json`) and XDG OS login autostart (`~/.config/autostart/luatools.desktop`).

use std::fs;
use std::path::{Path, PathBuf};

use crate::models::config::{default_app_settings_path, AppSettings};
use crate::services::credential_encryption::{
    decrypt_secret_string, encrypt_secret_string, SECRET_PREFIX,
};
use crate::services::doh_resolver::DohResolver;

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
        let (mut settings, needs_key_migration) = Self::load_from_with_migration(&path);
        if let Some(ref ap) = autostart_path {
            if is_default_config || ap.exists() {
                settings.autostart = ap.is_file();
            }
        }
        if is_default_config {
            DohResolver::configure_global(settings.doh_enabled, &settings.doh_endpoint);
        }
        let svc = Self {
            config_path: path,
            autostart_path,
            settings,
        };
        if needs_key_migration {
            let _ = svc.save();
        }
        svc
    }

    pub fn with_autostart_path(config_path: PathBuf, autostart_path: PathBuf) -> Self {
        let (mut settings, needs_key_migration) = Self::load_from_with_migration(&config_path);
        settings.autostart = autostart_path.is_file();
        let svc = Self {
            config_path,
            autostart_path: Some(autostart_path),
            settings,
        };
        if needs_key_migration {
            let _ = svc.save();
        }
        svc
    }

    fn load_from_with_migration(path: &Path) -> (AppSettings, bool) {
        if let Ok(raw) = fs::read_to_string(path) {
            if let Ok(mut parsed) = serde_json::from_str::<AppSettings>(&raw) {
                let key = parsed.hubcap_api_key.trim().to_string();
                if key.starts_with(SECRET_PREFIX) {
                    match decrypt_secret_string(&key) {
                        Ok(plain) => parsed.hubcap_api_key = plain,
                        Err(e) => {
                            log::warn!("Failed to decrypt stored hubcap_api_key: {}", e);
                            parsed.hubcap_api_key.clear();
                        }
                    }
                    return (parsed, false);
                }
                let had_plaintext_key = !key.is_empty();
                return (parsed, had_plaintext_key);
            }
        }
        (AppSettings::default(), false)
    }

    pub fn reload(&mut self) {
        let (settings, needs_key_migration) = Self::load_from_with_migration(&self.config_path);
        self.settings = settings;
        let _ = self.sync_autostart_from_disk();
        if needs_key_migration {
            let _ = self.save();
        }
    }

    pub fn save(&self) -> bool {
        if let Some(parent) = self.config_path.parent() {
            if fs::create_dir_all(parent).is_err() {
                return false;
            }
        }
        // LT-40: Encrypt `hubcap_api_key` at rest (`enc:v1:...`) without mutating in-memory plaintext
        let mut disk_settings = self.settings.clone();
        let raw_key = disk_settings.hubcap_api_key.trim().to_string();
        if !raw_key.is_empty() && !raw_key.starts_with(SECRET_PREFIX) {
            match encrypt_secret_string(&raw_key) {
                Ok(enc) => disk_settings.hubcap_api_key = enc,
                Err(e) => {
                    log::warn!("Could not encrypt hubcap_api_key before saving settings: {}", e);
                }
            }
        }
        let Ok(json) = serde_json::to_string_pretty(&disk_settings) else {
            return false;
        };
        let tmp_path = self.config_path.with_extension("tmp");
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let Ok(mut f) = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp_path)
            else {
                return false;
            };
            if f.write_all(json.as_bytes()).is_err() || f.sync_all().is_err() {
                let _ = fs::remove_file(&tmp_path);
                return false;
            }
        }
        #[cfg(not(unix))]
        {
            if fs::write(&tmp_path, json).is_err() {
                return false;
            }
        }
        let ok = fs::rename(&tmp_path, &self.config_path).is_ok();
        if ok && self.config_path == default_app_settings_path() {
            DohResolver::configure_global(self.settings.doh_enabled, &self.settings.doh_endpoint);
        }
        ok
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

    #[test]
    fn test_hubcap_api_key_encrypted_at_rest_and_migrated_on_load() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("settings.json");

        // 1. Seed a legacy plaintext settings.json
        let plain_key = format!("smm_{}", "a1".repeat(48));
        fs::write(
            &cfg_path,
            serde_json::json!({ "hubcap_api_key": plain_key }).to_string(),
        )
        .unwrap();

        // 2. Loading transparently decrypts/preserves plaintext in memory AND migrates disk to enc:v1:
        let svc = SettingsService::new(Some(cfg_path.clone()));
        assert_eq!(svc.settings.hubcap_api_key, plain_key);

        let on_disk = fs::read_to_string(&cfg_path).unwrap();
        assert!(!on_disk.contains(&plain_key), "plaintext key must not remain on disk");
        assert!(on_disk.contains(SECRET_PREFIX));

        // 3. Reloading decrypts back to the exact original plaintext key
        let svc2 = SettingsService::new(Some(cfg_path));
        assert_eq!(svc2.settings.hubcap_api_key, plain_key);
    }
}
