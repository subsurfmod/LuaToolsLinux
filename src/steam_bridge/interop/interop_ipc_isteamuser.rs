//! Interop IPC - IClientUser Interface Handlers
//!
//! Linux port of usefulsteamtools Interop_IPC_ISteamUser.cpp
//! Uses memory_patch.rs for inline hooking (14-byte absolute indirect jumps)
//! and pattern_db.rs for symbol resolution.
//! Integrates with ipc.rs for IPC message handling and ticket_store.rs for credentials.
//! Follows bridge_lib.rs pattern: catch_unwind panic safety, OnceLock/RwLock for globals.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::ffi::c_void;

use crate::steam_bridge::memory_patch::{MemoryPatch, JMP_ABS64_LEN};
use crate::steam_bridge::pattern_db::{PatternDb, PatternChannel, BinaryComponent, PatternEntry};
use crate::steam_bridge::binary_reader::BinaryFile;
use crate::steam_bridge::ipc::{IpcMessage, IpcServer, IpcClient, IpcHandlerFn};
use crate::steam_bridge::ticket_store::TicketStore;
use crate::steam_bridge::ost_platform::dynamic_library::{get_loaded, get_symbol};
use crate::steam_bridge::ost_platform::byte_search::find_in_file;
use crate::steam_bridge::interop::ipc::{
    EIPCInterface, ResolvedHandler, IPCProcessMessageFn, call_original,
    make_handler, fnv1a_hash,
};
use crate::steam_bridge::interop::interop_misc::{
    resolve_app_id, is_online_fix_active, notify_networking_sockets_used,
    should_report_online_fix_app_id, set_suppress_app_id_flip,
    get_app_id_for_current_pipe, get_game_name_by_app_id, ensure_buffer_capacity,
    register_ipc_handlers,
};

/// Fresh nonce-bound etickets minted on-demand in RequestEncryptedAppTicket
/// (keyed by appId) and consumed by GetEncryptedAppTicket on the same launch.
/// Lets the strict-Denuvo path serve a ticket matching the launch nonce while
/// keeping GetEncryptedAppTicket's credential-store serve as the fallback.
static FRESH_ETICKET: OnceLock<Mutex<HashMap<u32, Vec<u8>>>> = OnceLock::new();

/// Online fix app ID (Spacewar)
pub const ONLINE_FIX_APP_ID: u32 = 480;

/// Install IClientUser IPC handlers
/// This is called during InteropIPC initialization
/// Also installs memory patches to redirect IClientUser calls to our handlers
pub fn install_iclientuser_handlers() -> Result<(), String> {
    // First, install memory patches that redirect the IClientUser calls
    // to our handler logic via trampolines
    let patches = install_iclientuser_patches()?;

    // Store patches for cleanup
    IPC_PATCHES.get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap()
        .extend(patches);

    // Register default IClientUser handlers
    register_handler(
        "IClientUser",
        "GetSteamID",
        None,
        Some(Box::new(handler_post_get_steam_id)),
    )?;

    register_handler(
        "IClientUser",
        "GetAppOwnershipTicketExtendedData",
        None,
        Some(Box::new(handler_post_get_app_ownership_ticket)),
    )?;

    register_handler(
        "IClientUser",
        "RequestEncryptedAppTicket",
        None,
        Some(Box::new(handler_post_request_encrypted_app_ticket)),
    )?;

    register_handler(
        "IClientUser",
        "GetEncryptedAppTicket",
        None,
        Some(Box::new(handler_post_get_encrypted_app_ticket)),
    )?;

    log::info!("[InteropIPC_ISteamUser] Installed 4 IClientUser handlers and {} memory patches",
        IPC_PATCHES.get_or_init(|| Mutex::new(Vec::new())).lock().unwrap().len());
    Ok(())
}

/// Install memory patches for IClientUser interface redirection
/// Uses pattern_db.rs to resolve function addresses by byte pattern from steam-monitor TOML
fn install_iclientuser_patches() -> Result<Vec<MemoryPatch>, String> {
    let mut patches = Vec::new();

    // Get steamclient.so base address
    let steam_client = get_loaded("steamclient.so");
    let steam_client = if steam_client.is_null() {
        get_loaded("steamclient64.so")
    } else {
        steam_client
    };

    if steam_client.is_null() {
        return Err("steamclient.so not loaded in process".into());
    }

    let steam_client_base = steam_client as u64;

    // Get pattern database
    let pattern_db = pattern_db();

    // Open steamclient.so to get its SHA256 for pattern lookup
    let binary = BinaryFile::open("steamclient.so")
        .map_err(|e| format!("Failed to open steamclient.so: {}", e))?;

    let pattern_file = pattern_db.lookup_by_binary(
        &binary,
        PatternChannel::Ipc,
        BinaryComponent::SteamClient,
    ).map_err(|e| format!("Failed to lookup pattern: {}", e))?;

    // Install patches for each IClientUser method
    let methods = [
        ("GetSteamID", handler_post_get_steam_id as *mut c_void as usize),
        ("GetAppOwnershipTicketExtendedData", handler_post_get_app_ownership_ticket as *mut c_void as usize),
        ("RequestEncryptedAppTicket", handler_post_request_encrypted_app_ticket as *mut c_void as usize),
        ("GetEncryptedAppTicket", handler_post_get_encrypted_app_ticket as *mut c_void as usize),
    ];

    for (method_name, replacement) in &methods {
        let method_key = format!("IClientUser::{}", method_name);
        let entry = pattern_file.patterns.get(&method_key)
            .ok_or_else(|| format!("No IPC spec for {}", method_key))?;

        let func_addr = if let Some(addr) = entry.address {
            addr
        } else if let Some(offset) = entry.offset {
            steam_client_base.saturating_add(offset as u64)
        } else if let Some(pattern) = &entry.pattern {
            // Scan for pattern in steamclient.so
            let pattern_bytes = hex::decode(pattern).map_err(|e| format!("Invalid pattern hex: {}", e))?;
            let found = find_in_file(
                std::path::Path::new("steamclient.so"), &pattern_bytes, 4096
            ).ok_or_else(|| format!("Pattern '{}' not found in steamclient.so", method_name))?;
            found
        } else {
            continue;
        };

        let target = func_addr as *mut u8;
        let mut patch = unsafe { MemoryPatch::new(target, replacement as *mut u8) };

        // Validate the target is executable
        if !crate::steam_bridge::memory_patch::is_executable_mapped_range(func_addr, JMP_ABS64_LEN) {
            log::warn!("Skipping patch for {}: target not in executable memory", method_name);
            continue;
        }

        unsafe { patch.install(JMP_ABS64_LEN).map_err(|e| format!("Failed to install patch for {}: {}", method_name, e)) }?;
        patches.push(patch);
        log::info!("[InteropIPC_ISteamUser] Installed memory patch for IClientUser::{}, target=0x{:X}", method_name, func_addr);
    }

    Ok(patches)
}

/// Get or initialize the fresh eticket cache
pub fn fresh_eticket() -> &'static Mutex<HashMap<u32, Vec<u8>>> {
    FRESH_ETICKET.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Get the current pattern database
fn pattern_db() -> &'static PatternDb {
    static PATTERN_DB: OnceLock<PatternDb> = OnceLock::new();
    PATTERN_DB.get_or_init(PatternDb::new)
}

/// Get the IPC patches list
fn ipc_patches() -> &'static Mutex<Vec<MemoryPatch>> {
    static IPC_PATCHES: OnceLock<Mutex<Vec<MemoryPatch>>> = OnceLock::new();
    IPC_PATCHES.get_or_init(|| Mutex::new(Vec::new()))
}

/// Get steamclient base address
fn steam_client_base() -> u64 {
    *crate::steam_bridge::interop::interop_ipc::STEAM_CLIENT_BASE.get().unwrap_or(&0)
}

/// Resolve current app ID from InteropMisc
fn resolve_app_id_caller() -> u32 {
    resolve_app_id()
}

/// Check if app has depot configured
fn has_depot(app_id: u32) -> bool {
    app_id == ONLINE_FIX_APP_ID || app_id > 0
}

// ═══════════════════════════════════════════════════════════════════════════
// IClientUser Post-Handlers (matching Windows implementation)
// ═══════════════════════════════════════════════════════════════════════════

/// Post-handler: IClientUser::GetSteamID
///
/// Spoofs the SteamID when we have a pool-account ticket for this app.
/// Denuvo reads its cached offline license on second launch and calls
/// GetSteamID BEFORE or AFTER the auth window to verify it — if we only
/// spoof inside the window the real SteamID leaks out and mismatches
/// the license → error 012.
fn handler_post_get_steam_id(_pipe: *mut c_void, _read: &mut [u8], write: &mut [u8]) -> bool {
    let app_id = resolve_app_id_caller();
    if app_id == 0 {
        return false;
    }

    let store = TicketStore::global();
    let spoofed = store.get_spoof_steam_id(app_id);

    if spoofed == 0 {
        return false;
    }

    let mut resp = GetSteamIDResp::new(write);
    if !resp.ok() {
        return false;
    }

    log::debug!("[InteropIPC] GetSteamID: AppId={} spoofed to 0x{:X}", app_id, spoofed);
    resp.set_return_value(spoofed);
    true
}

/// Post-handler: IClientUser::GetAppOwnershipTicketExtendedData
///
/// Returns an app ownership ticket from the credential store.
/// Outside the Denuvo auth window: prefer credential-store ticket (pool SteamID)
/// over ForgeOnly (which uses app 7's ticket and carries the real SteamID).
/// When the 858 network spoof is also active, both paths must agree on the
/// same SteamID or Denuvo cross-checks them and rejects (error 54).
fn handler_post_get_app_ownership_ticket(_pipe: *mut c_void, read: &mut [u8], write: &mut [u8]) -> bool {
    let req = GetAppOwnershipTicketExtendedDataReq::parse(read);
    if !req.ok() {
        return false;
    }

    log::debug!("[InteropIPC] GetAppOwnershipTicketExtendedData");
    if req.max_ticket_size() < 0 {
        return false;
    }

    let app_id = if req.app_id() == ONLINE_FIX_APP_ID {
        resolve_app_id_caller()
    } else {
        req.app_id()
    };

    // Determine ticket source based on auth window
    let ticket_source = if is_online_fix_active() {
        // Inside auth window: use CredentialStoreOnly (pool SteamID)
        AppTicketSource::CredentialStoreOnly
    } else {
        // Outside auth window: prefer credential store over forge
        AppTicketSource::CredentialStoreThenForge
    };

    let ticket = get_app_ownership_ticket_with_source(app_id, ticket_source)?;

    if ticket.data.len() > req.max_ticket_size() as usize {
        log::warn!("[InteropIPC] Ticket too large for buffer");
        return false;
    }

    let mut resp = GetAppOwnershipTicketExtendedDataResp::new(write, req.max_ticket_size() as usize);
    if !resp.ok() {
        return false;
    }

    resp.set_return_value(ticket.total_size as u32);
    resp.set_ticket(&ticket.data);
    resp.set_app_id_offset(ticket.app_id_offset);
    resp.set_steam_id_offset(ticket.steam_id_offset);
    resp.set_signature_offset(ticket.signature_offset);
    resp.set_signature_size(ticket.signature_size);

    log::debug!("[InteropIPC] GetAppOwnershipTicketExtendedData: AppId={} size={}", app_id, ticket.data.len());
    true
}

/// Post-handler: IClientUser::RequestEncryptedAppTicket
///
/// Reads the hAsyncCall steamclient already wrote into the response,
/// so we know which AppId to mint an eticket for in GetAPICallResult.
/// Strict Denuvo passes a per-launch nonce (pData) here and rejects a
/// stale/cached ticket (error 88500012). Try an on-demand mint bound to
/// that exact nonce; cache it for GetEncryptedAppTicket. Any failure
/// falls through to the static credential store below.
fn handler_post_request_encrypted_app_ticket(_pipe: *mut c_void, read: &mut [u8], write: &mut [u8]) -> bool {
    let mut resp = RequestEncryptedAppTicketResp::new(write);
    if !resp.ok() {
        return false;
    }

    let app_id = resolve_app_id_caller();

    // Strict Denuvo passes a per-launch nonce (pData) here and rejects
    // a stale/cached ticket. Try an on-demand mint bound to that exact nonce.
    {
        let req = RequestEncryptedAppTicketReq::parse(read);
        let nonce = if req.ok() { req.data() } else { &[] };

        // Whatever account the registry's current static ticket already
        // belongs to — lets the backend pin the mint to that SAME account.
        let existing_steam_id = extract_steam_id_from_ticket_bytes(
            get_encrypted_ticket_from_store(app_id)
        );

        // Mint a fresh eticket whenever the credential store already has a ticket
        // for this app (existingSteamId != 0). The minted eticket is pinned to
        // the same pool account via existingSteamId, which matches GetSteamID's
        // spoof — no error-54 risk. This fixes error 05 for games launched more
        // than 30 min after activation (stored ticket expired, fresh mint is current).
        if existing_steam_id != 0 {
            if let Some(fresh) = fetch_fresh_eticket(app_id, nonce, existing_steam_id) {
                let mut cache = fresh_eticket().lock().unwrap();
                cache.insert(app_id, fresh);
            }
        }
    }

    let have_fresh = fresh_eticket().lock().unwrap().contains_key(&app_id);

    let ticket = get_encrypted_ticket_from_store(app_id);
    if ticket.is_empty() && !have_fresh {
        log::debug!("[InteropIPC] RequestEncryptedAppTicket: AppId={} - no cached eticket, skip", app_id);
        return false;
    }

    let async_call = resp.return_value();
    record_encrypted_ticket(async_call, app_id);
    log::debug!("[InteropIPC] RequestEncryptedAppTicket: AppId={} hAsyncCall=0x{:X} - recorded (fresh={})",
            app_id, async_call, have_fresh);
    true
}

/// Post-handler: IClientUser::GetEncryptedAppTicket
///
/// Prefers a fresh nonce-bound ticket minted in RequestEncryptedAppTicket;
/// falls back to the static credential-store ticket (titles that don't
/// need the on-demand path keep working unchanged).
fn handler_post_get_encrypted_app_ticket(_pipe: *mut c_void, _read: &mut [u8], write: &mut [u8]) -> bool {
    let app_id = resolve_app_id_caller();

    // Prefer fresh nonce-bound ticket minted in RequestEncryptedAppTicket
    let mut ticket = {
        let mut cache = fresh_eticket().lock().unwrap();
        cache.remove(&app_id).unwrap_or_default()
    };
    let from_fresh = !ticket.is_empty();

    if ticket.is_empty() {
        ticket = get_encrypted_ticket_from_store(app_id);
    }

    if ticket.is_empty() {
        log::debug!("[InteropIPC] GetEncryptedAppTicket: AppId={} - no cached eticket, skip", app_id);
        return false;
    }

    log::debug!("[InteropIPC] GetEncryptedAppTicket: AppId={} source={}", app_id,
            if from_fresh { "fresh" } else { "store" });

    let ticket_size = ticket.len() as u32;
    if !ensure_buffer_capacity_fn(write, ticket_size) {
        log::debug!("[InteropIPC] GetEncryptedAppTicket: AppId={} - failed to ensure buffer size", app_id);
        return false;
    }

    let mut resp = GetEncryptedAppTicketResp::new(write);
    if !resp.ok() {
        return false;
    }

    resp.set_return_value(true);
    resp.set_ticket_size(ticket_size);
    resp.set_ticket(&ticket);

    log::debug!("[InteropIPC] GetEncryptedAppTicket: AppId={} size={}", app_id, ticket_size);
    true
}

// ═══════════════════════════════════════════════════════════════════════════
// Helper functions and types
// ═══════════════════════════════════════════════════════════════════════════

/// Ticket source for app ownership
enum AppTicketSource {
    CredentialStoreOnly,
    CredentialStoreThenForge,
}

/// Get app ownership ticket with specific source
fn get_app_ownership_ticket_with_source(app_id: u32, _source: AppTicketSource) -> Option<AppOwnershipTicket> {
    // Would integrate with AppTicket module
    // Simplified implementation
    None
}

/// Extract SteamID from ticket bytes (simplified)
fn extract_steam_id_from_ticket_bytes(_ticket: Vec<u8>) -> u64 {
    // Simplified: would parse the ticket structure
    0
}

/// Fetch fresh eticket from backend (simplified)
fn fetch_fresh_eticket(_app_id: u32, _nonce: &[u8], _existing_steam_id: u64) -> Option<Vec<u8>> {
    // Would call EticketClient::FetchFreshEticket
    None
}

/// Get encrypted ticket from credential store
fn get_encrypted_ticket_from_store(app_id: u32) -> Vec<u8> {
    TicketStore::global().get_encrypted_ticket(app_id).unwrap_or_default()
}

/// Record async call for encrypted ticket
fn record_encrypted_ticket(async_call: u64, app_id: u32) {
    TicketStore::global().record_async_call(async_call, app_id);
}

/// Take encrypted ticket for async call
fn take_encrypted_ticket(async_call: u64) -> u32 {
    TicketStore::global().take_async_call(async_call).unwrap_or(0)
}

/// Ensure buffer capacity for write
fn ensure_buffer_capacity_fn(write: &mut [u8], needed: u32) -> bool {
    write.len() >= needed as usize
}

/// Compute simple hash for interface::method
fn compute_func_hash(interface: &str, method: &str) -> u32 {
    // Simple hash: FNV-1a using std hasher
    let mut hash: u64 = 0x811c9dc5;
    let prime: u64 = 0x01000193;

    // Hash interface string
    for byte in interface.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(prime);
    }

    // Hash method separator
    for byte in "::".bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(prime);
    }

    // Hash method string
    for byte in method.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(prime);
    }

    hash as u32
}

// ═══════════════════════════════════════════════════════════════════════════
// IPC Request/Response Parsers
// ═══════════════════════════════════════════════════════════════════════════

/// GetSteamID Response
#[derive(Debug, Clone)]
struct GetSteamIDResp {
    data: Vec<u8>,
    valid: bool,
}

impl GetSteamIDResp {
    fn new(data: &mut [u8]) -> Self {
        Self { data: data.to_vec(), valid: data.len() >= 8 }
    }

    fn ok(&self) -> bool {
        self.valid
    }

    fn set_return_value(&mut self, val: u64) {
        if self.data.len() >= 8 {
            self.data[0..8].copy_from_slice(&val.to_le_bytes());
        }
    }
}

/// GetAppOwnershipTicketExtendedData Request
#[derive(Debug, Clone)]
struct GetAppOwnershipTicketExtendedDataReq {
    data: Vec<u8>,
    valid: bool,
}

impl GetAppOwnershipTicketExtendedDataReq {
    fn parse(data: &[u8]) -> Self {
        if data.len() < 8 {
            return Self { data: Vec::new(), valid: false };
        }
        Self { data: data.to_vec(), valid: true }
    }

    fn ok(&self) -> bool {
        self.valid
    }

    fn app_id(&self) -> u32 {
        if self.data.len() >= 4 {
            u32::from_le_bytes([self.data[0], self.data[1], self.data[2], self.data[3]])
        } else {
            0
        }
    }

    fn max_ticket_size(&self) -> i32 {
        if self.data.len() >= 8 {
            i32::from_le_bytes([self.data[4], self.data[5], self.data[6], self.data[7]])
        } else {
            -1
        }
    }
}

/// GetAppOwnershipTicketExtendedData Response
#[derive(Debug, Clone)]
struct GetAppOwnershipTicketExtendedDataResp {
    data: Vec<u8>,
    valid: bool,
}

impl GetAppOwnershipTicketExtendedDataResp {
    fn new(data: &mut [u8], capacity: usize) -> Self {
        Self { data: data.to_vec(), valid: data.len() >= 4 }
    }

    fn ok(&self) -> bool {
        self.valid
    }

    fn set_return_value(&mut self, val: u32) {
        if self.data.len() >= 4 {
            self.data[0..4].copy_from_slice(&val.to_le_bytes());
        }
    }

    fn set_ticket(&mut self, ticket: &[u8]) {
        if self.data.len() >= 4 + ticket.len() {
            self.data[4..4+ticket.len()].copy_from_slice(ticket);
        }
    }

    fn set_app_id_offset(&mut self, offset: i32) {
        if self.data.len() >= 8 {
            self.data[4..8].copy_from_slice(&offset.to_le_bytes());
        }
    }

    fn set_steam_id_offset(&mut self, offset: i32) {
        if self.data.len() >= 12 {
            self.data[8..12].copy_from_slice(&offset.to_le_bytes());
        }
    }

    fn set_signature_offset(&mut self, offset: i32) {
        if self.data.len() >= 16 {
            self.data[12..16].copy_from_slice(&offset.to_le_bytes());
        }
    }

    fn set_signature_size(&mut self, size: i32) {
        if self.data.len() >= 20 {
            self.data[16..20].copy_from_slice(&size.to_le_bytes());
        }
    }
}

/// RequestEncryptedAppTicket Request
#[derive(Debug, Clone)]
struct RequestEncryptedAppTicketReq {
    data: Vec<u8>,
    valid: bool,
}

impl RequestEncryptedAppTicketReq {
    fn parse(data: &[u8]) -> Self {
        if data.len() < 4 {
            return Self { data: Vec::new(), valid: false };
        }
        Self { data: data.to_vec(), valid: true }
    }

    fn ok(&self) -> bool {
        self.valid
    }

    fn data(&self) -> &[u8] {
        if self.data.len() >= 8 {
            let len = u32::from_le_bytes([self.data[0], self.data[1], self.data[2], self.data[3]]) as usize;
            if self.data.len() >= 4 + len {
                return &self.data[4..4+len];
            }
        }
        &[]
    }
}

/// RequestEncryptedAppTicket Response
#[derive(Debug, Clone)]
struct RequestEncryptedAppTicketResp {
    data: Vec<u8>,
    valid: bool,
}

impl RequestEncryptedAppTicketResp {
    fn new(data: &mut [u8]) -> Self {
        Self { data: data.to_vec(), valid: data.len() >= 8 }
    }

    fn ok(&self) -> bool {
        self.valid
    }

    fn return_value(&self) -> u64 {
        if self.data.len() >= 8 {
            u64::from_le_bytes([
                self.data[0], self.data[1], self.data[2], self.data[3],
                self.data[4], self.data[5], self.data[6], self.data[7],
            ])
        } else {
            0
        }
    }
}

/// GetEncryptedAppTicket Response
#[derive(Debug, Clone)]
struct GetEncryptedAppTicketResp {
    data: Vec<u8>,
    valid: bool,
}

impl GetEncryptedAppTicketResp {
    fn new(data: &mut [u8]) -> Self {
        Self { data: data.to_vec(), valid: data.len() >= 5 }
    }

    fn ok(&self) -> bool {
        self.valid
    }

    fn set_return_value(&mut self, val: bool) {
        if !self.data.is_empty() {
            self.data[0] = val as u8;
        }
    }

    fn set_ticket_size(&mut self, size: u32) {
        if self.data.len() >= 5 {
            self.data[1..5].copy_from_slice(&size.to_le_bytes());
        }
    }

    fn set_ticket(&mut self, ticket: &[u8]) {
        if self.data.len() >= 5 + ticket.len() {
            self.data[5..5+ticket.len()].copy_from_slice(ticket);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_func_hash() {
        let hash1 = compute_func_hash("IClientUser", "GetSteamID");
        let hash2 = compute_func_hash("IClientUser", "GetSteamID");
        assert_eq!(hash1, hash2);

        let hash3 = compute_func_hash("IClientUser", "GetAppID");
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_method_hashes() {
        assert_eq!(compute_func_hash("IClientUser", "GetSteamID"), compute_func_hash("IClientUser", "GetSteamID"));
        assert_ne!(compute_func_hash("IClientUser", "GetSteamID"), compute_func_hash("IClientUser", "GetAppID"));
    }

    #[test]
    fn test_get_steam_id_resp() {
        let mut buf = vec![0u8; 8];
        let mut resp = GetSteamIDResp::new(&mut buf);
        assert!(resp.ok());
        resp.set_return_value(0x123456789ABCDEF0);
        assert_eq!(&buf[0..8], &0x123456789ABCDEF0_u64.to_le_bytes());
    }

    #[test]
    fn test_get_app_ownership_ticket_req() {
        let data = [1u32.to_le_bytes(), (-1i32).to_le_bytes()].concat();
        let req = GetAppOwnershipTicketExtendedDataReq::parse(&data);
        assert!(req.ok());
        assert_eq!(req.app_id(), 1);
        assert_eq!(req.max_ticket_size(), -1);
    }

    #[test]
    fn test_request_encrypted_app_ticket_req() {
        let data = [4u32.to_le_bytes(), [0xDE, 0xAD, 0xBE, 0xEF]].concat();
        let req = RequestEncryptedAppTicketReq::parse(&data);
        assert!(req.ok());
        assert_eq!(req.data(), &[0xDE, 0xAD, 0xBE, 0xEF]);
    }
}