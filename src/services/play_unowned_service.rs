//! PlayUnownedService — synchronizes unowned-game playback strictly via `SLSsteam` `config.yaml`
//! (`PlayNotOwnedGames`, `AdditionalApps`, and `FakeAppIds`) and cleans up any stale legacy
//! `stplug-in/play_unowned_games.lua` placebo script (LNX-03).

use std::fs;
use std::path::PathBuf;

use crate::models::config::default_slssteam_config_path;
use crate::models::steam::SteamInstallation;
use crate::services::slssteam_service::SlssteamService;

pub const PLUGIN_FILENAME: &str = "play_unowned_games.lua";

#[derive(Debug, Clone, Default)]
pub struct PlayUnownedService {
    pub steam_installation: Option<SteamInstallation>,
}

impl PlayUnownedService {
    pub fn new(steam_installation: Option<SteamInstallation>) -> Self {
        Self { steam_installation }
    }

    pub fn stplug_in_dir(&self) -> Option<PathBuf> {
        self.steam_installation
            .as_ref()
            .filter(|i| !i.stplug_in_path.as_os_str().is_empty())
            .map(|i| i.stplug_in_path.clone())
    }

    pub fn lua_path(&self) -> Option<PathBuf> {
        self.stplug_in_dir().map(|d| d.join(PLUGIN_FILENAME))
    }

    /// Returns true if `SLSsteam` `config.yaml` exists and has `PlayNotOwnedGames` enabled (LNX-03).
    pub fn is_installed(&self) -> bool {
        let cfg_path = default_slssteam_config_path();
        if !cfg_path.is_file() {
            return false;
        }
        SlssteamService::new(Some(cfg_path)).config.play_not_owned
    }

    pub fn is_enabled(&self, sls: &SlssteamService) -> bool {
        sls.config.play_not_owned
    }

    pub fn is_app_enabled(&self, app_id: u32, sls: &SlssteamService) -> bool {
        sls.config.play_not_owned && sls.config.additional_apps.contains(&app_id)
    }

    pub fn get_managed_app_ids(&self, sls: &SlssteamService) -> Vec<u32> {
        let mut ids = sls.config.additional_apps.clone();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Removes any stale legacy `stplug-in/play_unowned_games.lua` file (which contained
    /// unexecuted `steam_api` closures) and persists the authoritative `SLSsteam` `config.yaml`
    /// state (`PlayNotOwnedGames` + `AdditionalApps`) (LNX-03).
    pub fn sync_plugin_file(&self, sls: &SlssteamService) -> Result<Option<PathBuf>, String> {
        if let Some(stale_lua) = self.lua_path() {
            if stale_lua.exists() {
                let _ = fs::remove_file(&stale_lua);
            }
        }
        if !sls.save() {
            return Err(format!(
                "Failed to save SLSsteam config at {}",
                sls.config_path.display()
            ));
        }
        if sls.config.play_not_owned {
            Ok(Some(sls.config_path.clone()))
        } else {
            Ok(None)
        }
    }

    pub fn set_enabled(&self, enabled: bool, sls: &mut SlssteamService) -> Result<(), String> {
        sls.config.play_not_owned = enabled;
        self.sync_plugin_file(sls)?;
        Ok(())
    }

    pub fn enable_for_app(&self, app_id: u32, sls: &mut SlssteamService) -> Result<(), String> {
        sls.add_additional_app(app_id);
        // REMOVED: Do NOT set fake_app_id to 480!
        // AppID 480 spoofing is strictly reserved for multiplayer Online-Fixes.
        sls.config.play_not_owned = true;
        self.sync_plugin_file(sls)?;
        Ok(())
    }

    pub fn disable_for_app(&self, app_id: u32, sls: &mut SlssteamService) -> Result<(), String> {
        sls.remove_additional_app(app_id);
        if sls.get_fake_app_id(app_id) == Some(480) {
            sls.remove_fake_app_id(app_id);
        }
        self.sync_plugin_file(sls)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::steam::SteamLibrary;

    #[test]
    fn syncs_slssteam_yaml_and_removes_stale_legacy_play_unowned_lua() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Steam");
        let stplug = root.join("config/stplug-in");
        fs::create_dir_all(&stplug).unwrap();

        // Seed a stale legacy placebo play_unowned_games.lua file
        let legacy_lua = stplug.join(PLUGIN_FILENAME);
        fs::write(&legacy_lua, b"-- stale steam_api closure\n").unwrap();
        assert!(legacy_lua.is_file());

        let inst = SteamInstallation {
            root: root.clone(),
            libraries: vec![SteamLibrary::new(root.clone())],
            config_path: root.join("config"),
            stplug_in_path: stplug.clone(),
        };
        let sls_path = tmp.path().join("SLSsteam/config.yaml");
        let mut sls = SlssteamService::new(Some(sls_path.clone()));
        let svc = PlayUnownedService::new(Some(inst));

        svc.enable_for_app(1245620, &mut sls).unwrap();
        svc.enable_for_app(1091500, &mut sls).unwrap();
        assert!(svc.is_app_enabled(1245620, &sls));
        assert!(svc.is_app_enabled(1091500, &sls));
        // Stale legacy Lua placebo must be removed, never regenerated
        assert!(!legacy_lua.exists());

        // Real SLSsteam config.yaml must be written with PlayNotOwnedGames + AdditionalApps
        let yaml = fs::read_to_string(&sls_path).unwrap();
        assert!(yaml.contains("PlayNotOwnedGames: true"));
        assert!(yaml.contains("1245620"));
        assert!(yaml.contains("1091500"));

        // Disabling one app keeps the other app active
        svc.disable_for_app(1245620, &mut sls).unwrap();
        assert!(!svc.is_app_enabled(1245620, &sls));
        assert!(svc.is_app_enabled(1091500, &sls));
        assert!(!legacy_lua.exists());

        // Turning off PlayNotOwnedGames persists PlayNotOwnedGames: false
        svc.set_enabled(false, &mut sls).unwrap();
        assert!(!svc.is_enabled(&sls));
        let yaml_off = fs::read_to_string(&sls_path).unwrap();
        assert!(yaml_off.contains("PlayNotOwnedGames: false"));
    }
}
