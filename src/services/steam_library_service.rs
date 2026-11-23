//! SteamLibraryService — adds games to the Steam library without ownership.
//! Creates `appmanifest_<appid>.acf` files, updates `libraryfolders.vdf`, and registers
//! the game with SLSsteam so it appears in the Steam client library.

use std::fs;
use std::path::Path;

use crate::models::steam::SteamInstallation;
use crate::services::slssteam_service::SlssteamService;

pub struct SteamLibraryService {
    steam_installation: std::sync::Arc<SteamInstallation>,
    slssteam_service: std::sync::Arc<std::sync::Mutex<SlssteamService>>,
}

impl SteamLibraryService {
    pub fn new(
        steam_installation: SteamInstallation,
        slssteam_service: SlssteamService,
    ) -> Self {
        Self {
            steam_installation: std::sync::Arc::new(steam_installation),
            slssteam_service: std::sync::Arc::new(std::sync::Mutex::new(slssteam_service)),
        }
    }

    pub fn steam_installation(&self) -> &SteamInstallation {
        &self.steam_installation
    }

    /// Add a game to the Steam library by creating appmanifest and updating libraryfolders.vdf
    pub fn add_game_to_library(
        &self,
        app_id: u32,
        game_name: &str,
        install_dir: &str,
    ) -> Result<(), String> {
        let inst = &self.steam_installation;

        let library = inst
            .libraries
            .first()
            .ok_or_else(|| "No Steam library found".to_string())?;
        let steamapps_dir = &library.steamapps();
        fs::create_dir_all(steamapps_dir)
            .map_err(|e| format!("Cannot create steamapps dir: {}", e))?;

        let manifest_path = steamapps_dir.join(format!("appmanifest_{}.acf", app_id));
        let manifest_content = format!(
            "appmanifest_{}\n{{\n\t\"appid\"\t\t\"{}\"\n\t\"name\"\t\t\"{}\"\n\t\"installdir\"\t\t\"{}\"\n\t\"LastUpdated\"\t\t\"0\"\n\t\"SizeOnDisk\"\t\t\"0\"\n\t\"buildid\"\t\t\"0\"\n\t\"LastOwner\"\t\t\"0\"\n\t\"BytesToDownload\"\t\t\"0\"\n\t\"BytesDownloaded\"\t\t\"0\"\n\t\"percent_complete_at_disconnect\"\t\t\"0.000000\"\n\t\"userconfig\"\t\t\"\"\n\t\"flags\"\t\t\"0\"\n}}",
            app_id, app_id, game_name, install_dir
        );

        fs::write(&manifest_path, &manifest_content)
            .map_err(|e| format!("Cannot write appmanifest: {}", e))?;

        self.add_to_library_folders(app_id, install_dir)?;

        let mut sls = self.slssteam_service.lock().unwrap();
        sls.add_additional_app(app_id);
        if sls.get_fake_app_id(app_id).is_none() {
            sls.set_fake_app_id(app_id, 480);
        }
        if !sls.save() {
            return Err("Cannot save SLSsteam config".to_string());
        }

        Ok(())
    }

    fn add_to_library_folders(&self, app_id: u32, _install_dir: &str) -> Result<(), String> {
        let inst = &self.steam_installation;
        let library = inst
            .libraries
            .first()
            .ok_or_else(|| "No Steam library found".to_string())?;
        let vdf_path = library.steamapps().join("libraryfolders.vdf");

        let mut content = if vdf_path.exists() {
            fs::read_to_string(&vdf_path)
                .map_err(|e| format!("Cannot read libraryfolders.vdf: {}", e))?
        } else {
            String::new()
        };

        if content.contains(&format!("\"{}\"", app_id)) {
            return Ok(());
        }

        if content.contains("\"apps\"") {
            content = content.replace(
                &format!("\"apps\"\\n\t{{"),
                &format!("\"apps\"\\n\t{{\n\t\t\"{}\"\t\t\"1\"", app_id),
            );
        } else {
            content.push_str(&format!(
                "\n\"apps\"\n\t{{\n\t\t\"{}\"\t\t\"1\"\n\t}}\n",
                app_id
            ));
        }

        fs::write(&vdf_path, &content)
            .map_err(|e| format!("Cannot write libraryfolders.vdf: {}", e))?;

        Ok(())
    }

    pub fn remove_game_from_library(&self, app_id: u32) -> Result<(), String> {
        let inst = &self.steam_installation;
        let library = inst
            .libraries
            .first()
            .ok_or_else(|| "No Steam library found".to_string())?;
        let steamapps_dir = &library.steamapps();

        let manifest_path = steamapps_dir.join(format!("appmanifest_{}.acf", app_id));
        if manifest_path.exists() {
            fs::remove_file(&manifest_path)
                .map_err(|e| format!("Cannot remove appmanifest: {}", e))?;
        }

        let vdf_path = library.steamapps().join("libraryfolders.vdf");
        if vdf_path.exists() {
            let content = fs::read_to_string(&vdf_path)
                .map_err(|e| format!("Cannot read libraryfolders.vdf: {}", e))?;
            let new_content: String = content
                .lines()
                .filter(|line| !line.contains(&format!("\"{}\"", app_id)))
                .collect::<Vec<&str>>()
                .join("\n");
            fs::write(&vdf_path, &new_content)
                .map_err(|e| format!("Cannot write libraryfolders.vdf: {}", e))?;
        }

        let mut sls = self.slssteam_service.lock().unwrap();
        sls.remove_additional_app(app_id);
        sls.remove_fake_app_id(app_id);
        if !sls.save() {
            return Err("Cannot save SLSsteam config".to_string());
        }

        Ok(())
    }

    pub fn list_installed_games(&self) -> Vec<(u32, String)> {
        let inst = &self.steam_installation;
        let mut games = Vec::new();

        for library in &inst.libraries {
            let steamapps_dir = &library.steamapps();
            if let Ok(entries) = fs::read_dir(steamapps_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name.starts_with("appmanifest_") && name.ends_with(".acf") {
                            if let Some(id_str) = name.strip_prefix("appmanifest_").and_then(|s| s.strip_suffix(".acf"))
                            {
                                if let Ok(app_id) = id_str.parse::<u32>() {
                                    let game_name = self.read_app_name(&path).unwrap_or_else(|| format!("App {}", app_id));
                                    games.push((app_id, game_name));
                                }
                            }
                        }
                    }
                }
            }
        }

        games.sort_by(|a, b| a.0.cmp(&b.0));
        games
    }

    fn read_app_name(&self, path: &Path) -> Option<String> {
        let content = fs::read_to_string(path).ok()?;
        let re = regex::Regex::new(r#"^\s*"name"\s+"([^"]+)""#).ok()?;
        re.captures(&content)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
    }

    pub fn is_game_in_library(&self, app_id: u32) -> bool {
        let inst = &self.steam_installation;
        for library in &inst.libraries {
            let manifest_path = library.steamapps().join(format!("appmanifest_{}.acf", app_id));
            if manifest_path.exists() {
                return true;
            }
        }
        false
    }
}

impl std::fmt::Debug for SteamLibraryService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SteamLibraryService")
            .field("steam_installation", &self.steam_installation.root.display())
            .finish()
    }
}