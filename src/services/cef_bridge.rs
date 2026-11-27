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

    /// Creates or repairs `.cef-enable-remote-debugging` as a relative symlink pointing to
    /// `cef_debug_enabled` inside `steam_root` (works both natively and inside Flatpak Steam's
    /// mount namespace) (LT-22).
    pub fn ensure_cef_debugging_enabled(steam_root: &Path) -> Result<String, String> {
        let marker = steam_root.join(".cef-enable-remote-debugging");
        let target = steam_root.join("cef_debug_enabled");
        let rel_target = Path::new("cef_debug_enabled");

        if !target.is_file() {
            fs::write(&target, b"1").map_err(|e| e.to_string())?;
        }

        if let Ok(meta) = fs::symlink_metadata(&marker) {
            if meta.file_type().is_symlink() {
                if let Ok(dest) = fs::read_link(&marker) {
                    if dest == rel_target {
                        return Ok(format!(
                            "CEF remote debugging symlink already active: {} -> {}",
                            marker.display(),
                            rel_target.display()
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

        std::os::unix::fs::symlink(rel_target, &marker).map_err(|e| e.to_string())?;
        let msg = format!(
            "Created symlink {} -> {} (restart Steam to open CDP on :8080)",
            marker.display(),
            rel_target.display()
        );
        PluginLog::global().info(Some("CefBridge"), &msg);
        Ok(msg)
    }

    /// Startup self-heal for `.cef-enable-remote-debugging`:
    /// detects missing, regular-file, directory, or broken/stale/non-relative symlinks and repairs them (LT-22).
    pub fn self_heal_cef_marker(steam_root: &Path) -> Result<String, String> {
        let marker = steam_root.join(".cef-enable-remote-debugging");
        let target = steam_root.join("cef_debug_enabled");
        let rel_target = Path::new("cef_debug_enabled");

        let needs_repair = match fs::symlink_metadata(&marker) {
            Err(_) => true,
            Ok(meta) => {
                if !meta.file_type().is_symlink() {
                    true
                } else {
                    match fs::read_link(&marker) {
                        Ok(dest) => dest != rel_target || !target.is_file(),
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
                rel_target.display()
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
        // Allocate a fresh stop flag per start() generation so stop() + start() in
        // set_rpc_endpoint cleanly terminates prior tab workers without resetting their flag.
        let stop = Arc::new(AtomicBool::new(false));
        self.stop_flag = Arc::clone(&stop);
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
            let injected_tabs: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
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
                            injected_tabs
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .retain(|tid| active_tids.contains(tid));

                            for tab in tabs {
                                let url = tab.get("url").and_then(|v| v.as_str()).unwrap_or("");
                                let ws = tab
                                    .get("webSocketDebuggerUrl")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let tid = tab.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                if url.contains("store.steampowered.com") && !ws.is_empty() {
                                    store_count += 1;
                                    let should_spawn = if !tid.is_empty() {
                                        injected_tabs
                                            .lock()
                                            .unwrap_or_else(|e| e.into_inner())
                                            .insert(tid.to_string())
                                    } else {
                                        false
                                    };
                                    if should_spawn {
                                        let ws_url = ws.to_string();
                                        let tid_owned = tid.to_string();
                                        let injected_ref = Arc::clone(&injected_tabs);
                                        let stop_tab = Arc::clone(&stop);
                                        let root_opt = steam_root.clone();
                                        let tok_opt = rpc_token.clone();
                                        thread::spawn(move || {
                                            struct TabCleanupGuard {
                                                tid: String,
                                                injected: Arc<Mutex<HashSet<String>>>,
                                            }
                                            impl Drop for TabCleanupGuard {
                                                fn drop(&mut self) {
                                                    self.injected
                                                        .lock()
                                                        .unwrap_or_else(|e| e.into_inner())
                                                        .remove(&self.tid);
                                                }
                                            }
                                            let _guard = TabCleanupGuard {
                                                tid: tid_owned,
                                                injected: injected_ref,
                                            };
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

fn parse_proc_status_uid_and_state(status_text: &str) -> (Option<u32>, Option<char>) {
    let mut uid = None;
    let mut state = None;
    for line in status_text.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            uid = rest
                .split_whitespace()
                .next()
                .and_then(|tok| tok.parse::<u32>().ok());
        } else if let Some(rest) = line.strip_prefix("State:") {
            state = rest.trim_start().chars().next();
        }
        if uid.is_some() && state.is_some() {
            break;
        }
    }
    (uid, state)
}

fn proc_entry_is_steamwebhelper_debug_pipe(proc_dir: &Path, current_uid: u32) -> bool {
    let Ok(status_text) = fs::read_to_string(proc_dir.join("status")) else {
        return false;
    };
    let (uid_opt, state_opt) = parse_proc_status_uid_and_state(&status_text);
    if uid_opt != Some(current_uid) {
        return false;
    }
    if matches!(state_opt, Some('Z') | Some('X')) {
        return false;
    }

    let comm_matches = fs::read_to_string(proc_dir.join("comm"))
        .map(|c| c.trim().to_ascii_lowercase().contains("steamwebhelper"))
        .unwrap_or(false);

    let Ok(cmdline_bytes) = fs::read(proc_dir.join("cmdline")) else {
        return false;
    };
    let args: Vec<String> = cmdline_bytes
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    if args.is_empty() {
        return false;
    }

    let argv0_matches = Path::new(&args[0])
        .file_name()
        .and_then(|n| n.to_str())
        .map(|base| base.to_ascii_lowercase().contains("steamwebhelper"))
        .unwrap_or(false);

    if !(comm_matches || argv0_matches) {
        return false;
    }

    args.iter()
        .any(|arg| arg == "--remote-debugging-pipe" || arg.starts_with("--remote-debugging-pipe="))
}

/// Inspects Linux `/proc` to check whether a running `steamwebhelper` process owned by the
/// current user was launched by Millennium with `--remote-debugging-pipe` (which suppresses `:8080`).
pub fn is_steamwebhelper_using_debug_pipe() -> bool {
    #[cfg(unix)]
    let current_uid = unsafe { libc::getuid() };
    #[cfg(not(unix))]
    let current_uid = 0u32;

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
        if proc_entry_is_steamwebhelper_debug_pipe(&entry.path(), current_uid) {
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
            if list.iter().any(|item| {
                item.as_str()
                    .map(|s| s.eq_ignore_ascii_case("LuaToolsLinux"))
                    .unwrap_or(false)
            }) {
                return true;
            }
        }

        // Shape 2: top-level enabledPlugins / enabled_plugins array
        for key in ["enabledPlugins", "enabled_plugins"] {
            if let Some(list) = val.get(key).and_then(|v| v.as_array()) {
                if list.iter().any(|item| {
                    item.as_str()
                        .map(|s| s.eq_ignore_ascii_case("LuaToolsLinux"))
                        .unwrap_or(false)
                }) {
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

fn is_luatoolslinux_millennium_plugin_enabled_with_home(
    steam_root: Option<&Path>,
    home: Option<&Path>,
) -> bool {
    let mut plugin_jsons = Vec::new();
    if let Ok(env_plugins) = std::env::var("MILLENNIUM__PLUGINS_PATH") {
        let trimmed = env_plugins.trim();
        if !trimmed.is_empty() {
            plugin_jsons.push(PathBuf::from(trimmed).join("LuaToolsLinux/plugin.json"));
        }
    }
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
    let mut config_paths =
        crate::services::millennium_service::MillenniumService::config_paths(steam_root);
    if let Some(home) = home {
        plugin_jsons.push(home.join(".local/share/millennium/plugins/LuaToolsLinux/plugin.json"));
        plugin_jsons.push(
            home.join(
                ".var/app/com.valvesoftware.Steam/.local/share/millennium/plugins/LuaToolsLinux/plugin.json",
            ),
        );
        plugin_jsons.push(
            home.join(
                ".var/app/com.valvesoftware.Steam/data/millennium/plugins/LuaToolsLinux/plugin.json",
            ),
        );

        let flatpak_root = home.join(".var/app/com.valvesoftware.Steam");
        for rel in [
            ".config/millennium/plugins.json",
            ".config/millennium/config.json",
            ".local/share/millennium/config/plugins.json",
            ".local/share/millennium/config/config.json",
            ".local/share/millennium/plugins.json",
            ".local/share/millennium/config.json",
            "config/millennium/plugins.json",
            "config/millennium/config.json",
            "data/millennium/config/plugins.json",
            "data/millennium/config/config.json",
        ] {
            config_paths.push(flatpak_root.join(rel));
        }
    }
    is_luatoolslinux_plugin_enabled_in(&plugin_jsons, &config_paths)
}

/// Checks whether the native `LuaToolsLinux` Millennium plugin manifest exists on disk
/// and `"LuaToolsLinux"` is enabled in Millennium's configuration (native and Flatpak).
pub fn is_luatoolslinux_millennium_plugin_enabled(steam_root: Option<&Path>) -> bool {
    let home = dirs::home_dir();
    is_luatoolslinux_millennium_plugin_enabled_with_home(steam_root, home.as_deref())
}

/// Touchpoint 4 check: returns true when Steam's `steamwebhelper` is running with
/// Millennium's `--remote-debugging-pipe` AND the `LuaToolsLinux` Millennium plugin is enabled.
pub fn is_millennium_native_plugin_active(steam_root: Option<&Path>) -> bool {
    is_steamwebhelper_using_debug_pipe() && is_luatoolslinux_millennium_plugin_enabled(steam_root)
}

pub(crate) fn find_luatools_js_with_env(
    steam_root: Option<&Path>,
    home: Option<&Path>,
    millennium_plugins_path: Option<&str>,
    millennium_data_path: Option<&str>,
    xdg_data_home: Option<&str>,
) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    // 1. Full Millennium plugin UI takes priority over minimal steamui fallback
    if let Some(p) = millennium_plugins_path.map(str::trim).filter(|s| !s.is_empty()) {
        candidates.push(PathBuf::from(p).join("LuaToolsLinux/public/luatools.js"));
    }
    if let Some(d) = millennium_data_path.map(str::trim).filter(|s| !s.is_empty()) {
        candidates.push(PathBuf::from(d).join("plugins/LuaToolsLinux/public/luatools.js"));
    }
    if let Some(x) = xdg_data_home.map(str::trim).filter(|s| !s.is_empty()) {
        candidates.push(PathBuf::from(x).join("millennium/plugins/LuaToolsLinux/public/luatools.js"));
    }
    if let Some(home) = home {
        candidates.push(home.join(".local/share/millennium/plugins/LuaToolsLinux/public/luatools.js"));
        candidates.push(
            home.join(
                ".var/app/com.valvesoftware.Steam/.local/share/millennium/plugins/LuaToolsLinux/public/luatools.js",
            ),
        );
    }
    // 2. Steam UI minimal fallback
    if let Some(root) = steam_root {
        candidates.push(root.join("steamui/LuaTools/luatools.js"));
    }
    // 3. Remaining legacy / secondary fallbacks
    if let Some(home) = home {
        candidates.push(home.join(".local/share/luatools/plugins/luatools/luatools.js"));
        candidates.push(home.join(".local/share/Steam/steamui/LuaTools/luatools.js"));
        candidates.push(
            home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/steamui/LuaTools/luatools.js"),
        );
        candidates.push(home.join(".local/share/millennium/plugins/luatools/public/luatools.js"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

fn find_luatools_js_with_home(steam_root: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    let env_plugins = std::env::var("MILLENNIUM__PLUGINS_PATH").ok();
    let env_data = std::env::var("MILLENNIUM__DATA_PATH").ok();
    let env_xdg = std::env::var("XDG_DATA_HOME").ok();
    find_luatools_js_with_env(
        steam_root,
        home,
        env_plugins.as_deref(),
        env_data.as_deref(),
        env_xdg.as_deref(),
    )
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
  const BASE = "http://127.0.0.1:{http_port}";
  window.__LUATOOLS_RPC_PORT__ = {http_port};
  window.__LUATOOLS_RPC_TOKEN__ = {token_json};
  // LT-23 / LT-28: Always refresh CEF Protocol Configuration before any early return
  window.PluginConfig = {{
    backendPort: {http_port},
    backendUrl: BASE,
    baseUrl: BASE,
    rpcToken: {token_json},
    pollInterval: 500,
    platform: "linux",
    version: "{version}"
  }};
  if (window.__luatoolsLinuxPolyfill) return;
  window.__luatoolsLinuxPolyfill = true;
  window.__lt_queue = window.__lt_queue || [];
  window.__lt_pending = window.__lt_pending || {{}};
  window.__lt_req_id = window.__lt_req_id || 0;
  window.__lt_drain = function() {{
    const q = window.__lt_queue;
    window.__lt_queue = [];
    return JSON.stringify(q);
  }};
  window.__lt_resolve = function(id, res) {{
    const entry = window.__lt_pending[id];
    if (entry) {{
      delete window.__lt_pending[id];
      if (entry.timer) clearTimeout(entry.timer);
      let data = res;
      if (typeof res === "string") {{
        try {{ data = JSON.parse(res); }} catch(_) {{}}
      }}
      entry.resolve(data);
    }}
  }};
  if (!window.Millennium) {{
    window.Millennium = {{}};
  }}
  if (typeof window.Millennium.callServerMethod !== "function") {{
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

      return new Promise(function(resolve, reject) {{
        const reqId = String(++window.__lt_req_id);
        const timer = setTimeout(function() {{
          if (window.__lt_pending[reqId]) {{
            delete window.__lt_pending[reqId];
            reject(new Error("LuaTools CEF RPC timeout after 15000ms: " + method));
          }}
        }}, 15000);
        window.__lt_pending[reqId] = {{ resolve: resolve, reject: reject, timer: timer }};
        window.__lt_queue.push({{
          id: reqId,
          method: method,
          args: args || {{}}
        }});
      }});
    }};
  }}
  if (!window.Millennium.callFrontendMethod) {{
    window.Millennium.callFrontendMethod = function() {{ return Promise.resolve(); }};
  }}
  if (!window.Millennium.findElement) {{
    window.Millennium.findElement = function() {{ return null; }};
  }}
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

#[derive(Debug)]
enum CdpReadError {
    Timeout,
    Disconnected,
}

/// Sends a CDP JSON-RPC command and reads frames until the response with matching `"id"`
/// arrives, answering WebSocket `Ping` frames with `Pong` and skipping `Pong` and unsolicited
/// CDP event frames (such as `Page.*`).
fn send_cdp_and_wait(
    socket: &mut WebSocket<TcpStream>,
    expected_id: i64,
    payload: &Value,
) -> Result<Value, CdpReadError> {
    if socket
        .send(Message::Text(payload.to_string().into()))
        .is_err()
    {
        return Err(CdpReadError::Disconnected);
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match socket.read() {
            Ok(Message::Ping(data)) => {
                if socket.send(Message::Pong(data)).is_err() {
                    return Err(CdpReadError::Disconnected);
                }
            }
            Ok(Message::Pong(_)) => {}
            Ok(Message::Close(_)) => return Err(CdpReadError::Disconnected),
            Ok(Message::Text(txt)) => {
                if let Ok(val) = serde_json::from_str::<Value>(&txt) {
                    if val.get("id").and_then(|v| v.as_i64()) == Some(expected_id) {
                        return Ok(val);
                    }
                }
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(ref io_err))
                if io_err.kind() == std::io::ErrorKind::WouldBlock
                    || io_err.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Err(CdpReadError::Timeout);
            }
            Err(_) => return Err(CdpReadError::Disconnected),
        }
        if std::time::Instant::now() >= deadline {
            return Err(CdpReadError::Timeout);
        }
    }
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
    if send_cdp_and_wait(&mut socket, 1, &page_enable).is_err() {
        return false;
    }

    // 2. Bypass CSP on this tab (LT-23 CSP handling)
    let bypass_csp = json!({
        "id": 2,
        "method": "Page.setBypassCSP",
        "params": { "enabled": true }
    });
    if send_cdp_and_wait(&mut socket, 2, &bypass_csp).is_err() {
        return false;
    }

    // 3. Add script to evaluate on new document so it survives same-tab navigation
    let new_doc_script = json!({
        "id": 3,
        "method": "Page.addScriptToEvaluateOnNewDocument",
        "params": {
            "source": js
        }
    });
    if send_cdp_and_wait(&mut socket, 3, &new_doc_script).is_err() {
        return false;
    }

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
    if send_cdp_and_wait(&mut socket, 4, &payload).is_err() {
        return false;
    }

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

        let drain_id = cmd_id;
        cmd_id += 1;
        let drain_eval = json!({
            "id": drain_id,
            "method": "Runtime.evaluate",
            "params": {
                "expression": "typeof window.__lt_drain === 'function' ? window.__lt_drain() : '[]'",
                "returnByValue": true
            }
        });

        let val = match send_cdp_and_wait(&mut socket, drain_id, &drain_eval) {
            Ok(v) => v,
            Err(CdpReadError::Timeout) => continue,
            Err(CdpReadError::Disconnected) => break,
        };

        let mut disconnected = false;
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
                        let resolve_id = cmd_id;
                        cmd_id += 1;
                        let resolve_cmd = json!({
                            "id": resolve_id,
                            "method": "Runtime.evaluate",
                            "params": {
                                "expression": resolve_js
                            }
                        });
                        if let Err(CdpReadError::Disconnected) =
                            send_cdp_and_wait(&mut socket, resolve_id, &resolve_cmd)
                        {
                            disconnected = true;
                            break;
                        }
                    }
                }
            }
        }
        if disconnected {
            break;
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
        let rel_target = Path::new("cef_debug_enabled");

        // Regular file gets repaired into relative symlink -> cef_debug_enabled
        fs::write(&marker, b"").unwrap();
        let healed = CefBridgeService::self_heal_cef_marker(steam_root).unwrap();
        assert!(healed.contains("Created symlink"));
        assert!(fs::symlink_metadata(&marker).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_link(&marker).unwrap(), rel_target);
        assert!(target.is_file());

        // Absolute symlink gets migrated to relative symlink for Flatpak compatibility
        fs::remove_file(&marker).unwrap();
        std::os::unix::fs::symlink(&target, &marker).unwrap();
        let healed_abs = CefBridgeService::self_heal_cef_marker(steam_root).unwrap();
        assert!(healed_abs.contains("Created symlink"));
        assert_eq!(fs::read_link(&marker).unwrap(), rel_target);

        // Broken symlink pointing elsewhere gets repaired
        fs::remove_file(&marker).unwrap();
        std::os::unix::fs::symlink(steam_root.join("nonexistent_other"), &marker).unwrap();
        let healed2 = CefBridgeService::self_heal_cef_marker(steam_root).unwrap();
        assert!(healed2.contains("Created symlink"));
        assert_eq!(fs::read_link(&marker).unwrap(), rel_target);

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
        let steam_root = dir.path().join("Steam");
        let steamui_fallback = steam_root.join("steamui/LuaTools/luatools.js");
        let canonical = home.join(".local/share/millennium/plugins/LuaToolsLinux/public/luatools.js");
        let legacy = home.join(".local/share/millennium/plugins/luatools/public/luatools.js");
        let env_plugins = dir.path().join("custom_plugins");
        let env_plugin_js = env_plugins.join("LuaToolsLinux/public/luatools.js");

        // Fallback to lowercase when others are absent
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, "// legacy").unwrap();
        assert_eq!(
            find_luatools_js_with_env(None, Some(home), None, None, None),
            Some(legacy.clone())
        );

        // steamui fallback is found when Millennium plugin is absent
        fs::create_dir_all(steamui_fallback.parent().unwrap()).unwrap();
        fs::write(&steamui_fallback, "// minimal steamui fallback").unwrap();
        assert_eq!(
            find_luatools_js_with_env(Some(&steam_root), Some(home), None, None, None),
            Some(steamui_fallback.clone())
        );

        // Prefer ~/.local/share/millennium/plugins/LuaToolsLinux/public/luatools.js over steamui fallback
        fs::create_dir_all(canonical.parent().unwrap()).unwrap();
        fs::write(&canonical, "// full millennium UI").unwrap();
        assert_eq!(
            find_luatools_js_with_env(Some(&steam_root), Some(home), None, None, None),
            Some(canonical.clone())
        );

        // Prefer $MILLENNIUM__PLUGINS_PATH over home & steamui
        fs::create_dir_all(env_plugin_js.parent().unwrap()).unwrap();
        fs::write(&env_plugin_js, "// env plugins UI").unwrap();
        assert_eq!(
            find_luatools_js_with_env(
                Some(&steam_root),
                Some(home),
                Some(env_plugins.to_str().unwrap()),
                None,
                None
            ),
            Some(env_plugin_js)
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

        // Present plugin.json + enabled in config.json (case-insensitive) -> true
        fs::create_dir_all(plugin_json.parent().unwrap()).unwrap();
        fs::write(&plugin_json, r#"{"name":"LuaToolsLinux"}"#).unwrap();
        fs::write(
            &cfg_json,
            r#"{"plugins":{"enabledPlugins":["luatoolslinux"]}}"#,
        )
        .unwrap();
        assert!(is_luatoolslinux_plugin_enabled_in(
            &[plugin_json.clone()],
            &[cfg_json.clone()]
        ));

        // Shape 2 top-level case-insensitive match -> true
        fs::write(&cfg_json, r#"{"enabled_plugins":["LUATOOLSLINUX"]}"#).unwrap();
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

        // Flatpak Steam Millennium paths are detected cleanly
        let flatpak_home = tempfile::tempdir().unwrap();
        let fp_plugin = flatpak_home.path().join(
            ".var/app/com.valvesoftware.Steam/.local/share/millennium/plugins/LuaToolsLinux/plugin.json",
        );
        let fp_cfg = flatpak_home
            .path()
            .join(".var/app/com.valvesoftware.Steam/.config/millennium/config.json");
        fs::create_dir_all(fp_plugin.parent().unwrap()).unwrap();
        fs::create_dir_all(fp_cfg.parent().unwrap()).unwrap();
        fs::write(&fp_plugin, r#"{"name":"LuaToolsLinux"}"#).unwrap();
        fs::write(
            &fp_cfg,
            r#"{"plugins":{"enabledPlugins":["luatoolslinux"]}}"#,
        )
        .unwrap();
        assert!(is_luatoolslinux_millennium_plugin_enabled_with_home(
            None,
            Some(flatpak_home.path())
        ));
    }

    #[test]
    fn send_cdp_and_wait_handles_events_pings_and_mismatched_ids() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = thread::spawn(move || -> bool {
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
            let _cmd = ws.read().unwrap();

            // Send unsolicited Page event, Ping, Pong, and wrong id BEFORE matching id 7
            ws.send(Message::Text(
                r#"{"method":"Page.frameNavigated","params":{}}"#.into(),
            ))
            .unwrap();
            ws.send(Message::Ping(b"keepalive".to_vec().into()))
                .unwrap();
            ws.send(Message::Pong(b"unsolicited".to_vec().into()))
                .unwrap();
            ws.send(Message::Text(r#"{"id":999,"result":{}}"#.into()))
                .unwrap();
            ws.send(Message::Text(
                r#"{"id":7,"result":{"ok":true}}"#.into(),
            ))
            .unwrap();

            // Verify client replied with Pong to our Ping
            matches!(ws.read(), Ok(Message::Pong(_)))
        });

        let ws_url = format!("ws://127.0.0.1:{}/devtools/page/SYNC", port);
        let mut sock = connect_ws_local(&ws_url).unwrap();
        let res = send_cdp_and_wait(&mut sock, 7, &json!({ "id": 7, "method": "Page.enable" }));
        assert!(res.is_ok());
        assert_eq!(res.unwrap()["result"]["ok"], true);
        assert!(server.join().unwrap());
    }

    #[test]
    fn build_polyfill_js_refreshes_config_before_return_guards_millennium_and_adds_timeout() {
        let js = build_polyfill_js(6767, Some("tok123"));
        let port_pos = js.find("window.__LUATOOLS_RPC_PORT__").unwrap();
        let tok_pos = js.find("window.__LUATOOLS_RPC_TOKEN__").unwrap();
        let cfg_pos = js.find("window.PluginConfig").unwrap();
        let ret_pos = js.find("if (window.__luatoolsLinuxPolyfill) return;").unwrap();
        assert!(port_pos < ret_pos);
        assert!(tok_pos < ret_pos);
        assert!(cfg_pos < ret_pos);
        assert!(js.contains(r#"typeof window.Millennium.callServerMethod !== "function""#));
        assert!(js.contains("15000"));
        assert!(js.contains("clearTimeout"));
    }

    #[test]
    fn proc_debug_pipe_check_verifies_uid_skips_zombies_and_checks_argv0() {
        let dir = tempfile::tempdir().unwrap();
        let my_uid = 1000u32;

        // 1. Valid running steamwebhelper owned by my_uid
        let p1 = dir.path().join("101");
        fs::create_dir_all(&p1).unwrap();
        fs::write(&p1.join("status"), "Name:\tsteamwebhelper\nState:\tS (sleeping)\nUid:\t1000\t1000\t1000\t1000\n").unwrap();
        fs::write(&p1.join("comm"), "steamwebhelper\n").unwrap();
        fs::write(&p1.join("cmdline"), b"/home/user/.steam/ubuntu12_64/steamwebhelper\0--remote-debugging-pipe\0").unwrap();
        assert!(proc_entry_is_steamwebhelper_debug_pipe(&p1, my_uid));

        // 2. Different UID -> rejected
        assert!(!proc_entry_is_steamwebhelper_debug_pipe(&p1, 2000));

        // 3. Zombie process -> rejected
        let p2 = dir.path().join("102");
        fs::create_dir_all(&p2).unwrap();
        fs::write(&p2.join("status"), "Name:\tsteamwebhelper\nState:\tZ (zombie)\nUid:\t1000\t1000\t1000\t1000\n").unwrap();
        fs::write(&p2.join("comm"), "steamwebhelper\n").unwrap();
        fs::write(&p2.join("cmdline"), b"/usr/bin/steamwebhelper\0--remote-debugging-pipe\0").unwrap();
        assert!(!proc_entry_is_steamwebhelper_debug_pipe(&p2, my_uid));

        // 4. Unrelated command mentioning steamwebhelper in argument -> rejected
        let p3 = dir.path().join("103");
        fs::create_dir_all(&p3).unwrap();
        fs::write(&p3.join("status"), "Name:\tgrep\nState:\tR (running)\nUid:\t1000\t1000\t1000\t1000\n").unwrap();
        fs::write(&p3.join("comm"), "grep\n").unwrap();
        fs::write(&p3.join("cmdline"), b"/usr/bin/grep\0steamwebhelper\0--remote-debugging-pipe\0").unwrap();
        assert!(!proc_entry_is_steamwebhelper_debug_pipe(&p3, my_uid));
    }

    #[test]
    fn start_allocates_fresh_stop_flag_per_generation() {
        let mut bridge = CefBridgeService::new(None, 6767);
        bridge.start();
        let gen1_flag = Arc::clone(&bridge.stop_flag);
        assert!(!gen1_flag.load(Ordering::Relaxed));

        bridge.set_rpc_endpoint(6768, Some("new-token".into()));
        // Previous generation's stop flag must stay true permanently so old tab workers exit
        assert!(gen1_flag.load(Ordering::Relaxed));
        assert!(!Arc::ptr_eq(&gen1_flag, &bridge.stop_flag));
        assert!(!bridge.stop_flag.load(Ordering::Relaxed));

        bridge.stop();
        assert!(bridge.stop_flag.load(Ordering::Relaxed));
    }
}
