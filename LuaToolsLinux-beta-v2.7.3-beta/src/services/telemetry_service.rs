//! TelemetryService — privacy-first, PII-scrubbed diagnostic & crash telemetry for bug fixing.
//!
//! Features:
//! - Strictly scrubbed local ring-buffered JSONL event log (`~/.local/share/luatools/telemetry/events.jsonl`)
//! - Global Rust panic hook capturing scrubbed crash locations (`file:line`) for post-mortem bug reports
//! - 1-click Markdown diagnostic report generator, file exporter, and pre-filled GitHub Issue launcher
//! - Optional asynchronous remote HTTPS JSON / Discord webhook delivery when configured
//! - Immediate opt-out (`set_enabled(false)`) and 1-click data wipe (`clear_all()`)

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use chrono::Utc;
use rand::RngCore;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::models::config::default_luatools_data_dir;
use crate::services::depot_downloader::which_in_path;
use crate::VERSION;

pub const GITHUB_NEW_ISSUE_URL: &str = "https://github.com/subsurfmod/LuaToolsLinux/issues/new";
const MAX_STORED_EVENTS: usize = 200;
const ROTATE_THRESHOLD_BYTES: u64 = 192 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEvent {
    pub timestamp: String,
    pub level: String,
    pub category: String,
    pub event: String,
    pub message: String,
    pub app_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemFingerprint {
    pub app_version: String,
    pub distro: String,
    pub kernel: String,
    pub arch: String,
    pub display_server: String,
    pub has_dotnet: bool,
    pub has_wine: bool,
    pub steam_detected: bool,
    pub steam_libraries: usize,
    pub slssteam_configured: bool,
}

impl Default for SystemFingerprint {
    fn default() -> Self {
        Self::detect()
    }
}

impl SystemFingerprint {
    pub fn detect() -> Self {
        let distro = read_os_pretty_name().unwrap_or_else(|| "Linux".into());
        let kernel = fs::read_to_string("/proc/sys/kernel/osrelease")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".into());

        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        let x11 = std::env::var_os("DISPLAY").is_some();
        let winit_backend = std::env::var("WINIT_UNIX_BACKEND").unwrap_or_default();
        let session_type = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
        let display_server = match (wayland, x11, winit_backend.as_str()) {
            (true, true, "x11") => "Wayland (XWayland x11 backend)".into(),
            (true, _, _) => "Wayland".into(),
            (false, true, _) => "X11".into(),
            _ if !session_type.is_empty() => session_type,
            _ => "Headless / Unknown".into(),
        };

        Self {
            app_version: VERSION.to_string(),
            distro,
            kernel,
            arch: std::env::consts::ARCH.to_string(),
            display_server,
            has_dotnet: which_in_path("dotnet").is_some(),
            has_wine: which_in_path("wine").is_some(),
            steam_detected: false,
            steam_libraries: 0,
            slssteam_configured: false,
        }
    }
}

fn read_os_pretty_name() -> Option<String> {
    let raw = fs::read_to_string("/etc/os-release").ok()?;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("PRETTY_NAME=") {
            let clean = rest.trim().trim_matches('"').trim_matches('\'').trim();
            if !clean.is_empty() {
                return Some(clean.to_string());
            }
        }
    }
    None
}

fn home_path_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"/(?:home|Users)/[^/\s\x22\x27]+").unwrap())
}

fn hubcap_key_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"smm_[0-9a-fA-F]{8,}").unwrap())
}

fn hex64_key_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b[0-9a-fA-F]{64}\b").unwrap())
}

fn jwt_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}").unwrap()
    })
}

fn bearer_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)Bearer\s+\S+").unwrap())
}

fn email_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").unwrap())
}

#[derive(Clone)]
pub struct TelemetryService {
    pub telemetry_dir: PathBuf,
    enabled: Arc<AtomicBool>,
    endpoint_url: Arc<Mutex<String>>,
    install_id: Arc<Mutex<String>>,
    fingerprint: Arc<Mutex<SystemFingerprint>>,
    write_lock: Arc<Mutex<()>>,
}

impl Default for TelemetryService {
    fn default() -> Self {
        Self::new(None, true, String::new(), Self::generate_install_id())
    }
}

impl TelemetryService {
    pub fn new(
        telemetry_dir: Option<PathBuf>,
        enabled: bool,
        endpoint_url: String,
        install_id: String,
    ) -> Self {
        let dir = telemetry_dir.unwrap_or_else(|| default_luatools_data_dir().join("telemetry"));
        if enabled {
            let _ = fs::create_dir_all(&dir);
        }
        let id = if install_id.trim().is_empty() {
            Self::generate_install_id()
        } else {
            install_id
        };
        Self {
            telemetry_dir: dir,
            enabled: Arc::new(AtomicBool::new(enabled)),
            endpoint_url: Arc::new(Mutex::new(endpoint_url.trim().to_string())),
            install_id: Arc::new(Mutex::new(id)),
            fingerprint: Arc::new(Mutex::new(SystemFingerprint::detect())),
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn generate_install_id() -> String {
        let mut bytes = [0u8; 8];
        rand::thread_rng().fill_bytes(&mut bytes);
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }

    pub fn install_id(&self) -> String {
        self.install_id.lock().unwrap().clone()
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
        if enabled {
            let _ = fs::create_dir_all(&self.telemetry_dir);
        }
    }

    pub fn set_endpoint_url(&self, url: String) {
        *self.endpoint_url.lock().unwrap() = url.trim().to_string();
    }

    pub fn endpoint_url(&self) -> String {
        self.endpoint_url.lock().unwrap().clone()
    }

    pub fn update_runtime_context(
        &self,
        steam_detected: bool,
        steam_libraries: usize,
        slssteam_configured: bool,
    ) {
        let mut fp = self.fingerprint.lock().unwrap();
        fp.steam_detected = steam_detected;
        fp.steam_libraries = steam_libraries;
        fp.slssteam_configured = slssteam_configured;
    }

    pub fn snapshot_fingerprint(&self) -> SystemFingerprint {
        self.fingerprint.lock().unwrap().clone()
    }

    pub fn events_file_path(&self) -> PathBuf {
        self.telemetry_dir.join("events.jsonl")
    }

    pub fn last_panic_path(&self) -> PathBuf {
        self.telemetry_dir.join("last_panic.txt")
    }

    /// Scrubs usernames in paths, API keys, 64-char hex depot keys, JWTs, Bearer tokens, and emails.
    pub fn scrub_sensitive_text(raw: &str) -> String {
        let mut s = raw.to_string();
        if let Some(home) = dirs::home_dir() {
            let home_str = home.to_string_lossy();
            if home_str.len() > 1 {
                s = s.replace(home_str.as_ref(), "~");
            }
        }
        s = home_path_re().replace_all(&s, "~").into_owned();
        s = hubcap_key_re()
            .replace_all(&s, "smm_[REDACTED]")
            .into_owned();
        s = hex64_key_re()
            .replace_all(&s, "[DEPOT_KEY_REDACTED]")
            .into_owned();
        s = jwt_re().replace_all(&s, "[JWT_REDACTED]").into_owned();
        s = bearer_re()
            .replace_all(&s, "Bearer [REDACTED]")
            .into_owned();
        s = email_re().replace_all(&s, "[EMAIL_REDACTED]").into_owned();
        if s.chars().count() > 600 {
            let mut truncated: String = s.chars().take(597).collect();
            truncated.push_str("...");
            return truncated;
        }
        s
    }

    pub fn record(&self, level: &str, category: &str, event: &str, message: &str) {
        if !self.is_enabled() {
            return;
        }
        let ev = TelemetryEvent {
            timestamp: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            level: level.to_string(),
            category: category.to_string(),
            event: event.to_string(),
            message: Self::scrub_sensitive_text(message),
            app_version: VERSION.to_string(),
        };

        self.append_event_to_disk(&ev);

        let endpoint = self.endpoint_url();
        if !endpoint.is_empty()
            && (endpoint.starts_with("https://") || endpoint.starts_with("http://"))
        {
            let install_id = self.install_id();
            let fp = self.snapshot_fingerprint();
            thread::spawn(move || {
                let _ = post_event_remote(&endpoint, &install_id, &fp, &ev);
            });
        }
    }

    fn append_event_to_disk(&self, ev: &TelemetryEvent) {
        let _guard = self.write_lock.lock().unwrap();
        if fs::create_dir_all(&self.telemetry_dir).is_err() {
            return;
        }
        let path = self.events_file_path();
        let Ok(line) = serde_json::to_string(ev) else {
            return;
        };

        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(f, "{}", line);
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
        }

        // Rotate in-place if file grows beyond threshold
        if let Ok(meta) = fs::metadata(&path) {
            if meta.len() > ROTATE_THRESHOLD_BYTES {
                if let Ok(raw) = fs::read_to_string(&path) {
                    let all_lines: Vec<&str> = raw.lines().filter(|l| !l.is_empty()).collect();
                    let start = all_lines.len().saturating_sub(MAX_STORED_EVENTS);
                    let kept = all_lines[start..].join("\n") + "\n";
                    let tmp = path.with_extension("jsonl.tmp");
                    if fs::write(&tmp, kept).is_ok() {
                        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
                        let _ = fs::rename(&tmp, &path);
                    }
                }
            }
        }
    }

    pub fn recent_events(&self, limit: usize) -> Vec<TelemetryEvent> {
        let _guard = self.write_lock.lock().unwrap();
        let Ok(raw) = fs::read_to_string(self.events_file_path()) else {
            return Vec::new();
        };
        let mut parsed: Vec<TelemetryEvent> = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<TelemetryEvent>(l).ok())
            .collect();
        parsed.reverse();
        parsed.truncate(limit);
        parsed
    }

    pub fn event_count(&self) -> usize {
        let _guard = self.write_lock.lock().unwrap();
        let Ok(raw) = fs::read_to_string(self.events_file_path()) else {
            return 0;
        };
        raw.lines().filter(|l| !l.trim().is_empty()).count()
    }

    pub fn clear_all(&self) {
        let _guard = self.write_lock.lock().unwrap();
        let _ = fs::remove_file(self.events_file_path());
        let _ = fs::remove_file(self.last_panic_path());
    }

    /// Installs a chained Rust panic hook so uncaught panics are scrubbed and logged when telemetry is active.
    pub fn install_panic_hook(&self) {
        static INSTALLED: OnceLock<()> = OnceLock::new();
        if INSTALLED.set(()).is_err() {
            return;
        }
        let svc = self.clone();
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if svc.is_enabled() {
                let loc = info
                    .location()
                    .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                    .unwrap_or_else(|| "unknown".into());
                let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
                    (*s).to_string()
                } else if let Some(s) = info.payload().downcast_ref::<String>() {
                    s.clone()
                } else {
                    "Box<dyn Any> panic".into()
                };
                let summary =
                    Self::scrub_sensitive_text(&format!("Panic at {} — {}", loc, payload));
                let ev = TelemetryEvent {
                    timestamp: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    level: "panic".into(),
                    category: "crash".into(),
                    event: "panic_caught".into(),
                    message: summary.clone(),
                    app_version: VERSION.to_string(),
                };
                svc.append_event_to_disk(&ev);
                let _ = fs::write(svc.last_panic_path(), &summary);
            }
            prev_hook(info);
        }));
    }

    pub fn send_test_ping(&self) -> Result<String, String> {
        let endpoint = self.endpoint_url();
        if endpoint.is_empty() {
            return Err("Enter an HTTPS JSON or Discord Webhook URL first.".into());
        }
        if !endpoint.starts_with("https://") && !endpoint.starts_with("http://") {
            return Err("Endpoint URL must start with https:// or http://.".into());
        }
        let ev = TelemetryEvent {
            timestamp: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            level: "info".into(),
            category: "telemetry".into(),
            event: "test_ping".into(),
            message: "Test diagnostic ping from LuaTools Linux Settings".into(),
            app_version: VERSION.to_string(),
        };
        post_event_remote(
            &endpoint,
            &self.install_id(),
            &self.snapshot_fingerprint(),
            &ev,
        )?;
        Ok("Delivered test diagnostic event to remote endpoint.".into())
    }

    pub fn build_diagnostic_markdown(&self, max_events: usize) -> String {
        let fp = self.snapshot_fingerprint();
        let events = self.recent_events(max_events);

        let mut md = String::new();
        md.push_str("## Environment & Runtime Diagnostics\n\n");
        md.push_str("| Property | Value |\n|---|---|\n");
        md.push_str(&format!(
            "| **LuaTools Version** | `v{}` |\n",
            fp.app_version
        ));
        md.push_str(&format!("| **OS / Distro** | `{}` |\n", fp.distro));
        md.push_str(&format!(
            "| **Kernel / Arch** | `{} ({})` |\n",
            fp.kernel, fp.arch
        ));
        md.push_str(&format!(
            "| **Display Backend** | `{}` |\n",
            fp.display_server
        ));
        md.push_str(&format!(
            "| **Steam Detected** | `{}` ({} libraries) |\n",
            fp.steam_detected, fp.steam_libraries
        ));
        md.push_str(&format!(
            "| **SLSsteam Config** | `{}` |\n",
            fp.slssteam_configured
        ));
        md.push_str(&format!(
            "| **Optional Runtimes** | `.NET={}` · `Wine={}` |\n\n",
            fp.has_dotnet, fp.has_wine
        ));

        md.push_str("## Recent Telemetry Events (Scrubbed)\n\n");
        if events.is_empty() {
            md.push_str("_No telemetry events recorded._\n");
        } else {
            md.push_str("```text\n");
            for e in events {
                md.push_str(&format!(
                    "[{}] {:<5} {:<11} {} :: {}\n",
                    e.timestamp,
                    e.level.to_ascii_uppercase(),
                    e.category,
                    e.event,
                    e.message
                ));
            }
            md.push_str("```\n");
        }
        md
    }

    pub fn export_report(&self, dest_path: &Path) -> Result<PathBuf, String> {
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let md = self.build_diagnostic_markdown(80);
        fs::write(dest_path, md).map_err(|e| e.to_string())?;
        Ok(dest_path.to_path_buf())
    }

    pub fn open_github_bug_report(&self) -> Result<String, String> {
        let mut body = String::from(
            "## Describe the Bug\n\n<!-- Briefly explain what happened and how to reproduce it -->\n\n",
        );
        let diag = self.build_diagnostic_markdown(18);
        body.push_str(&diag);
        if body.chars().count() > 3600 {
            body = body.chars().take(3600).collect();
            body.push_str("\n```\n_(Truncated — paste full report from Settings if needed)_\n");
        }
        let title = format!("[BUG] LuaTools Linux v{} — ", VERSION);
        let url = format!(
            "{}?labels=bug&title={}&body={}",
            GITHUB_NEW_ISSUE_URL,
            urlencoding::encode(&title),
            urlencoding::encode(&body)
        );
        Command::new("xdg-open")
            .arg(&url)
            .spawn()
            .map_err(|e| format!("Could not launch browser via xdg-open: {}", e))?;
        Ok("Opened pre-filled GitHub Bug Report in your browser.".into())
    }
}

fn post_event_remote(
    endpoint: &str,
    install_id: &str,
    fp: &SystemFingerprint,
    ev: &TelemetryEvent,
) -> Result<(), String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .build();

    let payload = if endpoint.contains("/api/webhooks/") {
        // Discord Webhook compatible payload
        let content = format!(
            "**[LuaTools v{} · {}]** `{}/{}` ({} · {})\n```text\n{}\n```",
            ev.app_version,
            ev.level.to_ascii_uppercase(),
            ev.category,
            ev.event,
            fp.distro,
            fp.display_server,
            ev.message
        );
        json!({ "content": content })
    } else {
        json!({
            "install_id": install_id,
            "environment": fp,
            "event": ev,
        })
    };

    let resp = agent
        .post(endpoint)
        .set("User-Agent", &format!("LuaToolsLinux/{}", VERSION))
        .send_json(payload)
        .map_err(|e| format!("Telemetry POST failed: {}", e))?;

    if (200..300).contains(&resp.status()) {
        Ok(())
    } else {
        Err(format!("Endpoint returned HTTP {}", resp.status()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_http::{Response, Server};

    #[test]
    fn scrubs_paths_tokens_keys_and_respects_opt_out() {
        let dirty = format!(
            "Failed at /home/alice/.local/share/Steam with key smm_0123456789abcdef0123456789abcdef and depot {} email user@example.com Bearer secret_tok_123",
            "a9".repeat(32)
        );
        let clean = TelemetryService::scrub_sensitive_text(&dirty);
        assert!(!clean.contains("/home/alice"));
        assert!(clean.contains("~/.local/share/Steam"));
        assert!(clean.contains("smm_[REDACTED]"));
        assert!(clean.contains("[DEPOT_KEY_REDACTED]"));
        assert!(clean.contains("[EMAIL_REDACTED]"));
        assert!(clean.contains("Bearer [REDACTED]"));

        let tmp = tempfile::tempdir().unwrap();
        let svc = TelemetryService::new(
            Some(tmp.path().join("telemetry")),
            false,
            String::new(),
            "deadbeef01020304".into(),
        );

        // Opted-out: must write zero events to disk
        svc.record("error", "depot_dl", "dl_failed", "Should not be stored");
        assert_eq!(svc.event_count(), 0);
        assert!(!svc.events_file_path().exists());

        // Enable telemetry: records scrubbed event and generates diagnostic markdown
        svc.set_enabled(true);
        svc.record(
            "warn",
            "depot_dl",
            "dl_failed",
            "Error in /home/bob/Steam with key smm_abcdef0123456789",
        );
        let events = svc.recent_events(10);
        assert_eq!(events.len(), 1);
        assert!(!events[0].message.contains("/home/bob"));
        assert!(events[0].message.contains("smm_[REDACTED]"));

        let md = svc.build_diagnostic_markdown(10);
        assert!(md.contains("Environment & Runtime Diagnostics"));
        assert!(md.contains("dl_failed"));

        // Clear wipes stored data immediately
        svc.clear_all();
        assert_eq!(svc.event_count(), 0);
    }

    #[test]
    fn delivers_test_ping_to_custom_endpoint() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let handle = thread::spawn(move || -> String {
            let mut req = server
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap();
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body).unwrap();
            let _ = req.respond(Response::from_string("{\"ok\":true}").with_status_code(200));
            body
        });

        let tmp = tempfile::tempdir().unwrap();
        let svc = TelemetryService::new(
            Some(tmp.path().join("tel")),
            true,
            format!("http://127.0.0.1:{}/collect", addr.port()),
            "0102030405060708".into(),
        );
        assert!(svc.send_test_ping().is_ok());
        let received = handle.join().unwrap();
        assert!(received.contains("test_ping"));
        assert!(received.contains("0102030405060708"));
    }
}
