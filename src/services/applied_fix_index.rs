//! AppliedFixIndexService — Central registry of applied fixes across all library folders (LT-76).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::models::config::default_luatools_data_dir;
use crate::services::steam_service::SteamService;

// Custom deserializer for Option<PathBuf> that gracefully handles missing, null, or invalid values
fn deserialize_option_path_buf<'de, D>(deserializer: D) -> Result<Option<PathBuf>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = serde::Deserialize::deserialize(deserializer)?;
    Ok(opt.map(PathBuf::from))
}

const APPLIED_FIXES_FILENAME: &str = "applied-fixes.json";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedFixRecord {
    pub app_id: u32,
    pub fix_id: String,
    pub applied_at: String,
    #[serde(default, deserialize_with = "deserialize_option_path_buf")]
    pub game_dir: Option<PathBuf>,
    pub files: Vec<AppliedFixFileEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wine_dll_overrides: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedFixFileEntry {
    pub relative_path: String,
    pub action: String, // "modified" | "added"
    pub backup_path: Option<String>,
    pub hash_before: Option<String>,
    pub hash_after: String,
}

#[derive(Debug, Clone)]
pub struct AppliedFixIndexService {
    path: PathBuf,
    pub records: BTreeMap<u32, Vec<AppliedFixRecord>>,
}

impl Default for AppliedFixIndexService {
    fn default() -> Self {
        Self::new(None)
    }
}

impl AppliedFixIndexService {
    pub fn new(storage_path: Option<PathBuf>) -> Self {
        let path = storage_path.unwrap_or_else(|| default_luatools_data_dir().join(APPLIED_FIXES_FILENAME));
        let records = Self::load_from(&path);
        Self { path, records }
    }

    fn load_from(path: &Path) -> BTreeMap<u32, Vec<AppliedFixRecord>> {
        for candidate in [path.to_path_buf(), path.with_extension("json.bak")] {
            if !candidate.exists() {
                continue;
            }
            let Ok(content) = fs::read_to_string(&candidate) else {
                continue;
            };
            if let Ok(records) = serde_json::from_str::<BTreeMap<u32, Vec<AppliedFixRecord>>>(&content) {
                return records;
            }
        }
        BTreeMap::new()
    }

    pub fn save(&self) -> bool {
        if let Some(parent) = self.path.parent() {
            if fs::create_dir_all(parent).is_err() {
                return false;
            }
        }
        let tmp = self.path.with_extension("json.tmp");
        if let Ok(content) = serde_json::to_string_pretty(&self.records) {
            if fs::write(&tmp, content).is_ok() {
                if self.path.exists() {
                    let _ = fs::copy(&self.path, self.path.with_extension("json.bak"));
                }
                if fs::rename(&tmp, &self.path).is_ok() {
                    return true;
                }
            }
        }
        false
    }

    /// Adds or replaces an applied fix record in the index (deduplicated by `(app_id, fix_id)`).
    pub fn add_fix(&mut self, record: AppliedFixRecord) {
        let entries = self.records.entry(record.app_id).or_default();
        entries.retain(|r| r.fix_id != record.fix_id);
        entries.push(record);
        let _ = self.save();
    }

    /// Gets all applied fixes for an app.
    pub fn get_fixes(&self, app_id: u32) -> Option<&Vec<AppliedFixRecord>> {
        self.records.get(&app_id)
    }

    /// Gets the latest applied fix for an app matching the fix_id.
    pub fn get_fix(&self, app_id: u32, fix_id: &str) -> Option<&AppliedFixRecord> {
        self.records.get(&app_id)?.iter().rev().find(|r| r.fix_id == fix_id)
    }

    /// Checks if a fix is applied for an app.
    pub fn is_fix_applied(&self, app_id: u32, fix_id: &str) -> bool {
        self.get_fix(app_id, fix_id).is_some()
    }

    /// Removes a fix record from the index (e.g., after unfix).
    pub fn remove_fix(&mut self, app_id: u32, fix_id: &str) -> bool {
        let (removed, is_empty) = {
            let Some(entries) = self.records.get_mut(&app_id) else {
                return false;
            };
            let len_before = entries.len();
            entries.retain(|r| r.fix_id != fix_id);
            (entries.len() < len_before, entries.is_empty())
        };
        if is_empty {
            self.records.remove(&app_id);
        }
        if removed {
            let _ = self.save();
        }
        removed
    }

    /// Removes all fixes for an app (e.g., when game is uninstalled).
    pub fn remove_app(&mut self, app_id: u32) -> bool {
        let removed = self.records.remove(&app_id).is_some();
        if removed {
            let _ = self.save();
        }
        removed
    }

    /// Rebuilds the index by scanning all library folders for `.luatools-fix/` directories.
    pub fn rebuild_index(&mut self, steam_service: &SteamService) -> usize {
        let mut new_records: BTreeMap<u32, Vec<AppliedFixRecord>> = BTreeMap::new();
        let mut count = 0;

        for lib in steam_service.libraries() {
            let mut search_dirs = vec![
                lib.path.join(crate::services::fixes_service::FIX_DIR_NAME),
            ];
            for sub in ["steamapps/common", "common"] {
                let common_path = lib.path.join(sub);
                if let Ok(entries) = fs::read_dir(&common_path) {
                    for entry in entries.flatten() {
                        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                            search_dirs.push(entry.path().join(crate::services::fixes_service::FIX_DIR_NAME));
                        }
                    }
                }
            }

            for fix_dir in search_dirs {
                if !fix_dir.exists() || !fix_dir.is_dir() {
                    continue;
                }

                if let Ok(entries) = fs::read_dir(&fix_dir) {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if p.extension().and_then(|e| e.to_str()) != Some("json") {
                            continue;
                        }

                        if let Ok(content) = fs::read_to_string(&p) {
                            if let Ok(mut record) = serde_json::from_str::<AppliedFixRecord>(&content) {
                                if record.game_dir.is_none() {
                                    record.game_dir = fix_dir.parent().map(Path::to_path_buf);
                                }
                                let list = new_records.entry(record.app_id).or_default();
                                if !list.iter().any(|r| r.fix_id == record.fix_id) {
                                    list.push(record);
                                    count += 1;
                                }
                            }
                        }
                    }
                }
            }
        }

        self.records = new_records;
        let _ = self.save();
        count
    }

    /// Gets all applied fixes across all apps.
    pub fn all_fixes(&self) -> Vec<&AppliedFixRecord> {
        self.records.values().flatten().collect()
    }

    /// Returns only fixes whose `.luatools-fix/<fix_id>.json` record still exists on disk,
    /// pruning stale entries from the central index.
    pub fn list_verified_fixes(&mut self) -> Vec<AppliedFixRecord> {
        let mut changed = false;
        let mut verified = Vec::new();
        let app_ids: Vec<u32> = self.records.keys().copied().collect();

        for app_id in app_ids {
            let mut remove_empty = false;
            if let Some(entries) = self.records.get_mut(&app_id) {
                let before = entries.len();
                entries.retain(|r| match &r.game_dir {
                    Some(dir) => crate::services::fixes_service::FixesService::is_fix_applied(dir, &r.fix_id),
                    None => true,
                });
                if entries.len() != before {
                    changed = true;
                }
                verified.extend(entries.iter().cloned());
                remove_empty = entries.is_empty();
            }
            if remove_empty {
                self.records.remove(&app_id);
                changed = true;
            }
        }

        if changed {
            let _ = self.save();
        }
        verified
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::path::PathBuf;

    #[test]
    fn test_applied_fix_index() {
        let dir = tempdir().unwrap();
        let idx_path = dir.path().join("applied-fixes.json");
        let mut idx = AppliedFixIndexService::new(Some(idx_path.clone()));

        let record = AppliedFixRecord {
            app_id: 123,
            fix_id: "online-fix:abc123".into(),
            applied_at: chrono::Utc::now().to_rfc3339(),
            game_dir: Some(PathBuf::from("/home/user/games/123")),
            files: vec![AppliedFixFileEntry {
                relative_path: "winmm.dll".into(),
                action: "modified".into(),
                backup_path: Some("backups/online-fix_abc123/winmm.dll".into()),
                hash_before: Some("aaaa".into()),
                hash_after: "bbbb".into(),
            }],
            wine_dll_overrides: Some("winmm=n,b".into()),
        };

        idx.add_fix(record.clone());
        // Adding same (app_id, fix_id) replaces instead of duplicating
        idx.add_fix(record.clone());
        assert!(idx.is_fix_applied(123, "online-fix:abc123"));
        assert_eq!(idx.get_fixes(123).unwrap().len(), 1);
        let stored = idx.get_fix(123, "online-fix:abc123").unwrap();
        assert_eq!(stored.files.len(), 1);
        assert_eq!(stored.files[0].hash_before.as_deref(), Some("aaaa"));
        assert_eq!(stored.files[0].hash_after, "bbbb");
        assert_eq!(stored.wine_dll_overrides.as_deref(), Some("winmm=n,b"));

        // Corrupt primary file recovers cleanly from .json.bak
        let _ = idx.save();
        fs::write(&idx_path, "{corrupt json").unwrap();
        let recovered = AppliedFixIndexService::new(Some(idx_path));
        assert!(recovered.is_fix_applied(123, "online-fix:abc123"));

        idx.remove_fix(123, "online-fix:abc123");
        assert!(!idx.is_fix_applied(123, "online-fix:abc123"));
        assert!(idx.get_fixes(123).is_none());
    }
}