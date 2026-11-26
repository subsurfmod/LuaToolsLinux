//! SteamLibraryService — registers and unregisters games in the local Steam library.
//! Writes valid Valve KeyValues `"AppState"` `appmanifest_<appid>.acf` files, updates
//! `libraryfolders.vdf` without corrupting structure, and syncs SLSsteam `config.yaml`.

use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

use regex::Regex;

use crate::models::steam::{
    get_game_name_from_acf, scan_installed_games, SteamInstallation, SteamLibrary,
};
use crate::services::slssteam_service::SlssteamService;

fn apps_block_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?m)^(?P<indent>[ \t]*)"apps"[ \t]*\r?\n[ \t]*\{"#).unwrap())
}

#[derive(Debug, Clone, Default)]
pub struct SteamLibraryService {
    pub steam_installation: Option<SteamInstallation>,
}

impl SteamLibraryService {
    pub fn new(steam_installation: Option<SteamInstallation>) -> Self {
        Self { steam_installation }
    }

    pub fn primary_library(&self) -> Result<&SteamLibrary, String> {
        self.steam_installation
            .as_ref()
            .and_then(|i| i.libraries.first())
            .ok_or_else(|| "No Steam library folder detected.".to_string())
    }

    pub fn sanitize_install_dir(game_name: &str, app_id: u32) -> String {
        let cleaned: String = game_name
            .chars()
            .filter(|&c| {
                !c.is_control()
                    && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
            })
            .collect();
        let trimmed = cleaned.trim().trim_matches('.');
        if trimmed.is_empty() {
            format!("App_{}", app_id)
        } else {
            trimmed.to_string()
        }
    }

    /// Registers `app_id` in Steam by writing a valid `"AppState"` `appmanifest_<appid>.acf`,
    /// creating `<steamapps>/common/<install_dir>`, updating `libraryfolders.vdf`, and syncing SLSsteam.
    pub fn add_game_to_library(
        &self,
        app_id: u32,
        game_name: &str,
        install_dir_override: Option<&str>,
        sls: &mut SlssteamService,
    ) -> Result<PathBuf, String> {
        let library = self.primary_library()?;
        let steamapps_dir = library.steamapps();
        fs::create_dir_all(&steamapps_dir)
            .map_err(|e| format!("Cannot create steamapps dir: {}", e))?;

        let dir_name = install_dir_override
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| Self::sanitize_install_dir(s, app_id))
            .unwrap_or_else(|| Self::sanitize_install_dir(game_name, app_id));

        let game_common_dir = library.common().join(&dir_name);
        fs::create_dir_all(&game_common_dir)
            .map_err(|e| format!("Cannot create common/{}: {}", dir_name, e))?;

        let escaped_name = escape_vdf(game_name);
        let escaped_dir = escape_vdf(&dir_name);

        let manifest_path = steamapps_dir.join(format!("appmanifest_{}.acf", app_id));
        let manifest_content = format!(
            "\"AppState\"\n\
             {{\n\
             \t\"appid\"\t\t\"{}\"\n\
             \t\"Universe\"\t\t\"1\"\n\
             \t\"name\"\t\t\"{}\"\n\
             \t\"StateFlags\"\t\t\"4\"\n\
             \t\"installdir\"\t\t\"{}\"\n\
             \t\"LastUpdated\"\t\t\"0\"\n\
             \t\"SizeOnDisk\"\t\t\"0\"\n\
             \t\"StagingSize\"\t\t\"0\"\n\
             \t\"buildid\"\t\t\"0\"\n\
             \t\"LastOwner\"\t\t\"0\"\n\
             \t\"UpdateResult\"\t\t\"0\"\n\
             \t\"BytesToDownload\"\t\t\"0\"\n\
             \t\"BytesDownloaded\"\t\t\"0\"\n\
             \t\"BytesToStage\"\t\t\"0\"\n\
             \t\"BytesStaged\"\t\t\"0\"\n\
             \t\"TargetBuildID\"\t\t\"0\"\n\
             \t\"AutoUpdateBehavior\"\t\t\"0\"\n\
             \t\"AllowOtherDownloadsWhileRunning\"\t\t\"0\"\n\
             \t\"ScheduledAutoUpdate\"\t\t\"0\"\n\
             }}\n",
            app_id, escaped_name, escaped_dir
        );

        fs::write(&manifest_path, manifest_content)
            .map_err(|e| format!("Cannot write {}: {}", manifest_path.display(), e))?;

        self.add_to_library_folders(library, app_id)?;

        sls.add_additional_app(app_id);
        if sls.get_fake_app_id(app_id).is_none() {
            sls.set_fake_app_id(app_id, 480);
        }
        sls.config.play_not_owned = true;
        if !sls.save() {
            return Err("Cannot save SLSsteam config.yaml".to_string());
        }

        Ok(manifest_path)
    }

    fn add_to_library_folders(&self, library: &SteamLibrary, app_id: u32) -> Result<(), String> {
        // LNX-05: Check if Steam is running before modifying libraryfolders.vdf
        // Skip check during tests (CI environment may have false positives)
        if !cfg!(test) && crate::steam_bridge::preload_loader::find_steam_pid().is_some() {
            return Err("Steam is running. Please close Steam before modifying library folders.".to_string());
        }

        let vdf_path = library.steamapps().join("libraryfolders.vdf");
        let app_entry_re = Regex::new(&format!(r#"(?m)^\s*"{}"\s+"[0-9]+""#, app_id)).unwrap();

        if vdf_path.exists() {
            let content = fs::read_to_string(&vdf_path)
                .map_err(|e| format!("Cannot read libraryfolders.vdf: {}", e))?;
            if app_entry_re.is_match(&content) {
                return Ok(());
            }
            if let Some(m) = apps_block_re().find(&content) {
                let indent = apps_block_re()
                    .captures(&content)
                    .and_then(|c| c.name("indent"))
                    .map(|x| x.as_str())
                    .unwrap_or("\t\t");
                let mut updated = String::with_capacity(content.len() + 32);
                updated.push_str(&content[..m.end()]);
                updated.push_str(&format!("\n{}\t\"{}\"\t\t\"0\"", indent, app_id));
                updated.push_str(&content[m.end()..]);
                fs::write(&vdf_path, updated)
                    .map_err(|e| format!("Cannot write libraryfolders.vdf: {}", e))?;
                return Ok(());
            }
        }

        let lib_path = escape_vdf(&library.path.to_string_lossy());
        let fresh = format!(
            "\"libraryfolders\"\n\
             {{\n\
             \t\"0\"\n\
             \t{{\n\
             \t\t\"path\"\t\t\"{}\"\n\
             \t\t\"apps\"\n\
             \t\t{{\n\
             \t\t\t\"{}\"\t\t\"0\"\n\
             \t\t}}\n\
             \t}}\n\
             }}\n",
            lib_path, app_id
        );
        fs::write(&vdf_path, fresh)
            .map_err(|e| format!("Cannot write libraryfolders.vdf: {}", e))?;
        Ok(())
    }

    /// Removes `appmanifest_<appid>.acf`, cleans `libraryfolders.vdf`, and unregisters from SLSsteam
    /// (leaves any downloaded game files inside `steamapps/common/` untouched).
    pub fn remove_game_from_library(
        &self,
        app_id: u32,
        sls: &mut SlssteamService,
    ) -> Result<(), String> {
        // LNX-05: Check if Steam is running before modifying libraryfolders.vdf
        // Skip check during tests (CI environment may have false positives)
        if !cfg!(test) && crate::steam_bridge::preload_loader::find_steam_pid().is_some() {
            return Err("Steam is running. Please close Steam before modifying library folders.".to_string());
        }

        let Some(inst) = &self.steam_installation else {
            return Err("No Steam installation detected.".into());
        };

        let line_re = Regex::new(&format!(r#"^\s*"{}"\s+"[0-9]+"\s*$"#, app_id)).unwrap();

        for library in &inst.libraries {
            let steamapps_dir = library.steamapps();
            let manifest_path = steamapps_dir.join(format!("appmanifest_{}.acf", app_id));
            if manifest_path.exists() {
                fs::remove_file(&manifest_path)
                    .map_err(|e| format!("Cannot remove {}: {}", manifest_path.display(), e))?;
            }

            let vdf_path = steamapps_dir.join("libraryfolders.vdf");
            if vdf_path.exists() {
                let content = fs::read_to_string(&vdf_path)
                    .map_err(|e| format!("Cannot read libraryfolders.vdf: {}", e))?;
                let mut filtered: String = content
                    .lines()
                    .filter(|line| !line_re.is_match(line))
                    .collect::<Vec<&str>>()
                    .join("\n");
                if !filtered.ends_with('\n') {
                    filtered.push('\n');
                }
                fs::write(&vdf_path, filtered)
                    .map_err(|e| format!("Cannot write libraryfolders.vdf: {}", e))?;
            }
        }

        sls.remove_additional_app(app_id);
        if sls.get_fake_app_id(app_id) == Some(480) {
            sls.remove_fake_app_id(app_id);
        }
        if !sls.save() {
            return Err("Cannot save SLSsteam config.yaml".to_string());
        }

        Ok(())
    }

    pub fn list_installed_games(&self) -> Vec<(u32, String)> {
        let Some(inst) = &self.steam_installation else {
            return Vec::new();
        };
        let mut games = Vec::new();
        for library in &inst.libraries {
            for aid in scan_installed_games(library) {
                let name =
                    get_game_name_from_acf(library, aid).unwrap_or_else(|| format!("App {}", aid));
                games.push((aid, name));
            }
        }
        games.sort_by_key(|a| a.0);
        games.dedup_by_key(|a| a.0);
        games
    }

    pub fn is_game_in_library(&self, app_id: u32) -> bool {
        let Some(inst) = &self.steam_installation else {
            return false;
        };
        inst.libraries.iter().any(|lib| {
            lib.steamapps()
                .join(format!("appmanifest_{}.acf", app_id))
                .exists()
        })
    }

    /// Migrates manifests from legacy directory (`<steam_root>/config/depotcache`)
    /// to the canonical depotcache directory (`<steam_root>/depotcache`).
    pub fn migrate_depotcache(&self) -> Result<DepotCacheMigrationResult, String> {
        let Some(inst) = &self.steam_installation else {
            return Err("No Steam installation detected.".to_string());
        };
        let legacy_dir = inst.root.join("config/depotcache");
        let target_dir = inst.root.join("depotcache");
        Self::migrate_depotcache_dir(&legacy_dir, &target_dir)
    }

    /// Migrates all `.manifest` files from `legacy_dir` to `target_dir` via `DepotCacheMigrationService`.
    pub fn migrate_depotcache_dir(
        legacy_dir: &std::path::Path,
        target_dir: &std::path::Path,
    ) -> Result<DepotCacheMigrationResult, String> {
        let report =
            crate::services::depot_cache_migration::DepotCacheMigrationService::migrate_dir(
                legacy_dir, target_dir,
            )?;
        if report.failed_count > 0 && report.migrated_count == 0 {
            return Err(format!(
                "Failed to migrate {} manifest(s) from '{}'",
                report.failed_count,
                legacy_dir.display()
            ));
        }
        Ok(DepotCacheMigrationResult {
            migrated_count: report.migrated_count,
            skipped_count: report.skipped_count,
            migrated_files: report.migrated_files,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DepotCacheMigrationResult {
    pub migrated_count: usize,
    pub skipped_count: usize,
    pub migrated_files: Vec<String>,
}

fn escape_vdf(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::steam::get_install_dir_from_acf;

    #[test]
    fn writes_valid_appstate_acf_updates_libraryfolders_and_removes_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Steam");
        let steamapps = root.join("steamapps");
        fs::create_dir_all(&steamapps).unwrap();

        // Seed realistic Valve libraryfolders.vdf
        let existing_vdf = format!(
            "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t\t\"apps\"\n\t\t{{\n\t\t\t\"228980\"\t\t\"123456\"\n\t\t}}\n\t}}\n}}\n",
            root.display()
        );
        fs::write(steamapps.join("libraryfolders.vdf"), existing_vdf).unwrap();

        let lib = SteamLibrary::new(root.clone());
        let inst = SteamInstallation {
            root: root.clone(),
            libraries: vec![lib.clone()],
            config_path: root.join("config"),
            stplug_in_path: root.join("config/stplug-in"),
        };
        let mut sls = SlssteamService::new(Some(tmp.path().join("SLSsteam/config.yaml")));
        let svc = SteamLibraryService::new(Some(inst));

        let acf_path = svc
            .add_game_to_library(1245620, "ELDEN RING", None, &mut sls)
            .unwrap();
        assert!(acf_path.exists());
        assert!(svc.is_game_in_library(1245620));

        // Verify ACF has valid "AppState" header and parses via standard Steam helpers
        let acf_text = fs::read_to_string(&acf_path).unwrap();
        assert!(acf_text.starts_with("\"AppState\""));
        assert_eq!(
            get_game_name_from_acf(&lib, 1245620).as_deref(),
            Some("ELDEN RING")
        );
        let install_dir = get_install_dir_from_acf(&lib, 1245620).unwrap();
        assert!(install_dir.exists());
        assert_eq!(
            svc.list_installed_games(),
            vec![(1245620, "ELDEN RING".to_string())]
        );

        // Verify libraryfolders.vdf kept 228980 and added 1245620 inside "apps"
        let vdf_after = fs::read_to_string(steamapps.join("libraryfolders.vdf")).unwrap();
        assert!(vdf_after.contains("\"228980\""));
        assert!(vdf_after.contains("\"1245620\"\t\t\"0\""));
        assert!(!vdf_after.contains("\\n"));

        // Verify SLSsteam synced
        assert!(sls.config.additional_apps.contains(&1245620));
        assert_eq!(sls.get_fake_app_id(1245620), Some(480));

        // Remove from library and verify clean state
        svc.remove_game_from_library(1245620, &mut sls).unwrap();
        assert!(!svc.is_game_in_library(1245620));
        let vdf_removed = fs::read_to_string(steamapps.join("libraryfolders.vdf")).unwrap();
        assert!(vdf_removed.contains("\"228980\""));
        assert!(!vdf_removed.contains("\"1245620\""));
    }

    #[test]
    fn test_migrate_depotcache_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("config/depotcache");
        let target = tmp.path().join("depotcache");
        fs::create_dir_all(&legacy).unwrap();

        fs::write(legacy.join("1245621_11111111.manifest"), b"MANIFEST_1").unwrap();
        fs::write(legacy.join("1245622_22222222.manifest"), b"MANIFEST_2").unwrap();
        fs::write(legacy.join("other.txt"), b"NOT_A_MANIFEST").unwrap();

        let res = SteamLibraryService::migrate_depotcache_dir(&legacy, &target).unwrap();
        assert_eq!(res.migrated_count, 2);
        assert_eq!(res.skipped_count, 0);
        assert!(target.join("1245621_11111111.manifest").exists());
        assert!(target.join("1245622_22222222.manifest").exists());
        assert!(!target.join("other.txt").exists());
        // Source files were cleanly moved/removed
        assert!(!legacy.join("1245621_11111111.manifest").exists());
        assert!(!legacy.join("1245622_22222222.manifest").exists());

        // If duplicate manifests appear in legacy again, they are skipped
        fs::write(legacy.join("1245621_11111111.manifest"), b"MANIFEST_1_DUP").unwrap();
        let res2 = SteamLibraryService::migrate_depotcache_dir(&legacy, &target).unwrap();
        assert_eq!(res2.migrated_count, 0);
        assert_eq!(res2.skipped_count, 1);
    }
}
