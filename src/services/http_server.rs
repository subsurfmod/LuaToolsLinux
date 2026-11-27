//! Local HTTP/RPC server (`127.0.0.1:6767`) for injected Steam Store JS & CEF CDP polyfill
//! (LT-26, LT-27, LT-28).

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use base64::{engine::general_purpose, Engine as _};
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::parsers::lua_parser::LuaFileParser;
use crate::services::api_client::HubcapService;
use crate::services::applied_fix_index::AppliedFixIndexService;
use crate::services::auth_service::AuthService;
use crate::services::millennium_service::MillenniumService;
use crate::services::plugin_add_service::PluginAddService;
use crate::services::plugin_log::PluginLog;
use crate::services::settings_service::SettingsService;
use crate::services::slssteam_service::SlssteamService;
use crate::services::steam_service::SteamService;
use crate::services::update_service::{UpdateCheckResult, UpdateService};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcUiEvent {
    AddApp(u32),
    OpenFix(u32),
    OpenSettings,
    OpenLibrary,
    OpenDepots(u32),
    RemovedApp(u32),
    RestoreFromTray,
}

static REPORTED_LOADED_APPS: OnceLock<Mutex<BTreeSet<u32>>> = OnceLock::new();
static LAST_UPDATE_CHECK: OnceLock<Mutex<Option<(Instant, UpdateCheckResult)>>> = OnceLock::new();

fn reported_loaded_apps() -> &'static Mutex<BTreeSet<u32>> {
    REPORTED_LOADED_APPS.get_or_init(|| Mutex::new(BTreeSet::new()))
}

fn last_update_check() -> &'static Mutex<Option<(Instant, UpdateCheckResult)>> {
    LAST_UPDATE_CHECK.get_or_init(|| Mutex::new(None))
}

pub struct HttpServerService {
    pub port: u16,
    stop_flag: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl HttpServerService {
    pub fn start(
        port: u16,
        stplug_dir: PathBuf,
        event_tx: Sender<RpcUiEvent>,
        wakeup: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Option<Self> {
        let server = Server::http(("127.0.0.1", port)).ok()?;
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop_flag);

        PluginLog::global().info(
            Some("HttpServer"),
            &format!("Listening on http://127.0.0.1:{}", port),
        );

        let handle = thread::spawn(move || {
            while !stop_clone.load(Ordering::Relaxed) {
                if let Ok(Some(req)) = server.recv_timeout(Duration::from_millis(250)) {
                    handle_request(req, &stplug_dir, &event_tx, &wakeup);
                }
            }
        });

        Some(Self {
            port,
            stop_flag,
            handle: Some(handle),
        })
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for HttpServerService {
    fn drop(&mut self) {
        self.stop();
    }
}

fn allow_external_spawn() -> bool {
    !cfg!(test) && std::env::var_os("LUATOOLS_NO_EXTERNAL_LAUNCH").is_none()
}

/// Validates a URL requested via `/open-url` before passing it to `xdg-open`.
/// Only `http://` and `https://` URLs with real non-reserved hosts are permitted;
/// RFC 2606 example/test domains (such as `example.com`) and non-HTTP schemes are rejected.
pub fn validate_external_url(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 2048 {
        return Err("Empty or overly long URL");
    }
    if trimmed.starts_with('-') || trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("URL contains invalid characters");
    }
    let parsed = url::Url::parse(trimmed).map_err(|_| "Malformed URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Only http:// and https:// URLs are allowed");
    }
    let host = parsed
        .host_str()
        .map(|h| h.to_ascii_lowercase())
        .ok_or("URL is missing host")?;
    let is_reserved_example = host == "example.com"
        || host.ends_with(".example.com")
        || host == "example.org"
        || host.ends_with(".example.org")
        || host == "example.net"
        || host.ends_with(".example.net")
        || host.ends_with(".example")
        || host.ends_with(".invalid")
        || host.ends_with(".test");
    if is_reserved_example {
        return Err("Reserved example/test domain is not allowed");
    }
    Ok(parsed.to_string())
}

fn get_request_header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

/// Validates whether an incoming HTTP request `Origin` header is permitted
/// to access local resources via CORS/PNA. Only official Steam domains (HTTPS)
/// and local loopback origins are permitted.
pub fn is_allowed_origin(origin: &str) -> bool {
    let trimmed = origin.trim();
    if trimmed.is_empty() {
        return false;
    }
    let Ok(parsed) = url::Url::parse(trimmed) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host_lower = host.to_ascii_lowercase();

    // Permitted Steam web domains (strictly HTTPS)
    if parsed.scheme() == "https" {
        if host_lower == "store.steampowered.com"
            || host_lower == "steamcommunity.com"
            || host_lower == "steamloopback.host"
            || host_lower.ends_with(".steampowered.com")
            || host_lower.ends_with(".steamcommunity.com")
        {
            return true;
        }
    }

    // Permitted local loopback origins
    if matches!(parsed.scheme(), "http" | "https") {
        if host_lower == "localhost"
            || host_lower == "127.0.0.1"
            || host_lower == "::1"
            || host_lower == "[::1]"
        {
            return true;
        }
        if let Ok(ip) = host_lower.parse::<std::net::IpAddr>() {
            if ip.is_loopback() {
                return true;
            }
        }
    }

    false
}

/// Validates the `Host` header to protect local HTTP services from DNS rebinding attacks.
pub fn is_allowed_host(host_header: &str) -> bool {
    let trimmed = host_header.trim();
    if trimmed.is_empty() {
        return true;
    }
    let host_part = trimmed.split(':').next().unwrap_or(trimmed).trim();
    let host_lower = host_part.to_ascii_lowercase();
    if host_lower == "127.0.0.1"
        || host_lower == "localhost"
        || host_lower == "::1"
        || host_lower == "[::1]"
    {
        return true;
    }
    if let Ok(ip) = host_lower.parse::<std::net::IpAddr>() {
        if ip.is_loopback() {
            return true;
        }
    }
    false
}

/// Checks whether an incoming HTTP request is authorized to invoke sensitive operations.
/// Direct local CLI/process requests (no `Origin` header and valid loopback `Host`) are allowed.
/// Browser-driven requests MUST possess a validated `Origin` from the Steam/loopback allowlist.
pub fn is_request_authorized(req: &Request) -> bool {
    // 1. Defend against DNS rebinding
    if let Some(host) = get_request_header(req, "Host") {
        if !is_allowed_host(host) {
            return false;
        }
    }

    // 2. Browser cross-origin requests must come from an allowed origin
    if let Some(origin) = get_request_header(req, "Origin") {
        if !is_allowed_origin(origin) {
            return false;
        }
    }

    // 3. Fetch metadata defense against cross-site CSRF (e.g. <img>, <form>)
    if let Some(site) = get_request_header(req, "Sec-Fetch-Site") {
        if site.eq_ignore_ascii_case("cross-site") {
            if let Some(origin) = get_request_header(req, "Origin") {
                if !is_allowed_origin(origin) {
                    return false;
                }
            } else {
                return false;
            }
        }
    }

    true
}

fn send_json(req: Request, status: u16, payload: Value) {
    let origin_opt = get_request_header(&req, "Origin").map(ToString::to_string);
    let body = payload.to_string();
    let mut resp = Response::from_string(body)
        .with_status_code(status)
        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());

    if let Some(ref orig) = origin_opt {
        if is_allowed_origin(orig) {
            resp = resp
                .with_header(Header::from_bytes("Access-Control-Allow-Origin", orig.clone()).unwrap())
                .with_header(Header::from_bytes("Vary", "Origin").unwrap())
                .with_header(
                    Header::from_bytes(
                        "Access-Control-Allow-Methods",
                        "GET, POST, PUT, DELETE, OPTIONS, HEAD",
                    )
                    .unwrap(),
                )
                .with_header(
                    Header::from_bytes(
                        "Access-Control-Allow-Headers",
                        "Content-Type, Authorization, X-Requested-With, Accept, Origin",
                    )
                    .unwrap(),
                )
                .with_header(Header::from_bytes("Access-Control-Max-Age", "86400").unwrap())
                .with_header(Header::from_bytes("Access-Control-Allow-Private-Network", "true").unwrap());
        }
    }

    let _ = req.respond(resp);
}

fn send_bytes(req: Request, status: u16, content_type: &str, bytes: Vec<u8>) {
    let origin_opt = get_request_header(&req, "Origin").map(ToString::to_string);
    let mut resp = Response::from_data(bytes)
        .with_status_code(status)
        .with_header(Header::from_bytes("Content-Type", content_type.to_string()).unwrap());

    if let Some(ref orig) = origin_opt {
        if is_allowed_origin(orig) {
            resp = resp
                .with_header(Header::from_bytes("Access-Control-Allow-Origin", orig.clone()).unwrap())
                .with_header(Header::from_bytes("Vary", "Origin").unwrap())
                .with_header(
                    Header::from_bytes(
                        "Access-Control-Allow-Methods",
                        "GET, POST, PUT, DELETE, OPTIONS, HEAD",
                    )
                    .unwrap(),
                )
                .with_header(
                    Header::from_bytes(
                        "Access-Control-Allow-Headers",
                        "Content-Type, Authorization, X-Requested-With, Accept, Origin",
                    )
                    .unwrap(),
                )
                .with_header(Header::from_bytes("Access-Control-Max-Age", "86400").unwrap())
                .with_header(Header::from_bytes("Access-Control-Allow-Private-Network", "true").unwrap());
        }
    }

    let _ = req.respond(resp);
}

fn extract_query_param(query_str: &str, param: &str) -> Option<String> {
    for pair in query_str.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k.eq_ignore_ascii_case(param) {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn extract_query_appid(query_str: &str) -> Option<u32> {
    for pair in query_str.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k.eq_ignore_ascii_case("appid") {
                if let Ok(id) = v.parse::<u32>() {
                    return Some(id);
                }
            }
        }
    }
    None
}

fn list_installed_apps(stplug_dir: &Path) -> Vec<u32> {
    let mut apps = Vec::new();
    if let Ok(entries) = std::fs::read_dir(stplug_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().map_or(false, |ext| ext == "lua") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    if let Ok(id) = stem.parse::<u32>() {
                        apps.push(id);
                    }
                }
            }
        }
    }
    apps.sort_unstable();
    apps.dedup();
    apps
}

fn collect_merged_loaded_apps(stplug_dir: &Path) -> Vec<u32> {
    let mut merged: BTreeSet<u32> = list_installed_apps(stplug_dir).into_iter().collect();
    if let Ok(guard) = reported_loaded_apps().lock() {
        merged.extend(guard.iter().copied());
    }
    if allow_external_spawn() {
        let sls = SlssteamService::new(None);
        merged.extend(sls.config.additional_apps.iter().copied());
    }
    merged.into_iter().collect()
}

fn parse_app_id_list(val: &Value) -> Vec<u32> {
    let arr_opt = val.as_array().or_else(|| {
        val.get("apps")
            .or_else(|| val.get("appIds"))
            .or_else(|| val.get("appids"))
            .or_else(|| val.get("loadedApps"))
            .and_then(|v| v.as_array())
    });
    let Some(arr) = arr_opt else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in arr {
        let parsed = item
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .or_else(|| item.as_str().and_then(|s| s.trim().parse::<u32>().ok()))
            .or_else(|| {
                item.get("appid")
                    .or_else(|| item.get("appId"))
                    .and_then(|v| {
                        v.as_u64()
                            .and_then(|n| u32::try_from(n).ok())
                            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u32>().ok()))
                    })
            });
        if let Some(id) = parsed.filter(|&id| id > 0) {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// Removes `<appid>.lua` and `<appid>.lua.disabled`, cancels any active headless add,
/// and purges all matching entries (including depot keys, manifest overrides, DLCs, tokens)
/// from `SLSsteam` `config.yaml` (LT-27).
pub fn remove_app_completely_with_sls(
    stplug_dir: &Path,
    aid: u32,
    sls_opt: Option<&mut SlssteamService>,
) -> (bool, bool) {
    let lua_path = stplug_dir.join(format!("{}.lua", aid));
    let disabled_path = stplug_dir.join(format!("{}.lua.disabled", aid));

    // Parse script contents BEFORE removal so we also scrub depot keys & manifest overrides
    let mut depot_ids: Vec<u32> = Vec::new();
    for path in [&lua_path, &disabled_path] {
        if let Ok(txt) = std::fs::read_to_string(path) {
            let parsed = LuaFileParser::parse_text(&txt, aid);
            for entry in parsed.entries.iter().chain(parsed.disabled_entries.iter()) {
                if !depot_ids.contains(&entry.id) {
                    depot_ids.push(entry.id);
                }
            }
        }
    }

    let removed_lua = lua_path.exists() && std::fs::remove_file(&lua_path).is_ok();
    let removed_disabled = disabled_path.exists() && std::fs::remove_file(&disabled_path).is_ok();

    PluginAddService::global().cancel_add(aid);
    if let Ok(mut guard) = reported_loaded_apps().lock() {
        guard.remove(&aid);
    }

    if let Some(sls) = sls_opt {
        sls.remove_fake_app_id(aid);
        sls.remove_token(aid);
        sls.remove_additional_app(aid);
        sls.remove_process_mapping(aid);
        sls.remove_force_denuvo(aid);
        sls.remove_pinned_app(aid);
        sls.remove_cd_key(aid);
        sls.remove_game_title(aid);
        sls.config.dlc_data.remove(&aid);
        for did in depot_ids {
            sls.config.depot_keys.remove(&did);
            sls.remove_manifest_override(did);
        }
        let _ = sls.save();
    }

    PluginLog::global().info(
        Some("HttpServer"),
        &format!(
            "Removed AppID {} (lua={}, lua.disabled={})",
            aid, removed_lua, removed_disabled
        ),
    );
    (removed_lua, removed_disabled)
}

fn remove_app_completely(stplug_dir: &Path, aid: u32) -> (bool, bool) {
    if allow_external_spawn() {
        let mut sls = SlssteamService::new(None);
        remove_app_completely_with_sls(stplug_dir, aid, Some(&mut sls))
    } else {
        remove_app_completely_with_sls(stplug_dir, aid, None)
    }
}

fn run_update_check(body_json: Option<&Value>) -> UpdateCheckResult {
    let explicit_repo = body_json
        .and_then(|v| v.get("repoDir").or_else(|| v.get("repo_dir")))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let explicit_remote = body_json
        .and_then(|v| v.get("remoteUrl").or_else(|| v.get("remote_url")))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    let force = body_json
        .and_then(|v| v.get("force"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if let Some(repo) = explicit_repo {
        let res = UpdateService::check_once(repo, explicit_remote);
        if let Ok(mut guard) = last_update_check().lock() {
            *guard = Some((Instant::now(), res.clone()));
        }
        return res;
    }

    if !force {
        if let Ok(guard) = last_update_check().lock() {
            if let Some((checked_at, cached)) = guard.as_ref() {
                if checked_at.elapsed() < Duration::from_secs(60) {
                    return cached.clone();
                }
            }
        }
    }

    let res = if allow_external_spawn() {
        let settings = SettingsService::default().settings;
        let remote = if !explicit_remote.is_empty() {
            explicit_remote
        } else {
            settings.git_remote_url.as_str()
        };
        UpdateService::check_once(&settings.update_repo_dir, remote)
    } else {
        let repo_dir = UpdateService::discover_repo_dir("");
        UpdateCheckResult {
            available: false,
            current_version: crate::VERSION.to_string(),
            remote_version: Some(crate::VERSION.to_string()),
            commits_behind: 0,
            latest_commit: None,
            repo_dir,
            remote_url: None,
            summary: format!("Up to date (v{})", crate::VERSION),
        }
    };

    if let Ok(mut guard) = last_update_check().lock() {
        *guard = Some((Instant::now(), res.clone()));
    }
    res
}

fn update_check_to_json(res: &UpdateCheckResult) -> Value {
    json!({
        "success": true,
        "updateAvailable": res.available,
        "currentVersion": res.current_version,
        "remoteVersion": res.remote_version,
        "commitsBehind": res.commits_behind,
        "latestCommit": res.latest_commit,
        "summary": res.summary,
        "result": {
            "updateAvailable": res.available,
            "currentVersion": res.current_version,
            "remoteVersion": res.remote_version,
            "commitsBehind": res.commits_behind,
            "latestCommit": res.latest_commit,
            "summary": res.summary,
        }
    })
}

fn download_status_state_json(aid: u32, stplug_dir: &Path) -> Value {
    let is_inst = aid > 0 && stplug_dir.join(format!("{}.lua", aid)).exists();
    let state = PluginAddService::global().get_add_state(aid, is_inst);
    let api_errors_val = if state.api_errors.is_empty() {
        Value::Null
    } else {
        json!(state.api_errors)
    };
    json!({
        "appid": aid,
        "status": state.status,
        "bytesRead": state.bytes_read,
        "totalBytes": state.total_bytes,
        "progress": state.progress,
        "currentApi": state.current_api,
        "api": state.current_api,
        "apiErrors": api_errors_val,
        "error": state.error,
        "installedPath": state.installed_path,
        "success": state.installed && !state.install_failed && !state.cancelled,
        "installed": state.installed,
        "cancelled": state.cancelled,
        "checking": state.checking,
        "fastFetch": state.fast_fetch,
        "sourcesLoaded": state.sources_loaded,
        "sources": state.sources,
        "installStatus": state.install_status,
        "installFailed": state.install_failed,
    })
}

fn sanitized_settings_json() -> Value {
    let s = SettingsService::default().settings;
    json!({
        "platform": "linux",
        "version": crate::VERSION,
        "autoUpdateApps": s.auto_update_apps,
        "fastFetch": s.fast_fetch,
        "donateKeys": s.donate_keys,
        "hasHubcapApiKey": HubcapService::is_valid_key_format(&s.hubcap_api_key),
        "cefBridgeEnabled": s.cef_bridge_enabled,
        "httpServerEnabled": s.http_server_enabled,
        "httpServerPort": s.http_server_port,
        "language": s.language,
        "checkUpdatesOnStartup": s.check_updates_on_startup,
        "autostart": s.autostart,
        "closeToTray": s.close_to_tray,
        "minimizeToTray": s.minimize_to_tray,
        "dohEnabled": s.doh_enabled,
    })
}

fn configured_apis_json() -> Vec<Value> {
    let s = SettingsService::default().settings;
    let has_hubcap = HubcapService::is_valid_key_format(&s.hubcap_api_key);
    let auth = AuthService::default();
    let has_discord = auth.initialize();
    vec![
        json!({
            "name": "Hubcap",
            "id": "hubcap",
            "index": 0,
            "configured": has_hubcap,
            "requiresKey": true,
        }),
        json!({
            "name": "Sadie (Morrenus)",
            "id": "morrenus",
            "index": 1,
            "configured": has_discord,
            "requiresKey": false,
        }),
    ]
}

fn resolve_launcher_path() -> String {
    if let Some(home) = dirs::home_dir() {
        for cand in [
            home.join(".local/bin/luatools-steam-launcher"),
            home.join(".local/bin/steam-sls"),
        ] {
            if cand.is_file() {
                return cand.to_string_lossy().to_string();
            }
        }
    }
    crate::services::steam_service::detect_steam_install(None)
        .map(|inst| inst.root.join("steam.sh").to_string_lossy().to_string())
        .unwrap_or_default()
}

fn resolve_workshop_path() -> String {
    let sls = SlssteamService::new(None);
    if !sls.config.workshop_path.trim().is_empty() {
        return sls.config.workshop_path;
    }
    crate::services::steam_service::detect_steam_install(None)
        .map(|inst| {
            inst.root
                .join("steamapps/workshop")
                .to_string_lossy()
                .to_string()
        })
        .unwrap_or_default()
}

fn available_locales_json() -> Vec<String> {
    vec!["en".into()]
}

fn handle_request(
    mut req: Request,
    stplug_dir: &Path,
    event_tx: &Sender<RpcUiEvent>,
    wakeup: &Option<Arc<dyn Fn() + Send + Sync>>,
) {
    // 1. CORS preflight handling (LT-26)
    if req.method() == &Method::Options {
        let origin_opt = get_request_header(&req, "Origin");
        if let Some(orig) = origin_opt {
            if is_allowed_origin(orig) {
                let resp = Response::empty(204)
                    .with_header(Header::from_bytes("Access-Control-Allow-Origin", orig).unwrap())
                    .with_header(Header::from_bytes("Vary", "Origin").unwrap())
                    .with_header(
                        Header::from_bytes(
                            "Access-Control-Allow-Methods",
                            "GET, POST, PUT, DELETE, OPTIONS, HEAD",
                        )
                        .unwrap(),
                    )
                    .with_header(
                        Header::from_bytes(
                            "Access-Control-Allow-Headers",
                            "Content-Type, Authorization, X-Requested-With, Accept, Origin",
                        )
                        .unwrap(),
                    )
                    .with_header(Header::from_bytes("Access-Control-Max-Age", "86400").unwrap())
                    .with_header(
                        Header::from_bytes("Access-Control-Allow-Private-Network", "true").unwrap(),
                    );
                let _ = req.respond(resp);
                return;
            }
        }
        // Disallowed origin or missing origin receives 403 Forbidden without CORS/PNA grant
        let resp = Response::empty(403);
        let _ = req.respond(resp);
        return;
    }

    let full_url = req.url().to_string();
    let method_str = req.method().to_string();
    let mut parts_split = full_url.splitn(2, '?');
    let path_only = parts_split.next().unwrap_or("/");
    let query_str = parts_split.next().unwrap_or("");
    let parts: Vec<&str> = path_only.split('/').filter(|s| !s.is_empty()).collect();

    if !path_only.starts_with("/add-status")
        && !path_only.starts_with("/has")
        && !path_only.starts_with("/download-status")
    {
        PluginLog::global().info(
            Some("HttpServer"),
            &format!("HTTP {} {}", method_str, path_only),
        );
    }

    let notify = || {
        if let Some(cb) = wakeup {
            cb();
        }
    };

    // Helper to extract appid from path segments, query param, or JSON body
    let mut aid = parts
        .iter()
        .find_map(|p| p.parse::<u32>().ok())
        .or_else(|| extract_query_appid(query_str))
        .unwrap_or(0);

    // Read JSON body for POST/PUT if available
    let body_json = if req.method() == &Method::Post || req.method() == &Method::Put {
        let mut raw = String::new();
        let _ = req.as_reader().take(256 * 1024).read_to_string(&mut raw);
        serde_json::from_str::<Value>(&raw).ok()
    } else {
        None
    };

    if aid == 0 {
        if let Some(ref val) = body_json {
            if let Some(id) = val.get("appid").or_else(|| val.get("appId")).and_then(|v| {
                v.as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u32>().ok()))
            }) {
                aid = id;
            }
        }
    }

    // Route: /ping
    if parts == ["ping"] {
        send_json(
            req,
            200,
            json!({ "success": true, "ok": true, "platform": "linux", "version": crate::VERSION }),
        );
        return;
    }

    // Route: /version
    if parts == ["version"] || parts == ["api", "version"] {
        send_json(
            req,
            200,
            json!({ "success": true, "ok": true, "version": crate::VERSION, "platform": "linux" }),
        );
        return;
    }

    // Route: /status or /health
    if parts == ["status"] || parts == ["health"] {
        send_json(
            req,
            200,
            json!({ "success": true, "ok": true, "status": "running", "platform": "linux" }),
        );
        return;
    }

    // Route: /apps or /installed or /list
    if parts == ["apps"] || parts == ["installed"] || parts == ["list"] {
        let apps = list_installed_apps(stplug_dir);
        send_json(
            req,
            200,
            json!({ "success": true, "ok": true, "apps": apps, "total": apps.len() }),
        );
        return;
    }

    // Route: /rpc (JSON-RPC — LT-27 & LT-28)
    if parts == ["rpc"] && req.method() == &Method::Post {
        if !is_request_authorized(&req) {
            send_json(
                req,
                403,
                json!({ "success": false, "error": "Unauthorized origin or host" }),
            );
            return;
        }
        let Some(payload) = body_json else {
            send_json(
                req,
                400,
                json!({ "success": false, "error": "Invalid JSON" }),
            );
            return;
        };
        let method = payload.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let args = payload.get("args").cloned().unwrap_or_else(|| json!({}));
        let rpc_aid = args
            .get("appid")
            .or_else(|| args.get("appId"))
            .and_then(|v| {
                v.as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u32>().ok()))
            })
            .unwrap_or(aid);

        match method {
            "Ping" | "ping" => {
                send_json(req, 200, json!({ "success": true, "result": "pong" }));
            }
            "HasLuaToolsForApp" => {
                if rpc_aid == 0 {
                    send_json(
                        req,
                        400,
                        json!({ "success": false, "error": "Missing or invalid appid" }),
                    );
                } else {
                    let exists = stplug_dir.join(format!("{}.lua", rpc_aid)).exists();
                    send_json(
                        req,
                        200,
                        json!({ "success": true, "exists": exists, "has": exists, "result": exists }),
                    );
                }
            }
            "AddViaLuaTools" | "StartAddViaLuaTools" | "StartLuaToolsAdd" => {
                if rpc_aid > 0 {
                    let game_name = args
                        .get("name")
                        .or_else(|| args.get("gameName"))
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string);
                    let source = args
                        .get("source")
                        .or_else(|| args.get("sourceName"))
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string);
                    PluginAddService::global().start_headless_add(
                        rpc_aid,
                        game_name,
                        stplug_dir,
                        source,
                    );
                    let _ = event_tx.send(RpcUiEvent::AddApp(rpc_aid));
                    notify();
                    send_json(
                        req,
                        200,
                        json!({
                            "success": true,
                            "queued": true,
                            "appid": rpc_aid,
                            "result": { "queued": true, "appid": rpc_aid }
                        }),
                    );
                } else {
                    send_json(
                        req,
                        400,
                        json!({ "success": false, "error": "Missing or invalid appid" }),
                    );
                }
            }
            "GetAddViaLuaToolsStatus" | "GetLuaToolsAddStatus" | "GetDownloadStatus" => {
                let state_json = download_status_state_json(rpc_aid, stplug_dir);
                send_json(
                    req,
                    200,
                    json!({
                        "success": true,
                        "result": state_json.clone(),
                        "state": state_json,
                        "appid": rpc_aid,
                    }),
                );
            }
            "CancelDownload" | "CancelAddViaLuaTools" | "CancelAdd" => {
                if rpc_aid > 0 {
                    let cancelled = PluginAddService::global().cancel_add(rpc_aid);
                    send_json(
                        req,
                        200,
                        json!({
                            "success": true,
                            "appid": rpc_aid,
                            "cancelled": cancelled,
                            "result": { "appid": rpc_aid, "cancelled": cancelled }
                        }),
                    );
                } else {
                    send_json(
                        req,
                        400,
                        json!({ "success": false, "error": "Missing or invalid appid" }),
                    );
                }
            }
            "CheckSources" | "CheckSource" => {
                if rpc_aid > 0 {
                    let sources = if allow_external_spawn() {
                        PluginAddService::global().check_sources(rpc_aid)
                    } else {
                        let is_inst = stplug_dir.join(format!("{}.lua", rpc_aid)).exists();
                        PluginAddService::global()
                            .get_add_state(rpc_aid, is_inst)
                            .sources
                    };
                    send_json(
                        req,
                        200,
                        json!({
                            "success": true,
                            "appid": rpc_aid,
                            "results": sources,
                            "sources": sources,
                            "result": {
                                "appid": rpc_aid,
                                "sources": sources,
                                "sourcesLoaded": true,
                            }
                        }),
                    );
                } else {
                    send_json(
                        req,
                        400,
                        json!({ "success": false, "error": "Missing or invalid appid" }),
                    );
                }
            }
            "DeleteLuaToolsForApp" => {
                if rpc_aid == 0 {
                    send_json(
                        req,
                        400,
                        json!({ "success": false, "error": "Missing or invalid appid" }),
                    );
                } else {
                    let (removed_lua, removed_disabled) =
                        remove_app_completely(stplug_dir, rpc_aid);
                    let _ = event_tx.send(RpcUiEvent::RemovedApp(rpc_aid));
                    notify();
                    send_json(
                        req,
                        200,
                        json!({
                            "success": true,
                            "removed": true,
                            "appid": rpc_aid,
                            "removedLua": removed_lua,
                            "removedDisabled": removed_disabled,
                            "result": {
                                "removed": true,
                                "appid": rpc_aid,
                                "removedLua": removed_lua,
                                "removedDisabled": removed_disabled,
                            }
                        }),
                    );
                }
            }
            "OpenFix" | "OpenFixes" | "CheckForFixes" => {
                if rpc_aid > 0 {
                    let _ = event_tx.send(RpcUiEvent::OpenFix(rpc_aid));
                    notify();
                    send_json(
                        req,
                        200,
                        json!({ "success": true, "appid": rpc_aid, "result": { "appid": rpc_aid } }),
                    );
                } else {
                    send_json(
                        req,
                        400,
                        json!({ "success": false, "error": "Missing or invalid appid" }),
                    );
                }
            }
            "OpenSettings" => {
                let _ = event_tx.send(RpcUiEvent::OpenSettings);
                notify();
                send_json(req, 200, json!({ "success": true, "result": true }));
            }
            "RestoreFromTray" => {
                let _ = event_tx.send(RpcUiEvent::RestoreFromTray);
                notify();
                send_json(req, 200, json!({ "success": true, "result": true }));
            }
            "CheckUpdates" => {
                let check_res = run_update_check(Some(&args));
                send_json(req, 200, update_check_to_json(&check_res));
            }
            "GetInstalledLuaScripts" | "ReadLoadedApps" | "GetInstalledApps" | "GetLoadedApps" => {
                let apps = collect_merged_loaded_apps(stplug_dir);
                send_json(
                    req,
                    200,
                    json!({ "success": true, "result": apps, "apps": apps, "total": apps.len() }),
                );
            }
            "SetLoadedApps" => {
                let incoming = parse_app_id_list(&args);
                let received = incoming.len();
                if let Ok(mut guard) = reported_loaded_apps().lock() {
                    guard.extend(incoming.iter().copied());
                }
                let merged = collect_merged_loaded_apps(stplug_dir);
                send_json(
                    req,
                    200,
                    json!({
                        "success": true,
                        "apps": merged,
                        "total": merged.len(),
                        "received": received,
                        "result": { "apps": merged, "received": received }
                    }),
                );
            }
            "GetSettingsConfig" => {
                let cfg = sanitized_settings_json();
                send_json(req, 200, json!({ "success": true, "result": cfg }));
            }
            "GetApplyFixStatus" => {
                let idx = AppliedFixIndexService::default();
                let count = if rpc_aid > 0 {
                    idx.get_fixes(rpc_aid).map(|v| v.len()).unwrap_or(0)
                } else {
                    idx.all_fixes().len()
                };
                send_json(
                    req,
                    200,
                    json!({
                        "success": true,
                        "result": {
                            "status": if count > 0 { "applied" } else { "idle" },
                            "appliedCount": count,
                            "appid": rpc_aid,
                        }
                    }),
                );
            }
            "GetInstalledFixes" => {
                let idx = AppliedFixIndexService::default();
                let fixes: Vec<Value> = if rpc_aid > 0 {
                    idx.get_fixes(rpc_aid)
                        .map(|list| {
                            list.iter()
                                .filter_map(|r| serde_json::to_value(r).ok())
                                .collect()
                        })
                        .unwrap_or_default()
                } else {
                    idx.all_fixes()
                        .into_iter()
                        .filter_map(|r| serde_json::to_value(r).ok())
                        .collect()
                };
                send_json(req, 200, json!({ "success": true, "result": fixes }));
            }
            "GetLauncherPath" => {
                let p = resolve_launcher_path();
                send_json(req, 200, json!({ "success": true, "result": p }));
            }
            "GetWorkshopToolPath" => {
                let p = resolve_workshop_path();
                send_json(req, 200, json!({ "success": true, "result": p }));
            }
            "RestartSteam" => {
                if allow_external_spawn() {
                    let _ = SteamService::relaunch_steam_sls(None);
                }
                send_json(req, 200, json!({ "success": true, "result": "restarted" }));
            }
            "GetApiList" => {
                let apis = configured_apis_json();
                send_json(
                    req,
                    200,
                    json!({ "success": true, "apis": apis, "result": apis }),
                );
            }
            "GetMillenniumStatus" => {
                let det = MillenniumService::default().detect();
                send_json(req, 200, json!({ "success": true, "result": det }));
            }
            "GetAvailableLocales" => {
                let locales = available_locales_json();
                send_json(req, 200, json!({ "success": true, "result": locales }));
            }
            "GetTranslations" => {
                send_json(
                    req,
                    200,
                    json!({
                        "success": true,
                        "result": {
                            "language": "en",
                            "availableLocales": available_locales_json(),
                        }
                    }),
                );
            }
            "version" | "GetVersion" => {
                send_json(
                    req,
                    200,
                    json!({ "success": true, "result": crate::VERSION }),
                );
            }
            other => {
                send_json(
                    req,
                    400,
                    json!({
                        "success": false,
                        "error": format!("Unsupported RPC method: {}", other),
                    }),
                );
            }
        }
        return;
    }

    // Route: /has or /has/:appid
    if parts.first() == Some(&"has")
        || (parts.len() == 2 && parts[0] == "api" && parts[1] == "has")
    {
        let exists = aid > 0 && stplug_dir.join(format!("{}.lua", aid)).exists();
        send_json(
            req,
            200,
            json!({ "success": true, "ok": true, "has": exists, "exists": exists, "appid": aid }),
        );
        return;
    }

    // Route: /add or /add/:appid
    if parts.first() == Some(&"add")
        || (parts.len() == 2 && parts[0] == "api" && parts[1] == "add")
    {
        if !is_request_authorized(&req) {
            send_json(
                req,
                403,
                json!({ "success": false, "error": "Unauthorized origin or host" }),
            );
            return;
        }
        if aid == 0 {
            send_json(
                req,
                400,
                json!({ "success": false, "ok": false, "error": "Missing or invalid appid" }),
            );
            return;
        }
        let game_name = body_json
            .as_ref()
            .and_then(|v| v.get("name").or_else(|| v.get("gameName")))
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        let source = body_json
            .as_ref()
            .and_then(|v| v.get("source").or_else(|| v.get("sourceName")))
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        PluginAddService::global().start_headless_add(aid, game_name, stplug_dir, source);
        let _ = event_tx.send(RpcUiEvent::AddApp(aid));
        notify();
        send_json(
            req,
            200,
            json!({ "success": true, "ok": true, "queued": true, "appid": aid }),
        );
        return;
    }

    // Route: /add-status/:appid
    if parts.first() == Some(&"add-status") {
        let is_inst = aid > 0 && stplug_dir.join(format!("{}.lua", aid)).exists();
        let state = PluginAddService::global().get_add_state(aid, is_inst);
        send_json(
            req,
            200,
            json!({
                "success": true,
                "appid": aid,
                "status": state.status,
                "checking": state.checking,
                "fastFetch": state.fast_fetch,
                "sourcesLoaded": state.sources_loaded,
                "sources": state.sources,
                "installStatus": state.install_status,
                "installFailed": state.install_failed,
                "error": state.error,
                "installed": state.installed,
                "cancelled": state.cancelled,
                "progress": state.progress,
                "bytesRead": state.bytes_read,
                "totalBytes": state.total_bytes,
            }),
        );
        return;
    }

    // Route: /add-source/:appid
    if parts.first() == Some(&"add-source") {
        if !is_request_authorized(&req) {
            send_json(
                req,
                403,
                json!({ "success": false, "error": "Unauthorized origin or host" }),
            );
            return;
        }
        let source_name = body_json
            .as_ref()
            .and_then(|v| v.get("source").or_else(|| v.get("sourceName")))
            .and_then(|v| v.as_str())
            .unwrap_or("morrenus");
        if aid == 0 {
            send_json(
                req,
                400,
                json!({ "success": false, "error": "Missing or invalid appid" }),
            );
            return;
        }
        PluginAddService::global().start_headless_add(
            aid,
            None,
            stplug_dir,
            Some(source_name.to_string()),
        );
        send_json(
            req,
            200,
            json!({ "success": true, "appid": aid, "source": source_name }),
        );
        return;
    }

    // Route: /check-sources or /check-sources/:appid (GET and POST — LT-28)
    if parts.first() == Some(&"check-sources") {
        if !is_request_authorized(&req) {
            send_json(
                req,
                403,
                json!({ "success": false, "error": "Unauthorized origin or host", "sources": [] }),
            );
            return;
        }
        if aid == 0 {
            send_json(
                req,
                400,
                json!({ "success": false, "error": "Missing or invalid appid", "sources": [] }),
            );
            return;
        }
        let custom_base = body_json
            .as_ref()
            .and_then(|v| v.get("backendUrl"))
            .and_then(|v| v.as_str());
        if let Some(base) = custom_base {
            if !PluginAddService::is_trusted_backend_url(base) {
                send_json(
                    req,
                    400,
                    json!({
                        "success": false,
                        "error": "Untrusted or disallowed backendUrl for SSRF prevention",
                        "sources": [],
                    }),
                );
                return;
            }
        }
        let sources = if let Some(base) = custom_base {
            PluginAddService::global().check_sources_with_urls(aid, base, None)
        } else if allow_external_spawn() {
            PluginAddService::global().check_sources(aid)
        } else {
            let is_inst = stplug_dir.join(format!("{}.lua", aid)).exists();
            PluginAddService::global()
                .get_add_state(aid, is_inst)
                .sources
        };
        send_json(
            req,
            200,
            json!({
                "success": true,
                "appid": aid,
                "results": sources,
                "sources": sources,
                "sourcesLoaded": true,
            }),
        );
        return;
    }

    // Route: /download/:appid
    if parts.first() == Some(&"download") {
        if !is_request_authorized(&req) {
            send_json(
                req,
                403,
                json!({ "success": false, "error": "Unauthorized origin or host" }),
            );
            return;
        }
        if aid == 0 {
            send_json(
                req,
                400,
                json!({ "success": false, "error": "Missing or invalid appid" }),
            );
            return;
        }
        let source_name = body_json
            .as_ref()
            .and_then(|v| v.get("source").or_else(|| v.get("apiName")))
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        PluginAddService::global().start_headless_add(aid, None, stplug_dir, source_name);
        send_json(
            req,
            200,
            json!({ "success": true, "appid": aid, "queued": true }),
        );
        return;
    }

    // Route: /download-status or /download-status/:appid (LT-28)
    if parts.first() == Some(&"download-status") {
        let state_json = download_status_state_json(aid, stplug_dir);
        send_json(
            req,
            200,
            json!({
                "success": true,
                "appid": aid,
                "state": state_json,
            }),
        );
        return;
    }

    // Route: /cancel or /cancel/:appid (POST — LT-28)
    if parts.first() == Some(&"cancel") {
        if !is_request_authorized(&req) {
            send_json(
                req,
                403,
                json!({ "success": false, "error": "Unauthorized origin or host" }),
            );
            return;
        }
        if req.method() != &Method::Post {
            send_json(
                req,
                405,
                json!({ "success": false, "error": "Method Not Allowed - POST required" }),
            );
            return;
        }
        if aid == 0 {
            send_json(
                req,
                400,
                json!({ "success": false, "error": "Missing or invalid appid" }),
            );
            return;
        }
        let cancelled = PluginAddService::global().cancel_add(aid);
        send_json(
            req,
            200,
            json!({ "success": true, "appid": aid, "cancelled": cancelled }),
        );
        return;
    }

    // Route: /open-url
    if parts == ["open-url"] {
        if !is_request_authorized(&req) {
            send_json(
                req,
                403,
                json!({ "success": false, "error": "Unauthorized origin or host" }),
            );
            return;
        }
        if req.method() != &Method::Post {
            send_json(
                req,
                405,
                json!({ "success": false, "error": "Method Not Allowed - POST required" }),
            );
            return;
        }
        let target_url = body_json
            .as_ref()
            .and_then(|v| v.get("url"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        match validate_external_url(target_url) {
            Ok(safe_url) => {
                let _ = event_tx.send(RpcUiEvent::RestoreFromTray);
                notify();
                if allow_external_spawn() {
                    let _ = std::process::Command::new("xdg-open").arg(&safe_url).spawn();
                }
                send_json(req, 200, json!({ "success": true, "url": safe_url }));
            }
            Err(reason) => {
                send_json(req, 400, json!({ "success": false, "error": reason }));
            }
        }
        return;
    }

    // Route: /restart-steam
    if parts == ["restart-steam"] {
        if !is_request_authorized(&req) {
            send_json(
                req,
                403,
                json!({ "success": false, "error": "Unauthorized origin or host" }),
            );
            return;
        }
        if req.method() != &Method::Post {
            send_json(
                req,
                405,
                json!({ "success": false, "error": "Method Not Allowed - POST required" }),
            );
            return;
        }
        if allow_external_spawn() {
            let _ = SteamService::relaunch_steam_sls(None);
        }
        send_json(req, 200, json!({ "success": true, "restarted": true }));
        return;
    }

    // Route: /check-updates (GET and POST — LT-28)
    if parts == ["check-updates"] {
        if !is_request_authorized(&req) {
            send_json(
                req,
                403,
                json!({ "success": false, "error": "Unauthorized origin or host" }),
            );
            return;
        }
        let res = run_update_check(body_json.as_ref());
        send_json(req, 200, update_check_to_json(&res));
        return;
    }

    // Route: /loaded-apps (GET and POST — LT-28)
    if parts == ["loaded-apps"] {
        if !is_request_authorized(&req) {
            send_json(
                req,
                403,
                json!({ "success": false, "error": "Unauthorized origin or host" }),
            );
            return;
        }
        if req.method() == &Method::Post {
            let incoming = body_json
                .as_ref()
                .map(parse_app_id_list)
                .unwrap_or_default();
            let received = incoming.len();
            if !incoming.is_empty() {
                if let Ok(mut guard) = reported_loaded_apps().lock() {
                    guard.extend(incoming.iter().copied());
                }
                if allow_external_spawn() {
                    let mut ss = SettingsService::default();
                    for id in &incoming {
                        ss.record_loaded_app(*id);
                    }
                }
            }
            let merged = collect_merged_loaded_apps(stplug_dir);
            send_json(
                req,
                200,
                json!({
                    "success": true,
                    "apps": merged,
                    "total": merged.len(),
                    "received": received,
                }),
            );
            return;
        } else {
            let apps = collect_merged_loaded_apps(stplug_dir);
            send_json(
                req,
                200,
                json!({ "success": true, "apps": apps, "total": apps.len() }),
            );
            return;
        }
    }

    // Route: /api-list
    if parts == ["api-list"] {
        let apis = configured_apis_json();
        send_json(req, 200, json!({ "success": true, "apis": apis }));
        return;
    }

    // Route: /icon
    if parts == ["icon"] {
        if aid > 0 {
            let agent = ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(10))
                .build();
            let icon_url = format!(
                "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/capsule_184x69.jpg",
                aid
            );
            if let Ok(resp) = agent.get(&icon_url).call() {
                if resp.status() == 200 {
                    let mut bytes = Vec::new();
                    if resp.into_reader().read_to_end(&mut bytes).is_ok() && !bytes.is_empty() {
                        let b64 = general_purpose::STANDARD.encode(&bytes);
                        send_json(
                            req,
                            200,
                            json!({ "success": true, "dataUrl": format!("data:image/jpeg;base64,{}", b64) }),
                        );
                        return;
                    }
                }
            }
            let header_url = format!(
                "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/header.jpg",
                aid
            );
            if let Ok(resp) = agent.get(&header_url).call() {
                if resp.status() == 200 {
                    let mut bytes = Vec::new();
                    if resp.into_reader().read_to_end(&mut bytes).is_ok() && !bytes.is_empty() {
                        let b64 = general_purpose::STANDARD.encode(&bytes);
                        send_json(
                            req,
                            200,
                            json!({ "success": true, "dataUrl": format!("data:image/jpeg;base64,{}", b64) }),
                        );
                        return;
                    }
                }
            }
        }
        send_json(req, 200, json!({ "success": false, "dataUrl": "" }));
        return;
    }

    // Route: /remove or /remove/:appid or /delete or /delete/:appid (POST only to prevent GET CSRF mutations)
    if parts.first() == Some(&"remove")
        || parts.first() == Some(&"delete")
        || (parts.len() == 2
            && parts[0] == "api"
            && (parts[1] == "remove" || parts[1] == "delete"))
    {
        if !is_request_authorized(&req) {
            send_json(
                req,
                403,
                json!({ "success": false, "error": "Unauthorized origin or host" }),
            );
            return;
        }
        if req.method() != &Method::Post {
            send_json(
                req,
                405,
                json!({ "error": "Method Not Allowed - POST required", "success": false }),
            );
            return;
        }
        if aid == 0 {
            send_json(
                req,
                400,
                json!({ "success": false, "ok": false, "error": "Missing or invalid appid" }),
            );
            return;
        }
        let (removed_lua, removed_disabled) = remove_app_completely(stplug_dir, aid);
        let _ = event_tx.send(RpcUiEvent::RemovedApp(aid));
        notify();
        send_json(
            req,
            200,
            json!({
                "success": true,
                "ok": true,
                "removed": true,
                "appid": aid,
                "removedLua": removed_lua,
                "removedDisabled": removed_disabled,
            }),
        );
        return;
    }

    // Route: /open/fix or /open/fix/:appid or /fix or /fix/:appid
    if (parts.len() >= 2 && parts[0] == "open" && parts[1] == "fix")
        || parts.first() == Some(&"fix")
    {
        if !is_request_authorized(&req) {
            send_json(req, 403, json!({ "success": false, "error": "Unauthorized origin or host" }));
            return;
        }
        let _ = event_tx.send(RpcUiEvent::OpenFix(aid));
        notify();
        send_json(req, 200, json!({ "success": true, "ok": true, "appid": aid }));
        return;
    }

    // Route: /open/settings or /settings
    if (parts.len() == 2 && parts[0] == "open" && parts[1] == "settings") || parts == ["settings"] {
        if !is_request_authorized(&req) {
            send_json(req, 403, json!({ "success": false, "error": "Unauthorized origin or host" }));
            return;
        }
        let _ = event_tx.send(RpcUiEvent::OpenSettings);
        notify();
        send_json(req, 200, json!({ "success": true, "ok": true }));
        return;
    }

    // Route: /open/app/:appid or /open/:appid
    if (parts.len() >= 2 && parts[0] == "open" && parts[1] == "app")
        || (parts.len() == 2 && parts[0] == "open" && aid > 0)
    {
        if !is_request_authorized(&req) {
            send_json(req, 403, json!({ "success": false, "error": "Unauthorized origin or host" }));
            return;
        }
        if aid > 0 {
            let _ = event_tx.send(RpcUiEvent::AddApp(aid));
            notify();
        }
        send_json(req, 200, json!({ "success": true, "ok": true, "appid": aid }));
        return;
    }

    // Route: /open/acquire or /open/acquire/:appid
    if parts.len() >= 2 && parts[0] == "open" && parts[1] == "acquire" {
        if !is_request_authorized(&req) {
            send_json(req, 403, json!({ "success": false, "error": "Unauthorized origin or host" }));
            return;
        }
        if aid > 0 {
            let _ = event_tx.send(RpcUiEvent::AddApp(aid));
            notify();
        }
        send_json(
            req,
            200,
            json!({ "success": true, "ok": true, "appid": aid, "queued": true }),
        );
        return;
    }

    // Route: /open/library
    if parts == ["open", "library"] || parts == ["library"] {
        if !is_request_authorized(&req) {
            send_json(req, 403, json!({ "success": false, "error": "Unauthorized origin or host" }));
            return;
        }
        let _ = event_tx.send(RpcUiEvent::OpenLibrary);
        notify();
        send_json(req, 200, json!({ "success": true, "ok": true }));
        return;
    }

    // Route: /open/depots or /open/depots/:appid
    if parts.len() >= 2 && parts[0] == "open" && parts[1] == "depots" {
        if !is_request_authorized(&req) {
            send_json(req, 403, json!({ "success": false, "error": "Unauthorized origin or host" }));
            return;
        }
        let _ = event_tx.send(RpcUiEvent::OpenDepots(aid));
        notify();
        send_json(req, 200, json!({ "success": true, "ok": true, "appid": aid }));
        return;
    }

    // Route: /steam/restart or /restart
    if parts == ["steam", "restart"] || parts == ["restart"] {
        if !is_request_authorized(&req) {
            send_json(
                req,
                403,
                json!({ "success": false, "error": "Unauthorized origin or host" }),
            );
            return;
        }
        if req.method() != &Method::Post {
            send_json(
                req,
                405,
                json!({ "success": false, "error": "Method Not Allowed - POST required" }),
            );
            return;
        }
        if allow_external_spawn() {
            let _ = SteamService::relaunch_steam_sls(None);
        }
        send_json(
            req,
            200,
            json!({ "success": true, "ok": true, "restarting": true }),
        );
        return;
    }

    // Route: /steam/status
    if parts == ["steam", "status"] {
        let running = SteamService::is_steam_running();
        send_json(
            req,
            200,
            json!({ "success": true, "ok": true, "running": running }),
        );
        return;
    }

    // Route: /config or /settings/config
    if parts == ["config"] || parts == ["settings", "config"] {
        let cfg = sanitized_settings_json();
        send_json(
            req,
            200,
            json!({ "success": true, "ok": true, "config": cfg }),
        );
        return;
    }

    // Route: /check/:appid or /check/fix/:appid
    if parts.first() == Some(&"check") {
        let has_fix = if aid > 0 {
            let api = crate::services::api_client::LuaToolsApiClient::new(AuthService::default());
            let fixes = api.get_drm_fix_fixes(aid);
            !fixes.is_empty()
        } else {
            false
        };
        send_json(
            req,
            200,
            json!({ "success": true, "ok": true, "appid": aid, "has_fix": has_fix }),
        );
        return;
    }

    // Route: /info/:appid
    if parts.first() == Some(&"info") {
        let exists = aid > 0 && stplug_dir.join(format!("{}.lua", aid)).exists();
        send_json(
            req,
            200,
            json!({ "success": true, "ok": true, "appid": aid, "installed": exists }),
        );
        return;
    }

    // Route: /api/givemethemanifestpunk (LT-44)
    if parts == ["api", "givemethemanifestpunk"] || parts == ["givemethemanifestpunk"] {
        let depot_id = parts
            .iter()
            .find_map(|p| p.parse::<u32>().ok())
            .or_else(|| extract_query_appid(query_str))
            .or_else(|| {
                extract_query_param(query_str, "depotid").and_then(|v| v.parse::<u32>().ok())
            })
            .or_else(|| {
                extract_query_param(query_str, "depot_id").and_then(|v| v.parse::<u32>().ok())
            })
            .unwrap_or(0);

        let gid = extract_query_param(query_str, "gid")
            .or_else(|| extract_query_param(query_str, "manifestid"))
            .or_else(|| extract_query_param(query_str, "manifest_id"))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        if depot_id > 0 && gid > 0 {
            if let Some(home) = dirs::home_dir() {
                let cache_candidates = [
                    home.join(format!(
                        ".local/share/luatools/depotcache/{}_{}.manifest",
                        depot_id, gid
                    )),
                    home.join(format!(
                        ".local/share/Steam/depotcache/{}_{}.manifest",
                        depot_id, gid
                    )),
                    stplug_dir.join(format!("{}_{}.manifest", depot_id, gid)),
                ];
                for p in cache_candidates {
                    if let Ok(b) = std::fs::read(&p) {
                        send_bytes(req, 200, "application/octet-stream", b);
                        return;
                    }
                }
            }

            let api_key = SettingsService::default().settings.hubcap_api_key;
            let key_opt = HubcapService::is_valid_key_format(&api_key).then_some(api_key.as_str());
            if let Ok(b) = HubcapService::fetch_raw_manifest(depot_id, gid, key_opt) {
                send_bytes(req, 200, "application/octet-stream", b);
                return;
            }
        }

        send_json(
            req,
            200,
            json!({ "success": false, "ok": false, "depot_id": depot_id, "gid": gid, "cached": false }),
        );
        return;
    }

    // Route: /manifest/:depotid/:gid or /api/manifest/:depotid/:gid (LT-44)
    if (parts.len() >= 3 && parts[0] == "manifest")
        || (parts.len() >= 4 && parts[0] == "api" && parts[1] == "manifest")
    {
        let offset = if parts[0] == "api" { 2 } else { 1 };
        let depot_id = parts[offset].parse::<u32>().unwrap_or(0);
        let gid = parts
            .get(offset + 1)
            .and_then(|g| g.parse::<u64>().ok())
            .unwrap_or(0);

        if depot_id > 0 && gid > 0 {
            if let Some(home) = dirs::home_dir() {
                let cache_candidates = [
                    home.join(format!(
                        ".local/share/luatools/depotcache/{}_{}.manifest",
                        depot_id, gid
                    )),
                    home.join(format!(
                        ".local/share/Steam/depotcache/{}_{}.manifest",
                        depot_id, gid
                    )),
                    stplug_dir.join(format!("{}_{}.manifest", depot_id, gid)),
                ];
                for p in cache_candidates {
                    if let Ok(b) = std::fs::read(&p) {
                        send_bytes(req, 200, "application/octet-stream", b);
                        return;
                    }
                }
            }

            let api_key = SettingsService::default().settings.hubcap_api_key;
            let key_opt = HubcapService::is_valid_key_format(&api_key).then_some(api_key.as_str());
            if let Ok(b) = HubcapService::fetch_raw_manifest(depot_id, gid, key_opt) {
                send_bytes(req, 200, "application/octet-stream", b);
                return;
            }
        }

        send_json(
            req,
            200,
            json!({ "success": false, "ok": false, "depot_id": depot_id, "gid": gid, "cached": false }),
        );
        return;
    }

    send_json(req, 404, json!({ "success": false, "error": "Not found" }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn get_free_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    }

    #[test]
    fn serves_ping_and_rpc_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let port = get_free_port();
        let mut srv = HttpServerService::start(port, dir.path().to_path_buf(), tx, None).unwrap();

        let ping: Value = ureq::get(&format!("http://127.0.0.1:{}/ping", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(ping["ok"], true);
        assert_eq!(ping["success"], true);

        let rpc: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .send_json(json!({ "method": "HasLuaToolsForApp", "args": { "appid": 1245620 } }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rpc["success"], true);
        assert_eq!(rpc["exists"], false);

        // Test AddViaLuaTools
        let rpc_add: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .send_json(json!({ "method": "AddViaLuaTools", "args": { "appid": 1245620 } }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rpc_add["success"], true);
        assert_eq!(rpc_add["queued"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::AddApp(1245620)));

        // Test GetAddViaLuaToolsStatus & GetDownloadStatus
        let rpc_st: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .send_json(json!({ "method": "GetAddViaLuaToolsStatus", "args": { "appid": 1245620 } }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rpc_st["success"], true);
        assert!(rpc_st.get("result").is_some());

        // Test CancelDownload
        let rpc_cancel: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .send_json(json!({ "method": "CancelDownload", "args": { "appid": 1245620 } }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rpc_cancel["success"], true);
        assert_eq!(rpc_cancel["cancelled"], true);

        // Test CheckSources
        let rpc_src: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .send_json(json!({ "method": "CheckSources", "args": { "appid": 1245620 } }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rpc_src["success"], true);
        assert!(rpc_src.get("sources").is_some());

        // Test OpenFix
        let rpc_fix: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .send_json(json!({ "method": "OpenFix", "args": { "appid": 1245620 } }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rpc_fix["success"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::OpenFix(1245620)));

        // Test RestoreFromTray RPC
        let rpc_restore: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .send_json(json!({ "method": "RestoreFromTray", "args": {} }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rpc_restore["success"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::RestoreFromTray));

        // Test CheckUpdates & GetLoadedApps RPC
        let rpc_up: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .send_json(json!({ "method": "CheckUpdates", "args": {} }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rpc_up["success"], true);
        assert_eq!(rpc_up["currentVersion"], crate::VERSION);

        // Unknown RPC method returns 400 with success: false
        let unknown = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .send_json(json!({ "method": "NoSuchMethod", "args": {} }));
        assert!(matches!(unknown, Err(ureq::Error::Status(400, _))));

        srv.stop();
    }

    #[test]
    fn handles_cors_preflight_and_headers() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::channel();
        let port = get_free_port();
        let mut srv = HttpServerService::start(port, dir.path().to_path_buf(), tx, None).unwrap();

        // 1. Authorized Steam origin receives specific origin and PNA header
        let resp = ureq::request("OPTIONS", &format!("http://127.0.0.1:{}/rpc", port))
            .set("Origin", "https://store.steampowered.com")
            .set("Access-Control-Request-Method", "POST")
            .set(
                "Access-Control-Request-Headers",
                "Content-Type, X-Requested-With",
            )
            .call()
            .unwrap();
        assert_eq!(resp.status(), 204);
        assert_eq!(
            resp.header("Access-Control-Allow-Origin"),
            Some("https://store.steampowered.com")
        );
        assert!(resp
            .header("Access-Control-Allow-Methods")
            .unwrap()
            .contains("OPTIONS"));
        assert!(resp
            .header("Access-Control-Allow-Headers")
            .unwrap()
            .contains("Content-Type"));
        assert_eq!(
            resp.header("Access-Control-Allow-Private-Network"),
            Some("true")
        );

        // 2. Disallowed origin is rejected with 403 and receives no CORS/PNA grants
        let blocked = ureq::request("OPTIONS", &format!("http://127.0.0.1:{}/rpc", port))
            .set("Origin", "https://malicious-attacker.com")
            .call();
        assert!(matches!(blocked, Err(ureq::Error::Status(403, _))));

        // 3. Sensitive routes reject unauthorized web origins
        let bad_rpc = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("Origin", "https://malicious-attacker.com")
            .send_json(json!({ "method": "Ping" }));
        assert!(matches!(bad_rpc, Err(ureq::Error::Status(403, _))));

        let bad_restart = ureq::post(&format!("http://127.0.0.1:{}/restart-steam", port))
            .set("Origin", "https://malicious-attacker.com")
            .send_json(json!({}));
        assert!(matches!(bad_restart, Err(ureq::Error::Status(403, _))));

        let bad_open_url = ureq::post(&format!("http://127.0.0.1:{}/open-url", port))
            .set("Origin", "https://malicious-attacker.com")
            .send_json(json!({ "url": "https://store.steampowered.com" }));
        assert!(matches!(bad_open_url, Err(ureq::Error::Status(403, _))));

        let bad_add = ureq::get(&format!("http://127.0.0.1:{}/add/1245620", port))
            .set("Origin", "https://malicious-attacker.com")
            .call();
        assert!(matches!(bad_add, Err(ureq::Error::Status(403, _))));

        // 4. SSRF defense on /check-sources
        let ssrf_check = ureq::post(&format!("http://127.0.0.1:{}/check-sources/1245620", port))
            .send_json(json!({ "backendUrl": "http://169.254.169.254/latest/meta-data" }));
        assert!(matches!(ssrf_check, Err(ureq::Error::Status(400, _))));

        srv.stop();
    }

    #[test]
    fn handles_store_plugin_and_extended_routes() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let port = get_free_port();
        let mut srv = HttpServerService::start(port, dir.path().to_path_buf(), tx, None).unwrap();

        // 1. /has
        let has1: Value = ureq::get(&format!("http://127.0.0.1:{}/has/1245620", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(has1["has"], false);
        assert_eq!(has1["success"], true);

        let has2: Value = ureq::get(&format!("http://127.0.0.1:{}/has?appid=1245620", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(has2["has"], false);

        // 2. /add
        let add1: Value = ureq::get(&format!("http://127.0.0.1:{}/add/1245620", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(add1["queued"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::AddApp(1245620)));

        // Create a dummy .lua and .lua.disabled
        std::fs::write(dir.path().join("1245620.lua"), "addappid(1245620)").unwrap();
        std::fs::write(dir.path().join("1245620.lua.disabled"), "addappid(1245620)").unwrap();
        let has3: Value = ureq::get(&format!("http://127.0.0.1:{}/has/1245620", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(has3["has"], true);

        // 3. /apps / /installed
        let apps: Value = ureq::get(&format!("http://127.0.0.1:{}/apps", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(apps["apps"], json!([1245620]));
        assert_eq!(apps["success"], true);

        // 4. /remove (POST required to prevent GET CSRF mutations) & removes .lua + .lua.disabled
        let rem_get = ureq::get(&format!("http://127.0.0.1:{}/remove/1245620", port)).call();
        assert!(matches!(rem_get, Err(ureq::Error::Status(405, _))));

        let rem: Value = ureq::post(&format!("http://127.0.0.1:{}/remove/1245620", port))
            .send_json(json!({}))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rem["removed"], true);
        assert_eq!(rem["removedLua"], true);
        assert_eq!(rem["removedDisabled"], true);
        assert!(!dir.path().join("1245620.lua").exists());
        assert!(!dir.path().join("1245620.lua.disabled").exists());
        assert!(matches!(
            rx.try_recv().unwrap(),
            RpcUiEvent::RemovedApp(1245620)
        ));

        // Also test DeleteLuaToolsForApp RPC cleans up .lua.disabled
        std::fs::write(dir.path().join("730.lua"), "addappid(730)").unwrap();
        std::fs::write(dir.path().join("730.lua.disabled"), "addappid(730)").unwrap();
        let rpc_del: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .send_json(json!({ "method": "DeleteLuaToolsForApp", "args": { "appid": 730 } }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rpc_del["success"], true);
        assert_eq!(rpc_del["removedDisabled"], true);
        assert!(!dir.path().join("730.lua.disabled").exists());
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::RemovedApp(730)));

        // 5. /open/fix and /open/settings
        let fix: Value = ureq::get(&format!("http://127.0.0.1:{}/open/fix/1245620", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(fix["ok"], true);
        assert_eq!(fix["success"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::OpenFix(1245620)));

        let set: Value = ureq::get(&format!("http://127.0.0.1:{}/open/settings", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(set["ok"], true);
        assert_eq!(set["success"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::OpenSettings));

        // 6. Extended routes: /version, /status, /config, /info/1245620
        let ver: Value = ureq::get(&format!("http://127.0.0.1:{}/version", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(ver["ok"], true);
        assert_eq!(ver["success"], true);

        let st: Value = ureq::get(&format!("http://127.0.0.1:{}/status", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(st["status"], "running");
        assert_eq!(st["success"], true);

        let cfg: Value = ureq::get(&format!("http://127.0.0.1:{}/config", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(cfg["ok"], true);
        assert_eq!(cfg["success"], true);

        let inf: Value = ureq::get(&format!("http://127.0.0.1:{}/info/1245620", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(inf["ok"], true);
        assert_eq!(inf["success"], true);

        // 7. Raw manifest routes: /manifest/:depotid/:gid & /api/givemethemanifestpunk
        std::fs::write(
            dir.path().join("1245621_987654321.manifest"),
            b"MANIFEST_BYTES_SAMPLE",
        )
        .unwrap();
        let mf_resp =
            ureq::get(&format!("http://127.0.0.1:{}/manifest/1245621/987654321", port))
                .call()
                .unwrap();
        assert_eq!(mf_resp.status(), 200);
        let mut mf_bytes = Vec::new();
        mf_resp.into_reader().read_to_end(&mut mf_bytes).unwrap();
        assert_eq!(mf_bytes, b"MANIFEST_BYTES_SAMPLE");

        let punk_resp = ureq::get(&format!(
            "http://127.0.0.1:{}/api/givemethemanifestpunk?depotid=1245621&gid=987654321",
            port
        ))
        .call()
        .unwrap();
        assert_eq!(punk_resp.status(), 200);
        let mut punk_bytes = Vec::new();
        punk_resp.into_reader().read_to_end(&mut punk_bytes).unwrap();
        assert_eq!(punk_bytes, b"MANIFEST_BYTES_SAMPLE");

        // 8. 13 Extended LTSP routes verification
        let add_st: Value = ureq::get(&format!("http://127.0.0.1:{}/add-status/1245620", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(add_st["success"], true);

        let add_src: Value = ureq::post(&format!("http://127.0.0.1:{}/add-source/1245620", port))
            .send_json(json!({ "source": "Sadie (Morrenus)" }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(add_src["success"], true);

        let chk_src: Value = ureq::get(&format!("http://127.0.0.1:{}/check-sources/1245620", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(chk_src["success"], true);

        let dl: Value = ureq::post(&format!("http://127.0.0.1:{}/download/1245620", port))
            .send_json(json!({ "source": "Sadie (Morrenus)" }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(dl["success"], true);

        // Cancel sets real cancelled state reflected in /download-status
        let cancel: Value = ureq::post(&format!("http://127.0.0.1:{}/cancel/1245620", port))
            .send_json(json!({}))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(cancel["success"], true);

        let dl_st: Value = ureq::get(&format!("http://127.0.0.1:{}/download-status/1245620", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(dl_st["success"], true);
        assert_eq!(dl_st["state"]["status"], "cancelled");
        assert_eq!(dl_st["state"]["cancelled"], true);

        // Reserved example.com and non-HTTP schemes must be rejected cleanly
        let blocked_example = ureq::post(&format!("http://127.0.0.1:{}/open-url", port))
            .send_json(json!({ "url": "https://example.com" }));
        assert!(matches!(blocked_example, Err(ureq::Error::Status(400, _))));

        let blocked_file = ureq::post(&format!("http://127.0.0.1:{}/open-url", port))
            .send_json(json!({ "url": "file:///etc/passwd" }));
        assert!(matches!(blocked_file, Err(ureq::Error::Status(400, _))));

        let open_url: Value = ureq::post(&format!("http://127.0.0.1:{}/open-url", port))
            .send_json(json!({ "url": "https://store.steampowered.com/app/1245620" }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(open_url["success"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::RestoreFromTray));

        let r_steam: Value = ureq::post(&format!("http://127.0.0.1:{}/restart-steam", port))
            .send_json(json!({}))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(r_steam["success"], true);

        let chk_up: Value = ureq::post(&format!("http://127.0.0.1:{}/check-updates", port))
            .send_json(json!({}))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(chk_up["success"], true);
        assert_eq!(chk_up["currentVersion"], crate::VERSION);

        // POST /loaded-apps parses JSON app list and merges with stplug-in
        std::fs::write(dir.path().join("570.lua"), "addappid(570)").unwrap();
        let l_apps_post: Value = ureq::post(&format!("http://127.0.0.1:{}/loaded-apps", port))
            .send_json(json!({ "apps": [440, "620"] }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(l_apps_post["success"], true);
        assert_eq!(l_apps_post["received"], 2);
        let merged_arr = l_apps_post["apps"].as_array().unwrap();
        assert!(merged_arr.contains(&json!(440)));
        assert!(merged_arr.contains(&json!(570)));
        assert!(merged_arr.contains(&json!(620)));

        let l_apps_get: Value = ureq::get(&format!("http://127.0.0.1:{}/loaded-apps", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(l_apps_get["success"], true);
        assert!(l_apps_get["apps"].as_array().unwrap().contains(&json!(570)));

        let api_l: Value = ureq::get(&format!("http://127.0.0.1:{}/api-list", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(api_l["success"], true);

        let icon: Value = ureq::get(&format!("http://127.0.0.1:{}/icon", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(icon["success"], false);

        // 9. /open/library and /open/depots emit RpcUiEvent
        let lib_resp: Value = ureq::get(&format!("http://127.0.0.1:{}/open/library", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(lib_resp["success"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::OpenLibrary));

        let depots_resp: Value = ureq::get(&format!("http://127.0.0.1:{}/open/depots/1245620", port))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(depots_resp["success"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::OpenDepots(1245620)));

        // 10. /steam/restart and /restart enforce POST and reject GET with 405
        let get_steam_restart = ureq::get(&format!("http://127.0.0.1:{}/steam/restart", port)).call();
        assert!(matches!(get_steam_restart, Err(ureq::Error::Status(405, _))));

        let post_steam_restart: Value = ureq::post(&format!("http://127.0.0.1:{}/steam/restart", port))
            .send_json(json!({}))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(post_steam_restart["success"], true);

        let get_restart = ureq::get(&format!("http://127.0.0.1:{}/restart", port)).call();
        assert!(matches!(get_restart, Err(ureq::Error::Status(405, _))));

        let post_restart: Value = ureq::post(&format!("http://127.0.0.1:{}/restart", port))
            .send_json(json!({}))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(post_restart["success"], true);

        // 11. Gated routes reject unauthorized web origins
        let bad_loaded_apps = ureq::post(&format!("http://127.0.0.1:{}/loaded-apps", port))
            .set("Origin", "https://untrusted-site.com")
            .send_json(json!({ "apps": [123] }));
        assert!(matches!(bad_loaded_apps, Err(ureq::Error::Status(403, _))));

        let bad_lib = ureq::get(&format!("http://127.0.0.1:{}/open/library", port))
            .set("Origin", "https://untrusted-site.com")
            .call();
        assert!(matches!(bad_lib, Err(ureq::Error::Status(403, _))));

        let bad_depots = ureq::get(&format!("http://127.0.0.1:{}/open/depots/1245620", port))
            .set("Origin", "https://untrusted-site.com")
            .call();
        assert!(matches!(bad_depots, Err(ureq::Error::Status(403, _))));

        let bad_app = ureq::get(&format!("http://127.0.0.1:{}/open/app/1245620", port))
            .set("Origin", "https://untrusted-site.com")
            .call();
        assert!(matches!(bad_app, Err(ureq::Error::Status(403, _))));

        let bad_acq = ureq::get(&format!("http://127.0.0.1:{}/open/acquire/1245620", port))
            .set("Origin", "https://untrusted-site.com")
            .call();
        assert!(matches!(bad_acq, Err(ureq::Error::Status(403, _))));

        let bad_fix = ureq::get(&format!("http://127.0.0.1:{}/open/fix/1245620", port))
            .set("Origin", "https://untrusted-site.com")
            .call();
        assert!(matches!(bad_fix, Err(ureq::Error::Status(403, _))));

        let bad_settings = ureq::get(&format!("http://127.0.0.1:{}/open/settings", port))
            .set("Origin", "https://untrusted-site.com")
            .call();
        assert!(matches!(bad_settings, Err(ureq::Error::Status(403, _))));

        let bad_chk_updates = ureq::get(&format!("http://127.0.0.1:{}/check-updates", port))
            .set("Origin", "https://untrusted-site.com")
            .call();
        assert!(matches!(bad_chk_updates, Err(ureq::Error::Status(403, _))));

        let bad_chk_src = ureq::get(&format!("http://127.0.0.1:{}/check-sources/1245620", port))
            .set("Origin", "https://untrusted-site.com")
            .call();
        assert!(matches!(bad_chk_src, Err(ureq::Error::Status(403, _))));

        srv.stop();
    }

    #[test]
    fn remove_app_completely_cleans_disabled_lua_and_slssteam_entries() {
        let dir = tempfile::tempdir().unwrap();
        let stplug = dir.path().join("stplug-in");
        std::fs::create_dir_all(&stplug).unwrap();

        let key = "ef".repeat(32);
        let lua_content = format!(
            "addappid(1245620)\naddappid(1245621, 1, \"{}\")\nsetManifestid(1245621, \"123456789\", 100)\n",
            key
        );
        std::fs::write(stplug.join("1245620.lua"), &lua_content).unwrap();
        std::fs::write(stplug.join("1245620.lua.disabled"), &lua_content).unwrap();

        let mut sls = SlssteamService::new(Some(dir.path().join("config.yaml")));
        sls.add_additional_app(1245620);
        sls.set_fake_app_id(1245620, 480);
        sls.set_token(1245620, "9999".into());
        sls.set_depot_key(1245621, key);
        sls.set_manifest_override(1245621, "123456789".into());
        assert!(sls.save());

        let (rem_lua, rem_dis) =
            remove_app_completely_with_sls(&stplug, 1245620, Some(&mut sls));
        assert!(rem_lua);
        assert!(rem_dis);
        assert!(!stplug.join("1245620.lua").exists());
        assert!(!stplug.join("1245620.lua.disabled").exists());
        assert!(!sls.config.additional_apps.contains(&1245620));
        assert!(sls.get_fake_app_id(1245620).is_none());
        assert!(sls.get_token(1245620).is_none());
        assert!(!sls.config.depot_keys.contains_key(&1245621));
        assert!(sls.get_manifest_override(1245621).is_none());
    }
}
