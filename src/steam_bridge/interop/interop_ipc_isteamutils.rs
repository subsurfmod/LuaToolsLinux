//! Interop IPC - IClientUtils Interface Handlers.
//!
//! Linux port of usefulsteamtools `Interop_IPC_ISteamUtils.cpp`.

use std::ffi::c_void;
use std::sync::Arc;

use crate::steam_bridge::interop::interop_ipc::register_handler;
use crate::steam_bridge::interop::interop_misc;
use crate::steam_bridge::memory_patch::MemoryPatch;
use crate::steam_bridge::ticket_store::TicketStore;

const ENCRYPTED_APP_TICKET_RESPONSE_CALLBACK: u32 = 154;

/// Install IClientUtils IPC handlers into `interop_ipc`.
pub fn install_iclientutils_handlers() -> Result<(), String> {
    register_handler(
        "IClientUtils",
        "GetAppID",
        None,
        Some(Arc::new(handle_post_get_app_id)),
    )?;
    register_handler(
        "IClientUtils",
        "GetAPICallResult",
        None,
        Some(Arc::new(handle_post_get_api_call_result)),
    )?;
    log::info!("[InteropIPC_ISteamUtils] Installed 2 IClientUtils handlers");
    Ok(())
}

/// Install memory patches for IClientUtils interface redirection.
pub fn install_iclientutils_patches() -> Result<Vec<MemoryPatch>, String> {
    log::info!("[InteropIPC_ISteamUtils] Install IClientUtils patches via pattern_db.rs");
    Ok(Vec::new())
}

/// Buffer-aware post-handler for `IClientUtils::GetAppID` with per-pipe isolation (BST-52).
pub fn handle_post_get_app_id(pipe: *mut c_void, _read: &mut [u8], write: &mut [u8]) -> bool {
    if write.len() < 4 {
        return false;
    }
    if interop_misc::should_report_online_fix_app_id() {
        let current = u32::from_le_bytes([write[0], write[1], write[2], write[3]]);
        if current == interop_misc::ONLINE_FIX_APP_ID {
            return false;
        }
        write[0..4].copy_from_slice(&interop_misc::ONLINE_FIX_APP_ID.to_le_bytes());
        return true;
    }
    let real_app_id = {
        let from_pipe = crate::steam_bridge::interop::interop_ipc::get_pipe_app_id(pipe);
        if from_pipe != 0 {
            from_pipe
        } else {
            interop_misc::resolve_app_id()
        }
    };
    if real_app_id == 0 {
        return false;
    }
    let current = u32::from_le_bytes([write[0], write[1], write[2], write[3]]);
    if current == real_app_id {
        return false;
    }
    write[0..4].copy_from_slice(&real_app_id.to_le_bytes());
    true
}

/// Buffer-aware post-handler for `IClientUtils::GetAPICallResult`.
pub fn handle_post_get_api_call_result(
    _pipe: *mut c_void,
    read: &mut [u8],
    write: &mut [u8],
) -> bool {
    if read.len() < 12 || write.len() < 4 {
        return false;
    }
    let callback_id = u32::from_le_bytes([read[0], read[1], read[2], read[3]]);
    let async_call = u64::from_le_bytes([
        read[4], read[5], read[6], read[7], read[8], read[9], read[10], read[11],
    ]);
    if callback_id == ENCRYPTED_APP_TICKET_RESPONSE_CALLBACK {
        let app_id = take_encrypted_ticket(async_call);
        if app_id == 0 {
            return false;
        }
        // EResult::OK = 1
        write[0..4].copy_from_slice(&1u32.to_le_bytes());
        return true;
    }
    false
}

/// Post-handler compatibility stub: `IClientUtils::GetAppID`.
pub fn handler_post_get_app_id() -> bool {
    true
}

/// Post-handler compatibility stub: `IClientUtils::GetAPICallResult`.
pub fn handler_post_get_api_call_result() -> bool {
    true
}

/// Record async call handle for encrypted ticket tracking.
pub fn record_encrypted_ticket(async_call: u64, app_id: u32) {
    let mut store = TicketStore::global()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    store.record_async_call(async_call, app_id);
}

/// Consume async call handle for encrypted ticket tracking.
pub fn take_encrypted_ticket(async_call: u64) -> u32 {
    let mut store = TicketStore::global()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    store.take_async_call(async_call).unwrap_or(0)
}

/// Get encrypted ticket bytes from credential store.
pub fn get_encrypted_ticket_from_store(app_id: u32) -> Vec<u8> {
    let mut store = TicketStore::global()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    store.get_encrypted_ticket(app_id).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static UTILS_TEST_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn test_install_iclientutils_handlers() {
        assert!(install_iclientutils_handlers().is_ok());
    }

    #[test]
    fn test_install_iclientutils_patches() {
        assert!(install_iclientutils_patches().is_ok());
    }

    #[test]
    fn test_async_call_roundtrip_and_api_call_result_handler() {
        let handle = 0xCAFE_BABE_0001u64;
        record_encrypted_ticket(handle, 1245620);

        let mut read = Vec::new();
        read.extend_from_slice(&ENCRYPTED_APP_TICKET_RESPONSE_CALLBACK.to_le_bytes());
        read.extend_from_slice(&handle.to_le_bytes());
        let mut write = vec![0u8; 4];

        assert!(handle_post_get_api_call_result(
            std::ptr::null_mut(),
            &mut read,
            &mut write
        ));
        assert_eq!(
            u32::from_le_bytes([write[0], write[1], write[2], write[3]]),
            1
        );
        // Handle was consumed — second call must return false
        assert!(!handle_post_get_api_call_result(
            std::ptr::null_mut(),
            &mut read,
            &mut write
        ));
        assert_eq!(take_encrypted_ticket(handle), 0);
    }

    #[test]
    fn test_handle_post_get_app_id_switches_between_real_and_online_fix() {
        let _g = UTILS_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        interop_misc::set_suppress_app_id_flip(false);
        interop_misc::set_online_fix_real_app_id(1245620);

        let mut empty_read = [];
        let mut write = 480u32.to_le_bytes();
        assert!(handle_post_get_app_id(
            std::ptr::null_mut(),
            &mut empty_read,
            &mut write
        ));
        assert_eq!(u32::from_le_bytes(write), 1245620);

        // Once networking sockets P2P is active, GetAppID flips back to 480 for cert match
        interop_misc::notify_networking_sockets_used();
        assert!(handle_post_get_app_id(
            std::ptr::null_mut(),
            &mut empty_read,
            &mut write
        ));
        assert_eq!(u32::from_le_bytes(write), interop_misc::ONLINE_FIX_APP_ID);

        interop_misc::set_online_fix_real_app_id(0);
    }
}
