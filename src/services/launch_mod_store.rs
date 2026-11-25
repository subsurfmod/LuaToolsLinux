//! LaunchModStore — Tracks original vs modified launch configurations in `launchmods.json`
//! and rebases them across Steam PICS updates (LT-58, LT-59).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::models::config::default_luatools_data_dir;
use crate::parsers::vdf_binary::LaunchEntry;
use crate::services::steam_service::SteamService;

const LAUNCHMODS_FILENAME: &str = "launchmods.json";
const MAX_CHANGE_NUMBERS: usize = 10; // Keep last 10 change numbers per app

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LaunchModRecord {
    pub app_id: u32,
    pub change_number: u64,
    pub original_launch_options: Vec<LaunchEntry>,
    pub modified_launch_options: Vec<LaunchEntry>,
    pub applied_at: String,
    pub rebased_from: Option<u64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LaunchModStore {
    #[serde(skip)]
    path: PathBuf,
    pub records: BTreeMap<u32, Vec<LaunchModRecord>>,
}

impl LaunchModStore {
    pub fn new(storage_path: Option<PathBuf>) -> Self {
        let path = storage_path.unwrap_or_else(|| default_luatools_data_dir().join(LAUNCHMODS_FILENAME));
        let records = Self::load_from(&path);
        Self { path, records }
    }

    fn load_from(path: &Path) -> BTreeMap<u32, Vec<LaunchModRecord>> {
        if !path.exists() {
            return BTreeMap::new();
        }
        let Ok(content) = fs::read_to_string(path) else {
            return BTreeMap::new();
        };
        serde_json::from_str(&content).unwrap_or_default()
    }

    pub fn save(&self) -> bool {
        if let Some(parent) = self.path.parent() {
            if fs::create_dir_all(parent).is_err() {
                return false;
            }
        }
        let tmp = self.path.with_extension("json.tmp");
        if let Ok(content) = serde_json::to_string_pretty(&self.records) {
            if fs::write(&tmp, content).is_ok() && fs::rename(&tmp, &self.path).is_ok() {
                return true;
            }
        }
        false
    }

    /// Records a new launch modification for an app.
    pub fn record_modification(
        &mut self,
        app_id: u32,
        change_number: u64,
        original: Vec<LaunchEntry>,
        modified: Vec<LaunchEntry>,
    ) {
        let record = LaunchModRecord {
            app_id,
            change_number,
            original_launch_options: original,
            modified_launch_options: modified,
            applied_at: chrono::Utc::now().to_rfc3339(),
            rebased_from: None,
        };
        self.records.entry(app_id).or_default().push(record);
        // Keep only last MAX_CHANGE_NUMBERS entries per app
        if let Some(entries) = self.records.get_mut(&app_id) {
            if entries.len() > MAX_CHANGE_NUMBERS {
                entries.drain(0..entries.len() - MAX_CHANGE_NUMBERS);
            }
        }
        let _ = self.save();
    }

    /// Gets the latest modification record for an app.
    pub fn get_latest(&self, app_id: u32) -> Option<&LaunchModRecord> {
        self.records.get(&app_id)?.last()
    }

    /// Gets all modification records for an app.
    pub fn get_all(&self, app_id: u32) -> Option<&Vec<LaunchModRecord>> {
        self.records.get(&app_id)
    }

    /// Removes records for an app (e.g., when game is uninstalled).
    pub fn remove_app(&mut self, app_id: u32) -> bool {
        let removed = self.records.remove(&app_id).is_some();
        if removed {
            let _ = self.save();
        }
        removed
    }

    /// Checks if Steam PICS has overwritten launch options by comparing change numbers.
    /// Returns true if drift detected.
    pub fn check_drift(&self, steam_service: &SteamService, app_id: u32) -> Option<DriftResult> {
        let Some(record) = self.get_latest(app_id) else {
            return None;
        };

        let Some(installation) = &steam_service.installation else {
            return None;
        };

        let appinfo_path = installation.root.join("appcache").join("appinfo.vdf");
        if !appinfo_path.exists() {
            return None;
        }

        // Parse current appinfo.vdf and extract change number
        let current_change_number = crate::parsers::vdf_binary::AppInfoFile::extract_app_change_number(&appinfo_path, app_id)?;

        if current_change_number > record.change_number {
            // Steam PICS has updated this app - check if our modifications are still present
            let current_launch_options = crate::parsers::vdf_binary::AppInfoFile::extract_launch_options(&appinfo_path, app_id)?;

            let is_drifted = !launch_options_match(&current_launch_options, &record.modified_launch_options);

            if is_drifted {
                return Some(DriftResult {
                    app_id,
                    steam_change_number: current_change_number,
                    recorded_change_number: record.change_number,
                    current_options: current_launch_options,
                    expected_options: record.modified_launch_options.clone(),
                });
            }
        }
        None
    }

    /// Rebases modifications onto new Steam PICS data.
    pub fn rebase(&mut self, app_id: u32, new_change_number: u64, new_original: Vec<LaunchEntry>) -> Option<Vec<LaunchEntry>> {
        let Some(record) = self.get_latest(app_id).cloned() else {
            return None;
        };

        // Compute the diff between old original and new original, apply to modified
        let rebased = apply_launch_diff(&record.original_launch_options, &record.modified_launch_options, &new_original);

        // Record the rebase
        let new_record = LaunchModRecord {
            app_id,
            change_number: new_change_number,
            original_launch_options: new_original,
            modified_launch_options: rebased.clone(),
            applied_at: chrono::Utc::now().to_rfc3339(),
            rebased_from: Some(record.change_number),
        };

        self.records.entry(app_id).or_default().push(new_record);
        if let Some(entries) = self.records.get_mut(&app_id) {
            if entries.len() > MAX_CHANGE_NUMBERS {
                entries.drain(0..entries.len() - MAX_CHANGE_NUMBERS);
            }
        }
        let _ = self.save();

        Some(rebased)
    }
}

#[derive(Debug, Clone)]
pub struct DriftResult {
    pub app_id: u32,
    pub steam_change_number: u64,
    pub recorded_change_number: u64,
    pub current_options: Vec<LaunchEntry>,
    pub expected_options: Vec<LaunchEntry>,
}

fn launch_options_match(a: &[LaunchEntry], b: &[LaunchEntry]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for (oa, ob) in a.iter().zip(b.iter()) {
        if oa.description != ob.description || oa.arguments != ob.arguments {
            return false;
        }
    }
    true
}

fn apply_launch_diff(old_original: &[LaunchEntry], modified: &[LaunchEntry], new_original: &[LaunchEntry]) -> Vec<LaunchEntry> {
    // Simple approach: for each modified entry, try to find matching entry in new_original by description
    // and preserve the modified command. If not found, keep the modified entry as-is.
    let mut result = new_original.to_vec();

    for mod_entry in modified {
        // Find matching entry in old_original
        let old_idx = old_original.iter().position(|e| e.description == mod_entry.description && e.arguments == mod_entry.arguments);

        if let Some(idx) = old_idx {
            // Find corresponding entry in new_original (by same index if possible, or by description)
            let new_idx = if idx < new_original.len() && new_original[idx].description == old_original[idx].description {
                Some(idx)
            } else {
                new_original.iter().position(|e| e.description == mod_entry.description)
            };

            if let Some(nidx) = new_idx {
                // Update the command while preserving description
                result[nidx] = LaunchEntry {
                    index: new_original[nidx].index.clone(),
                    executable: new_original[nidx].executable.clone(),
                    arguments: mod_entry.arguments.clone(),
                    workingdir: new_original[nidx].workingdir.clone(),
                    description: new_original[nidx].description.clone(),
                    launch_type: new_original[nidx].launch_type.clone(),
                    oslist: new_original[nidx].oslist.clone(),
                    osarch: new_original[nidx].osarch.clone(),
                };
            } else {
                // Add as new entry
                result.push(mod_entry.clone());
            }
        } else {
            // No matching original - keep as-is
            result.push(mod_entry.clone());
        }
    }

    result
}

/// Runs drift detection on all recorded apps and returns any drifted apps.
pub fn check_all_drift(steam_service: &SteamService, store: &LaunchModStore) -> Vec<DriftResult> {
    let mut drifted = Vec::new();
    for (app_id, _) in &store.records {
        if let Some(drift) = store.check_drift(steam_service, *app_id) {
            drifted.push(drift);
        }
    }
    drifted
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use crate::parsers::vdf_binary::LaunchEntry;

    #[test]
    fn test_launch_mod_store() {
        let dir = tempdir().unwrap();
        let mut store = LaunchModStore::new(Some(dir.path().join("launchmods.json")));

        let original = vec![
            LaunchEntry { index: "0".into(), executable: "".into(), arguments: "".into(), workingdir: "".into(), description: "Default".into(), launch_type: "default".into(), oslist: "".into(), osarch: "".into() },
            LaunchEntry { index: "1".into(), executable: "".into(), arguments: "-vulkan".into(), workingdir: "".into(), description: "Vulkan".into(), launch_type: "option1".into(), oslist: "".into(), osarch: "".into() },
        ];
        let modified = vec![
            LaunchEntry { index: "0".into(), executable: "".into(), arguments: "".into(), workingdir: "".into(), description: "Default".into(), launch_type: "default".into(), oslist: "".into(), osarch: "".into() },
            LaunchEntry { index: "1".into(), executable: "".into(), arguments: "-vulkan -novid".into(), workingdir: "".into(), description: "Vulkan".into(), launch_type: "option1".into(), oslist: "".into(), osarch: "".into() },
        ];

        store.record_modification(123, 100, original.clone(), modified.clone());
        let saved = store.get_latest(123).unwrap();
        assert_eq!(saved.change_number, 100);
        assert_eq!(saved.modified_launch_options.len(), 2);
        assert_eq!(saved.modified_launch_options[1].arguments, "-vulkan -novid");
    }
}