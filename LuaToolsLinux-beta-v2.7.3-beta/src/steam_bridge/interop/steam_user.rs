//! Interop ISteamUser IPC handlers for Linux.
//!
//! Port of usefulsteamtools `Interop_IPC_ISteamUser.cpp`.
//! Handles: GetSteamID, GetAppOwnershipTicketExtendedData, RequestEncryptedAppTicket, GetEncryptedAppTicket.
//! On Linux: uses ticket_store.rs (localconfig.vdf) for credential storage,
//! integrates with EticketClient for on-demand ticket minting.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::os::raw::c_void;

use crate::steam_bridge::ticket_store::{AppTicketData, TicketStore};
use crate::steam_bridge::ost_platform::steam_credential_store::{
    get_app_ticket, write_app_ticket, get_e_ticket, write_e_ticket,
    get_steam_id, write_steam_id,
};
use crate::steam_bridge::interop::misc::{resolve_app_id, is_online_fix_active, should_report_online_fix_app_id};

// ============================================================================
// Constants
// ============================================================================

const ONLINE_FIX_APP_ID: u32 = 480; // Spacewar
const APP_TICKET_SIGNATURE_SIZE: usize = 8; // Last 8 bytes are signature

// ============================================================================
// Global state for fresh etickets (minted on-demand per launch)
// ============================================================================

static FRESH_ETICKETS: LazyLock<Mutex<HashMap<u32, Vec<u8>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn fresh_etickets() -> &'static Mutex<HashMap<u32, Vec<u8>>> {
    FRESH_ETICKETS.get_or_init(|| Mutex::new(HashMap::new()))
}

// ============================================================================
// Ticket source enum (matches usefulsteamtools)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppTicketSource {
    CredentialStoreOnly,
    CredentialStoreThenForge,
    ForgeOnly,
}

#[derive(Debug, Clone, Default)]
pub struct AppOwnershipTicket {
    pub data: Vec<u8>,
    pub total_size: u32,
    pub app_id_offset: u32,
    pub steam_id_offset: u32,
    pub signature_offset: u32,
    pub signature_size: u32,
}

impl AppOwnershipTicket {
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

// ============================================================================
// SteamID extraction from ticket bytes
// ============================================================================

/// Extract SteamID from ticket bytes
/// Layout: [uint32 Size][uint32 Version][uint64 SteamID][...]
pub fn extract_steam_id_from_ticket_bytes(ticket: &[u8]) -> u64 {
    if ticket.len() < 16 { // Size(4) + Version(4) + SteamID(8)
        return 0;
    }
    // SteamID is at offset 8 (after Size and Version)
    u64::from_le_bytes([
        ticket[8], ticket[9], ticket[10], ticket[11],
        ticket[12], ticket[13], ticket[14], ticket[15],
    ])
}

// ============================================================================
// Spoofed SteamID for an app
// ============================================================================

/// Get spoofed SteamID for an app
/// Returns 0 if no spoofing should occur
pub fn get_spoof_steam_id(app_id: u32) -> u64 {
    // Only spoof for apps in our config (have depot entries)
    if !crate::steam_bridge::ost_platform::lua_config::has_depot(app_id) {
        return 0;
    }

    // Try credential store first
    let mut steam_id = 0u64;
    if get_steam_id(app_id, &mut steam_id) == crate::steam_bridge::ost_platform::steam_credential_store::Status::Ok {
        return steam_id;
    }

    // Fallback: extract from cached AppOwnershipTicket
    extract_steam_id_from_ticket_bytes(&get_cached_app_ownership_ticket(app_id))
}

fn get_cached_app_ownership_ticket(app_id: u32) -> Vec<u8> {
    let mut ticket = Vec::new();
    let _ = get_app_ticket(app_id, &mut ticket);
    ticket
}

// ============================================================================
// AppTicket forging (core usefulsteamtools technique)
// ============================================================================

/// Forge a local AppOwnershipTicket by splicing app_id into a cached signed ticket
/// The source ticket must be a real signed ticket from the same Steam account
pub fn forge_local_app_ownership_ticket(app_id: u32) -> Option<Vec<u8>> {
    let source = pick_forge_source_ticket()?;

    // Layout: signed_part | app_id (4 bytes) | signature (8 bytes)
    let signature_size = APP_TICKET_SIGNATURE_SIZE;
    let signed_size = source.data.len().saturating_sub(signature_size);

    let mut ticket = Vec::with_capacity(source.data.len() + 4);

    // Copy signed portion
    ticket.extend_from_slice(&source.data[..signed_size]);

    // Splice in the target app_id (little endian)
    ticket.extend_from_slice(&app_id.to_le_bytes());

    // Copy signature
    ticket.extend_from_slice(&source.data[signed_size..]);

    log::info!(
        "Forged App Ownership Ticket: AppId={}, SourceAppId={}, PhysicalSize={}, TotalSize={}",
        app_id, source.app_id, ticket.len(), source.data.len()
    );

    Some(ticket)
}

/// Pick the best source ticket for forging
fn pick_forge_source_ticket() -> Option<SourceTicket> {
    // Try cached source ticket first
    {
        let cache = SOURCE_TICKET_CACHE.lock().unwrap();
        if let Some(ref ticket) = *cache {
            return Some(ticket.clone());
        }
    }

    // Try to read from localconfig.vdf
    if let Some(ticket) = read_local_config_source_ticket() {
        *SOURCE_TICKET_CACHE.lock().unwrap() = Some(ticket.clone());
        return Some(ticket);
    }

    // Fallback: try well-known app tickets
    const PRIMARY_SOURCE_APP_ID: u32 = 7;         // Steam client
    const FALLBACK_SOURCE_APP_ID: u32 = 228980;   // The Witness

    if let Some(ticket) = try_source_app_id(PRIMARY_SOURCE_APP_ID) {
        return Some(ticket);
    }
    if let Some(ticket) = try_source_app_id(FALLBACK_SOURCE_APP_ID) {
        return Some(ticket);
    }

    // Try any seen app tickets from decryption hook
    // (would iterate InteropDecryption::GetSeenAppTicketIds())

    None
}

#[derive(Debug, Clone)]
struct SourceTicket {
    app_id: u32,
    data: Vec<u8>,
}

static SOURCE_TICKET_CACHE: LazyLock<Mutex<Option<SourceTicket>>> = LazyLock::new(|| Mutex::new(None));

fn source_ticket_cache() -> &'static Mutex<Option<SourceTicket>> {
    SOURCE_TICKET_CACHE.get_or_init(|| Mutex::new(None))
}

fn try_source_app_id(app_id: u32) -> Option<SourceTicket> {
    let mut ticket = Vec::new();
    if get_app_ticket(app_id, &mut ticket) == crate::steam_bridge::ost_platform::steam_credential_store::Status::Ok {
        if is_valid_source_ticket(&ticket) {
            return Some(SourceTicket { app_id, data: ticket });
        }
    }
    None
}

fn is_valid_source_ticket(ticket: &[u8]) -> bool {
    !ticket.is_empty() && ticket.len() > APP_TICKET_SIGNATURE_SIZE
}

/// Read the active account's localconfig.vdf and return the first valid cached app ticket
fn read_local_config_source_ticket() -> Option<SourceTicket> {
    // Would read ~/.steam/steam/userdata/<accountId>/config/localconfig.vdf
    // Parse "apptickets" section for first valid ticket
    // For now, return None (falls back to well-known apps)
    None
}

// ============================================================================
// AppTicket handlers (matching usefulsteamtools)
// ============================================================================

/// Get AppOwnershipTicket from credential store or forge one
pub fn get_app_ownership_ticket(app_id: u32, source: AppTicketSource) -> Option<AppOwnershipTicket> {
    let mut ticket = AppOwnershipTicket::default();

    // Try credential store first
    if source == AppTicketSource::CredentialStoreOnly || source == AppTicketSource::CredentialStoreThenForge {
        let mut data = Vec::new();
        if get_app_ticket(app_id, &mut data) == crate::steam_bridge::ost_platform::steam_credential_store::Status::Ok {
            if data.len() >= 4 {
                ticket.data = data;
                ticket.total_size = ticket.data.len() as u32;
                ticket.app_id_offset = 0; // Offset to app_id in ticket
                ticket.steam_id_offset = 8; // Offset to SteamID
                ticket.signature_offset = u32::from_le_bytes([
                    ticket.data[0], ticket.data[1], ticket.data[2], ticket.data[3]
                ]) as u32;
                ticket.signature_size = APP_TICKET_SIGNATURE_SIZE as u32;
                return Some(ticket);
            }
        }
    }

    // Forge if allowed
    if source != AppTicketSource::CredentialStoreOnly {
        if let Some(forged_data) = forge_local_app_ownership_ticket(app_id) {
            ticket.data = forged_data;
            ticket.total_size = (ticket.data.len() - 4) as u32; // Exclude spliced app_id
            ticket.app_id_offset = ticket.total_size - APP_TICKET_SIGNATURE_SIZE as u32;
            ticket.steam_id_offset = 8;
            ticket.signature_offset = ticket.app_id_offset + 4;
            ticket.signature_size = APP_TICKET_SIGNATURE_SIZE as u32;
            return Some(ticket);
        }
    }

    None
}

/// Get AppOwnershipTicket from credential store only
pub fn get_app_ownership_ticket_from_credential_store(app_id: u32) -> Vec<u8> {
    if !crate::steam_bridge::ost_platform::lua_config::has_depot(app_id) {
        return Vec::new();
    }
    let mut ticket = Vec::new();
    let _ = get_app_ticket(app_id, &mut ticket);
    ticket
}

/// Get Encrypted App Ticket (ETicket) from credential store
pub fn get_encrypted_ticket_from_credential_store(app_id: u32) -> Vec<u8> {
    if !crate::steam_bridge::ost_platform::lua_config::has_depot(app_id) {
        return Vec::new();
    }
    let mut ticket = Vec::new();
    let _ = get_e_ticket(app_id, &mut ticket);
    ticket
}

/// Write AppOwnershipTicket to credential store
pub fn write_app_ownership_ticket(app_id: u32, data: &[u8]) -> bool {
    write_app_ticket(app_id, data) == crate::steam_bridge::ost_platform::steam_credential_store::Status::Ok
}

/// Write Encrypted App Ticket to credential store
pub fn write_encrypted_ticket(app_id: u32, data: &[u8]) -> bool {
    write_e_ticket(app_id, data) == crate::steam_bridge::ost_platform::steam_credential_store::Status::Ok
}

/// Write spoofed SteamID to credential store
pub fn write_steam_id_credential(app_id: u32, steam_id: u64) -> bool {
    write_steam_id(app_id, steam_id) == crate::steam_bridge::ost_platform::steam_credential_store::Status::Ok
}

// ============================================================================
// IPC Post-Handlers (called after Steam processes the message)
// ============================================================================

/// Post-handler: IClientUser::GetSteamID
/// Spoofs the returned SteamID to match the pool account
pub fn handler_post_get_steam_id(
    pipe: *mut c_void,
    read_buf: &[u8],
    write_buf: &mut [u8],
) {
    let app_id = resolve_app_id();

    // Parse response
    let mut resp = GetSteamIDResp::new(write_buf);
    if !resp.is_ok() {
        return;
    }

    let spoofed = get_spoof_steam_id(app_id);
    if spoofed == 0 {
        return;
    }

    log::debug!("IClientUser::GetSteamID: AppId={} Original={} -> Spoofed=0x{:X}({})",
        app_id, resp.get_return_value(), spoofed, spoofed);

    resp.set_return_value(spoofed);
}

/// Post-handler: IClientUser::GetAppOwnershipTicketExtendedData
/// Returns forged or cached ownership ticket
pub fn handler_post_get_app_ownership_ticket_extended_data(
    pipe: *mut c_void,
    read_buf: &[u8],
    write_buf: &mut [u8],
) {
    let mut req = GetAppOwnershipTicketExtendedDataReq::new(read_buf);
    if !req.is_ok() || req.max_ticket_size() < 0 {
        return;
    }

    log::debug!("IClientUser::GetAppOwnershipTicketExtendedData: {:?}", req);

    let app_id = if req.app_id() == ONLINE_FIX_APP_ID {
        resolve_app_id()
    } else {
        req.app_id()
    };

    // Choose ticket source based on Denuvo auth window
    let ticket_source = if is_denuvo_authorized_pipe(pipe) {
        AppTicketSource::CredentialStoreOnly
    } else {
        AppTicketSource::CredentialStoreThenForge
    };

    let Some(ticket) = get_app_ownership_ticket(app_id, ticket_source) else {
        return;
    };

    if ticket.data.len() > req.max_ticket_size() as usize {
        log::warn!("Ticket too large ({} bytes) for buffer ({} bytes)",
            ticket.data.len(), req.max_ticket_size());
        return;
    }

    let mut resp = GetAppOwnershipTicketExtendedDataResp::new(write_buf, req.max_ticket_size() as usize);
    if !resp.is_ok() {
        return;
    }

    resp.set_return_value(ticket.total_size);
    resp.set_ticket(&ticket.data);
    resp.set_app_id_offset(ticket.app_id_offset);
    resp.set_steam_id_offset(ticket.steam_id_offset);
    resp.set_signature_offset(ticket.signature_offset);
    resp.set_signature_size(ticket.signature_size);

    log::debug!("IClientUser::GetAppOwnershipTicketExtendedData: AppId={} {:?}", app_id, resp);
}

/// Post-handler: IClientUser::RequestEncryptedAppTicket
/// Reads hAsyncCall and optionally mints fresh nonce-bound eticket
pub fn handler_post_request_encrypted_app_ticket(
    pipe: *mut c_void,
    read_buf: &[u8],
    write_buf: &mut [u8],
) {
    let mut resp = RequestEncryptedAppTicketResp::new(write_buf);
    if !resp.is_ok() {
        return;
    }

    let app_id = resolve_app_id();

    // Try to mint fresh nonce-bound eticket for strict Denuvo
    let mut have_fresh = false;
    {
        let mut req = RequestEncryptedAppTicketReq::new(read_buf);
        if req.is_ok() {
            let nonce = req.nonce_data();
            let existing_steam_id = extract_steam_id_from_ticket_bytes(
                &get_app_ownership_ticket_from_credential_store(app_id)
            );

            if existing_steam_id != 0 {
                // Mint fresh eticket via backend
                if let Some(fresh) = fetch_fresh_eticket(app_id, &nonce, existing_steam_id) {
                    fresh_etickets().lock().unwrap().insert(app_id, fresh);
                    have_fresh = true;
                }
            }
        }
    }

    // Get cached eticket
    let cached = get_encrypted_ticket_from_credential_store(app_id);

    if cached.is_empty() && !have_fresh {
        log::debug!("RequestEncryptedAppTicket: AppId={} - no cached eticket, skip", app_id);
        return;
    }

    let async_call = resp.get_return_value();
    record_encrypted_ticket(async_call, app_id);

    log::debug!("RequestEncryptedAppTicket: AppId={} hAsyncCall=0x{:X} - recorded (fresh={})",
        app_id, async_call, have_fresh);
}

/// Post-handler: IClientUser::GetEncryptedAppTicket
/// Returns fresh nonce-bound eticket or cached one
pub fn handler_post_get_encrypted_app_ticket(
    pipe: *mut c_void,
    read_buf: &[u8],
    write_buf: &mut [u8],
) {
    let app_id = resolve_app_id();

    // Try fresh eticket first
    let mut ticket = Vec::new();
    {
        let mut cache = fresh_etickets().lock().unwrap();
        if let Some(fresh) = cache.remove(&app_id) {
            ticket = fresh;
        }
    }

    let from_fresh = !ticket.is_empty();

    if ticket.is_empty() {
        ticket = get_encrypted_ticket_from_credential_store(app_id);
    }

    if ticket.is_empty() {
        log::debug!("GetEncryptedAppTicket: AppId={} - no cached eticket, skip", app_id);
        return;
    }

    log::debug!("GetEncryptedAppTicket: AppId={} serving source={}", app_id, if from_fresh { "fresh" } else { "store" });

    let ticket_size = ticket.len() as u32;
    let new_capacity = write_buf.len() + ticket.len();

    // Ensure buffer capacity
    if !ensure_buffer_capacity(write_buf, new_capacity, true) {
        log::debug!("GetEncryptedAppTicket: AppId={} - failed to ensure buffer size", app_id);
        return;
    }

    let mut resp = GetEncryptedAppTicketResp::new(write_buf);
    if !resp.is_ok() {
        return;
    }

    resp.set_return_value(true);
    resp.set_ticket_size(ticket_size);
    resp.set_ticket(&ticket);

    log::debug!("GetEncryptedAppTicket: AppId={} {:?}", app_id, resp);
}

// ============================================================================
// Helper types and functions (simplified)
// ============================================================================

struct GetSteamIDResp<'a> {
    buf: &'a mut [u8],
}

impl<'a> GetSteamIDResp<'a> {
    fn new(buf: &'a mut [u8]) -> Self { Self { buf } }
    fn is_ok(&self) -> bool { self.buf.len() >= 8 }
    fn get_return_value(&self) -> u64 {
        u64::from_le_bytes([
            self.buf[0], self.buf[1], self.buf[2], self.buf[3],
            self.buf[4], self.buf[5], self.buf[5], self.buf[7],
        ])
    }
    fn set_return_value(&mut self, val: u64) {
        self.buf[0..8].copy_from_slice(&val.to_le_bytes());
    }
}

// Note: GetAppOwnershipTicketExtendedDataReq, GetAppOwnershipTicketExtendedDataResp,
// GetAPICallResultReq, GetAPICallResultResp, RequestEncryptedAppTicketResp are
// defined in steam_utils.rs

// ============================================================================
// Backend API integration
// ============================================================================

/// Fetch fresh nonce-bound eticket from backend
fn fetch_fresh_eticket(app_id: u32, nonce: &[u8], existing_steam_id: u64) -> Option<Vec<u8>> {
    // Would call backend API: POST /api/eticket with {app_id, nonce, existing_steam_id}
    // For now, return None (falls back to cached)
    log::debug!("Would fetch fresh eticket for app {}, nonce len {}, steam_id {}", app_id, nonce.len(), existing_steam_id);
    None
}

/// Record async call handle -> app_id mapping
fn record_encrypted_ticket(async_call: u64, app_id: u32) {
    crate::steam_bridge::interop::steam_utils::record_encrypted_ticket(async_call, app_id);
}

/// Take and remove async call -> app_id mapping
pub fn take_encrypted_ticket(async_call: u64) -> Option<u32> {
    crate::steam_bridge::interop::steam_utils::take_encrypted_ticket(async_call)
}

/// Check if pipe is in Denuvo auth window
fn is_denuvo_authorized_pipe(pipe: *mut c_void) -> bool {
    // Would check PipeManager::DenuvoAuth::IsAuthorizedPipe
    false
}

/// Ensure buffer capacity (from InteropMisc)
fn ensure_buffer_capacity(buf: &mut [u8], new_capacity: usize, update_put: bool) -> bool {
    if buf.len() >= new_capacity {
        return true;
    }
    // In real impl, would grow CUtlBuffer
    false
}

// ============================================================================
// Module tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_steam_id() {
        // Create a mock ticket with known SteamID
        let mut ticket = vec![0u8; 32];
        ticket[0..4].copy_from_slice(&28u32.to_le_bytes()); // Size
        ticket[4..8].copy_from_slice(&4u32.to_le_bytes());  // Version
        ticket[8..16].copy_from_slice(&0x123456789ABCDEFFu64.to_le_bytes()); // SteamID
        assert_eq!(extract_steam_id_from_ticket_bytes(&ticket), 0x123456789ABCDEFF);
    }

    #[test]
    fn test_is_valid_source_ticket() {
        assert!(is_valid_source_ticket(&vec![0u8; 100]));
        assert!(!is_valid_source_ticket(&vec![0u8; 5]));
        assert!(!is_valid_source_ticket(&[]));
    }
}