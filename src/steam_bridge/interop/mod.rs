//! Interop module - Linux port of Windows Interop layer
//!
//! This module replaces Windows-specific hooking and IPC interception
//! with Linux equivalents using memory_patch.rs and pattern_db.rs.

pub mod interop_ipc;
pub mod interop_ipc_isteamuser;
pub mod interop_ipc_isteamutils;
pub mod interop_misc;
pub mod ipc;

pub use interop_ipc::{
    install as interop_ipc_install, register_handler, uninstall as interop_ipc_uninstall,
    EIPCCommand, EIPCInterface, IpcHandlerEntry,
};

pub use interop_ipc_isteamuser::{
    handler_post_get_app_ownership_ticket, handler_post_get_encrypted_app_ticket,
    handler_post_get_steam_id, handler_post_request_encrypted_app_ticket,
    install_iclientuser_handlers, ONLINE_FIX_APP_ID,
};

pub use interop_ipc_isteamutils::{
    handler_post_get_api_call_result, handler_post_get_app_id, install_iclientutils_handlers,
};

pub use interop_misc::{
    ensure_buffer_capacity, get_app_id_for_current_pipe, get_game_name_by_app_id,
    install as interop_misc_install, is_online_fix_active, notify_networking_sockets_used,
    register_ipc_handlers, resolve_app_id, set_suppress_app_id_flip,
    should_report_online_fix_app_id,
};

pub use ipc::{
    dispatch_ipc_message, find_handler, fnv1a_hash, make_handler,
    register_handler as ipc_register_handler, register_handlers, FuncHash, HandlerFn, IpcContext,
    IpcInterface, ResolvedHandler,
};
