//! LaunchModStore — Tracks original vs modified launch configurations in `launchmods.json`,
//! rebases them across Steam PICS updates (LT-58, LT-59), and coordinates Steam process
//! shutdown/relaunch around `appinfo.vdf` writes so Steam never overwrites edits on exit (LT-57).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::models::config::default_luatools_data_dir;
use crate::parsers::vdf_binary::{AppInfoFile, LaunchEntry, LaunchMod, LaunchOptionsService};
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

impl Default for LaunchModStore {
    fn default() -> Self {
        Self::new(None)
    }
}

impl LaunchModStore {
    pub fn new(storage_path: Option<PathBuf>) -> Self {
        let is_default_path = storage_path.is_none();
        let path =
            storage_path.unwrap_or_else(|| default_luatools_data_dir().join(LAUNCHMODS_FILENAME));
        let mut records = Self::load_from(&path);

        // One-time transparent migration from legacy ~/.config/luatools/launchmods.json if present
        if is_default_path && records.is_empty() && !path.exists() {
            if let Some(cfg_dir) = dirs::config_dir() {
                let legacy_path = cfg_dir.join("luatools").join(LAUNCHMODS_FILENAME);
                if legacy_path.exists() {
                    records = Self::load_from(&legacy_path);
                    if !records.is_empty() {
                        let migrated = Self {
                            path: path.clone(),
                            records: records.clone(),
                        };
                        let _ = migrated.save();
                    }
                }
            }
        }

        Self { path, records }
    }

    fn load_from(path: &Path) -> BTreeMap<u32, Vec<LaunchModRecord>> {
        let candidate_paths = [path.to_path_buf(), path.with_extension("json.bak")];
        for p in &candidate_paths {
            if !p.exists() {
                continue;
            }
            let Ok(content) = fs::read_to_string(p) else {
                continue;
            };
            if let Ok(map) = serde_json::from_str::<BTreeMap<u32, Vec<LaunchModRecord>>>(&content) {
                return map;
            }
            // Support legacy Vec<LaunchMod> format
            if let Ok(legacy_list) = serde_json::from_str::<Vec<LaunchMod>>(&content) {
                let mut migrated: BTreeMap<u32, Vec<LaunchModRecord>> = BTreeMap::new();
                for m in legacy_list {
                    migrated.entry(m.app_id).or_default().push(LaunchModRecord {
                        app_id: m.app_id,
                        change_number: m.change_number as u64,
                        original_launch_options: m.original,
                        modified_launch_options: m.desired,
                        applied_at: m.saved_at,
                        rebased_from: None,
                    });
                }
                return migrated;
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
        self.push_record(app_id, record);
        let _ = self.save();
    }

    fn push_record(&mut self, app_id: u32, record: LaunchModRecord) {
        let entries = self.records.entry(app_id).or_default();
        entries.push(record);
        if entries.len() > MAX_CHANGE_NUMBERS {
            entries.drain(0..entries.len() - MAX_CHANGE_NUMBERS);
        }
    }

    /// Gets the latest modification record for an app.
    pub fn get_latest(&self, app_id: u32) -> Option<&LaunchModRecord> {
        self.records.get(&app_id)?.last()
    }

    /// Gets all modification records for an app.
    pub fn get_all(&self, app_id: u32) -> Option<&Vec<LaunchModRecord>> {
        self.records.get(&app_id)
    }

    /// Removes records for an app (e.g., when game is uninstalled or backup restored).
    pub fn remove_app(&mut self, app_id: u32) -> bool {
        let removed = self.records.remove(&app_id).is_some();
        if removed {
            let _ = self.save();
        }
        removed
    }

    /// Checks if Steam PICS or a Steam client flush has overwritten our modified launch options.
    pub fn check_drift(&self, steam_service: &SteamService, app_id: u32) -> Option<DriftResult> {
        let record = self.get_latest(app_id)?;
        let installation = steam_service.installation.as_ref()?;
        let appinfo_path = installation.root.join("appcache").join("appinfo.vdf");
        if !appinfo_path.exists() {
            return None;
        }

        let current_change_number =
            AppInfoFile::extract_app_change_number(&appinfo_path, app_id)?;
        let current_launch_options =
            AppInfoFile::extract_launch_options(&appinfo_path, app_id)?;

        let is_drifted =
            !launch_options_match(&current_launch_options, &record.modified_launch_options);

        if is_drifted {
            return Some(DriftResult {
                app_id,
                steam_change_number: current_change_number,
                recorded_change_number: record.change_number,
                current_options: current_launch_options,
                expected_options: record.modified_launch_options.clone(),
            });
        }
        None
    }

    /// Computes rebased launch options without mutating the store yet.
    pub fn preview_rebase(&self, app_id: u32, new_original: &[LaunchEntry]) -> Option<Vec<LaunchEntry>> {
        let record = self.get_latest(app_id)?;
        Some(apply_launch_diff(
            &record.original_launch_options,
            &record.modified_launch_options,
            new_original,
        ))
    }

    /// Rebases modifications onto new Steam PICS data and persists the rebased record.
    pub fn rebase(
        &mut self,
        app_id: u32,
        new_change_number: u64,
        new_original: Vec<LaunchEntry>,
    ) -> Option<Vec<LaunchEntry>> {
        let record = self.get_latest(app_id)?.clone();
        let rebased = apply_launch_diff(
            &record.original_launch_options,
            &record.modified_launch_options,
            &new_original,
        );

        let new_record = LaunchModRecord {
            app_id,
            change_number: new_change_number,
            original_launch_options: new_original,
            modified_launch_options: rebased.clone(),
            applied_at: chrono::Utc::now().to_rfc3339(),
            rebased_from: Some(record.change_number),
        };

        self.push_record(app_id, new_record);
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

fn normalized_launch_type(t: &str) -> &str {
    let trimmed = t.trim();
    if trimmed.is_empty() {
        "default"
    } else {
        trimmed
    }
}

pub fn launch_entry_semantic_eq(a: &LaunchEntry, b: &LaunchEntry) -> bool {
    a.executable.trim() == b.executable.trim()
        && a.arguments.trim() == b.arguments.trim()
        && a.workingdir.trim() == b.workingdir.trim()
        && a.description.trim() == b.description.trim()
        && normalized_launch_type(&a.launch_type) == normalized_launch_type(&b.launch_type)
        && a.oslist.trim() == b.oslist.trim()
        && a.osarch.trim() == b.osarch.trim()
}

pub fn launch_options_match(a: &[LaunchEntry], b: &[LaunchEntry]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .all(|(oa, ob)| launch_entry_semantic_eq(oa, ob))
}

fn find_matching_entry_index(
    target: &LaunchEntry,
    candidates: &[LaunchEntry],
    claimed: &[bool],
) -> Option<usize> {
    let t_exe = target.executable.trim();
    let t_desc = target.description.trim();
    let t_idx = target.index.trim();

    // 1. Exact semantic match (preferring same slot index first)
    if let Some(i) = candidates.iter().enumerate().position(|(i, c)| {
        !claimed[i] && c.index.trim() == t_idx && launch_entry_semantic_eq(c, target)
    }) {
        return Some(i);
    }
    if let Some(i) = candidates
        .iter()
        .enumerate()
        .position(|(i, c)| !claimed[i] && launch_entry_semantic_eq(c, target))
    {
        return Some(i);
    }

    // 2. Same slot index + matching executable or non-empty description
    if let Some(i) = candidates.iter().enumerate().position(|(i, c)| {
        !claimed[i]
            && c.index.trim() == t_idx
            && ((!t_exe.is_empty() && c.executable.trim() == t_exe)
                || (!t_desc.is_empty() && c.description.trim() == t_desc))
    }) {
        return Some(i);
    }

    // 3. Matching both non-empty executable and non-empty description at any index
    if !t_exe.is_empty() && !t_desc.is_empty() {
        if let Some(i) = candidates.iter().enumerate().position(|(i, c)| {
            !claimed[i] && c.executable.trim() == t_exe && c.description.trim() == t_desc
        }) {
            return Some(i);
        }
    }

    // 4. Matching non-empty description or non-empty executable
    if !t_desc.is_empty() {
        if let Some(i) = candidates
            .iter()
            .enumerate()
            .position(|(i, c)| !claimed[i] && c.description.trim() == t_desc)
        {
            return Some(i);
        }
    }
    if !t_exe.is_empty() {
        if let Some(i) = candidates
            .iter()
            .enumerate()
            .position(|(i, c)| !claimed[i] && c.executable.trim() == t_exe)
        {
            return Some(i);
        }
    }

    // 5. Fallback: same slot index (e.g., user replaced both executable and description in-place)
    candidates
        .iter()
        .enumerate()
        .position(|(i, c)| !claimed[i] && c.index.trim() == t_idx)
}

fn merge_launch_entry(
    old_orig: &LaunchEntry,
    modified: &LaunchEntry,
    new_orig: &LaunchEntry,
) -> LaunchEntry {
    let pick = |old_v: &str, mod_v: &str, new_v: &str| -> String {
        if mod_v.trim() != old_v.trim() {
            mod_v.trim().to_string()
        } else {
            new_v.trim().to_string()
        }
    };

    let launch_type = if normalized_launch_type(&modified.launch_type)
        != normalized_launch_type(&old_orig.launch_type)
    {
        normalized_launch_type(&modified.launch_type).to_string()
    } else {
        normalized_launch_type(&new_orig.launch_type).to_string()
    };

    LaunchEntry {
        index: new_orig.index.clone(),
        executable: pick(&old_orig.executable, &modified.executable, &new_orig.executable),
        arguments: pick(&old_orig.arguments, &modified.arguments, &new_orig.arguments),
        workingdir: pick(&old_orig.workingdir, &modified.workingdir, &new_orig.workingdir),
        description: pick(&old_orig.description, &modified.description, &new_orig.description),
        launch_type,
        oslist: pick(&old_orig.oslist, &modified.oslist, &new_orig.oslist),
        osarch: pick(&old_orig.osarch, &modified.osarch, &new_orig.osarch),
    }
}

/// Three-way PICS launch entry rebase:
/// Preserves field-level user customizations and user-added entries from `modified`
/// (relative to `old_original`) while adopting upstream PICS changes in `new_original`.
pub fn apply_launch_diff(
    old_original: &[LaunchEntry],
    modified: &[LaunchEntry],
    new_original: &[LaunchEntry],
) -> Vec<LaunchEntry> {
    let mut old_claimed_by_mod = vec![false; old_original.len()];
    let mut mod_to_old: Vec<Option<usize>> = Vec::with_capacity(modified.len());
    let mut old_to_mod: Vec<Option<usize>> = vec![None; old_original.len()];

    for (m_idx, mod_entry) in modified.iter().enumerate() {
        if let Some(o_idx) =
            find_matching_entry_index(mod_entry, old_original, &old_claimed_by_mod)
        {
            old_claimed_by_mod[o_idx] = true;
            mod_to_old.push(Some(o_idx));
            old_to_mod[o_idx] = Some(m_idx);
        } else {
            mod_to_old.push(None);
        }
    }

    let mut new_claimed_by_old = vec![false; new_original.len()];
    let mut old_to_new: Vec<Option<usize>> = vec![None; old_original.len()];
    let mut new_to_old: Vec<Option<usize>> = vec![None; new_original.len()];

    for (o_idx, old_entry) in old_original.iter().enumerate() {
        if let Some(n_idx) =
            find_matching_entry_index(old_entry, new_original, &new_claimed_by_old)
        {
            new_claimed_by_old[n_idx] = true;
            old_to_new[o_idx] = Some(n_idx);
            new_to_old[n_idx] = Some(o_idx);
        }
    }

    let mut result = Vec::new();

    // 1. Walk upstream new_original entries in order, applying field-level user diffs
    for (n_idx, new_entry) in new_original.iter().enumerate() {
        if let Some(o_idx) = new_to_old[n_idx] {
            if let Some(m_idx) = old_to_mod[o_idx] {
                result.push(merge_launch_entry(
                    &old_original[o_idx],
                    &modified[m_idx],
                    new_entry,
                ));
            } else {
                // User deleted this entry in modified; keep deleted unless Valve changed it in PICS
                if !launch_entry_semantic_eq(&old_original[o_idx], new_entry) {
                    result.push(new_entry.clone());
                }
            }
        } else {
            // Brand-new launch slot introduced by Steam PICS
            result.push(new_entry.clone());
        }
    }

    // 2. Preserve customized old slots that Valve removed in PICS, plus user-added custom slots
    for (m_idx, mod_entry) in modified.iter().enumerate() {
        match mod_to_old[m_idx] {
            Some(o_idx) => {
                if old_to_new[o_idx].is_none()
                    && !launch_entry_semantic_eq(&old_original[o_idx], mod_entry)
                    && !result.iter().any(|r| launch_entry_semantic_eq(r, mod_entry))
                {
                    result.push(mod_entry.clone());
                }
            }
            None => {
                if !result.iter().any(|r| launch_entry_semantic_eq(r, mod_entry)) {
                    result.push(mod_entry.clone());
                }
            }
        }
    }

    // 3. Normalize sequential slot indices ("0", "1", ...)
    for (idx, entry) in result.iter_mut().enumerate() {
        entry.index = idx.to_string();
    }

    result
}

/// Runs drift detection on all recorded apps and returns any drifted apps.
pub fn check_all_drift(steam_service: &SteamService, store: &LaunchModStore) -> Vec<DriftResult> {
    let mut drifted = Vec::new();
    for app_id in store.records.keys() {
        if let Some(drift) = store.check_drift(steam_service, *app_id) {
            drifted.push(drift);
        }
    }
    drifted
}

// ============================================================================
// LT-57: Coordinated Steam Shutdown & Relaunch Around appinfo.vdf Edits
// ============================================================================

pub trait SteamProcessController {
    fn is_running(&self) -> bool;
    fn shutdown(&self) -> Result<usize, String>;
    fn restart(&self, steam_root: &Path) -> Result<(), String>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemSteamProcessController;

impl SteamProcessController for SystemSteamProcessController {
    fn is_running(&self) -> bool {
        SteamService::is_steam_running()
            || !crate::steam_bridge::preload_loader::find_all_steam_pids().is_empty()
    }

    fn shutdown(&self) -> Result<usize, String> {
        let terminated = SteamService::terminate_steam_instances()?;
        for _ in 0..10 {
            if !self.is_running() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if self.is_running() {
            return Err(
                "Steam is still running after shutdown attempt; aborting appinfo.vdf modification so Steam cannot overwrite changes on exit."
                    .to_string(),
            );
        }
        // Allow kernel/filesystem flush from exiting Steam processes to settle
        std::thread::sleep(Duration::from_millis(150));
        Ok(terminated)
    }

    fn restart(&self, steam_root: &Path) -> Result<(), String> {
        SteamService::relaunch_steam_sls(Some(steam_root))
    }
}

#[derive(Debug, Clone)]
pub struct CoordinatedAppInfoResult {
    pub message: String,
    pub steam_was_running: bool,
    pub steam_restarted: bool,
    pub restart_error: Option<String>,
    pub entries: Vec<LaunchEntry>,
}

/// Checks whether any Steam client or helper process is currently running.
pub fn is_any_steam_running() -> bool {
    SystemSteamProcessController.is_running()
}

/// Coordinates shutting down Steam (if running), saving modified launch options to `appinfo.vdf`,
/// recording the baseline & modified snapshot in `LaunchModStore`, and restarting Steam via SLSsteam.
pub fn save_launch_options_coordinated(
    steam_root: &Path,
    app_id: u32,
    desired_entries: &[LaunchEntry],
    store: &mut LaunchModStore,
) -> Result<CoordinatedAppInfoResult, String> {
    save_launch_options_with_controller(
        steam_root,
        app_id,
        desired_entries,
        store,
        &SystemSteamProcessController,
    )
}

pub fn save_launch_options_with_controller(
    steam_root: &Path,
    app_id: u32,
    desired_entries: &[LaunchEntry],
    store: &mut LaunchModStore,
    controller: &dyn SteamProcessController,
) -> Result<CoordinatedAppInfoResult, String> {
    let svc = LaunchOptionsService::new(steam_root.to_path_buf());
    let appinfo_path = svc.appinfo_path();
    if !appinfo_path.exists() {
        return Err("appinfo.vdf not found.".to_string());
    }

    let was_running = controller.is_running();
    if was_running {
        controller.shutdown()?;
    }

    // Read post-shutdown PICS change_number & launch entries so any Steam exit flush is captured
    let change_number = AppInfoFile::extract_app_change_number(&appinfo_path, app_id).unwrap_or(0);
    let live_before_write = svc.get_launch_entries(app_id);
    let baseline_original = match store.get_latest(app_id) {
        Some(existing) if existing.change_number == change_number => {
            existing.original_launch_options.clone()
        }
        _ => live_before_write,
    };

    match svc.save_launch_entries(app_id, desired_entries) {
        Ok(base_msg) => {
            let reloaded = svc.get_launch_entries(app_id);
            let persisted_modified = if reloaded.is_empty() && !desired_entries.is_empty() {
                desired_entries.to_vec()
            } else {
                reloaded.clone()
            };
            store.record_modification(
                app_id,
                change_number,
                baseline_original,
                persisted_modified.clone(),
            );

            let mut steam_restarted = false;
            let mut restart_error = None;
            let message = if was_running {
                match controller.restart(steam_root) {
                    Ok(()) => {
                        steam_restarted = true;
                        format!("{} Steam cleanly restarted.", base_msg)
                    }
                    Err(e) => {
                        restart_error = Some(e.clone());
                        format!("{} (Warning: failed to restart Steam: {})", base_msg, e)
                    }
                }
            } else {
                base_msg
            };

            Ok(CoordinatedAppInfoResult {
                message,
                steam_was_running: was_running,
                steam_restarted,
                restart_error,
                entries: persisted_modified,
            })
        }
        Err(write_err) => {
            if was_running {
                let _ = controller.restart(steam_root);
            }
            Err(write_err)
        }
    }
}

/// Coordinates shutting down Steam (if running), restoring `appinfo.vdf.luatools.bak`,
/// clearing superseded `LaunchModStore` records, and restarting Steam if it was running.
pub fn restore_launch_backup_coordinated(
    steam_root: &Path,
    app_id: u32,
    store: &mut LaunchModStore,
) -> Result<CoordinatedAppInfoResult, String> {
    restore_launch_backup_with_controller(
        steam_root,
        app_id,
        store,
        &SystemSteamProcessController,
    )
}

pub fn restore_launch_backup_with_controller(
    steam_root: &Path,
    app_id: u32,
    store: &mut LaunchModStore,
    controller: &dyn SteamProcessController,
) -> Result<CoordinatedAppInfoResult, String> {
    let svc = LaunchOptionsService::new(steam_root.to_path_buf());
    if !svc.backup_path().exists() {
        return Err("No appinfo.vdf.luatools.bak backup found.".to_string());
    }

    let was_running = controller.is_running();
    if was_running {
        controller.shutdown()?;
    }

    match svc.restore_backup() {
        Ok(base_msg) => {
            let reloaded = svc.get_launch_entries(app_id);

            // Clear current app and any tracked apps that now match their original baseline
            store.remove_app(app_id);
            let tracked_ids: Vec<u32> = store.records.keys().copied().collect();
            for tid in tracked_ids {
                let live = svc.get_launch_entries(tid);
                if let Some(rec) = store.get_latest(tid) {
                    if launch_options_match(&live, &rec.original_launch_options) {
                        store.remove_app(tid);
                    }
                }
            }

            let mut steam_restarted = false;
            let mut restart_error = None;
            let message = if was_running {
                match controller.restart(steam_root) {
                    Ok(()) => {
                        steam_restarted = true;
                        format!("{} Steam cleanly restarted.", base_msg)
                    }
                    Err(e) => {
                        restart_error = Some(e.clone());
                        format!("{} (Warning: failed to restart Steam: {})", base_msg, e)
                    }
                }
            } else {
                base_msg
            };

            Ok(CoordinatedAppInfoResult {
                message,
                steam_was_running: was_running,
                steam_restarted,
                restart_error,
                entries: reloaded,
            })
        }
        Err(restore_err) => {
            if was_running {
                let _ = controller.restart(steam_root);
            }
            Err(restore_err)
        }
    }
}

/// Checks all recorded apps for Steam PICS drift at startup, coordinating Steam shutdown/restart
/// if needed, rebasing modified launch options onto new PICS snapshots, and writing them back.
pub fn reconcile_startup_drift(steam_service: &SteamService, store: &mut LaunchModStore) -> usize {
    reconcile_startup_drift_with_controller(steam_service, store, &SystemSteamProcessController)
}

pub fn reconcile_startup_drift_with_controller(
    steam_service: &SteamService,
    store: &mut LaunchModStore,
    controller: &dyn SteamProcessController,
) -> usize {
    if store.records.is_empty() {
        return 0;
    }

    let initial_drifts = check_all_drift(steam_service, store);
    if initial_drifts.is_empty() {
        return 0;
    }

    let Some(installation) = steam_service.installation.as_ref() else {
        return 0;
    };
    let root = &installation.root;

    let was_running = controller.is_running();
    if was_running && controller.shutdown().is_err() {
        return 0;
    }

    let svc = LaunchOptionsService::new(root.clone());
    let tracked_ids: Vec<u32> = store.records.keys().copied().collect();
    let mut rebased_apps = 0usize;

    for app_id in tracked_ids {
        let Some(drift) = store.check_drift(steam_service, app_id) else {
            continue;
        };

        let baseline_original = if drift.steam_change_number != drift.recorded_change_number {
            drift.current_options.clone()
        } else {
            store
                .get_latest(app_id)
                .map(|r| r.original_launch_options.clone())
                .unwrap_or_else(|| drift.current_options.clone())
        };

        let Some(rebased_entries) = store.preview_rebase(app_id, &baseline_original) else {
            continue;
        };

        if svc.save_launch_entries(app_id, &rebased_entries).is_ok() {
            store.rebase(app_id, drift.steam_change_number, baseline_original);
            rebased_apps += 1;
        }
    }

    if was_running {
        let _ = controller.restart(root);
    }

    rebased_apps
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use crate::models::steam::{SteamInstallation, SteamLibrary};
    use crate::parsers::vdf_binary::{MAGIC_27, MAGIC_29, TYPE_END, TYPE_STRING, TYPE_TABLE};
    use tempfile::tempdir;

    fn make_entry(idx: &str, exe: &str, args: &str, desc: &str) -> LaunchEntry {
        LaunchEntry {
            index: idx.into(),
            executable: exe.into(),
            arguments: args.into(),
            workingdir: String::new(),
            description: desc.into(),
            launch_type: "default".into(),
            oslist: String::new(),
            osarch: String::new(),
        }
    }

    fn write_synthetic_appinfo_v29(
        steam_root: &Path,
        app_id: u32,
        change_number: u32,
        entries: &[LaunchEntry],
    ) -> PathBuf {
        let appcache = steam_root.join("appcache");
        fs::create_dir_all(&appcache).unwrap();
        let vdf_path = appcache.join("appinfo.vdf");

        let mut strings: Vec<String> = vec![
            "appinfo".into(),
            "config".into(),
            "launch".into(),
            "executable".into(),
            "arguments".into(),
            "description".into(),
            "type".into(),
        ];
        let mut intern = |s: &str| -> u32 {
            if let Some(pos) = strings.iter().position(|x| x == s) {
                pos as u32
            } else {
                strings.push(s.to_string());
                (strings.len() - 1) as u32
            }
        };

        let mut blob = Vec::new();
        blob.push(TYPE_TABLE);
        blob.extend_from_slice(&intern("appinfo").to_le_bytes());
        blob.push(TYPE_TABLE);
        blob.extend_from_slice(&intern("config").to_le_bytes());
        blob.push(TYPE_TABLE);
        blob.extend_from_slice(&intern("launch").to_le_bytes());

        for (i, e) in entries.iter().enumerate() {
            let slot_idx = intern(&i.to_string());
            blob.push(TYPE_TABLE);
            blob.extend_from_slice(&slot_idx.to_le_bytes());

            let write_str = |buf: &mut Vec<u8>, key_idx: u32, val: &str| {
                if !val.is_empty() {
                    buf.push(TYPE_STRING);
                    buf.extend_from_slice(&key_idx.to_le_bytes());
                    buf.extend_from_slice(val.as_bytes());
                    buf.push(0);
                }
            };
            let exe_k = intern("executable");
            let arg_k = intern("arguments");
            let desc_k = intern("description");
            let type_k = intern("type");

            write_str(&mut blob, exe_k, &e.executable);
            write_str(&mut blob, arg_k, &e.arguments);
            write_str(&mut blob, desc_k, &e.description);
            write_str(
                &mut blob,
                type_k,
                if e.launch_type.is_empty() {
                    "default"
                } else {
                    &e.launch_type
                },
            );

            blob.push(TYPE_END);
        }

        blob.extend_from_slice(&[TYPE_END, TYPE_END, TYPE_END, TYPE_END]);

        let mut meta = vec![0u8; 60];
        meta[36..40].copy_from_slice(&change_number.to_le_bytes());
        let entry_size = (meta.len() + blob.len()) as u32;

        let mut file_bytes = Vec::new();
        file_bytes.extend_from_slice(&MAGIC_29.to_le_bytes());
        file_bytes.extend_from_slice(&1u32.to_le_bytes());
        file_bytes.extend_from_slice(&0i64.to_le_bytes());
        file_bytes.extend_from_slice(&app_id.to_le_bytes());
        file_bytes.extend_from_slice(&entry_size.to_le_bytes());
        file_bytes.extend_from_slice(&meta);
        file_bytes.extend_from_slice(&blob);
        file_bytes.extend_from_slice(&0u32.to_le_bytes());

        let table_pos = file_bytes.len() as i64;
        file_bytes.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        for s in &strings {
            file_bytes.extend_from_slice(s.as_bytes());
            file_bytes.push(0);
        }
        file_bytes[8..16].copy_from_slice(&table_pos.to_le_bytes());
        fs::write(&vdf_path, &file_bytes).unwrap();
        vdf_path
    }

    fn mock_steam_service(root: &Path) -> SteamService {
        SteamService {
            installation: Some(SteamInstallation {
                root: root.to_path_buf(),
                config_path: root.join("config"),
                stplug_in_path: root.join("config/stplug-in"),
                libraries: vec![SteamLibrary {
                    path: root.to_path_buf(),
                }],
            }),
        }
    }

    struct MockSteamController<F: Fn()> {
        running: RefCell<bool>,
        fail_shutdown: bool,
        events: RefCell<Vec<&'static str>>,
        on_shutdown_flush: F,
    }

    impl<F: Fn()> SteamProcessController for MockSteamController<F> {
        fn is_running(&self) -> bool {
            *self.running.borrow()
        }

        fn shutdown(&self) -> Result<usize, String> {
            self.events.borrow_mut().push("shutdown");
            if self.fail_shutdown {
                return Err("Permission denied stopping Steam".into());
            }
            // Simulate Steam flushing its in-memory appinfo.vdf right as it exits
            (self.on_shutdown_flush)();
            *self.running.borrow_mut() = false;
            Ok(3)
        }

        fn restart(&self, _steam_root: &Path) -> Result<(), String> {
            self.events.borrow_mut().push("restart");
            *self.running.borrow_mut() = true;
            Ok(())
        }
    }

    #[test]
    fn test_launch_mod_store() {
        let dir = tempdir().unwrap();
        let store_file = dir.path().join("launchmods.json");
        let mut store = LaunchModStore::new(Some(store_file.clone()));

        let original = vec![
            make_entry("0", "game.exe", "", "Default"),
            make_entry("1", "game.exe", "-vulkan", "Vulkan"),
        ];
        let modified = vec![
            make_entry("0", "game.exe", "", "Default"),
            make_entry("1", "game.exe", "-vulkan -novid", "Vulkan"),
        ];

        store.record_modification(123, 100, original, modified);
        let saved = store.get_latest(123).unwrap();
        assert_eq!(saved.change_number, 100);
        assert_eq!(saved.modified_launch_options.len(), 2);
        assert_eq!(saved.modified_launch_options[1].arguments, "-vulkan -novid");

        // Reload from disk and verify MAX_CHANGE_NUMBERS ring trimming
        let mut reloaded = LaunchModStore::new(Some(store_file));
        assert_eq!(reloaded.get_latest(123).unwrap().change_number, 100);
        for cn in 101..115 {
            reloaded.record_modification(123, cn, vec![], vec![]);
        }
        assert_eq!(reloaded.get_all(123).unwrap().len(), MAX_CHANGE_NUMBERS);
        assert_eq!(reloaded.get_latest(123).unwrap().change_number, 114);
    }

    #[test]
    fn test_extract_change_number_v27_and_v29_without_panic() {
        let dir = tempdir().unwrap();
        let v29_path = write_synthetic_appinfo_v29(
            dir.path(),
            1245620,
            987_654,
            &[make_entry("0", "eldenring.exe", "", "")],
        );
        assert_eq!(
            AppInfoFile::extract_app_change_number(&v29_path, 1245620),
            Some(987_654)
        );
        assert_eq!(AppInfoFile::extract_app_change_number(&v29_path, 999), None);

        // Also test v27 (40-byte meta block)
        let v27_dir = tempdir().unwrap();
        let v27_path = v27_dir.path().join("appinfo.vdf");
        let mut meta_v27 = vec![0u8; 40];
        meta_v27[36..40].copy_from_slice(&42_424u32.to_le_bytes());
        let blob_v27 = vec![TYPE_END];
        let sz = (meta_v27.len() + blob_v27.len()) as u32;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC_27.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&480u32.to_le_bytes());
        bytes.extend_from_slice(&sz.to_le_bytes());
        bytes.extend_from_slice(&meta_v27);
        bytes.extend_from_slice(&blob_v27);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        fs::write(&v27_path, &bytes).unwrap();

        assert_eq!(
            AppInfoFile::extract_app_change_number(&v27_path, 480),
            Some(42_424)
        );
        assert_eq!(
            AppInfoFile::extract_launch_options(&v27_path, 480),
            Some(Vec::new())
        );
    }

    #[test]
    fn test_three_way_pics_rebase_preserves_edits_and_new_pics_entries() {
        let old_original = vec![
            make_entry("0", "game.exe", "", "Play"),
            make_entry("1", "game.exe", "-vulkan", "Vulkan"),
        ];
        let modified = vec![
            make_entry("0", "game_loader.exe", "", "Play"),
            make_entry("1", "game.exe", "-vulkan -novid", "Vulkan"),
            make_entry("2", "offline_bypass.exe", "-offline", "Offline Bypass"),
        ];
        // Upstream PICS update changes slot 0 args, updates slot 1 executable, and adds DX12 slot
        let new_original = vec![
            make_entry("0", "game.exe", "- telemetry-off", "Play"),
            make_entry("1", "game_v2.exe", "-vulkan", "Vulkan"),
            make_entry("2", "game_v2.exe", "-dx12", "DirectX 12"),
        ];

        let rebased = apply_launch_diff(&old_original, &modified, &new_original);
        assert_eq!(rebased.len(), 4);
        // Slot 0 keeps user's custom executable AND adopts PICS's new arguments
        assert_eq!(rebased[0].index, "0");
        assert_eq!(rebased[0].executable, "game_loader.exe");
        assert_eq!(rebased[0].arguments, "- telemetry-off".trim());
        // Slot 1 adopts PICS's new executable AND keeps user's custom arguments (no duplicate!)
        assert_eq!(rebased[1].index, "1");
        assert_eq!(rebased[1].executable, "game_v2.exe");
        assert_eq!(rebased[1].arguments, "-vulkan -novid");
        // Slot 2 keeps new PICS DX12 option
        assert_eq!(rebased[2].index, "2");
        assert_eq!(rebased[2].description, "DirectX 12");
        // Slot 3 preserves user-added custom launch option renumbered cleanly
        assert_eq!(rebased[3].index, "3");
        assert_eq!(rebased[3].executable, "offline_bypass.exe");
    }

    #[test]
    fn test_coordinated_save_and_restore_with_running_steam_exit_flush() {
        let dir = tempdir().unwrap();
        let steam_root = dir.path().join("Steam");
        let store_path = dir.path().join("launchmods.json");
        let mut store = LaunchModStore::new(Some(store_path));

        let orig = vec![make_entry("0", "eldenring.exe", "", "Default")];
        write_synthetic_appinfo_v29(&steam_root, 1245620, 500, &orig);

        let root_for_flush = steam_root.clone();
        let orig_for_flush = orig.clone();
        let controller = MockSteamController {
            running: RefCell::new(true),
            fail_shutdown: false,
            events: RefCell::new(Vec::new()),
            on_shutdown_flush: move || {
                // Simulate Steam overwriting appinfo.vdf with its stale in-memory state on exit!
                write_synthetic_appinfo_v29(&root_for_flush, 1245620, 500, &orig_for_flush);
            },
        };

        let edit1 = vec![make_entry("0", "ersc_launcher.exe", "-noeac", "Seamless Co-op")];
        let res = save_launch_options_with_controller(
            &steam_root,
            1245620,
            &edit1,
            &mut store,
            &controller,
        )
        .unwrap();

        assert!(res.steam_was_running);
        assert!(res.steam_restarted);
        assert_eq!(*controller.events.borrow(), vec!["shutdown", "restart"]);

        // Verify our edit survived Steam's exit flush in appinfo.vdf!
        let svc = LaunchOptionsService::new(steam_root.clone());
        let on_disk = svc.get_launch_entries(1245620);
        assert_eq!(on_disk.len(), 1);
        assert_eq!(on_disk[0].executable, "ersc_launcher.exe");
        assert_eq!(on_disk[0].arguments, "-noeac");

        // Consecutive save at same change_number preserves true unmodded PICS original baseline
        *controller.running.borrow_mut() = false;
        let edit2 = vec![make_entry(
            "0",
            "ersc_launcher.exe",
            "-noeac -skipintro",
            "Seamless Co-op",
        )];
        let no_op_ctrl = MockSteamController {
            running: RefCell::new(false),
            fail_shutdown: false,
            events: RefCell::new(Vec::new()),
            on_shutdown_flush: || {},
        };
        let res2 = save_launch_options_with_controller(
            &steam_root,
            1245620,
            &edit2,
            &mut store,
            &no_op_ctrl,
        )
        .unwrap();
        assert!(!res2.steam_was_running);
        assert!(!res2.steam_restarted);
        assert!(no_op_ctrl.events.borrow().is_empty());
        assert_eq!(
            store.get_latest(1245620).unwrap().original_launch_options[0].executable,
            "eldenring.exe"
        );

        // Restore backup while Steam is running -> shuts down Steam, restores original, clears store, restarts Steam
        let restore_ctrl = MockSteamController {
            running: RefCell::new(true),
            fail_shutdown: false,
            events: RefCell::new(Vec::new()),
            on_shutdown_flush: || {},
        };
        let restored = restore_launch_backup_with_controller(
            &steam_root,
            1245620,
            &mut store,
            &restore_ctrl,
        )
        .unwrap();
        assert!(restored.steam_restarted);
        assert_eq!(restored.entries[0].executable, "eldenring.exe");
        assert!(store.get_latest(1245620).is_none());
        assert_eq!(*restore_ctrl.events.borrow(), vec!["shutdown", "restart"]);
    }

    #[test]
    fn test_coordinated_save_aborts_when_steam_shutdown_fails() {
        let dir = tempdir().unwrap();
        let steam_root = dir.path().join("Steam");
        let mut store = LaunchModStore::new(Some(dir.path().join("launchmods.json")));
        let orig = vec![make_entry("0", "game.exe", "", "Default")];
        write_synthetic_appinfo_v29(&steam_root, 480, 10, &orig);

        let failing_ctrl = MockSteamController {
            running: RefCell::new(true),
            fail_shutdown: true,
            events: RefCell::new(Vec::new()),
            on_shutdown_flush: || {},
        };

        let modded = vec![make_entry("0", "mod.exe", "-mod", "Modded")];
        let err = save_launch_options_with_controller(
            &steam_root,
            480,
            &modded,
            &mut store,
            &failing_ctrl,
        )
        .unwrap_err();
        assert!(err.contains("Permission denied"));
        // appinfo.vdf and store must remain untouched
        let svc = LaunchOptionsService::new(steam_root);
        assert_eq!(svc.get_launch_entries(480)[0].executable, "game.exe");
        assert!(store.get_latest(480).is_none());
        assert_eq!(*failing_ctrl.events.borrow(), vec!["shutdown"]);
    }

    #[test]
    fn test_startup_pics_drift_detection_and_coordinated_rebase() {
        let dir = tempdir().unwrap();
        let steam_root = dir.path().join("Steam");
        let steam_svc = mock_steam_service(&steam_root);
        let mut store = LaunchModStore::new(Some(dir.path().join("launchmods.json")));

        let orig_v1 = vec![
            make_entry("0", "game.exe", "", "Default"),
            make_entry("1", "game.exe", "-vulkan", "Vulkan"),
        ];
        write_synthetic_appinfo_v29(&steam_root, 1245620, 100, &orig_v1);

        let desired_v1 = vec![
            make_entry("0", "game.exe", "", "Default"),
            make_entry("1", "game.exe", "-vulkan -novid", "Vulkan"),
        ];
        let idle_ctrl = MockSteamController {
            running: RefCell::new(false),
            fail_shutdown: false,
            events: RefCell::new(Vec::new()),
            on_shutdown_flush: || {},
        };
        save_launch_options_with_controller(
            &steam_root,
            1245620,
            &desired_v1,
            &mut store,
            &idle_ctrl,
        )
        .unwrap();

        // No drift right after saving
        assert!(store.check_drift(&steam_svc, 1245620).is_none());
        assert_eq!(
            reconcile_startup_drift_with_controller(&steam_svc, &mut store, &idle_ctrl),
            0
        );

        // Simulate Steam PICS update (change_number 100 -> 250) overwriting appinfo.vdf
        let pics_v2 = vec![
            make_entry("0", "game_v2.exe", "", "Default"),
            make_entry("1", "game_v2.exe", "-vulkan", "Vulkan"),
        ];
        write_synthetic_appinfo_v29(&steam_root, 1245620, 250, &pics_v2);

        let drift = store.check_drift(&steam_svc, 1245620).expect("Expected PICS drift");
        assert_eq!(drift.recorded_change_number, 100);
        assert_eq!(drift.steam_change_number, 250);

        // Reconcile startup drift while Steam is running -> shuts down Steam, rebases, writes, restarts
        let running_ctrl = MockSteamController {
            running: RefCell::new(true),
            fail_shutdown: false,
            events: RefCell::new(Vec::new()),
            on_shutdown_flush: || {},
        };
        let rebased_count =
            reconcile_startup_drift_with_controller(&steam_svc, &mut store, &running_ctrl);
        assert_eq!(rebased_count, 1);
        assert_eq!(*running_ctrl.events.borrow(), vec!["shutdown", "restart"]);

        // Verify appinfo.vdf now has game_v2.exe + user's "-vulkan -novid", and change_number 250 is recorded
        let svc = LaunchOptionsService::new(steam_root);
        let after = svc.get_launch_entries(1245620);
        assert_eq!(after.len(), 2);
        assert_eq!(after[1].executable, "game_v2.exe");
        assert_eq!(after[1].arguments, "-vulkan -novid");

        let latest = store.get_latest(1245620).unwrap();
        assert_eq!(latest.change_number, 250);
        assert_eq!(latest.rebased_from, Some(100));
        assert!(store.check_drift(&steam_svc, 1245620).is_none());
    }
}
