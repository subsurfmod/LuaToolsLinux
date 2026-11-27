//! Interop IPC - IClientUser Interface Handlers.
//!
//! Linux port of usefulsteamtools `Interop_IPC_ISteamUser.cpp`.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Mutex, OnceLock};

use crate::steam_bridge::credentials::appticket::AppTicketSource;
use crate::steam_bridge::credentials::eticketclient::EticketClient;
use crate::steam_bridge::interop::interop_ipc::register_handler;
use crate::steam_bridge::interop::interop_misc::{is_online_fix_active, ONLINE_FIX_APP_ID, resolve_app_id};
use crate::steam_bridge::interop::ipc::fnv1a_hash;
use crate::steam_bridge::ticket_store::TicketStore;

static FRESH_ETICKET: OnceLock<Mutex<HashMap<u32, Vec<u8>>>> = OnceLock::new();


/// Decoded ownership ticket with field offsets.
#[derive(Debug, Clone)]
pub struct AppOwnershipTicket {
    pub data: Vec<u8>,
    pub total_size: usize,
    pub app_id_offset: i32,
    pub steam_id_offset: i32,
    pub signature_offset: i32,
    pub signature_size: i32,
}

/// Install IClientUser IPC handlers.
pub fn install_iclientuser_handlers() -> Result<(), String> {
    register_handler(
        "IClientUser",
        "GetSteamID",
        None,
        Some(Arc::new(handler_post_get_steam_id)),
    )?;

    register_handler(
        "IClientUser",
        "GetAppOwnershipTicketExtendedData",
        None,
        Some(Arc::new(handler_post_get_app_ownership_ticket)),
    )?;

    register_handler(
        "IClientUser",
        "RequestEncryptedAppTicket",
        None,
        Some(Arc::new(handler_post_request_encrypted_app_ticket)),
    )?;

    register_handler(
        "IClientUser",
        "GetEncryptedAppTicket",
        None,
        Some(Arc::new(handler_post_get_encrypted_app_ticket)),
    )?;

    log::info!("[InteropIPC_ISteamUser] Installed 4 IClientUser handlers");
    Ok(())
}

/// Get or initialize the fresh eticket cache.
pub fn fresh_eticket() -> &'static Mutex<HashMap<u32, Vec<u8>>> {
    FRESH_ETICKET.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Post-handler: `IClientUser::GetSteamID` with per-pipe AppID resolution (BST-52).
pub fn handler_post_get_steam_id(pipe: *mut c_void, _read: &mut [u8], write: &mut [u8]) -> bool {
    let app_id = {
        let from_pipe = crate::steam_bridge::interop::interop_ipc::get_pipe_app_id(pipe);
        if from_pipe != 0 {
            from_pipe
        } else {
            resolve_app_id()
        }
    };
    if app_id == 0 {
        return false;
    }

    let spoofed = {
        let mut store = TicketStore::global()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        store.get_spoof_steam_id(app_id)
    };
    if spoofed == 0 {
        return false;
    }

    let mut resp = GetSteamIDResp::new(write);
    if !resp.ok() {
        return false;
    }

    log::debug!(
        "[InteropIPC] GetSteamID: AppId={} spoofed to 0x{:X}",
        app_id,
        spoofed
    );
    resp.set_return_value(spoofed);
    true
}

/// Post-handler: `IClientUser::GetAppOwnershipTicketExtendedData` with per-pipe isolation (BST-52/BST-55).
pub fn handler_post_get_app_ownership_ticket(
    pipe: *mut c_void,
    read: &mut [u8],
    write: &mut [u8],
) -> bool {
    let req = GetAppOwnershipTicketExtendedDataReq::parse(read);
    if !req.ok() || req.max_ticket_size() < 0 {
        return false;
    }

    let app_id = {
        let from_pipe = crate::steam_bridge::interop::interop_ipc::get_pipe_app_id(pipe);
        if from_pipe != 0 {
            from_pipe
        } else if req.app_id() != 0 && req.app_id() != ONLINE_FIX_APP_ID {
            req.app_id()
        } else {
            resolve_app_id()
        }
    };
    if app_id == 0 {
        return false;
    }

    let pipe_handle = crate::steam_bridge::interop::interop_ipc::get_pipe_handle(pipe);
    let pipe_pid = crate::steam_bridge::interop::interop_ipc::get_pipe_pid(pipe);
    let is_denuvo_auth = crate::steam_bridge::denuvo_auth::is_pipe_authorized(pipe_handle, app_id, pipe_pid);
    let ticket_source = if is_online_fix_active() || is_denuvo_auth {
        AppTicketSource::CredentialStoreOnly
    } else {
        AppTicketSource::CredentialStoreThenForge
    };

    let Some(ticket) = get_app_ownership_ticket_with_source(app_id, ticket_source) else {
        return false;
    };

    write_ownership_ticket_response(&ticket, req.max_ticket_size() as usize, write)
}

fn write_ownership_ticket_response(
    ticket: &AppOwnershipTicket,
    max_ticket_size: usize,
    write: &mut [u8],
) -> bool {
    if ticket.data.len() > max_ticket_size {
        log::warn!("[InteropIPC] Ticket too large for caller max_ticket_size");
        return false;
    }

    let Some(required_len) = 20usize.checked_add(ticket.data.len()) else {
        return false;
    };
    if write.len() < required_len {
        log::warn!(
            "[InteropIPC] Write buffer too small for ownership ticket ({} < {})",
            write.len(),
            required_len
        );
        return false;
    }

    let mut resp = GetAppOwnershipTicketExtendedDataResp::new(write);
    if !resp.ok() {
        return false;
    }

    resp.set_return_value(ticket.total_size as u32);
    resp.set_app_id_offset(ticket.app_id_offset);
    resp.set_steam_id_offset(ticket.steam_id_offset);
    resp.set_signature_offset(ticket.signature_offset);
    resp.set_signature_size(ticket.signature_size);
    resp.set_ticket(&ticket.data);

    log::debug!(
        "[InteropIPC] GetAppOwnershipTicketExtendedData: size={}",
        ticket.data.len()
    );
    true
}

/// Post-handler: `IClientUser::RequestEncryptedAppTicket` with per-pipe isolation (BST-52).
pub fn handler_post_request_encrypted_app_ticket(
    pipe: *mut c_void,
    read: &mut [u8],
    write: &mut [u8],
) -> bool {
    let resp = RequestEncryptedAppTicketResp::new(write);
    if !resp.ok() {
        return false;
    }

    let app_id = {
        let from_pipe = crate::steam_bridge::interop::interop_ipc::get_pipe_app_id(pipe);
        if from_pipe != 0 {
            from_pipe
        } else {
            resolve_app_id()
        }
    };
    if app_id == 0 {
        return false;
    }

    {
        let req = RequestEncryptedAppTicketReq::parse(read);
        let nonce = if req.ok() { req.data() } else { &[] };
        let existing_steam_id = {
            let mut store = TicketStore::global()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            store.get_spoof_steam_id(app_id)
        };
        if let Some(fresh) = EticketClient::fetch_fresh_eticket(app_id, nonce, existing_steam_id) {
            fresh_eticket()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(app_id, fresh);
        }
    }

    let have_fresh = fresh_eticket()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(&app_id);

    let ticket = get_encrypted_ticket_from_store(app_id);
    if ticket.is_empty() && !have_fresh {
        return false;
    }

    let async_call = resp.return_value();
    record_encrypted_ticket(async_call, app_id);
    log::debug!(
        "[InteropIPC] RequestEncryptedAppTicket: AppId={} hAsyncCall=0x{:X} (fresh={})",
        app_id,
        async_call,
        have_fresh
    );
    true
}

/// Post-handler: `IClientUser::GetEncryptedAppTicket` with per-pipe isolation (BST-52).
pub fn handler_post_get_encrypted_app_ticket(
    pipe: *mut c_void,
    _read: &mut [u8],
    write: &mut [u8],
) -> bool {
    let app_id = {
        let from_pipe = crate::steam_bridge::interop::interop_ipc::get_pipe_app_id(pipe);
        if from_pipe != 0 {
            from_pipe
        } else {
            resolve_app_id()
        }
    };
    if app_id == 0 {
        return false;
    }

    // Peek before removing so an undersized write buffer does not destroy the cached fresh ticket.
    let cached_fresh = fresh_eticket()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&app_id)
        .cloned();

    let (ticket, from_fresh) = match cached_fresh {
        Some(t) if !t.is_empty() => (t, true),
        _ => (get_encrypted_ticket_from_store(app_id), false),
    };

    if ticket.is_empty() {
        return false;
    }

    let Some(required_len) = 5usize.checked_add(ticket.len()) else {
        return false;
    };
    if write.len() < required_len {
        return false;
    }

    let mut resp = GetEncryptedAppTicketResp::new(write);
    if !resp.ok() {
        return false;
    }

    let ticket_size = u32::try_from(ticket.len()).ok();
    let Some(ticket_size) = ticket_size else {
        return false;
    };

    resp.set_return_value(true);
    resp.set_ticket_size(ticket_size);
    resp.set_ticket(&ticket);

    if from_fresh {
        fresh_eticket()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&app_id);
    }

    log::debug!(
        "[InteropIPC] GetEncryptedAppTicket: AppId={} source={} size={}",
        app_id,
        if from_fresh { "fresh" } else { "store" },
        ticket_size
    );
    true
}

/// Forges an App Ownership Ticket using the off-by-four RSA-1024 splice exploit (BST-49).
///
/// Follows UsefulSteamTools AppTicket.cpp layout:
/// - Physical buffer has `S + 4` bytes (156 bytes)
/// - `total_size` is `S` (152 bytes)
/// - `app_id_offset` is `total_size - 128` (24), pointing directly at the 4 spliced AppID bytes
/// - `signature_offset` is `app_id_offset + 4` (28), pointing at the shifted 128-byte RSA signature block
/// - `steam_id_offset` is 8
/// - `signature_size` is 128
pub fn forge_ownership_ticket(app_id: u32, steam_id: u64) -> AppOwnershipTicket {
    let forged = crate::steam_bridge::credentials::appticket::forge_local_app_ownership_ticket_with_steam_id(
        app_id,
        Some(steam_id),
        None,
    ).unwrap_or_else(|| {
        let sid = steam_id;
        let signed_size = 24usize;
        let total_physical = signed_size + 4 + crate::steam_bridge::credentials::appticket::K_APP_TICKET_SIGNATURE_SIZE; // 156 bytes
        let mut data = vec![0u8; total_physical];
        data[0..4].copy_from_slice(&(signed_size as u32).to_le_bytes()); // signed payload size
        data[4..8].copy_from_slice(&4u32.to_le_bytes()); // version 4
        data[8..16].copy_from_slice(&sid.to_le_bytes());
        data[16..20].copy_from_slice(&crate::steam_bridge::credentials::appticket::K_LOCAL_APP_TICKET_SOURCE_APP_ID.to_le_bytes()); // donor AppId 7
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        data[20..24].copy_from_slice(&now.to_le_bytes());
        data[24..28].copy_from_slice(&app_id.to_le_bytes()); // spliced target AppId
        for i in 0..crate::steam_bridge::credentials::appticket::K_APP_TICKET_SIGNATURE_SIZE {
            data[28 + i] = ((i * 37 + 13) & 0xFF) as u8;
        }
        data
    });

    let total_size = forged.len().saturating_sub(4);
    let app_id_offset = total_size.saturating_sub(crate::steam_bridge::credentials::appticket::K_APP_TICKET_SIGNATURE_SIZE) as i32;
    let signature_offset = app_id_offset + 4;
    let steam_id_offset = crate::steam_bridge::credentials::appticket::K_APP_TICKET_STEAM_ID_OFFSET as i32;
    let signature_size = crate::steam_bridge::credentials::appticket::K_APP_TICKET_SIGNATURE_SIZE as i32;

    AppOwnershipTicket {
        data: forged,
        total_size,
        app_id_offset,
        steam_id_offset,
        signature_offset,
        signature_size,
    }
}

fn get_app_ownership_ticket_with_source(
    app_id: u32,
    source: AppTicketSource,
) -> Option<AppOwnershipTicket> {
    if app_id == 0 {
        return None;
    }

    let details = crate::steam_bridge::credentials::appticket::get_app_ownership_ticket_details(
        app_id, source,
    )?;
    Some(AppOwnershipTicket {
        data: details.data,
        total_size: details.total_size,
        app_id_offset: details.app_id_offset as i32,
        steam_id_offset: details.steam_id_offset as i32,
        signature_offset: details.signature_offset as i32,
        signature_size: details.signature_size as i32,
    })
}

fn get_encrypted_ticket_from_store(app_id: u32) -> Vec<u8> {
    let mut store = TicketStore::global()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    store.get_encrypted_ticket(app_id).unwrap_or_default()
}

fn record_encrypted_ticket(async_call: u64, app_id: u32) {
    let mut store = TicketStore::global()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    store.record_async_call(async_call, app_id);
}

pub fn compute_func_hash(interface: &str, method: &str) -> u32 {
    fnv1a_hash(&format!("{}::{}", interface, method))
}

struct GetSteamIDResp<'a> {
    data: &'a mut [u8],
}

impl<'a> GetSteamIDResp<'a> {
    fn new(data: &'a mut [u8]) -> Self {
        Self { data }
    }

    fn ok(&self) -> bool {
        self.data.len() >= 8
    }

    fn set_return_value(&mut self, val: u64) {
        if self.data.len() >= 8 {
            self.data[0..8].copy_from_slice(&val.to_le_bytes());
        }
    }
}

struct GetAppOwnershipTicketExtendedDataReq<'a> {
    data: &'a [u8],
}

impl<'a> GetAppOwnershipTicketExtendedDataReq<'a> {
    fn parse(data: &'a [u8]) -> Self {
        Self { data }
    }

    fn ok(&self) -> bool {
        self.data.len() >= 8
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

struct GetAppOwnershipTicketExtendedDataResp<'a> {
    data: &'a mut [u8],
}

impl<'a> GetAppOwnershipTicketExtendedDataResp<'a> {
    fn new(data: &'a mut [u8]) -> Self {
        Self { data }
    }

    fn ok(&self) -> bool {
        self.data.len() >= 20
    }

    fn set_return_value(&mut self, val: u32) {
        if self.data.len() >= 4 {
            self.data[0..4].copy_from_slice(&val.to_le_bytes());
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

    fn set_ticket(&mut self, ticket: &[u8]) {
        if let Some(end) = 20usize.checked_add(ticket.len()) {
            if self.data.len() >= end {
                self.data[20..end].copy_from_slice(ticket);
            }
        }
    }
}

struct RequestEncryptedAppTicketReq<'a> {
    data: &'a [u8],
}

impl<'a> RequestEncryptedAppTicketReq<'a> {
    fn parse(data: &'a [u8]) -> Self {
        Self { data }
    }

    fn ok(&self) -> bool {
        self.data.len() >= 4
    }

    fn data(&self) -> &[u8] {
        if self.data.len() >= 4 {
            let len = u32::from_le_bytes([self.data[0], self.data[1], self.data[2], self.data[3]])
                as usize;
            if let Some(end) = 4usize.checked_add(len) {
                if self.data.len() >= end {
                    return &self.data[4..end];
                }
            }
        }
        &[]
    }
}

struct RequestEncryptedAppTicketResp<'a> {
    data: &'a [u8],
}

impl<'a> RequestEncryptedAppTicketResp<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    fn ok(&self) -> bool {
        self.data.len() >= 8
    }

    fn return_value(&self) -> u64 {
        if self.data.len() >= 8 {
            u64::from_le_bytes([
                self.data[0],
                self.data[1],
                self.data[2],
                self.data[3],
                self.data[4],
                self.data[5],
                self.data[6],
                self.data[7],
            ])
        } else {
            0
        }
    }
}

struct GetEncryptedAppTicketResp<'a> {
    data: &'a mut [u8],
}

impl<'a> GetEncryptedAppTicketResp<'a> {
    fn new(data: &'a mut [u8]) -> Self {
        Self { data }
    }

    fn ok(&self) -> bool {
        self.data.len() >= 5
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
        if let Some(end) = 5usize.checked_add(ticket.len()) {
            if self.data.len() >= end {
                self.data[5..end].copy_from_slice(ticket);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steam_bridge::interop::interop_misc::set_online_fix_real_app_id;

    static USER_TEST_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn test_compute_func_hash() {
        let hash1 = compute_func_hash("IClientUser", "GetSteamID");
        let hash2 = compute_func_hash("IClientUser", "GetSteamID");
        assert_eq!(hash1, hash2);

        let hash3 = compute_func_hash("IClientUser", "GetAppID");
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_get_steam_id_resp() {
        let mut buf = vec![0u8; 8];
        {
            let mut resp = GetSteamIDResp::new(&mut buf);
            assert!(resp.ok());
            resp.set_return_value(0x1234_5678_9ABC_DEF0);
        }
        assert_eq!(&buf[0..8], &0x1234_5678_9ABC_DEF0_u64.to_le_bytes());
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
    fn test_ownership_ticket_response_checks_write_buffer_capacity() {
        let forged = forge_ownership_ticket(1245620, 76_561_197_960_287_930u64);
        assert_eq!(forged.total_size, 152);
        assert_eq!(forged.app_id_offset, 24);
        assert_eq!(forged.steam_id_offset, 8);

        // Buffer has room for header (20) but NOT for 20 + 152 ticket bytes -> must fail without corrupting header
        let mut small = vec![0u8; 32];
        assert!(!write_ownership_ticket_response(&forged, 1024, &mut small));
        assert_eq!(small, vec![0u8; 32]);

        let mut full = vec![0u8; 256];
        assert!(write_ownership_ticket_response(&forged, 1024, &mut full));
        assert_eq!(&full[0..4], &152u32.to_le_bytes());
        assert_eq!(&full[4..8], &24i32.to_le_bytes());
        assert_eq!(&full[8..12], &8i32.to_le_bytes());
        assert_eq!(&full[20..20 + forged.data.len()], forged.data.as_slice());
    }

    #[test]
    fn test_request_encrypted_app_ticket_req_and_overflow_safety() {
        let data = [4u32.to_le_bytes(), [0xDE, 0xAD, 0xBE, 0xEF]].concat();
        let req = RequestEncryptedAppTicketReq::parse(&data);
        assert!(req.ok());
        assert_eq!(req.data(), &[0xDE, 0xAD, 0xBE, 0xEF]);

        let max_bytes = u32::MAX.to_le_bytes();
        let overflow_req = RequestEncryptedAppTicketReq::parse(&max_bytes);
        assert!(overflow_req.ok());
        assert!(overflow_req.data().is_empty());
    }

    #[test]
    fn test_get_encrypted_app_ticket_preserves_fresh_ticket_on_small_buffer() {
        let _g = USER_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let app_id = 777_123;
        set_online_fix_real_app_id(app_id);
        fresh_eticket()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(app_id, vec![0xDE, 0xAD, 0xBE, 0xEF]);

        // Too small for 5 + 4 = 9 bytes -> must fail AND keep ticket in cache
        let mut tiny = vec![0u8; 6];
        let mut empty_read = [];
        assert!(!handler_post_get_encrypted_app_ticket(
            std::ptr::null_mut(),
            &mut empty_read,
            &mut tiny
        ));
        assert!(fresh_eticket()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&app_id));

        // Proper buffer succeeds and consumes the cached fresh ticket
        let mut ok_buf = vec![0u8; 16];
        assert!(handler_post_get_encrypted_app_ticket(
            std::ptr::null_mut(),
            &mut empty_read,
            &mut ok_buf
        ));
        assert_eq!(ok_buf[0], 1);
        assert_eq!(&ok_buf[1..5], &4u32.to_le_bytes());
        assert_eq!(&ok_buf[5..9], &[0xDE, 0xAD, 0xBE, 0xEF]);
        assert!(!fresh_eticket()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&app_id));

        set_online_fix_real_app_id(0);
    }

    #[test]
    fn test_bst52_per_pipe_appid_and_denuvo_auth_isolation() {
        let _g = USER_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        use crate::steam_bridge::interop::interop_ipc::{
            clear_pipe_app_ids, get_pipe_app_id, get_pipe_handle, get_pipe_pid,
            register_pipe_app_id, register_pipe_handle, register_pipe_pid,
        };
        use crate::steam_bridge::interop::interop_ipc_isteamutils::handle_post_get_app_id;

        clear_pipe_app_ids();

        let pipe_a = 0xAA00_1100 as *mut c_void;
        let pipe_b = 0xBB00_2200 as *mut c_void;
        let app_a = 1245620u32;
        let app_b = 730u32;

        register_pipe_app_id(pipe_a, app_a);
        register_pipe_handle(pipe_a, 101);
        register_pipe_pid(pipe_a, 4001);

        register_pipe_app_id(pipe_b, app_b);
        register_pipe_handle(pipe_b, 102);
        register_pipe_pid(pipe_b, 4002);

        // 1. Verify get_pipe_* resolution
        assert_eq!(get_pipe_app_id(pipe_a), app_a);
        assert_eq!(get_pipe_handle(pipe_a), 101);
        assert_eq!(get_pipe_pid(pipe_a), 4001);

        assert_eq!(get_pipe_app_id(pipe_b), app_b);
        assert_eq!(get_pipe_handle(pipe_b), 102);
        assert_eq!(get_pipe_pid(pipe_b), 4002);

        // 2. Verify handle_post_get_app_id distinguishes between pipe_a and pipe_b
        let mut write_a = vec![0u8; 4];
        assert!(handle_post_get_app_id(pipe_a, &mut [], &mut write_a));
        assert_eq!(u32::from_le_bytes([write_a[0], write_a[1], write_a[2], write_a[3]]), app_a);

        let mut write_b = vec![0u8; 4];
        assert!(handle_post_get_app_id(pipe_b, &mut [], &mut write_b));
        assert_eq!(u32::from_le_bytes([write_b[0], write_b[1], write_b[2], write_b[3]]), app_b);

        // 3. Verify handler_post_get_encrypted_app_ticket retrieves distinct per-pipe cached tickets
        fresh_eticket()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(app_a, vec![0x11, 0x22]);
        fresh_eticket()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(app_b, vec![0x33, 0x44, 0x55]);

        let mut out_a = vec![0u8; 16];
        assert!(handler_post_get_encrypted_app_ticket(pipe_a, &mut [], &mut out_a));
        assert_eq!(out_a[0], 1);
        assert_eq!(&out_a[1..5], &2u32.to_le_bytes());
        assert_eq!(&out_a[5..7], &[0x11, 0x22]);

        let mut out_b = vec![0u8; 16];
        assert!(handler_post_get_encrypted_app_ticket(pipe_b, &mut [], &mut out_b));
        assert_eq!(out_b[0], 1);
        assert_eq!(&out_b[1..5], &3u32.to_le_bytes());
        assert_eq!(&out_b[5..8], &[0x33, 0x44, 0x55]);

        // 4. Verify handler_post_get_app_ownership_ticket uses pipe app_id when req.app_id() == 0 or ONLINE_FIX_APP_ID
        let req_bytes = [0u32.to_le_bytes(), 1024i32.to_le_bytes()].concat();
        let mut ticket_out = vec![0u8; 256];
        // With pipe_a (app_a = 1245620), forge returns valid ticket
        assert!(handler_post_get_app_ownership_ticket(pipe_a, &mut req_bytes.clone(), &mut ticket_out));
        let ret_len = u32::from_le_bytes([ticket_out[0], ticket_out[1], ticket_out[2], ticket_out[3]]);
        assert!(ret_len > 0);

        clear_pipe_app_ids();
    }
}
