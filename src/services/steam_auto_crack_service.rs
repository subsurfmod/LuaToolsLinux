//! SteamAutoCrackService — applies native Rust `steam_auto_patch_linux` Goldberg `.so`/`.dll`
//! deployment first when Goldberg libraries are staged, with automatic fallback to external
//! `SteamAutoPatch` (`SteamAutoCrack`) CLI via `ExternalToolsManager`.

use std::path::{Path, PathBuf};

use steam_auto_patch_linux::{AutoPatchConfig, SteamAutoPatch};

use crate::services::external_tools::ExternalToolsManager;

#[derive(Debug, Clone)]
pub struct SteamAutoCrackService {
    pub tools_manager: ExternalToolsManager,
}

impl SteamAutoCrackService {
    pub fn new(tools_manager: ExternalToolsManager) -> Self {
        Self { tools_manager }
    }

    pub fn goldberg_root(&self) -> PathBuf {
        self.tools_manager.goldberg_root()
    }

    pub fn settings_template_dir(&self) -> PathBuf {
        self.tools_manager
            .tools_dir
            .parent()
            .map(|p| p.join("steam_settings"))
            .unwrap_or_else(|| self.tools_manager.tools_dir.join("steam_settings"))
    }

    pub fn has_native_goldberg(&self) -> bool {
        let cfg = AutoPatchConfig {
            goldberg_path: self.goldberg_root(),
            ..Default::default()
        };
        SteamAutoPatch::new(cfg)
            .map(|p| p.check_goldberg())
            .unwrap_or(false)
    }

    pub fn is_available(&self) -> bool {
        self.has_native_goldberg() || self.tools_manager.inspect_tool("steamautopatch").installed
    }

    pub fn tool_path(&self) -> Option<PathBuf> {
        if self.has_native_goldberg() {
            Some(self.goldberg_root())
        } else {
            self.tools_manager.inspect_tool("steamautopatch").exe_path
        }
    }

    pub fn launch_for_app(&self, app_id: u32, install_path: &Path) -> Result<String, String> {
        if self.has_native_goldberg() {
            let cfg = AutoPatchConfig {
                app_id: if app_id > 0 { Some(app_id) } else { None },
                config_path: self.settings_template_dir(),
                goldberg_path: self.goldberg_root(),
                apply_path: install_path.to_path_buf(),
                local_save: "steam_settings/saves".into(),
                use_local_save: false,
                generate_interfaces: true,
                force_interfaces: false,
                use_experimental: false,
            };
            let patcher = SteamAutoPatch::new(cfg).map_err(|e| e.to_string())?;
            let changed = patcher.apply().map_err(|e| e.to_string())?;
            if changed {
                return Ok(format!(
                    "Applied native Linux Goldberg SteamAutoPatch to {}",
                    install_path.display()
                ));
            }
            return Ok(format!(
                "No Steam API binaries found under {} (nothing needed patching)",
                install_path.display()
            ));
        }

        self.tools_manager
            .launch_steamautopatch(app_id, install_path)
    }

    pub fn launch_for_folder(&self, folder: &Path) -> Result<String, String> {
        self.launch_for_app(0, folder)
    }
}

impl Default for SteamAutoCrackService {
    fn default() -> Self {
        Self {
            tools_manager: ExternalToolsManager::new(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn reports_uninstalled_and_returns_helpful_error_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = ExternalToolsManager::new(Some(tmp.path().join("tools")));
        let svc = SteamAutoCrackService::new(mgr);
        assert!(!svc.is_available());
        assert!(svc.tool_path().is_none());
        let err = svc.launch_for_folder(tmp.path()).unwrap_err();
        assert!(err.contains("SteamAutoPatch"));
    }

    #[test]
    fn applies_native_goldberg_when_staged() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = ExternalToolsManager::new(Some(tmp.path().join("tools")));
        let gb_x64 = tmp.path().join("goldberg/regular/x64");
        fs::create_dir_all(&gb_x64).unwrap();
        fs::write(gb_x64.join("libsteam_api.so"), b"NATIVE_GBE_SO").unwrap();

        let game_dir = tmp.path().join("EldenRing");
        fs::create_dir_all(&game_dir).unwrap();
        let mut elf = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0];
        elf.extend_from_slice(b"SteamUser021\0");
        fs::write(game_dir.join("libsteam_api.so"), &elf).unwrap();

        let svc = SteamAutoCrackService::new(mgr);
        assert!(svc.is_available());
        let msg = svc.launch_for_app(1245620, &game_dir).unwrap();
        assert!(msg.contains("Applied native Linux Goldberg"));
        assert_eq!(
            fs::read(game_dir.join("libsteam_api.so")).unwrap(),
            b"NATIVE_GBE_SO"
        );
        assert!(game_dir
            .join("steam_settings/steam_interfaces.txt")
            .is_file());
    }
}
