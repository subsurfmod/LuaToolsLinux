//! PluginAddService — Headless Store-Page Add state machine & plugin manager (LT-24).
//!
//! Orchestrates real source probing (`check_apis` + `HubcapService::check_status`),
//! real manifest package downloading (using `HubcapService` API key from `SettingsService`
//! or `LuaToolsApiClient` Discord auth), real installation via `LuaInstaller::install_any`
//! (syncing `LuaVault` + `SLSsteam`), per-AppID progress tracking, and cooperative
//! `AtomicBool` cancellation.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::lua_vault::LuaVault;
use crate::models::steam::{SteamInstallation, SteamLibrary};
use crate::services::api_client::{
    HubcapService, LuaToolsApiClient, MANIFEST_BACKEND_UA, MANIFEST_BACKEND_URL,
};
use crate::services::auth_service::AuthService;
use crate::services::lua_installer::{InstallResult, LuaInstaller};
use crate::services::plugin_log::PluginLog;
use crate::services::settings_service::SettingsService;
use crate::services::slssteam_service::SlssteamService;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRow {
    pub name: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub status: String,
    #[serde(rename = "needsKey")]
    pub needs_key: bool,
    pub locked: bool,
    pub stats: Option<String>,
    pub downloading: bool,
    pub progress: f64,
    pub indeterminate: bool,
    pub available: bool,
    #[serde(rename = "canDownload")]
    pub can_download: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddState {
    pub appid: u64,
    #[serde(rename = "gameName")]
    pub game_name: Option<String>,
    pub checking: bool,
    #[serde(rename = "fastFetch")]
    pub fast_fetch: bool,
    #[serde(rename = "sourcesLoaded")]
    pub sources_loaded: bool,
    pub sources: Vec<SourceRow>,
    #[serde(rename = "installStatus")]
    pub install_status: Option<String>,
    #[serde(rename = "installFailed")]
    pub install_failed: bool,
    pub error: Option<String>,
    pub installed: bool,
    pub status: String,
    #[serde(rename = "bytesRead")]
    pub bytes_read: u64,
    #[serde(rename = "totalBytes")]
    pub total_bytes: u64,
    pub progress: f64,
    #[serde(rename = "currentApi")]
    pub current_api: Option<String>,
    #[serde(rename = "installedPath")]
    pub installed_path: Option<String>,
    #[serde(rename = "apiErrors")]
    pub api_errors: HashMap<String, String>,
    pub cancelled: bool,
}

impl AddState {
    pub fn idle(app_id: u32, is_installed: bool) -> Self {
        Self {
            appid: app_id as u64,
            game_name: None,
            checking: false,
            fast_fetch: true,
            sources_loaded: false,
            sources: Vec::new(),
            install_status: if is_installed {
                Some("Installed".into())
            } else {
                None
            },
            install_failed: false,
            error: None,
            installed: is_installed,
            status: if is_installed {
                "done".into()
            } else {
                "idle".into()
            },
            bytes_read: 0,
            total_bytes: 0,
            progress: if is_installed { 1.0 } else { 0.0 },
            current_api: None,
            installed_path: None,
            api_errors: HashMap::new(),
            cancelled: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub download_url: String,
    pub checksum: Option<String>,
    pub dependencies: Vec<String>,
    pub min_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginInstallResult {
    pub success: bool,
    pub plugin_name: String,
    pub installed_path: Option<String>,
    pub error_message: Option<String>,
}

static GLOBAL_PLUGIN_ADD: OnceLock<PluginAddService> = OnceLock::new();

#[derive(Clone)]
pub struct PluginAddService {
    plugin_dir: PathBuf,
    states: Arc<Mutex<HashMap<u32, AddState>>>,
    cancel_flags: Arc<Mutex<HashMap<u32, Arc<AtomicBool>>>>,
}

impl PluginAddService {
    /// Returns the shared process-wide `PluginAddService` singleton used by `http_server.rs`.
    pub fn global() -> &'static PluginAddService {
        GLOBAL_PLUGIN_ADD.get_or_init(|| Self::new(None))
    }

    pub fn new(plugin_dir: Option<&Path>) -> Self {
        let dir = plugin_dir.map(|p| p.to_path_buf()).unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/share/luatools/plugins")
        });
        let _ = fs::create_dir_all(&dir);
        Self {
            plugin_dir: dir,
            states: Arc::new(Mutex::new(HashMap::new())),
            cancel_flags: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn plugin_dir(&self) -> &Path {
        &self.plugin_dir
    }

    /// Returns the current per-AppID headless add/download state.
    pub fn get_add_state(&self, app_id: u32, is_installed: bool) -> AddState {
        let mut map = self.states.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(st) = map.get_mut(&app_id) {
            if is_installed && st.status != "downloading" && st.status != "checking" {
                st.installed = true;
            }
            return st.clone();
        }
        AddState::idle(app_id, is_installed)
    }

    /// Checks whether cancellation has been requested for `app_id`.
    pub fn is_cancelled(&self, app_id: u32) -> bool {
        self.cancel_flags
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&app_id)
            .map(|f| f.load(Ordering::SeqCst))
            .unwrap_or(false)
    }

    /// Cooperatively cancels any active headless add/download for `app_id` via its `AtomicBool`.
    pub fn cancel_add(&self, app_id: u32) -> bool {
        let mut had_active = false;
        {
            let flags = self.cancel_flags.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(flag) = flags.get(&app_id) {
                flag.store(true, Ordering::SeqCst);
                had_active = true;
            }
        }
        {
            let mut map = self.states.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(st) = map.get_mut(&app_id) {
                if matches!(
                    st.status.as_str(),
                    "checking" | "downloading" | "installing"
                ) {
                    had_active = true;
                }
                st.checking = false;
                st.cancelled = true;
                st.status = "cancelled".into();
                st.install_status = Some("Cancelled".into());
                for row in &mut st.sources {
                    row.downloading = false;
                    row.indeterminate = false;
                }
            } else if app_id > 0 {
                let mut cancelled_state = AddState::idle(app_id, false);
                cancelled_state.cancelled = true;
                cancelled_state.status = "cancelled".into();
                cancelled_state.install_status = Some("Cancelled".into());
                map.insert(app_id, cancelled_state);
            }
        }
        PluginLog::global().info(
            Some("PluginAdd"),
            &format!("cancel_add appid={} active={}", app_id, had_active),
        );
        had_active
    }

    /// Validates whether a given `backendUrl` is a trusted official backend address
    /// or a local loopback test server, preventing arbitrary external SSRF.
    pub fn is_trusted_backend_url(raw: &str) -> bool {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return false;
        }
        let url_to_parse = if !trimmed.contains("://") {
            format!("https://{}", trimmed)
        } else {
            trimmed.to_string()
        };
        let Ok(parsed) = url::Url::parse(&url_to_parse) else {
            return false;
        };
        if !matches!(parsed.scheme(), "http" | "https") {
            return false;
        }
        let Some(host) = parsed.host_str() else {
            return false;
        };
        let host_lower = host.to_ascii_lowercase();

        // 1. Trusted official backend IPs and domains
        if host_lower == "167.235.229.108"
            || host_lower == "lua.tools"
            || host_lower.ends_with(".lua.tools")
        {
            return true;
        }

        // 2. Loopback / localhost for local test suites and internal services
        let is_loopback_host = host_lower == "localhost"
            || host_lower == "127.0.0.1"
            || host_lower == "::1"
            || host_lower == "[::1]"
            || host_lower
                .parse::<std::net::IpAddr>()
                .map(|ip| ip.is_loopback())
                .unwrap_or(false);

        if is_loopback_host {
            // Restrict loopback ports: disallow privileged ports (<1024 except 80/443)
            // and dangerous internal ports (e.g. database/cache/daemon ports)
            if let Some(port) = parsed.port() {
                let is_disallowed = matches!(
                    port,
                    0..=79 | 81..=442 | 444..=1023 | 3306 | 5432 | 6379 | 11211
                );
                if is_disallowed {
                    return false;
                }
            }
            return true;
        }

        false
    }

    /// Probes manifest source availability for `app_id` using the live `check_apis` backend
    /// and `HubcapService::check_status` (when a valid Hubcap key is configured in settings).
    pub fn check_sources(&self, app_id: u32) -> Vec<SourceRow> {
        self.check_sources_with_urls(app_id, MANIFEST_BACKEND_URL, None)
    }

    /// Testable source-probing entrypoint allowing a custom `check_apis` base URL
    /// and optional Hubcap API key override.
    pub fn check_sources_with_urls(
        &self,
        app_id: u32,
        check_apis_base_url: &str,
        hubcap_key_override: Option<&str>,
    ) -> Vec<SourceRow> {
        if app_id == 0 {
            return Vec::new();
        }
        if !Self::is_trusted_backend_url(check_apis_base_url) {
            PluginLog::global().warn(
                Some("PluginAdd"),
                &format!(
                    "Blocked untrusted check_apis_base_url (SSRF prevention): {}",
                    check_apis_base_url
                ),
            );
            return Vec::new();
        }
        let settings = SettingsService::default().settings;
        let hubcap_key = hubcap_key_override
            .map(ToString::to_string)
            .unwrap_or(settings.hubcap_api_key);
        let has_valid_hubcap = HubcapService::is_valid_key_format(&hubcap_key);

        let auth = AuthService::default();
        let has_discord_auth = auth.initialize();

        let mut source_map: Vec<(String, String)> = Vec::new();

        // 1. Query check_apis backend with fast connect/read timeout so HTTP routes stay responsive
        let fast_agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(3))
            .timeout(Duration::from_secs(6))
            .build();

        let mut probe_candidates = Vec::new();
        let trimmed_base = check_apis_base_url.trim_end_matches('/');
        if trimmed_base == MANIFEST_BACKEND_URL || trimmed_base == "https://167.235.229.108" {
            // 167.235.229.108 serves /check_apis on HTTP port 80 (port 443 has a self-signed cert & 404)
            probe_candidates.push(format!("http://167.235.229.108/check_apis?appid={}", app_id));
        } else {
            probe_candidates.push(format!("{}/check_apis?appid={}", trimmed_base, app_id));
        }

        for probe_url in probe_candidates {
            match fast_agent
                .get(&probe_url)
                .set("User-Agent", MANIFEST_BACKEND_UA)
                .call()
            {
                Ok(resp) => match resp.into_json::<Value>() {
                    Ok(Value::Object(obj)) => {
                        let mut pairs: Vec<(String, String)> = obj
                            .into_iter()
                            .map(|(k, v)| {
                                let st = v
                                    .as_str()
                                    .map(ToString::to_string)
                                    .unwrap_or_else(|| v.to_string());
                                (k, st)
                            })
                            .collect();
                        pairs.sort_by(|a, b| a.0.cmp(&b.0));
                        source_map.extend(pairs);
                        break;
                    }
                    Ok(_) => {
                        PluginLog::global().warn(
                            Some("PluginAdd"),
                            &format!("check_apis {} returned non-object JSON", probe_url),
                        );
                    }
                    Err(e) => {
                        PluginLog::global().warn(
                            Some("PluginAdd"),
                            &format!("check_apis {} JSON parse error: {}", probe_url, e),
                        );
                    }
                },
                Err(e) => {
                    PluginLog::global().warn(
                        Some("PluginAdd"),
                        &format!("check_apis {} request failed: {}", probe_url, e),
                    );
                }
            }
        }

        // 2. Check Hubcap directly if user configured a valid Hubcap API key
        if has_valid_hubcap && check_apis_base_url == MANIFEST_BACKEND_URL {
            if let Some(has_hc) = HubcapService::new().check_status(&hubcap_key, app_id) {
                source_map.retain(|(k, _)| !k.eq_ignore_ascii_case("hubcap"));
                source_map.insert(
                    0,
                    (
                        "hubcap".into(),
                        if has_hc {
                            "available".into()
                        } else {
                            "unavailable".into()
                        },
                    ),
                );
            }
        }

        let rows: Vec<SourceRow> = source_map
            .into_iter()
            .map(|(raw_name, raw_status)| {
                let lower_status = raw_status.trim().to_ascii_lowercase();
                let available = matches!(
                    lower_status.as_str(),
                    "available" | "ok" | "true" | "online" | "ready"
                );
                let is_hubcap = raw_name.eq_ignore_ascii_case("hubcap");
                let needs_key = is_hubcap;
                let locked = if is_hubcap {
                    !has_valid_hubcap
                } else {
                    !has_discord_auth
                };
                SourceRow {
                    display_name: format_source_display_name(&raw_name),
                    name: raw_name,
                    status: raw_status,
                    needs_key,
                    locked,
                    stats: Some(if is_hubcap {
                        "Hubcap API".into()
                    } else {
                        "LuaTools API".into()
                    }),
                    downloading: false,
                    progress: 0.0,
                    indeterminate: false,
                    available,
                    can_download: available && !locked,
                }
            })
            .collect();

        // Update tracked state with discovered sources
        {
            let mut map = self.states.lock().unwrap_or_else(|e| e.into_inner());
            let entry = map
                .entry(app_id)
                .or_insert_with(|| AddState::idle(app_id, false));
            entry.sources = rows.clone();
            entry.sources_loaded = true;
            entry.fast_fetch = settings.fast_fetch;
            if entry.status == "checking" {
                entry.checking = false;
            }
        }

        PluginLog::global().info(
            Some("PluginAdd"),
            &format!(
                "check_sources appid={} found {} source(s) ({} available)",
                app_id,
                rows.len(),
                rows.iter().filter(|r| r.available).count()
            ),
        );

        rows
    }

    /// Starts the real headless add pipeline on a background thread with cooperative
    /// `AtomicBool` cancellation, real source probing, real manifest archive download,
    /// and real `LuaInstaller::install_any` + `SLSsteam` sync.
    pub fn start_headless_add(
        &self,
        app_id: u32,
        game_name: Option<String>,
        stplug_dir: &Path,
        preferred_source: Option<String>,
    ) {
        if app_id == 0 {
            return;
        }
        let stplug_dir = stplug_dir.to_path_buf();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        {
            let mut flags = self.cancel_flags.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(prev) = flags.insert(app_id, Arc::clone(&cancel_flag)) {
                prev.store(true, Ordering::SeqCst);
            }
        }

        let settings_snapshot = SettingsService::default().settings;
        {
            let mut map = self.states.lock().unwrap_or_else(|e| e.into_inner());
            let mut init = AddState::idle(app_id, false);
            init.game_name = game_name.clone();
            init.checking = true;
            init.fast_fetch = settings_snapshot.fast_fetch;
            init.status = "checking".into();
            init.install_status = Some("Checking manifest sources...".into());
            map.insert(app_id, init);
        }

        PluginLog::global().info(
            Some("PluginAdd"),
            &format!(
                "start_headless_add appid={} preferred_source={:?}",
                app_id, preferred_source
            ),
        );

        let svc = self.clone();
        thread::spawn(move || {
            svc.run_headless_add_worker(
                app_id,
                game_name,
                stplug_dir,
                preferred_source,
                cancel_flag,
            );
        });
    }

    fn run_headless_add_worker(
        &self,
        app_id: u32,
        game_name: Option<String>,
        stplug_dir: PathBuf,
        preferred_source: Option<String>,
        cancel_flag: Arc<AtomicBool>,
    ) {
        if cancel_flag.load(Ordering::SeqCst) {
            self.mark_cancelled(app_id);
            return;
        }

        let mut settings_svc = SettingsService::default();
        let hubcap_key = settings_svc.settings.hubcap_api_key.clone();
        let has_valid_hubcap = HubcapService::is_valid_key_format(&hubcap_key);

        let auth = AuthService::default();
        let has_discord_auth = auth.initialize();

        // Step 1: Probe real sources via check_apis + HubcapService
        let rows = self.check_sources(app_id);
        if cancel_flag.load(Ordering::SeqCst) {
            self.mark_cancelled(app_id);
            return;
        }

        // Step 2: Build ordered candidate download source list
        let mut candidates: Vec<String> = Vec::new();
        if let Some(ref pref) = preferred_source {
            let norm = normalize_source_name(pref);
            if !norm.is_empty() {
                candidates.push(norm);
            }
        } else {
            if has_valid_hubcap
                && rows
                    .iter()
                    .any(|r| r.name.eq_ignore_ascii_case("hubcap") && r.available)
            {
                candidates.push("hubcap".into());
            }
            if has_discord_auth {
                for r in &rows {
                    if r.available && !r.name.eq_ignore_ascii_case("hubcap") {
                        candidates.push(r.name.clone());
                    }
                }
            }
            if candidates.is_empty() && has_valid_hubcap {
                candidates.push("hubcap".into());
            }
        }

        if candidates.is_empty() {
            let err_msg = if !has_valid_hubcap && !has_discord_auth {
                "No manifest source credentials configured (add a Hubcap API key or sign in with Discord in Settings)".to_string()
            } else {
                format!("No available manifest sources found for AppID {}", app_id)
            };
            self.mark_failed(app_id, &err_msg);
            return;
        }

        // Step 3: Download manifest package (.zip / .lua) from first working source
        let tmp_dir = std::env::temp_dir().join(format!(
            "luatools_plugin_add_{}_{}",
            app_id,
            std::process::id()
        ));
        let _ = fs::create_dir_all(&tmp_dir);

        let hc = HubcapService::new();
        let api = LuaToolsApiClient::new(auth);
        let mut downloaded: Option<(String, PathBuf)> = None;
        let mut last_error = String::new();

        for src in candidates {
            if cancel_flag.load(Ordering::SeqCst) {
                let _ = fs::remove_dir_all(&tmp_dir);
                self.mark_cancelled(app_id);
                return;
            }

            {
                let mut map = self.states.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(st) = map.get_mut(&app_id) {
                    st.checking = false;
                    st.status = "downloading".into();
                    st.current_api = Some(src.clone());
                    st.install_status = Some(format!("Downloading from {}...", src));
                    for row in &mut st.sources {
                        row.downloading = row.name.eq_ignore_ascii_case(&src);
                    }
                }
            }

            let states_ref = Arc::clone(&self.states);
            let src_label = src.clone();
            let progress_cb = move |pct: f32, msg: String| {
                let (bytes_read, total_bytes) = parse_kb_progress(&msg);
                let mut map = states_ref.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(st) = map.get_mut(&app_id) {
                    st.progress = pct as f64;
                    if bytes_read > 0 {
                        st.bytes_read = bytes_read;
                    }
                    if total_bytes > 0 {
                        st.total_bytes = total_bytes;
                    }
                    st.install_status = Some(msg);
                    for row in &mut st.sources {
                        if row.name.eq_ignore_ascii_case(&src_label) {
                            row.progress = pct as f64;
                            row.indeterminate = pct <= 0.0;
                        }
                    }
                }
            };

            let dl_res = if src.eq_ignore_ascii_case("hubcap") {
                if !has_valid_hubcap {
                    Err("Hubcap API key not configured or invalid".into())
                } else {
                    hc.download_manifest(app_id, &hubcap_key, &tmp_dir, progress_cb)
                }
            } else {
                api.download_manifest(
                    app_id,
                    &src,
                    game_name.as_deref(),
                    &tmp_dir,
                    progress_cb,
                )
            };

            match dl_res {
                Ok(path) => {
                    downloaded = Some((src, path));
                    break;
                }
                Err(e) => {
                    PluginLog::global().warn(
                        Some("PluginAdd"),
                        &format!("Download failed from '{}' for appid={}: {}", src, app_id, e),
                    );
                    last_error = e.clone();
                    let mut map = self.states.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(st) = map.get_mut(&app_id) {
                        st.api_errors.insert(src.clone(), e);
                        for row in &mut st.sources {
                            if row.name.eq_ignore_ascii_case(&src) {
                                row.downloading = false;
                            }
                        }
                    }
                    if preferred_source.is_some() {
                        break;
                    }
                }
            }
        }

        if cancel_flag.load(Ordering::SeqCst) {
            let _ = fs::remove_dir_all(&tmp_dir);
            self.mark_cancelled(app_id);
            return;
        }

        let Some((chosen_src, downloaded_path)) = downloaded else {
            let _ = fs::remove_dir_all(&tmp_dir);
            let msg = if last_error.is_empty() {
                format!("Failed to download manifest package for AppID {}", app_id)
            } else {
                last_error
            };
            self.mark_failed(app_id, &msg);
            return;
        };

        // Step 4: Install downloaded archive/lua via real LuaInstaller + SLSsteam sync
        {
            let mut map = self.states.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(st) = map.get_mut(&app_id) {
                st.status = "installing".into();
                st.install_status = Some("Installing manifest & syncing SLSsteam...".into());
            }
        }

        let mut sls_svc = SlssteamService::new(None);
        let install_res = self.install_downloaded_package(
            app_id,
            &downloaded_path,
            &stplug_dir,
            &chosen_src,
            Some(&mut settings_svc),
            Some(&mut sls_svc),
        );

        let _ = fs::remove_dir_all(&tmp_dir);

        if install_res.ok {
            PluginLog::global().info(
                Some("PluginAdd"),
                &format!(
                    "Headless add completed for appid={} via '{}' ({} manifest(s))",
                    app_id, chosen_src, install_res.manifest_count
                ),
            );
        } else {
            PluginLog::global().error(
                Some("PluginAdd"),
                &format!(
                    "Headless install failed for appid={}: {:?}",
                    app_id, install_res.error
                ),
            );
        }
    }

    /// Installs a downloaded `.zip` / `.lua` / `.manifest` package into `stplug_dir`
    /// using `LuaInstaller::install_any` and updates per-AppID `AddState`.
    pub fn install_downloaded_package(
        &self,
        app_id: u32,
        archive_path: &Path,
        stplug_dir: &Path,
        source: &str,
        settings_svc: Option<&mut SettingsService>,
        sls_svc: Option<&mut SlssteamService>,
    ) -> InstallResult {
        let config_path = stplug_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| stplug_dir.to_path_buf());
        let steam_root = config_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| config_path.clone());

        let inst = SteamInstallation {
            root: steam_root.clone(),
            libraries: vec![SteamLibrary::new(steam_root)],
            config_path,
            stplug_in_path: stplug_dir.to_path_buf(),
        };
        let vault = LuaVault::new(Some(inst.clone()), None);
        let installer = LuaInstaller::new(inst, vault);

        let res = installer.install_any(
            archive_path,
            Some(app_id),
            false,
            Some(source),
            settings_svc,
            sls_svc,
        );

        let mut map = self.states.lock().unwrap_or_else(|e| e.into_inner());
        let st = map
            .entry(app_id)
            .or_insert_with(|| AddState::idle(app_id, false));
        st.checking = false;
        st.current_api = Some(source.to_string());
        for row in &mut st.sources {
            row.downloading = false;
            row.indeterminate = false;
            if row.name.eq_ignore_ascii_case(source) && res.ok {
                row.progress = 1.0;
            }
        }
        if res.ok {
            st.status = "done".into();
            st.installed = true;
            st.install_failed = false;
            st.error = None;
            st.progress = 1.0;
            st.installed_path = res
                .lua_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string());
            st.install_status = Some("✓ Added via LuaTools".into());
        } else {
            let err = res
                .error
                .clone()
                .unwrap_or_else(|| "Failed to install manifest package".into());
            st.status = "failed".into();
            st.install_failed = true;
            st.error = Some(err.clone());
            st.install_status = Some(format!("Install failed: {}", err));
        }

        res
    }

    fn mark_cancelled(&self, app_id: u32) {
        let mut map = self.states.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(st) = map.get_mut(&app_id) {
            st.checking = false;
            st.cancelled = true;
            st.status = "cancelled".into();
            st.install_status = Some("Cancelled".into());
            for row in &mut st.sources {
                row.downloading = false;
                row.indeterminate = false;
            }
        }
    }

    fn mark_failed(&self, app_id: u32, reason: &str) {
        PluginLog::global().warn(
            Some("PluginAdd"),
            &format!("Headless add failed appid={}: {}", app_id, reason),
        );
        let mut map = self.states.lock().unwrap_or_else(|e| e.into_inner());
        let st = map
            .entry(app_id)
            .or_insert_with(|| AddState::idle(app_id, false));
        st.checking = false;
        st.status = "failed".into();
        st.install_failed = true;
        st.error = Some(reason.to_string());
        st.install_status = Some(format!("Failed: {}", reason));
        for row in &mut st.sources {
            row.downloading = false;
            row.indeterminate = false;
        }
    }

    pub fn validate_plugin(&self, info: &PluginInfo) -> Result<(), String> {
        let name = info.name.trim();
        if !is_safe_plugin_name(name) {
            return Err(
                "Plugin name is required and must not contain path separators or traversal segments"
                    .into(),
            );
        }
        if info.version.trim().is_empty() {
            return Err("Plugin version is required".into());
        }
        let url = info.download_url.trim();
        if url.is_empty() {
            return Err("Plugin download URL is required".into());
        }
        if !url.starts_with("http://")
            && !url.starts_with("https://")
            && !url.starts_with("file://")
        {
            return Err("Plugin download URL must start with http://, https://, or file://".into());
        }
        if let Some(ref csum) = info.checksum {
            let clean = csum
                .trim()
                .strip_prefix("sha256:")
                .or_else(|| csum.trim().strip_prefix("SHA256:"))
                .unwrap_or(csum.trim());
            if clean.len() != 64 || !clean.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(
                    "Plugin checksum must be a valid 64-character hex SHA-256 digest".into(),
                );
            }
        }
        Ok(())
    }

    pub fn add_plugin(&self, info: &PluginInfo) -> PluginInstallResult {
        if let Err(e) = self.validate_plugin(info) {
            return PluginInstallResult {
                success: false,
                plugin_name: info.name.clone(),
                installed_path: None,
                error_message: Some(e),
            };
        }

        let clean_name = info.name.trim();
        let plugin_dir = self.plugin_dir.join(clean_name);
        if fs::create_dir_all(&plugin_dir).is_err() {
            return PluginInstallResult {
                success: false,
                plugin_name: clean_name.to_string(),
                installed_path: None,
                error_message: Some("Failed to create plugin directory".into()),
            };
        }

        let mut normalized = info.clone();
        normalized.name = clean_name.to_string();
        let meta_path = plugin_dir.join("plugin.json");
        if let Ok(js) = serde_json::to_string_pretty(&normalized) {
            if let Err(e) = fs::write(&meta_path, js) {
                let _ = fs::remove_dir_all(&plugin_dir);
                return PluginInstallResult {
                    success: false,
                    plugin_name: clean_name.to_string(),
                    installed_path: None,
                    error_message: Some(format!("Failed to write plugin metadata: {}", e)),
                };
            }
        }

        PluginInstallResult {
            success: true,
            plugin_name: clean_name.to_string(),
            installed_path: Some(plugin_dir.to_string_lossy().to_string()),
            error_message: None,
        }
    }

    pub fn list_plugins(&self) -> Vec<PluginInfo> {
        let mut plugins = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.plugin_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        let meta_path = path.join("plugin.json");
                        if let Ok(raw) = fs::read_to_string(&meta_path) {
                            if let Ok(info) = serde_json::from_str::<PluginInfo>(&raw) {
                                plugins.push(info);
                                continue;
                            }
                        }
                        plugins.push(PluginInfo {
                            name: name.to_string(),
                            version: "1.0.0".into(),
                            author: "Unknown".into(),
                            description: "Installed plugin".into(),
                            download_url: String::new(),
                            checksum: None,
                            dependencies: Vec::new(),
                            min_version: None,
                        });
                    }
                }
            }
        }
        plugins.sort_by(|a, b| a.name.cmp(&b.name));
        plugins
    }

    pub fn remove_plugin(&self, name: &str) -> bool {
        let clean = name.trim();
        if !is_safe_plugin_name(clean) {
            return false;
        }
        let plugin_dir = self.plugin_dir.join(clean);
        if plugin_dir.is_dir() {
            fs::remove_dir_all(&plugin_dir).is_ok()
        } else {
            false
        }
    }
}

impl Default for PluginAddService {
    fn default() -> Self {
        Self::new(None)
    }
}

// Module-level delegates backed by the shared `PluginAddService::global()` singleton

pub fn get_add_state(app_id: u64, is_installed: bool) -> AddState {
    let aid = u32::try_from(app_id).unwrap_or(0);
    PluginAddService::global().get_add_state(aid, is_installed)
}

pub fn start_headless_add(app_id: u64, game_name: Option<String>, stplug_dir: &Path) {
    if let Ok(aid) = u32::try_from(app_id) {
        PluginAddService::global().start_headless_add(aid, game_name, stplug_dir, None);
    }
}

pub fn pick_source_and_download(app_id: u64, source_name: &str, stplug_dir: &Path) {
    if let Ok(aid) = u32::try_from(app_id) {
        let pref = (!source_name.trim().is_empty()).then(|| source_name.trim().to_string());
        PluginAddService::global().start_headless_add(aid, None, stplug_dir, pref);
    }
}

pub fn cancel_headless_add(app_id: u64) -> bool {
    u32::try_from(app_id)
        .map(|aid| PluginAddService::global().cancel_add(aid))
        .unwrap_or(false)
}

pub fn check_sources(app_id: u64) -> Vec<SourceRow> {
    u32::try_from(app_id)
        .map(|aid| PluginAddService::global().check_sources(aid))
        .unwrap_or_default()
}

fn normalize_source_name(raw: &str) -> String {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("hubcap") {
        "hubcap".into()
    } else if lower.contains("morrenus") || lower.contains("sadie") {
        "morrenus".into()
    } else {
        lower
    }
}

fn format_source_display_name(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "hubcap" => "Hubcap".into(),
        "morrenus" | "sadie" => "Sadie (Morrenus)".into(),
        other if !other.is_empty() => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        }
        _ => "Unknown".into(),
    }
}

fn parse_kb_progress(msg: &str) -> (u64, u64) {
    let nums: Vec<u64> = msg
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<u64>().ok())
        .collect();
    if msg.contains("KB") {
        match nums.as_slice() {
            [read_kb, total_kb, ..] => (read_kb * 1024, total_kb * 1024),
            [read_kb] => (read_kb * 1024, 0),
            _ => (0, 0),
        }
    } else {
        (0, 0)
    }
}

fn is_safe_plugin_name(name: &str) -> bool {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.chars().any(|c| c.is_control() || c == '\0')
    {
        return false;
    }
    let mut comps = Path::new(name).components();
    matches!(comps.next(), Some(std::path::Component::Normal(_))) && comps.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;
    use zip::write::SimpleFileOptions;

    #[test]
    fn test_validate_plugin() {
        let dir = tempdir().unwrap();
        let service = PluginAddService::new(Some(dir.path()));
        let info = PluginInfo {
            name: "TestPlugin".into(),
            version: "1.0.0".into(),
            author: "Test".into(),
            description: "Test plugin".into(),
            download_url: "https://example.com/plugin.zip".into(),
            checksum: Some("ab".repeat(32)),
            dependencies: Vec::new(),
            min_version: None,
        };
        assert!(service.validate_plugin(&info).is_ok());

        for bad_name in ["", "   ", "../escape", "a/b", "a\\b", "..", "."] {
            let mut bad = info.clone();
            bad.name = bad_name.into();
            assert!(
                service.validate_plugin(&bad).is_err(),
                "expected '{}' to be rejected",
                bad_name
            );
        }

        let mut bad_url = info.clone();
        bad_url.download_url = "ftp://example.com/p.zip".into();
        assert!(service.validate_plugin(&bad_url).is_err());

        let mut bad_checksum = info.clone();
        bad_checksum.checksum = Some("not-a-sha256".into());
        assert!(service.validate_plugin(&bad_checksum).is_err());
    }

    #[test]
    fn test_add_list_and_remove_plugin_persists_metadata_and_blocks_traversal() {
        let dir = tempdir().unwrap();
        let service = PluginAddService::new(Some(dir.path()));
        let info = PluginInfo {
            name: "TestPlugin".into(),
            version: "2.3.4".into(),
            author: "LuaTools".into(),
            description: "Real description".into(),
            download_url: "https://example.com/plugin.zip".into(),
            checksum: None,
            dependencies: vec!["core".into()],
            min_version: Some("2.9.0".into()),
        };
        let result = service.add_plugin(&info);
        assert!(result.success);
        assert!(result.installed_path.is_some());

        let listed = service.list_plugins();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].version, "2.3.4");
        assert_eq!(listed[0].author, "LuaTools");
        assert_eq!(listed[0].dependencies, vec!["core"]);

        assert!(!service.remove_plugin(".."));
        assert!(!service.remove_plugin("../TestPlugin"));
        assert!(service.remove_plugin("TestPlugin"));
        assert!(service.list_plugins().is_empty());
    }

    #[test]
    fn test_check_sources_probes_live_check_apis_endpoint() {
        let srv = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = srv.server_addr().to_ip().unwrap();
        let handle = thread::spawn(move || {
            let req = srv.recv().unwrap();
            assert!(req.url().contains("/check_apis?appid=1245620"));
            let resp = tiny_http::Response::from_string(
                r#"{"morrenus":"available","ryuu":"unavailable"}"#,
            )
            .with_header(
                tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
            );
            let _ = req.respond(resp);
        });

        let dir = tempdir().unwrap();
        let svc = PluginAddService::new(Some(dir.path()));
        let rows = svc.check_sources_with_urls(1245620, &format!("http://{}", addr), Some(""));
        handle.join().unwrap();

        assert_eq!(rows.len(), 2);
        let morrenus = rows.iter().find(|r| r.name == "morrenus").unwrap();
        assert!(morrenus.available);
        assert_eq!(morrenus.display_name, "Sadie (Morrenus)");
        let ryuu = rows.iter().find(|r| r.name == "ryuu").unwrap();
        assert!(!ryuu.available);
    }

    #[test]
    fn test_check_sources_rejects_untrusted_ssrf_urls() {
        let dir = tempdir().unwrap();
        let svc = PluginAddService::new(Some(dir.path()));

        assert!(PluginAddService::is_trusted_backend_url(MANIFEST_BACKEND_URL));
        assert!(PluginAddService::is_trusted_backend_url("https://167.235.229.108"));
        assert!(PluginAddService::is_trusted_backend_url("https://lua.tools"));
        assert!(PluginAddService::is_trusted_backend_url("http://127.0.0.1:8080"));
        assert!(PluginAddService::is_trusted_backend_url("http://localhost:3000"));

        // Reject external/private/cloud metadata SSRF targets
        assert!(!PluginAddService::is_trusted_backend_url("http://169.254.169.254/latest/meta-data"));
        assert!(!PluginAddService::is_trusted_backend_url("http://192.168.1.1/admin"));
        assert!(!PluginAddService::is_trusted_backend_url("http://10.0.0.1"));
        assert!(!PluginAddService::is_trusted_backend_url("http://attacker.com/evil"));
        assert!(!PluginAddService::is_trusted_backend_url("file:///etc/passwd"));
        assert!(!PluginAddService::is_trusted_backend_url("gopher://127.0.0.1:25/"));

        // Reject privileged or dangerous loopback ports
        assert!(!PluginAddService::is_trusted_backend_url("http://127.0.0.1:22/"));
        assert!(!PluginAddService::is_trusted_backend_url("http://127.0.0.1:25/"));
        assert!(!PluginAddService::is_trusted_backend_url("http://127.0.0.1:3306/"));
        assert!(!PluginAddService::is_trusted_backend_url("http://127.0.0.1:6379/"));

        let blocked1 = svc.check_sources_with_urls(1245620, "http://169.254.169.254", None);
        assert!(blocked1.is_empty());
        let blocked2 = svc.check_sources_with_urls(1245620, "http://attacker.com", None);
        assert!(blocked2.is_empty());
    }

    #[test]
    fn test_install_downloaded_package_and_cooperative_cancel() {
        let dir = tempdir().unwrap();
        let steam_root = dir.path().join("Steam");
        let stplug_dir = steam_root.join("config/stplug-in");
        fs::create_dir_all(&stplug_dir).unwrap();

        let svc = PluginAddService::new(Some(&dir.path().join("plugins")));
        let mut settings = SettingsService::new(Some(dir.path().join("settings.json")));
        settings.settings.auto_update_apps = false;
        let mut sls = SlssteamService::new(Some(dir.path().join("sls.yaml")));

        let key_hex = "ab".repeat(32);
        let zip_path = dir.path().join("1245620.zip");
        {
            let f = fs::File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts = SimpleFileOptions::default();
            zw.start_file("1245620.lua", opts).unwrap();
            zw.write_all(
                format!(
                    "addappid(1245620)\naddappid(1245621, 1, \"{}\")\n",
                    key_hex
                )
                .as_bytes(),
            )
            .unwrap();
            zw.start_file("1245621_998877.manifest", opts).unwrap();
            zw.write_all(b"MANIFEST_DATA").unwrap();
            zw.finish().unwrap();
        }

        let res = svc.install_downloaded_package(
            1245620,
            &zip_path,
            &stplug_dir,
            "hubcap",
            Some(&mut settings),
            Some(&mut sls),
        );
        assert!(res.ok);
        assert!(res.lua_installed);
        assert_eq!(res.manifest_count, 1);

        let installed_lua = fs::read_to_string(stplug_dir.join("1245620.lua")).unwrap();
        assert!(!installed_lua.contains("NEED_KEY_"));
        assert!(installed_lua.contains(&key_hex));
        assert_eq!(
            sls.config.depot_keys.get(&1245621).map(String::as_str),
            Some(key_hex.as_str())
        );

        let st = svc.get_add_state(1245620, true);
        assert_eq!(st.status, "done");
        assert!(st.installed);
        assert_eq!(st.current_api.as_deref(), Some("hubcap"));

        // Verify cooperative cancellation state transition
        svc.cancel_add(730);
        let cancelled_st = svc.get_add_state(730, false);
        assert_eq!(cancelled_st.status, "cancelled");
        assert!(cancelled_st.cancelled);
    }
}
