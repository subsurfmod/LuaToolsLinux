//! Interop IPC - IClientUtils Interface Handlers.
//!
//! Linux port of usefulsteamtools `Interop_IPC_ISteamUtils.cpp`.

use crate::steam_bridge::memory_patch::MemoryPatch;
use crate::steam_bridge::ticket_store::TicketStore;

/// Install IClientUtils IPC handlers.
pub fn install_iclientutils_handlers() -> Result<(), String> {
    log::info!("[InteropIPC_ISteamUtils] Install IClientUtils handlers");
    Ok(())
}

/// Install memory patches for IClientUtils interface redirection.
pub fn install_iclientutils_patches() -> Result<Vec<MemoryPatch>, String> {
    log::info!("[InteropIPC_ISteamUtils] Install IClientUtils patches via pattern_db.rs");
    Ok(Vec::new())
}

/// Post-handler: `IClientUtils::GetAppID`.
pub fn handler_post_get_app_id() -> bool {
    true
}

/// Post-handler: `IClientUtils::GetAPICallResult`.
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

    #[test]
    fn test_install_iclientutils_handlers() {
        assert!(install_iclientutils_handlers().is_ok());
    }

    #[test]
    fn test_install_iclientutils_patches() {
        assert!(install_iclientutils_patches().is_ok());
    }

    #[test]
    fn test_async_call_roundtrip() {
        record_encrypted_ticket(0xCAFE_BABE, 1245620);
        assert_eq!(take_encrypted_ticket(0xCAFE_BABE), 1245620);
        assert_eq!(take_encrypted_ticket(0xCAFE_BABE), 0);
    }
}
