//! DepotCacheMigrationService — Migrates legacy manifests from `<Steam>/config/depotcache`
//! to `<Steam>/depotcache` on startup and library refresh (LT-34).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::parsers::manifest_binary::{parse_manifest_filename, read_manifest_file};
use crate::services::steam_service::SteamService;

/// Process-wide one-shot guard so repeated UI startup/refresh hooks do not redundantly re-scan (LT-34).
static GLOBAL_MIGRATION_DONE: AtomicBool = AtomicBool::new(false);

/// Resets the global one-shot migration guard (e.g., when switching Steam root or in tests).
pub fn reset_depot_cache_migration_guard() {
    GLOBAL_MIGRATION_DONE.store(false, Ordering::SeqCst);
}

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
#[derive(Debug, Clone)]
pub struct DepotCacheMigrationService {
    pub steam_root: Option<PathBuf>,
    completed: Arc<AtomicBool>,
}

impl Default for DepotCacheMigrationService {
    fn default() -> Self {
        Self::new(None)
    }
}

impl DepotCacheMigrationService {
    pub fn new(steam_root: Option<PathBuf>) -> Self {
        Self {
            steam_root,
            completed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn from_steam_service(steam_service: &SteamService) -> Self {
        Self {
            steam_root: steam_service
                .installation
                .as_ref()
                .map(|inst| inst.root.clone()),
            completed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Runs migration if `<Steam>/config/depotcache` exists on the configured Steam root.
    /// Guarded by an `AtomicBool` one-shot flag once a legacy directory has been processed (LT-34).
    pub fn run(&self) -> DepotCacheMigrationReport {
        if self.completed.load(Ordering::Acquire) {
            return DepotCacheMigrationReport::default();
        }
        let Some(root) = self.steam_root.as_deref() else {
            return DepotCacheMigrationReport::default();
        };
        let legacy_dir = root.join("config").join("depotcache");
        if !legacy_dir.is_dir() {
            return DepotCacheMigrationReport::default();
        }
        let report = Self::migrate_steam_root(root);
        self.completed.store(true, Ordering::Release);
        report
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
            // LT-34: Validate `<depotId>_<gid>.manifest` filename and binary manifest payload before migrating
            let Some((fn_depot_id, _fn_gid)) = parse_manifest_filename(&src) else {
                log::warn!(
                    "Skipping legacy manifest with non-standard filename: {}",
                    src.display()
                );
                report.skipped_count += 1;
                continue;
            };
            match read_manifest_file(&src) {
                Ok(meta) => {
                    if meta.depot_id != 0 && meta.depot_id != fn_depot_id {
                        log::warn!(
                            "Skipping legacy manifest {} whose header depot_id ({}) disagrees with filename ({})",
                            name,
                            meta.depot_id,
                            fn_depot_id
                        );
                        report.skipped_count += 1;
                        continue;
                    }
                    log::debug!(
                        "Validated legacy manifest {} (depot={}, gid={}, files={}, size={})",
                        name,
                        meta.depot_id,
                        meta.manifest_gid,
                        meta.file_count,
                        meta.total_size
                    );
                }
                Err(e) => {
                    log::warn!(
                        "Skipping invalid/corrupt legacy manifest {}: {}",
                        src.display(),
                        e
                    );
                    report.skipped_count += 1;
                    continue;
                }
            }

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

/// Runs the depot cache migration for `steam_service` once per process if `config/depotcache` exists.
pub fn run_depot_cache_migration(steam_service: &SteamService) -> usize {
    if GLOBAL_MIGRATION_DONE.swap(true, Ordering::AcqRel) {
        return 0;
    }
    DepotCacheMigrationService::from_steam_service(steam_service)
        .run()
        .migrated_count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::steam::{SteamInstallation, SteamLibrary};
    use crate::parsers::manifest_binary::MAGIC_V2;
    use std::fs;
    use tempfile::tempdir;

    fn encode_varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let mut b = (v & 0x7F) as u8;
            v >>= 7;
            if v != 0 {
                b |= 0x80;
            }
            out.push(b);
            if v == 0 {
                break;
            }
        }
    }

    fn make_valid_manifest_bytes(depot_id: u32, gid: u64) -> Vec<u8> {
        let mut buf = MAGIC_V2.to_le_bytes().to_vec();
        buf.push(0x08); // field 1: depot_id
        encode_varint(u64::from(depot_id), &mut buf);
        buf.push(0x10); // field 2: manifest_gid
        encode_varint(gid, &mut buf);
        buf.push(0x18); // field 3: file_count = 0
        buf.push(0x00);
        buf.push(0x20); // field 4: total_size = 1024
        encode_varint(1024, &mut buf);
        buf
    }

    #[test]
    fn test_migrate_depot_cache() {
        let dir = tempdir().unwrap();
        let steam_root = dir.path().join("Steam");
        let legacy = steam_root.join("config/depotcache");
        let new = steam_root.join("depotcache");

        fs::create_dir_all(&legacy).unwrap();
        fs::create_dir_all(&new).unwrap();

        // Create valid binary manifest files, a corrupt manifest, and a non-manifest file
        fs::write(legacy.join("123_456.manifest"), make_valid_manifest_bytes(123, 456)).unwrap();
        fs::write(legacy.join("789_012.manifest"), make_valid_manifest_bytes(789, 12)).unwrap();
        fs::write(legacy.join("999_888.manifest"), b"CORRUPT_NOT_A_MANIFEST").unwrap();
        fs::write(legacy.join("not_a_manifest.txt"), b"ignore").unwrap();

        let migrated = migrate_depot_cache(&steam_root);
        assert_eq!(migrated, 2);

        assert!(new.join("123_456.manifest").exists());
        assert!(new.join("789_012.manifest").exists());
        assert!(!new.join("999_888.manifest").exists());
        assert!(!new.join("not_a_manifest.txt").exists());
        // Source manifests were moved out of config/depotcache
        assert!(!legacy.join("123_456.manifest").exists());
        assert!(!legacy.join("789_012.manifest").exists());
        // Corrupt and non-manifest files remain untouched
        assert!(legacy.join("999_888.manifest").exists());
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
        fs::write(legacy.join("123_456.manifest"), make_valid_manifest_bytes(123, 456)).unwrap();
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

        // Populate config/depotcache with only valid manifests -> migrates and removes empty legacy dir
        let legacy = steam_root.join("config/depotcache");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(
            legacy.join("480_999999999.manifest"),
            make_valid_manifest_bytes(480, 999999999),
        )
        .unwrap();

        let report = svc.run();
        assert_eq!(report.migrated_count, 1);
        assert_eq!(report.skipped_count, 0);
        assert_eq!(report.failed_count, 0);
        assert!(steam_root.join("depotcache/480_999999999.manifest").exists());
        assert!(!legacy.exists());

        // Repeated call on the same service is guarded by one-shot AtomicBool
        fs::create_dir_all(&legacy).unwrap();
        fs::write(
            legacy.join("480_888888888.manifest"),
            make_valid_manifest_bytes(480, 888888888),
        )
        .unwrap();
        let second = svc.run();
        assert!(second.is_empty());
    }
}
