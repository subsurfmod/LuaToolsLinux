//! NetPacket Manifest hook layer for Linux.
//!
//! Port of usefulsteamtools `Interop_NetPacket.cpp` namespace
//! `Hooks_NetPacket_Manifest` (lines 1227-1458).
//!
//! Handles async manifest request code fetching:
//! - **Outgoing** (eMsg 151): ContentServerDirectory.GetManifestRequestCode#1
//!   - Track jobid_source -> (depot_id, manifest_gid)
//!   - For Lua depots NOT genuinely owned: launch async HTTP fetch for manifest_request_code
//!   - Pre-seed depotcache manifest from archive (EnsureCached)
//!   - Passive capture: track all requests for donor code harvesting
//! - **Incoming** (eMsg 147): Response
//!   - Passive capture: harvest genuine codes from owned depots
//!   - For tracked Lua depot requests: wait up to 12s for async fetch
//!   - If code received: patch header (eresult=OK) + body (manifest_request_code)

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::steam_bridge::interop::interop_misc::resolve_app_id;
use crate::steam_bridge::interop::active_download_count;
use crate::steam_bridge::manifest_hooks::{ensure_manifest_cached, get_seen_depot, report_missing_manifest};

const JOB_ID_BASE: u64 = 0x7E52_0000_0000_0000;
const PENDING_TIMEOUT: Duration = Duration::from_secs(12); // 12s wait for async fetch
const MSG_GET_MANIFEST_REQUEST_CODE: u32 = 151; // eMsg 151
const MSG_GET_MANIFEST_REQUEST_CODE_RESPONSE: u32 = 147; // eMsg 147

static MANIFEST_STATE: OnceLock<Mutex<ManifestState>> = OnceLock::new();
static MANIFEST_ACTIVE: AtomicBool = AtomicBool::new(false);
static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(JOB_ID_BASE);

fn manifest_state() -> &'static Mutex<ManifestState> {
    MANIFEST_STATE.get_or_init(|| Mutex::new(ManifestState::new()))
}

struct ManifestState {
    /// jobid -> (depot_id, manifest_gid, app_id, is_lua_depot, is_owned)
    sent_requests: HashMap<u64, SentRequest>,
    /// Pending async fetches: job_id -> (depot_id, manifest_gid, future)
    pending_fetches: HashMap<u64, (u32, u64, std::sync::mpsc::Receiver<u64>)>,
    /// Passive capture: harvested genuine codes from owned depots
    harvested_codes: HashMap<(u32, u64), u64>,
    /// Steam root for depotcache operations
    steam_root: Option<std::path::PathBuf>,
    /// Whether we've initialized the HTTP client
    http_client_ready: bool,
}

struct SentRequest {
    depot_id: u32,
    manifest_gid: u64,
    app_id: u32,
    is_lua_depot: bool,
    is_owned: bool,
    sent_at: Instant,
}

impl ManifestState {
    fn new() -> Self {
        Self {
            sent_requests: HashMap::new(),
            pending_fetches: HashMap::new(),
            harvested_codes: HashMap::new(),
            steam_root: None,
            http_client_ready: false,
        }
    }

    fn set_steam_root(&mut self, root: std::path::PathBuf) {
        self.steam_root = Some(root);
    }

    fn next_job_id(&self) -> u64 {
        let id = NEXT_JOB_ID.fetch_add(1, Ordering::SeqCst);
        if id == 0 {
            NEXT_JOB_ID.store(JOB_ID_BASE, Ordering::SeqCst);
            JOB_ID_BASE
        } else {
            id
        }
    }

    fn is_lua_depot(&self, depot_id: u32) -> bool {
        crate::steam_bridge::interop::interop_package::has_depot(depot_id)
    }

    fn is_owned_depot(&self, depot_id: u32) -> bool {
        let (ready, owned) = crate::steam_bridge::interop::interop_package::get_owned_depots();
        ready && owned.contains(&depot_id)
    }

    fn lookup_depot(&self, depot_id: u32) -> Option<(u32, u64)> {
        get_seen_depot(0, depot_id)
    }

    fn pre_seed_manifest(&self, app_id: u32, depot_id: u32, manifest_gid: u64) {
        if let Some(ref steam_root) = self.steam_root {
            if !ensure_manifest_cached(steam_root, app_id, depot_id, manifest_gid) {
                report_missing_manifest(depot_id, manifest_gid);
            }
        }
    }
}

/// CMsgProtoBufHeader for request/response messages
struct CMsgProtoBufHeader {
    buf: Vec<u8>,
    jobid_source: u64,
    jobid_target: u64,
    emsg: u32,
}

impl CMsgProtoBufHeader {
    fn parse_from_array(data: &[u8]) -> Result<Self, String> {
        if data.len() < 24 {
            return Err("header too short".to_string());
        }
        let emsg = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let jobid_source = u64::from_le_bytes([
            data[4], data[5], data[6], data[7], data[8], data[9], data[10], data[11],
        ]);
        let jobid_target = u64::from_le_bytes([
            data[12], data[13], data[14], data[15], data[16], data[17], data[18], data[19],
        ]);
        Ok(CMsgProtoBufHeader {
            buf: data.to_vec(),
            jobid_source,
            jobid_target,
            emsg,
        })
    }
}

/// GetManifestRequestCodeRequest protobuf
struct GetManifestRequestCodeRequest {
    depot_id: u32,
    manifest_gid: u64,
    app_id: u32,
}

impl GetManifestRequestCodeRequest {
    fn parse_from_array(data: &[u8]) -> Result<Self, String> {
        if data.len() < 16 {
            return Err("request too short".to_string());
        }
        let depot_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let manifest_gid = u64::from_le_bytes([
            data[4], data[5], data[6], data[7], data[8], data[9], data[10], data[11],
        ]);
        let app_id = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
        Ok(Self { depot_id, manifest_gid, app_id })
    }
}

/// GetManifestRequestCodeResponse protobuf
struct GetManifestRequestCodeResponse {
    manifest_request_code: u64,
    eresult: i32,
}

impl GetManifestRequestCodeResponse {
    fn parse_from_array(data: &[u8]) -> Result<Self, String> {
        if data.len() < 12 {
            return Err("response too short".to_string());
        }
        let manifest_request_code = u64::from_le_bytes([
            data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
        ]);
        let eresult = i32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        Ok(Self { manifest_request_code, eresult })
    }

    fn is_ok(&self) -> bool {
        self.eresult == 1 || self.eresult == 0
    }

    fn serialize(&self) -> [u8; 12] {
        let mut out = [0u8; 12];
        out[0..8].copy_from_slice(&self.manifest_request_code.to_le_bytes());
        out[8..12].copy_from_slice(&self.eresult.to_le_bytes());
        out
    }
}

/// ManifestClient - handles async HTTP fetch for manifest request codes using background thread
struct ManifestClient;

impl ManifestClient {
    /// Fetch manifest request code via HTTP (spawns background thread with channel)
    fn fetch_manifest_request_code(
        depot_id: u32,
        manifest_gid: u64,
        _hubcap_api_key: Option<String>,
    ) -> mpsc::Receiver<u64> {
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            let code = Self::fetch_manifest_request_code_sync(depot_id, manifest_gid, _hubcap_api_key);
            let _ = tx.send(code.unwrap_or(0));
        });

        rx
    }

    /// Synchronous manifest request code fetch using blocking HTTP with multi-endpoint fallback
    pub fn fetch_manifest_request_code_sync(
        depot_id: u32,
        manifest_gid: u64,
        hubcap_api_key: Option<String>,
    ) -> Option<u64> {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(5))
            .build();

        // 1. If Hubcap API key is available, query Hubcap endpoint first
        if let Some(ref api_key) = hubcap_api_key {
            let key = api_key.trim();
            if !key.is_empty() {
                let hubcap_urls = [
                    format!("https://api.hubcap.app/v1/depots/{}/manifests/{}/request_code", depot_id, manifest_gid),
                    format!("https://manifest.opensteamtool.com/{}/{}", depot_id, manifest_gid),
                ];
                for url in &hubcap_urls {
                    let req = agent.get(url).set("Authorization", &format!("Bearer {}", key));
                    if let Ok(resp) = req.call() {
                        if let Ok(text) = resp.into_string() {
                            if let Some(code) = Self::parse_manifest_request_code(&text) {
                                log::info!("[netpacket_manifest] Resolved request code {} from Hubcap/OST: {}", code, url);
                                return Some(code);
                            }
                        }
                    }
                }
            }
        }

        // 2. Standard fallback endpoints: manifest.opensteamtool.com, gmrc.wudrm.com, manifest.steam.run
        let endpoints = [
            format!("https://manifest.opensteamtool.com/{}/{}", depot_id, manifest_gid),
            format!("https://manifest.opensteamtool.com/{}", manifest_gid),
            format!("http://gmrc.wudrm.com/manifest/{}", manifest_gid),
            format!("https://manifest.steam.run/api/manifest/{}", manifest_gid),
        ];

        for url in &endpoints {
            if let Ok(resp) = agent.get(url).call() {
                if let Ok(text) = resp.into_string() {
                    if let Some(code) = Self::parse_manifest_request_code(&text) {
                        log::info!("[netpacket_manifest] Resolved request code {} from {}", code, url);
                        return Some(code);
                    }
                }
            }
        }

        log::warn!(
            "[netpacket_manifest] Failed to resolve manifest request code for depot={} gid={}",
            depot_id, manifest_gid
        );
        None
    }

    /// Parse manifest request code from string (decimal, hex with 0x prefix, or JSON object)
    pub fn parse_manifest_request_code(raw: &str) -> Option<u64> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }

        // Try JSON format e.g. {"content": "123456"} or {"code": 123456} or {"request_code": ...}
        if trimmed.starts_with('{') {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(n) = val.get("content").or_else(|| val.get("code")).or_else(|| val.get("request_code")) {
                    if let Some(num) = n.as_u64() {
                        if num > 0 {
                            return Some(num);
                        }
                    }
                    if let Some(s) = n.as_str() {
                        if let Some(parsed) = Self::parse_manifest_request_code(s) {
                            return Some(parsed);
                        }
                    }
                }
            }
        }

        // Try hex format "0x..." or decimal
        if let Some(hex_part) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
            u64::from_str_radix(hex_part.trim(), 16).ok().filter(|&c| c > 0)
        } else if let Ok(dec) = trimmed.parse::<u64>() {
            if dec > 0 {
                Some(dec)
            } else {
                None
            }
        } else if trimmed.len() == 16 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
            // Raw 16-hex characters
            u64::from_str_radix(trimmed, 16).ok().filter(|&c| c > 0)
        } else {
            None
        }
    }
}

/// Handle outgoing NetPacket message (eMsg 151 - GetManifestRequestCode)
pub fn handle_send_get_manifest_request_code(
    p_hdr: &mut [u8],
    p_body: &mut [u8],
    steam_root: Option<std::path::PathBuf>,
) -> bool {
    let mut state = manifest_state().lock().unwrap_or_else(|e| e.into_inner());

    if let Some(root) = steam_root {
        state.set_steam_root(root);
    }
    state.http_client_ready = true;

    let Ok(hdr) = CMsgProtoBufHeader::parse_from_array(p_hdr) else {
        return false;
    };

    // Only handle eMsg 151 (GetManifestRequestCode)
    if hdr.emsg != MSG_GET_MANIFEST_REQUEST_CODE {
        return false;
    }

    let Ok(req) = GetManifestRequestCodeRequest::parse_from_array(p_body) else {
        return false;
    };

    let depot_id = req.depot_id;
    let manifest_gid = req.manifest_gid;
    let app_id = if req.app_id != 0 { req.app_id } else { resolve_app_id() };

    // Passive capture: track ALL requests for donor code harvesting
    let job_id = hdr.jobid_source;
    let is_lua_depot = state.is_lua_depot(depot_id);
    let is_owned = state.is_owned_depot(depot_id);

    state.sent_requests.insert(
        job_id,
        SentRequest {
            depot_id,
            manifest_gid,
            app_id,
            is_lua_depot,
            is_owned,
            sent_at: Instant::now(),
        },
    );

    // Pre-seed depotcache manifest from archive
    state.pre_seed_manifest(app_id, depot_id, manifest_gid);

    // For Lua depots NOT genuinely owned: launch async HTTP fetch
    if is_lua_depot && !is_owned {
        let hubcap_key = crate::models::config::SlssteamConfig::default()
            .extra_fields
            .get("hubcap_api_key")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);

        let receiver = ManifestClient::fetch_manifest_request_code(depot_id, manifest_gid, hubcap_key);
        state.pending_fetches.insert(job_id, (depot_id, manifest_gid, receiver));

        log::debug!(
            "[netpacket_manifest] Outgoing eMsg 151: jobid=0x{:X} depot={} gid={} app={} lua={} owned={} -> async fetch launched",
            job_id, depot_id, manifest_gid, app_id, is_lua_depot, is_owned
        );
    } else {
        log::debug!(
            "[netpacket_manifest] Outgoing eMsg 151: jobid=0x{:X} depot={} gid={} app={} lua={} owned={} (passive only)",
            job_id, depot_id, manifest_gid, app_id, is_lua_depot, is_owned
        );
    }

    false // Don't block the original message
}

/// Handle incoming NetPacket response (eMsg 147 - GetManifestRequestCodeResponse)
pub fn handle_recv_get_manifest_request_code_response(
    p_hdr: &mut [u8],
    p_body: &mut [u8],
) -> bool {
    let mut state = manifest_state().lock().unwrap_or_else(|e| e.into_inner());

    let Ok(hdr) = CMsgProtoBufHeader::parse_from_array(p_hdr) else {
        return false;
    };

    // Only handle eMsg 147 (GetManifestRequestCodeResponse)
    if hdr.emsg != MSG_GET_MANIFEST_REQUEST_CODE_RESPONSE {
        return false;
    }

    let job_id = hdr.jobid_target;
    if job_id == 0 {
        return false;
    }

    let Ok(resp) = GetManifestRequestCodeResponse::parse_from_array(p_body) else {
        return false;
    };

    // Check if this is a tracked request
    let sent_request = state.sent_requests.remove(&job_id);

    // Passive capture: harvest genuine codes from owned depots
    if let Some(ref req) = sent_request {
        if req.is_owned && resp.is_ok() && resp.manifest_request_code != 0 {
            state.harvested_codes.insert((req.depot_id, req.manifest_gid), resp.manifest_request_code);
            log::debug!(
                "[netpacket_manifest] Harvested genuine code: depot={} gid={} code=0x{:X}",
                req.depot_id, req.manifest_gid, resp.manifest_request_code
            );
        }
    }

    // For tracked Lua depot requests: check if async fetch completed
    if let Some((depot_id, manifest_gid, receiver)) = state.pending_fetches.remove(&job_id) {
        // Try to receive the result (non-blocking check)
        match receiver.try_recv() {
            Ok(fetched_code) if fetched_code != 0 => {
                // Patch header: eresult=OK
                if p_hdr.len() >= 4 {
                    p_hdr[0..4].copy_from_slice(&1i32.to_le_bytes()); // EResult::OK = 1
                }
                // Patch body: manifest_request_code
                let patched_resp = GetManifestRequestCodeResponse {
                    manifest_request_code: fetched_code,
                    eresult: 1,
                };
                if p_body.len() >= 12 {
                    p_body[..12].copy_from_slice(&patched_resp.serialize());
                }
                log::info!(
                    "[netpacket_manifest] Patched response for jobid=0x{:X}: code=0x{:X}",
                    job_id, fetched_code
                );
                return true; // Response was patched
            }
            Ok(_) => {
                log::warn!(
                    "[netpacket_manifest] Async fetch returned 0 for jobid=0x{:X}",
                    job_id
                );
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // Fetch not complete yet - re-queue for next response
                state.pending_fetches.insert(job_id, (depot_id, manifest_gid, receiver));
                log::debug!("[netpacket_manifest] Async fetch still pending for jobid=0x{:X}", job_id);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                log::warn!(
                    "[netpacket_manifest] Async fetch channel disconnected for jobid=0x{:X}",
                    job_id
                );
            }
        }
    }

    false // Don't block, let original response through
}

/// Get a harvested code for a depot/gid pair (for donor code reuse)
pub fn get_harvested_code(depot_id: u32, manifest_gid: u64) -> Option<u64> {
    let state = manifest_state().lock().unwrap_or_else(|e| e.into_inner());
    state.harvested_codes.get(&(depot_id, manifest_gid)).copied()
}

/// Install Manifest NetPacket hooks
pub fn install_manifest_hooks(steam_root: std::path::PathBuf) -> Result<(), String> {
    let mut state = manifest_state().lock().unwrap_or_else(|e| e.into_inner());
    state.set_steam_root(steam_root);
    state.http_client_ready = true;

    MANIFEST_ACTIVE.store(true, Ordering::Release);
    log::info!("Installed Manifest NetPacket hooks");
    Ok(())
}

/// Uninstall Manifest NetPacket hooks
pub fn uninstall_manifest_hooks() {
    MANIFEST_ACTIVE.store(false, Ordering::Release);
    manifest_state()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .sent_requests.clear();
    manifest_state()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pending_fetches.clear();
    log::info!("Uninstalled Manifest NetPacket hooks");
}

pub fn install_netpacket_manifest_hooks(steam_root: std::path::PathBuf) -> Result<(), String> {
    install_manifest_hooks(steam_root)
}

pub fn uninstall_netpacket_manifest_hooks() {
    uninstall_manifest_hooks()
}

pub fn is_netpacket_manifest_hook_active() -> bool {
    MANIFEST_ACTIVE.load(Ordering::Acquire)
}

/// Expire stale pending requests (call periodically)
pub fn expire_manifest_stale() {
    let mut state = manifest_state().lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    state.sent_requests.retain(|_, req| now.duration_since(req.sent_at) < PENDING_TIMEOUT);
    // Note: pending_fetches are kept until response or disconnect
}

/// Check if we have active downloads
fn has_active_downloads() -> bool {
    active_download_count() > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn test_manifest_header_parsing() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        let mut hdr = vec![0u8; 24];
        hdr[0..4].copy_from_slice(&MSG_GET_MANIFEST_REQUEST_CODE.to_le_bytes());
        hdr[4..12].copy_from_slice(&0x123456789ABCDEFu64.to_le_bytes());
        hdr[12..20].copy_from_slice(&0xFEDCBA9876543210u64.to_le_bytes());

        let parsed = CMsgProtoBufHeader::parse_from_array(&hdr).unwrap();
        assert_eq!(parsed.emsg, MSG_GET_MANIFEST_REQUEST_CODE);
        assert_eq!(parsed.jobid_source, 0x123456789ABCDEF);
        assert_eq!(parsed.jobid_target, 0xFEDCBA9876543210);
    }

    #[test]
    fn test_request_parsing() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        let mut body = [0u8; 16];
        body[0..4].copy_from_slice(&12345u32.to_le_bytes());
        body[4..12].copy_from_slice(&0x1122334455667788u64.to_le_bytes());
        body[12..16].copy_from_slice(&730u32.to_le_bytes());

        let req = GetManifestRequestCodeRequest::parse_from_array(&body).unwrap();
        assert_eq!(req.depot_id, 12345);
        assert_eq!(req.manifest_gid, 0x1122334455667788);
        assert_eq!(req.app_id, 730);
    }

    #[test]
    fn test_response_parsing() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        let mut body = [0u8; 12];
        body[0..8].copy_from_slice(&0xDEADBEEFCAFE0001u64.to_le_bytes());
        body[8..12].copy_from_slice(&1i32.to_le_bytes());

        let resp = GetManifestRequestCodeResponse::parse_from_array(&body).unwrap();
        assert_eq!(resp.manifest_request_code, 0xDEADBEEFCAFE0001);
        assert_eq!(resp.eresult, 1);
        assert!(resp.is_ok());
    }

    #[test]
    fn test_manifest_request_code_parsing() {
        assert_eq!(
            ManifestClient::parse_manifest_request_code("123456789012345678"),
            Some(123456789012345678)
        );
        assert_eq!(
            ManifestClient::parse_manifest_request_code("0xDEADBEEFCAFE0001"),
            Some(0xDEADBEEFCAFE0001)
        );
        assert_eq!(
            ManifestClient::parse_manifest_request_code(r#"{"content": "9876543210123456"}"#),
            Some(9876543210123456)
        );
        assert_eq!(
            ManifestClient::parse_manifest_request_code(r#"{"code": 1122334455}"#),
            Some(1122334455)
        );
        assert_eq!(ManifestClient::parse_manifest_request_code(""), None);
        assert_eq!(ManifestClient::parse_manifest_request_code("invalid_text"), None);
    }
}