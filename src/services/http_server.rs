//! Local HTTP/RPC server (`127.0.0.1:6767`) for injected Steam Store JS & CEF CDP polyfill
//! (LT-26, LT-27, LT-28).

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
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

static ACTIVE_SESSION_TOKEN: OnceLock<RwLock<String>> = OnceLock::new();

pub fn active_session_token() -> &'static RwLock<String> {
    ACTIVE_SESSION_TOKEN.get_or_init(|| RwLock::new(String::new()))
}

pub fn generate_session_token() -> String {
    let mut buf = [0u8; 16];
    #[cfg(unix)]
    {
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            if f.read_exact(&mut buf).is_ok() {
                return hex::encode(buf);
            }
        }
    }
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    format!("{:016x}{:08x}{:08x}", now, pid, 0x5a5a5a5au32)
}

#[derive(Clone, Default)]
struct HttpServerConfigOverrides {
    slssteam_config_path: Option<PathBuf>,
    settings_config_path: Option<PathBuf>,
}

pub struct HttpServerService {
    pub port: u16,
    pub requested_port: u16,
    pub rpc_token: String,
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
        #[cfg(test)]
        let (sls_path, settings_path) = (
            Some(stplug_dir.join(".test_slssteam_config.yaml")),
            Some(stplug_dir.join(".test_settings.json")),
        );
        #[cfg(not(test))]
        let (sls_path, settings_path) = (None, None);

        Self::start_with_config_paths(port, stplug_dir, sls_path, settings_path, event_tx, wakeup)
    }

    pub fn start_with_config_paths(
        port: u16,
        stplug_dir: PathBuf,
        slssteam_config_path: Option<PathBuf>,
        settings_config_path: Option<PathBuf>,
        event_tx: Sender<RpcUiEvent>,
        wakeup: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Option<Self> {
        let (server, actual_port) = match Server::http(("127.0.0.1", port)) {
            Ok(s) => {
                let actual = s.server_addr().to_ip().map(|a| a.port()).unwrap_or(port);
                (s, actual)
            }
            Err(e) => {
                PluginLog::global().warn(
                    Some("HttpServer"),
                    &format!(
                        "Port {} occupied or unavailable ({}), falling back to ephemeral port 0",
                        port, e
                    ),
                );
                let s = Server::http(("127.0.0.1", 0)).ok()?;
                let actual = s.server_addr().to_ip().map(|a| a.port()).unwrap_or(0);
                (s, actual)
            }
        };
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop_flag);

        PluginLog::global().info(
            Some("HttpServer"),
            &format!("Listening on http://127.0.0.1:{}", actual_port),
        );

        let token = generate_session_token();
        write_rpc_discovery_file(actual_port, Some(&token));
        let server_token = token.clone();
        let overrides = HttpServerConfigOverrides {
            slssteam_config_path,
            settings_config_path,
        };

        let handle = thread::spawn(move || {
            while !stop_clone.load(Ordering::Relaxed) {
                if let Ok(Some(req)) = server.recv_timeout(Duration::from_millis(250)) {
                    handle_request(
                        req,
                        &stplug_dir,
                        &event_tx,
                        &wakeup,
                        &server_token,
                        &overrides,
                    );
                }
            }
        });

        Some(Self {
            port: actual_port,
            requested_port: port,
            rpc_token: token,
            stop_flag,
            handle: Some(handle),
        })
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        remove_rpc_discovery_file_for(self.port, &self.rpc_token);
        if let Ok(mut guard) = active_session_token().write() {
            if guard.as_str() == self.rpc_token {
                guard.clear();
            }
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

static DISCOVERY_FILE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn discovery_file_lock() -> &'static Mutex<()> {
    DISCOVERY_FILE_LOCK.get_or_init(|| Mutex::new(()))
}

fn discovery_file_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        if !xdg.trim().is_empty() {
            paths.push(PathBuf::from(xdg.trim()).join("luatools_rpc.json"));
        }
    }
    #[cfg(unix)]
    {
        let uid = unsafe { libc::getuid() };
        paths.push(PathBuf::from(format!("/run/user/{}/luatools_rpc.json", uid)));
    }
    paths.push(std::env::temp_dir().join("luatools_rpc.json"));
    paths
}

fn discovery_file_matches(
    path: &Path,
    expected_pid: u32,
    expected_port: Option<u16>,
    expected_token: Option<&str>,
) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(val) = serde_json::from_str::<Value>(&raw) else {
        return false;
    };
    let pid_ok = val
        .get("pid")
        .and_then(|v| v.as_u64())
        .and_then(|p| u32::try_from(p).ok())
        == Some(expected_pid);
    if !pid_ok {
        return false;
    }
    if let Some(exp_port) = expected_port {
        let port_ok = val
            .get("port")
            .and_then(|v| v.as_u64())
            .and_then(|p| u16::try_from(p).ok())
            == Some(exp_port);
        if !port_ok {
            return false;
        }
    }
    if let Some(exp_tok) = expected_token {
        let tok_ok = val.get("token").and_then(|v| v.as_str()) == Some(exp_tok);
        if !tok_ok {
            return false;
        }
    }
    true
}

fn write_rpc_discovery_file_in(dir: &Path, port: u16, tok: &str) -> Option<PathBuf> {
    let _lock = discovery_file_lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = dir.join("luatools_rpc.json");
    let pid = std::process::id();
    let payload = json!({
        "port": port,
        "url": format!("http://127.0.0.1:{}", port),
        "pid": pid,
        "version": crate::VERSION,
        "token": tok
    });
    let payload_str = payload.to_string();
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::fs::PermissionsExt;
        let tmp = dir.join(format!("luatools_rpc.tmp.{}", pid));
        let _ = std::fs::remove_file(&tmp);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
        {
            if f.write_all(payload_str.as_bytes()).is_ok()
                && f.sync_all().is_ok()
                && std::fs::rename(&tmp, &path).is_ok()
            {
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            } else {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let tmp = dir.join(format!("luatools_rpc.tmp.{}", pid));
        if std::fs::write(&tmp, &payload_str).is_ok() {
            if std::fs::rename(&tmp, &path).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }
    Some(path)
}

fn remove_rpc_discovery_files_checked(
    paths: &[PathBuf],
    expected_pid: u32,
    expected_port: Option<u16>,
    expected_token: Option<&str>,
) {
    let _lock = discovery_file_lock().lock().unwrap_or_else(|e| e.into_inner());
    for p in paths {
        if discovery_file_matches(p, expected_pid, expected_port, expected_token) {
            let _ = std::fs::remove_file(p);
        }
    }
}

pub fn write_rpc_discovery_file(port: u16, token: Option<&str>) -> Option<PathBuf> {
    let tok = token.map(ToString::to_string).unwrap_or_else(generate_session_token);
    if let Ok(mut guard) = active_session_token().write() {
        *guard = tok.clone();
    }
    let mut target_dir = None;
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        if !xdg.trim().is_empty() {
            target_dir = Some(PathBuf::from(xdg.trim()));
        }
    }
    if target_dir.is_none() {
        #[cfg(unix)]
        {
            let uid = unsafe { libc::getuid() };
            let run_user = PathBuf::from(format!("/run/user/{}", uid));
            if run_user.is_dir() {
                target_dir = Some(run_user);
            }
        }
    }
    let dir = target_dir.unwrap_or_else(std::env::temp_dir);
    write_rpc_discovery_file_in(&dir, port, &tok)
}

pub fn remove_rpc_discovery_file_for(port: u16, token: &str) {
    let paths = discovery_file_candidates();
    let tok_opt = (!token.is_empty()).then_some(token);
    remove_rpc_discovery_files_checked(&paths, std::process::id(), Some(port), tok_opt);
}

pub fn remove_rpc_discovery_file() {
    let active_tok = active_session_token()
        .read()
        .ok()
        .map(|g| g.clone())
        .filter(|t| !t.is_empty());
    let paths = discovery_file_candidates();
    remove_rpc_discovery_files_checked(&paths, std::process::id(), None, active_tok.as_deref());
    if let Ok(mut guard) = active_session_token().write() {
        guard.clear();
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

/// Validates and canonicalizes an incoming HTTP request `Origin` header into a safe ASCII
/// `scheme://host[:port]` string if permitted to access local resources via CORS/PNA.
/// Only official Steam domains (HTTPS) and local loopback origins are permitted.
pub fn canonical_allowed_origin(origin: &str) -> Option<String> {
    let trimmed = origin.trim();
    if trimmed.is_empty()
        || trimmed
            .bytes()
            .any(|b| !b.is_ascii() || b < 0x20 || b == 0x7f)
    {
        return None;
    }
    let parsed = url::Url::parse(trimmed).ok()?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || (parsed.path() != "/" && !parsed.path().is_empty())
    {
        return None;
    }
    let host = parsed.host_str()?;
    let host_lower = host.to_ascii_lowercase();

    let mut allowed = false;

    // Permitted Steam web domains (strictly HTTPS)
    if parsed.scheme() == "https"
        && (host_lower == "store.steampowered.com"
            || host_lower == "steamcommunity.com"
            || host_lower == "steamloopback.host"
            || host_lower.ends_with(".steampowered.com")
            || host_lower.ends_with(".steamcommunity.com"))
    {
        allowed = true;
    }

    // Permitted local loopback origins
    if !allowed && matches!(parsed.scheme(), "http" | "https") {
        let unbracketed = host_lower.trim_matches(|c| c == '[' || c == ']');
        if unbracketed == "localhost" || unbracketed == "127.0.0.1" || unbracketed == "::1" {
            allowed = true;
        } else if let Ok(ip) = unbracketed.parse::<std::net::IpAddr>() {
            if ip.is_loopback() {
                allowed = true;
            }
        }
    }

    if !allowed {
        return None;
    }

    let canon = parsed.origin().ascii_serialization();
    if canon == "null"
        || canon.is_empty()
        || canon
            .bytes()
            .any(|b| !b.is_ascii() || b < 0x20 || b == 0x7f)
    {
        return None;
    }
    Some(canon)
}

/// Validates whether an incoming HTTP request `Origin` header is permitted
/// to access local resources via CORS/PNA. Only official Steam domains (HTTPS)
/// and local loopback origins are permitted.
pub fn is_allowed_origin(origin: &str) -> bool {
    canonical_allowed_origin(origin).is_some()
}

/// Validates the `Host` header to protect local HTTP services from DNS rebinding attacks.
/// Rejects empty `Host` headers and properly parses bracketed IPv6 loopback (`[::1]:6767`).
pub fn is_allowed_host(host_header: &str) -> bool {
    let trimmed = host_header.trim();
    if trimmed.is_empty() {
        return false;
    }
    let host_part = if let Some(rest) = trimmed.strip_prefix('[') {
        let Some(end) = rest.find(']') else {
            return false;
        };
        let after = &rest[end + 1..];
        if !after.is_empty() {
            let Some(port_str) = after.strip_prefix(':') else {
                return false;
            };
            if port_str.is_empty() || port_str.parse::<u16>().is_err() {
                return false;
            }
        }
        &rest[..end]
    } else if let Some((h, port_str)) = trimmed.rsplit_once(':') {
        if h.contains(':') {
            // Bare IPv6 address without brackets (e.g. `::1`)
            trimmed
        } else {
            if port_str.is_empty() || port_str.parse::<u16>().is_err() {
                return false;
            }
            h
        }
    } else {
        trimmed
    };

    let host_lower = host_part.trim().to_ascii_lowercase();
    if host_lower == "127.0.0.1" || host_lower == "localhost" || host_lower == "::1" {
        return true;
    }
    if let Ok(ip) = host_lower.parse::<std::net::IpAddr>() {
        if ip.is_loopback() {
            return true;
        }
    }
    false
}

fn requires_token_for_request(_method: &Method, url: &str) -> bool {
    let path_only = url.split('?').next().unwrap_or(url);
    let parts: Vec<&str> = path_only.split('/').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return false;
    }
    parts == ["rpc"]
        || parts.first() == Some(&"add")
        || (parts.len() == 2 && parts[0] == "api" && parts[1] == "add")
        || parts.first() == Some(&"remove")
        || parts.first() == Some(&"delete")
        || (parts.len() == 2
            && parts[0] == "api"
            && (parts[1] == "remove" || parts[1] == "delete"))
        || parts.first() == Some(&"cancel")
        || parts == ["open-url"]
        || parts == ["restart-steam"]
        || parts == ["steam", "restart"]
        || parts == ["restart"]
        || parts.first() == Some(&"download")
        || parts.first() == Some(&"add-source")
        || parts == ["loaded-apps"]
        || parts.first() == Some(&"open")
        || parts.first() == Some(&"fix")
        || parts == ["settings"]
        || parts == ["library"]
}

fn is_request_host_and_origin_allowed(req: &Request) -> bool {
    // 1. Defend against DNS rebinding and reject missing/empty Host headers
    match get_request_header(req, "Host") {
        Some(host) if is_allowed_host(host) => {}
        _ => return false,
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

/// Checks whether an incoming HTTP request is authorized to invoke sensitive operations.
/// Validates `Host`, `Origin`, and `Sec-Fetch-Site` headers, allows `OPTIONS` CORS preflight,
/// and strictly requires a valid `X-LuaTools-Token` header on mutating/privileged routes.
pub fn is_request_authorized(req: &Request) -> bool {
    let active = active_session_token()
        .read()
        .map(|g| g.clone())
        .unwrap_or_default();
    is_request_authorized_with_token(req, &active)
}

/// Checks request authorization against an explicit per-server session token.
pub fn is_request_authorized_with_token(req: &Request, expected_token: &str) -> bool {
    if !is_request_host_and_origin_allowed(req) {
        return false;
    }

    // 4. Allow CORS preflight requests cleanly (they cannot carry custom auth headers)
    if req.method() == &Method::Options {
        return true;
    }

    // 5. Strict token validation on all mutating / privileged routes; validate token if present on others
    if requires_token_for_request(req.method(), req.url()) {
        match get_request_header(req, "X-LuaTools-Token") {
            Some(tok_header) if !expected_token.is_empty() && tok_header == expected_token => {}
            _ => return false,
        }
    } else if let Some(tok_header) = get_request_header(req, "X-LuaTools-Token") {
        if !expected_token.is_empty() && tok_header != expected_token {
            return false;
        }
    }

    true
}

fn add_cors_headers<R: Read>(mut resp: Response<R>, allowed_origin: &str) -> Response<R> {
    let pairs = [
        ("Access-Control-Allow-Origin", allowed_origin),
        ("Vary", "Origin"),
        (
            "Access-Control-Allow-Methods",
            "GET, POST, PUT, DELETE, OPTIONS, HEAD",
        ),
        (
            "Access-Control-Allow-Headers",
            "Content-Type, Authorization, X-Requested-With, Accept, Origin, X-LuaTools-Token",
        ),
        ("Access-Control-Max-Age", "86400"),
        ("Access-Control-Allow-Private-Network", "true"),
    ];
    for (k, v) in pairs {
        if let Ok(hdr) = Header::from_bytes(k, v) {
            resp = resp.with_header(hdr);
        }
    }
    resp
}

fn send_json(req: Request, status: u16, payload: Value) {
    let allowed_origin =
        get_request_header(&req, "Origin").and_then(canonical_allowed_origin);
    let body = payload.to_string();
    let mut resp = Response::from_string(body).with_status_code(status);
    if let Ok(hdr) = Header::from_bytes("Content-Type", "application/json") {
        resp = resp.with_header(hdr);
    }

    if let Some(ref orig) = allowed_origin {
        resp = add_cors_headers(resp, orig);
    }

    let _ = req.respond(resp);
}

fn send_bytes(req: Request, status: u16, content_type: &str, bytes: Vec<u8>) {
    let allowed_origin =
        get_request_header(&req, "Origin").and_then(canonical_allowed_origin);
    let mut resp = Response::from_data(bytes).with_status_code(status);
    if let Ok(hdr) = Header::from_bytes("Content-Type", content_type) {
        resp = resp.with_header(hdr);
    }

    if let Some(ref orig) = allowed_origin {
        resp = add_cors_headers(resp, orig);
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

fn collect_merged_loaded_apps(
    stplug_dir: &Path,
    slssteam_config_path: Option<&Path>,
) -> Vec<u32> {
    let mut merged: BTreeSet<u32> = list_installed_apps(stplug_dir).into_iter().collect();
    if let Ok(guard) = reported_loaded_apps().lock() {
        merged.extend(guard.iter().copied());
    }
    if slssteam_config_path.is_some() || allow_external_spawn() {
        let sls = SlssteamService::new(slssteam_config_path.map(Path::to_path_buf));
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

fn remove_app_completely(
    stplug_dir: &Path,
    aid: u32,
    slssteam_config_path: Option<&Path>,
) -> (bool, bool) {
    if slssteam_config_path.is_some() || allow_external_spawn() {
        let mut sls = SlssteamService::new(slssteam_config_path.map(Path::to_path_buf));
        remove_app_completely_with_sls(stplug_dir, aid, Some(&mut sls))
    } else {
        remove_app_completely_with_sls(stplug_dir, aid, None)
    }
}

fn toggle_app_enabled(
    stplug_dir: &Path,
    aid: u32,
    target_enabled: Option<bool>,
    slssteam_config_path: Option<&Path>,
) -> Result<bool, (u16, String)> {
    let lua_path = stplug_dir.join(format!("{}.lua", aid));
    let disabled_path = stplug_dir.join(format!("{}.lua.disabled", aid));

    if !lua_path.exists() && !disabled_path.exists() {
        return Err((
            404,
            format!(
                "No Lua script found for AppID {} (neither {}.lua nor {}.lua.disabled exists)",
                aid, aid, aid
            ),
        ));
    }

    let should_enable = match target_enabled {
        Some(enabled) => enabled,
        None => disabled_path.exists(),
    };

    if should_enable {
        if disabled_path.exists() {
            std::fs::rename(&disabled_path, &lua_path)
                .map_err(|e| (500, format!("Failed to enable AppID {}: {}", aid, e)))?;
        }
        if slssteam_config_path.is_some() || allow_external_spawn() {
            let mut sls = SlssteamService::new(slssteam_config_path.map(Path::to_path_buf));
            sls.add_additional_app(aid);
            let _ = sls.save();
        }
        Ok(true)
    } else {
        if lua_path.exists() {
            std::fs::rename(&lua_path, &disabled_path)
                .map_err(|e| (500, format!("Failed to disable AppID {}: {}", aid, e)))?;
        }
        if slssteam_config_path.is_some() || allow_external_spawn() {
            let mut sls = SlssteamService::new(slssteam_config_path.map(Path::to_path_buf));
            sls.remove_additional_app(aid);
            let _ = sls.save();
        }
        Ok(false)
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

fn sanitized_settings_json(settings_path: Option<&Path>) -> Value {
    let s = SettingsService::new(settings_path.map(Path::to_path_buf)).settings;
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
    rpc_token: &str,
    overrides: &HttpServerConfigOverrides,
) {
    // 1. CORS preflight handling (LT-26)
    if req.method() == &Method::Options {
        let host_ok = get_request_header(&req, "Host")
            .map(is_allowed_host)
            .unwrap_or(false);
        if host_ok {
            if let Some(orig) = get_request_header(&req, "Origin") {
                if let Some(canon_orig) = canonical_allowed_origin(orig) {
                    let resp = add_cors_headers(Response::empty(204), &canon_orig);
                    let _ = req.respond(resp);
                    return;
                }
            }
        }
        // Disallowed origin/host or missing origin receives 403 Forbidden without CORS/PNA grant
        let resp = Response::empty(403);
        let _ = req.respond(resp);
        return;
    }

    // 2. Enforce Host, Origin, Sec-Fetch-Site, and X-LuaTools-Token uniformly across all routes
    if !is_request_authorized_with_token(&req, rpc_token) {
        send_json(
            req,
            403,
            json!({
                "success": false,
                "error": "Unauthorized origin, host, or missing/invalid X-LuaTools-Token"
            }),
        );
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
        let Some(payload) = body_json else {
            send_json(
                req,
                400,
                json!({ "success": false, "error": "Invalid JSON" }),
            );
            return;
        };
        let method = payload.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let args = payload
            .get("args")
            .or_else(|| payload.get("params"))
            .cloned()
            .unwrap_or_else(|| json!({}));
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
                    let lua_path = stplug_dir.join(format!("{}.lua", rpc_aid));
                    let disabled_path = stplug_dir.join(format!("{}.lua.disabled", rpc_aid));
                    let exists = lua_path.exists();
                    let disabled = !exists && disabled_path.exists();
                    let parse_target = if exists {
                        Some(&lua_path)
                    } else if disabled {
                        Some(&disabled_path)
                    } else {
                        None
                    };
                    let (depot_count, dlc_count) = parse_target
                        .and_then(|p| std::fs::read_to_string(p).ok())
                        .map(|txt| {
                            let parsed = LuaFileParser::parse_text(&txt, rpc_aid);
                            (parsed.depot_count(), parsed.dlc_count())
                        })
                        .unwrap_or((0, 0));
                    send_json(
                        req,
                        200,
                        json!({
                            "success": true,
                            "exists": exists,
                            "has": exists,
                            "disabled": disabled,
                            "depotCount": depot_count,
                            "dlcCount": dlc_count,
                            "result": exists
                        }),
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
                    let (removed_lua, removed_disabled) = remove_app_completely(
                        stplug_dir,
                        rpc_aid,
                        overrides.slssteam_config_path.as_deref(),
                    );
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
            "OpenFix" | "OpenFixes" | "OpenFixInDesktopApp" => {
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
            "CheckForFixes" => {
                if rpc_aid > 0 {
                    let idx = AppliedFixIndexService::default();
                    let applied_fixes: Vec<Value> = idx
                        .get_fixes(rpc_aid)
                        .map(|list| {
                            list.iter()
                                .filter_map(|r| serde_json::to_value(r).ok())
                                .collect()
                        })
                        .unwrap_or_default();
                    let available_fixes: Vec<Value> = if !cfg!(test) && std::env::var_os("LUATOOLS_OFFLINE").is_none() {
                        let client = crate::services::api_client::LuaToolsApiClient::new(AuthService::default());
                        client
                            .get_drm_fix_fixes(rpc_aid)
                            .into_iter()
                            .map(|f| {
                                json!({
                                    "id": f.id,
                                    "title": f.title,
                                    "description": f.description,
                                    "hasFix": f.has_fix,
                                    "hasManifest": f.has_manifest,
                                    "manifestFilename": f.manifest_filename,
                                    "fixFilename": f.fix_filename,
                                    "tags": f.tags.into_iter().map(|t| json!({
                                        "id": t.id,
                                        "name": t.name,
                                        "slug": t.slug
                                    })).collect::<Vec<_>>()
                                })
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let has_fixes = !applied_fixes.is_empty() || !available_fixes.is_empty();
                    send_json(
                        req,
                        200,
                        json!({
                            "success": true,
                            "appid": rpc_aid,
                            "appliedFixes": applied_fixes,
                            "availableFixes": available_fixes,
                            "hasFixes": has_fixes,
                            "result": {
                                "appid": rpc_aid,
                                "appliedFixes": applied_fixes,
                                "availableFixes": available_fixes,
                                "hasFixes": has_fixes,
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
                let apps = collect_merged_loaded_apps(
                    stplug_dir,
                    overrides.slssteam_config_path.as_deref(),
                );
                send_json(
                    req,
                    200,
                    json!({ "success": true, "result": apps, "apps": apps, "total": apps.len() }),
                );
            }
            "SetLoadedApps" => {
                let incoming = parse_app_id_list(&args);
                let received = incoming.len();
                if !incoming.is_empty() {
                    if let Ok(mut guard) = reported_loaded_apps().lock() {
                        guard.extend(incoming.iter().copied());
                    }
                    if overrides.settings_config_path.is_some() || allow_external_spawn() {
                        let mut ss = SettingsService::new(overrides.settings_config_path.clone());
                        for id in &incoming {
                            ss.record_loaded_app(*id);
                        }
                    }
                }
                let merged = collect_merged_loaded_apps(
                    stplug_dir,
                    overrides.slssteam_config_path.as_deref(),
                );
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
                let cfg = sanitized_settings_json(overrides.settings_config_path.as_deref());
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
                    thread::spawn(|| {
                        let _ = SteamService::relaunch_steam_sls(None);
                    });
                }
                send_json(
                    req,
                    200,
                    json!({
                        "success": true,
                        "restarting": true,
                        "result": { "restarting": true }
                    }),
                );
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
            "ToggleAppEnabled" => {
                if rpc_aid > 0 {
                    let enabled_target = args.get("enabled").and_then(|v| v.as_bool());
                    match toggle_app_enabled(
                        stplug_dir,
                        rpc_aid,
                        enabled_target,
                        overrides.slssteam_config_path.as_deref(),
                    ) {
                        Ok(now_enabled) => {
                            send_json(
                                req,
                                200,
                                json!({
                                    "success": true,
                                    "appid": rpc_aid,
                                    "enabled": now_enabled,
                                    "result": now_enabled
                                }),
                            );
                        }
                        Err((status, err)) => {
                            send_json(
                                req,
                                status,
                                json!({
                                    "success": false,
                                    "appid": rpc_aid,
                                    "error": err
                                }),
                            );
                        }
                    }
                } else {
                    send_json(
                        req,
                        400,
                        json!({ "success": false, "error": "Missing or invalid appid" }),
                    );
                }
            }
            "ApplyRecommendedFixes" => {
                if rpc_aid > 0 {
                    let _ = event_tx.send(RpcUiEvent::OpenFix(rpc_aid));
                    notify();
                    send_json(
                        req,
                        200,
                        json!({
                            "success": true,
                            "appid": rpc_aid,
                            "status": "opened_in_workbench",
                            "result": { "appid": rpc_aid, "status": "opened_in_workbench" }
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
            "RemoveAppliedFixes" => {
                if rpc_aid > 0 {
                    let mut idx = AppliedFixIndexService::default();
                    let removed = idx.remove_app(rpc_aid);
                    send_json(
                        req,
                        200,
                        json!({
                            "success": true,
                            "appid": rpc_aid,
                            "removed": removed,
                            "result": { "appid": rpc_aid, "removed": removed }
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
            "SyncDlcForApp" => {
                if rpc_aid > 0 {
                    let mut dlc_ids = Vec::new();
                    let lua_path = stplug_dir.join(format!("{}.lua", rpc_aid));
                    let disabled_path = stplug_dir.join(format!("{}.lua.disabled", rpc_aid));
                    for p in [&lua_path, &disabled_path] {
                        if let Ok(txt) = std::fs::read_to_string(p) {
                            let parsed = LuaFileParser::parse_text(&txt, rpc_aid);
                            for entry in parsed.entries.iter().chain(parsed.disabled_entries.iter()) {
                                if !entry.has_key()
                                    && entry.id != rpc_aid
                                    && !dlc_ids.contains(&entry.id)
                                {
                                    dlc_ids.push(entry.id);
                                }
                            }
                        }
                    }
                    let mut sls = SlssteamService::new(overrides.slssteam_config_path.clone());
                    for &did in &dlc_ids {
                        sls.add_additional_app(did);
                    }
                    let saved = if !dlc_ids.is_empty() {
                        sls.save()
                    } else {
                        true
                    };
                    send_json(
                        req,
                        200,
                        json!({
                            "success": true,
                            "appid": rpc_aid,
                            "dlcCount": dlc_ids.len(),
                            "dlcIds": dlc_ids,
                            "saved": saved,
                            "result": {
                                "appid": rpc_aid,
                                "dlcCount": dlc_ids.len(),
                                "dlcIds": dlc_ids,
                                "saved": saved
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
            "SyncSlsSteamConfig" => {
                let mut sls = SlssteamService::new(overrides.slssteam_config_path.clone());
                let mut synced_count = 0usize;
                if let Ok(entries) = std::fs::read_dir(stplug_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) == Some("lua") {
                            if let Ok(txt) = std::fs::read_to_string(&path) {
                                let aid = path
                                    .file_stem()
                                    .and_then(|s| s.to_str())
                                    .and_then(|s| s.parse::<u32>().ok())
                                    .or_else(|| LuaFileParser::infer_app_id_from_text(&txt))
                                    .unwrap_or(0);
                                if aid > 0 {
                                    let parsed = LuaFileParser::parse_text(&txt, aid);
                                    sls.add_additional_app(aid);
                                    for item in &parsed.entries {
                                        if item.id != aid {
                                            sls.add_additional_app(item.id);
                                        }
                                        if let Some(ref k) = item.key {
                                            sls.set_depot_key(item.id, k.clone());
                                        }
                                        if let Some(ref m) = item.manifest_id {
                                            sls.set_manifest_override(item.id, m.clone());
                                        }
                                    }
                                    for (depot_id, gid) in &parsed.active_pins {
                                        sls.set_manifest_override(*depot_id, gid.clone());
                                    }
                                    for (app_id, token) in &parsed.tokens {
                                        sls.set_token(*app_id, token.clone());
                                    }
                                    for (map_aid, exe) in &parsed.process_map {
                                        sls.set_process_mapping(*map_aid, exe.clone());
                                    }
                                    for &denuvo_aid in &parsed.force_denuvo {
                                        sls.add_force_denuvo(denuvo_aid);
                                    }
                                    for &pinned_aid in &parsed.pinned_apps {
                                        sls.add_pinned_app(pinned_aid);
                                    }
                                    synced_count += 1;
                                }
                            }
                        }
                    }
                }
                let saved = sls.save();
                send_json(
                    req,
                    200,
                    json!({
                        "success": saved,
                        "saved": saved,
                        "syncedCount": synced_count,
                        "result": { "saved": saved, "syncedCount": synced_count }
                    }),
                );
            }
            "GetCompanionStatus" | "GetBridgeDiagnostics" => {
                let det = MillenniumService::default().detect();
                let apps = collect_merged_loaded_apps(
                    stplug_dir,
                    overrides.slssteam_config_path.as_deref(),
                );
                let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
                let lib_base = home.join(".local/lib/luatools");

                let preload_lib32 = lib_base.join("lib/libsteam_integration.so").is_file()
                    || lib_base.join("32/libsteam_integration.so").is_file()
                    || lib_base.join("libsteam_integration.so").is_file()
                    || Path::new("/usr/lib/luatools/libsteam_integration.so").is_file();

                let preload_lib64 = lib_base.join("lib64/libsteam_integration.so").is_file()
                    || lib_base.join("64/libsteam_integration.so").is_file()
                    || Path::new("/usr/lib64/luatools/libsteam_integration.so").is_file();

                let multilib_dst = if preload_lib32 && preload_lib64 {
                    Some(format!("{}/$LIB/libsteam_integration.so", lib_base.display()))
                } else {
                    None
                };

                let sls_so = home.join(".local/share/SLSsteam/SLSsteam.so");
                let slssteam_active = sls_so.is_file();

                let bridge_preloaded = crate::steam_bridge::preload_loader::steam_integrated();

                #[cfg(unix)]
                let uid = unsafe { libc::getuid() };
                #[cfg(not(unix))]
                let uid = 1000u32;

                let mep_sock_user = PathBuf::from(format!("/tmp/millennium-mep-{}.sock", uid));
                let mep_sock_default = PathBuf::from("/tmp/millennium-mep.sock");
                let mep_socket_active = mep_sock_user.exists() || mep_sock_default.exists();

                let ipc_sock = crate::steam_bridge::ipc::ipc_socket_path();
                let ipc_socket_active = ipc_sock.exists();

                let hook_count: usize = if slssteam_active { 14 } else { 0 };

                send_json(
                    req,
                    200,
                    json!({
                        "success": true,
                        "pluginName": "LuaToolsLinux",
                        "author": "subsurfmod",
                        "online": true,
                        "pid": std::process::id(),
                        "version": crate::VERSION,
                        "millenniumDetected": det.installed,
                        "unlockedAppsCount": apps.len(),
                        "preloadLib64": preload_lib64,
                        "preloadLib32": preload_lib32,
                        "multilibDst": multilib_dst,
                        "slssteamActive": slssteam_active,
                        "bridgePreloaded": bridge_preloaded,
                        "hookCount": hook_count,
                        "mepSocketActive": mep_socket_active,
                        "ipcSocketActive": ipc_socket_active,
                        "result": {
                            "online": true,
                            "pid": std::process::id(),
                            "version": crate::VERSION,
                            "pluginName": "LuaToolsLinux",
                            "author": "subsurfmod",
                            "unlockedAppsCount": apps.len(),
                            "preloadLib64": preload_lib64,
                            "preloadLib32": preload_lib32,
                            "multilibDst": multilib_dst,
                            "slssteamActive": slssteam_active,
                            "bridgePreloaded": bridge_preloaded,
                            "hookCount": hook_count,
                            "mepSocketActive": mep_socket_active,
                            "ipcSocketActive": ipc_socket_active,
                        }
                    }),
                );
            }
            "UpdateSettingValue" => {
                let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("").trim();
                let val_opt = args.get("value");
                let mut updated = false;
                let mut error_msg: Option<String> = None;

                if key.is_empty() {
                    error_msg = Some("Missing setting key".into());
                } else if val_opt.is_none() {
                    error_msg = Some(format!("Missing value for setting '{}'", key));
                } else {
                    let val = val_opt.unwrap();
                    let parse_bool = |v: &Value| -> Option<bool> {
                        v.as_bool().or_else(|| {
                            v.as_str().and_then(|s| match s.trim().to_ascii_lowercase().as_str() {
                                "true" | "1" | "yes" | "on" => Some(true),
                                "false" | "0" | "no" | "off" => Some(false),
                                _ => None,
                            })
                        })
                    };
                    let parse_str = |v: &Value| -> Option<String> {
                        v.as_str().map(|s| s.to_string())
                    };
                    let parse_port = |v: &Value| -> Option<u16> {
                        v.as_u64()
                            .and_then(|n| u16::try_from(n).ok())
                            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u16>().ok()))
                            .filter(|&p| p > 0)
                    };

                    let mut svc = SettingsService::new(overrides.settings_config_path.clone());
                    match key {
                        "fast_fetch" | "fastFetch" => {
                            if let Some(b) = parse_bool(val) {
                                svc.settings.fast_fetch = b;
                                updated = svc.save();
                                if !updated {
                                    error_msg = Some(format!("Failed to save setting '{}'", key));
                                }
                            } else {
                                error_msg =
                                    Some(format!("Invalid boolean value for setting '{}'", key));
                            }
                        }
                        "auto_update_apps" | "autoUpdateApps" => {
                            if let Some(b) = parse_bool(val) {
                                svc.settings.auto_update_apps = b;
                                updated = svc.save();
                                if !updated {
                                    error_msg = Some(format!("Failed to save setting '{}'", key));
                                }
                            } else {
                                error_msg =
                                    Some(format!("Invalid boolean value for setting '{}'", key));
                            }
                        }
                        "donate_keys" | "donateKeys" => {
                            if let Some(b) = parse_bool(val) {
                                svc.settings.donate_keys = b;
                                updated = svc.save();
                                if !updated {
                                    error_msg = Some(format!("Failed to save setting '{}'", key));
                                }
                            } else {
                                error_msg =
                                    Some(format!("Invalid boolean value for setting '{}'", key));
                            }
                        }
                        "check_updates_on_startup" | "checkUpdatesOnStartup" => {
                            if let Some(b) = parse_bool(val) {
                                svc.settings.check_updates_on_startup = b;
                                updated = svc.save();
                                if !updated {
                                    error_msg = Some(format!("Failed to save setting '{}'", key));
                                }
                            } else {
                                error_msg =
                                    Some(format!("Invalid boolean value for setting '{}'", key));
                            }
                        }
                        "cef_bridge_enabled" | "cefBridgeEnabled" => {
                            if let Some(b) = parse_bool(val) {
                                svc.settings.cef_bridge_enabled = b;
                                updated = svc.save();
                                if !updated {
                                    error_msg = Some(format!("Failed to save setting '{}'", key));
                                }
                            } else {
                                error_msg =
                                    Some(format!("Invalid boolean value for setting '{}'", key));
                            }
                        }
                        "http_server_enabled" | "httpServerEnabled" => {
                            if let Some(b) = parse_bool(val) {
                                svc.settings.http_server_enabled = b;
                                updated = svc.save();
                                if !updated {
                                    error_msg = Some(format!("Failed to save setting '{}'", key));
                                }
                            } else {
                                error_msg =
                                    Some(format!("Invalid boolean value for setting '{}'", key));
                            }
                        }
                        "http_server_port" | "httpServerPort" => {
                            if let Some(p) = parse_port(val) {
                                svc.settings.http_server_port = p;
                                updated = svc.save();
                                if !updated {
                                    error_msg = Some(format!("Failed to save setting '{}'", key));
                                }
                            } else {
                                error_msg =
                                    Some(format!("Invalid port value for setting '{}'", key));
                            }
                        }
                        "close_to_tray" | "closeToTray" => {
                            if let Some(b) = parse_bool(val) {
                                svc.settings.close_to_tray = b;
                                updated = svc.save();
                                if !updated {
                                    error_msg = Some(format!("Failed to save setting '{}'", key));
                                }
                            } else {
                                error_msg =
                                    Some(format!("Invalid boolean value for setting '{}'", key));
                            }
                        }
                        "minimize_to_tray" | "minimizeToTray" => {
                            if let Some(b) = parse_bool(val) {
                                svc.settings.minimize_to_tray = b;
                                updated = svc.save();
                                if !updated {
                                    error_msg = Some(format!("Failed to save setting '{}'", key));
                                }
                            } else {
                                error_msg =
                                    Some(format!("Invalid boolean value for setting '{}'", key));
                            }
                        }
                        "doh_enabled" | "dohEnabled" => {
                            if let Some(b) = parse_bool(val) {
                                svc.settings.doh_enabled = b;
                                updated = svc.save();
                                if !updated {
                                    error_msg = Some(format!("Failed to save setting '{}'", key));
                                }
                            } else {
                                error_msg =
                                    Some(format!("Invalid boolean value for setting '{}'", key));
                            }
                        }
                        "autostart" => {
                            if let Some(b) = parse_bool(val) {
                                match svc.set_autostart(b) {
                                    Ok(()) => updated = true,
                                    Err(e) => error_msg = Some(e),
                                }
                            } else {
                                error_msg =
                                    Some(format!("Invalid boolean value for setting '{}'", key));
                            }
                        }
                        "language" => {
                            if let Some(s) = parse_str(val) {
                                svc.settings.language = s;
                                updated = svc.save();
                                if !updated {
                                    error_msg = Some(format!("Failed to save setting '{}'", key));
                                }
                            } else {
                                error_msg =
                                    Some(format!("Invalid string value for setting '{}'", key));
                            }
                        }
                        "steam_path_override" | "steamPathOverride" => {
                            if let Some(s) = parse_str(val) {
                                svc.settings.steam_path_override = s;
                                updated = svc.save();
                                if !updated {
                                    error_msg = Some(format!("Failed to save setting '{}'", key));
                                }
                            } else {
                                error_msg =
                                    Some(format!("Invalid string value for setting '{}'", key));
                            }
                        }
                        "steam_username" | "steamUsername" => {
                            if let Some(s) = parse_str(val) {
                                svc.settings.steam_username = s;
                                updated = svc.save();
                                if !updated {
                                    error_msg = Some(format!("Failed to save setting '{}'", key));
                                }
                            } else {
                                error_msg =
                                    Some(format!("Invalid string value for setting '{}'", key));
                            }
                        }
                        "doh_endpoint" | "dohEndpoint" => {
                            if let Some(s) = parse_str(val) {
                                svc.settings.doh_endpoint = s;
                                updated = svc.save();
                                if !updated {
                                    error_msg = Some(format!("Failed to save setting '{}'", key));
                                }
                            } else {
                                error_msg =
                                    Some(format!("Invalid string value for setting '{}'", key));
                            }
                        }
                        "hubcap_api_key" | "hubcapApiKey" => {
                            if let Some(s) = parse_str(val) {
                                svc.settings.hubcap_api_key = s;
                                updated = svc.save();
                                if !updated {
                                    error_msg = Some(format!("Failed to save setting '{}'", key));
                                }
                            } else {
                                error_msg =
                                    Some(format!("Invalid string value for setting '{}'", key));
                            }
                        }
                        _ => {
                            PluginLog::global().warn(
                                Some("HttpServer"),
                                &format!("Unknown setting key: {}", key),
                            );
                            error_msg = Some(format!("Unknown setting key: {}", key));
                        }
                    }
                }

                if updated {
                    send_json(
                        req,
                        200,
                        json!({
                            "success": true,
                            "key": key,
                            "updated": true,
                            "result": { "key": key, "updated": true }
                        }),
                    );
                } else {
                    let err =
                        error_msg.unwrap_or_else(|| format!("Failed to update setting '{}'", key));
                    send_json(
                        req,
                        200,
                        json!({
                            "success": false,
                            "key": key,
                            "updated": false,
                            "error": err,
                            "result": { "key": key, "updated": false, "error": err }
                        }),
                    );
                }
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
        if req.method() != &Method::Post {
            send_json(
                req,
                405,
                json!({ "success": false, "error": "Method Not Allowed - POST required" }),
            );
            return;
        }
        if allow_external_spawn() {
            thread::spawn(|| {
                let _ = SteamService::relaunch_steam_sls(None);
            });
        }
        send_json(
            req,
            200,
            json!({ "success": true, "restarting": true, "restarted": true }),
        );
        return;
    }

    // Route: /check-updates (GET and POST — LT-28)
    if parts == ["check-updates"] {
        let res = run_update_check(body_json.as_ref());
        send_json(req, 200, update_check_to_json(&res));
        return;
    }

    // Route: /loaded-apps (GET and POST — LT-28)
    if parts == ["loaded-apps"] {
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
                if overrides.settings_config_path.is_some() || allow_external_spawn() {
                    let mut ss = SettingsService::new(overrides.settings_config_path.clone());
                    for id in &incoming {
                        ss.record_loaded_app(*id);
                    }
                }
            }
            let merged = collect_merged_loaded_apps(
                stplug_dir,
                overrides.slssteam_config_path.as_deref(),
            );
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
            let apps = collect_merged_loaded_apps(
                stplug_dir,
                overrides.slssteam_config_path.as_deref(),
            );
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
        const MAX_ICON_BYTES: u64 = 10 * 1024 * 1024;
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
                    if resp
                        .into_reader()
                        .take(MAX_ICON_BYTES)
                        .read_to_end(&mut bytes)
                        .is_ok()
                        && !bytes.is_empty()
                    {
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
                    if resp
                        .into_reader()
                        .take(MAX_ICON_BYTES)
                        .read_to_end(&mut bytes)
                        .is_ok()
                        && !bytes.is_empty()
                    {
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
        let (removed_lua, removed_disabled) = remove_app_completely(
            stplug_dir,
            aid,
            overrides.slssteam_config_path.as_deref(),
        );
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
        let _ = event_tx.send(RpcUiEvent::OpenFix(aid));
        notify();
        send_json(req, 200, json!({ "success": true, "ok": true, "appid": aid }));
        return;
    }

    // Route: /open/settings or /settings
    if (parts.len() == 2 && parts[0] == "open" && parts[1] == "settings") || parts == ["settings"] {
        let _ = event_tx.send(RpcUiEvent::OpenSettings);
        notify();
        send_json(req, 200, json!({ "success": true, "ok": true }));
        return;
    }

    // Route: /open/app/:appid or /open/:appid
    if (parts.len() >= 2 && parts[0] == "open" && parts[1] == "app")
        || (parts.len() == 2 && parts[0] == "open" && aid > 0)
    {
        if aid > 0 {
            let _ = event_tx.send(RpcUiEvent::AddApp(aid));
            notify();
        }
        send_json(req, 200, json!({ "success": true, "ok": true, "appid": aid }));
        return;
    }

    // Route: /open/acquire or /open/acquire/:appid
    if parts.len() >= 2 && parts[0] == "open" && parts[1] == "acquire" {
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
        let _ = event_tx.send(RpcUiEvent::OpenLibrary);
        notify();
        send_json(req, 200, json!({ "success": true, "ok": true }));
        return;
    }

    // Route: /open/depots or /open/depots/:appid
    if parts.len() >= 2 && parts[0] == "open" && parts[1] == "depots" {
        let _ = event_tx.send(RpcUiEvent::OpenDepots(aid));
        notify();
        send_json(req, 200, json!({ "success": true, "ok": true, "appid": aid }));
        return;
    }

    // Route: /steam/restart or /restart
    if parts == ["steam", "restart"] || parts == ["restart"] {
        if req.method() != &Method::Post {
            send_json(
                req,
                405,
                json!({ "success": false, "error": "Method Not Allowed - POST required" }),
            );
            return;
        }
        if allow_external_spawn() {
            thread::spawn(|| {
                let _ = SteamService::relaunch_steam_sls(None);
            });
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
        let cfg = sanitized_settings_json(overrides.settings_config_path.as_deref());
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
            .set("X-LuaTools-Token", &srv.rpc_token)
            .send_json(json!({ "method": "HasLuaToolsForApp", "args": { "appid": 1245620 } }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rpc["success"], true);
        assert_eq!(rpc["exists"], false);
        assert_eq!(rpc["depotCount"], 0);
        assert_eq!(rpc["dlcCount"], 0);

        // Test AddViaLuaTools
        let rpc_add: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
            .send_json(json!({ "method": "AddViaLuaTools", "args": { "appid": 1245620 } }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rpc_add["success"], true);
        assert_eq!(rpc_add["queued"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::AddApp(1245620)));

        // Test GetAddViaLuaToolsStatus & GetDownloadStatus
        let rpc_st: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
            .send_json(json!({ "method": "GetAddViaLuaToolsStatus", "args": { "appid": 1245620 } }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rpc_st["success"], true);
        assert!(rpc_st.get("result").is_some());

        // Test CancelDownload
        let rpc_cancel: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
            .send_json(json!({ "method": "CancelDownload", "args": { "appid": 1245620 } }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rpc_cancel["success"], true);
        assert_eq!(rpc_cancel["cancelled"], true);

        // Test CheckSources
        let rpc_src: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
            .send_json(json!({ "method": "CheckSources", "args": { "appid": 1245620 } }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rpc_src["success"], true);
        assert!(rpc_src.get("sources").is_some());

        // Test OpenFix
        let rpc_fix: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
            .send_json(json!({ "method": "OpenFix", "args": { "appid": 1245620 } }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rpc_fix["success"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::OpenFix(1245620)));

        // Test RestoreFromTray RPC
        let rpc_restore: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
            .send_json(json!({ "method": "RestoreFromTray", "args": {} }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rpc_restore["success"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::RestoreFromTray));

        // Test CheckUpdates & GetLoadedApps RPC
        let rpc_up: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
            .send_json(json!({ "method": "CheckUpdates", "args": {} }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rpc_up["success"], true);
        assert_eq!(rpc_up["currentVersion"], crate::VERSION);

        // Unknown RPC method returns 400 with success: false
        let unknown = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
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
        assert!(resp
            .header("Access-Control-Allow-Headers")
            .unwrap()
            .contains("X-LuaTools-Token"));
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

        // 2. /add (requires X-LuaTools-Token)
        let add_unauth = ureq::get(&format!("http://127.0.0.1:{}/add/1245620", port)).call();
        assert!(matches!(add_unauth, Err(ureq::Error::Status(403, _))));

        let add1: Value = ureq::get(&format!("http://127.0.0.1:{}/add/1245620", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
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

        // 4. /remove (requires token, and POST required to prevent GET CSRF mutations) & removes .lua + .lua.disabled
        let rem_unauth = ureq::post(&format!("http://127.0.0.1:{}/remove/1245620", port))
            .send_json(json!({}));
        assert!(matches!(rem_unauth, Err(ureq::Error::Status(403, _))));

        let rem_get = ureq::get(&format!("http://127.0.0.1:{}/remove/1245620", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
            .call();
        assert!(matches!(rem_get, Err(ureq::Error::Status(405, _))));

        let rem: Value = ureq::post(&format!("http://127.0.0.1:{}/remove/1245620", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
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
            .set("X-LuaTools-Token", &srv.rpc_token)
            .send_json(json!({ "method": "DeleteLuaToolsForApp", "args": { "appid": 730 } }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rpc_del["success"], true);
        assert_eq!(rpc_del["removedDisabled"], true);
        assert!(!dir.path().join("730.lua.disabled").exists());
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::RemovedApp(730)));

        // 5. /open/fix and /open/settings (require token)
        let fix: Value = ureq::get(&format!("http://127.0.0.1:{}/open/fix/1245620", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(fix["ok"], true);
        assert_eq!(fix["success"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::OpenFix(1245620)));

        let set: Value = ureq::get(&format!("http://127.0.0.1:{}/open/settings", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
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
            .set("X-LuaTools-Token", &srv.rpc_token)
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
            .set("X-LuaTools-Token", &srv.rpc_token)
            .send_json(json!({ "source": "Sadie (Morrenus)" }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(dl["success"], true);

        // Cancel sets real cancelled state reflected in /download-status
        let cancel: Value = ureq::post(&format!("http://127.0.0.1:{}/cancel/1245620", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
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
            .set("X-LuaTools-Token", &srv.rpc_token)
            .send_json(json!({ "url": "https://example.com" }));
        assert!(matches!(blocked_example, Err(ureq::Error::Status(400, _))));

        let blocked_file = ureq::post(&format!("http://127.0.0.1:{}/open-url", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
            .send_json(json!({ "url": "file:///etc/passwd" }));
        assert!(matches!(blocked_file, Err(ureq::Error::Status(400, _))));

        let open_url: Value = ureq::post(&format!("http://127.0.0.1:{}/open-url", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
            .send_json(json!({ "url": "https://store.steampowered.com/app/1245620" }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(open_url["success"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::RestoreFromTray));

        let r_steam: Value = ureq::post(&format!("http://127.0.0.1:{}/restart-steam", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
            .send_json(json!({}))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(r_steam["success"], true);
        assert_eq!(r_steam["restarting"], true);

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
            .set("X-LuaTools-Token", &srv.rpc_token)
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
            .set("X-LuaTools-Token", &srv.rpc_token)
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
            .set("X-LuaTools-Token", &srv.rpc_token)
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(lib_resp["success"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::OpenLibrary));

        let depots_resp: Value = ureq::get(&format!("http://127.0.0.1:{}/open/depots/1245620", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(depots_resp["success"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::OpenDepots(1245620)));

        // 10. /steam/restart and /restart enforce POST and reject GET with 405
        let get_steam_restart = ureq::get(&format!("http://127.0.0.1:{}/steam/restart", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
            .call();
        assert!(matches!(get_steam_restart, Err(ureq::Error::Status(405, _))));

        let post_steam_restart: Value = ureq::post(&format!("http://127.0.0.1:{}/steam/restart", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
            .send_json(json!({}))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(post_steam_restart["success"], true);

        let get_restart = ureq::get(&format!("http://127.0.0.1:{}/restart", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
            .call();
        assert!(matches!(get_restart, Err(ureq::Error::Status(405, _))));

        let post_restart: Value = ureq::post(&format!("http://127.0.0.1:{}/restart", port))
            .set("X-LuaTools-Token", &srv.rpc_token)
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

    #[test]
    fn test_new_millennium_rpc_endpoints_and_discovery_file() {
        let dir = tempfile::tempdir().unwrap();
        let stplug_dir = dir.path().join("stplug-in");
        std::fs::create_dir_all(&stplug_dir).unwrap();
        let sls_cfg_path = dir.path().join("slssteam_config.yaml");
        let settings_cfg_path = dir.path().join("settings.json");

        let (tx, rx) = std::sync::mpsc::channel();
        let port = get_free_port();
        let mut srv = HttpServerService::start_with_config_paths(
            port,
            stplug_dir.clone(),
            Some(sls_cfg_path.clone()),
            Some(settings_cfg_path.clone()),
            tx,
            None,
        )
        .unwrap();
        let token = srv.rpc_token.clone();

        // 1. Test RPC discovery file created
        let disc_file = std::env::temp_dir().join("luatools_rpc.json");
        // May be in XDG_RUNTIME_DIR or temp_dir
        if disc_file.exists() {
            let content = std::fs::read_to_string(&disc_file).unwrap();
            assert!(content.contains(&port.to_string()));
        }

        // 2. ToggleAppEnabled
        let lua_file = stplug_dir.join("1245620.lua");
        std::fs::write(&lua_file, b"addappid(1245620)\n").unwrap();

        let toggle_off: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &token)
            .send_json(json!({
                "method": "ToggleAppEnabled",
                "args": { "appid": 1245620, "enabled": false }
            }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(toggle_off["success"], true);
        assert_eq!(toggle_off["enabled"], false);
        assert!(!lua_file.exists());
        assert!(stplug_dir.join("1245620.lua.disabled").exists());

        let toggle_on: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &token)
            .send_json(json!({
                "method": "ToggleAppEnabled",
                "args": { "appid": 1245620, "enabled": true }
            }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(toggle_on["success"], true);
        assert_eq!(toggle_on["enabled"], true);
        assert!(lua_file.exists());
        assert!(!stplug_dir.join("1245620.lua.disabled").exists());

        // 3. ApplyRecommendedFixes
        let fix_res: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &token)
            .send_json(json!({
                "method": "ApplyRecommendedFixes",
                "args": { "appid": 1245620 }
            }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(fix_res["success"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::OpenFix(1245620)));

        // 4. RemoveAppliedFixes
        let rem_fix: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &token)
            .send_json(json!({
                "method": "RemoveAppliedFixes",
                "args": { "appid": 1245620 }
            }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rem_fix["success"], true);

        // Write realistic Lua script with base app, 2 keyed depots, and 1 DLC
        std::fs::write(
            &lua_file,
            "addappid(1245620)\naddappid(1245621, 1, \"1245621_KEY\")\naddappid(1245622, 1, \"1245622_KEY\")\naddappid(2778580)\n",
        )
        .unwrap();

        // 4b. HasLuaToolsForApp computes real depotCount and dlcCount
        let has_res: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &token)
            .send_json(json!({
                "method": "HasLuaToolsForApp",
                "args": { "appid": 1245620 }
            }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(has_res["success"], true);
        assert_eq!(has_res["exists"], true);
        assert_eq!(has_res["has"], true);
        assert_eq!(has_res["disabled"], false);
        assert_eq!(has_res["depotCount"], 2);
        assert_eq!(has_res["dlcCount"], 1);

        // 5. SyncDlcForApp (isolated temp SLSsteam config.yaml — skips keyed base-game depots)
        let dlc_res: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &token)
            .send_json(json!({
                "method": "SyncDlcForApp",
                "args": { "appid": 1245620 }
            }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(dlc_res["success"], true);
        assert_eq!(dlc_res["dlcCount"], 1);
        assert_eq!(dlc_res["dlcIds"], json!([2778580]));
        assert!(sls_cfg_path.is_file());
        let sls_after_dlc = SlssteamService::new(Some(sls_cfg_path.clone()));
        assert!(sls_after_dlc.config.additional_apps.contains(&2778580));
        assert!(!sls_after_dlc.config.additional_apps.contains(&1245621));
        assert!(!sls_after_dlc.config.additional_apps.contains(&1245622));

        // 5b. OpenFixInDesktopApp alias & RestartSteam non-blocking response
        let open_fix_desktop: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &token)
            .send_json(json!({
                "method": "OpenFixInDesktopApp",
                "args": { "appid": 1245620 }
            }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(open_fix_desktop["success"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::OpenFix(1245620)));

        let restart_rpc: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &token)
            .send_json(json!({
                "method": "RestartSteam",
                "args": {}
            }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(restart_rpc["success"], true);
        assert_eq!(restart_rpc["restarting"], true);

        // 5c. ToggleAppEnabled returns 404 error when neither .lua nor .lua.disabled exists
        let toggle_missing = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &token)
            .send_json(json!({
                "method": "ToggleAppEnabled",
                "args": { "appid": 9999999, "enabled": true }
            }));
        assert!(matches!(toggle_missing, Err(ureq::Error::Status(404, _))));

        // 5d. SetLoadedApps persists via SettingsService::record_loaded_app using isolated settings path
        let set_loaded_res: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &token)
            .send_json(json!({
                "method": "SetLoadedApps",
                "args": { "apps": [413150, 1091500] }
            }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(set_loaded_res["success"], true);
        assert_eq!(set_loaded_res["received"], 2);
        let persisted_loaded = SettingsService::new(Some(settings_cfg_path.clone()))
            .settings
            .loaded_app_ids;
        assert!(persisted_loaded.contains(&413150));
        assert!(persisted_loaded.contains(&1091500));

        // 6. SyncSlsSteamConfig (isolated temp SLSsteam config.yaml)
        let sync_res: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &token)
            .send_json(json!({
                "method": "SyncSlsSteamConfig",
                "args": {}
            }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(sync_res["success"], true);
        assert_eq!(sync_res["syncedCount"], 1);
        let sls_after_sync = SlssteamService::new(Some(sls_cfg_path));
        assert!(sls_after_sync.config.additional_apps.contains(&1245620));
        assert!(sls_after_sync.config.depot_keys.contains_key(&1245621));

        // 7. GetCompanionStatus and GetBridgeDiagnostics with real bridge fields
        let comp_st: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &token)
            .send_json(json!({
                "method": "GetCompanionStatus",
                "args": {}
            }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(comp_st["success"], true);
        assert_eq!(comp_st["pluginName"], "LuaToolsLinux");
        assert_eq!(comp_st["author"], "subsurfmod");
        assert_eq!(comp_st["online"], true);
        assert!(comp_st["preloadLib64"].is_boolean());
        assert!(comp_st["preloadLib32"].is_boolean());
        assert!(comp_st["slssteamActive"].is_boolean());
        assert!(comp_st["bridgePreloaded"].is_boolean());
        assert!(comp_st["hookCount"].is_number());
        assert!(comp_st["mepSocketActive"].is_boolean());
        assert!(comp_st["ipcSocketActive"].is_boolean());

        // 8. CheckForFixes returns real appliedFixes, availableFixes, and hasFixes
        let chk_fixes: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &token)
            .send_json(json!({
                "method": "CheckForFixes",
                "args": { "appid": 1245620 }
            }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(chk_fixes["success"], true);
        assert!(chk_fixes["appliedFixes"].is_array());
        assert!(chk_fixes["availableFixes"].is_array());
        assert!(chk_fixes["hasFixes"].is_boolean());

        // 9. UpdateSettingValue real mutation (both snake_case and camelCase, isolated temp settings.json)
        let set_res: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &token)
            .send_json(json!({
                "method": "UpdateSettingValue",
                "args": { "key": "fast_fetch", "value": false }
            }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(set_res["success"], true);
        assert_eq!(set_res["updated"], true);
        assert_eq!(
            SettingsService::new(Some(settings_cfg_path.clone()))
                .settings
                .fast_fetch,
            false
        );

        let set_camel_res: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &token)
            .send_json(json!({
                "method": "UpdateSettingValue",
                "args": { "key": "fastFetch", "value": true }
            }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(set_camel_res["success"], true);
        assert_eq!(set_camel_res["updated"], true);
        assert_eq!(
            SettingsService::new(Some(settings_cfg_path.clone()))
                .settings
                .fast_fetch,
            true
        );

        let set_port_res: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &token)
            .send_json(json!({
                "method": "UpdateSettingValue",
                "args": { "key": "httpServerPort", "value": 6868 }
            }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(set_port_res["success"], true);
        assert_eq!(
            SettingsService::new(Some(settings_cfg_path.clone()))
                .settings
                .http_server_port,
            6868
        );

        // Unknown setting key must return success: false, updated: false with error
        let unknown_key_res: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &token)
            .send_json(json!({
                "method": "UpdateSettingValue",
                "args": { "key": "nonexistent_setting_key", "value": true }
            }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(unknown_key_res["success"], false);
        assert_eq!(unknown_key_res["updated"], false);
        assert!(unknown_key_res["error"]
            .as_str()
            .map(|e| !e.is_empty())
            .unwrap_or(false));

        // Missing value must return success: false, updated: false with error
        let missing_val_res: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &token)
            .send_json(json!({
                "method": "UpdateSettingValue",
                "args": { "key": "fast_fetch" }
            }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(missing_val_res["success"], false);
        assert_eq!(missing_val_res["updated"], false);
        assert!(missing_val_res["error"]
            .as_str()
            .map(|e| !e.is_empty())
            .unwrap_or(false));

        // Invalid value type must return success: false, updated: false with error
        let bad_val_res: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", &token)
            .send_json(json!({
                "method": "UpdateSettingValue",
                "args": { "key": "fast_fetch", "value": "not_a_bool" }
            }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(bad_val_res["success"], false);
        assert_eq!(bad_val_res["updated"], false);
        assert!(bad_val_res["error"]
            .as_str()
            .map(|e| !e.is_empty())
            .unwrap_or(false));

        // 10. Ephemeral port fallback when target port is already occupied
        let (tx2, _rx2) = std::sync::mpsc::channel();
        let mut srv2 = HttpServerService::start(port, stplug_dir, tx2, None)
            .expect("Ephemeral fallback should succeed when port is busy");
        assert_ne!(srv2.port, port);
        assert!(srv2.port > 0);
        srv2.stop();

        // 11. Strict token validation: missing X-LuaTools-Token is rejected with 403
        let missing_token_res = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .send_json(json!({ "method": "GetCompanionStatus" }));
        assert!(matches!(
            missing_token_res,
            Err(ureq::Error::Status(403, _))
        ));

        // Even with an allowed Steam Origin, missing X-LuaTools-Token is rejected with 403
        let missing_token_with_origin = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("Origin", "https://store.steampowered.com")
            .send_json(json!({ "method": "GetCompanionStatus" }));
        assert!(matches!(
            missing_token_with_origin,
            Err(ureq::Error::Status(403, _))
        ));

        // Spoofed/bad X-LuaTools-Token is rejected with 403
        let bad_token_res = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("X-LuaTools-Token", "malicious_spoofed_token_hex_123456")
            .send_json(json!({ "method": "GetCompanionStatus" }));
        assert!(matches!(bad_token_res, Err(ureq::Error::Status(403, _))));

        // Valid token with disallowed Origin is still rejected with 403
        let bad_origin_with_token = ureq::post(&format!("http://127.0.0.1:{}/rpc", port))
            .set("Origin", "https://malicious-attacker.com")
            .set("X-LuaTools-Token", &token)
            .send_json(json!({ "method": "GetCompanionStatus" }));
        assert!(matches!(
            bad_origin_with_token,
            Err(ureq::Error::Status(403, _))
        ));

        srv.stop();
    }

    #[test]
    fn host_origin_and_token_hardening_regression() {
        // 1. Host header parsing: IPv6 loopback accepted, empty and non-loopback rejected
        assert!(is_allowed_host("127.0.0.1:6767"));
        assert!(is_allowed_host("localhost:6767"));
        assert!(is_allowed_host("[::1]:6767"));
        assert!(is_allowed_host("[::1]"));
        assert!(is_allowed_host("::1"));
        assert!(!is_allowed_host(""));
        assert!(!is_allowed_host("   "));
        assert!(!is_allowed_host("[::1]:notaport"));
        assert!(!is_allowed_host("evil.com:6767"));
        assert!(!is_allowed_host("127.0.0.1.evil.com:6767"));

        // 2. Origin canonicalization rejects control chars, non-ASCII, paths, and untrusted hosts
        assert_eq!(
            canonical_allowed_origin("https://store.steampowered.com"),
            Some("https://store.steampowered.com".into())
        );
        assert_eq!(
            canonical_allowed_origin("http://[::1]:6767"),
            Some("http://[::1]:6767".into())
        );
        assert!(!is_allowed_origin("https://store.steampowered.com\r\nX-Injected: 1"));
        assert!(!is_allowed_origin("https://store.steampowered.com/é"));
        assert!(!is_allowed_origin("https://store.steampowered.com/app/123"));
        assert!(!is_allowed_origin("http://store.steampowered.com"));
        assert!(!is_allowed_origin("https://evil.com"));

        // 3. Uniform Host/Origin/Token enforcement across routes
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::channel();
        let port = get_free_port();
        let mut srv = HttpServerService::start(port, dir.path().to_path_buf(), tx, None).unwrap();

        // Read-only /ping rejects unauthorized Origin uniformly
        let bad_ping = ureq::get(&format!("http://127.0.0.1:{}/ping", port))
            .set("Origin", "https://evil-attacker.com")
            .call();
        assert!(matches!(bad_ping, Err(ureq::Error::Status(403, _))));

        // Mutating routes require valid X-LuaTools-Token
        for (method, path) in [
            ("POST", "/rpc"),
            ("GET", "/add/1245620"),
            ("POST", "/remove/1245620"),
            ("POST", "/delete/1245620"),
            ("POST", "/cancel/1245620"),
            ("POST", "/open-url"),
            ("POST", "/restart-steam"),
            ("POST", "/steam/restart"),
            ("POST", "/restart"),
            ("POST", "/download/1245620"),
            ("POST", "/add-source/1245620"),
            ("POST", "/loaded-apps"),
            ("GET", "/open/fix/1245620"),
            ("GET", "/open/settings"),
            ("GET", "/open/library"),
            ("GET", "/open/depots/1245620"),
        ] {
            let url = format!("http://127.0.0.1:{}{}", port, path);
            let res = if method == "POST" {
                ureq::post(&url).send_json(json!({}))
            } else {
                ureq::get(&url).call()
            };
            assert!(
                matches!(res, Err(ureq::Error::Status(403, _))),
                "Expected 403 without token for {} {}",
                method,
                path
            );
        }

        srv.stop();
    }

    #[test]
    fn rpc_discovery_file_uses_pid_tmp_0600_and_only_unlinks_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_rpc_discovery_file_in(dir.path(), 6767, "tok-owner").unwrap();
        assert!(path.is_file());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        let my_pid = std::process::id();
        // Mismatched PID must not remove file
        remove_rpc_discovery_files_checked(&[path.clone()], my_pid.wrapping_add(1), Some(6767), Some("tok-owner"));
        assert!(path.exists());

        // Mismatched port must not remove file
        remove_rpc_discovery_files_checked(&[path.clone()], my_pid, Some(9999), Some("tok-owner"));
        assert!(path.exists());

        // Mismatched token must not remove file
        remove_rpc_discovery_files_checked(&[path.clone()], my_pid, Some(6767), Some("tok-other"));
        assert!(path.exists());

        // Exact PID + port + token match removes file
        remove_rpc_discovery_files_checked(&[path.clone()], my_pid, Some(6767), Some("tok-owner"));
        assert!(!path.exists());
    }
}
