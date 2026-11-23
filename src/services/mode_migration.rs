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

/// Legacy mode values from the C# version
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum LegacyMode {
    SteamTools,
    OpenSteamTools,
    OpenSteamToolsNightly,
}

/// Current mode
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum UnlockerMode {
    /// Original SteamTools mode (retired)
    SteamTools,
    /// BetterSteamTools mode (OpenSteamTools upstream)
    Bst,
    /// Custom mode
    Custom,
    /// No mode selected
    None,
}

impl Default for UnlockerMode {
    fn default() -> Self {
        UnlockerMode::None
    }
}

impl Default for LegacyMode {
    fn default() -> Self {
        LegacyMode::SteamTools
    }
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
        let stored = match stored {
            Some(s) if !s.trim().is_empty() => s,
            _ => return MigrationResult::new(UnlockerMode::None, false),
        };

        // Already current mode - leave it be
        if let Ok(current) = UnlockerMode::try_from(stored) {
            return MigrationResult::new(current, false);
        }

        match stored {
            // Both were branded "BetterSteamTools" in the UI, so BST is what these users think they run.
            "OpenSteamTools" | "OpenSteamToolsNightly" => {
                MigrationResult::new(UnlockerMode::Bst, false)
            }

            // SteamTools and the CloudRedirect fix are retired with nothing to map onto.
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
        matches!(stored, Some(s) if !s.trim().is_empty() && UnlockerMode::try_from(s).is_err())
    }
}

impl Default for ModeMigration {
    fn default() -> Self {
        Self::new()
    }
}

impl TryFrom<&str> for UnlockerMode {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "SteamTools" => Ok(UnlockerMode::SteamTools),
            "Bst" | "BetterSteamTools" | "OpenSteamTool" | "OpenSteamTools" => {
                Ok(UnlockerMode::Bst)
            }
            "Custom" => Ok(UnlockerMode::Custom),
            _ => Err(format!("Unknown mode: {}", s)),
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
    }

    #[test]
    fn test_migrate_legacy_unknown() {
        let migration = ModeMigration::new();
        let result = migration.migrate(Some("UnknownMode"));
        assert_eq!(result.new_mode, UnlockerMode::None);
        assert!(result.reset_onboarding);
    }

    #[test]
    fn test_migrate_none() {
        let migration = ModeMigration::new();
        let result = migration.migrate(None);
        assert_eq!(result.new_mode, UnlockerMode::None);
        assert!(!result.reset_onboarding);
    }

    #[test]
    fn test_needs_migration() {
        assert!(!ModeMigration::needs_migration(None));
        assert!(!ModeMigration::needs_migration(Some("Bst")));
        assert!(ModeMigration::needs_migration(Some("UnknownMode")));
    }
}