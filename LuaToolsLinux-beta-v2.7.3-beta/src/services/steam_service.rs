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
}
