//! ModeMigration - Migration of legacy modes to current mode system.
//!
//! In the original C# LuaTools, there were multiple modes (SteamTools,
//! OpenSteamTools/OpenSteamToolsNightly branded as "BetterSteamTools", and a
//! CloudRedirect CLI fix). This module handles migrating legacy mode settings
//! to the current mode system.
//!
//! Note: In the Rust rewrite, there is no legacy mode system to migrate from.
//! This service is kept for API compatibility and future-proofing.

use serde::{Deserialize, Serialize};
use std::convert::TryFrom;
use std::fs;
use std::path::{Path, PathBuf};

/// Legacy mode values from the C# version
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum LegacyMode {
    #[default]
    SteamTools,
    OpenSteamTools,
    OpenSteamToolsNightly,
}

/// Current mode
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum UnlockerMode {
    /// Original SteamTools mode (retired)
    SteamTools,
    /// BetterSteamTools mode (OpenSteamTools upstream)
    Bst,
    /// Custom mode
    Custom,
    /// No mode selected
    #[default]
    None,
}

/// Result of migration
#[derive(Debug, Clone)]
pub struct MigrationResult {
    pub new_mode: UnlockerMode,
    pub reset_onboarding: bool,
}

impl MigrationResult {
    pub fn new(mode: UnlockerMode, reset_onboarding: bool) -> Self {
        Self {
            new_mode: mode,
            reset_onboarding,
        }
    }
}

/// Mode migration service
pub struct ModeMigration;

impl ModeMigration {
    pub fn new() -> Self {
        Self
    }

    /// Migrate a legacy mode string to the current mode system.
    /// Returns the new mode and whether onboarding must be re-shown.
    pub fn migrate(&self, stored: Option<&str>) -> MigrationResult {
        let stored = match stored.map(str::trim) {
            Some(s) if !s.is_empty() => s,
            _ => return MigrationResult::new(UnlockerMode::None, false),
        };

        // Already current mode - leave it be
        if let Ok(current) = UnlockerMode::try_from(stored) {
            return MigrationResult::new(current, false);
        }

        match stored.to_ascii_lowercase().as_str() {
            // Both were branded "BetterSteamTools" in the UI, so BST is what these users think they run.
            "opensteamtools" | "opensteamtoolsnightly" => {
                MigrationResult::new(UnlockerMode::Bst, false)
            }

            // Unknown or retired modes with nothing to map onto:
            // Clear the mode and send them back through onboarding.
            _ => MigrationResult::new(UnlockerMode::None, true),
        }
    }

    /// Apply migration to settings. Returns true if onboarding must be re-shown.
    pub fn apply(self, stored: Option<&str>, mut set_mode: impl FnMut(UnlockerMode)) -> bool {
        let result = self.migrate(stored);

        set_mode(result.new_mode);

        result.reset_onboarding
    }

    /// Check if a mode migration is needed
    pub fn needs_migration(stored: Option<&str>) -> bool {
        matches!(stored.map(str::trim), Some(s) if !s.is_empty() && UnlockerMode::try_from(s).is_err())
    }

    /// Scans a game directory for legacy SmokeAPI, GreenLuma, and BST / SteamTools unlocker files.
    pub fn scan_game_for_legacy_unlockers(game_dir: &Path) -> Vec<LegacyUnlockerFile> {
        let mut results = Vec::new();
        Self::collect_legacy_files_recursive(game_dir, game_dir, 0, &mut results);
        results.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        results
    }

    fn collect_legacy_files_recursive(
        base_dir: &Path,
        current_dir: &Path,
        depth: usize,
        out: &mut Vec<LegacyUnlockerFile>,
    ) {
        if depth > 4 || !current_dir.is_dir() {
            return;
        }
        let Ok(entries) = fs::read_dir(current_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(ft) = entry.file_type() {
                if ft.is_dir() {
                    let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                    if !name.starts_with('.') && name != "redist" && name != "directx" {
                        Self::collect_legacy_files_recursive(base_dir, &path, depth + 1, out);
                    }
                } else if ft.is_file() {
                    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                        continue;
                    };
                    let lower = name.to_ascii_lowercase();
                    let Ok(rel) = path.strip_prefix(base_dir) else {
                        continue;
                    };
                    let rel_str = rel.to_string_lossy().replace('\\', "/");

                    if lower == "smokeapi.dll"
                        || lower == "smokeapi64.dll"
                        || lower == "smokeapi32.dll"
                        || lower == "smokeapi.config.json"
                        || lower == "smokeapi.json"
                    {
                        out.push(LegacyUnlockerFile {
                            relative_path: rel_str,
                            unlocker_kind: "SmokeAPI".to_string(),
                            is_backup_orig: false,
                        });
                    } else if lower.starts_with("greenluma")
                        && (lower.ends_with(".dll") || lower.ends_with(".ini"))
                    {
                        out.push(LegacyUnlockerFile {
                            relative_path: rel_str,
                            unlocker_kind: "GreenLuma".to_string(),
                            is_backup_orig: false,
                        });
                    } else if lower == "bst.dll"
                        || lower == "bst64.dll"
                        || lower == "bst32.dll"
                        || lower == "bettersteamtools.dll"
                        || lower == "steamtools.dll"
                        || lower == "opensteamtool.dll"
                        || lower == "opensteamtools.dll"
                    {
                        out.push(LegacyUnlockerFile {
                            relative_path: rel_str,
                            unlocker_kind: "BST".to_string(),
                            is_backup_orig: false,
                        });
                    } else if lower == "steam_api64_o.dll"
                        || lower == "steam_api_o.dll"
                        || lower == "steam_api64_original.dll"
                        || lower == "steam_api_original.dll"
                        || lower == "steam_api64.dll.bak"
                        || lower == "steam_api.dll.bak"
                    {
                        out.push(LegacyUnlockerFile {
                            relative_path: rel_str,
                            unlocker_kind: "OriginalBackup".to_string(),
                            is_backup_orig: true,
                        });
                    }
                }
            }
        }
    }

    /// Migrates legacy unlockers in `game_dir` by backing up legacy DLLs/configs,
    /// restoring original `steam_api(64).dll` from backups if available, and removing legacy injectors.
    pub fn migrate_legacy_unlockers_in_game(game_dir: &Path) -> LegacyMigrationReport {
        let mut report = LegacyMigrationReport {
            game_dir: game_dir.to_path_buf(),
            ..Default::default()
        };

        let detected = Self::scan_game_for_legacy_unlockers(game_dir);
        if detected.is_empty() {
            return report;
        }
        report.detected_files = detected.clone();

        let backup_root = game_dir.join(".luatools-backup/legacy-unlockers");
        if let Err(e) = fs::create_dir_all(&backup_root) {
            report.errors.push(format!("Failed to create backup directory: {}", e));
            return report;
        }

        // 1. Back up all detected legacy files
        for item in &detected {
            let src = game_dir.join(&item.relative_path);
            let dst = backup_root.join(&item.relative_path);
            if let Some(p) = dst.parent() {
                let _ = fs::create_dir_all(p);
            }
            if let Err(e) = fs::copy(&src, &dst) {
                report.errors.push(format!("Failed to back up '{}': {}", item.relative_path, e));
            } else {
                report.backed_up_files.push(item.relative_path.clone());
            }
        }

        // 2. Restore original DLLs from backups if available
        for item in &detected {
            if item.is_backup_orig {
                let bkp_src = game_dir.join(&item.relative_path);
                let lower = Path::new(&item.relative_path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                let target_name = if lower.contains("64") {
                    "steam_api64.dll"
                } else {
                    "steam_api.dll"
                };
                if let Some(parent) = bkp_src.parent() {
                    let restore_target = parent.join(target_name);
                    if let Err(e) = fs::copy(&bkp_src, &restore_target) {
                        report.errors.push(format!("Failed to restore '{}': {}", target_name, e));
                    } else {
                        report.restored_files.push(target_name.to_string());
                        let _ = fs::remove_file(&bkp_src);
                    }
                }
            }
        }

        // 3. Remove legacy unlocker binaries & configs from game directory
        for item in &detected {
            if !item.is_backup_orig {
                let target = game_dir.join(&item.relative_path);
                if target.exists() {
                    if let Err(e) = fs::remove_file(&target) {
                        report.errors.push(format!("Failed to remove legacy file '{}': {}", item.relative_path, e));
                    } else {
                        report.removed_files.push(item.relative_path.clone());
                    }
                }
            }
        }

        report
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LegacyUnlockerFile {
    pub relative_path: String,
    pub unlocker_kind: String,
    pub is_backup_orig: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LegacyMigrationReport {
    pub game_dir: PathBuf,
    pub detected_files: Vec<LegacyUnlockerFile>,
    pub backed_up_files: Vec<String>,
    pub restored_files: Vec<String>,
    pub removed_files: Vec<String>,
    pub errors: Vec<String>,
}

impl Default for ModeMigration {
    fn default() -> Self {
        Self::new()
    }
}

impl TryFrom<&str> for UnlockerMode {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        let trimmed = s.trim();
        match trimmed.to_ascii_lowercase().as_str() {
            "steamtools" => Ok(UnlockerMode::SteamTools),
            "bst" | "bettersteamtools" | "opensteamtool" | "opensteamtools" => {
                Ok(UnlockerMode::Bst)
            }
            "custom" => Ok(UnlockerMode::Custom),
            "none" => Ok(UnlockerMode::None),
            _ => Err(format!("Unknown mode: {}", trimmed)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_migrate_legacy_steamtools() {
        let migration = ModeMigration::new();
        let result = migration.migrate(Some("SteamTools"));
        assert_eq!(result.new_mode, UnlockerMode::SteamTools);
        assert!(!result.reset_onboarding);
    }

    #[test]
    fn test_migrate_legacy_opensteamtools() {
        let migration = ModeMigration::new();
        let result = migration.migrate(Some("OpenSteamTools"));
        assert_eq!(result.new_mode, UnlockerMode::Bst);
        assert!(!result.reset_onboarding);

        let nightly = migration.migrate(Some("OpenSteamToolsNightly"));
        assert_eq!(nightly.new_mode, UnlockerMode::Bst);
        assert!(!nightly.reset_onboarding);
    }

    #[test]
    fn test_migrate_legacy_unknown() {
        let migration = ModeMigration::new();
        let result = migration.migrate(Some("UnknownMode"));
        assert_eq!(result.new_mode, UnlockerMode::None);
        assert!(result.reset_onboarding);
    }

    #[test]
    fn test_migrate_none_and_whitespace() {
        let migration = ModeMigration::new();
        let result = migration.migrate(None);
        assert_eq!(result.new_mode, UnlockerMode::None);
        assert!(!result.reset_onboarding);

        let none_str = migration.migrate(Some("None"));
        assert_eq!(none_str.new_mode, UnlockerMode::None);
        assert!(!none_str.reset_onboarding);

        let padded = migration.migrate(Some("  bst  "));
        assert_eq!(padded.new_mode, UnlockerMode::Bst);
        assert!(!padded.reset_onboarding);
    }

    #[test]
    fn test_needs_migration_and_apply() {
        assert!(!ModeMigration::needs_migration(None));
        assert!(!ModeMigration::needs_migration(Some("   ")));
        assert!(!ModeMigration::needs_migration(Some("Bst")));
        assert!(!ModeMigration::needs_migration(Some("None")));
        assert!(ModeMigration::needs_migration(Some(
            "OpenSteamToolsNightly"
        )));
        assert!(ModeMigration::needs_migration(Some("UnknownMode")));

        let mut applied = UnlockerMode::None;
        let reset = ModeMigration::new().apply(Some("OpenSteamToolsNightly"), |m| applied = m);
        assert_eq!(applied, UnlockerMode::Bst);
        assert!(!reset);
    }

    #[test]
    fn test_scan_and_migrate_legacy_unlockers() {
        let tmp = tempfile::tempdir().unwrap();
        let game_dir = tmp.path().join("TestGame");
        fs::create_dir_all(&game_dir).unwrap();

        // Seed fake SmokeAPI & BST files and an original backup DLL
        fs::write(game_dir.join("smokeapi64.dll"), b"SMOKEAPI_HOOK").unwrap();
        fs::write(game_dir.join("smokeapi.config.json"), b"{}").unwrap();
        fs::write(game_dir.join("bst.dll"), b"BST_HOOK").unwrap();
        fs::write(game_dir.join("steam_api64.dll"), b"CURRENT_MODDED").unwrap();
        fs::write(game_dir.join("steam_api64_o.dll"), b"ORIGINAL_CLEAN_VALVE").unwrap();

        let scanned = ModeMigration::scan_game_for_legacy_unlockers(&game_dir);
        assert_eq!(scanned.len(), 4);

        let report = ModeMigration::migrate_legacy_unlockers_in_game(&game_dir);
        assert_eq!(report.errors.len(), 0);
        assert_eq!(report.detected_files.len(), 4);
        assert_eq!(report.backed_up_files.len(), 4);
        assert!(report.restored_files.contains(&"steam_api64.dll".to_string()));

        // Legacy hook files removed
        assert!(!game_dir.join("smokeapi64.dll").exists());
        assert!(!game_dir.join("smokeapi.config.json").exists());
        assert!(!game_dir.join("bst.dll").exists());
        assert!(!game_dir.join("steam_api64_o.dll").exists());

        // Original clean DLL restored
        assert_eq!(
            fs::read(game_dir.join("steam_api64.dll")).unwrap(),
            b"ORIGINAL_CLEAN_VALVE"
        );

        // Backup files safely preserved
        let bkp = game_dir.join(".luatools-backup/legacy-unlockers");
        assert!(bkp.join("smokeapi64.dll").exists());
        assert!(bkp.join("steam_api64_o.dll").exists());
    }
}
