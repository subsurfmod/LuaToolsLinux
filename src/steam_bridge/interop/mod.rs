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
    install as interop_ipc_install,
    uninstall as interop_ipc_uninstall,
    register_handler,
    EIPCCommand,
    EIPCInterface,
    IpcHandlerEntry,
};

pub use interop_ipc_isteamuser::{
    install_iclientuser_handlers,
    ONLINE_FIX_APP_ID,
    handler_post_get_steam_id,
    handler_post_get_app_ownership_ticket,
    handler_post_request_encrypted_app_ticket,
    handler_post_get_encrypted_app_ticket,
};

pub use interop_ipc_isteamutils::{
    install_iclientutils_handlers,
    handler_post_get_app_id,
    handler_post_get_api_call_result,
};

pub use interop_misc::{
    install as interop_misc_install,
    resolve_app_id,
    is_online_fix_active,
    notify_networking_sockets_used,
    should_report_online_fix_app_id,
    set_suppress_app_id_flip,
    get_app_id_for_current_pipe,
    get_game_name_by_app_id,
    ensure_buffer_capacity,
    register_ipc_handlers,
};

pub use ipc::{
    IpcInterface,
    FuncHash,
    IpcContext,
    HandlerFn,
    ResolvedHandler,
    make_handler,
    fnv1a_hash,
    dispatch_ipc_message,
    register_handler as ipc_register_handler,
    register_handlers,
    find_handler,
};