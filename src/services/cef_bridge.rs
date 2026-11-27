//! CefBridgeService — polls Steam CEF CDP on `127.0.0.1:8080/json`, probes `:8080`
//! for port conflicts, self-heals `.cef-enable-remote-debugging` symlink markers, and
//! injects `window.Millennium` + `window.PluginConfig` + `luatools.js` into store tabs.

use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use rand::RngCore;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use tungstenite::protocol::Role;
use tungstenite::{Message, WebSocket};
use url::Url;

use crate::services::plugin_log::PluginLog;

pub const DEFAULT_CEF_DEBUG_PORT: u16 = 8080;
const CEF_DEBUG_URL: &str = "http://127.0.0.1:8080/json";

/// Result of probing a local TCP port to distinguish Steam CEF CDP from another service (LT-22).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CefPortProbe {
    /// No listener is bound on the port.
    Free,
    /// A Chromium / Steam CEF DevTools Protocol endpoint responded on `/json/version`.
    SteamCef { browser: String },
    /// Another non-CDP service is occupying the port.
    Conflict { detail: String },
}

/// Probes `127.0.0.1:<port>` via TCP connect + `GET /json/version` to distinguish
/// Steam CEF remote debugging from an unrelated service occupying `:8080` (LT-22).
pub fn probe_cef_port(port: u16) -> CefPortProbe {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    match TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
        Err(_) => return CefPortProbe::Free,
        Ok(stream) => drop(stream),
    }

    let url = format!("http://127.0.0.1:{}/json/version", port);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_millis(500))
        .timeout(Duration::from_millis(1200))
        .build();

    match agent.get(&url).call() {
        Ok(resp) => {
            let status = resp.status();
            if status != 200 {
                return CefPortProbe::Conflict {
                    detail: format!("HTTP {} on /json/version (not Steam CEF)", status),
                };
            }
            match resp.into_json::<Value>() {
                Ok(val) => {
                    let browser_opt = val.get("Browser").and_then(|v| v.as_str());
                    let has_cdp_fields = browser_opt.is_some()
                        || val.get("Protocol-Version").is_some()
                        || val.get("webSocketDebuggerUrl").is_some();
                    if has_cdp_fields {
                        CefPortProbe::SteamCef {
                            browser: browser_opt.unwrap_or("Steam CEF").to_string(),
                        }
                    } else {
                        CefPortProbe::Conflict {
                            detail: "HTTP 200 JSON without CDP fields on /json/version".into(),
                        }
                    }
                }
                Err(_) => CefPortProbe::Conflict {
                    detail: "Non-JSON HTTP service occupying port".into(),
                },
            }
        }
        Err(ureq::Error::Status(code, _)) => CefPortProbe::Conflict {
            detail: format!("HTTP {} on /json/version (another service on :{})", code, port),
        },
        Err(e) => CefPortProbe::Conflict {
            detail: format!("Non-CDP TCP listener on :{} ({})", port, e),
        },
    }
}

pub struct CefBridgeService {
    pub steam_root: Option<PathBuf>,
    pub http_port: u16,
    pub rpc_token: Option<String>,
    stop_flag: Arc<AtomicBool>,
    status: Arc<Mutex<String>>,
    handle: Option<JoinHandle<()>>,
}

impl CefBridgeService {
    pub fn new(steam_root: Option<PathBuf>, http_port: u16) -> Self {
        Self {
            steam_root,
            http_port,
            rpc_token: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
            status: Arc::new(Mutex::new("Stopped".into())),
            handle: None,
        }
    }

    /// Updates the local HTTP RPC port and session auth token used by CEF bridge calls
    /// and injected Store scripts, restarting the worker thread if already running and changed.
    pub fn set_rpc_endpoint(&mut self, http_port: u16, rpc_token: Option<String>) {
        let changed = self.http_port != http_port || self.rpc_token != rpc_token;
        self.http_port = http_port;
        self.rpc_token = rpc_token;
        if changed && self.is_running() {
            self.stop();
            self.start();
        }
    }

    pub fn is_running(&self) -> bool {
        self.handle
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }

    pub fn last_status(&self) -> String {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Probes the default Steam CEF remote debugging port (`:8080`).
    pub fn probe_port() -> CefPortProbe {
        probe_cef_port(DEFAULT_CEF_DEBUG_PORT)
    }

    /// Creates or repairs `.cef-enable-remote-debugging` as a symlink pointing to
    /// `<steam_root>/cef_debug_enabled` (LT-22).
    pub fn ensure_cef_debugging_enabled(steam_root: &Path) -> Result<String, String> {
        let marker = steam_root.join(".cef-enable-remote-debugging");
        let target = steam_root.join("cef_debug_enabled");

        if !target.is_file() {
            fs::write(&target, b"1").map_err(|e| e.to_string())?;
        }

        if let Ok(meta) = fs::symlink_metadata(&marker) {
            if meta.file_type().is_symlink() {
                if let Ok(dest) = fs::read_link(&marker) {
                    if dest == target {
                        return Ok(format!(
                            "CEF remote debugging symlink already active: {} -> {}",
                            marker.display(),
                            target.display()
                        ));
                    }
                }
            }
            if meta.file_type().is_dir() {
                fs::remove_dir_all(&marker).map_err(|e| e.to_string())?;
            } else {
                fs::remove_file(&marker).map_err(|e| e.to_string())?;
            }
        }

        std::os::unix::fs::symlink(&target, &marker).map_err(|e| e.to_string())?;
        let msg = format!(
            "Created symlink {} -> {} (restart Steam to open CDP on :8080)",
            marker.display(),
            target.display()
        );
        PluginLog::global().info(Some("CefBridge"), &msg);
        Ok(msg)
    }

    /// Startup self-heal for `.cef-enable-remote-debugging`:
    /// detects missing, regular-file, directory, or broken/stale symlinks and repairs them (LT-22).
    pub fn self_heal_cef_marker(steam_root: &Path) -> Result<String, String> {
        let marker = steam_root.join(".cef-enable-remote-debugging");
        let target = steam_root.join("cef_debug_enabled");

        let needs_repair = match fs::symlink_metadata(&marker) {
            Err(_) => true,
            Ok(meta) => {
                if !meta.file_type().is_symlink() {
                    true
                } else {
                    match fs::read_link(&marker) {
                        Ok(dest) => dest != target || !target.is_file(),
                        Err(_) => true,
                    }
                }
            }
        };

        if needs_repair {
            PluginLog::global().info(
                Some("CefBridge"),
                &format!(
                    "Self-healing CEF remote debugging marker at {}",
                    marker.display()
                ),
            );
            Self::ensure_cef_debugging_enabled(steam_root)
        } else {
            Ok(format!(
                "CEF marker healthy: {} -> {}",
                marker.display(),
                target.display()
            ))
        }
    }

    /// Removes `.cef-enable-remote-debugging` and companion `cef_debug_enabled` marker files (LT-22).
    pub fn cleanup_cef_debugging_marker(steam_root: &Path) -> Result<bool, String> {
        let marker = steam_root.join(".cef-enable-remote-debugging");
        let target = steam_root.join("cef_debug_enabled");
        let mut removed = false;

        if let Ok(meta) = fs::symlink_metadata(&marker) {
            if meta.file_type().is_dir() {
                fs::remove_dir_all(&marker).map_err(|e| e.to_string())?;
            } else {
                fs::remove_file(&marker).map_err(|e| e.to_string())?;
            }
            removed = true;
        }
        if target.exists() {
            fs::remove_file(&target).map_err(|e| e.to_string())?;
            removed = true;
        }
        if removed {
            PluginLog::global().info(
                Some("CefBridge"),
                &format!(
                    "Cleaned up CEF debugging marker in {}",
                    steam_root.display()
                ),
            );
        }
        Ok(removed)
    }

    pub fn start(&mut self) {
        if self.is_running() {
            return;
        }
        self.stop_flag.store(false, Ordering::Relaxed);
        let stop = Arc::clone(&self.stop_flag);
        let status = Arc::clone(&self.status);
        let port = self.http_port;
        let rpc_token = self.rpc_token.clone();
        let steam_root = self.steam_root.clone();

        self.handle = Some(thread::spawn(move || {
            let mut yielded_to_millennium = false;

            // Startup self-heal of .cef-enable-remote-debugging symlink (LT-22),
            // skipped when Millennium's native plugin is already active via --remote-debugging-pipe.
            if !is_millennium_native_plugin_active(steam_root.as_deref()) {
                if let Some(ref sr) = steam_root {
                    if let Err(e) = Self::self_heal_cef_marker(sr) {
                        PluginLog::global().warn(
                            Some("CefBridge"),
                            &format!("CEF marker self-heal skipped: {}", e),
                        );
                    }
                }
                *status.lock().unwrap_or_else(|e| e.into_inner()) = "Polling CDP :8080...".into();
            }

            let agent = ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(2))
                .build();
            let mut injected_tabs: HashSet<String> = HashSet::new();
            let mut logged_port_conflict = false;

            while !stop.load(Ordering::Relaxed) {
                // Touchpoint 4: If Steam's steamwebhelper is running with Millennium's
                // --remote-debugging-pipe AND the LuaToolsLinux Millennium plugin is enabled,
                // yield cleanly instead of endlessly polling TCP :8080.
                if is_millennium_native_plugin_active(steam_root.as_deref()) {
                    if !yielded_to_millennium {
                        let msg =
                            "Active via Millennium Native Plugin (LuaToolsLinux by subsurfmod)";
                        PluginLog::global().info(
                            Some("CefBridge"),
                            &format!(
                                "Detected steamwebhelper --remote-debugging-pipe with LuaToolsLinux Millennium plugin enabled; yielding CDP :8080 polling ({})",
                                msg
                            ),
                        );
                        *status.lock().unwrap_or_else(|e| e.into_inner()) = msg.into();
                        yielded_to_millennium = true;
                        logged_port_conflict = false;
                    }
                    for _ in 0..20 {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(250));
                    }
                    continue;
                }

                if yielded_to_millennium {
                    yielded_to_millennium = false;
                    PluginLog::global().info(
                        Some("CefBridge"),
                        "Millennium native plugin no longer active; resuming CDP :8080 fallback polling",
                    );
                    *status.lock().unwrap_or_else(|e| e.into_inner()) =
                        "Polling CDP :8080...".into();
                }

                match agent.get(CEF_DEBUG_URL).call() {
                    Ok(resp) => match resp.into_json::<Vec<Value>>() {
                        Ok(tabs) => {
                            logged_port_conflict = false;
                            let mut store_count = 0usize;
                            let active_tids: HashSet<String> = tabs
                                .iter()
                                .filter_map(|t| {
                                    t.get("id")
                                        .and_then(|v| v.as_str())
                                        .map(ToString::to_string)
                                })
                                .collect();
                            injected_tabs.retain(|tid| active_tids.contains(tid));

                            for tab in tabs {
                                let url = tab.get("url").and_then(|v| v.as_str()).unwrap_or("");
                                let ws = tab
                                    .get("webSocketDebuggerUrl")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let tid = tab.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                if url.contains("store.steampowered.com") && !ws.is_empty() {
                                    store_count += 1;
                                    if !tid.is_empty() && !injected_tabs.contains(tid) {
                                        injected_tabs.insert(tid.to_string());
                                        let ws_url = ws.to_string();
                                        let stop_tab = Arc::clone(&stop);
                                        let root_opt = steam_root.clone();
                                        let tok_opt = rpc_token.clone();
                                        thread::spawn(move || {
                                            bridge_ws_tab_with_token(
                                                &ws_url,
                                                port,
                                                tok_opt.as_deref(),
                                                root_opt.as_deref(),
                                                Some(&stop_tab),
                                            );
                                        });
                                    }
                                }
                            }
                            *status.lock().unwrap_or_else(|e| e.into_inner()) =
                                format!("CDP connected ({} store tab(s))", store_count);
                        }
                        Err(_) => {
                            // :8080 returned HTTP 200 but not a CDP tab array — probe conflict
                            match probe_cef_port(DEFAULT_CEF_DEBUG_PORT) {
                                CefPortProbe::Conflict { detail } => {
                                    if !logged_port_conflict {
                                        PluginLog::global().warn(
                                            Some("CefBridge"),
                                            &format!("Port :8080 conflict detected: {}", detail),
                                        );
                                        logged_port_conflict = true;
                                    }
                                    *status.lock().unwrap_or_else(|e| e.into_inner()) =
                                        format!("Port :8080 conflict ({})", detail);
                                }
                                CefPortProbe::SteamCef { browser } => {
                                    *status.lock().unwrap_or_else(|e| e.into_inner()) =
                                        format!("CDP :8080 ({}) — waiting for tabs...", browser);
                                }
                                CefPortProbe::Free => {
                                    *status.lock().unwrap_or_else(|e| e.into_inner()) =
                                        "Waiting for Steam CDP :8080...".into();
                                }
                            }
                        }
                    },
                    Err(_) => match probe_cef_port(DEFAULT_CEF_DEBUG_PORT) {
                        CefPortProbe::Conflict { detail } => {
                            if !logged_port_conflict {
                                PluginLog::global().warn(
                                    Some("CefBridge"),
                                    &format!("Port :8080 conflict detected: {}", detail),
                                );
                                logged_port_conflict = true;
                            }
                            *status.lock().unwrap_or_else(|e| e.into_inner()) =
                                format!("Port :8080 conflict ({})", detail);
                        }
                        CefPortProbe::SteamCef { browser } => {
                            logged_port_conflict = false;
                            *status.lock().unwrap_or_else(|e| e.into_inner()) =
                                format!("CDP :8080 ({}) — waiting for store tabs...", browser);
                        }
                        CefPortProbe::Free => {
                            logged_port_conflict = false;
                            *status.lock().unwrap_or_else(|e| e.into_inner()) =
                                "Waiting for Steam CDP :8080...".into();
                        }
                    },
                }
                for _ in 0..8 {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(250));
                }
            }
            *status.lock().unwrap_or_else(|e| e.into_inner()) = "Stopped".into();
        }));
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = "Stopped".into();
    }
}

impl Drop for CefBridgeService {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Inspects Linux `/proc` to check whether a running `steamwebhelper` process
/// was launched by Millennium with `--remote-debugging-pipe` (which suppresses `:8080`).
pub fn is_steamwebhelper_using_debug_pipe() -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let Some(pid_str) = fname.to_str() else {
            continue;
        };
        if !pid_str.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let proc_dir = entry.path();
        let comm_matches = fs::read_to_string(proc_dir.join("comm"))
            .map(|c| c.trim().to_ascii_lowercase().starts_with("steamwebhelper"))
            .unwrap_or(false);
        let Ok(cmdline_bytes) = fs::read(proc_dir.join("cmdline")) else {
            continue;
        };
        let cmdline = String::from_utf8_lossy(&cmdline_bytes);
        let is_webhelper = comm_matches || cmdline.to_ascii_lowercase().contains("steamwebhelper");
        if is_webhelper
            && cmdline
                .split('\0')
                .any(|arg| arg == "--remote-debugging-pipe" || arg.starts_with("--remote-debugging-pipe="))
        {
            return true;
        }
    }
    false
}

fn is_luatoolslinux_plugin_enabled_in(
    plugin_json_paths: &[PathBuf],
    config_paths: &[PathBuf],
) -> bool {
    let manifest_present = plugin_json_paths.iter().any(|p| p.is_file());
    if !manifest_present {
        return false;
    }

    for cfg in config_paths {
        let Ok(raw) = fs::read_to_string(cfg) else {
            continue;
        };
        let Ok(val) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };

        // Shape 1: config.json { "plugins": { "enabledPlugins": ["LuaToolsLinux", ...] } }
        if let Some(list) = val
            .get("plugins")
            .and_then(|p| p.get("enabledPlugins"))
            .and_then(|e| e.as_array())
        {
            if list.iter().any(|item| item.as_str() == Some("LuaToolsLinux")) {
                return true;
            }
        }

        // Shape 2: top-level enabledPlugins / enabled_plugins array
        for key in ["enabledPlugins", "enabled_plugins"] {
            if let Some(list) = val.get(key).and_then(|v| v.as_array()) {
                if list.iter().any(|item| item.as_str() == Some("LuaToolsLinux")) {
                    return true;
                }
            }
        }

        // Shape 3: plugins.json array of objects [ { "name": "LuaToolsLinux", "enabled": true } ]
        if let Some(arr) = val.as_array() {
            for item in arr {
                let enabled = item
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let name = item
                    .get("name")
                    .or_else(|| item.get("common_name"))
                    .or_else(|| item.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if enabled && name.eq_ignore_ascii_case("LuaToolsLinux") {
                    return true;
                }
            }
        }
    }

    false
}

/// Checks whether the native `LuaToolsLinux` Millennium plugin manifest exists on disk
/// and `"LuaToolsLinux"` is enabled in Millennium's configuration.
pub fn is_luatoolslinux_millennium_plugin_enabled(steam_root: Option<&Path>) -> bool {
    let mut plugin_jsons = Vec::new();
    if let Ok(env_data) = std::env::var("MILLENNIUM__DATA_PATH") {
        let trimmed = env_data.trim();
        if !trimmed.is_empty() {
            plugin_jsons.push(PathBuf::from(trimmed).join("plugins/LuaToolsLinux/plugin.json"));
        }
    }
    if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME") {
        let trimmed = xdg_data.trim();
        if !trimmed.is_empty() {
            plugin_jsons.push(
                PathBuf::from(trimmed).join("millennium/plugins/LuaToolsLinux/plugin.json"),
            );
        }
    }
    if let Some(home) = dirs::home_dir() {
        plugin_jsons.push(home.join(".local/share/millennium/plugins/LuaToolsLinux/plugin.json"));
    }
    let config_paths = crate::services::millennium_service::MillenniumService::config_paths(steam_root);
    is_luatoolslinux_plugin_enabled_in(&plugin_jsons, &config_paths)
}

/// Touchpoint 4 check: returns true when Steam's `steamwebhelper` is running with
/// Millennium's `--remote-debugging-pipe` AND the `LuaToolsLinux` Millennium plugin is enabled.
pub fn is_millennium_native_plugin_active(steam_root: Option<&Path>) -> bool {
    is_steamwebhelper_using_debug_pipe() && is_luatoolslinux_millennium_plugin_enabled(steam_root)
}

fn find_luatools_js_with_home(steam_root: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(root) = steam_root {
        candidates.push(root.join("steamui/LuaTools/luatools.js"));
    }
    if let Some(home) = home {
        candidates.push(home.join(".local/share/millennium/plugins/LuaToolsLinux/public/luatools.js"));
        candidates.push(home.join(".local/share/luatools/plugins/luatools/luatools.js"));
        candidates.push(home.join(".local/share/Steam/steamui/LuaTools/luatools.js"));
        candidates.push(home.join(".local/share/millennium/plugins/luatools/public/luatools.js"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Locates `luatools.js` across the canonical search paths, checking
/// `millennium/plugins/LuaToolsLinux/public/luatools.js` before legacy lowercase
/// `millennium/plugins/luatools/public/luatools.js` (LT-19 / LT-23).
pub fn find_luatools_js(steam_root: Option<&Path>) -> Option<PathBuf> {
    let home = dirs::home_dir();
    find_luatools_js_with_home(steam_root, home.as_deref())
}

pub fn ensure_luatools_js_installed(steam_root: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = find_luatools_js(steam_root) {
        return Ok(p);
    }
    let installer = crate::services::plugin_installer_service::PluginInstallerService::default();
    installer.ensure_luatools_js_installed(steam_root)
}

fn build_polyfill_js(http_port: u16, rpc_token: Option<&str>) -> String {
    let version = crate::VERSION;
    let token_json =
        serde_json::to_string(rpc_token.unwrap_or("")).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"(function() {{
  window.__LUATOOLS_RPC_PORT__ = {http_port};
  window.__LUATOOLS_RPC_TOKEN__ = {token_json};
  if (window.__luatoolsLinuxPolyfill) return;
  window.__luatoolsLinuxPolyfill = true;
  const BASE = "http://127.0.0.1:{http_port}";
  window.__lt_queue = window.__lt_queue || [];
  window.__lt_pending = window.__lt_pending || {{}};
  window.__lt_req_id = window.__lt_req_id || 0;
  window.__lt_drain = function() {{
    const q = window.__lt_queue;
    window.__lt_queue = [];
    return JSON.stringify(q);
  }};
  window.__lt_resolve = function(id, res) {{
    if (window.__lt_pending[id]) {{
      let data = res;
      if (typeof res === "string") {{
        try {{ data = JSON.parse(res); }} catch(_) {{}}
      }}
      window.__lt_pending[id](data);
      delete window.__lt_pending[id];
    }}
  }};
  if (!window.Millennium) {{
    window.Millennium = {{}};
  }}
  window.Millennium.callServerMethod = function(a, b, c) {{
    let method = "";
    let args = {{}};
    if (c !== undefined) {{
      method = String(b || "");
      args = c;
    }} else if (b !== undefined) {{
      if (typeof b === "object" && b !== null) {{
        method = String(a || "");
        args = b;
      }} else {{
        method = String(b || "");
        args = {{}};
      }}
    }} else {{
      method = String(a || "");
      args = {{}};
    }}

    if (typeof args === "string") {{
      try {{ args = JSON.parse(args); }} catch (_) {{}}
    }}

    return new Promise(function(resolve) {{
      const reqId = String(++window.__lt_req_id);
      window.__lt_pending[reqId] = resolve;
      window.__lt_queue.push({{
        id: reqId,
        method: method,
        args: args || {{}}
      }});
    }});
  }};
  if (!window.Millennium.callFrontendMethod) {{
    window.Millennium.callFrontendMethod = function() {{ return Promise.resolve(); }};
  }}
  if (!window.Millennium.findElement) {{
    window.Millennium.findElement = function() {{ return null; }};
  }}

  // LT-23 / LT-28: CEF Protocol Configuration for LuaTools Store Plugin
  window.PluginConfig = {{
    backendPort: {http_port},
    backendUrl: BASE,
    baseUrl: BASE,
    rpcToken: {token_json},
    pollInterval: 500,
    platform: "linux",
    version: "{version}"
  }};
}})();"#
    )
}

pub fn connect_ws_local(ws_url: &str) -> Result<WebSocket<TcpStream>, String> {
    let parsed = Url::parse(ws_url).map_err(|e| e.to_string())?;
    let host = parsed.host_str().unwrap_or("127.0.0.1");
    let port = parsed.port_or_known_default().unwrap_or(8080);
    let addr: SocketAddr = format!("{}:{}", host, port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or_else(|| "No socket address resolved".to_string())?;

    if !addr.ip().is_loopback() {
        return Err(format!(
            "Forbidden non-loopback WebSocket address in CEF bridge: {}",
            addr
        ));
    }

    let mut stream =
        TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|e| e.to_string())?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

    let mut nonce = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ws_key = BASE64.encode(nonce);

    let mut path_and_query = parsed.path().to_string();
    if path_and_query.is_empty() {
        path_and_query.push('/');
    }
    if let Some(q) = parsed.query() {
        path_and_query.push('?');
        path_and_query.push_str(q);
    }

    let req = format!(
        "GET {} HTTP/1.1\r\n\
         Host: {}:{}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {}\r\n\
         Sec-WebSocket-Version: 13\r\n\r\n",
        path_and_query, host, port, ws_key
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;

    let mut resp_buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 256];
    let header_end = loop {
        let n = stream.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("EOF during WebSocket HTTP upgrade".into());
        }
        resp_buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = resp_buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        if resp_buf.len() > 16 * 1024 {
            return Err("WebSocket upgrade header exceeded 16 KiB".into());
        }
    };

    let headers_str = String::from_utf8_lossy(&resp_buf[..header_end]);
    let status_line = headers_str.lines().next().unwrap_or("");
    if !status_line.contains(" 101 ") && !status_line.ends_with(" 101") {
        return Err(format!("Unexpected WS status: {}", status_line));
    }

    let mut sha = Sha1::new();
    sha.update(ws_key.as_bytes());
    sha.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    let expected_accept = BASE64.encode(sha.finalize());
    let accept_ok = headers_str.lines().any(|line| {
        if let Some((k, v)) = line.split_once(':') {
            k.trim().eq_ignore_ascii_case("sec-websocket-accept") && v.trim() == expected_accept
        } else {
            false
        }
    });
    if !accept_ok {
        return Err("Invalid Sec-WebSocket-Accept header".into());
    }

    let leftover = resp_buf[header_end..].to_vec();
    Ok(WebSocket::from_partially_read(
        stream,
        leftover,
        Role::Client,
        None,
    ))
}

pub fn bridge_ws_tab(ws_url: &str, http_port: u16, steam_root: Option<&Path>) -> bool {
    bridge_ws_tab_with_token(ws_url, http_port, None, steam_root, None)
}

pub fn bridge_ws_tab_with_stop(
    ws_url: &str,
    http_port: u16,
    steam_root: Option<&Path>,
    stop: Option<&AtomicBool>,
) -> bool {
    bridge_ws_tab_with_token(ws_url, http_port, None, steam_root, stop)
}

pub fn bridge_ws_tab_with_token(
    ws_url: &str,
    http_port: u16,
    rpc_token: Option<&str>,
    steam_root: Option<&Path>,
    stop: Option<&AtomicBool>,
) -> bool {
    let effective_token: Option<String> = rpc_token
        .filter(|t| !t.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            crate::services::http_server::active_session_token()
                .read()
                .ok()
                .filter(|t| !t.is_empty())
                .map(|t| t.clone())
        });

    let mut js = build_polyfill_js(http_port, effective_token.as_deref());
    if let Ok(p) = ensure_luatools_js_installed(steam_root) {
        if let Ok(extra) = fs::read_to_string(p) {
            js.push('\n');
            js.push_str(&extra);
        }
    }

    let Ok(mut socket) = connect_ws_local(ws_url) else {
        return false;
    };

    // 1. Enable Page domain
    let page_enable = json!({
        "id": 1,
        "method": "Page.enable"
    });
    if socket
        .send(Message::Text(page_enable.to_string().into()))
        .is_err()
    {
        return false;
    }
    let _ = socket.read();

    // 2. Bypass CSP on this tab (LT-23 CSP handling)
    let bypass_csp = json!({
        "id": 2,
        "method": "Page.setBypassCSP",
        "params": { "enabled": true }
    });
    if socket
        .send(Message::Text(bypass_csp.to_string().into()))
        .is_err()
    {
        return false;
    }
    let _ = socket.read();

    // 3. Add script to evaluate on new document so it survives same-tab navigation
    let new_doc_script = json!({
        "id": 3,
        "method": "Page.addScriptToEvaluateOnNewDocument",
        "params": {
            "source": js
        }
    });
    let _ = socket.send(Message::Text(new_doc_script.to_string().into()));
    let _ = socket.read();

    // 4. Evaluate polyfill + luatools.js in current document
    let payload = json!({
        "id": 4,
        "method": "Runtime.evaluate",
        "params": {
            "expression": js,
            "userGesture": true,
            "awaitPromise": false
        }
    });
    if socket
        .send(Message::Text(payload.to_string().into()))
        .is_err()
    {
        return false;
    }
    let _ = socket.read();

    // 5. Keep WebSocket open and poll window.__lt_drain() to dispatch queued RPC calls outside browser CSP
    let mut cmd_id: i64 = 5;
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(8))
        .build();

    loop {
        if let Some(s) = stop {
            if s.load(Ordering::Relaxed) {
                break;
            }
        }

        let drain_eval = json!({
            "id": cmd_id,
            "method": "Runtime.evaluate",
            "params": {
                "expression": "typeof window.__lt_drain === 'function' ? window.__lt_drain() : '[]'",
                "returnByValue": true
            }
        });
        cmd_id += 1;

        if socket
            .send(Message::Text(drain_eval.to_string().into()))
            .is_err()
        {
            break;
        }

        let msg = match socket.read() {
            Ok(m) => m,
            Err(e) => match e {
                tungstenite::Error::Io(ref io_err)
                    if io_err.kind() == std::io::ErrorKind::WouldBlock
                        || io_err.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue;
                }
                _ => break,
            },
        };

        if let Ok(txt) = msg.into_text() {
            if let Ok(val) = serde_json::from_str::<Value>(&txt) {
                if let Some(res_str) = val
                    .get("result")
                    .and_then(|r| r.get("result"))
                    .and_then(|r| r.get("value"))
                    .and_then(|v| v.as_str())
                {
                    if let Ok(queued) = serde_json::from_str::<Vec<Value>>(res_str) {
                        for item in queued {
                            if let Some(req_id) = item.get("id").and_then(|i| i.as_str()) {
                                let method = item
                                    .get("method")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("");
                                let args = item.get("args").cloned().unwrap_or_else(|| json!({}));
                                let mut rpc_req =
                                    agent.post(&format!("http://127.0.0.1:{}/rpc", http_port));
                                if let Some(ref tok) = effective_token {
                                    rpc_req = rpc_req.set("X-LuaTools-Token", tok);
                                }
                                let rpc_res = match rpc_req
                                    .send_json(json!({ "method": method, "args": args }))
                                {
                                    Ok(r) => r.into_string().unwrap_or_else(|_| {
                                        json!({ "success": false, "error": "Failed to read RPC response" }).to_string()
                                    }),
                                    Err(ureq::Error::Status(_, resp)) => {
                                        resp.into_string().unwrap_or_else(|_| {
                                            json!({ "success": false, "error": "RPC error status" }).to_string()
                                        })
                                    }
                                    Err(e) => {
                                        PluginLog::global().warn(
                                            Some("CefBridge"),
                                            &format!("RPC '{}' dispatch failed: {}", method, e),
                                        );
                                        json!({
                                            "success": false,
                                            "error": format!("Local LuaTools backend unreachable: {}", e)
                                        })
                                        .to_string()
                                    }
                                };

                                let res_arg = match serde_json::from_str::<Value>(&rpc_res) {
                                    Ok(v) => v.to_string(),
                                    Err(_) => serde_json::to_string(&rpc_res)
                                        .unwrap_or_else(|_| "{}".into()),
                                };
                                let req_id_json =
                                    serde_json::to_string(req_id).unwrap_or_else(|_| "\"\"".into());

                                let resolve_js = format!(
                                    "if (typeof window.__lt_resolve === 'function') window.__lt_resolve({}, {});",
                                    req_id_json, res_arg
                                );
                                let resolve_cmd = json!({
                                    "id": cmd_id,
                                    "method": "Runtime.evaluate",
                                    "params": {
                                        "expression": resolve_js
                                    }
                                });
                                cmd_id += 1;
                                if socket
                                    .send(Message::Text(resolve_cmd.to_string().into()))
                                    .is_err()
                                {
                                    break;
                                }
                                let _ = socket.read();
                            }
                        }
                    }
                }
            }
        }

        let mut stopped = false;
        for _ in 0..5 {
            if let Some(s) = stop {
                if s.load(Ordering::Relaxed) {
                    stopped = true;
                    break;
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
        if stopped {
            break;
        }
    }

    let _ = socket.close(None);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn performs_rfc6455_handshake_and_evaluates_cdp_polyfill() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = thread::spawn(move || -> Vec<String> {
            let (mut conn, _) = listener.accept().unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 512];
            loop {
                let n = conn.read(&mut buf).unwrap();
                req.extend_from_slice(&buf[..n]);
                if req.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let req_str = String::from_utf8_lossy(&req);
            let key = req_str
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.trim()
                        .eq_ignore_ascii_case("sec-websocket-key")
                        .then(|| v.trim().to_string())
                })
                .unwrap();

            let mut sha = Sha1::new();
            sha.update(key.as_bytes());
            sha.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
            let accept = BASE64.encode(sha.finalize());

            let resp = format!(
                "HTTP/1.1 101 Switching Protocols\r\n\
                 Upgrade: websocket\r\n\
                 Connection: Upgrade\r\n\
                 Sec-WebSocket-Accept: {}\r\n\r\n",
                accept
            );
            conn.write_all(resp.as_bytes()).unwrap();

            let mut ws = WebSocket::from_raw_socket(conn, Role::Server, None);
            let mut received = Vec::new();
            for _ in 0..5 {
                if let Ok(msg) = ws.read() {
                    if let Ok(text) = msg.into_text() {
                        let text_str = text.to_string();
                        let id = serde_json::from_str::<Value>(&text_str)
                            .ok()
                            .and_then(|v| v.get("id").and_then(|i| i.as_i64()))
                            .unwrap_or(1);
                        received.push(text_str);
                        let _ = ws.send(Message::Text(
                            format!(r#"{{"id":{},"result":{{"result":{{"value":"[]"}}}}}}"#, id)
                                .into(),
                        ));
                    }
                }
            }
            received
        });

        let ws_url = format!("ws://127.0.0.1:{}/devtools/page/TAB1", port);
        assert!(bridge_ws_tab(&ws_url, 6767, None));
        let received = server.join().unwrap();
        assert_eq!(received.len(), 5);
        assert!(received[0].contains("Page.enable"));
        assert!(received[1].contains("Page.setBypassCSP"));
        assert!(received[1].contains(r#""enabled":true"#));
        assert!(received[2].contains("Page.addScriptToEvaluateOnNewDocument"));
        assert!(received[3].contains("Runtime.evaluate"));
        assert!(received[3].contains("http://127.0.0.1:6767"));
        assert!(received[3].contains("PluginConfig"));
        assert!(received[4].contains("__lt_drain"));
    }

    #[test]
    fn cdp_queue_drain_retrieves_requests_and_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let (rpc_tx, _rpc_rx) = std::sync::mpsc::channel();
        let http_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let http_port = http_listener.local_addr().unwrap().port();
        drop(http_listener);

        let mut srv = crate::services::http_server::HttpServerService::start(
            http_port,
            dir.path().to_path_buf(),
            rpc_tx,
            None,
        )
        .unwrap();

        let ws_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let ws_port = ws_listener.local_addr().unwrap().port();

        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);

        let server = thread::spawn(move || -> Vec<String> {
            let (mut conn, _) = ws_listener.accept().unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 512];
            loop {
                let n = conn.read(&mut buf).unwrap();
                req.extend_from_slice(&buf[..n]);
                if req.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let req_str = String::from_utf8_lossy(&req);
            let key = req_str
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.trim()
                        .eq_ignore_ascii_case("sec-websocket-key")
                        .then(|| v.trim().to_string())
                })
                .unwrap();

            let mut sha = Sha1::new();
            sha.update(key.as_bytes());
            sha.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
            let accept = BASE64.encode(sha.finalize());

            let resp = format!(
                "HTTP/1.1 101 Switching Protocols\r\n\
                 Upgrade: websocket\r\n\
                 Connection: Upgrade\r\n\
                 Sec-WebSocket-Accept: {}\r\n\r\n",
                accept
            );
            conn.write_all(resp.as_bytes()).unwrap();

            let mut ws = WebSocket::from_raw_socket(conn, Role::Server, None);
            let mut received = Vec::new();

            for _ in 0..4 {
                if let Ok(msg) = ws.read() {
                    if let Ok(text) = msg.into_text() {
                        let id = serde_json::from_str::<Value>(&text)
                            .ok()
                            .and_then(|v| v.get("id").and_then(|i| i.as_i64()))
                            .unwrap_or(1);
                        received.push(text.to_string());
                        let _ = ws.send(Message::Text(
                            format!(r#"{{"id":{},"result":{{}}}}"#, id).into(),
                        ));
                    }
                }
            }

            if let Ok(msg) = ws.read() {
                if let Ok(text) = msg.into_text() {
                    let id = serde_json::from_str::<Value>(&text)
                        .ok()
                        .and_then(|v| v.get("id").and_then(|i| i.as_i64()))
                        .unwrap_or(5);
                    received.push(text.to_string());
                    let payload = r#"[{"id":"req-42","method":"Ping","args":{}}]"#;
                    let resp_body = json!({
                        "id": id,
                        "result": {
                            "result": {
                                "value": payload
                            }
                        }
                    });
                    let _ = ws.send(Message::Text(resp_body.to_string().into()));
                }
            }

            if let Ok(msg) = ws.read() {
                if let Ok(text) = msg.into_text() {
                    let id = serde_json::from_str::<Value>(&text)
                        .ok()
                        .and_then(|v| v.get("id").and_then(|i| i.as_i64()))
                        .unwrap_or(6);
                    received.push(text.to_string());
                    let _ = ws.send(Message::Text(
                        format!(r#"{{"id":{},"result":{{}}}}"#, id).into(),
                    ));
                }
            }

            stop_clone.store(true, Ordering::Relaxed);
            received
        });

        let ws_url = format!("ws://127.0.0.1:{}/devtools/page/TAB1", ws_port);
        assert!(bridge_ws_tab_with_token(
            &ws_url,
            srv.port,
            Some(&srv.rpc_token),
            None,
            Some(&stop)
        ));
        let received = server.join().unwrap();
        assert!(received.len() >= 6);
        assert!(received[3].contains("__LUATOOLS_RPC_PORT__"));
        assert!(received[3].contains("__LUATOOLS_RPC_TOKEN__"));
        assert!(received[3].contains(&srv.rpc_token));
        assert!(received[4].contains("__lt_drain"));
        assert!(received[5].contains("__lt_resolve"));
        assert!(received[5].contains("req-42"));
        assert!(received[5].contains("pong"));

        srv.stop();
    }

    #[test]
    fn probe_cef_port_distinguishes_free_steam_cef_and_conflict() {
        // 1. Free port
        let tmp_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let free_port = tmp_listener.local_addr().unwrap().port();
        drop(tmp_listener);
        assert_eq!(probe_cef_port(free_port), CefPortProbe::Free);

        // 2. Steam CEF mock (/json/version returning Browser + Protocol-Version)
        let cef_srv = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let cef_port = cef_srv.server_addr().to_ip().unwrap().port();
        let cef_handle = thread::spawn(move || {
            if let Ok(req) = cef_srv.recv() {
                let resp = tiny_http::Response::from_string(
                    r#"{"Browser":"Chrome/126.0.6478.183","Protocol-Version":"1.3"}"#,
                )
                .with_header(
                    tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                );
                let _ = req.respond(resp);
            }
        });
        assert_eq!(
            probe_cef_port(cef_port),
            CefPortProbe::SteamCef {
                browser: "Chrome/126.0.6478.183".into()
            }
        );
        let _ = cef_handle.join();

        // 3. Non-CDP HTTP service conflict
        let conflict_srv = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let conflict_port = conflict_srv.server_addr().to_ip().unwrap().port();
        let conflict_handle = thread::spawn(move || {
            if let Ok(req) = conflict_srv.recv() {
                let resp = tiny_http::Response::from_string("<html>Apache Tomcat</html>")
                    .with_status_code(404);
                let _ = req.respond(resp);
            }
        });
        assert!(matches!(
            probe_cef_port(conflict_port),
            CefPortProbe::Conflict { .. }
        ));
        let _ = conflict_handle.join();
    }

    #[test]
    fn self_heal_and_cleanup_cef_marker_repairs_plain_file_and_broken_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let steam_root = dir.path();
        let marker = steam_root.join(".cef-enable-remote-debugging");
        let target = steam_root.join("cef_debug_enabled");

        // Regular file gets repaired into symlink -> cef_debug_enabled
        fs::write(&marker, b"").unwrap();
        let healed = CefBridgeService::self_heal_cef_marker(steam_root).unwrap();
        assert!(healed.contains("Created symlink"));
        assert!(fs::symlink_metadata(&marker).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_link(&marker).unwrap(), target);
        assert!(target.is_file());

        // Broken symlink pointing elsewhere gets repaired
        fs::remove_file(&marker).unwrap();
        std::os::unix::fs::symlink(steam_root.join("nonexistent_other"), &marker).unwrap();
        let healed2 = CefBridgeService::self_heal_cef_marker(steam_root).unwrap();
        assert!(healed2.contains("Created symlink"));
        assert_eq!(fs::read_link(&marker).unwrap(), target);

        // Cleanup removes both symlink and target cleanly
        assert!(CefBridgeService::cleanup_cef_debugging_marker(steam_root).unwrap());
        assert!(fs::symlink_metadata(&marker).is_err());
        assert!(!target.exists());
    }

    #[test]
    fn connect_ws_local_rejects_non_loopback() {
        let err = connect_ws_local("ws://1.1.1.1:8080/devtools").unwrap_err();
        assert!(err.contains("Forbidden non-loopback WebSocket address"));
    }

    #[test]
    fn find_luatools_js_prefers_millennium_luatoolslinux_over_lowercase_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let canonical = home.join(".local/share/millennium/plugins/LuaToolsLinux/public/luatools.js");
        let legacy = home.join(".local/share/millennium/plugins/luatools/public/luatools.js");

        // Fallback to lowercase when LuaToolsLinux is absent
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, "// legacy").unwrap();
        assert_eq!(find_luatools_js_with_home(None, Some(home)), Some(legacy.clone()));

        // Prefer LuaToolsLinux/public/luatools.js when both exist
        fs::create_dir_all(canonical.parent().unwrap()).unwrap();
        fs::write(&canonical, "// canonical").unwrap();
        assert_eq!(
            find_luatools_js_with_home(None, Some(home)),
            Some(canonical.clone())
        );
    }

    #[test]
    fn detects_enabled_luatoolslinux_millennium_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_json = dir.path().join("LuaToolsLinux/plugin.json");
        let cfg_json = dir.path().join("config.json");

        // Missing plugin.json -> false even if listed in config.json
        fs::write(
            &cfg_json,
            r#"{"plugins":{"enabledPlugins":["LuaToolsLinux"]}}"#,
        )
        .unwrap();
        assert!(!is_luatoolslinux_plugin_enabled_in(
            &[plugin_json.clone()],
            &[cfg_json.clone()]
        ));

        // Present plugin.json + enabled in config.json -> true
        fs::create_dir_all(plugin_json.parent().unwrap()).unwrap();
        fs::write(&plugin_json, r#"{"name":"LuaToolsLinux"}"#).unwrap();
        assert!(is_luatoolslinux_plugin_enabled_in(
            &[plugin_json.clone()],
            &[cfg_json.clone()]
        ));

        // Disabled in config.json -> false
        fs::write(&cfg_json, r#"{"plugins":{"enabledPlugins":["protondb"]}}"#).unwrap();
        assert!(!is_luatoolslinux_plugin_enabled_in(
            &[plugin_json],
            &[cfg_json]
        ));
    }
}
