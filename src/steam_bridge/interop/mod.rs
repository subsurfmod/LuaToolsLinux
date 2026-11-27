//! Interop module - Linux port of Windows Interop layer
//!
//! This module replaces Windows-specific hooking and IPC interception
//! with Linux equivalents using memory_patch.rs and pattern_db.rs.

pub mod bridge_lib;
pub mod interop_callbacks;
pub mod interop_decryption;
pub mod interop_ipc;
pub mod interop_ipc_isteamuser;
pub mod interop_ipc_isteamutils;
pub mod interop_keyvalues;
pub mod interop_misc;
pub mod interop_package;
pub mod interop_steamui;
pub mod ipc;

pub use interop_ipc::{
    install as interop_ipc_install, register_handler, uninstall as interop_ipc_uninstall,
    get_pipe_app_id, register_pipe_app_id, clear_pipe_app_ids,
    EIPCCommand, EIPCInterface, IpcHandlerEntry,
};

pub use interop_ipc_isteamuser::{
    forge_ownership_ticket, handler_post_get_app_ownership_ticket, handler_post_get_encrypted_app_ticket,
    handler_post_get_steam_id, handler_post_request_encrypted_app_ticket,
    install_iclientuser_handlers,
};

pub use interop_ipc_isteamutils::{
    handler_post_get_api_call_result, handler_post_get_app_id, install_iclientutils_handlers,
};

pub use interop_misc::{
    ensure_buffer_capacity, get_app_id_for_current_pipe, get_game_name_by_app_id,
    install as interop_misc_install, is_online_fix_active, is_lua_unlocked, notify_networking_sockets_used,
    register_ipc_handlers, resolve_app_id, set_suppress_app_id_flip,
    should_report_online_fix_app_id, ONLINE_FIX_APP_ID,
    FriendEntry, ClanData, OtherGameData, PersonaStateMsg,
};

pub use interop_package::{
    install as interop_package_install, uninstall as interop_package_uninstall,
    on_license_list, try_dump_owned_depots, get_owned_depots, has_depot,
    add_owned_depot_id,
    notify_license_changed, K_INJECTED_PACKAGE_ID, K_INJECTED_PKG_ACCESS_TOKEN,
    License, AppOwnership,
};

pub use interop_decryption::{
    has_depot_decryption_key, install as interop_decryption_install,
    uninstall as interop_decryption_uninstall,
};

pub use interop_steamui::{
    install as interop_steamui_install, uninstall as interop_steamui_uninstall,
    queue_removal, cancel_removal, active_download_count,
    set_purchase_time, get_purchase_time, clear_purchase_times,
    EAppState, EAppOwnershipFlags, EAppChangeFlags, CSteamApp, CAppOverviewChange,
};

pub use ipc::{
    dispatch_ipc_message, find_handler, fnv1a_hash, make_handler,
    register_handler as ipc_register_handler, register_handlers, FuncHash, HandlerFn, IpcContext,
    IpcInterface, ResolvedHandler,
};
