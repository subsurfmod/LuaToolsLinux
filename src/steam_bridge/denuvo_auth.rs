//! Denuvo authorization window integration and PE protection scanner for Linux (BST-54, BST-55).
//!
//! Port of usefulsteamtools `Pipe/Features/DenuvoAuth/DenuvoAuth.cpp` and `ProtectionScan.cpp`.

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs;
use std::os::raw::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::steam_bridge::ost_platform::http;

const DEFAULT_TICKET_TTL_SECS: u32 = 86_400;
const MAX_AUTH_RESPONSE_BYTES: u32 = 64 * 1024;
const AUTH_HTTP_TIMEOUT_MS: u32 = 2_000;

/// Minimum candidate packed module size on disk (80 MiB = 83,886,080 bytes) per BST-54.
pub const MIN_PACKED_MODULE_BYTES: u64 = 80 * 1024 * 1024;

/// Minimum raw size of a W+X protected section blob (4 MiB = 4,194,304 bytes) per BST-54.
pub const PROTECTOR_BLOB_MIN_BYTES: u32 = 4 * 1024 * 1024;

/// Shannon entropy floor for W+X protected blobs (6.0 bits/byte) per BST-54.
pub const PROTECTOR_BLOB_MIN_ENTROPY: f64 = 6.0;

/// Shannon entropy threshold for high-confidence encrypted blobs (7.0 bits/byte).
pub const PROTECTOR_BLOB_HIGH_CONFIDENCE_ENTROPY: f64 = 7.0;

/// Maximum bytes sampled from a section for entropy calculation (8 MiB).
pub const PROTECTOR_BLOB_ENTROPY_SAMPLE_BYTES: usize = 8 * 1024 * 1024;

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

/// Computes Shannon byte entropy (bits/byte) over a byte slice:
/// H = - sum_{b=0}^{255} p_b * log2(p_b)
/// Ranges from 0.0 (all bytes identical) to 8.0 (uniform random / packed).
pub fn calculate_shannon_entropy(bytes: &[u8]) -> f64 {
    if bytes.is_empty() {
        return 0.0;
    }
    let mut counts = [0u64; 256];
    for &b in bytes {
        counts[b as usize] += 1;
    }
    let total = bytes.len() as f64;
    let mut entropy = 0.0;
    for &count in &counts {
        if count > 0 {
            let p = count as f64 / total;
            entropy -= p * p.log2();
        }
    }
    entropy
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

/// Stage in the 2-handshake Denuvo authorization window state machine (BST-55).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenuvoStage {
    None,
    Authorizing,
    EndAuthorization,
}

/// State tracking per process for the Denuvo authorization window (BST-55).
#[derive(Debug, Clone)]
pub struct ProcessAuthState {
    pub scanned: bool,
    pub denuvo: bool,
    pub stage: DenuvoStage,
    pub pid: u32,
    pub handshake_count: u32,
    pub authorization_pipe: Option<u32>,
    pub authorized_app_id: u32,
}

#[derive(Debug)]
struct PendingAuth {
    created_at: u64,
    pid: u32,
}

#[derive(Debug)]
pub struct DenuvoAuthService {
    config: DenuvoAuthConfig,
    pending_auth: Mutex<HashMap<u32, PendingAuth>>,
    /// Authorized PIDs per app_id (0 represents a wildcard/unspecified PID).
    authorized_pipes: RwLock<HashMap<u32, HashSet<u32>>>,
    stored_tickets: RwLock<HashMap<u32, StoredTicket>>,
    protected_apps: RwLock<HashSet<u32>>,
    /// 2-handshake state machine tracking per PID (BST-55)
    process_auth: Mutex<HashMap<u32, ProcessAuthState>>,
    /// Pipe handle to PID reverse mapping (BST-55)
    pipe_to_pid: Mutex<HashMap<u32, u32>>,
}

impl DenuvoAuthService {
    pub fn new() -> Self {
        Self {
            config: DenuvoAuthConfig::default(),
            pending_auth: Mutex::new(HashMap::new()),
            authorized_pipes: RwLock::new(HashMap::new()),
            stored_tickets: RwLock::new(HashMap::new()),
            protected_apps: RwLock::new(HashSet::new()),
            process_auth: Mutex::new(HashMap::new()),
            pipe_to_pid: Mutex::new(HashMap::new()),
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

    /// Check if an app is Denuvo-protected (either registered or forced).
    pub fn is_denuvo_protected(&self, app_id: u32) -> bool {
        if app_id == 0 || self.config.pool_steam_id.is_none() {
            return false;
        }
        let protected = self
            .protected_apps
            .read()
            .unwrap_or_else(|e| e.into_inner());
        protected.contains(&app_id)
    }

    /// Lazy one-time protection scanner for process auth state (BST-54/55).
    fn ensure_scanned(&self, auth: &mut ProcessAuthState, pid: u32, app_id: u32) {
        // Always check if app is explicitly registered/forced as Denuvo, even if already scanned
        let is_denuvo_app = self.should_force_denuvo(app_id) || self.is_denuvo_protected(app_id);

        if auth.scanned && !is_denuvo_app {
            // Previously scanned and not a known Denuvo app - keep existing result
            return;
        }

        // Not yet scanned OR app is now registered as Denuvo - (re)scan
        auth.scanned = true;
        if is_denuvo_app {
            auth.denuvo = true;
            log::info!("[denuvo] AppID {} is forced/registered Denuvo; skipping binary scan", app_id);
        } else {
            let report = scan_process(pid);
            auth.denuvo = report.denuvo_detected;
            if auth.denuvo {
                log::info!(
                    "[denuvo] Detected Denuvo in PID {} for AppID {} via {:?}",
                    pid,
                    app_id,
                    report.method
                );
                self.register_denuvo_app(app_id);
            }
        }
        if !auth.denuvo {
            auth.stage = DenuvoStage::None;
        }
    }

    /// Register a pipe handshake with pipe handle and client PID (BST-55).
    pub fn on_handshake_pipe(&self, pipe_id: u32, app_id: u32, pid: u32) -> bool {
        if !self.config.enabled || app_id == 0 {
            return false;
        }

        let mut process_auth = self.process_auth.lock().unwrap_or_else(|e| e.into_inner());
        let mut pipe_to_pid = self.pipe_to_pid.lock().unwrap_or_else(|e| e.into_inner());
        if pipe_id != 0 {
            pipe_to_pid.insert(pipe_id, pid);
        }

        let auth = process_auth.entry(pid).or_insert_with(|| ProcessAuthState {
            scanned: false,
            denuvo: false,
            stage: DenuvoStage::None,
            pid,
            handshake_count: 0,
            authorization_pipe: None,
            authorized_app_id: app_id,
        });

        self.ensure_scanned(auth, pid, app_id);
        auth.handshake_count += 1;

        if !auth.denuvo {
            auth.stage = DenuvoStage::None;
            return false;
        }

        // Handshake 1: Open 2-handshake authorization window
        if auth.authorization_pipe.is_none() {
            auth.authorization_pipe = Some(pipe_id);
            auth.authorized_app_id = app_id;
            auth.pid = pid;
            auth.stage = DenuvoStage::Authorizing;
            log::info!(
                "[denuvo] Handshake #1: authorization window opened for app {} (pid={}, pipe={})",
                app_id, pid, pipe_id
            );
        }

        // Handshake 2+: End authorization window permanently
        if auth.stage == DenuvoStage::Authorizing && auth.handshake_count >= 2 {
            auth.stage = DenuvoStage::EndAuthorization;
            log::info!(
                "[denuvo] Handshake #2+: authorization window ended for app {} (pid={}, pipe={})",
                app_id, pid, pipe_id
            );
            self.write_steam_id_on_end_authorization(app_id);
        }

        // Keep legacy pending_auth queue for fallback HTTP token collectors
        let mut pending = self.pending_auth.lock().unwrap_or_else(|e| e.into_inner());
        pending.insert(
            app_id,
            PendingAuth {
                created_at: now_unix_secs(),
                pid,
            },
        );

        true
    }

    /// Convenience wrapper for handshake without an explicit pipe handle.
    pub fn on_handshake(&self, app_id: u32, pid: u32) -> bool {
        self.on_handshake_pipe(0, app_id, pid)
    }

    /// Check if a pipe is currently in the Denuvo authorization window (BST-55).
    pub fn is_authorized_pipe(&self, pipe_id: u32, app_id: u32, pid: u32) -> bool {
        if app_id == 0 {
            return false;
        }

        let effective_pid = if pid != 0 {
            pid
        } else if pipe_id != 0 {
            let pipe_map = self.pipe_to_pid.lock().unwrap_or_else(|e| e.into_inner());
            pipe_map.get(&pipe_id).copied().unwrap_or(0)
        } else {
            0
        };

        let process_auth = self.process_auth.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(auth) = process_auth.get(&effective_pid) {
            if auth.denuvo && auth.stage == DenuvoStage::Authorizing {
                if pipe_id == 0 || auth.authorization_pipe == Some(pipe_id) || auth.authorization_pipe == Some(0) {
                    return true;
                }
            }
        }

        // Fallback check against manual authorized_pipes
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

    /// Persist current Pool SteamID on handshake end-authorization (BST-55).
    fn write_steam_id_on_end_authorization(&self, app_id: u32) {
        if let Some(sid) = self.config.pool_steam_id {
            if sid != 0 {
                let mut store = crate::steam_bridge::ticket_store::TicketStore::global()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                store.set_spoof_steam_id(app_id, sid);
                log::info!(
                    "[denuvo] Persisted end-authorization SteamID 0x{:X} for app {}",
                    sid,
                    app_id
                );
            }
        }
    }

    pub fn apply(&self, app_id: u32, ticket: Option<Vec<u8>>) {
        if app_id == 0 {
            return;
        }

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
        if let Some(sec) = self.config.client_secret.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
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

    /// Fetches an uncorrupted Denuvo auth ticket (BST-55).
    ///
    /// Cryptographic note: Ticket bytes are returned EXACTLY as issued by the signer.
    /// Under NO circumstances are raw nonce/steam_id bytes appended onto the payload,
    /// which would corrupt the ASN.1 / RSA signature structures.
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
                return Some(DenuvoAuthResult {
                    success: true,
                    ticket: Some(entry.bytes),
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
                    return Some(DenuvoAuthResult {
                        success: true,
                        ticket: Some(ticket_bytes),
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

    /// Scan a candidate PE executable for Denuvo protection with the >=80 MiB gate (BST-54).
    pub fn scan_executable(&self, exe_path: &Path) -> ProtectionScanReport {
        let size = fs::metadata(exe_path).map(|m| m.len()).unwrap_or(0);
        if size > 0 && size < MIN_PACKED_MODULE_BYTES {
            log::trace!(
                "[denuvo_scan] Skipping {} ({} bytes < 80 MiB floor)",
                exe_path.display(),
                size
            );
            return ProtectionScanReport {
                denuvo_detected: false,
                method: DenuvoDetectionMethod::None,
                module_path: Some(exe_path.to_string_lossy().into_owned()),
                file_size: size,
                module_size: size,
                ..Default::default()
            };
        }
        let mut rep = scan_protection_pe(exe_path);
        rep.module_path = Some(exe_path.to_string_lossy().into_owned());
        rep
    }

    /// Scan a game folder for any Denuvo-protected PE executable >=80 MiB (BST-54).
    pub fn scan_game_folder(&self, app_id: u32, game_dir: &Path) -> ProtectionScanReport {
        if let Ok(entries) = fs::read_dir(game_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file()
                    && p.extension()
                        .map(|e| e.eq_ignore_ascii_case("exe"))
                        .unwrap_or(false)
                {
                    let size = p.metadata().map(|m| m.len()).unwrap_or(0);
                    if size >= MIN_PACKED_MODULE_BYTES {
                        let report = scan_protection_pe(&p);
                        if report.denuvo_detected {
                            self.register_denuvo_app(app_id);
                            return report;
                        }
                    }
                }
            }
        }
        ProtectionScanReport::default()
    }
}

impl Default for DenuvoAuthService {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
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

    pub fn on_handshake_pipe(&self, pipe_id: u32, app_id: u32, pid: u32) -> bool {
        self.service.on_handshake_pipe(pipe_id, app_id, pid)
    }

    pub fn on_handshake(&self, app_id: u32, pid: u32) -> bool {
        self.service.on_handshake(app_id, pid)
    }

    pub fn is_authorized_pipe(&self, pipe_id: u32, app_id: u32, pid: u32) -> bool {
        self.service.is_authorized_pipe(pipe_id, app_id, pid)
    }

    pub fn is_authorized(&self, app_id: u32, pid: u32) -> bool {
        self.service.is_authorized_pipe(0, app_id, pid)
    }

    pub fn apply(&self, app_id: u32, ticket: Option<Vec<u8>>) {
        self.service.apply(app_id, ticket);
    }

    pub fn set_pool_steam_id(&mut self, steam_id: u64) {
        self.service.config.pool_steam_id = Some(steam_id);
    }

    pub fn pool_steam_id(&self) -> Option<u64> {
        self.service.config.pool_steam_id
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

    pub fn scan_executable(&self, exe_path: &Path) -> ProtectionScanReport {
        self.service.scan_executable(exe_path)
    }

    pub fn scan_game_folder(&self, app_id: u32, game_dir: &Path) -> ProtectionScanReport {
        self.service.scan_game_folder(app_id, game_dir)
    }
}

static GLOBAL_DENUVO_AUTH: OnceLock<RwLock<DenuvoAuth>> = OnceLock::new();

/// Global singleton DenuvoAuth instance.
pub fn global() -> &'static RwLock<DenuvoAuth> {
    GLOBAL_DENUVO_AUTH.get_or_init(|| RwLock::new(DenuvoAuth::new()))
}

/// Convenience dispatcher for IPC handshake hooking.
pub fn on_pipe_handshake(pipe_id: u32, app_id: u32, pid: u32) -> bool {
    let auth = global().read().unwrap_or_else(|e| e.into_inner());
    auth.on_handshake_pipe(pipe_id, app_id, pid)
}

/// Convenience query for pipe authorization.
pub fn is_pipe_authorized(pipe_id: u32, app_id: u32, pid: u32) -> bool {
    let auth = global().read().unwrap_or_else(|e| e.into_inner());
    auth.is_authorized_pipe(pipe_id, app_id, pid)
}

/// Convenience scan for candidate PE executable (BST-54).
pub fn scan_executable_protection(exe_path: &Path) -> ProtectionScanReport {
    let auth = global().read().unwrap_or_else(|e| e.into_inner());
    auth.scan_executable(exe_path)
}

/// Convenience scan for game folder (BST-54).
pub fn scan_game_folder_protection(app_id: u32, game_dir: &Path) -> ProtectionScanReport {
    let auth = global().read().unwrap_or_else(|e| e.into_inner());
    auth.scan_game_folder(app_id, game_dir)
}

/// Convenience scan for running process by PID (BST-54).
pub fn scan_process_protection(pid: u32) -> ProtectionScanReport {
    scan_process(pid)
}

/// Denuvo heuristic detection method (BST-54).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenuvoDetectionMethod {
    None,
    LegacySectionString,
    OepPattern,
    ProtectedBlobSection,
}

/// Protection scan report matching usefulsteamtools `ProtectionScanReport` (BST-54).
#[derive(Debug, Clone)]
pub struct ProtectionScanReport {
    pub denuvo_detected: bool,
    pub method: DenuvoDetectionMethod,
    pub module_path: Option<String>,
    pub section_name: Option<String>,
    pub pid: u32,
    pub module_size: u64,
    pub file_size: u64,
    pub entry_point_rva: u32,
    pub match_rva: u32,
    pub match_raw_offset: usize,
    pub scanned_modules: usize,
    pub elapsed_ms: f64,
}

impl Default for ProtectionScanReport {
    fn default() -> Self {
        Self {
            denuvo_detected: false,
            method: DenuvoDetectionMethod::None,
            module_path: None,
            section_name: None,
            pid: 0,
            module_size: 0,
            file_size: 0,
            entry_point_rva: 0,
            match_rva: 0,
            match_raw_offset: 0,
            scanned_modules: 0,
            elapsed_ms: 0.0,
        }
    }
}

/// Core PE binary scanner for Denuvo protection signatures (BST-54).
///
/// 3-Tier Denuvo Detection Cascade:
/// 1. OEP Pattern scan: `48 B9 44 4F 44 45 4E 55 56 4F` (`mov rcx, 'DODENUVO'`) in entry-point section.
/// 2. Legacy Section String scan: `.arch`, `.srdata`, `.xpdata`, `.xdata`, `.xtls` containing `"DENUVO"`.
/// 3. Protected W+X blob section: simultaneous write + execute flags, raw size >= 4 MiB, and Shannon entropy >= 6.0.
pub fn scan_protection_pe(exe_path: &Path) -> ProtectionScanReport {
    let path_str = match exe_path.to_str() {
        Some(s) => s,
        None => return ProtectionScanReport::default(),
    };
    let image = match crate::steam_bridge::ost_platform::pe::Image::new(path_str) {
        Ok(img) if img.is_valid() => img,
        _ => return ProtectionScanReport::default(),
    };

    let file_bytes = image.bytes();
    let file_size = image.file_size();

    // 1. Tier 1 — OEP Pattern scan: 48 B9 44 4F 44 45 4E 55 56 4F (mov rcx, 'DODENUVO')
    const DENUVO_OEP_PATTERN: [u8; 10] = [
        0x48, 0xB9, 0x44, 0x4F, 0x44, 0x45, 0x4E, 0x55, 0x56, 0x4F,
    ];
    let ep_rva = image.entry_point_rva();
    if ep_rva != 0 {
        if let Some(sec) = image.section_containing_rva(ep_rva) {
            let offset = sec.raw_offset as usize;
            let size = sec.raw_size as usize;
            if let Some(slice) = file_bytes.get(offset..offset.saturating_add(size)) {
                if let Some(pos) = slice.windows(DENUVO_OEP_PATTERN.len()).position(|w| w == DENUVO_OEP_PATTERN) {
                    return ProtectionScanReport {
                        denuvo_detected: true,
                        method: DenuvoDetectionMethod::OepPattern,
                        section_name: Some(sec.name.clone()),
                        entry_point_rva: ep_rva,
                        match_rva: sec.virtual_address.saturating_add(pos as u32),
                        match_raw_offset: offset.saturating_add(pos),
                        file_size,
                        module_size: file_size,
                        ..Default::default()
                    };
                }
            }
        }
    }

    // Broad fallback OEP pattern check across file if entry point section mapping is truncated
    if file_bytes
        .windows(DENUVO_OEP_PATTERN.len())
        .any(|w| w == DENUVO_OEP_PATTERN)
    {
        let sec_name = image.section_containing_rva(ep_rva).map(|s| s.name.clone());
        return ProtectionScanReport {
            denuvo_detected: true,
            method: DenuvoDetectionMethod::OepPattern,
            section_name: sec_name,
            entry_point_rva: ep_rva,
            file_size,
            module_size: file_size,
            ..Default::default()
        };
    }

    // 2. Tier 2 — Legacy Section String scan (".arch", ".srdata", ".xpdata", ".xdata", ".xtls" containing "DENUVO")
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
                        entry_point_rva: ep_rva,
                        file_size,
                        module_size: file_size,
                        ..Default::default()
                    };
                }
            }
        }
    }

    // 3. Tier 3 — Protected W+X blob section (executable + writable, >= 4 MiB, and Shannon entropy >= 6.0)
    for sec in image.sections() {
        if sec.is_executable_and_writable() && sec.raw_size >= PROTECTOR_BLOB_MIN_BYTES {
            let offset = sec.raw_offset as usize;
            let sample_len = (sec.raw_size as usize).min(PROTECTOR_BLOB_ENTROPY_SAMPLE_BYTES);
            if let Some(sample) = file_bytes.get(offset..offset.saturating_add(sample_len)) {
                let entropy = calculate_shannon_entropy(sample);
                if entropy >= PROTECTOR_BLOB_MIN_ENTROPY {
                    let confidence = if entropy >= PROTECTOR_BLOB_HIGH_CONFIDENCE_ENTROPY {
                        "high(encrypted)"
                    } else {
                        "elevated"
                    };
                    log::info!(
                        "[denuvo_scan] Protector blob detected in {} section '{}' raw_size={} ({:.2} MB) entropy={:.3} confidence={}",
                        exe_path.display(),
                        sec.name,
                        sec.raw_size,
                        sec.raw_size as f64 / (1024.0 * 1024.0),
                        entropy,
                        confidence
                    );
                    return ProtectionScanReport {
                        denuvo_detected: true,
                        method: DenuvoDetectionMethod::ProtectedBlobSection,
                        section_name: Some(sec.name.clone()),
                        entry_point_rva: ep_rva,
                        match_rva: sec.virtual_address,
                        match_raw_offset: offset,
                        file_size,
                        module_size: file_size,
                        ..Default::default()
                    };
                } else {
                    log::debug!(
                        "[denuvo_scan] RWX section '{}' below entropy floor ({:.3} < {:.1}) in {}",
                        sec.name,
                        entropy,
                        PROTECTOR_BLOB_MIN_ENTROPY,
                        exe_path.display()
                    );
                }
            }
        }
    }

    ProtectionScanReport {
        denuvo_detected: false,
        method: DenuvoDetectionMethod::None,
        section_name: None,
        file_size,
        module_size: file_size,
        entry_point_rva: ep_rva,
        ..Default::default()
    }
}

/// Enumerate modules of a live process and run candidate priority filtering (BST-54).
pub fn scan_process(pid: u32) -> ProtectionScanReport {
    if pid == 0 {
        return ProtectionScanReport::default();
    }
    let timer = std::time::Instant::now();
    let modules = crate::steam_bridge::ost_platform::process::enumerate_modules(pid);
    if modules.is_empty() {
        return ProtectionScanReport::default();
    }

    struct Candidate {
        path: PathBuf,
        size: u64,
        is_executable: bool,
        in_game_tree: bool,
        order: usize,
    }

    let steam_runtime_modules = [
        "steamclient.dll",
        "steamclient64.dll",
        "steam_api.dll",
        "steam_api64.dll",
        "tier0_s.dll",
        "tier0_s64.dll",
        "vstdlib_s.dll",
        "vstdlib_s64.dll",
        "gameoverlayrenderer.dll",
        "gameoverlayrenderer64.dll",
    ];

    let mut candidates = Vec::new();
    let mut order = 0;

    for m in &modules {
        let path = &m.native_path;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let is_exe = ext.eq_ignore_ascii_case("exe");
        let is_dll = ext.eq_ignore_ascii_case("dll");
        if !is_exe && !is_dll {
            continue;
        }

        let file_size = fs::metadata(path).map(|meta| meta.len()).unwrap_or(m.size as u64);
        if file_size < MIN_PACKED_MODULE_BYTES {
            continue;
        }

        let path_str = path.to_string_lossy();
        if !is_exe && (path_str.starts_with("/usr") || path_str.starts_with("/lib") || path_str.contains("windows/system32")) {
            continue;
        }

        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
        if !is_exe && steam_runtime_modules.contains(&fname.as_str()) {
            continue;
        }

        candidates.push(Candidate {
            path: path.clone(),
            size: file_size,
            is_executable: is_exe,
            in_game_tree: false,
            order,
        });
        order += 1;
    }

    let game_dir = candidates
        .iter()
        .find(|c| c.is_executable)
        .and_then(|c| c.path.parent().map(|p| p.to_path_buf()));
    if let Some(ref gdir) = game_dir {
        for c in &mut candidates {
            if !c.is_executable && c.path.starts_with(gdir) {
                c.in_game_tree = true;
            }
        }
    }

    // Sort: (1) main exe, (2) in-game-tree DLLs, (3) descending module size, (4) original order
    candidates.sort_by(|a, b| {
        if a.is_executable != b.is_executable {
            return b.is_executable.cmp(&a.is_executable);
        }
        if !a.is_executable && a.in_game_tree != b.in_game_tree {
            return b.in_game_tree.cmp(&a.in_game_tree);
        }
        if !a.is_executable && a.size != b.size {
            return b.size.cmp(&a.size);
        }
        a.order.cmp(&b.order)
    });

    let mut scanned_count = 0;
    for cand in &candidates {
        scanned_count += 1;
        let mut report = scan_protection_pe(&cand.path);
        if report.denuvo_detected {
            report.pid = pid;
            report.module_path = Some(cand.path.to_string_lossy().into_owned());
            report.module_size = cand.size;
            report.scanned_modules = scanned_count;
            report.elapsed_ms = timer.elapsed().as_secs_f64() * 1000.0;
            return report;
        }
    }

    ProtectionScanReport {
        denuvo_detected: false,
        method: DenuvoDetectionMethod::None,
        pid,
        scanned_modules: scanned_count,
        elapsed_ms: timer.elapsed().as_secs_f64() * 1000.0,
        ..Default::default()
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
        assert!(!auth.is_authorized_pipe(0, 1245620, 9999));

        auth.apply(1245620, Some(vec![0xAA, 0xBB, 0xCC]));
        assert!(auth.is_authorized_pipe(0, 1245620, 4242));
        assert!(!auth.is_authorized_pipe(0, 1245620, 9999));
        assert!(auth.is_authorized_pipe(0, 1245620, 0));

        // BST-55: Ticket bytes are genuine and uncorrupted (NOT concatenated with nonce!)
        let hit = auth.fetch_auth_ticket(1245620, &[0x01, 0x02]).unwrap();
        assert!(hit.success);
        assert_eq!(hit.ticket, Some(vec![0xAA, 0xBB, 0xCC]));
        assert!(hit.expires_in > 0);

        // Expire immediately with max_age_secs = 0
        auth.cleanup_expired(0);
        let expired = auth.fetch_auth_ticket(1245620, &[]).unwrap();
        assert!(!expired.success);

        auth.revoke(1245620);
        assert!(!auth.is_authorized_pipe(0, 1245620, 4242));
    }

    #[test]
    fn denuvo_two_handshake_state_machine_window() {
        let mut auth = DenuvoAuthService::new();
        auth.configure(DenuvoAuthConfig {
            enabled: true,
            auth_url: None,
            pool_steam_id: Some(76_561_198_028_121_353),
            ..Default::default()
        });
        auth.register_denuvo_app(1245620);

        let pipe_id = 1001;
        let pid = 5555;

        // Handshake 1: Opens authorization window
        assert!(auth.on_handshake_pipe(pipe_id, 1245620, pid));
        assert!(auth.is_authorized_pipe(pipe_id, 1245620, pid));
        assert!(!auth.is_authorized_pipe(pipe_id + 1, 1245620, pid));

        // Handshake 2: Ends authorization window
        assert!(auth.on_handshake_pipe(pipe_id, 1245620, pid));
        assert!(!auth.is_authorized_pipe(pipe_id, 1245620, pid));
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

        // BST-55: Ticket bytes are untouched and uncorrupted
        let first = auth.fetch_auth_ticket(1245620, &[0x01, 0x02]).unwrap();
        assert!(first.success);
        assert_eq!(first.ticket, Some(vec![0xDE, 0xAD, 0xBE, 0xEF]));
        assert_eq!(first.expires_in, 3600);

        // Second call must hit local cache
        let second = auth.fetch_auth_ticket(1245620, &[0x03]).unwrap();
        assert!(second.success);
        assert_eq!(second.ticket, Some(vec![0xDE, 0xAD, 0xBE, 0xEF]));

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

        // 3. Size gate on scan_executable: < 80 MiB returns denuvo_detected = false
        let auth = DenuvoAuthService::new();
        let rep_gate = auth.scan_executable(&oep_exe);
        assert!(!rep_gate.denuvo_detected);
    }

    #[test]
    fn test_shannon_entropy_calculation() {
        // Uniform byte slice has 0 entropy
        let zeros = vec![0u8; 1024];
        assert_eq!(calculate_shannon_entropy(&zeros), 0.0);

        // Maximum entropy: equal distribution of all 256 bytes (8.0 bits/byte)
        let mut uniform = Vec::with_capacity(256 * 10);
        for _ in 0..10 {
            for b in 0..=255u8 {
                uniform.push(b);
            }
        }
        let ent = calculate_shannon_entropy(&uniform);
        assert!((ent - 8.0).abs() < 1e-6);
    }
}
