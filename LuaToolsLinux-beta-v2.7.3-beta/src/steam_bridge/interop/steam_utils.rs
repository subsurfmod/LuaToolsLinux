//! Interop ISteamUtils IPC handlers for Linux.
//!
//! Port of usefulsteamtools `Interop_IPC_ISteamUtils.cpp`.
//! Handles: GetAppID, GetAPICallResult (EncryptedAppTicketResponse).
//! On Linux: integrates with InteropMisc for AppID resolution.

use std::os::raw::c_void;
use std::marker::Send;
use std::marker::Sync;

use crate::steam_bridge::interop::misc::{resolve_app_id, should_report_online_fix_app_id};

// ============================================================================
// Constants
// ============================================================================

pub const ENCRYPTED_APP_TICKET_RESPONSE_CALLBACK: u32 = 154; // k_iSteamUserCallbacks + 54
pub const K_ERESULT_OK: i32 = 1;

// ============================================================================
// IPC Post-Handlers
// ============================================================================

/// Post-handler: IClientUtils::GetAppID
/// Restores real AppID when Steam returns 480 (Spacewar) for OnlineFix games.
/// Skips spoofing once P2P networking is active (for session cert matching).
pub fn handler_post_get_app_id(
    _pipe: *mut c_void,
    read_buf: &[u8],
    write_buf: &mut [u8],
) {
    // Once P2P is up, leave 480 so the socket matches the 480 session cert
    if should_report_online_fix_app_id() {
        return;
    }

    let real_app_id = resolve_app_id();
    if real_app_id == 0 {
        return;
    }

    let mut resp = GetAppIDResp::new(write_buf);
    if !resp.is_ok() {
        return;
    }

    let current = resp.get_return_value();
    if current == real_app_id {
        return;
    }

    log::info!("GetAppID: spoof response {} -> {}", current, real_app_id);
    resp.set_return_value(real_app_id);
}

/// Post-handler: IClientUtils::GetAPICallResult
/// Handles EncryptedAppTicketResponse callback - forces k_EResultOK
pub fn handler_post_get_api_call_result(
    pipe: *mut c_void,
    read_buf: &[u8],
    write_buf: &mut [u8],
) {
    let mut req = GetAPICallResultReq::new(read_buf);
    if !req.is_ok() {
        return;
    }

    let app_id = resolve_app_id();
    log::debug!("GetAPICallResult: {:?}, AppId={}", req, app_id);

    // Check if this is an EncryptedAppTicketResponse callback
    if req.callback_expected() == ENCRYPTED_APP_TICKET_RESPONSE_CALLBACK {
        handle_encrypted_app_ticket_response(write_buf, req.async_call(), req.callback_size());
    }
}

// ============================================================================
// Callback handling
// ============================================================================

#[repr(C)]
struct EncryptedAppTicketResponse {
    result: i32, // EResult
}

impl EncryptedAppTicketResponse {
    fn new_ok() -> Self {
        Self { result: K_ERESULT_OK }
    }

    fn as_bytes(&self) -> [u8; 4] {
        self.result.to_le_bytes()
    }
}

fn handle_encrypted_app_ticket_response(
    write_buf: &mut [u8],
    async_call: u64,
    callback_capacity: u32,
) {
    // Take the app_id for this async call
    let app_id = take_encrypted_ticket(async_call);
    let Some(app_id) = app_id else {
        log::debug!("No pending eticket for async call 0x{:X}", async_call);
        return;
    };

    let callback = EncryptedAppTicketResponse::new_ok();
    let callback_bytes = callback.as_bytes();

    if (callback_capacity as usize) < callback_bytes.len() {
        // Re-record for retry
        record_encrypted_ticket(async_call, app_id);
        log::warn!("Failed to write EncryptedAppTicketResponse for AppId={} hAsyncCall=0x{:X} - buffer too small",
            app_id, async_call);
        return;
    }

    let mut resp = GetAPICallResultResp::new(write_buf, callback_capacity as usize);
    if !resp.is_ok() {
        record_encrypted_ticket(async_call, app_id);
        return;
    }

    resp.set_callback(&callback_bytes);
    resp.set_return_value(true);
    resp.set_failed(false);

    log::debug!("Set K_EResultOK for EncryptedAppTicketResponse callback, AppId={} hAsyncCall=0x{:X}",
        app_id, async_call);
}

// ============================================================================
// Helper types and functions
// ============================================================================

struct GetAppIDResp<'a> {
    buf: &'a mut [u8],
}

impl<'a> GetAppIDResp<'a> {
    fn new(buf: &'a mut [u8]) -> Self { Self { buf } }
    fn is_ok(&self) -> bool { self.buf.len() >= 4 }
    fn get_return_value(&self) -> u32 {
        u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]])
    }
    fn set_return_value(&mut self, val: u32) {
        self.buf[0..4].copy_from_slice(&val.to_le_bytes());
    }
}

struct GetAPICallResultReq<'a> {
    buf: &'a [u8],
}

impl<'a> GetAPICallResultReq<'a> {
    fn new(buf: &'a [u8]) -> Self { Self { buf } }
    fn is_ok(&self) -> bool { self.buf.len() >= 24 } // hAsyncCall(8) + iCallbackExpected(4) + cubCallback(4) + padding
    fn async_call(&self) -> u64 {
        u64::from_le_bytes([
            self.buf[0], self.buf[1], self.buf[2], self.buf[3],
            self.buf[4], self.buf[5], self.buf[6], self.buf[7],
        ])
    }
    fn callback_expected(&self) -> u32 {
        u32::from_le_bytes([self.buf[8], self.buf[9], self.buf[10], self.buf[11]])
    }
    fn callback_size(&self) -> u32 {
        u32::from_le_bytes([self.buf[12], self.buf[13], self.buf[14], self.buf[15]])
    }
}

struct GetAPICallResultResp<'a> {
    buf: &'a mut [u8],
    capacity: usize,
}

impl<'a> GetAPICallResultResp<'a> {
    fn new(buf: &'a mut [u8], capacity: usize) -> Self { Self { buf, capacity } }
    fn is_ok(&self) -> bool { self.buf.len() >= 4 }
    fn set_callback(&mut self, data: &[u8]) {
        if self.buf.len() >= 4 + data.len() {
            self.buf[4..4+data.len()].copy_from_slice(data);
        }
    }
    fn set_return_value(&mut self, val: bool) {
        self.buf[0] = val as u8;
    }
    fn set_failed(&mut self, val: bool) {
        if self.buf.len() >= 2 {
            self.buf[1] = val as u8;
        }
    }
}

// ============================================================================
// Pending async call tracking
// ============================================================================

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::fmt;

static PENDING_ETICKETS: OnceLock<Mutex<HashMap<u64, u32>>> = OnceLock::new();

fn pending_etickets() -> &'static Mutex<HashMap<u64, u32>> {
    PENDING_ETICKETS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record async call handle -> app_id mapping
pub fn record_encrypted_ticket(async_call: u64, app_id: u32) {
    pending_etickets().lock().unwrap().insert(async_call, app_id);
}

/// Take and remove async call -> app_id mapping
pub fn take_encrypted_ticket(async_call: u64) -> Option<u32> {
    pending_etickets().lock().unwrap().remove(&async_call)
}

// ============================================================================
// Module registration
// ============================================================================

/// Register IClientUtils handlers with the IPC system
pub fn register_handlers() {
    use crate::steam_bridge::interop::ipc::{register_handler, make_handler, fnv1a_hash, HandlerFn, IpcInterface};

    let handlers = vec![
        make_handler(
            IpcInterface::IClientUtils,
            fnv1a_hash("GetAppID"),
            "IClientUtils::GetAppID",
            0,
            1,
            None,
            Some(Box::new(|ctx| {
                handler_post_get_app_id(ctx.pipe, &ctx.read_buffer, &mut ctx.write_buffer);
                Ok(())
            })),
        ),
        make_handler(
            IpcInterface::IClientUtils,
            fnv1a_hash("GetAPICallResult"),
            "IClientUtils::GetAPICallResult",
            0,
            3,
            None,
            Some(Box::new(|ctx| {
                handler_post_get_api_call_result(ctx.pipe, &ctx.read_buffer, &mut ctx.write_buffer);
                Ok(())
            })),
        ),
    ];

    crate::steam_bridge::interop::ipc::register_handlers(handlers);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypted_app_ticket_response() {
        let resp = EncryptedAppTicketResponse::new_ok();
        assert_eq!(resp.result, K_ERESULT_OK);
        let bytes = resp.as_bytes();
        assert_eq!(bytes.len(), 4);
    }

    #[test]
    fn test_pending_etickets() {
        record_encrypted_ticket(0x12345678, 12345);
        assert_eq!(take_encrypted_ticket(0x12345678), Some(12345));
        assert_eq!(take_encrypted_ticket(0x12345678), None); // Already taken
    }
}