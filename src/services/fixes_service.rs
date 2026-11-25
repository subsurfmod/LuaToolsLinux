//! FixesService — Game fixes catalog, `.luatools-fix/` SHA-256 backup, apply & verified revert.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::services::api_client::{DRMFixFixItem, LuaToolsApiClient};
use crate::services::lua_installer::LuaInstaller;
use crate::services::settings_service::SettingsService;
use crate::services::slssteam_service::SlssteamService;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedFixRecord {
    pub app_id: u32,
    pub fix_id: String,
    pub applied_at: String,
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
        install_dir
            .join(FIX_DIR_NAME)
            .join(format!("{}.json", fix_id))
            .exists()
    }

    pub fn apply_drm_fix_manifest<F>(
        api: &LuaToolsApiClient,
        installer: &LuaInstaller,
        app_id: u32,
        fix: &DRMFixFixItem,
        settings_svc: Option<&mut SettingsService>,
        sls_svc: Option<&mut SlssteamService>,
        progress_cb: F,
    ) -> FixApplicationResult
    where
        F: FnMut(f32, String),
    {
        if !is_safe_fix_id(&fix.id) {
            return FixApplicationResult::err("Invalid fix ID.");
        }
        let tmp = std::env::temp_dir().join(format!(
            "luatools_fix_mf_{}_{}_{}",
            std::process::id(),
            app_id,
            fix.id
        ));
        let _ = fs::create_dir_all(&tmp);
        let fallback = fix
            .manifest_filename
            .clone()
            .unwrap_or_else(|| format!("{}.zip", app_id));

        let res = match api.download_drm_fix_slot(&fix.id, "manifest", &tmp, &fallback, progress_cb)
        {
            Ok(dl_path) => {
                let inst_res = installer.install_any(
                    &dl_path,
                    Some(app_id),
                    true,
                    Some(&format!("fix:{}", fix.id)),
                    settings_svc,
                    sls_svc,
                );
                if inst_res.ok {
                    FixApplicationResult {
                        ok: true,
                        error: None,
                        applied_files: vec![dl_path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string()],
                    }
                } else {
                    FixApplicationResult::err(
                        inst_res
                            .error
                            .unwrap_or_else(|| "Failed to install manifest.".into()),
                    )
                }
            }
            Err(e) => FixApplicationResult::err(e),
        };
        let _ = fs::remove_dir_all(&tmp);
        res
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

        let fix_meta_dir = game_dir.join(FIX_DIR_NAME);
        let backup_dir = fix_meta_dir.join("backups").join(fix_id);
        if let Err(e) = fs::create_dir_all(&backup_dir) {
            return FixApplicationResult::err(e.to_string());
        }

        let mut records = Vec::new();
        let mut applied = Vec::new();

        for src in files_to_apply {
            let Ok(rel) = src.strip_prefix(extract_dir) else {
                continue;
            };
            if !LuaInstaller::is_safe_relative_path(rel) {
                continue;
            }
            let rel_posix = rel.to_string_lossy().replace('\\', "/");
            let dst = game_dir.join(rel);
            if let Some(parent) = dst.parent() {
                if let Err(e) = fs::create_dir_all(parent) {
                    Self::rollback_partial_apply(game_dir, &fix_meta_dir, &records);
                    let _ = fs::remove_dir_all(&backup_dir);
                    return FixApplicationResult::err(format!("Failed to create folder: {}", e));
                }
            }

            if dst.exists() {
                let Ok(hash_before) = sha256_file(&dst) else {
                    Self::rollback_partial_apply(game_dir, &fix_meta_dir, &records);
                    let _ = fs::remove_dir_all(&backup_dir);
                    return FixApplicationResult::err(format!(
                        "Failed to hash existing file {}",
                        rel_posix
                    ));
                };
                let bkp_file = backup_dir.join(rel);
                if let Some(bp) = bkp_file.parent() {
                    if let Err(e) = fs::create_dir_all(bp) {
                        Self::rollback_partial_apply(game_dir, &fix_meta_dir, &records);
                        let _ = fs::remove_dir_all(&backup_dir);
                        return FixApplicationResult::err(format!("Backup dir failed: {}", e));
                    }
                }
                if let Err(e) = fs::copy(&dst, &bkp_file) {
                    Self::rollback_partial_apply(game_dir, &fix_meta_dir, &records);
                    let _ = fs::remove_dir_all(&backup_dir);
                    return FixApplicationResult::err(format!("Backup failed: {}", e));
                }
                if let Err(e) = fs::copy(&src, &dst) {
                    let _ = fs::copy(&bkp_file, &dst);
                    Self::rollback_partial_apply(game_dir, &fix_meta_dir, &records);
                    let _ = fs::remove_dir_all(&backup_dir);
                    return FixApplicationResult::err(format!("Apply failed: {}", e));
                }
                let Ok(hash_after) = sha256_file(&dst) else {
                    let _ = fs::copy(&bkp_file, &dst);
                    Self::rollback_partial_apply(game_dir, &fix_meta_dir, &records);
                    let _ = fs::remove_dir_all(&backup_dir);
                    return FixApplicationResult::err("Post-apply hash failed.");
                };
                records.push(FixFileEntry {
                    relative_path: rel_posix.clone(),
                    action: "modified".into(),
                    backup_path: Some(format!("backups/{}/{}", fix_id, rel_posix)),
                    hash_before: Some(hash_before),
                    hash_after,
                });
            } else {
                if let Err(e) = fs::copy(&src, &dst) {
                    Self::rollback_partial_apply(game_dir, &fix_meta_dir, &records);
                    let _ = fs::remove_dir_all(&backup_dir);
                    return FixApplicationResult::err(format!("Apply failed: {}", e));
                }
                let Ok(hash_after) = sha256_file(&dst) else {
                    let _ = fs::remove_file(&dst);
                    Self::rollback_partial_apply(game_dir, &fix_meta_dir, &records);
                    let _ = fs::remove_dir_all(&backup_dir);
                    return FixApplicationResult::err("Post-apply hash failed.");
                };
                records.push(FixFileEntry {
                    relative_path: rel_posix.clone(),
                    action: "added".into(),
                    backup_path: None,
                    hash_before: None,
                    hash_after,
                });
            }
            applied.push(rel_posix);
        }

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
            let val = dll_overrides
                .iter()
                .map(|d| format!("{}=n,b", d))
                .collect::<Vec<_>>()
                .join(";");
            let env_content = format!("WINEDLLOVERRIDES=\"{}\"\n", val);
            let _ = fs::write(fix_meta_dir.join("winedlloverrides.env"), &env_content);
            Self::configure_wine_registry_overrides(game_dir, app_id, &dll_overrides);
            Some(val)
        } else {
            None
        };

        let record = AppliedFixRecord {
            app_id,
            fix_id: fix_id.to_string(),
            applied_at: Utc::now().to_rfc3339(),
            files: records.clone(),
            wine_dll_overrides: wine_overrides_str,
        };
        let record_file = fix_meta_dir.join(format!("{}.json", fix_id));
        let js = match serde_json::to_string_pretty(&record) {
            Ok(s) => s,
            Err(e) => {
                Self::rollback_partial_apply(game_dir, &fix_meta_dir, &records);
                let _ = fs::remove_dir_all(&backup_dir);
                return FixApplicationResult::err(e.to_string());
            }
        };
        if let Err(e) = fs::write(&record_file, js) {
            Self::rollback_partial_apply(game_dir, &fix_meta_dir, &records);
            let _ = fs::remove_dir_all(&backup_dir);
            return FixApplicationResult::err(e.to_string());
        }

        if !cfg!(test) && !game_dir.starts_with(std::env::temp_dir()) {
            let _ = AppliedFixIndexService::default().record_fix_applied(
                app_id,
                fix_id,
                game_dir,
                applied.len(),
            );
        }

        FixApplicationResult {
            ok: true,
            error: None,
            applied_files: applied,
        }
    }

    pub fn unfix_game(game_dir: &Path, fix_id: &str) -> FixApplicationResult {
        if !is_safe_fix_id(fix_id) {
            return FixApplicationResult::err("Invalid fix ID.");
        }
        let fix_meta_dir = game_dir.join(FIX_DIR_NAME);
        let record_file = fix_meta_dir.join(format!("{}.json", fix_id));
        let Ok(raw) = fs::read_to_string(&record_file) else {
            return FixApplicationResult::err(format!("Fix record '{}' not found.", fix_id));
        };
        let Ok(record) = serde_json::from_str::<AppliedFixRecord>(&raw) else {
            return FixApplicationResult::err("Corrupted fix record JSON.");
        };

        // Pre-flight validation: verify paths, external modification hashes, and backup presence
        for entry in &record.files {
            if !LuaInstaller::is_safe_relative_path(Path::new(&entry.relative_path)) {
                return FixApplicationResult::err(format!(
                    "Unsafe path '{}' in fix record.",
                    entry.relative_path
                ));
            }
            let dst = game_dir.join(&entry.relative_path);
            if dst.exists() {
                if let Ok(curr_hash) = sha256_file(&dst) {
                    if curr_hash != entry.hash_after {
                        return FixApplicationResult::err(format!(
                            "File '{}' was modified externally after fix '{}' was applied. Revert newer fixes first.",
                            entry.relative_path, fix_id
                        ));
                    }
                }
            }
            if entry.action == "modified" {
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
        }

        let mut reverted = Vec::new();
        for entry in &record.files {
            let dst = game_dir.join(&entry.relative_path);
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
                if let Some(bkp_rel) = &entry.backup_path {
                    let bkp = fix_meta_dir.join(bkp_rel);
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

        let _ = fs::remove_file(fix_meta_dir.join("winedlloverrides.env"));
        if let Some(ref overrides_str) = record.wine_dll_overrides {
            Self::remove_wine_registry_overrides(game_dir, record.app_id, overrides_str);
        }
        let _ = fs::remove_file(&record_file);
        let _ = fs::remove_dir_all(fix_meta_dir.join("backups").join(fix_id));
        if !cfg!(test) && !game_dir.starts_with(std::env::temp_dir()) {
            let _ = AppliedFixIndexService::default().record_fix_reverted(record.app_id, fix_id);
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
                let section_prefix = "[Software\\Wine\\DllOverrides]";
                if !new_content.contains(section_prefix) {
                    new_content.push_str(&format!("\n{}\n", section_prefix));
                }
                for dll in dlls {
                    let entry = format!("\"{}\"= \"native,builtin\"", dll);
                    if !new_content.contains(&entry) {
                        if let Some(pos) = new_content.find(section_prefix) {
                            let line_end = new_content[pos..].find('\n').map(|p| pos + p + 1).unwrap_or(pos + section_prefix.len());
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
                            let prefix = format!("\"{}\"=", dll_stem);
                            lines.retain(|l| !l.trim().starts_with(&prefix));
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
        let record_file = fix_meta_dir.join(format!("{}.json", fix_id));
        let raw = fs::read_to_string(&record_file).ok()?;
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
}

// ============================================================================
// LT-76: AppliedFixIndexService Central Registry (applied-fixes.json)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppliedFixIndexEntry {
    pub app_id: u32,
    pub fix_id: String,
    pub game_dir: PathBuf,
    pub applied_at: String,
    pub files_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppliedFixIndex {
    pub entries: Vec<AppliedFixIndexEntry>,
}

#[derive(Debug, Clone)]
pub struct AppliedFixIndexService {
    path: PathBuf,
}

impl Default for AppliedFixIndexService {
    fn default() -> Self {
        Self::new(None)
    }
}

impl AppliedFixIndexService {
    pub fn new(custom_path: Option<PathBuf>) -> Self {
        let path = custom_path.unwrap_or_else(|| {
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("luatools/applied-fixes.json")
        });
        Self { path }
    }

    pub fn load_index(&self) -> AppliedFixIndex {
        if !self.path.exists() {
            return AppliedFixIndex::default();
        }
        let Ok(raw) = fs::read_to_string(&self.path) else {
            return AppliedFixIndex::default();
        };
        serde_json::from_str::<AppliedFixIndex>(&raw).unwrap_or_default()
    }

    pub fn record_fix_applied(
        &self,
        app_id: u32,
        fix_id: &str,
        game_dir: &Path,
        files_count: usize,
    ) -> Result<(), String> {
        let mut index = self.load_index();
        index.entries.retain(|e| !(e.app_id == app_id && e.fix_id == fix_id));
        index.entries.push(AppliedFixIndexEntry {
            app_id,
            fix_id: fix_id.to_string(),
            game_dir: game_dir.to_path_buf(),
            applied_at: chrono::Utc::now().to_rfc3339(),
            files_count,
        });
        self.persist(&index)
    }

    pub fn record_fix_reverted(&self, app_id: u32, fix_id: &str) -> Result<(), String> {
        let mut index = self.load_index();
        let orig_len = index.entries.len();
        index.entries.retain(|e| !(e.app_id == app_id && e.fix_id == fix_id));
        if index.entries.len() != orig_len {
            self.persist(&index)?;
        }
        Ok(())
    }

    pub fn list_verified_entries(&self) -> Vec<AppliedFixIndexEntry> {
        let index = self.load_index();
        let mut live = Vec::new();
        let mut changed = false;

        for entry in index.entries {
            if FixesService::is_fix_applied(&entry.game_dir, &entry.fix_id) {
                live.push(entry);
            } else {
                changed = true;
            }
        }

        if changed {
            let _ = self.persist(&AppliedFixIndex {
                entries: live.clone(),
            });
        }
        live
    }

    fn persist(&self, index: &AppliedFixIndex) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let json = serde_json::to_string_pretty(index).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, json).map_err(|e| e.to_string())?;
        if self.path.exists() {
            let _ = fs::copy(&self.path, self.path.with_extension("bak"));
        }
        fs::rename(&tmp, &self.path).map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests_applied_fixes_index {
    use super::*;

    #[test]
    fn test_applied_fix_index_service_lifecycle() {
        let tmp = tempfile::tempdir().unwrap();
        let index_path = tmp.path().join("applied-fixes.json");
        let svc = AppliedFixIndexService::new(Some(index_path));

        let game_dir = tmp.path().join("elden_ring");
        fs::create_dir_all(&game_dir).unwrap();

        svc.record_fix_applied(1245620, "fix:seamless", &game_dir, 3).unwrap();
        let idx = svc.load_index();
        assert_eq!(idx.entries.len(), 1);
        assert_eq!(idx.entries[0].app_id, 1245620);
        assert_eq!(idx.entries[0].fix_id, "fix:seamless");

        svc.record_fix_reverted(1245620, "fix:seamless").unwrap();
        let idx2 = svc.load_index();
        assert_eq!(idx2.entries.len(), 0);
    }
}
