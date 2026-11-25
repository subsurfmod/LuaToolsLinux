//! DepotCacheMigrationService — Migrates legacy manifests from `<Steam>/config/depotcache`
//! to `<Steam>/depotcache` on startup.

use std::fs;
use std::path::Path;

use crate::services::steam_service::SteamService;

/// Migrates legacy depot cache files from the old location to the new location.
/// Returns the number of files migrated.
pub fn migrate_depot_cache(steam_root: &Path) -> usize {
    let legacy_cache = steam_root.join("config").join("depotcache");
    let new_cache = steam_root.join("depotcache");

    if !legacy_cache.exists() || !legacy_cache.is_dir() {
        return 0;
    }

    let _ = fs::create_dir_all(&new_cache);

    let mut migrated = 0;
    if let Ok(entries) = fs::read_dir(&legacy_cache) {
        for entry in entries.flatten() {
            let src = entry.path();
            if !src.is_file() {
                continue;
            }

            // Only migrate manifest files
            if let Some(ext) = src.extension().and_then(|e| e.to_str()) {
                if ext != "manifest" {
                    continue;
                }
            } else {
                continue;
            }

            let dst = new_cache.join(src.file_name().unwrap());

            // Only migrate if destination doesn't exist or source is newer
            let should_migrate = if dst.exists() {
                if let (Ok(src_meta), Ok(dst_meta)) = (src.metadata(), dst.metadata()) {
                    src_meta.modified().ok() > dst_meta.modified().ok()
                } else {
                    false
                }
            } else {
                true
            };

            if should_migrate {
                if fs::copy(&src, &dst).is_ok() {
                    migrated += 1;
                }
            }
        }
    }

    migrated
}

/// Runs the depot cache migration and logs the result.
pub fn run_depot_cache_migration(steam_service: &SteamService) -> usize {
    let steam_root = if let Some(inst) = &steam_service.installation {
        inst.root.as_path()
    } else {
        return 0;
    };

    let migrated = migrate_depot_cache(steam_root);
    if migrated > 0 {
        log::info!("Migrated {} depot cache files from config/depotcache to depotcache", migrated);
    }
    migrated
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs;

    #[test]
    fn test_migrate_depot_cache() {
        let dir = tempdir().unwrap();
        let steam_root = dir.path().join("Steam");
        let legacy = steam_root.join("config/depotcache");
        let new = steam_root.join("depotcache");

        fs::create_dir_all(&legacy).unwrap();
        fs::create_dir_all(&new).unwrap();

        // Create test manifest files
        fs::write(legacy.join("123_456.manifest"), b"test1").unwrap();
        fs::write(legacy.join("789_012.manifest"), b"test2").unwrap();
        fs::write(legacy.join("not_a_manifest.txt"), b"ignore").unwrap();

        let migrated = migrate_depot_cache(&steam_root);
        assert_eq!(migrated, 2);

        assert!(new.join("123_456.manifest").exists());
        assert!(new.join("789_012.manifest").exists());
        assert!(!new.join("not_a_manifest.txt").exists());
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
        assert_eq!(migrated, 0); // Should skip since destination exists and is newer

        let content = fs::read_to_string(new.join("123_456.manifest")).unwrap();
        assert_eq!(content, "new"); // Should keep new version
    }
}