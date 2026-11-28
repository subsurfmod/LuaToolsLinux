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
        // LNX-05: Refuse to mutate Steam library manifests/VDF while Steam is running
        if !cfg!(test) && crate::steam_bridge::preload_loader::find_steam_pid().is_some() {
            return Err(
                "Steam is running. Please close Steam before modifying library folders."
                    .to_string(),
            );
        }

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

        // LNX-05: Compute real SizeOnDisk from existing game files when present
        let size_on_disk = compute_directory_size_bytes(&game_common_dir);
        let last_updated = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

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
             \t\"LastUpdated\"\t\t\"{}\"\n\
             \t\"SizeOnDisk\"\t\t\"{}\"\n\
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
            app_id, escaped_name, escaped_dir, last_updated, size_on_disk
        );

        fs::write(&manifest_path, manifest_content)
            .map_err(|e| format!("Cannot write {}: {}", manifest_path.display(), e))?;

        self.add_to_library_folders(library, app_id)?;

        sls.add_additional_app(app_id);
        // Do NOT set fake_app_id to 480: AppID 480 spoofing is strictly reserved for multiplayer Online-Fixes.
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

/// Computes total file size in bytes inside `dir` (non-symlink files) for `SizeOnDisk` (LNX-05).
pub fn compute_directory_size_bytes(dir: &std::path::Path) -> u64 {
    if !dir.is_dir() {
        return 0;
    }
    walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .fold(0u64, u64::saturating_add)
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

        // Verify SLSsteam synced (without 480 Spacewar poisoning)
        assert!(sls.config.additional_apps.contains(&1245620));
        assert_eq!(sls.get_fake_app_id(1245620), None);

        // Remove from library and verify clean state
        svc.remove_game_from_library(1245620, &mut sls).unwrap();
        assert!(!svc.is_game_in_library(1245620));
        let vdf_removed = fs::read_to_string(steamapps.join("libraryfolders.vdf")).unwrap();
        assert!(vdf_removed.contains("\"228980\""));
        assert!(!vdf_removed.contains("\"1245620\""));
    }

    #[test]
    fn test_migrate_depotcache_dir() {
        use crate::parsers::manifest_binary::MAGIC_V2;

        let make_manifest = |depot_id: u32, gid: u64| -> Vec<u8> {
            let mut buf = MAGIC_V2.to_le_bytes().to_vec();
            let push_varint = |mut v: u64, out: &mut Vec<u8>| loop {
                let mut b = (v & 0x7F) as u8;
                v >>= 7;
                if v != 0 {
                    b |= 0x80;
                }
                out.push(b);
                if v == 0 {
                    break;
                }
            };
            buf.push(0x08);
            push_varint(u64::from(depot_id), &mut buf);
            buf.push(0x10);
            push_varint(gid, &mut buf);
            buf.push(0x18);
            buf.push(0x00);
            buf
        };

        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("config/depotcache");
        let target = tmp.path().join("depotcache");
        fs::create_dir_all(&legacy).unwrap();

        fs::write(
            legacy.join("1245621_11111111.manifest"),
            make_manifest(1245621, 11111111),
        )
        .unwrap();
        fs::write(
            legacy.join("1245622_22222222.manifest"),
            make_manifest(1245622, 22222222),
        )
        .unwrap();
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
        fs::write(
            legacy.join("1245621_11111111.manifest"),
            make_manifest(1245621, 11111111),
        )
        .unwrap();
        let res2 = SteamLibraryService::migrate_depotcache_dir(&legacy, &target).unwrap();
        assert_eq!(res2.migrated_count, 0);
        assert_eq!(res2.skipped_count, 1);
    }

    #[test]
    fn computes_real_size_on_disk_for_existing_install_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Steam");
        let game_dir = root.join("steamapps/common/ELDEN RING");
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(game_dir.join("game.bin"), vec![0u8; 4096]).unwrap();

        let lib = SteamLibrary::new(root.clone());
        let inst = SteamInstallation {
            root: root.clone(),
            libraries: vec![lib],
            config_path: root.join("config"),
            stplug_in_path: root.join("config/stplug-in"),
        };
        let mut sls = SlssteamService::new(Some(tmp.path().join("SLSsteam/config.yaml")));
        let svc = SteamLibraryService::new(Some(inst));

        let acf_path = svc
            .add_game_to_library(1245620, "ELDEN RING", None, &mut sls)
            .unwrap();
        let acf_text = fs::read_to_string(&acf_path).unwrap();
        assert!(acf_text.contains("\"SizeOnDisk\"\t\t\"4096\""));
    }
}
