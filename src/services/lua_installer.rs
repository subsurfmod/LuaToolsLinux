//! LuaInstaller — installs `.lua`, `.manifest`, and `.zip` archives into Steam,
//! captures variants in `LuaVault`, and synchronizes `SLSsteam` `config.yaml`.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use regex::Regex;
use serde::{Deserialize, Serialize};
use zip::ZipArchive;

use crate::models::config::default_luatools_data_dir;
use crate::models::lua_vault::{LuaVault, KIND_BUILD, KIND_DEFAULT};
use crate::models::steam::SteamInstallation;
use crate::parsers::lua_parser::{LuaContents, LuaFileParser};
use crate::services::settings_service::SettingsService;
use crate::services::slssteam_service::SlssteamService;
use crate::steam_bridge::credentials::appticket::{write_app_ticket_to, write_e_ticket_to};
use crate::steam_bridge::credentials::eticketclient::EticketClient;
use crate::steam_bridge::credentials::legacycdkey::LegacyCdKeyStore;
use crate::steam_bridge::ost_platform::steam_credential_store;
use crate::steam_bridge::ticket_store::{AppTicketData, TicketStore};

/// Valve Individual Public 64-bit `SteamID` base (`0x0110_0001_0000_0000` = `76561197960265728`).
pub const STEAM_ID64_INDIVIDUAL_BASE: u64 = 76_561_197_960_265_728;

/// Bridge-only directives (`addprocess`, `forcedenuvo`, `pinApp`, `setStat`) persisted per `app_id`
/// in `~/.local/share/luatools/bridge_directives.json` instead of polluting `SLSsteam` `config.yaml`
/// with unknown YAML keys (BST-08, BST-14, BST-15, BST-16).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppBridgeDirectives {
    pub app_id: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub process_map: BTreeMap<u32, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub force_denuvo: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denuvo_steam_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned_apps: Vec<u32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stats: BTreeMap<u32, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stat_overrides: BTreeMap<u32, BTreeMap<String, String>>,
}

impl AppBridgeDirectives {
    pub fn is_empty(&self) -> bool {
        self.process_map.is_empty()
            && self.force_denuvo.is_empty()
            && self.pinned_apps.is_empty()
            && self.stats.is_empty()
            && self.stat_overrides.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeDirectivesState {
    #[serde(default)]
    pub apps: BTreeMap<u32, AppBridgeDirectives>,
}

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
        let stamp_label = build_id.clone().or_else(|| source.map(str::to_string));
        let _ = self.lua_vault.capture(
            app_id,
            &dest,
            kind,
            build_id,
            source.map(ToString::to_string),
        );
        // LT-36: Stamp build-locked installs via LuaVault::stamp_now while preserving KIND_DEFAULT on standard installs
        if kind == KIND_BUILD {
            let _ = self.lua_vault.stamp_now(app_id, stamp_label.as_deref());
        }

        if let Some(ss) = settings_svc {
            ss.record_loaded_app(app_id);
        }

        let ticket_dir = Self::resolve_ticket_config_dir(
            Some(&self.steam_installation.root),
            &self.steam_installation.config_path,
        );
        if let Some(sls) = sls_svc {
            Self::sync_with_slssteam_internal(
                sls,
                app_id,
                &parsed,
                Some(&self.steam_installation.root),
                &ticket_dir,
            );
        } else {
            let active_sid =
                Self::resolve_active_steam_id64(Some(&self.steam_installation.root), &ticket_dir);
            let directives_path = Self::resolve_bridge_directives_path(
                None,
                Some(&self.steam_installation.root),
                &ticket_dir,
            );
            let _ = Self::sync_bridge_directives_to(&directives_path, app_id, &parsed, active_sid);
            Self::sync_credentials_with_root(
                Some(&self.steam_installation.root),
                &ticket_dir,
                &parsed,
            );
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

    /// Returns true if `dest` already exists on disk with identical bytes or already parses
    /// as a valid Steam binary manifest (LT-33: avoid overwriting existing valid manifests).
    fn should_preserve_existing_manifest(dest: &Path, incoming_bytes: &[u8]) -> bool {
        if !dest.is_file() {
            return false;
        }
        if let Ok(existing) = fs::read(dest) {
            if existing == incoming_bytes {
                return true;
            }
        }
        crate::parsers::manifest_binary::read_manifest_file(dest).is_ok()
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
        let Ok(incoming) = fs::read(source_manifest) else {
            return InstallResult {
                ok: false,
                failed: vec![fname.to_string_lossy().to_string()],
                error: Some(format!(
                    "Failed to read manifest {}",
                    source_manifest.display()
                )),
                ..Default::default()
            };
        };
        if Self::should_preserve_existing_manifest(&dest, &incoming) {
            return InstallResult {
                ok: true,
                manifest_count: 1,
                ..Default::default()
            };
        }
        match fs::write(&dest, &incoming) {
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
        let Ok(mut file) = File::open(zip_path) else {
            return InstallResult::err(format!("Cannot open archive {}", zip_path.display()));
        };

        // Check magic bytes: ZIP archives must start with PK (0x50, 0x4B)
        let mut magic = [0u8; 4];
        let mut is_zip_magic = false;
        if let Ok(n) = file.read(&mut magic) {
            if n >= 2 && magic[0] == 0x50 && magic[1] == 0x4B {
                is_zip_magic = true;
            }
        }

        if !is_zip_magic {
            // Bare uncompressed .lua file passed to install_zip or file with .zip extension that is raw lua
            if let Ok(content) = fs::read_to_string(zip_path) {
                let fname = zip_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let num_lua_re = Regex::new(r"(?i)^(\d+)(?:_(\d+))?").unwrap();
                let mut aid = expected_app_id;
                let mut build_id = None;
                if let Some(cap) = num_lua_re.captures(fname) {
                    if let Some(id_m) = cap.get(1) {
                        aid = id_m.as_str().parse::<u32>().ok().or(aid);
                    }
                    build_id = cap.get(2).map(|m| m.as_str().to_string());
                }
                if aid.is_none() {
                    aid = LuaFileParser::infer_app_id_from_text(&content);
                }
                if let Some(target_aid) = aid {
                    return self.install_lua(
                        target_aid,
                        &content,
                        force_locked,
                        build_id,
                        source,
                        settings_svc,
                        sls_svc,
                    );
                }
            }
            return InstallResult::err("Invalid .zip archive (missing PK magic bytes)");
        }

        if let Err(e) = file.seek(SeekFrom::Start(0)) {
            return InstallResult::err(format!("Failed to seek archive: {}", e));
        }

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
                let mut incoming = Vec::new();
                if io::Read::read_to_end(&mut entry, &mut incoming).is_err() {
                    failed.push(file_name.to_string());
                    continue;
                }
                if Self::should_preserve_existing_manifest(&dest, &incoming) {
                    manifest_count += 1;
                } else {
                    match fs::write(&dest, &incoming) {
                        Ok(_) => manifest_count += 1,
                        Err(_) => failed.push(file_name.to_string()),
                    }
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
            // Check magic bytes to detect bare uncompressed .lua file masquerading as .zip
            let mut magic = [0u8; 4];
            let is_zip_magic = File::open(path)
                .and_then(|mut f| {
                    f.read(&mut magic).map(|n| n >= 2 && magic[0] == 0x50 && magic[1] == 0x4B)
                })
                .unwrap_or(false);

            if !is_zip_magic {
                if let Ok(txt) = fs::read_to_string(path) {
                    let aid = expected_app_id.or_else(|| LuaFileParser::infer_app_id_from_text(&txt));
                    if let Some(target_aid) = aid {
                        let build_id = Self::build_id_from_filename(path);
                        return self.install_lua(
                            target_aid,
                            &txt,
                            force_locked,
                            build_id,
                            source,
                            settings_svc,
                            sls_svc,
                        );
                    }
                }
            }

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

        // Read and parse `.lua` BEFORE deleting it so bridge directives, eticket URLs,
        // and SLSsteam mappings are cleaned up accurately.
        let parsed_lua = self
            .read_lua_text(app_id)
            .map(|txt| LuaFileParser::parse_text(&txt, app_id));

        if let Err(e) = fs::remove_file(&p) {
            return InstallResult::err(format!("Failed to delete lua file: {}", e));
        }

        let sls_cfg_path = sls_svc.as_ref().map(|s| s.config_path.as_path());
        let directives_path = Self::resolve_bridge_directives_path(
            sls_cfg_path,
            Some(&self.steam_installation.root),
            &self.steam_installation.config_path,
        );
        let _ = Self::remove_bridge_directives_from(&directives_path, app_id);

        // BST-07: Cleanly refresh or clear `EticketClient` URL when uninstalling a script that set it
        if let Some(ref parsed) = parsed_lua {
            if !parsed.eticket_urls.is_empty() {
                Self::reconcile_eticket_url_after_uninstall(self.stplug_in_dir());
                EticketClient::clear_cached_fresh_eticket(app_id);
            }
        }

        if remove_from_slssteam {
            if let Some(sls) = sls_svc {
                sls.remove_additional_app(app_id);
                sls.remove_fake_app_id(app_id);
                sls.remove_token(app_id);
                sls.remove_cd_key(app_id);
                sls.remove_game_title(app_id);
                sls.remove_process_mapping(app_id);
                sls.remove_force_denuvo(app_id);
                sls.remove_pinned_app(app_id);
                sls.config.dlc_data.remove(&app_id);

                let mut denuvo_ids_to_prune = vec![app_id];
                if let Some(ref parsed) = parsed_lua {
                    for &did in &parsed.force_denuvo {
                        if !denuvo_ids_to_prune.contains(&did) {
                            denuvo_ids_to_prune.push(did);
                        }
                    }
                    for entry in &parsed.entries {
                        sls.config.depot_keys.remove(&entry.id);
                        sls.remove_manifest_override(entry.id);
                    }
                }
                sls.config.denuvo_games.remove(&0);
                sls.config.denuvo_games.retain(|_, apps| {
                    apps.retain(|id| !denuvo_ids_to_prune.contains(id));
                    !apps.is_empty()
                });

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

    /// Canonical production path for bridge-only directives (`~/.local/share/luatools/bridge_directives.json`).
    pub fn default_bridge_directives_path() -> PathBuf {
        if let Ok(ovr) = std::env::var("LUATOOLS_BRIDGE_DIRECTIVES_PATH") {
            let trimmed = ovr.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed);
            }
        }
        default_luatools_data_dir().join("bridge_directives.json")
    }

    pub fn bridge_directives_path(&self) -> PathBuf {
        Self::resolve_bridge_directives_path(
            None,
            Some(&self.steam_installation.root),
            &self.steam_installation.config_path,
        )
    }

    fn resolve_bridge_directives_path(
        sls_path: Option<&Path>,
        steam_root: Option<&Path>,
        config_dir: &Path,
    ) -> PathBuf {
        if let Ok(ovr) = std::env::var("LUATOOLS_BRIDGE_DIRECTIVES_PATH") {
            let trimmed = ovr.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed);
            }
        }
        #[cfg(test)]
        {
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/nonexistent"));
            let anchor = steam_root
                .and_then(Path::parent)
                .or_else(|| sls_path.and_then(Path::parent))
                .or_else(|| config_dir.parent());
            if let Some(base) = anchor {
                if !base.starts_with(&home) {
                    return base.join("luatools").join("bridge_directives.json");
                }
            }
        }
        let _ = (sls_path, steam_root, config_dir);
        Self::default_bridge_directives_path()
    }

    pub fn load_bridge_directives_from(path: &Path) -> BridgeDirectivesState {
        let Ok(raw) = fs::read_to_string(path) else {
            return BridgeDirectivesState::default();
        };
        serde_json::from_str::<BridgeDirectivesState>(&raw).unwrap_or_default()
    }

    fn write_bridge_directives_atomic(
        path: &Path,
        state: &BridgeDirectivesState,
    ) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let payload = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
        let tmp = path.with_file_name(format!(
            ".bridge_directives.json.tmp.{}",
            std::process::id()
        ));
        #[cfg(unix)]
        {
            use std::io::Write as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            let mut f = fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(&tmp)
                .map_err(|e| e.to_string())?;
            f.write_all(payload.as_bytes()).map_err(|e| e.to_string())?;
        }
        #[cfg(not(unix))]
        {
            fs::write(&tmp, payload.as_bytes()).map_err(|e| e.to_string())?;
        }
        fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    pub fn sync_bridge_directives_to(
        path: &Path,
        app_id: u32,
        lua: &LuaContents,
        active_steam_id: Option<u64>,
    ) -> Result<(), String> {
        let mut state = Self::load_bridge_directives_from(path);
        let force_denuvo: Vec<u32> = lua.force_denuvo.iter().copied().collect();
        let pinned_apps: Vec<u32> = lua.pinned_apps.iter().copied().collect();
        let entry = AppBridgeDirectives {
            app_id,
            process_map: lua.process_map.clone(),
            denuvo_steam_id: (!force_denuvo.is_empty()).then_some(active_steam_id).flatten(),
            force_denuvo,
            pinned_apps,
            stats: lua.stats.clone(),
            stat_overrides: lua.stat_overrides.clone(),
        };
        if entry.is_empty() {
            if state.apps.remove(&app_id).is_none() && !path.exists() {
                return Ok(());
            }
        } else {
            state.apps.insert(app_id, entry);
        }
        Self::write_bridge_directives_atomic(path, &state)
    }

    pub fn remove_bridge_directives_from(path: &Path, app_id: u32) -> Result<bool, String> {
        if !path.exists() {
            return Ok(false);
        }
        let mut state = Self::load_bridge_directives_from(path);
        if state.apps.remove(&app_id).is_some() {
            Self::write_bridge_directives_atomic(path, &state)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn parse_active_steam_id_from_loginusers_vdf(content: &str) -> Option<u64> {
        let mut current_sid: Option<u64> = None;
        let mut best_ts: Option<(u64, u64)> = None;
        let mut first_sid: Option<u64> = None;

        for raw_line in content.lines() {
            let trimmed = raw_line.trim();
            if let Some(rest) = trimmed.strip_prefix('"') {
                if let Some(end_q) = rest.find('"') {
                    let key_or_sec = &rest[..end_q];
                    let after = rest[end_q + 1..].trim();
                    if (after.is_empty() || after == "{")
                        && !key_or_sec.is_empty()
                        && key_or_sec.bytes().all(|b| b.is_ascii_digit())
                    {
                        if let Ok(sid) = key_or_sec.parse::<u64>() {
                            if sid >= STEAM_ID64_INDIVIDUAL_BASE {
                                current_sid = Some(sid);
                                if first_sid.is_none() {
                                    first_sid = Some(sid);
                                }
                                continue;
                            }
                        }
                    }
                    if let (Some(sid), Some(val_rest)) = (current_sid, after.strip_prefix('"')) {
                        if let Some(val_end) = val_rest.find('"') {
                            let val = val_rest[..val_end].trim();
                            if key_or_sec.eq_ignore_ascii_case("MostRecent") && val == "1" {
                                return Some(sid);
                            }
                            if key_or_sec.eq_ignore_ascii_case("Timestamp") {
                                if let Ok(ts) = val.parse::<u64>() {
                                    if best_ts.is_none_or(|(b, _)| ts > b) {
                                        best_ts = Some((ts, sid));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        best_ts.map(|(_, sid)| sid).or(first_sid)
    }

    fn infer_steam_root(steam_root: Option<&Path>, config_dir: &Path) -> Option<PathBuf> {
        if let Some(r) = steam_root {
            return Some(r.to_path_buf());
        }
        // `<steam_root>/userdata/<account_id>/config`
        if config_dir.file_name().and_then(|s| s.to_str()) == Some("config") {
            if let Some(acc_dir) = config_dir.parent() {
                if let Some(ud_dir) = acc_dir.parent() {
                    if ud_dir.file_name().and_then(|s| s.to_str()) == Some("userdata") {
                        return ud_dir.parent().map(Path::to_path_buf);
                    }
                }
                // `<steam_root>/config`
                return Some(acc_dir.to_path_buf());
            }
        }
        None
    }

    /// Resolves the active Steam user's 64-bit `SteamID` (`76561197960265728 + account_id`)
    /// from `loginusers.vdf` or `userdata/<account_id>` instead of hardcoding `0` (BST-15).
    pub fn resolve_active_steam_id64(
        steam_root: Option<&Path>,
        config_dir: &Path,
    ) -> Option<u64> {
        let inferred_root = Self::infer_steam_root(steam_root, config_dir);

        // 1. Check `loginusers.vdf` in `<steam_root>/config/loginusers.vdf` or `<config_dir>/loginusers.vdf`
        let mut loginusers_candidates = Vec::new();
        if let Some(ref r) = inferred_root {
            loginusers_candidates.push(r.join("config").join("loginusers.vdf"));
        }
        loginusers_candidates.push(config_dir.join("loginusers.vdf"));
        for candidate in loginusers_candidates {
            if let Ok(content) = fs::read_to_string(&candidate) {
                if let Some(sid) = Self::parse_active_steam_id_from_loginusers_vdf(&content) {
                    return Some(sid);
                }
            }
        }

        // 2. If `config_dir` is already `.../userdata/<account_id>/config`, extract `<account_id>`
        if config_dir.file_name().and_then(|s| s.to_str()) == Some("config") {
            if let Some(acc_dir) = config_dir.parent() {
                if acc_dir
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    == Some("userdata")
                {
                    if let Some(acc_id) = acc_dir
                        .file_name()
                        .and_then(|s| s.to_str())
                        .and_then(|s| s.parse::<u32>().ok())
                        .filter(|&id| id > 0)
                    {
                        return Some(STEAM_ID64_INDIVIDUAL_BASE + u64::from(acc_id));
                    }
                }
            }
        }

        // 3. Scan `<steam_root>/userdata/<account_id>` preferring the most recent `localconfig.vdf`
        if let Some(ref root) = inferred_root {
            let userdata = root.join("userdata");
            if let Ok(entries) = fs::read_dir(&userdata) {
                let mut best: Option<(SystemTime, u32)> = None;
                for entry in entries.flatten() {
                    let p = entry.path();
                    if !p.is_dir() {
                        continue;
                    }
                    let Some(acc_id) = p
                        .file_name()
                        .and_then(|s| s.to_str())
                        .and_then(|s| s.parse::<u32>().ok())
                        .filter(|&id| id > 0)
                    else {
                        continue;
                    };
                    let vdf = p.join("config").join("localconfig.vdf");
                    let mtime = fs::metadata(&vdf)
                        .or_else(|_| fs::metadata(&p))
                        .and_then(|m| m.modified())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    if best.is_none_or(|(b_time, _)| mtime >= b_time) {
                        best = Some((mtime, acc_id));
                    }
                }
                if let Some((_, acc_id)) = best {
                    return Some(STEAM_ID64_INDIVIDUAL_BASE + u64::from(acc_id));
                }
            }
        }

        // 4. Non-test fallback to active system Steam user via `steam_credential_store` / `TicketStore::default_path()`
        if !cfg!(test) {
            let mut account_id = 0u32;
            let mut universe = String::new();
            if steam_credential_store::get_active_user(&mut account_id, &mut universe)
                == steam_credential_store::Status::Ok
                && account_id > 0
            {
                return Some(STEAM_ID64_INDIVIDUAL_BASE + u64::from(account_id));
            }
            let def_path = TicketStore::default_path();
            if let Some(acc_id) = def_path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<u32>().ok())
                .filter(|&id| id > 0)
            {
                return Some(STEAM_ID64_INDIVIDUAL_BASE + u64::from(acc_id));
            }
        }

        None
    }

    /// Resolves the user-scoped `userdata/<account_id>/config` directory for `AppTicket`/`ETicket`
    /// persistence instead of `<steam_root>/config` where Steam ignores user tickets (BST-05, BST-06).
    pub fn resolve_ticket_config_dir(steam_root: Option<&Path>, config_dir: &Path) -> PathBuf {
        // Already pointing at `.../userdata/<account_id>/config`
        if config_dir.file_name().and_then(|s| s.to_str()) == Some("config")
            && config_dir
                .parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                == Some("userdata")
        {
            return config_dir.to_path_buf();
        }

        if let Some(root) = Self::infer_steam_root(steam_root, config_dir) {
            if let Some(sid) = Self::resolve_active_steam_id64(Some(&root), config_dir) {
                let account_id = (sid & 0xFFFF_FFFF) as u32;
                if account_id > 0
                    && (root.join("userdata").exists()
                        || root.join("config").join("loginusers.vdf").exists())
                {
                    return root
                        .join("userdata")
                        .join(account_id.to_string())
                        .join("config");
                }
            }
            let resolved = TicketStore::new_for_steam_root(&root)
                .config_dir()
                .to_path_buf();
            if resolved != root && resolved != config_dir {
                return resolved;
            }
        }

        if !cfg!(test) && !config_dir.join("localconfig.vdf").exists() {
            return TicketStore::default_path();
        }

        TicketStore::resolve_config_dir(config_dir)
    }

    fn decode_lua_ticket_hex(raw: &str) -> Option<Vec<u8>> {
        let trimmed = raw.trim();
        let clean = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
            .unwrap_or(trimmed);
        if clean.is_empty() || !clean.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        // Match upstream BST `ParseHexByte`: pad odd-length hex strings with a trailing '0'
        let normalized = if clean.len() % 2 == 1 {
            format!("{}0", clean)
        } else {
            clean.to_string()
        };
        hex::decode(normalized).ok().filter(|b| !b.is_empty())
    }

    fn reconcile_eticket_url_after_uninstall(stplug_dir: &Path) {
        let mut replacement_url: Option<String> = None;
        if let Ok(entries) = fs::read_dir(stplug_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|x| x.to_str()) != Some("lua") {
                    continue;
                }
                let aid = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                if let Ok(txt) = fs::read_to_string(&path) {
                    let parsed = LuaFileParser::parse_text(&txt, aid);
                    if let Some(url) = parsed
                        .eticket_urls
                        .iter()
                        .rev()
                        .find(|u| !u.trim().is_empty())
                    {
                        replacement_url = Some(url.trim().to_string());
                        break;
                    }
                }
            }
        }
        match replacement_url {
            Some(url) => EticketClient::set_custom_url(&url),
            None => EticketClient::clear_custom_url(),
        }
    }

    pub fn sync_with_slssteam(sls: &mut SlssteamService, app_id: u32, lua: &LuaContents) {
        Self::sync_with_slssteam_to(sls, app_id, lua, &TicketStore::default_path());
    }

    pub fn sync_credentials_to_config_dir(config_dir: &Path, lua: &LuaContents) {
        Self::sync_credentials_with_root(None, config_dir, lua);
    }

    fn sync_credentials_with_root(
        steam_root: Option<&Path>,
        config_dir: &Path,
        lua: &LuaContents,
    ) {
        let ticket_dir = Self::resolve_ticket_config_dir(steam_root, config_dir);
        let active_sid = Self::resolve_active_steam_id64(steam_root, &ticket_dir);

        if !lua.app_tickets.is_empty() || !lua.e_tickets.is_empty() {
            steam_credential_store::initialize(&ticket_dir);

            for (&aid, app_ticket) in &lua.app_tickets {
                let e_ticket = lua.e_tickets.get(&aid).cloned();
                let _ = write_app_ticket_to(
                    &ticket_dir,
                    AppTicketData {
                        app_id: aid,
                        steam_id: active_sid.map(|s| s.to_string()),
                        app_ticket: Some(app_ticket.clone()),
                        e_ticket: e_ticket.clone(),
                        ..Default::default()
                    },
                );
                if let Some(sid) = active_sid {
                    let _ = steam_credential_store::write_steam_id(aid, sid);
                }
                if let Some(bytes) = Self::decode_lua_ticket_hex(app_ticket) {
                    let _ = steam_credential_store::write_app_ticket(aid, &bytes);
                }
                if let Some(ref et) = e_ticket {
                    if let Some(ebytes) = Self::decode_lua_ticket_hex(et) {
                        let _ = steam_credential_store::write_e_ticket(aid, &ebytes);
                    }
                }
            }

            for (&aid, e_ticket) in &lua.e_tickets {
                if !lua.app_tickets.contains_key(&aid) {
                    let _ = write_e_ticket_to(&ticket_dir, aid, e_ticket.clone());
                    if let Some(sid) = active_sid {
                        let _ = steam_credential_store::write_steam_id(aid, sid);
                    }
                    if let Some(ebytes) = Self::decode_lua_ticket_hex(e_ticket) {
                        let _ = steam_credential_store::write_e_ticket(aid, &ebytes);
                    }
                }
            }
        }

        if !lua.legacy_cd_keys.is_empty() {
            let mut cdkey_store = LegacyCdKeyStore::new(&ticket_dir);
            let _ = cdkey_store.load();
            for (&aid, cdkey) in &lua.legacy_cd_keys {
                let _ = cdkey_store.set_override(aid, cdkey.clone());
            }
        }

        // BST-07: Wire persistent on-demand eticket URLs from `seteticketurl(...)`
        for url in &lua.eticket_urls {
            let trimmed = url.trim();
            if trimmed.is_empty() {
                EticketClient::clear_custom_url();
            } else {
                EticketClient::set_custom_url(trimmed);
            }
        }
    }

    pub fn sync_with_slssteam_to(
        sls: &mut SlssteamService,
        app_id: u32,
        lua: &LuaContents,
        config_dir: &Path,
    ) {
        let inferred_root = Self::infer_steam_root(None, config_dir);
        Self::sync_with_slssteam_internal(sls, app_id, lua, inferred_root.as_deref(), config_dir);
    }

    fn sync_with_slssteam_internal(
        sls: &mut SlssteamService,
        app_id: u32,
        lua: &LuaContents,
        steam_root: Option<&Path>,
        config_dir: &Path,
    ) {
        sls.add_additional_app(app_id);
        sls.config.play_not_owned = true;
        if sls.get_fake_app_id(app_id).is_none() {
            sls.set_fake_app_id(app_id, 480);
        }

        // LT-31 / LNX-02: Prune disabled DLC/depot entries so per-depot toggles remove them from SLSsteam
        for disabled in &lua.disabled_entries {
            sls.config.depot_keys.remove(&disabled.id);
            sls.remove_manifest_override(disabled.id);
            if let Some(dlcs) = sls.config.dlc_data.get_mut(&app_id) {
                dlcs.remove(&disabled.id);
            }
        }
        if let Some(dlcs) = sls.config.dlc_data.get(&app_id) {
            if dlcs.is_empty() {
                sls.config.dlc_data.remove(&app_id);
            }
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
            if let Some(ref gid) = entry.manifest_id {
                sls.set_manifest_override(entry.id, gid.clone());
            }
        }

        for (&tok_aid, tok_val) in &lua.tokens {
            sls.set_token(tok_aid, tok_val.clone());
        }

        for (aid, cdkey) in &lua.legacy_cd_keys {
            sls.set_cd_key(*aid, cdkey.clone());
        }

        // BST-08, BST-14, BST-15, BST-16: Do NOT write unknown `ProcessMap`/`ForceDenuvo`/`PinnedApps`
        // keys into SLSsteam `config.yaml`. Scrub any stale per-app entries and persist bridge-only
        // directives to `~/.local/share/luatools/bridge_directives.json`.
        sls.remove_process_mapping(app_id);
        sls.remove_force_denuvo(app_id);
        sls.remove_pinned_app(app_id);
        for &aid in lua.process_map.keys() {
            sls.remove_process_mapping(aid);
        }
        for &aid in &lua.force_denuvo {
            sls.remove_force_denuvo(aid);
        }
        for &aid in &lua.pinned_apps {
            sls.remove_pinned_app(aid);
        }

        // Resolve real 64-bit SteamID for `forcedenuvo` -> `SLSsteam` `DenuvoGames` (never `0`!)
        let active_steam_id = Self::resolve_active_steam_id64(steam_root, config_dir).or_else(|| {
            sls.config
                .steam_id_override
                .filter(|&sid| sid >= STEAM_ID64_INDIVIDUAL_BASE)
        });
        sls.config.denuvo_games.remove(&0);
        if !lua.force_denuvo.is_empty() {
            if let Some(real_sid) = active_steam_id.filter(|&sid| sid > 0) {
                let mut games = sls.config.denuvo_games.remove(&real_sid).unwrap_or_default();
                for &did in &lua.force_denuvo {
                    if !games.contains(&did) {
                        games.push(did);
                    }
                }
                games.sort_unstable();
                games.dedup();
                sls.set_denuvo_games(real_sid, games);
            }
        } else {
            sls.config.denuvo_games.retain(|_, games| {
                games.retain(|&id| id != app_id);
                !games.is_empty()
            });
        }

        let _ = sls.save();

        // Persist bridge-only directives (`addprocess`, `forcedenuvo`, `pinApp`, `setStat`)
        let directives_path =
            Self::resolve_bridge_directives_path(Some(&sls.config_path), steam_root, config_dir);
        let _ = Self::sync_bridge_directives_to(&directives_path, app_id, lua, active_steam_id);

        // Persist AppTicket, ETicket, LegacyCDKey, and EticketUrl overrides to user's Steam config dir
        Self::sync_credentials_with_root(steam_root, config_dir, lua);

        // LT-33 / LNX-02: Resolve `<steam_root>/config/config.vdf` dynamically and guard against
        // writing `config.vdf` while Steam is actively running.
        let global_cfg_vdf_candidate = steam_root
            .map(|r| r.join("config").join("config.vdf"))
            .filter(|p| p.is_file())
            .or_else(|| {
                let direct = config_dir.join("config.vdf");
                direct.is_file().then_some(direct)
            });
        let dynamic_config_vdf = if let Some(found) = global_cfg_vdf_candidate {
            Some(found)
        } else if !cfg!(test) {
            crate::services::steam_service::detect_steam_install(None)
                .map(|inst| inst.config_path.join("config.vdf"))
                .filter(|p| p.is_file())
        } else {
            None
        };

        if let Some(vdf_path) = dynamic_config_vdf {
            if !cfg!(test) && crate::services::steam_service::SteamService::is_steam_running() {
                log::warn!(
                    "Steam is running — skipping config.vdf depot key write at {} to avoid Steam overwrite/corruption (keys synced to SLSsteam config.yaml).",
                    vdf_path.display()
                );
            } else {
                let _ = Self::write_depot_keys_to_config_vdf(&vdf_path, &lua.entries);
            }
        }
    }

    fn write_depot_keys_to_config_vdf(path: &Path, entries: &[crate::parsers::lua_parser::LuaEntry]) -> bool {
        let Ok(content) = fs::read_to_string(path) else {
            return false;
        };
        let mut keys_to_write = Vec::new();
        for entry in entries {
            if let Some(ref k) = entry.key {
                if !content.contains(&format!("\"{}\"", entry.id)) {
                    keys_to_write.push((entry.id, k.clone()));
                }
            }
        }
        if keys_to_write.is_empty() {
            return true;
        }

        // Check if "depots" section exists
        if let Some(pos) = content.find("\"depots\"") {
            if let Some(brace_pos) = content[pos..].find('{') {
                let insert_idx = pos + brace_pos + 1;
                let mut snippet = String::from("\n");
                for (did, key) in keys_to_write {
                    snippet.push_str(&format!("\t\t\t\t\"{}\"\n\t\t\t\t{{\n\t\t\t\t\t\"DecryptionKey\"\t\t\"{}\"\n\t\t\t\t}}\n", did, key));
                }
                let mut new_content = String::with_capacity(content.len() + snippet.len());
                new_content.push_str(&content[..insert_idx]);
                new_content.push_str(&snippet);
                new_content.push_str(&content[insert_idx..]);
                let tmp = path.with_extension("vdf.tmp");
                if fs::write(&tmp, &new_content).is_ok() {
                    return fs::rename(&tmp, path).is_ok();
                }
            }
        }
        false
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

        // Test bare uncompressed .lua file passed to install_zip and install_any with .zip extension
        let bare_zip_path = dir.path().join("bare_lua_masquerading.zip");
        fs::write(
            &bare_zip_path,
            b"addappid(730)\naddappid(731, 1, \"11223344556677889900aabbccddeeff11223344556677889900aabbccddeeff\")\n",
        )
        .unwrap();

        let bare_res = installer.install_zip(
            &bare_zip_path,
            Some(730),
            false,
            Some("bare-test"),
            Some(&mut settings),
            Some(&mut sls),
        );
        assert!(bare_res.ok && bare_res.lua_installed);
        assert_eq!(bare_res.app_id, Some(730));

        let any_res = installer.install_any(
            &bare_zip_path,
            Some(730),
            false,
            Some("bare-any-test"),
            Some(&mut settings),
            Some(&mut sls),
        );
        assert!(any_res.ok && any_res.lua_installed);
    }

    #[test]
    fn install_lua_persists_tickets_and_legacy_cdkeys_to_steam_config() {
        use crate::steam_bridge::credentials::appticket::{get_e_ticket_from, read_app_ticket_from};

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
        let installer = LuaInstaller::new(inst.clone(), vault);
        let mut settings = SettingsService::new(Some(dir.path().join("settings.json")));
        let mut sls = SlssteamService::new(Some(dir.path().join("sls.yaml")));

        let lua_src = "\
            addappid(1245620)\n\
            setAppTicket(1245620, \"deadbeef01\")\n\
            setETicket(1245620, \"cafebabe02\")\n\
            setETicket(730, \"feedface03\")\n\
            setlegacycdkey(1245620, \"AAAA-BBBB-CCCC-DDDD\")\n";

        let res = installer.install_lua(
            1245620,
            lua_src,
            false,
            None,
            Some("cred-test"),
            Some(&mut settings),
            Some(&mut sls),
        );
        assert!(res.ok);

        let ticket = read_app_ticket_from(&inst.config_path, 1245620)
            .unwrap()
            .expect("AppTicket persisted");
        assert_eq!(ticket.app_ticket.as_deref(), Some("deadbeef01"));
        assert_eq!(ticket.e_ticket.as_deref(), Some("cafebabe02"));
        assert_eq!(
            get_e_ticket_from(&inst.config_path, 730).unwrap().as_deref(),
            Some("feedface03")
        );

        let mut cdkeys = LegacyCdKeyStore::new(&inst.config_path);
        let cd = cdkeys.get(1245620).expect("LegacyCDKey persisted");
        assert_eq!(cd.override_key.as_deref(), Some("AAAA-BBBB-CCCC-DDDD"));
    }

    #[test]
    fn install_lua_resolves_userdata_tickets_real_steamid_bridge_directives_and_uninstall_cleanup() {
        use crate::steam_bridge::credentials::appticket::read_app_ticket_from;

        let dir = tempfile::tempdir().unwrap();
        let steam_root = dir.path().join("Steam");
        let stplug = steam_root.join("config/stplug-in");
        let account_id: u32 = 67_855_625;
        let expected_sid64: u64 = STEAM_ID64_INDIVIDUAL_BASE + u64::from(account_id); // 76561198028121353
        let userdata_cfg = steam_root
            .join("userdata")
            .join(account_id.to_string())
            .join("config");
        fs::create_dir_all(&stplug).unwrap();
        fs::create_dir_all(&userdata_cfg).unwrap();

        // Write `loginusers.vdf` under `<steam_root>/config` and `localconfig.vdf` under `userdata/<id>/config`
        let loginusers = format!(
            "\"users\"\n{{\n    \"{expected_sid64}\"\n    {{\n        \"AccountName\" \"active_player\"\n        \"MostRecent\" \"1\"\n        \"Timestamp\" \"1700000100\"\n    }}\n}}\n"
        );
        fs::write(steam_root.join("config/loginusers.vdf"), loginusers).unwrap();
        fs::write(
            userdata_cfg.join("localconfig.vdf"),
            "\"UserLocalConfigStore\"\n{\n    \"Software\" { \"Valve\" { \"Steam\" { \"apps\" { } } } }\n}\n",
        )
        .unwrap();

        let inst = SteamInstallation {
            root: steam_root.clone(),
            libraries: vec![SteamLibrary::new(steam_root.clone())],
            config_path: steam_root.join("config"),
            stplug_in_path: stplug,
        };
        let vault = LuaVault::new(Some(inst.clone()), Some(dir.path().join("vault")));
        let installer = LuaInstaller::new(inst.clone(), vault);
        let mut settings = SettingsService::new(Some(dir.path().join("settings.json")));
        let sls_yaml_path = dir.path().join("sls.yaml");
        let mut sls = SlssteamService::new(Some(sls_yaml_path.clone()));

        let lua_src = "\
            addappid(1245620)\n\
            setAppTicket(1245620, \"deadbeef01\")\n\
            setETicket(1245620, \"cafebabe02\")\n\
            seteticketurl(\"https://eticket.example.test/api\")\n\
            addprocess(1245620, \"eldenring.exe\")\n\
            forcedenuvo(1245620)\n\
            pinApp(1245620)\n\
            setStat(1245620, \"76561198000000999\")\n\
            setStat(1245620, \"kills\", 42)\n";

        let eticket_ep_file = dir.path().join("eticket_endpoint.json");
        EticketClient::with_endpoint_file(&eticket_ep_file, || {
            let res = installer.install_lua(
                1245620,
                lua_src,
                false,
                None,
                Some("bst-directive-test"),
                Some(&mut settings),
                Some(&mut sls),
            );
            assert!(res.ok && res.lua_installed);

            // 1. BST-05 & BST-06: Tickets written to `<steam_root>/userdata/<account_id>/config`
            // (NOT `<steam_root>/config`) AND synced into `steam_credential_store`!
            assert!(
                !inst.config_path.join("localconfig.vdf").exists(),
                "must not create <steam_root>/config/localconfig.vdf when userdata exists"
            );
            let ud_ticket = read_app_ticket_from(&userdata_cfg, 1245620)
                .unwrap()
                .expect("AppTicket persisted in userdata/<account_id>/config");
            assert_eq!(ud_ticket.app_ticket.as_deref(), Some("deadbeef01"));
            assert_eq!(ud_ticket.e_ticket.as_deref(), Some("cafebabe02"));
            assert_eq!(
                ud_ticket.steam_id.as_deref(),
                Some(expected_sid64.to_string().as_str())
            );

            let mut cred_app = Vec::new();
            assert_eq!(
                steam_credential_store::get_app_ticket(1245620, &mut cred_app),
                steam_credential_store::Status::Ok
            );
            assert_eq!(cred_app, vec![0xde, 0xad, 0xbe, 0xef, 0x01]);
            let mut cred_et = Vec::new();
            assert_eq!(
                steam_credential_store::get_e_ticket(1245620, &mut cred_et),
                steam_credential_store::Status::Ok
            );
            assert_eq!(cred_et, vec![0xca, 0xfe, 0xba, 0xbe, 0x02]);

            // 2. BST-07: `seteticketurl` wired into `EticketClient`
            assert_eq!(
                EticketClient::get_custom_url().as_deref(),
                Some("https://eticket.example.test/api")
            );

            // 3. BST-15: `forcedenuvo` resolved real 64-bit SteamID (never `0`) in `DenuvoGames`
            assert!(!sls.config.denuvo_games.contains_key(&0));
            assert_eq!(
                sls.config.denuvo_games.get(&expected_sid64),
                Some(&vec![1245620])
            );

            // 4. BST-08, BST-14, BST-15, BST-16: `SLSsteam` YAML has ZERO unknown `ProcessMap`/`ForceDenuvo`/`PinnedApps`
            // keys, and `bridge_directives.json` holds all 4 bridge directives keyed by `app_id`!
            let raw_yaml = fs::read_to_string(&sls_yaml_path).unwrap();
            assert!(!raw_yaml.contains("ProcessMap:"));
            assert!(!raw_yaml.contains("ForceDenuvo:"));
            assert!(!raw_yaml.contains("PinnedApps:"));

            let directives_path = installer.bridge_directives_path();
            let bridge_state = LuaInstaller::load_bridge_directives_from(&directives_path);
            let app_dir = bridge_state
                .apps
                .get(&1245620)
                .expect("bridge directives saved for 1245620");
            assert_eq!(
                app_dir.process_map.get(&1245620).map(String::as_str),
                Some("eldenring.exe")
            );
            assert_eq!(app_dir.force_denuvo, vec![1245620]);
            assert_eq!(app_dir.denuvo_steam_id, Some(expected_sid64));
            assert_eq!(app_dir.pinned_apps, vec![1245620]);
            assert_eq!(
                app_dir
                    .stat_overrides
                    .get(&1245620)
                    .and_then(|m| m.get("kills"))
                    .map(String::as_str),
                Some("42")
            );

            // 5. Uninstall cleans `bridge_directives.json`, `DenuvoGames`, and `EticketClient` URL!
            let un = installer.uninstall_lua(1245620, true, Some(&mut sls));
            assert!(un.ok);
            let after_state = LuaInstaller::load_bridge_directives_from(&directives_path);
            assert!(!after_state.apps.contains_key(&1245620));
            assert!(sls.config.denuvo_games.is_empty());
            assert!(EticketClient::get_custom_url().is_none());
        });
    }
}
