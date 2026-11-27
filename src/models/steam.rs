//! Steam installation, library folders, and ACF manifest models.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

fn vdf_path_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#""path"\s+"([^"]+)""#).unwrap())
}

fn acf_name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#""name"\s+"([^"]+)""#).unwrap())
}

fn acf_installdir_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#""installdir"\s+"([^"]+)""#).unwrap())
}

#[derive(Debug, Clone)]
pub struct SteamLibrary {
    pub path: PathBuf,
}

impl SteamLibrary {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn steamapps(&self) -> PathBuf {
        self.path.join("steamapps")
    }

    pub fn common(&self) -> PathBuf {
        self.steamapps().join("common")
    }

    pub fn depotcache(&self) -> PathBuf {
        self.path.join("depotcache")
    }

    pub fn steamapps_depotcache(&self) -> PathBuf {
        self.steamapps().join("depotcache")
    }
}

#[derive(Debug, Clone, Default)]
pub struct SteamInstallation {
    pub root: PathBuf,
    pub libraries: Vec<SteamLibrary>,
    pub config_path: PathBuf,
    pub stplug_in_path: PathBuf,
}

impl SteamInstallation {
    pub fn is_valid(&self) -> bool {
        self.root.exists()
            && (self.root.join("steam.sh").exists()
                || self.root.join("steamapps").exists()
                || self.root.join("config").exists())
    }
}

pub fn default_steam_candidates() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    vec![
        home.join(".local/share/Steam"),
        home.join(".steam/steam"),
        home.join(".steam/root"),
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        PathBuf::from("/var/lib/steam"),
    ]
}

pub fn discover_libraries(steam_root: &Path) -> Vec<SteamLibrary> {
    let mut libraries = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    let mut add_lib = |p: &Path| {
        if !p.exists() {
            return;
        }
        let canon = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        if seen.insert(canon.clone()) {
            libraries.push(SteamLibrary::new(canon));
        }
    };

    add_lib(steam_root);

    for rel in ["steamapps/libraryfolders.vdf", "config/libraryfolders.vdf"] {
        let vdf_path = steam_root.join(rel);
        if let Ok(content) = fs::read_to_string(&vdf_path) {
            for cap in vdf_path_re().captures_iter(&content) {
                if let Some(m) = cap.get(1) {
                    let norm = m.as_str().replace("\\\\", "/");
                    add_lib(Path::new(&norm));
                }
            }
        }
    }

    libraries
}

pub fn scan_installed_games(library: &SteamLibrary) -> Vec<u32> {
    let mut ids = Vec::new();
    let steamapps = library.steamapps();
    let Ok(entries) = fs::read_dir(&steamapps) else {
        return ids;
    };

    for entry in entries.flatten() {
        let fname = entry.file_name();
        let name = fname.to_string_lossy();
        if let Some(rest) = name.strip_prefix("appmanifest_") {
            if let Some(id_str) = rest.strip_suffix(".acf") {
                if let Ok(aid) = id_str.parse::<u32>() {
                    ids.push(aid);
                }
            }
        }
    }

    ids.sort_unstable();
    ids.dedup();
    ids
}

pub fn get_game_name_from_acf(library: &SteamLibrary, app_id: u32) -> Option<String> {
    let acf = library
        .steamapps()
        .join(format!("appmanifest_{}.acf", app_id));
    let content = fs::read_to_string(acf).ok()?;
    acf_name_re()
        .captures(&content)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

pub fn get_install_dir_from_acf(library: &SteamLibrary, app_id: u32) -> Option<PathBuf> {
    let acf = library
        .steamapps()
        .join(format!("appmanifest_{}.acf", app_id));
    let content = fs::read_to_string(acf).ok()?;
    let dir_name = acf_installdir_re().captures(&content)?.get(1)?.as_str();
    let rel = Path::new(dir_name);
    if !crate::services::lua_installer::LuaInstaller::is_safe_relative_path(rel) {
        return None;
    }
    let full = library.common().join(dir_name);
    Some(full)
}
