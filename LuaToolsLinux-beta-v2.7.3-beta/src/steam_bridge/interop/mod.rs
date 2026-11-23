//! Interop hook layer for Linux.
//!
//! Ports usefulsteamtools Interop modules from Windows Detours/VEH to Linux
//! memory_patch.rs + pattern_db.rs + ipc.rs.

pub mod ipc;
pub mod misc;
pub mod steam_user;
pub mod steam_utils;

// Re-export key types
pub use ipc::{
    IpcInterface, IpcCommand, FuncHash, ResolvedHandler, HandlerFn, IpcContext,
    find_handler, register_handler, register_handlers, parse_ipc_message,
    dispatch_ipc_message, resolve_ipc_function, make_handler, fnv1a_hash,
};
pub use misc::{
    resolve_app_id, is_online_fix_active, should_report_online_fix_app_id,
    notify_networking_sockets_used, set_suppress_app_id_flip,
    ensure_buffer_capacity, get_game_name_by_app_id, install as install_misc,
    uninstall as uninstall_misc, ONLINE_FIX_APP_ID,
};
pub use steam_user::{
    get_spoof_steam_id, get_app_ownership_ticket, forge_local_app_ownership_ticket,
    get_app_ownership_ticket_from_credential_store, get_encrypted_ticket_from_credential_store,
    write_app_ownership_ticket, write_encrypted_ticket, write_steam_id_credential,
    handler_post_get_steam_id, handler_post_get_app_ownership_ticket_extended_data,
    handler_post_request_encrypted_app_ticket, handler_post_get_encrypted_app_ticket,
    fetch_fresh_eticket, record_encrypted_ticket, extract_steam_id_from_ticket_bytes,
    AppTicketSource, AppOwnershipTicket,
};
pub use steam_utils::{
    handler_post_get_app_id, handler_post_get_api_call_result,
    record_encrypted_ticket as utils_record_encrypted_ticket,
    take_encrypted_ticket, register_handlers as register_steam_utils_handlers,
    ENCRYPTED_APP_TICKET_RESPONSE_CALLBACK, K_ERESULT_OK,
};

/// Install all interop hooks
pub async fn install_all(steam_root: &std::path::Path) -> Result<(), String> {
    crate::steam_bridge::interop::steam_utils::register_handlers();
    crate::steam_bridge::interop::misc::install(steam_root).await?;
    Ok(())
}

/// Uninstall all interop hooks
pub fn uninstall_all() {
    crate::steam_bridge::interop::misc::uninstall();
}