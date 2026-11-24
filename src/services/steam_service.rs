//! Steam discovery & multi-library game scanner.

use std::fs;
use std::path::{Path, PathBuf};

use crate::models::steam::{
    default_steam_candidates, discover_libraries, get_game_name_from_acf, get_install_dir_from_acf,
    scan_installed_games, SteamInstallation, SteamLibrary,
};

pub fn detect_steam_install(override_path: Option<&str>) -> Option<SteamInstallation> {
    let mut candidates = Vec::new();
    if let Some(ov) = override_path.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(expanded) = expand_tilde(ov) {
            candidates.push(expanded);
        }
    }
    candidates.extend(default_steam_candidates());

    for cand in candidates {
        if !cand.exists() {
            continue;
        }
        let root = fs::canonicalize(&cand).unwrap_or(cand);
        let has_markers = root.join("steam.sh").exists()
            || root.join("steamapps").exists()
            || root.join("config").exists();
        if has_markers {
            let libraries = discover_libraries(&root);
            return Some(SteamInstallation {
                config_path: root.join("config"),
                stplug_in_path: root.join("config").join("stplug-in"),
                root,
                libraries,
            });
        }
    }
    None
}

pub fn expand_tilde(path_str: &str) -> Option<PathBuf> {
    if let Some(rest) = path_str.strip_prefix("~/") {
        return Some(dirs::home_dir()?.join(rest));
    }
    if path_str == "~" {
        return dirs::home_dir();
    }
    Some(PathBuf::from(path_str))
}

#[derive(Debug, Clone, Default)]
pub struct SteamService {
    pub installation: Option<SteamInstallation>,
}

impl SteamService {
    pub fn new() -> Self {
        Self { installation: None }
    }

    pub fn detect(&mut self, override_path: Option<&str>) -> Option<SteamInstallation> {
        self.installation = detect_steam_install(override_path);
        self.installation.clone()
    }

    pub fn is_available(&self) -> bool {
        self.installation
            .as_ref()
            .map(SteamInstallation::is_valid)
            .unwrap_or(false)
    }

    pub fn libraries(&self) -> &[SteamLibrary] {
        self.installation
            .as_ref()
            .map(|i| i.libraries.as_slice())
            .unwrap_or(&[])
    }

    pub fn scan_all_games(&self) -> Vec<u32> {
        let mut all = Vec::new();
        for lib in self.libraries() {
            all.extend(scan_installed_games(lib));
        }
        all.sort_unstable();
        all.dedup();
        all
    }

    pub fn get_game_name(&self, app_id: u32) -> Option<String> {
        for lib in self.libraries() {
            if let Some(name) = get_game_name_from_acf(lib, app_id) {
                return Some(name);
            }
        }
        None
    }

    pub fn find_install_dir(&self, app_id: u32) -> Option<PathBuf> {
        for lib in self.libraries() {
            if let Some(dir) = get_install_dir_from_acf(lib, app_id) {
                if dir.exists() {
                    return Some(dir);
                }
            }
        }
        None
    }

    pub fn list_lua_app_ids(stplug_dir: &Path) -> Vec<u32> {
        let mut ids = Vec::new();
        if let Ok(entries) = fs::read_dir(stplug_dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("lua") {
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        if let Ok(aid) = stem.parse::<u32>() {
                            ids.push(aid);
                        }
                    }
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Terminates all running Steam client, webhelper, overlay, and Steam Runtime processes cleanly.
    /// Sends `SIGTERM` first, waits up to 2 seconds for clean exit, then sends `SIGKILL` to any
    /// stubborn processes.
    pub fn terminate_steam_instances() -> Result<usize, String> {
        crate::steam_bridge::preload_loader::kill_all_steam_instances()
    }

    /// Checks whether any Steam client or helper process is currently running.
    pub fn is_steam_running() -> bool {
        crate::steam_bridge::preload_loader::find_steam_pid().is_some()
    }

    /// Cleanly terminates any running Steam instances and relaunches Steam via `steam-sls`
    /// (with `LD_AUDIT=SLSsteam.so`) so SLSsteam is retained without 32-bit / 64-bit LD_PRELOAD crashes.
    pub fn relaunch_steam_sls(override_root: Option<&Path>) -> Result<(), String> {
        // 1. Terminate all running Steam processes
        let _ = Self::terminate_steam_instances();

        // 2. Brief grace delay for socket unbinding
        std::thread::sleep(std::time::Duration::from_millis(150));

        // 3. Resolve steam-sls launcher path
        let home = dirs::home_dir().ok_or_else(|| "Cannot determine HOME directory".to_string())?;
        let steam_sls_bin = home.join(".local/bin/steam-sls");

        let mut cmd = if steam_sls_bin.is_file() {
            std::process::Command::new(&steam_sls_bin)
        } else {
            // Find steam binary (steam.sh or fallback to PATH)
            let steam_bin = override_root
                .map(|r| r.join("steam.sh"))
                .filter(|p| p.is_file())
                .unwrap_or_else(|| PathBuf::from("steam"));
            let mut c = std::process::Command::new(steam_bin);

            // Retain SLSsteam.so via LD_AUDIT
            let sls_so = home.join(".local/share/SLSsteam/SLSsteam.so");
            if sls_so.is_file() {
                c.env("LD_AUDIT", &sls_so);
            }
            c
        };

        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to spawn steam-sls: {}", e))?;

        Ok(())
    }

    /// Instance method: cleanly terminates running Steam and relaunches via `steam-sls`.
    pub fn relaunch_via_sls(&self, override_root: Option<&Path>) -> Result<(), String> {
        let root = override_root.or_else(|| self.installation.as_ref().map(|i| i.root.as_path()));
        Self::relaunch_steam_sls(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_steam_service_detection_and_query() {
        let mut svc = SteamService::new();
        let _ = svc.detect(None);
        let _ = svc.is_available();
        let _ = svc.scan_all_games();
    }
}
