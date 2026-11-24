//! Interop IPC - IClientUser Interface Handlers.
//!
//! Linux port of usefulsteamtools `Interop_IPC_ISteamUser.cpp`.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Mutex, OnceLock};

use crate::steam_bridge::credentials::appticket::AppTicketSource;
use crate::steam_bridge::credentials::eticketclient::EticketClient;
use crate::steam_bridge::interop::interop_ipc::register_handler;
use crate::steam_bridge::interop::interop_misc::{is_online_fix_active, resolve_app_id};
use crate::steam_bridge::interop::ipc::fnv1a_hash;
use crate::steam_bridge::ticket_store::TicketStore;

static FRESH_ETICKET: OnceLock<Mutex<HashMap<u32, Vec<u8>>>> = OnceLock::new();

/// Online fix app ID (Spacewar).
pub const ONLINE_FIX_APP_ID: u32 = 480;

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

/// Post-handler: `IClientUser::GetSteamID`.
pub fn handler_post_get_steam_id(_pipe: *mut c_void, _read: &mut [u8], write: &mut [u8]) -> bool {
    let app_id = resolve_app_id();
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

/// Post-handler: `IClientUser::GetAppOwnershipTicketExtendedData`.
pub fn handler_post_get_app_ownership_ticket(
    _pipe: *mut c_void,
    read: &mut [u8],
    write: &mut [u8],
) -> bool {
    let req = GetAppOwnershipTicketExtendedDataReq::parse(read);
    if !req.ok() || req.max_ticket_size() < 0 {
        return false;
    }

    let app_id = if req.app_id() == ONLINE_FIX_APP_ID {
        resolve_app_id()
    } else {
        req.app_id()
    };

    let ticket_source = if is_online_fix_active() {
        AppTicketSource::CredentialStoreOnly
    } else {
        AppTicketSource::CredentialStoreThenForge
    };

    let Some(ticket) = get_app_ownership_ticket_with_source(app_id, ticket_source) else {
        return false;
    };

    if ticket.data.len() > req.max_ticket_size() as usize {
        log::warn!("[InteropIPC] Ticket too large for buffer");
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
        "[InteropIPC] GetAppOwnershipTicketExtendedData: AppId={} size={}",
        app_id,
        ticket.data.len()
    );
    true
}

/// Post-handler: `IClientUser::RequestEncryptedAppTicket`.
pub fn handler_post_request_encrypted_app_ticket(
    _pipe: *mut c_void,
    read: &mut [u8],
    write: &mut [u8],
) -> bool {
    let resp = RequestEncryptedAppTicketResp::new(write);
    if !resp.ok() {
        return false;
    }

    let app_id = resolve_app_id();
    {
        let req = RequestEncryptedAppTicketReq::parse(read);
        let nonce = if req.ok() { req.data() } else { &[] };
        let existing_steam_id = {
            let mut store = TicketStore::global()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            store.get_spoof_steam_id(app_id)
        };
        if existing_steam_id != 0 {
            if let Some(fresh) =
                EticketClient::fetch_fresh_eticket(app_id, nonce, existing_steam_id)
            {
                fresh_eticket()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(app_id, fresh);
            }
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

/// Post-handler: `IClientUser::GetEncryptedAppTicket`.
pub fn handler_post_get_encrypted_app_ticket(
    _pipe: *mut c_void,
    _read: &mut [u8],
    write: &mut [u8],
) -> bool {
    let app_id = resolve_app_id();

    let mut ticket = fresh_eticket()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&app_id)
        .unwrap_or_default();
    let from_fresh = !ticket.is_empty();

    if ticket.is_empty() {
        ticket = get_encrypted_ticket_from_store(app_id);
    }

    if ticket.is_empty() {
        return false;
    }

    let ticket_size = ticket.len() as u32;
    if write.len() < 5 + ticket.len() {
        return false;
    }

    let mut resp = GetEncryptedAppTicketResp::new(write);
    if !resp.ok() {
        return false;
    }

    resp.set_return_value(true);
    resp.set_ticket_size(ticket_size);
    resp.set_ticket(&ticket);

    log::debug!(
        "[InteropIPC] GetEncryptedAppTicket: AppId={} source={} size={}",
        app_id,
        if from_fresh { "fresh" } else { "store" },
        ticket_size
    );
    true
}

fn get_app_ownership_ticket_with_source(
    app_id: u32,
    _source: AppTicketSource,
) -> Option<AppOwnershipTicket> {
    let mut store = TicketStore::global()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let data = store.get(app_id).ok()?;
    let raw = data
        .app_ticket
        .as_deref()
        .and_then(|s| hex::decode(s).ok())?;
    let total_size = raw.len();
    Some(AppOwnershipTicket {
        data: raw,
        total_size,
        app_id_offset: 0,
        steam_id_offset: 0,
        signature_offset: 0,
        signature_size: 0,
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
        if self.data.len() >= 20 + ticket.len() {
            self.data[20..20 + ticket.len()].copy_from_slice(ticket);
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
            if self.data.len() >= 4 + len {
                return &self.data[4..4 + len];
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
        if self.data.len() >= 5 + ticket.len() {
            self.data[5..5 + ticket.len()].copy_from_slice(ticket);
        }
    }
}

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
    fn test_request_encrypted_app_ticket_req() {
        let data = [4u32.to_le_bytes(), [0xDE, 0xAD, 0xBE, 0xEF]].concat();
        let req = RequestEncryptedAppTicketReq::parse(&data);
        assert!(req.ok());
        assert_eq!(req.data(), &[0xDE, 0xAD, 0xBE, 0xEF]);
    }
}
