//! Denuvo authorization window integration for Linux.
//!
//! Port of usefulsteamtools `Pipe/Features/Auth/DenuvoAuth.cpp`.

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs;
use std::os::raw::c_void;
use std::path::Path;
use std::sync::{Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::steam_bridge::ost_platform::http;

const DEFAULT_TICKET_TTL_SECS: u32 = 86_400;
const MAX_AUTH_RESPONSE_BYTES: u32 = 64 * 1024;
const AUTH_HTTP_TIMEOUT_MS: u32 = 2_000;

fn is_http_url(url: &str) -> bool {
    url.get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("http://"))
        || url
            .get(..8)
            .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Debug, Clone)]
pub struct DenuvoAuthConfig {
    pub enabled: bool,
    pub auth_url: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub pool_steam_id: Option<u64>,
}

impl Default for DenuvoAuthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auth_url: Some("https://auth.luatools.xyz".to_string()),
            client_id: None,
            client_secret: None,
            pool_steam_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenuvoAuthResult {
    pub success: bool,
    pub ticket: Option<Vec<u8>>,
    pub error: Option<String>,
    pub expires_in: u32,
}

#[derive(Debug, Clone)]
struct StoredTicket {
    bytes: Vec<u8>,
    stored_at: u64,
    ttl_secs: u32,
}

pub struct DenuvoAuthService {
    config: DenuvoAuthConfig,
    pending_auth: Mutex<HashMap<u32, PendingAuth>>,
    /// Authorized PIDs per app_id (0 represents a wildcard/unspecified PID).
    authorized_pipes: RwLock<HashMap<u32, HashSet<u32>>>,
    stored_tickets: RwLock<HashMap<u32, StoredTicket>>,
    protected_apps: RwLock<HashSet<u32>>,
}

struct PendingAuth {
    created_at: u64,
    pid: u32,
}

impl DenuvoAuthService {
    pub fn new() -> Self {
        Self {
            config: DenuvoAuthConfig::default(),
            pending_auth: Mutex::new(HashMap::new()),
            authorized_pipes: RwLock::new(HashMap::new()),
            stored_tickets: RwLock::new(HashMap::new()),
            protected_apps: RwLock::new(HashSet::new()),
        }
    }

    pub fn configure(&mut self, config: DenuvoAuthConfig) {
        self.config = config;
    }

    /// Explicitly register a Denuvo-protected app ID.
    pub fn register_denuvo_app(&self, app_id: u32) {
        if app_id != 0 {
            self.protected_apps
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(app_id);
        }
    }

    /// Unregister an explicitly registered Denuvo app ID.
    pub fn unregister_denuvo_app(&self, app_id: u32) {
        self.protected_apps
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&app_id);
    }

    /// Register a pipe handshake for a Denuvo game.
    pub fn on_handshake(&self, app_id: u32, pid: u32) -> bool {
        if !self.config.enabled || app_id == 0 {
            return false;
        }

        if self.is_denuvo_protected(app_id) {
            let mut pending = self.pending_auth.lock().unwrap_or_else(|e| e.into_inner());
            let now = now_unix_secs();

            pending.insert(
                app_id,
                PendingAuth {
                    created_at: now,
                    pid,
                },
            );

            log::debug!(
                "[denuvo] detected Denuvo game {} (pid={}), scheduling auth",
                app_id,
                pid
            );
            true
        } else {
            false
        }
    }

    pub fn is_denuvo_protected(&self, app_id: u32) -> bool {
        if app_id == 0 || self.config.pool_steam_id.is_none() {
            return false;
        }
        let protected = self
            .protected_apps
            .read()
            .unwrap_or_else(|e| e.into_inner());
        protected.is_empty() || protected.contains(&app_id)
    }

    pub fn is_authorized_pipe(&self, app_id: u32, pid: u32) -> bool {
        if app_id == 0 {
            return false;
        }
        let auth_pipes = self
            .authorized_pipes
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let Some(pids) = auth_pipes.get(&app_id) else {
            return false;
        };
        if pid == 0 {
            !pids.is_empty()
        } else {
            pids.contains(&pid) || pids.contains(&0)
        }
    }

    pub fn apply(&self, app_id: u32, ticket: Option<Vec<u8>>) {
        if app_id == 0 {
            return;
        }

        // Release `pending_auth` lock before acquiring RwLocks to avoid nested lock ordering.
        let pending_auth = {
            let mut pending = self.pending_auth.lock().unwrap_or_else(|e| e.into_inner());
            pending.remove(&app_id)
        };

        let Some(authorized_pid) = pending_auth.map(|p| {
            log::debug!(
                "[denuvo] auth applied for app {} (pid={}, age: {}s)",
                app_id,
                p.pid,
                now_unix_secs().saturating_sub(p.created_at)
            );
            p.pid
        }) else {
            // Allow storing an explicit non-empty ticket even without a prior handshake,
            // authorizing wildcard PID 0.
            if let Some(ticket_data) = ticket.filter(|t| !t.is_empty()) {
                self.store_auth_ticket(app_id, &ticket_data);
                self.authorized_pipes
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .entry(app_id)
                    .or_default()
                    .insert(0);
            }
            return;
        };

        self.authorized_pipes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(app_id)
            .or_default()
            .insert(authorized_pid);

        if let Some(ticket_data) = ticket.filter(|t| !t.is_empty()) {
            self.store_auth_ticket(app_id, &ticket_data);
        }
    }

    /// Revoke authorization and discard any cached Denuvo ticket for an app.
    pub fn revoke(&self, app_id: u32) {
        self.pending_auth
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&app_id);
        self.authorized_pipes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&app_id);
        self.stored_tickets
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&app_id);
    }

    fn store_auth_ticket(&self, app_id: u32, ticket: &[u8]) {
        self.store_auth_ticket_with_ttl(app_id, ticket, DEFAULT_TICKET_TTL_SECS);
    }

    fn store_auth_ticket_with_ttl(&self, app_id: u32, ticket: &[u8], ttl_secs: u32) {
        if ticket.is_empty() {
            return;
        }
        let effective_ttl = ttl_secs.max(1);
        let mut store = self
            .stored_tickets
            .write()
            .unwrap_or_else(|e| e.into_inner());
        store.insert(
            app_id,
            StoredTicket {
                bytes: ticket.to_vec(),
                stored_at: now_unix_secs(),
                ttl_secs: effective_ttl,
            },
        );
        log::debug!(
            "[denuvo] stored auth ticket for app {} ({} bytes, ttl={}s)",
            app_id,
            ticket.len(),
            effective_ttl
        );
    }

    /// Fetch a Denuvo ticket from `config.auth_url` via `ost_platform::http::Execute`.
    fn fetch_remote_auth_ticket(
        &self,
        app_id: u32,
        pool_steam_id: u64,
        nonce: &[u8],
    ) -> Result<(Vec<u8>, u32), String> {
        let base_url = self
            .config
            .auth_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .ok_or_else(|| "No auth_url configured".to_string())?;

        if !is_http_url(base_url) {
            return Err(format!("Unsupported auth_url scheme: {}", base_url));
        }

        let target_url = format!("{}/ticket/{}", base_url.trim_end_matches('/'), app_id);
        let payload = serde_json::json!({
            "app_id": app_id,
            "steam_id": pool_steam_id.to_string(),
            "nonce": hex::encode(nonce),
        });
        let body_str = payload.to_string();
        let body_bytes = body_str.as_bytes();
        let body_len = u32::try_from(body_bytes.len())
            .map_err(|_| "Auth request payload exceeds u32 length".to_string())?;

        let mut header_lines = String::from("Content-Type: application/json\r\nAccept: application/json, text/plain\r\n");
        if let Some(cid) = self.config.client_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let clean = cid.replace(['\r', '\n'], "");
            header_lines.push_str(&format!("X-Client-Id: {}\r\n", clean));
        }
        if let Some(sec) = self
            .config
            .client_secret
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let clean = sec.replace(['\r', '\n'], "");
            header_lines.push_str(&format!("X-Client-Secret: {}\r\n", clean));
        }
        let headers_cstr = CString::new(header_lines)
            .map_err(|_| "Auth headers contain interior NUL byte".to_string())?;

        let resp = http::Execute(
            "POST",
            &target_url,
            body_bytes.as_ptr() as *const c_void,
            body_len,
            headers_cstr.as_ptr(),
            AUTH_HTTP_TIMEOUT_MS,
            AUTH_HTTP_TIMEOUT_MS,
            AUTH_HTTP_TIMEOUT_MS,
            AUTH_HTTP_TIMEOUT_MS,
            MAX_AUTH_RESPONSE_BYTES,
        );

        if !resp.ok {
            let snippet: String = resp.body.chars().take(160).collect();
            return Err(format!(
                "Remote Denuvo auth failed (status={}): {}",
                resp.status, snippet
            ));
        }

        let trimmed = resp.body.trim();
        if trimmed.is_empty() {
            return Err("Remote Denuvo auth returned empty body".to_string());
        }

        if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if json.get("success").and_then(|v| v.as_bool()) == Some(false) {
                let err_msg = json
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Remote auth rejected request");
                return Err(err_msg.to_string());
            }

            let ticket_str = json
                .get("ticket")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "Remote Denuvo auth JSON missing non-empty 'ticket'".to_string())?;

            let ticket_bytes =
                hex::decode(ticket_str).unwrap_or_else(|_| ticket_str.as_bytes().to_vec());
            let ttl = json
                .get("expires_in")
                .and_then(|v| v.as_u64())
                .map(|v| v.clamp(1, u64::from(u32::MAX)) as u32)
                .unwrap_or(DEFAULT_TICKET_TTL_SECS);
            return Ok((ticket_bytes, ttl));
        }

        let ticket_bytes = hex::decode(trimmed).unwrap_or_else(|_| trimmed.as_bytes().to_vec());
        if ticket_bytes.is_empty() {
            return Err("Remote Denuvo auth returned empty ticket".to_string());
        }
        Ok((ticket_bytes, DEFAULT_TICKET_TTL_SECS))
    }

    pub fn fetch_auth_ticket(&self, app_id: u32, nonce: &[u8]) -> Option<DenuvoAuthResult> {
        if !self.config.enabled || app_id == 0 {
            return None;
        }

        let pool_sid = self.config.pool_steam_id?;
        let now = now_unix_secs();

        log::debug!(
            "[denuvo] fetching auth ticket for app {}, nonce len={}",
            app_id,
            nonce.len()
        );

        let stored = {
            let store = self
                .stored_tickets
                .read()
                .unwrap_or_else(|e| e.into_inner());
            store.get(&app_id).cloned()
        };

        if let Some(entry) = stored {
            let elapsed = now.saturating_sub(entry.stored_at);
            if elapsed < u64::from(entry.ttl_secs) {
                let remaining =
                    (u64::from(entry.ttl_secs) - elapsed).min(u64::from(u32::MAX)) as u32;
                let mut payload = entry.bytes;
                if !nonce.is_empty() {
                    payload.extend_from_slice(nonce);
                }
                return Some(DenuvoAuthResult {
                    success: true,
                    ticket: Some(payload),
                    error: None,
                    expires_in: remaining,
                });
            }
        }

        if self
            .config
            .auth_url
            .as_deref()
            .is_some_and(|u| !u.trim().is_empty())
        {
            match self.fetch_remote_auth_ticket(app_id, pool_sid, nonce) {
                Ok((ticket_bytes, ttl)) => {
                    self.store_auth_ticket_with_ttl(app_id, &ticket_bytes, ttl);
                    let mut payload = ticket_bytes;
                    if !nonce.is_empty() {
                        payload.extend_from_slice(nonce);
                    }
                    return Some(DenuvoAuthResult {
                        success: true,
                        ticket: Some(payload),
                        error: None,
                        expires_in: ttl,
                    });
                }
                Err(err) => {
                    return Some(DenuvoAuthResult {
                        success: false,
                        ticket: None,
                        error: Some(err),
                        expires_in: 0,
                    });
                }
            }
        }

        Some(DenuvoAuthResult {
            success: false,
            ticket: None,
            error: Some(format!(
                "No active Denuvo auth ticket cached for app {}",
                app_id
            )),
            expires_in: 0,
        })
    }

    pub fn cleanup_expired(&self, max_age_secs: u64) {
        let now = now_unix_secs();

        {
            let mut pending = self.pending_auth.lock().unwrap_or_else(|e| e.into_inner());
            pending.retain(|_, auth| now.saturating_sub(auth.created_at) < max_age_secs);
        }

        {
            let mut store = self
                .stored_tickets
                .write()
                .unwrap_or_else(|e| e.into_inner());
            store.retain(|_, t| {
                let age = now.saturating_sub(t.stored_at);
                age < max_age_secs && age < u64::from(t.ttl_secs)
            });
        }
    }

    pub fn get_pool_steam_id(&self, app_id: u32) -> Option<u64> {
        if app_id == 0 {
            return None;
        }
        self.config.pool_steam_id
    }

    pub fn should_force_denuvo(&self, app_id: u32) -> bool {
        self.config.enabled && self.is_denuvo_protected(app_id)
    }
}

impl Default for DenuvoAuthService {
    fn default() -> Self {
        Self::new()
    }
}

pub struct DenuvoAuth {
    service: DenuvoAuthService,
}

impl Default for DenuvoAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl DenuvoAuth {
    pub fn new() -> Self {
        Self {
            service: DenuvoAuthService::new(),
        }
    }

    pub fn configure(&mut self, config: DenuvoAuthConfig) {
        self.service.configure(config);
    }

    pub fn on_handshake(&self, app_id: u32, pid: u32) -> bool {
        self.service.on_handshake(app_id, pid)
    }

    pub fn is_authorized(&self, app_id: u32, pid: u32) -> bool {
        self.service.is_authorized_pipe(app_id, pid)
    }

    pub fn apply(&self, app_id: u32, ticket: Option<Vec<u8>>) {
        self.service.apply(app_id, ticket);
    }

    pub fn set_pool_steam_id(&mut self, steam_id: u64) {
        self.service.config.pool_steam_id = Some(steam_id);
    }

    pub fn set_auth_url(&mut self, url: String) {
        self.service.config.auth_url = Some(url);
    }

    pub fn is_denuvo_game(&self, app_id: u32) -> bool {
        self.service.is_denuvo_protected(app_id)
    }

    pub fn register_denuvo_app(&self, app_id: u32) {
        self.service.register_denuvo_app(app_id);
    }

    pub fn unregister_denuvo_app(&self, app_id: u32) {
        self.service.unregister_denuvo_app(app_id);
    }

    pub fn should_force_denuvo(&self, app_id: u32) -> bool {
        self.service.should_force_denuvo(app_id)
    }

    pub fn get_pool_steam_id(&self, app_id: u32) -> Option<u64> {
        self.service.get_pool_steam_id(app_id)
    }

    pub fn revoke(&self, app_id: u32) {
        self.service.revoke(app_id);
    }

    pub fn cleanup_expired(&self, max_age_secs: u64) {
        self.service.cleanup_expired(max_age_secs);
    }

    pub fn fetch_auth_ticket(&self, app_id: u32, nonce: &[u8]) -> Option<DenuvoAuthResult> {
        self.service.fetch_auth_ticket(app_id, nonce)
    }

    /// Scan a game executable for Denuvo protection (BST-54).
    pub fn scan_executable(&self, exe_path: &Path) -> ProtectionScanReport {
        self.service.scan_executable(exe_path)
    }

    /// Recursively scan a game directory for Denuvo-protected executables and auto-register (BST-54).
    pub fn scan_game_folder(&self, app_id: u32, game_dir: &Path) -> ProtectionScanReport {
        self.service.scan_game_folder(app_id, game_dir)
    }
}

/// Denuvo heuristic detection method (BST-54).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenuvoDetectionMethod {
    None,
    LegacySectionString,
    OepPattern,
    ProtectedBlobSection,
}

/// Protection scan report matching usefulsteamtools::ProtectionScanReport (BST-54).
#[derive(Debug, Clone)]
pub struct ProtectionScanReport {
    pub denuvo_detected: bool,
    pub method: DenuvoDetectionMethod,
    pub section_name: Option<String>,
    pub file_size: u64,
}

impl DenuvoAuthService {
    /// Scan a PE executable for Denuvo signatures (BST-54).
    pub fn scan_executable(&self, exe_path: &Path) -> ProtectionScanReport {
        scan_protection_pe(exe_path)
    }

    /// Scan a game folder for any Denuvo-protected PE executable and auto-register the app (BST-54).
    pub fn scan_game_folder(&self, app_id: u32, game_dir: &Path) -> ProtectionScanReport {
        if let Ok(entries) = fs::read_dir(game_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file()
                    && p.extension()
                        .map(|e| e.eq_ignore_ascii_case("exe"))
                        .unwrap_or(false)
                {
                    let report = scan_protection_pe(&p);
                    if report.denuvo_detected {
                        self.register_denuvo_app(app_id);
                        return report;
                    }
                }
            }
        }
        ProtectionScanReport {
            denuvo_detected: false,
            method: DenuvoDetectionMethod::None,
            section_name: None,
            file_size: 0,
        }
    }
}

/// Core PE binary scanner for Denuvo protection signatures (BST-54).
///
/// Ported from `usefulsteamtools::PipeManager::DenuvoAuth::RunProtectionScan`:
/// 1. OEP Pattern scan: `48 B9 44 4F 44 45 4E 55 56 4F` (`mov rcx, 'DODENUVO'`)
/// 2. Legacy Section String scan: `.arch`, `.srdata`, `.xpdata`, `.xdata`, `.xtls` containing `"DENUVO"`
/// 3. Protected W+X blob section: simultaneous write + execute flags with packed multi-MB payload
pub fn scan_protection_pe(exe_path: &Path) -> ProtectionScanReport {
    let path_str = match exe_path.to_str() {
        Some(s) => s,
        None => {
            return ProtectionScanReport {
                denuvo_detected: false,
                method: DenuvoDetectionMethod::None,
                section_name: None,
                file_size: 0,
            }
        }
    };
    let image = match crate::steam_bridge::ost_platform::pe::Image::new(path_str) {
        Ok(img) if img.is_valid() => img,
        _ => {
            return ProtectionScanReport {
                denuvo_detected: false,
                method: DenuvoDetectionMethod::None,
                section_name: None,
                file_size: 0,
            }
        }
    };

    let file_bytes = image.bytes();
    let file_size = image.file_size();

    // 1. OEP Pattern scan: 48 B9 44 4F 44 45 4E 55 56 4F (mov rcx, 'DODENUVO')
    const DENUVO_OEP_PATTERN: [u8; 10] = [
        0x48, 0xB9, 0x44, 0x4F, 0x44, 0x45, 0x4E, 0x55, 0x56, 0x4F,
    ];
    if file_bytes
        .windows(DENUVO_OEP_PATTERN.len())
        .any(|w| w == DENUVO_OEP_PATTERN)
    {
        let ep_rva = image.entry_point_rva();
        let sec_name = image.section_containing_rva(ep_rva).map(|s| s.name.clone());
        return ProtectionScanReport {
            denuvo_detected: true,
            method: DenuvoDetectionMethod::OepPattern,
            section_name: sec_name,
            file_size,
        };
    }

    // 2. Legacy Section String scan (".arch", ".srdata", ".xpdata", ".xdata", ".xtls" containing "DENUVO")
    const LEGACY_SECTIONS: &[&str] = &[".arch", ".srdata", ".xpdata", ".xdata", ".xtls"];
    for sec in image.sections() {
        let name_lower = sec.name.to_ascii_lowercase();
        if LEGACY_SECTIONS.iter().any(|&ls| name_lower.starts_with(ls)) {
            let offset = sec.raw_offset as usize;
            let size = sec.raw_size as usize;
            if let Some(sec_slice) = file_bytes.get(offset..offset.saturating_add(size)) {
                if sec_slice
                    .windows(6)
                    .any(|w| w == b"DENUVO" || w == b"denuvo")
                {
                    return ProtectionScanReport {
                        denuvo_detected: true,
                        method: DenuvoDetectionMethod::LegacySectionString,
                        section_name: Some(sec.name.clone()),
                        file_size,
                    };
                }
            }
        }
    }

    // 3. Protected W+X blob section (executable + writable with multi-MB payload)
    for sec in image.sections() {
        if sec.is_executable_and_writable() && sec.raw_size >= 1_000_000 {
            return ProtectionScanReport {
                denuvo_detected: true,
                method: DenuvoDetectionMethod::ProtectedBlobSection,
                section_name: Some(sec.name.clone()),
                file_size,
            };
        }
    }

    ProtectionScanReport {
        denuvo_detected: false,
        method: DenuvoDetectionMethod::None,
        section_name: None,
        file_size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_apply_pid_isolation_and_ticket_fetch() {
        let mut auth = DenuvoAuthService::new();
        assert!(!auth.is_denuvo_protected(1245620));
        assert!(!auth.on_handshake(1245620, 4242));

        auth.configure(DenuvoAuthConfig {
            enabled: true,
            auth_url: None,
            pool_steam_id: Some(76_561_198_028_121_353),
            ..Default::default()
        });
        auth.register_denuvo_app(1245620);

        assert!(auth.is_denuvo_protected(1245620));
        assert!(!auth.is_denuvo_protected(730));
        assert!(!auth.is_denuvo_protected(0));

        // Uncached fetch returns success=false instead of fabricating success=true with None
        let miss = auth.fetch_auth_ticket(1245620, b"nonce").unwrap();
        assert!(!miss.success);
        assert!(miss.ticket.is_none());

        assert!(auth.on_handshake(1245620, 4242));
        assert!(!auth.is_authorized_pipe(1245620, 4242));

        auth.apply(1245620, Some(vec![0xAA, 0xBB, 0xCC]));
        assert!(auth.is_authorized_pipe(1245620, 4242));
        assert!(!auth.is_authorized_pipe(1245620, 9999));
        assert!(auth.is_authorized_pipe(1245620, 0));

        let hit = auth.fetch_auth_ticket(1245620, &[0x01, 0x02]).unwrap();
        assert!(hit.success);
        assert_eq!(hit.ticket, Some(vec![0xAA, 0xBB, 0xCC, 0x01, 0x02]));
        assert!(hit.expires_in > 0);

        // Expire immediately with max_age_secs = 0
        auth.cleanup_expired(0);
        let expired = auth.fetch_auth_ticket(1245620, &[]).unwrap();
        assert!(!expired.success);

        auth.revoke(1245620);
        assert!(!auth.is_authorized_pipe(1245620, 4242));
    }

    #[test]
    fn disabled_config_blocks_handshake_and_fetch() {
        let mut auth = DenuvoAuth::new();
        auth.configure(DenuvoAuthConfig {
            enabled: false,
            pool_steam_id: Some(76_561_198_028_121_353),
            ..Default::default()
        });
        assert!(!auth.on_handshake(1245620, 100));
        assert!(auth.fetch_auth_ticket(1245620, b"n").is_none());
    }

    #[test]
    fn remote_fetch_rejects_invalid_scheme_offline() {
        let mut auth = DenuvoAuth::new();
        auth.configure(DenuvoAuthConfig {
            enabled: true,
            auth_url: Some("ftp://invalid.example".to_string()),
            pool_steam_id: Some(76_561_198_028_121_353),
            ..Default::default()
        });
        let res = auth.fetch_auth_ticket(1245620, b"nonce").unwrap();
        assert!(!res.success);
        assert!(res.ticket.is_none());
        assert!(res
            .error
            .as_deref()
            .is_some_and(|e| e.contains("Unsupported auth_url scheme")));
    }

    #[test]
    fn remote_fetch_closed_loopback_port_fails_fast() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let mut auth = DenuvoAuth::new();
        auth.configure(DenuvoAuthConfig {
            enabled: true,
            auth_url: Some(format!("http://{}", addr)),
            pool_steam_id: Some(76_561_198_028_121_353),
            ..Default::default()
        });
        let res = auth.fetch_auth_ticket(1245620, b"nonce").unwrap();
        assert!(!res.success);
        assert!(res.ticket.is_none());
        assert!(res.error.is_some());
    }

    #[test]
    fn remote_fetch_succeeds_against_local_http_and_caches() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut req_buf = [0u8; 1024];
                let _ = stream.read(&mut req_buf);
                let body = r#"{"success":true,"ticket":"deadbeef","expires_in":3600}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        let mut auth = DenuvoAuth::new();
        auth.configure(DenuvoAuthConfig {
            enabled: true,
            auth_url: Some(format!("http://{}", addr)),
            client_id: Some("lt-test".to_string()),
            client_secret: None,
            pool_steam_id: Some(76_561_198_028_121_353),
        });
        auth.register_denuvo_app(1245620);

        let first = auth.fetch_auth_ticket(1245620, &[0x01, 0x02]).unwrap();
        assert!(first.success);
        assert_eq!(first.ticket, Some(vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02]));
        assert_eq!(first.expires_in, 3600);

        // Second call must hit local cache even though the single-accept server has exited.
        let second = auth.fetch_auth_ticket(1245620, &[0x03]).unwrap();
        assert!(second.success);
        assert_eq!(second.ticket, Some(vec![0xDE, 0xAD, 0xBE, 0xEF, 0x03]));

        handle.join().unwrap();
    }

    #[test]
    fn test_denuvo_protection_scan_heuristics() {
        let tmp = tempfile::tempdir().unwrap();

        // 1. Non-PE file returns no detection
        let dummy = tmp.path().join("plain.exe");
        fs::write(&dummy, b"NOT A PE").unwrap();
        let rep1 = scan_protection_pe(&dummy);
        assert!(!rep1.denuvo_detected);

        // 2. Build synthetic PE64 with OEP pattern
        let mut pe = vec![0u8; 0x1000];
        pe[0..2].copy_from_slice(&0x5A4Du16.to_le_bytes()); // MZ
        pe[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        pe[0x80..0x84].copy_from_slice(&0x0000_4550u32.to_le_bytes()); // PE00
        let fh = 0x84;
        pe[fh + 2..fh + 4].copy_from_slice(&1u16.to_le_bytes()); // 1 section
        pe[fh + 16..fh + 18].copy_from_slice(&240u16.to_le_bytes()); // opt header size
        let opt = fh + 20;
        pe[opt..opt + 2].copy_from_slice(&0x20Bu16.to_le_bytes()); // PE64
        let sec = opt + 240;
        pe[sec..sec + 5].copy_from_slice(b".text");
        pe[sec + 8..sec + 12].copy_from_slice(&0x200u32.to_le_bytes());
        pe[sec + 12..sec + 16].copy_from_slice(&0x1000u32.to_le_bytes()); // RVA
        pe[sec + 16..sec + 20].copy_from_slice(&0x400u32.to_le_bytes()); // raw size
        pe[sec + 20..sec + 24].copy_from_slice(&0x400u32.to_le_bytes()); // raw offset
        pe[sec + 36..sec + 40].copy_from_slice(&0x6000_0020u32.to_le_bytes());

        // Plant OEP pattern in code
        let pattern = [0x48, 0xB9, 0x44, 0x4F, 0x44, 0x45, 0x4E, 0x55, 0x56, 0x4F];
        pe[0x500..0x50A].copy_from_slice(&pattern);

        let oep_exe = tmp.path().join("oep_denuvo.exe");
        fs::write(&oep_exe, &pe).unwrap();
        let rep2 = scan_protection_pe(&oep_exe);
        assert!(rep2.denuvo_detected);
        assert_eq!(rep2.method, DenuvoDetectionMethod::OepPattern);
    }
}
