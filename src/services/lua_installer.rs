//! LuaInstaller — installs `.lua`, `.manifest`, and `.zip` archives into Steam,
//! captures variants in `LuaVault`, and synchronizes `SLSsteam` `config.yaml`.

use std::fs::{self, File};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use regex::Regex;
use zip::ZipArchive;

use crate::models::lua_vault::{LuaVault, KIND_BUILD, KIND_DEFAULT};
use crate::models::steam::SteamInstallation;
use crate::parsers::lua_parser::{LuaContents, LuaFileParser};
use crate::services::settings_service::SettingsService;
use crate::services::slssteam_service::SlssteamService;

#[derive(Debug, Clone, Default)]
pub struct InstallResult {
    pub ok: bool,
    pub lua_installed: bool,
    pub manifest_count: usize,
    pub failed: Vec<String>,
    pub error: Option<String>,
    pub app_id: Option<u32>,
    pub lua_path: Option<PathBuf>,
}

impl InstallResult {
    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct LuaInstaller {
    pub steam_installation: SteamInstallation,
    pub lua_vault: LuaVault,
}

impl LuaInstaller {
    pub fn new(steam_installation: SteamInstallation, lua_vault: LuaVault) -> Self {
        Self {
            steam_installation,
            lua_vault,
        }
    }

    pub fn stplug_in_dir(&self) -> &Path {
        &self.steam_installation.stplug_in_path
    }

    pub fn depotcache_dir(&self) -> PathBuf {
        self.steam_installation.root.join("depotcache")
    }

    pub fn build_id_from_filename(path: &Path) -> Option<String> {
        let stem = path.file_stem()?.to_str()?.trim();
        let re = Regex::new(r"^\d+_(\d+)$").ok()?;
        re.captures(stem)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
    }

    pub fn read_installed_lua_path(&self, app_id: u32) -> Option<PathBuf> {
        let p = self.stplug_in_dir().join(format!("{}.lua", app_id));
        p.exists().then_some(p)
    }

    pub fn read_lua_text(&self, app_id: u32) -> Option<String> {
        let p = self.read_installed_lua_path(app_id)?;
        fs::read_to_string(p).ok()
    }

    pub fn enumerate_installed(&self) -> Vec<(u32, PathBuf)> {
        let dir = self.stplug_in_dir();
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };

        let mut items: Vec<(u32, PathBuf, SystemTime)> = Vec::new();
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("lua") {
                continue;
            }
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if let Ok(aid) = stem.parse::<u32>() {
                let mtime = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                items.push((aid, p, mtime));
            }
        }
        items.sort_by_key(|a| std::cmp::Reverse(a.2));
        items.into_iter().map(|(aid, p, _)| (aid, p)).collect()
    }

    pub fn get_installed_games(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self
            .enumerate_installed()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    #[allow(clippy::too_many_arguments)]
    pub fn install_lua(
        &self,
        app_id: u32,
        lua_content: &str,
        force_locked: bool,
        build_id: Option<String>,
        source: Option<&str>,
        settings_svc: Option<&mut SettingsService>,
        sls_svc: Option<&mut SlssteamService>,
    ) -> InstallResult {
        let stplug = self.stplug_in_dir();
        if let Err(e) = fs::create_dir_all(stplug) {
            return InstallResult::err(format!("Failed to create stplug-in directory: {}", e));
        }

        let auto_update = settings_svc
            .as_ref()
            .map(|s| s.settings.auto_update_apps)
            .unwrap_or(false);
        let keep_pins = force_locked || build_id.is_some();

        let final_text = if auto_update && !keep_pins {
            LuaFileParser::comment_out_manifest_pins(lua_content)
        } else {
            lua_content.to_string()
        };

        let dest = stplug.join(format!("{}.lua", app_id));
        if let Err(e) = fs::write(&dest, &final_text) {
            return InstallResult::err(format!("Failed to write {}: {}", dest.display(), e));
        }

        let parsed = LuaFileParser::parse_text(&final_text, app_id);
        let kind = if build_id.is_some() {
            KIND_BUILD
        } else {
            KIND_DEFAULT
        };
        let _ = self.lua_vault.capture(
            app_id,
            &dest,
            kind,
            build_id,
            source.map(ToString::to_string),
        );

        if let Some(ss) = settings_svc {
            ss.record_loaded_app(app_id);
        }

        if let Some(sls) = sls_svc {
            Self::sync_with_slssteam(sls, app_id, &parsed);
        }

        InstallResult {
            ok: true,
            lua_installed: true,
            app_id: Some(app_id),
            lua_path: Some(dest),
            ..Default::default()
        }
    }

    pub fn install_lua_file(
        &self,
        source_lua: &Path,
        app_id: u32,
        force_locked: bool,
        source: Option<&str>,
        settings_svc: Option<&mut SettingsService>,
        sls_svc: Option<&mut SlssteamService>,
    ) -> InstallResult {
        let Ok(text) = fs::read_to_string(source_lua) else {
            return InstallResult::err(format!("Could not read {}", source_lua.display()));
        };
        let build_id = Self::build_id_from_filename(source_lua);
        self.install_lua(
            app_id,
            &text,
            force_locked,
            build_id,
            source,
            settings_svc,
            sls_svc,
        )
    }

    pub fn install_manifest_file(&self, source_manifest: &Path) -> InstallResult {
        let Some(fname) = source_manifest.file_name() else {
            return InstallResult::err("Invalid manifest path");
        };
        let dc = self.depotcache_dir();
        if let Err(e) = fs::create_dir_all(&dc) {
            return InstallResult::err(e.to_string());
        }
        let dest = dc.join(fname);
        match fs::copy(source_manifest, &dest) {
            Ok(_) => InstallResult {
                ok: true,
                manifest_count: 1,
                ..Default::default()
            },
            Err(e) => InstallResult {
                ok: false,
                failed: vec![fname.to_string_lossy().to_string()],
                error: Some(e.to_string()),
                ..Default::default()
            },
        }
    }

    pub fn is_safe_relative_path(rel: &Path) -> bool {
        !rel.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    }

    pub fn install_zip(
        &self,
        zip_path: &Path,
        expected_app_id: Option<u32>,
        force_locked: bool,
        source: Option<&str>,
        mut settings_svc: Option<&mut SettingsService>,
        mut sls_svc: Option<&mut SlssteamService>,
    ) -> InstallResult {
        let Ok(file) = File::open(zip_path) else {
            return InstallResult::err(format!("Cannot open archive {}", zip_path.display()));
        };
        let Ok(mut archive) = ZipArchive::new(file) else {
            return InstallResult::err("Invalid .zip archive");
        };

        let dc = self.depotcache_dir();
        let _ = fs::create_dir_all(&dc);
        let _ = fs::create_dir_all(self.stplug_in_dir());

        let num_lua_re = Regex::new(r"(?i)^(\d+)(?:_(\d+))?\.lua$").unwrap();
        let mut lua_installed = false;
        let mut manifest_count = 0usize;
        let mut failed = Vec::new();
        let mut installed_app_id = expected_app_id;

        for i in 0..archive.len() {
            let Ok(mut entry) = archive.by_index(i) else {
                continue;
            };
            if entry.is_dir() {
                continue;
            }
            let raw_name = PathBuf::from(entry.name());
            if !Self::is_safe_relative_path(&raw_name) {
                continue; // ZipSlip protection
            }
            let Some(file_name) = raw_name.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let lower = file_name.to_ascii_lowercase();

            if lower.ends_with(".lua") {
                let mut content = String::new();
                if io::Read::read_to_string(&mut entry, &mut content).is_err() {
                    failed.push(file_name.to_string());
                    continue;
                }
                let mut aid = expected_app_id;
                let mut build_id = None;
                if let Some(cap) = num_lua_re.captures(file_name) {
                    if let Some(id_m) = cap.get(1) {
                        aid = id_m.as_str().parse::<u32>().ok().or(aid);
                    }
                    build_id = cap.get(2).map(|m| m.as_str().to_string());
                }
                if aid.is_none() {
                    aid = LuaFileParser::infer_app_id_from_text(&content);
                }
                let Some(target_aid) = aid else {
                    failed.push(file_name.to_string());
                    continue;
                };
                let res = self.install_lua(
                    target_aid,
                    &content,
                    force_locked,
                    build_id,
                    source,
                    settings_svc.as_deref_mut(),
                    sls_svc.as_deref_mut(),
                );
                if res.lua_installed {
                    lua_installed = true;
                    installed_app_id = Some(target_aid);
                } else {
                    failed.push(file_name.to_string());
                }
            } else if lower.ends_with(".manifest") {
                let dest = dc.join(file_name);
                match File::create(&dest).and_then(|mut out| io::copy(&mut entry, &mut out)) {
                    Ok(_) => manifest_count += 1,
                    Err(_) => failed.push(file_name.to_string()),
                }
            }
        }

        if !lua_installed && manifest_count == 0 && failed.is_empty() {
            return InstallResult::err("Archive contained no .lua or .manifest files.");
        }
        let ok = lua_installed || manifest_count > 0;
        InstallResult {
            ok,
            lua_installed,
            manifest_count,
            error: (!ok).then(|| "Failed to install archive contents.".to_string()),
            failed,
            app_id: installed_app_id,
            lua_path: installed_app_id.and_then(|id| self.read_installed_lua_path(id)),
        }
    }

    pub fn install_any(
        &self,
        path: &Path,
        expected_app_id: Option<u32>,
        force_locked: bool,
        source: Option<&str>,
        settings_svc: Option<&mut SettingsService>,
        sls_svc: Option<&mut SlssteamService>,
    ) -> InstallResult {
        let Some(fname) = path.file_name().and_then(|n| n.to_str()) else {
            return InstallResult::err("Invalid file path");
        };
        let lower = fname.to_ascii_lowercase();

        if lower.ends_with(".zip") {
            return self.install_zip(
                path,
                expected_app_id,
                force_locked,
                source,
                settings_svc,
                sls_svc,
            );
        }
        if lower.ends_with(".lua") {
            let num_re = Regex::new(r"(?i)^(\d+)(?:_\d+)?\.lua$").unwrap();
            let mut aid = num_re
                .captures(fname)
                .and_then(|c| c.get(1))
                .and_then(|m| m.as_str().parse::<u32>().ok())
                .or(expected_app_id);

            if aid.is_none() {
                if let Ok(txt) = fs::read_to_string(path) {
                    aid = LuaFileParser::infer_app_id_from_text(&txt);
                }
            }
            let Some(target_aid) = aid else {
                return InstallResult::err(format!(
                    "Cannot determine AppID from '{}' (no numeric name or addappid found).",
                    fname
                ));
            };
            return self.install_lua_file(
                path,
                target_aid,
                force_locked,
                source,
                settings_svc,
                sls_svc,
            );
        }
        if lower.ends_with(".manifest") {
            return self.install_manifest_file(path);
        }

        InstallResult::err(format!("Unsupported file type: {}", fname))
    }

    pub fn uninstall_lua(
        &self,
        app_id: u32,
        remove_from_slssteam: bool,
        sls_svc: Option<&mut SlssteamService>,
    ) -> InstallResult {
        let p = self.stplug_in_dir().join(format!("{}.lua", app_id));
        if !p.exists() {
            return InstallResult::err(format!("Lua file not found for AppID {}", app_id));
        }
        if let Err(e) = fs::remove_file(&p) {
            return InstallResult::err(format!("Failed to delete lua file: {}", e));
        }
        if remove_from_slssteam {
            if let Some(sls) = sls_svc {
                sls.remove_additional_app(app_id);
                sls.remove_fake_app_id(app_id);
                let _ = sls.save();
            }
        }
        InstallResult {
            ok: true,
            lua_installed: true,
            app_id: Some(app_id),
            ..Default::default()
        }
    }

    pub fn sync_with_slssteam(sls: &mut SlssteamService, app_id: u32, lua: &LuaContents) {
        sls.add_additional_app(app_id);
        if sls.get_fake_app_id(app_id).is_none() {
            sls.set_fake_app_id(app_id, 480);
        }

        for entry in &lua.entries {
            if let Some(key) = &entry.key {
                sls.set_depot_key(entry.id, key.clone());
            } else if entry.id != app_id {
                let dlc_name = entry
                    .comment
                    .clone()
                    .unwrap_or_else(|| format!("DLC {}", entry.id));
                sls.config
                    .dlc_data
                    .entry(app_id)
                    .or_default()
                    .insert(entry.id, dlc_name);
            }
        }

        for (&tok_aid, tok_val) in &lua.tokens {
            sls.set_token(tok_aid, tok_val.clone());
        }

        let _ = sls.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::steam::SteamLibrary;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    #[test]
    fn installs_zip_rejects_zipslip_and_syncs_vault_and_slssteam() {
        let dir = tempfile::tempdir().unwrap();
        let steam_root = dir.path().join("Steam");
        let stplug = steam_root.join("config/stplug-in");
        fs::create_dir_all(&stplug).unwrap();

        let inst = SteamInstallation {
            root: steam_root.clone(),
            libraries: vec![SteamLibrary::new(steam_root.clone())],
            config_path: steam_root.join("config"),
            stplug_in_path: stplug,
        };
        let vault = LuaVault::new(Some(inst.clone()), Some(dir.path().join("vault")));
        let installer = LuaInstaller::new(inst, vault.clone());
        let mut settings = SettingsService::new(Some(dir.path().join("settings.json")));
        settings.settings.auto_update_apps = false;
        let mut sls = SlssteamService::new(Some(dir.path().join("sls.yaml")));

        let zip_path = dir.path().join("pkg.zip");
        {
            let f = File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts = SimpleFileOptions::default();
            zw.start_file("1245620.lua", opts).unwrap();
            zw.write_all(
                format!(
                    "addappid(1245620)\naddappid(1245621, 1, \"{}\")\nsetManifestid(1245621, \"998877\", 1024)\n",
                    "c".repeat(64)
                )
                .as_bytes(),
            )
            .unwrap();
            zw.start_file("1245621_998877.manifest", opts).unwrap();
            zw.write_all(b"MANIFEST").unwrap();
            zw.start_file("../../escape.lua", opts).unwrap();
            zw.write_all(b"addappid(666)\n").unwrap();
            zw.finish().unwrap();
        }

        let res = installer.install_any(
            &zip_path,
            Some(1245620),
            false,
            Some("unit-test"),
            Some(&mut settings),
            Some(&mut sls),
        );
        assert!(res.ok && res.lua_installed && res.manifest_count == 1);
        assert!(!dir.path().join("escape.lua").exists());
        assert_eq!(
            sls.config.depot_keys.get(&1245621).map(String::len),
            Some(64)
        );
        assert!(sls.get_token(1245620).is_none());
        assert_eq!(vault.get_variants(1245620).len(), 1);
    }
}
