//! Native Linux SteamAutoPatch — Goldberg Steam Emulator Deployment
//!
//! Provides native Rust SteamAutoPatch functionality for Linux & Proton titles:
//! - Deploys Goldberg Steam Emulator `.so` (`libsteam_api.so`, `steam_api64.so`, `steam_api.so`) and `.dll` files
//! - Manages `steam_settings/` (`steam_appid.txt`, `configs.user.ini`, `steam_interfaces.txt`)
//! - Extracts `steam_interfaces.txt` safely from binary ELF/PE Steamworks libraries
//! - Preserves `.bak` backups of original game binaries

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use configparser::ini::Ini;
use walkdir::WalkDir;

use crate::goldberg::GoldbergProvider;
use crate::interface_generator::InterfaceGenerator;

pub mod config;
pub mod goldberg;
pub mod interface_generator;

#[derive(Debug, Clone)]
pub struct AutoPatchConfig {
    /// Optional Steam AppID (automatically writes `steam_settings/steam_appid.txt` when set)
    pub app_id: Option<u32>,
    /// Path to `steam_settings` template directory
    pub config_path: PathBuf,
    /// Path to Goldberg emulator files
    pub goldberg_path: PathBuf,
    /// Target path to apply emulator (game directory or specific `.so`/`.dll` file)
    pub apply_path: PathBuf,
    /// Local save path override
    pub local_save: String,
    /// Whether to use local save override
    pub use_local_save: bool,
    /// Whether to generate `steam_interfaces.txt`
    pub generate_interfaces: bool,
    /// Force interface generation even if `steam_interfaces.txt` already exists
    pub force_interfaces: bool,
    /// Use experimental Goldberg build when available
    pub use_experimental: bool,
}

impl Default for AutoPatchConfig {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let base = home.join(".local/share/luatools");
        Self {
            app_id: None,
            config_path: base.join("steam_settings"),
            goldberg_path: base.join("goldberg"),
            apply_path: PathBuf::new(),
            local_save: "steam_settings/saves".to_string(),
            use_local_save: false,
            generate_interfaces: true,
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
    interface_gen: InterfaceGenerator,
}

impl SteamAutoPatch {
    pub fn new(config: AutoPatchConfig) -> Result<Self> {
        let goldberg = GoldbergProvider::new(&config.goldberg_path)?;
        let interface_gen = InterfaceGenerator::new();
        Ok(Self {
            config,
            goldberg,
            interface_gen,
        })
    }

    /// Check if Goldberg emulator files are available
    pub fn check_goldberg(&self) -> bool {
        self.goldberg.is_valid()
    }

    /// Apply Steam emulator to a game directory or specific `.so` / `.dll` file.
    pub fn apply(&self) -> Result<bool> {
        if !self.config.apply_path.exists() {
            return Err(anyhow::anyhow!(
                "Apply path does not exist: {}",
                self.config.apply_path.display()
            ));
        }

        if !self.goldberg.is_valid() {
            return Err(anyhow::anyhow!(
                "Goldberg emulator binaries not found at {}",
                self.goldberg.path().display()
            ));
        }

        if self.config.app_id.is_none() && !self.config.config_path.join("steam_appid.txt").exists()
        {
            return Err(anyhow::anyhow!(
                "steam_appid.txt not found in {} and no app_id provided.",
                self.config.config_path.display()
            ));
        }

        if self.config.apply_path.is_dir() {
            let patched = self.apply_to_folder()?;
            Ok(patched > 0)
        } else {
            let fname = self
                .config
                .apply_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            let patched = match fname {
                "steam_api.so" => self.apply_so(&self.config.apply_path, false)?,
                "steam_api64.so" => self.apply_so(&self.config.apply_path, true)?,
                "libsteam_api.so" => {
                    let is_64 = detect_elf_is_64bit(&self.config.apply_path);
                    self.apply_so(&self.config.apply_path, is_64)?
                }
                "steam_api.dll" => self.apply_dll(&self.config.apply_path, false)?,
                "steam_api64.dll" => self.apply_dll(&self.config.apply_path, true)?,
                _ => {
                    return Err(anyhow::anyhow!(
                        "Invalid target '{}': must be libsteam_api.so, steam_api64.so, steam_api.so, steam_api64.dll, steam_api.dll, or a game directory",
                        fname
                    ));
                }
            };
            Ok(patched)
        }
    }

    fn apply_so(&self, file_path: &Path, is_64bit: bool) -> Result<bool> {
        let src = self
            .goldberg
            .get_so_path(is_64bit, self.config.use_experimental);
        if !src.is_file() {
            return Err(anyhow::anyhow!(
                "Missing Goldberg {} Linux .so at {}",
                if is_64bit { "64-bit" } else { "32-bit" },
                src.display()
            ));
        }
        self.replace_and_configure(file_path, src)
    }

    fn apply_dll(&self, file_path: &Path, is_64bit: bool) -> Result<bool> {
        let src = self
            .goldberg
            .get_dll_path(is_64bit, self.config.use_experimental);
        if !src.is_file() {
            return Err(anyhow::anyhow!(
                "Missing Goldberg {} Windows .dll at {}",
                if is_64bit { "64-bit" } else { "32-bit" },
                src.display()
            ));
        }
        self.replace_and_configure(file_path, src)
    }

    fn replace_and_configure(&self, file_path: &Path, goldberg_src: &Path) -> Result<bool> {
        let backup_path = PathBuf::from(format!("{}.bak", file_path.display()));
        if !backup_path.exists() {
            fs::rename(file_path, &backup_path)?;
        }
        fs::copy(goldberg_src, file_path)?;

        let parent_dir = file_path.parent().unwrap_or_else(|| Path::new("."));
        let settings_dir = parent_dir.join("steam_settings");
        if self.config.config_path.is_dir() && !settings_dir.exists() {
            copy_dir(&self.config.config_path, &settings_dir)?;
        } else {
            fs::create_dir_all(&settings_dir)?;
        }

        if let Some(aid) = self.config.app_id {
            if aid > 0 {
                fs::write(settings_dir.join("steam_appid.txt"), format!("{}\n", aid))?;
            }
        }

        if self.config.generate_interfaces {
            let interfaces = self
                .interface_gen
                .generate_interfaces(&backup_path, self.config.force_interfaces)?;
            if !interfaces.is_empty() {
                self.interface_gen.write_interfaces_file(
                    &interfaces,
                    &settings_dir.join("steam_interfaces.txt"),
                )?;
            }
        }

        if self.config.use_local_save {
            self.configure_local_save(&settings_dir)?;
        }

        Ok(true)
    }

    fn apply_to_folder(&self) -> Result<usize> {
        let mut patched = 0usize;
        for entry in WalkDir::new(&self.config.apply_path)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if path.components().any(|c| c.as_os_str() == "steam_settings") {
                continue;
            }
            let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            patched += match fname {
                "steam_api.so" => usize::from(self.apply_so(path, false).unwrap_or(false)),
                "steam_api64.so" => usize::from(self.apply_so(path, true).unwrap_or(false)),
                "libsteam_api.so" => {
                    let is_64 = detect_elf_is_64bit(path);
                    usize::from(self.apply_so(path, is_64).unwrap_or(false))
                }
                "steam_api.dll" => usize::from(self.apply_dll(path, false).unwrap_or(false)),
                "steam_api64.dll" => usize::from(self.apply_dll(path, true).unwrap_or(false)),
                _ => 0,
            };
        }
        Ok(patched)
    }

    fn configure_local_save(&self, settings_dir: &Path) -> Result<()> {
        let ini_path = settings_dir.join("configs.user.ini");
        let mut ini = Ini::new();
        let _ = ini.load(&ini_path);

        let map = ini.get_mut_map();
        let section = map.entry("user::saves".to_string()).or_default();
        section.insert(
            "local_save_path".to_string(),
            Some(self.config.local_save.clone()),
        );

        ini.write(&ini_path)
            .map_err(|e| anyhow::anyhow!("Failed writing {}: {}", ini_path.display(), e))?;
        Ok(())
    }
}

fn detect_elf_is_64bit(path: &Path) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return true;
    };
    if bytes.len() >= 5 && &bytes[0..4] == b"\x7fELF" {
        return bytes[4] != 1;
    }
    true
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

    #[test]
    fn test_apply_patches_elf64_libsteam_api_and_extracts_interfaces() {
        let tmp = tempfile::tempdir().unwrap();
        let goldberg_dir = tmp.path().join("goldberg/regular/x64");
        fs::create_dir_all(&goldberg_dir).unwrap();
        fs::write(goldberg_dir.join("libsteam_api.so"), b"GOLDBERG_X64_SO").unwrap();

        let game_dir = tmp.path().join("game/bin");
        fs::create_dir_all(&game_dir).unwrap();
        let mut orig_elf = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0xff, 0x00];
        orig_elf.extend_from_slice(b"SteamClient021\0SteamApps008\0");
        fs::write(game_dir.join("libsteam_api.so"), &orig_elf).unwrap();

        let cfg = AutoPatchConfig {
            app_id: Some(1245620),
            config_path: tmp.path().join("template_settings"),
            goldberg_path: tmp.path().join("goldberg"),
            apply_path: tmp.path().join("game"),
            local_save: "steam_settings/saves".into(),
            use_local_save: true,
            generate_interfaces: true,
            force_interfaces: true,
            use_experimental: false,
        };
        let patcher = SteamAutoPatch::new(cfg).unwrap();
        assert!(patcher.check_goldberg());
        assert!(patcher.apply().unwrap());

        assert!(game_dir.join("libsteam_api.so.bak").is_file());
        assert_eq!(
            fs::read(game_dir.join("libsteam_api.so")).unwrap(),
            b"GOLDBERG_X64_SO"
        );
        assert_eq!(
            fs::read_to_string(game_dir.join("steam_settings/steam_appid.txt"))
                .unwrap()
                .trim(),
            "1245620"
        );
        let ifaces =
            fs::read_to_string(game_dir.join("steam_settings/steam_interfaces.txt")).unwrap();
        assert!(ifaces.contains("SteamClient021"));
        assert!(ifaces.contains("SteamApps008"));
        assert!(game_dir.join("steam_settings/configs.user.ini").is_file());
    }
}
