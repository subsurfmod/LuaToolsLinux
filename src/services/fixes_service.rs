//! FixesService — Game fixes catalog, `.luatools-fix/` SHA-256 backup, apply & verified revert.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::services::api_client::{DRMFixFixItem, LuaToolsApiClient};
use crate::services::applied_fix_index::{
    AppliedFixFileEntry, AppliedFixIndexService, AppliedFixRecord as IndexAppliedFixRecord,
};
use crate::services::lua_installer::LuaInstaller;
use crate::steam_bridge::ost_platform::encoding::wide_to_utf8_lossy;

fn read_wine_registry_text(path: &Path) -> io::Result<String> {
    let bytes = fs::read(path)?;
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let wide: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        return Ok(wide_to_utf8_lossy(&wide));
    }
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Ok(String::from_utf8_lossy(&bytes[3..]).into_owned());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub const FIX_DIR_NAME: &str = ".luatools-fix";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FixFileEntry {
    pub relative_path: String,
    pub action: String, // "modified" | "added"
    pub backup_path: Option<String>,
    pub hash_before: Option<String>,
    pub hash_after: String,
}

// Custom deserializer for Option<PathBuf> that gracefully handles missing, null, or invalid values
fn deserialize_option_path_buf<'de, D>(deserializer: D) -> Result<Option<PathBuf>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(opt.map(PathBuf::from))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedFixRecord {
    pub app_id: u32,
    pub fix_id: String,
    pub applied_at: String,
    #[serde(default, deserialize_with = "deserialize_option_path_buf")]
    pub game_dir: Option<PathBuf>,
    pub files: Vec<FixFileEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wine_dll_overrides: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FixApplicationResult {
    pub ok: bool,
    pub error: Option<String>,
    pub applied_files: Vec<String>,
}

impl FixApplicationResult {
    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
            applied_files: Vec::new(),
        }
    }
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    io::copy(&mut f, &mut hasher).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn sanitize_fix_id(fix_id: &str) -> String {
    fix_id.replace(':', "_")
}

pub fn is_safe_fix_id(fix_id: &str) -> bool {
    let s = fix_id.trim();
    !s.is_empty()
        && s != "."
        && s != ".."
        && !s.contains("..")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

#[derive(Debug, Clone, Default)]
pub struct FixesService;

impl FixesService {
    pub fn is_fix_applied(install_dir: &Path, fix_id: &str) -> bool {
        if !is_safe_fix_id(fix_id) {
            return false;
        }
        let safe_file = format!("{}.json", sanitize_fix_id(fix_id));
        let legacy_file = format!("{}.json", fix_id);
        let dir = install_dir.join(FIX_DIR_NAME);
        dir.join(&safe_file).exists() || dir.join(&legacy_file).exists()
    }

    pub fn apply_drm_fix_fix<F>(
        api: &LuaToolsApiClient,
        app_id: u32,
        fix: &DRMFixFixItem,
        install_dir: &Path,
        progress_cb: F,
    ) -> FixApplicationResult
    where
        F: FnMut(f32, String),
    {
        if !is_safe_fix_id(&fix.id) {
            return FixApplicationResult::err("Invalid fix ID.");
        }
        if !install_dir.exists() {
            return FixApplicationResult::err(format!(
                "Game directory does not exist: {}",
                install_dir.display()
            ));
        }

        let tmp = std::env::temp_dir().join(format!(
            "luatools_fix_dl_{}_{}_{}",
            std::process::id(),
            app_id,
            fix.id
        ));
        let extract_dir = tmp.join("extract");
        let _ = fs::remove_dir_all(&tmp);
        if let Err(e) = fs::create_dir_all(&extract_dir) {
            return FixApplicationResult::err(e.to_string());
        }

        let fallback = fix
            .fix_filename
            .clone()
            .unwrap_or_else(|| format!("{}.zip", fix.id));

        let dl_path = match api.download_drm_fix_slot(&fix.id, "fix", &tmp, &fallback, progress_cb)
        {
            Ok(p) => p,
            Err(e) => {
                let _ = fs::remove_dir_all(&tmp);
                return FixApplicationResult::err(e);
            }
        };

        let is_zip = dl_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("zip"))
            .unwrap_or(false);

        if is_zip {
            let Ok(zf) = File::open(&dl_path) else {
                let _ = fs::remove_dir_all(&tmp);
                return FixApplicationResult::err("Cannot open downloaded fix archive.");
            };
            let Ok(mut archive) = ZipArchive::new(zf) else {
                let _ = fs::remove_dir_all(&tmp);
                return FixApplicationResult::err("Invalid fix .zip archive.");
            };
            for i in 0..archive.len() {
                let mut entry = match archive.by_index(i) {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = fs::remove_dir_all(&tmp);
                        return FixApplicationResult::err(format!(
                            "Failed to read archive entry {}: {}",
                            i, e
                        ));
                    }
                };
                if entry.is_dir() {
                    continue;
                }
                let rel = PathBuf::from(entry.name());
                if !LuaInstaller::is_safe_relative_path(&rel) {
                    continue;
                }
                let target = extract_dir.join(&rel);
                if let Some(p) = target.parent() {
                    if let Err(e) = fs::create_dir_all(p) {
                        let _ = fs::remove_dir_all(&tmp);
                        return FixApplicationResult::err(format!(
                            "Failed to create extraction directory: {}",
                            e
                        ));
                    }
                }
                let mut out = match File::create(&target) {
                    Ok(f) => f,
                    Err(e) => {
                        let _ = fs::remove_dir_all(&tmp);
                        return FixApplicationResult::err(format!(
                            "Failed to create extracted file '{}': {}",
                            rel.display(),
                            e
                        ));
                    }
                };
                if let Err(e) = io::copy(&mut entry, &mut out) {
                    let _ = fs::remove_dir_all(&tmp);
                    return FixApplicationResult::err(format!(
                        "Failed to extract '{}': {}",
                        rel.display(),
                        e
                    ));
                }
            }
        } else if let Some(fname) = dl_path.file_name() {
            if let Err(e) = fs::copy(&dl_path, extract_dir.join(fname)) {
                let _ = fs::remove_dir_all(&tmp);
                return FixApplicationResult::err(format!("Failed to stage fix file: {}", e));
            }
        }

        let res = Self::apply_extracted_with_backup(app_id, &fix.id, &extract_dir, install_dir);
        let _ = fs::remove_dir_all(&tmp);
        res
    }

    fn rollback_partial_apply(game_dir: &Path, fix_meta_dir: &Path, records: &[FixFileEntry]) {
        for entry in records.iter().rev() {
            let dst = game_dir.join(&entry.relative_path);
            if entry.action == "added" {
                let _ = fs::remove_file(&dst);
            } else if entry.action == "modified" {
                if let Some(ref bkp_rel) = entry.backup_path {
                    let bkp = fix_meta_dir.join(bkp_rel);
                    if bkp.is_file() {
                        let _ = fs::copy(&bkp, &dst);
                    }
                }
            }
        }
    }

    pub fn apply_extracted_with_backup(
        app_id: u32,
        fix_id: &str,
        extract_dir: &Path,
        game_dir: &Path,
    ) -> FixApplicationResult {
        Self::apply_extracted_with_backup_inner(app_id, fix_id, extract_dir, game_dir, None)
    }

    pub fn apply_extracted_with_backup_with_index(
        app_id: u32,
        fix_id: &str,
        extract_dir: &Path,
        game_dir: &Path,
        index: &mut AppliedFixIndexService,
    ) -> FixApplicationResult {
        Self::apply_extracted_with_backup_inner(app_id, fix_id, extract_dir, game_dir, Some(index))
    }

    fn apply_extracted_with_backup_inner(
        app_id: u32,
        fix_id: &str,
        extract_dir: &Path,
        game_dir: &Path,
        index_override: Option<&mut AppliedFixIndexService>,
    ) -> FixApplicationResult {
        if !is_safe_fix_id(fix_id) {
            return FixApplicationResult::err("Invalid fix ID.");
        }
        if Self::is_fix_applied(game_dir, fix_id) {
            return FixApplicationResult::err(format!(
                "Fix '{}' is already applied. Revert it before re-applying to preserve original backups.",
                fix_id
            ));
        }

        let mut files_to_apply = Vec::new();
        collect_files_recursive(extract_dir, &mut files_to_apply);
        files_to_apply.sort();
        if files_to_apply.is_empty() {
            return FixApplicationResult::err("Fix archive contained no files to apply.");
        }

        let safe_id = sanitize_fix_id(fix_id);
        let fix_meta_dir = game_dir.join(FIX_DIR_NAME);
        let backup_dir = fix_meta_dir.join("backups").join(&safe_id);
        if let Err(e) = fs::create_dir_all(&backup_dir) {
            return FixApplicationResult::err(e.to_string());
        }

        // Phase 1: Validate all paths, pre-hash staged sources, and back up existing game files
        // BEFORE mutating any file in `game_dir`.
        let mut planned: Vec<(PathBuf, PathBuf, FixFileEntry)> = Vec::new();
        for src in files_to_apply {
            let Ok(rel) = src.strip_prefix(extract_dir) else {
                continue;
            };
            if !LuaInstaller::is_safe_relative_path(rel) {
                continue;
            }
            let rel_posix = rel.to_string_lossy().replace('\\', "/");
            let dst = game_dir.join(rel);
            let Ok(hash_after) = sha256_file(&src) else {
                let _ = fs::remove_dir_all(&backup_dir);
                return FixApplicationResult::err(format!(
                    "Failed to hash staged fix file {}",
                    rel_posix
                ));
            };

            if dst.exists() {
                let Ok(hash_before) = sha256_file(&dst) else {
                    let _ = fs::remove_dir_all(&backup_dir);
                    return FixApplicationResult::err(format!(
                        "Failed to hash existing file {}",
                        rel_posix
                    ));
                };
                let bkp_file = backup_dir.join(rel);
                if let Some(bp) = bkp_file.parent() {
                    if let Err(e) = fs::create_dir_all(bp) {
                        let _ = fs::remove_dir_all(&backup_dir);
                        return FixApplicationResult::err(format!("Backup dir failed: {}", e));
                    }
                }
                if let Err(e) = fs::copy(&dst, &bkp_file) {
                    let _ = fs::remove_dir_all(&backup_dir);
                    return FixApplicationResult::err(format!("Backup failed: {}", e));
                }
                planned.push((
                    src,
                    dst,
                    FixFileEntry {
                        relative_path: rel_posix.clone(),
                        action: "modified".into(),
                        backup_path: Some(format!("backups/{}/{}", safe_id, rel_posix)),
                        hash_before: Some(hash_before),
                        hash_after,
                    },
                ));
            } else {
                planned.push((
                    src,
                    dst,
                    FixFileEntry {
                        relative_path: rel_posix,
                        action: "added".into(),
                        backup_path: None,
                        hash_before: None,
                        hash_after,
                    },
                ));
            }
        }

        if planned.is_empty() {
            let _ = fs::remove_dir_all(&backup_dir);
            return FixApplicationResult::err("Fix archive contained no valid files to apply.");
        }

        let records: Vec<FixFileEntry> = planned.iter().map(|(_, _, e)| e.clone()).collect();
        let mut dll_overrides = Vec::new();
        for r in &records {
            let p = Path::new(&r.relative_path);
            if p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("dll"))
                .unwrap_or(false)
            {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    let s_lower = stem.to_ascii_lowercase();
                    if !s_lower.is_empty() && !dll_overrides.contains(&s_lower) {
                        dll_overrides.push(s_lower);
                    }
                }
            }
        }

        let wine_overrides_str = if !dll_overrides.is_empty() {
            Some(
                dll_overrides
                    .iter()
                    .map(|d| format!("{}=n,b", d))
                    .collect::<Vec<_>>()
                    .join(";"),
            )
        } else {
            None
        };

        // Phase 2 (LT-74): Persist the fix ledger JSON atomically BEFORE overwriting any game
        // files so an interrupted/crashed install can still be cleanly reverted via `unfix_game`.
        let applied_at = Utc::now().to_rfc3339();
        let record = AppliedFixRecord {
            app_id,
            fix_id: fix_id.to_string(),
            applied_at: applied_at.clone(),
            game_dir: Some(game_dir.to_path_buf()),
            files: records.clone(),
            wine_dll_overrides: wine_overrides_str.clone(),
        };
        let record_file = fix_meta_dir.join(format!("{}.json", safe_id));
        let record_tmp = fix_meta_dir.join(format!("{}.json.tmp", safe_id));
        let js = match serde_json::to_string_pretty(&record) {
            Ok(s) => s,
            Err(e) => {
                let _ = fs::remove_dir_all(&backup_dir);
                return FixApplicationResult::err(e.to_string());
            }
        };
        if let Err(e) = fs::write(&record_tmp, &js).and_then(|_| fs::rename(&record_tmp, &record_file)) {
            let _ = fs::remove_file(&record_tmp);
            let _ = fs::remove_dir_all(&backup_dir);
            return FixApplicationResult::err(e.to_string());
        }

        // Phase 3: Copy fix files into `game_dir` and verify SHA-256; rollback cleanly on error.
        let mut applied_entries: Vec<FixFileEntry> = Vec::with_capacity(planned.len());
        let mut applied_paths: Vec<String> = Vec::with_capacity(planned.len());

        for (src, dst, entry) in planned {
            if let Some(parent) = dst.parent() {
                if let Err(e) = fs::create_dir_all(parent) {
                    Self::rollback_partial_apply(game_dir, &fix_meta_dir, &applied_entries);
                    let _ = fs::remove_file(&record_file);
                    let _ = fs::remove_dir_all(&backup_dir);
                    return FixApplicationResult::err(format!("Failed to create folder: {}", e));
                }
            }
            if let Err(e) = fs::copy(&src, &dst) {
                if entry.action == "modified" {
                    if let Some(ref bkp_rel) = entry.backup_path {
                        let _ = fs::copy(fix_meta_dir.join(bkp_rel), &dst);
                    }
                } else {
                    let _ = fs::remove_file(&dst);
                }
                Self::rollback_partial_apply(game_dir, &fix_meta_dir, &applied_entries);
                let _ = fs::remove_file(&record_file);
                let _ = fs::remove_dir_all(&backup_dir);
                return FixApplicationResult::err(format!("Apply failed: {}", e));
            }
            let post_hash = sha256_file(&dst).ok();
            if post_hash.as_deref() != Some(entry.hash_after.as_str()) {
                if entry.action == "modified" {
                    if let Some(ref bkp_rel) = entry.backup_path {
                        let _ = fs::copy(fix_meta_dir.join(bkp_rel), &dst);
                    }
                } else {
                    let _ = fs::remove_file(&dst);
                }
                Self::rollback_partial_apply(game_dir, &fix_meta_dir, &applied_entries);
                let _ = fs::remove_file(&record_file);
                let _ = fs::remove_dir_all(&backup_dir);
                return FixApplicationResult::err("Post-apply hash verification failed.");
            }
            applied_paths.push(entry.relative_path.clone());
            applied_entries.push(entry);
        }

        // Phase 4: Sync shared Wine DLL overrides and record real metadata into AppliedFixIndexService.
        if !dll_overrides.is_empty() {
            Self::sync_wine_dll_overrides_env(&fix_meta_dir);
            Self::configure_wine_registry_overrides(game_dir, app_id, &dll_overrides);
        }

        let index_record = IndexAppliedFixRecord {
            app_id,
            fix_id: fix_id.to_string(),
            applied_at,
            game_dir: Some(game_dir.to_path_buf()),
            files: records
                .iter()
                .map(|r| AppliedFixFileEntry {
                    relative_path: r.relative_path.clone(),
                    action: r.action.clone(),
                    backup_path: r.backup_path.clone(),
                    hash_before: r.hash_before.clone(),
                    hash_after: r.hash_after.clone(),
                })
                .collect(),
            wine_dll_overrides: wine_overrides_str,
        };
        if let Some(idx) = index_override {
            idx.add_fix(index_record);
        } else if !cfg!(test) && !game_dir.starts_with(std::env::temp_dir()) {
            let mut idx = AppliedFixIndexService::default();
            idx.add_fix(index_record);
        }

        FixApplicationResult {
            ok: true,
            error: None,
            applied_files: applied_paths,
        }
    }

    /// Rebuilds `.luatools-fix/winedlloverrides.env` from all remaining `.json` fix records,
    /// removing it only when no remaining fix requires DLL overrides.
    fn sync_wine_dll_overrides_env(fix_meta_dir: &Path) {
        let mut combined: Vec<String> = Vec::new();
        if let Ok(rd) = fs::read_dir(fix_meta_dir) {
            let mut json_paths: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
                .collect();
            json_paths.sort();
            for p in json_paths {
                if let Ok(raw) = fs::read_to_string(&p) {
                    if let Ok(rec) = serde_json::from_str::<AppliedFixRecord>(&raw) {
                        if let Some(ov) = rec.wine_dll_overrides {
                            for part in ov.split(';').map(str::trim).filter(|s| !s.is_empty()) {
                                if !combined.iter().any(|c| c.eq_ignore_ascii_case(part)) {
                                    combined.push(part.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        let env_path = fix_meta_dir.join("winedlloverrides.env");
        if combined.is_empty() {
            let _ = fs::remove_file(&env_path);
        } else {
            let content = format!("WINEDLLOVERRIDES=\"{}\"\n", combined.join(";"));
            let _ = fs::write(&env_path, content);
        }
    }

    fn collect_remaining_dll_stems(fix_meta_dir: &Path) -> Vec<String> {
        let mut stems = Vec::new();
        if let Ok(rd) = fs::read_dir(fix_meta_dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(raw) = fs::read_to_string(&p) {
                    if let Ok(rec) = serde_json::from_str::<AppliedFixRecord>(&raw) {
                        if let Some(ov) = rec.wine_dll_overrides {
                            for part in ov.split(';') {
                                let stem = part.split('=').next().unwrap_or("").trim().to_ascii_lowercase();
                                if !stem.is_empty() && !stems.contains(&stem) {
                                    stems.push(stem);
                                }
                            }
                        }
                    }
                }
            }
        }
        stems
    }

    pub fn unfix_game(game_dir: &Path, fix_id: &str) -> FixApplicationResult {
        Self::unfix_game_inner(game_dir, fix_id, None)
    }

    pub fn unfix_game_with_index(
        game_dir: &Path,
        fix_id: &str,
        index: &mut AppliedFixIndexService,
    ) -> FixApplicationResult {
        Self::unfix_game_inner(game_dir, fix_id, Some(index))
    }

    fn unfix_game_inner(
        game_dir: &Path,
        fix_id: &str,
        index_override: Option<&mut AppliedFixIndexService>,
    ) -> FixApplicationResult {
        if !is_safe_fix_id(fix_id) {
            return FixApplicationResult::err("Invalid fix ID.");
        }
        let safe_id = sanitize_fix_id(fix_id);
        let fix_meta_dir = game_dir.join(FIX_DIR_NAME);
        let safe_file = fix_meta_dir.join(format!("{}.json", safe_id));
        let legacy_file = fix_meta_dir.join(format!("{}.json", fix_id));
        let record_file = if safe_file.is_file() {
            safe_file
        } else if legacy_file.is_file() {
            legacy_file
        } else {
            return FixApplicationResult::err(format!("Fix record '{}' not found.", fix_id));
        };
        let Ok(raw) = fs::read_to_string(&record_file) else {
            return FixApplicationResult::err(format!("Fix record '{}' not found.", fix_id));
        };
        let Ok(record) = serde_json::from_str::<AppliedFixRecord>(&raw) else {
            return FixApplicationResult::err("Corrupted fix record JSON.");
        };

        // Pre-flight validation: verify paths, external modification hashes, and backup presence
        let mut already_reverted_flags = Vec::with_capacity(record.files.len());
        for entry in &record.files {
            if !LuaInstaller::is_safe_relative_path(Path::new(&entry.relative_path)) {
                return FixApplicationResult::err(format!(
                    "Unsafe path '{}' in fix record.",
                    entry.relative_path
                ));
            }
            let dst = game_dir.join(&entry.relative_path);
            let mut is_already_reverted = false;
            if dst.exists() {
                if let Ok(curr_hash) = sha256_file(&dst) {
                    is_already_reverted = entry
                        .hash_before
                        .as_deref()
                        .map(|hb| hb == curr_hash)
                        .unwrap_or(false);
                    if curr_hash != entry.hash_after && !is_already_reverted {
                        return FixApplicationResult::err(format!(
                            "File '{}' was modified externally after fix '{}' was applied. Revert newer fixes first.",
                            entry.relative_path, fix_id
                        ));
                    }
                }
            }
            if entry.action == "modified" && !is_already_reverted {
                let Some(bkp_rel) = &entry.backup_path else {
                    return FixApplicationResult::err(format!(
                        "Missing backup path metadata for '{}'.",
                        entry.relative_path
                    ));
                };
                if !LuaInstaller::is_safe_relative_path(Path::new(bkp_rel)) {
                    return FixApplicationResult::err(format!(
                        "Unsafe backup path '{}' in fix record.",
                        bkp_rel
                    ));
                }
                let bkp = fix_meta_dir.join(bkp_rel);
                if !bkp.is_file() {
                    return FixApplicationResult::err(format!(
                        "Backup file for '{}' is missing; cannot revert safely.",
                        entry.relative_path
                    ));
                }
            }
            already_reverted_flags.push(is_already_reverted);
        }

        let mut reverted = Vec::new();
        for (entry, is_already_reverted) in record.files.iter().zip(already_reverted_flags) {
            let rel_path = Path::new(&entry.relative_path);
            if !LuaInstaller::is_safe_relative_path(rel_path) {
                return FixApplicationResult::err(format!(
                    "Unsafe path '{}' encountered during fix revert.",
                    entry.relative_path
                ));
            }
            let dst = game_dir.join(rel_path);
            if entry.action == "added" {
                if dst.exists() {
                    if let Err(e) = fs::remove_file(&dst) {
                        return FixApplicationResult::err(format!(
                            "Failed to remove added file '{}': {}",
                            entry.relative_path, e
                        ));
                    }
                }
                reverted.push(entry.relative_path.clone());
            } else if entry.action == "modified" {
                if is_already_reverted {
                    reverted.push(entry.relative_path.clone());
                } else if let Some(bkp_rel) = &entry.backup_path {
                    let bkp_path = Path::new(bkp_rel);
                    if !LuaInstaller::is_safe_relative_path(bkp_path) {
                        return FixApplicationResult::err(format!(
                            "Unsafe backup path '{}' encountered during fix revert.",
                            bkp_rel
                        ));
                    }
                    let bkp = fix_meta_dir.join(bkp_path);
                    if let Some(p) = dst.parent() {
                        if let Err(e) = fs::create_dir_all(p) {
                            return FixApplicationResult::err(format!(
                                "Failed to recreate directory for '{}': {}",
                                entry.relative_path, e
                            ));
                        }
                    }
                    if let Err(e) = fs::copy(&bkp, &dst) {
                        return FixApplicationResult::err(format!(
                            "Failed to restore backup for '{}': {}",
                            entry.relative_path, e
                        ));
                    }
                    reverted.push(entry.relative_path.clone());
                }
            }
        }

        let _ = fs::remove_file(&record_file);
        let _ = fs::remove_dir_all(fix_meta_dir.join("backups").join(&safe_id));
        if safe_id != fix_id && is_safe_fix_id(fix_id) {
            let _ = fs::remove_dir_all(fix_meta_dir.join("backups").join(fix_id));
        }

        // Preserve shared winedlloverrides.env for any remaining applied fix records (LT-75)
        Self::sync_wine_dll_overrides_env(&fix_meta_dir);

        if let Some(ref overrides_str) = record.wine_dll_overrides {
            let still_needed = Self::collect_remaining_dll_stems(&fix_meta_dir);
            let to_remove: Vec<&str> = overrides_str
                .split(';')
                .filter(|part| {
                    let stem = part.split('=').next().unwrap_or("").trim().to_ascii_lowercase();
                    !stem.is_empty() && !still_needed.contains(&stem)
                })
                .collect();
            if !to_remove.is_empty() {
                Self::remove_wine_registry_overrides(game_dir, record.app_id, &to_remove.join(";"));
            }
        }
        if let Some(idx) = index_override {
            idx.remove_fix(record.app_id, fix_id);
        } else if !cfg!(test) && !game_dir.starts_with(std::env::temp_dir()) {
            let mut idx = AppliedFixIndexService::default();
            idx.remove_fix(record.app_id, fix_id);
        }
        FixApplicationResult {
            ok: true,
            error: None,
            applied_files: reverted,
        }
    }

    pub fn configure_wine_registry_overrides(game_dir: &Path, app_id: u32, dlls: &[String]) {
        if dlls.is_empty() {
            return;
        }
        let pfx_reg = game_dir
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("compatdata").join(app_id.to_string()).join("pfx/user.reg"));
        if let Some(reg_path) = pfx_reg {
            if !reg_path.is_file() {
                if let Some(parent) = reg_path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let initial = format!("WINE REGISTRY Version 2\n;; All keys relative to \\User\\...\n\n[Software\\\\Wine\\\\DllOverrides]\n\n");
                let _ = fs::write(&reg_path, initial);
            }
            if let Ok(content) = read_wine_registry_text(&reg_path) {
                let mut new_content = content.clone();
                let section_header = "[Software\\\\Wine\\\\DllOverrides]";
                let mut found_section = None;
                for (_idx, line) in new_content.lines().enumerate() {
                    let trimmed = line.trim();
                    let norm = trimmed.replace('/', "\\");
                    if norm.eq_ignore_ascii_case("[Software\\Wine\\DllOverrides]")
                        || norm.eq_ignore_ascii_case("[Software\\\\Wine\\\\DllOverrides]")
                    {
                        found_section = Some(line.to_string());
                        break;
                    }
                }
                let section_match = match found_section {
                    Some(s) => s,
                    None => {
                        new_content.push_str(&format!("\n{}\n", section_header));
                        section_header.to_string()
                    }
                };
                for dll in dlls {
                    let entry = format!("\"{}\"= \"native,builtin\"", dll);
                    let dll_prefix = format!("\"{}\"=", dll.to_ascii_lowercase());
                    let has_dll = new_content
                        .lines()
                        .any(|l| l.trim().to_ascii_lowercase().starts_with(&dll_prefix));
                    if !has_dll {
                        if let Some(pos) = new_content.find(&section_match) {
                            let line_end = new_content[pos..].find('\n').map(|p| pos + p + 1).unwrap_or(pos + section_match.len());
                            new_content.insert_str(line_end, &format!("{}\n", entry));
                        }
                    }
                }
                let _ = fs::write(&reg_path, new_content);
            }
        }
    }

    pub fn remove_wine_registry_overrides(game_dir: &Path, app_id: u32, overrides_str: &str) {
        let pfx_reg = game_dir
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("compatdata").join(app_id.to_string()).join("pfx/user.reg"));
        if let Some(reg_path) = pfx_reg {
            if reg_path.is_file() {
                if let Ok(content) = read_wine_registry_text(&reg_path) {
                    let mut lines = content.lines().map(String::from).collect::<Vec<_>>();
                    for part in overrides_str.split(';') {
                        let dll_stem = part.split('=').next().unwrap_or("").trim();
                        if !dll_stem.is_empty() {
                            let prefix = format!("\"{}\"=", dll_stem.to_ascii_lowercase());
                            lines.retain(|l| !l.trim().to_ascii_lowercase().starts_with(&prefix));
                        }
                    }
                    let _ = fs::write(&reg_path, lines.join("\n"));
                }
            }
        }
    }

    pub fn get_wine_dll_overrides(game_dir: &Path, fix_id: &str) -> Option<String> {
        if !is_safe_fix_id(fix_id) {
            return None;
        }
        let fix_meta_dir = game_dir.join(FIX_DIR_NAME);
        let safe_file = fix_meta_dir.join(format!("{}.json", sanitize_fix_id(fix_id)));
        let legacy_file = fix_meta_dir.join(format!("{}.json", fix_id));
        let raw = fs::read_to_string(&safe_file)
            .or_else(|_| fs::read_to_string(&legacy_file))
            .ok()?;
        let record = serde_json::from_str::<AppliedFixRecord>(&raw).ok()?;
        record.wine_dll_overrides
    }
}

fn collect_files_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let Ok(ft) = e.file_type() else {
            continue;
        };
        if ft.is_symlink() {
            continue;
        }
        let p = e.path();
        if ft.is_dir() {
            collect_files_recursive(&p, out);
        } else if ft.is_file() {
            out.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_apply_and_sha256_verified_revert() {
        let dir = tempfile::tempdir().unwrap();
        let game_dir = dir.path().join("game");
        let extract_dir = dir.path().join("extract");
        fs::create_dir_all(&game_dir).unwrap();
        fs::create_dir_all(&extract_dir).unwrap();

        fs::write(game_dir.join("steam_api64.dll"), b"ORIGINAL_DLL").unwrap();
        fs::write(extract_dir.join("steam_api64.dll"), b"PATCHED_DLL").unwrap();
        fs::write(extract_dir.join("steam_settings.ini"), b"ADDED_INI").unwrap();

        let res =
            FixesService::apply_extracted_with_backup(1245620, "fix1", &extract_dir, &game_dir);
        assert!(res.ok);
        assert_eq!(
            fs::read(game_dir.join("steam_api64.dll")).unwrap(),
            b"PATCHED_DLL"
        );
        assert!(FixesService::is_fix_applied(&game_dir, "fix1"));

        // Re-applying the same fix_id must be rejected so original backups aren't overwritten
        let dup =
            FixesService::apply_extracted_with_backup(1245620, "fix1", &extract_dir, &game_dir);
        assert!(!dup.ok);

        let rev = FixesService::unfix_game(&game_dir, "fix1");
        assert!(rev.ok);
        assert_eq!(
            fs::read(game_dir.join("steam_api64.dll")).unwrap(),
            b"ORIGINAL_DLL"
        );
        assert!(!game_dir.join("steam_settings.ini").exists());
        assert!(!FixesService::is_fix_applied(&game_dir, "fix1"));
    }

    #[test]
    fn blocks_fix_id_traversal_empty_apply_and_missing_backup_revert() {
        let dir = tempfile::tempdir().unwrap();
        let game_dir = dir.path().join("game");
        let extract_dir = dir.path().join("extract");
        fs::create_dir_all(&game_dir).unwrap();
        fs::create_dir_all(&extract_dir).unwrap();

        // Reject traversal fix_ids
        assert!(!FixesService::is_fix_applied(&game_dir, "../escape"));
        assert!(
            !FixesService::apply_extracted_with_backup(
                1245620,
                "../escape",
                &extract_dir,
                &game_dir
            )
            .ok
        );
        assert!(!FixesService::unfix_game(&game_dir, "../escape").ok);

        // Empty extract_dir must fail rather than writing an empty fix record
        let empty = FixesService::apply_extracted_with_backup(
            1245620,
            "empty-fix",
            &extract_dir,
            &game_dir,
        );
        assert!(!empty.ok);
        assert!(!FixesService::is_fix_applied(&game_dir, "empty-fix"));

        // Missing backup file must abort revert AND preserve fix record
        fs::write(game_dir.join("app.dll"), b"ORIG").unwrap();
        fs::write(extract_dir.join("app.dll"), b"MOD").unwrap();
        assert!(
            FixesService::apply_extracted_with_backup(1245620, "fix2", &extract_dir, &game_dir).ok
        );
        fs::remove_file(game_dir.join(".luatools-fix/backups/fix2/app.dll")).unwrap();
        let bad_rev = FixesService::unfix_game(&game_dir, "fix2");
        assert!(!bad_rev.ok);
        assert!(FixesService::is_fix_applied(&game_dir, "fix2"));
    }

    #[test]
    fn online_fix_id_with_colon_and_winedlloverrides_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let game_dir = dir.path().join("game");
        let extract_dir = dir.path().join("extract");
        fs::create_dir_all(&game_dir).unwrap();
        fs::create_dir_all(&extract_dir).unwrap();

        let online_fix_id = "online-fix:6ba7b810-9dad-11d1-80b4-00c04fd430c8";
        assert!(is_safe_fix_id(online_fix_id));

        fs::write(extract_dir.join("winmm.dll"), b"ONLINEFIX_HOOK").unwrap();
        fs::write(extract_dir.join("OnlineFix64.dll"), b"ONLINEFIX_DLL").unwrap();

        let res = FixesService::apply_extracted_with_backup(1245620, online_fix_id, &extract_dir, &game_dir);
        assert!(res.ok);
        assert!(FixesService::is_fix_applied(&game_dir, online_fix_id));

        let env_file = game_dir.join(FIX_DIR_NAME).join("winedlloverrides.env");
        assert!(env_file.exists());
        let env_content = fs::read_to_string(&env_file).unwrap();
        assert!(env_content.contains("winmm=n,b"));
        assert!(env_content.contains("onlinefix64=n,b"));

        let overrides = FixesService::get_wine_dll_overrides(&game_dir, online_fix_id);
        assert!(overrides.is_some());
        let ov = overrides.unwrap();
        assert!(ov.contains("winmm=n,b"));
        assert!(ov.contains("onlinefix64=n,b"));

        let unfix = FixesService::unfix_game(&game_dir, online_fix_id);
        assert!(unfix.ok);
        assert!(!env_file.exists());
        assert!(!game_dir.join("winmm.dll").exists());
        assert!(!FixesService::is_fix_applied(&game_dir, online_fix_id));
    }

    #[test]
    fn wine_registry_reads_utf16le_bom_user_reg() {
        let dir = tempfile::tempdir().unwrap();
        let steamapps = dir.path().join("steamapps");
        let game_dir = steamapps.join("common/MyGame");
        let pfx_dir = steamapps.join("compatdata/1245620/pfx");
        fs::create_dir_all(&game_dir).unwrap();
        fs::create_dir_all(&pfx_dir).unwrap();

        let reg_text = "WINE REGISTRY Version 2\n[Software\\Wine\\DllOverrides]\n";
        let mut utf16_bytes = vec![0xFFu8, 0xFEu8];
        for unit in reg_text.encode_utf16() {
            utf16_bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let user_reg = pfx_dir.join("user.reg");
        fs::write(&user_reg, utf16_bytes).unwrap();

        FixesService::configure_wine_registry_overrides(
            &game_dir,
            1245620,
            &["winmm".to_string()],
        );
        let updated = fs::read_to_string(&user_reg).unwrap();
        assert!(updated.contains("\"winmm\"= \"native,builtin\""));

        FixesService::remove_wine_registry_overrides(&game_dir, 1245620, "winmm=n,b");
        let cleaned = fs::read_to_string(&user_reg).unwrap();
        assert!(!cleaned.contains("\"winmm\"="));
    }

    #[test]
    fn unfix_allows_already_reverted_hash_before_and_preserves_shared_winedlloverrides() {
        let dir = tempfile::tempdir().unwrap();
        let game_dir = dir.path().join("game");
        let extract1 = dir.path().join("extract1");
        let extract2 = dir.path().join("extract2");
        fs::create_dir_all(&game_dir).unwrap();
        fs::create_dir_all(&extract1).unwrap();
        fs::create_dir_all(&extract2).unwrap();

        fs::write(game_dir.join("engine.dll"), b"ORIG_ENGINE").unwrap();
        fs::write(extract1.join("engine.dll"), b"FIX1_ENGINE").unwrap();
        fs::write(extract1.join("winmm.dll"), b"FIX1_WINMM").unwrap();
        fs::write(extract2.join("dinput8.dll"), b"FIX2_DINPUT").unwrap();

        let idx_path = dir.path().join("applied-fixes.json");
        let mut idx = AppliedFixIndexService::new(Some(idx_path));

        let r1 = FixesService::apply_extracted_with_backup_with_index(
            1245620,
            "online-fix:1111",
            &extract1,
            &game_dir,
            &mut idx,
        );
        assert!(r1.ok);

        // Verify LT-76: AppliedFixIndexService received real hashes, backup path, and wine overrides
        let rec1 = idx.get_fix(1245620, "online-fix:1111").unwrap();
        assert_eq!(rec1.files.len(), 2);
        let eng_entry = rec1.files.iter().find(|f| f.relative_path == "engine.dll").unwrap();
        assert!(eng_entry.hash_before.is_some());
        assert!(!eng_entry.hash_after.is_empty());
        assert_ne!(eng_entry.hash_before.as_deref(), Some(eng_entry.hash_after.as_str()));
        assert!(eng_entry.backup_path.as_deref().unwrap().contains("online-fix_1111"));
        assert!(rec1.wine_dll_overrides.as_deref().unwrap().contains("winmm=n,b"));

        let r2 = FixesService::apply_extracted_with_backup_with_index(
            1245620,
            "online-fix:2222",
            &extract2,
            &game_dir,
            &mut idx,
        );
        assert!(r2.ok);

        // Shared winedlloverrides.env must contain both fixes' DLL overrides
        let env_file = game_dir.join(FIX_DIR_NAME).join("winedlloverrides.env");
        let env_both = fs::read_to_string(&env_file).unwrap();
        assert!(env_both.contains("winmm=n,b"));
        assert!(env_both.contains("dinput8=n,b"));

        // Simulate partial revert / interrupted install: engine.dll already restored to ORIG_ENGINE (hash_before)
        fs::write(game_dir.join("engine.dll"), b"ORIG_ENGINE").unwrap();
        let rev1 = FixesService::unfix_game_with_index(&game_dir, "online-fix:1111", &mut idx);
        assert!(rev1.ok, "Expected already-reverted hash_before to succeed: {:?}", rev1.error);
        assert!(!idx.is_fix_applied(1245620, "online-fix:1111"));
        assert!(idx.is_fix_applied(1245620, "online-fix:2222"));

        // Shared winedlloverrides.env must survive with fix 2's override intact
        assert!(env_file.exists());
        let env_after_1 = fs::read_to_string(&env_file).unwrap();
        assert!(env_after_1.contains("dinput8=n,b"));
        assert!(!env_after_1.contains("winmm=n,b"));

        // Reverting the last remaining fix removes winedlloverrides.env and clears the index
        let rev2 = FixesService::unfix_game_with_index(&game_dir, "online-fix:2222", &mut idx);
        assert!(rev2.ok);
        assert!(!env_file.exists());
        assert!(!idx.is_fix_applied(1245620, "online-fix:2222"));
    }
}
