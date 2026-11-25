//! DepotCacheMigrationService — Migrates legacy manifests from `<Steam>/config/depotcache`
//! to `<Steam>/depotcache` on startup and library refresh (LT-34).

use std::fs;
use std::path::{Path, PathBuf};

use crate::services::steam_service::SteamService;

/// Outcome of a DepotCache migration pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DepotCacheMigrationReport {
    pub migrated_count: usize,
    pub skipped_count: usize,
    pub failed_count: usize,
    pub migrated_files: Vec<String>,
}

impl DepotCacheMigrationReport {
    pub fn is_empty(&self) -> bool {
        self.migrated_count == 0 && self.skipped_count == 0 && self.failed_count == 0
    }
}

/// Service that relocates stranded `.manifest` files from `<Steam>/config/depotcache`
/// into Steam's canonical `<Steam>/depotcache` folder.
#[derive(Debug, Clone, Default)]
pub struct DepotCacheMigrationService {
    pub steam_root: Option<PathBuf>,
}

impl DepotCacheMigrationService {
    pub fn new(steam_root: Option<PathBuf>) -> Self {
        Self { steam_root }
    }

    pub fn from_steam_service(steam_service: &SteamService) -> Self {
        Self {
            steam_root: steam_service
                .installation
                .as_ref()
                .map(|inst| inst.root.clone()),
        }
    }

    /// Runs migration if `<Steam>/config/depotcache` exists on the configured Steam root.
    pub fn run(&self) -> DepotCacheMigrationReport {
        let Some(root) = self.steam_root.as_deref() else {
            return DepotCacheMigrationReport::default();
        };
        Self::migrate_steam_root(root)
    }

    /// Migrates `<steam_root>/config/depotcache/*.manifest` to `<steam_root>/depotcache/`.
    pub fn migrate_steam_root(steam_root: &Path) -> DepotCacheMigrationReport {
        let legacy_dir = steam_root.join("config").join("depotcache");
        if !legacy_dir.is_dir() {
            return DepotCacheMigrationReport::default();
        }
        let target_dir = steam_root.join("depotcache");
        match Self::migrate_dir(&legacy_dir, &target_dir) {
            Ok(report) => {
                if report.migrated_count > 0 {
                    log::info!(
                        "Migrated {} depot cache manifest(s) from {} to {} ({} already present, {} failed)",
                        report.migrated_count,
                        legacy_dir.display(),
                        target_dir.display(),
                        report.skipped_count,
                        report.failed_count
                    );
                }
                report
            }
            Err(e) => {
                log::warn!(
                    "DepotCache migration from {} to {} failed: {}",
                    legacy_dir.display(),
                    target_dir.display(),
                    e
                );
                DepotCacheMigrationReport {
                    failed_count: 1,
                    ..Default::default()
                }
            }
        }
    }

    /// Migrates `.manifest` files from `legacy_dir` into `target_dir`, removing migrated source
    /// manifests and cleaning up `legacy_dir` once empty. Never overwrites existing manifests in `target_dir`.
    pub fn migrate_dir(
        legacy_dir: &Path,
        target_dir: &Path,
    ) -> Result<DepotCacheMigrationReport, String> {
        if !legacy_dir.exists() || !legacy_dir.is_dir() {
            return Ok(DepotCacheMigrationReport::default());
        }

        if let (Ok(canon_src), Ok(canon_dst)) = (
            fs::canonicalize(legacy_dir),
            fs::canonicalize(target_dir),
        ) {
            if canon_src == canon_dst {
                return Ok(DepotCacheMigrationReport::default());
            }
        }

        let entries = fs::read_dir(legacy_dir)
            .map_err(|e| format!("Cannot read legacy depotcache dir '{}': {}", legacy_dir.display(), e))?;

        let mut candidates: Vec<(PathBuf, String)> = Vec::new();
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if !ft.is_file() {
                continue;
            }
            let path = entry.path();
            let Some(name) = path
                .file_name()
                .and_then(|s| s.to_str())
                .map(ToString::to_string)
            else {
                continue;
            };
            let is_manifest = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("manifest"))
                .unwrap_or(false);
            if is_manifest {
                candidates.push((path, name));
            }
        }

        if candidates.is_empty() {
            Self::try_remove_if_empty(legacy_dir);
            return Ok(DepotCacheMigrationReport::default());
        }

        fs::create_dir_all(target_dir).map_err(|e| {
            format!(
                "Cannot create depotcache directory '{}': {}",
                target_dir.display(),
                e
            )
        })?;

        let mut report = DepotCacheMigrationReport::default();
        for (src, name) in candidates {
            let dst = target_dir.join(&name);
            if dst.exists() {
                report.skipped_count += 1;
                continue;
            }

            if fs::rename(&src, &dst).is_ok() {
                report.migrated_count += 1;
                report.migrated_files.push(name);
                continue;
            }

            // Fallback across filesystem boundaries: copy to temp file, atomic rename, then remove source
            let tmp_dst = target_dir.join(format!(".{}.migrating.{}", name, std::process::id()));
            match fs::copy(&src, &tmp_dst) {
                Ok(copied_bytes) => {
                    let expected_bytes = src.metadata().map(|m| m.len()).unwrap_or(copied_bytes);
                    if copied_bytes == expected_bytes && fs::rename(&tmp_dst, &dst).is_ok() {
                        let _ = fs::remove_file(&src);
                        report.migrated_count += 1;
                        report.migrated_files.push(name);
                    } else {
                        let _ = fs::remove_file(&tmp_dst);
                        report.failed_count += 1;
                    }
                }
                Err(_) => {
                    let _ = fs::remove_file(&tmp_dst);
                    report.failed_count += 1;
                }
            }
        }

        Self::try_remove_if_empty(legacy_dir);
        Ok(report)
    }

    fn try_remove_if_empty(dir: &Path) {
        if let Ok(mut it) = fs::read_dir(dir) {
            if it.next().is_none() {
                let _ = fs::remove_dir(dir);
            }
        }
    }
}

/// Migrates legacy depot cache files from `<Steam>/config/depotcache` to `<Steam>/depotcache`.
/// Returns the number of files migrated.
pub fn migrate_depot_cache(steam_root: &Path) -> usize {
    DepotCacheMigrationService::migrate_steam_root(steam_root).migrated_count
}

/// Runs the depot cache migration for `steam_service` if `config/depotcache` exists.
pub fn run_depot_cache_migration(steam_service: &SteamService) -> usize {
    DepotCacheMigrationService::from_steam_service(steam_service)
        .run()
        .migrated_count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::steam::{SteamInstallation, SteamLibrary};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_migrate_depot_cache() {
        let dir = tempdir().unwrap();
        let steam_root = dir.path().join("Steam");
        let legacy = steam_root.join("config/depotcache");
        let new = steam_root.join("depotcache");

        fs::create_dir_all(&legacy).unwrap();
        fs::create_dir_all(&new).unwrap();

        // Create test manifest files and a non-manifest file
        fs::write(legacy.join("123_456.manifest"), b"test1").unwrap();
        fs::write(legacy.join("789_012.manifest"), b"test2").unwrap();
        fs::write(legacy.join("not_a_manifest.txt"), b"ignore").unwrap();

        let migrated = migrate_depot_cache(&steam_root);
        assert_eq!(migrated, 2);

        assert!(new.join("123_456.manifest").exists());
        assert!(new.join("789_012.manifest").exists());
        assert!(!new.join("not_a_manifest.txt").exists());
        // Source manifests were moved out of config/depotcache
        assert!(!legacy.join("123_456.manifest").exists());
        assert!(!legacy.join("789_012.manifest").exists());
        // Non-manifest file remains untouched
        assert!(legacy.join("not_a_manifest.txt").exists());
    }

    #[test]
    fn test_migrate_skips_existing() {
        let dir = tempdir().unwrap();
        let steam_root = dir.path().join("Steam");
        let legacy = steam_root.join("config/depotcache");
        let new = steam_root.join("depotcache");

        fs::create_dir_all(&legacy).unwrap();
        fs::create_dir_all(&new).unwrap();

        // Create file in both locations
        fs::write(legacy.join("123_456.manifest"), b"old").unwrap();
        fs::write(new.join("123_456.manifest"), b"new").unwrap();

        let migrated = migrate_depot_cache(&steam_root);
        assert_eq!(migrated, 0); // Should skip since destination already exists

        let content = fs::read_to_string(new.join("123_456.manifest")).unwrap();
        assert_eq!(content, "new"); // Should keep existing version in depotcache
    }

    #[test]
    fn test_service_removes_empty_legacy_dir_and_handles_missing_dir() {
        let dir = tempdir().unwrap();
        let steam_root = dir.path().join("Steam");
        fs::create_dir_all(steam_root.join("config")).unwrap();

        let mut steam_svc = SteamService::new();
        steam_svc.installation = Some(SteamInstallation {
            root: steam_root.clone(),
            libraries: vec![SteamLibrary::new(steam_root.clone())],
            config_path: steam_root.join("config"),
            stplug_in_path: steam_root.join("config/stplug-in"),
        });

        // No config/depotcache directory -> clean no-op
        let svc = DepotCacheMigrationService::from_steam_service(&steam_svc);
        let empty_report = svc.run();
        assert!(empty_report.is_empty());

        // Populate config/depotcache with only manifests -> migrates and removes empty legacy dir
        let legacy = steam_root.join("config/depotcache");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("480_999999999.manifest"), b"MANIFEST_DATA").unwrap();

        let report = svc.run();
        assert_eq!(report.migrated_count, 1);
        assert_eq!(report.skipped_count, 0);
        assert_eq!(report.failed_count, 0);
        assert!(steam_root.join("depotcache/480_999999999.manifest").exists());
        assert!(!legacy.exists());
    }
}
