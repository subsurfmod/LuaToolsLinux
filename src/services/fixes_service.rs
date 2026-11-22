//! FixesService — Game & Denuvo fixes catalog, `.luatools-fix/` SHA-256 backup, apply & verified revert.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::services::api_client::{DenuvoFixItem, LuaToolsApiClient};
use crate::services::lua_installer::LuaInstaller;
use crate::services::settings_service::SettingsService;
use crate::services::slssteam_service::SlssteamService;

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

#[derive(Debug, Clone, Default)]
pub struct FixesService;

impl FixesService {
    pub fn is_fix_applied(install_dir: &Path, fix_id: &str) -> bool {
        install_dir
            .join(FIX_DIR_NAME)
            .join(format!("{}.json", fix_id))
            .exists()
    }

    pub fn apply_denuvo_manifest<F>(
        api: &LuaToolsApiClient,
        installer: &LuaInstaller,
        app_id: u32,
        fix: &DenuvoFixItem,
        settings_svc: Option<&mut SettingsService>,
        sls_svc: Option<&mut SlssteamService>,
        progress_cb: F,
    ) -> FixApplicationResult
    where
        F: FnMut(f32, String),
    {
        let tmp = std::env::temp_dir().join(format!("luatools_fix_mf_{}_{}", app_id, fix.id));
        let _ = fs::create_dir_all(&tmp);
        let fallback = fix
            .manifest_filename
            .clone()
            .unwrap_or_else(|| format!("{}.zip", app_id));

        let res = match api.download_denuvo_slot(&fix.id, "manifest", &tmp, &fallback, progress_cb)
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

    pub fn apply_denuvo_fix<F>(
        api: &LuaToolsApiClient,
        app_id: u32,
        fix: &DenuvoFixItem,
        install_dir: &Path,
        progress_cb: F,
    ) -> FixApplicationResult
    where
        F: FnMut(f32, String),
    {
        if !install_dir.exists() {
            return FixApplicationResult::err(format!(
                "Game directory does not exist: {}",
                install_dir.display()
            ));
        }

        let tmp = std::env::temp_dir().join(format!("luatools_fix_dl_{}_{}", app_id, fix.id));
        let extract_dir = tmp.join("extract");
        let _ = fs::remove_dir_all(&tmp);
        if let Err(e) = fs::create_dir_all(&extract_dir) {
            return FixApplicationResult::err(e.to_string());
        }

        let fallback = fix
            .fix_filename
            .clone()
            .unwrap_or_else(|| format!("{}.zip", fix.id));

        let dl_path = match api.download_denuvo_slot(&fix.id, "fix", &tmp, &fallback, progress_cb) {
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
                let Ok(mut entry) = archive.by_index(i) else {
                    continue;
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
                    let _ = fs::create_dir_all(p);
                }
                if let Ok(mut out) = File::create(&target) {
                    let _ = io::copy(&mut entry, &mut out);
                }
            }
        } else if let Some(fname) = dl_path.file_name() {
            let _ = fs::copy(&dl_path, extract_dir.join(fname));
        }

        let res = Self::apply_extracted_with_backup(app_id, &fix.id, &extract_dir, install_dir);
        let _ = fs::remove_dir_all(&tmp);
        res
    }

    pub fn apply_extracted_with_backup(
        app_id: u32,
        fix_id: &str,
        extract_dir: &Path,
        game_dir: &Path,
    ) -> FixApplicationResult {
        let fix_meta_dir = game_dir.join(FIX_DIR_NAME);
        let backup_dir = fix_meta_dir.join("backups").join(fix_id);
        if let Err(e) = fs::create_dir_all(&backup_dir) {
            return FixApplicationResult::err(e.to_string());
        }

        let mut files_to_apply = Vec::new();
        collect_files_recursive(extract_dir, &mut files_to_apply);

        let mut records = Vec::new();
        let mut applied = Vec::new();

        for src in files_to_apply {
            let Ok(rel) = src.strip_prefix(extract_dir) else {
                continue;
            };
            let rel_posix = rel.to_string_lossy().replace('\\', "/");
            let dst = game_dir.join(rel);
            if let Some(parent) = dst.parent() {
                let _ = fs::create_dir_all(parent);
            }

            if dst.exists() {
                let Ok(hash_before) = sha256_file(&dst) else {
                    return FixApplicationResult::err(format!(
                        "Failed to hash existing file {}",
                        rel_posix
                    ));
                };
                let bkp_file = backup_dir.join(rel);
                if let Some(bp) = bkp_file.parent() {
                    let _ = fs::create_dir_all(bp);
                }
                if let Err(e) = fs::copy(&dst, &bkp_file) {
                    return FixApplicationResult::err(format!("Backup failed: {}", e));
                }
                if let Err(e) = fs::copy(&src, &dst) {
                    return FixApplicationResult::err(format!("Apply failed: {}", e));
                }
                let Ok(hash_after) = sha256_file(&dst) else {
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
                    return FixApplicationResult::err(format!("Apply failed: {}", e));
                }
                let Ok(hash_after) = sha256_file(&dst) else {
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

        let record = AppliedFixRecord {
            app_id,
            fix_id: fix_id.to_string(),
            applied_at: Utc::now().to_rfc3339(),
            files: records,
        };
        let record_file = fix_meta_dir.join(format!("{}.json", fix_id));
        if let Ok(js) = serde_json::to_string_pretty(&record) {
            if let Err(e) = fs::write(&record_file, js) {
                return FixApplicationResult::err(e.to_string());
            }
        }

        FixApplicationResult {
            ok: true,
            error: None,
            applied_files: applied,
        }
    }

    pub fn unfix_game(game_dir: &Path, fix_id: &str) -> FixApplicationResult {
        let fix_meta_dir = game_dir.join(FIX_DIR_NAME);
        let record_file = fix_meta_dir.join(format!("{}.json", fix_id));
        let Ok(raw) = fs::read_to_string(&record_file) else {
            return FixApplicationResult::err(format!("Fix record '{}' not found.", fix_id));
        };
        let Ok(record) = serde_json::from_str::<AppliedFixRecord>(&raw) else {
            return FixApplicationResult::err("Corrupted fix record JSON.");
        };

        for entry in &record.files {
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
        }

        let mut reverted = Vec::new();
        for entry in &record.files {
            let dst = game_dir.join(&entry.relative_path);
            if entry.action == "added" {
                let _ = fs::remove_file(&dst);
                reverted.push(entry.relative_path.clone());
            } else if entry.action == "modified" {
                if let Some(bkp_rel) = &entry.backup_path {
                    let bkp = fix_meta_dir.join(bkp_rel);
                    if bkp.exists() {
                        if let Some(p) = dst.parent() {
                            let _ = fs::create_dir_all(p);
                        }
                        if fs::copy(&bkp, &dst).is_ok() {
                            reverted.push(entry.relative_path.clone());
                        }
                    }
                }
            }
        }

        let _ = fs::remove_file(&record_file);
        let _ = fs::remove_dir_all(fix_meta_dir.join("backups").join(fix_id));
        FixApplicationResult {
            ok: true,
            error: None,
            applied_files: reverted,
        }
    }
}

fn collect_files_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_files_recursive(&p, out);
        } else if p.is_file() {
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

        let rev = FixesService::unfix_game(&game_dir, "fix1");
        assert!(rev.ok);
        assert_eq!(
            fs::read(game_dir.join("steam_api64.dll")).unwrap(),
            b"ORIGINAL_DLL"
        );
        assert!(!game_dir.join("steam_settings.ini").exists());
        assert!(!FixesService::is_fix_applied(&game_dir, "fix1"));
    }
}
