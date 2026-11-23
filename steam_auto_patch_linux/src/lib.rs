//! Native Linux SteamAutoPatch - Goldberg Steam Emulator Deployment
//!
//! This crate provides a native Linux implementation of SteamAutoPatch functionality:
//! - Deploys Goldberg Steam Emulator .so files (steam_api.so, steam_api64.so)
//! - Manages steam_settings configuration directory
//! - Generates steam_interfaces.txt from game binaries
//! - Handles local save paths for portable behavior

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result};
use configparser::ini::Ini;
use log;
use regex::Regex;
use walkdir::WalkDir;

use crate::goldberg::GoldbergProvider;

pub mod config;
pub mod goldberg;
pub mod interface_generator;

#[derive(Debug, Clone)]
pub struct AutoPatchConfig {
    /// Path to steam_settings configuration directory
    pub config_path: PathBuf,
    /// Path to Goldberg emulator files
    pub goldberg_path: PathBuf,
    /// Target path to apply emulator (game directory or specific .so file)
    pub apply_path: PathBuf,
    /// Local save path override
    pub local_save: String,
    /// Whether to use local save override
    pub use_local_save: bool,
    /// Whether to generate steam_interfaces.txt
    pub generate_interfaces: bool,
    /// Force interface generation even if not needed
    pub force_interfaces: bool,
    /// Use experimental Goldberg build
    pub use_experimental: bool,
}

impl Default for AutoPatchConfig {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let base = home.join(".local/share/luatools");
        Self {
            config_path: base.join("steam_settings"),
            goldberg_path: base.join("goldberg"),
            apply_path: PathBuf::new(),
            local_save: "steam_settings/saves".to_string(),
            use_local_save: false,
            generate_interfaces: false,
            force_interfaces: false,
            use_experimental: false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AutoPatchError {
    #[error("Goldberg emulator files not found at {0}")]
    GoldbergNotFound(String),
    #[error("Config not found at {0}")]
    ConfigNotFound(String),
    #[error("Invalid apply path: {0}")]
    InvalidPath(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("INI parsing error: {0}")]
    IniParse(String),
    #[error("Other error: {0}")]
    Other(String),
}

pub struct SteamAutoPatch {
    config: AutoPatchConfig,
    goldberg: GoldbergProvider,
    interface_names: Vec<String>,
}

impl SteamAutoPatch {
    pub fn new(config: AutoPatchConfig) -> Result<Self> {
        let goldberg = GoldbergProvider::new(&config.goldberg_path)?;
        let interface_names = Self::default_interface_names();
        Ok(Self {
            config,
            goldberg,
            interface_names,
        })
    }

    fn default_interface_names() -> Vec<String> {
        vec![
            "SteamClient".to_string(),
            "SteamGameServer".to_string(),
            "SteamGameServerStats".to_string(),
            "SteamUser".to_string(),
            "SteamFriends".to_string(),
            "SteamUtils".to_string(),
            "SteamMatchMaking".to_string(),
            "SteamMatchMakingServers".to_string(),
            "STEAMUSERSTATS_INTERFACE_VERSION".to_string(),
            "STEAMAPPS_INTERFACE_VERSION".to_string(),
            "SteamNetworking".to_string(),
            "STEAMREMOTESTORAGE_INTERFACE_VERSION".to_string(),
            "STEAMSCREENSHOTS_INTERFACE_VERSION".to_string(),
            "STEAMHTTP_INTERFACE_VERSION".to_string(),
            "STEAMUNIFIEDMESSAGES_INTERFACE_VERSION".to_string(),
            "STEAMUGC_INTERFACE_VERSION".to_string(),
            "STEAMAPPLIST_INTERFACE_VERSION".to_string(),
            "STEAMMUSIC_INTERFACE_VERSION".to_string(),
            "STEAMMUSICREMOTE_INTERFACE_VERSION".to_string(),
            "STEAMHTMLSURFACE_INTERFACE_VERSION_".to_string(),
            "STEAMINVENTORY_INTERFACE_V".to_string(),
            "STEAMAPPLIST_INTERFACE_VERSION".to_string(),
            "STEAMMUSIC_INTERFACE_VERSION".to_string(),
            "STEAMMUSICREMOTE_INTERFACE_VERSION".to_string(),
            "STEAMHTMLSURFACE_INTERFACE_VERSION_".to_string(),
            "STEAMINVENTORY_INTERFACE_V".to_string(),
            "STEAMAPPLIST_INTERFACE_VERSION".to_string(),
            "STEAMMUSIC_INTERFACE_VERSION".to_string(),
            "STEAMMUSICREMOTE_INTERFACE_VERSION".to_string(),
            "STEAMHTMLSURFACE_INTERFACE_VERSION_".to_string(),
            "STEAMINVENTORY_INTERFACE_V".to_string(),
            "SteamController".to_string(),
            "SteamMasterServerUpdater".to_string(),
            "STEAMVIDEO_INTERFACE_V".to_string(),
        ]
    }

    /// Check if Goldberg emulator files are available
    pub fn check_goldberg(&self) -> bool {
        self.goldberg.is_valid()
    }

    /// Apply Steam emulator to a game directory or specific .so file
    pub fn apply(&self) -> Result<bool> {
        if !self.config.apply_path.exists() {
            return Err(anyhow::anyhow!("Apply path does not exist: {}", self.config.apply_path.display()));
        }

        if !self.goldberg.is_valid() {
            return Err(anyhow::anyhow!("Goldberg emulator not found at {}", self.goldberg.path().display()));
        }

        if !self.config.config_path.join("steam_appid.txt").exists() {
            return Err(anyhow::anyhow!("steam_appid.txt not found in config. Generate emulator game info first."));
        }

        if self.config.apply_path.is_dir() {
            self.apply_to_folder()?;
        } else if self.config.apply_path.file_name().map(|s| s.to_string_lossy()) == Some("steam_api".into()) {
            self.apply_x86(&self.config.apply_path)?;
        } else if self.config.apply_path.file_name().map(|s| s.to_string_lossy()) == Some("steam_api64".into()) {
            self.apply_x64(&self.config.apply_path)?;
        } else {
            return Err(anyhow::anyhow!("Invalid file type: must be steam_api.so, steam_api64.so, or a directory"));
        }

        Ok(true)
    }

    fn apply_x64(&self, file_path: &Path) -> Result<()> {
        let backup_path = file_path.with_extension("so.bak");
        if backup_path.exists() {
            log::info!("Backup already exists, skipping: {}", file_path.display());
            return Ok(());
        }

        // Backup original
        fs::rename(file_path, &backup_path)?;

        // Copy Goldberg steam_api64.so
        let src = if self.config.use_experimental {
            self.goldberg.experimental_x64()
        } else {
            self.goldberg.regular_x64()
        };
        fs::copy(src, file_path)?;

        // Copy steam_settings if not exists
        let settings_dir = file_path.parent().unwrap().join("steam_settings");
        if !settings_dir.exists() {
            copy_dir(&self.config.config_path, &settings_dir)?;
        }

        // Generate interfaces if needed
        if self.config.generate_interfaces {
            self.generate_interfaces_file(&backup_path, self.config.force_interfaces)?;
        }

        // Handle local save
        if self.config.use_local_save {
            self.configure_local_save(&settings_dir)?;
        }

        Ok(())
    }

    fn apply_x86(&self, file_path: &Path) -> Result<()> {
        let backup_path = file_path.with_extension("so.bak");
        if backup_path.exists() {
            log::info!("Backup already exists, skipping: {}", file_path.display());
            return Ok(());
        }

        fs::rename(file_path, &backup_path)?;

        let src = if self.config.use_experimental {
            self.goldberg.experimental_x86()
        } else {
            self.goldberg.regular_x86()
        };
        fs::copy(src, file_path)?;

        let settings_dir = file_path.parent().unwrap().join("steam_settings");
        if !settings_dir.exists() {
            copy_dir(&self.config.config_path, &settings_dir)?;
        }

        if self.config.generate_interfaces {
            self.generate_interfaces_file(&backup_path, self.config.force_interfaces)?;
        }

        if self.config.use_local_save {
            self.configure_local_save(&settings_dir)?;
        }

        Ok(())
    }

    fn apply_to_folder(&self) -> Result<()> {
        for entry in WalkDir::new(&self.config.apply_path) {
            let entry = entry?;
            let path = entry.path();
            if path.file_name().map(|s| s == "steam_api.so").unwrap_or(false) {
                self.apply_x86(path)?;
            } else if path.file_name().map(|s| s == "steam_api64.so").unwrap_or(false) {
                self.apply_x64(path)?;
            }
        }
        Ok(())
    }

    fn generate_interfaces_file(&self, original_so: &Path, force: bool) -> Result<()> {
        let interfaces_path = original_so.parent().unwrap().join("steam_settings").join("steam_interfaces.txt");
        if interfaces_path.exists() && !force {
            log::debug!("steam_interfaces.txt already exists, skipping");
            return Ok(());
        }

        // Check file timestamp if not forced
        if !force {
            // In a real implementation, we'd check the binary signature timestamp
            // For now, we'll skip if not forced
        }

        let mut interfaces = HashSet::new();
        let dll_content = fs::read_to_string(&self.config.apply_path.join("steam_api64.so"))
            .or_else(|_| fs::read_to_string(&self.config.apply_path.join("steam_api.so")))
            .unwrap_or_default();

        for name in &self.interface_names {
            let regex = Regex::new(&format!(r"{}\d{{3}}", regex::escape(name)))?;
            for mat in regex.find_iter(&dll_content) {
                interfaces.insert(mat.as_str().to_string());
            }
        }

        let interfaces_path = self.config.apply_path.join("steam_settings").join("steam_interfaces.txt");
        fs::write(interfaces_path, interfaces.iter().map(|s| format!("{}\n", s)).collect::<String>())?;

        Ok(())
    }

    fn configure_local_save(&self, settings_dir: &Path) -> Result<()> {
        let ini_path = settings_dir.join("configs.user.ini");
        let mut ini = Ini::new();
        ini.load(&ini_path).unwrap_or_default();
        
        // Get mutable reference to the inner map
        let map = ini.get_mut_map();
        
        // Get or create the "user::saves" section
        let section = map.entry("user::saves".to_string()).or_insert_with(|| std::collections::HashMap::new());
        section.insert("local_save_path".to_string(), Some(self.config.local_save.clone()));
        
        ini.write(&ini_path)?;
        Ok(())
    }
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in WalkDir::new(src) {
        let entry = entry?;
        let rel = entry.path().strip_prefix(src)?;
        let dst_path = dst.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(dst_path)?;
        } else {
            if let Some(parent) = dst_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_default_config() {
        let config = AutoPatchConfig::default();
        assert!(!config.config_path.as_os_str().is_empty());
        assert!(!config.goldberg_path.as_os_str().is_empty());
    }

    #[test]
    fn test_copy_dir() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();

        fs::write(src.path().join("test.txt"), "hello").unwrap();
        fs::create_dir_all(src.path().join("subdir")).unwrap();
        fs::write(src.path().join("subdir").join("nested.txt"), "world").unwrap();

        copy_dir(src.path(), dst.path()).unwrap();

        assert!(dst.path().join("test.txt").exists());
        assert!(dst.path().join("subdir").join("nested.txt").exists());
    }
}