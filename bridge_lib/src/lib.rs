//! Steam client integration shared library (`libsteam_integration.so`).
//!
//! Loaded into the Steam client process via `LD_PRELOAD`. Its constructor first verifies
//! that the host process is actually the main Steam client (`steam`, excluding
//! `steamwebhelper`, shells, wrappers, and helper tools), installs a diagnostic panic hook
//! with `catch_unwind` containment so no fault can ever `SIGABRT` Steam, starts the Unix
//! domain socket IPC server, loads persisted LuaTools overrides & `.lua` manifests,
//! resolves ASLR runtime addresses in `steamclient.so` via `/proc/self/maps`, and
//! installs 64-bit indirect memory patches with safe no-op fallback when patterns absent.

#![allow(clippy::missing_safety_doc)]

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::raw::c_void;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Once, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ctor::ctor;
use luatools_linux::parsers::lua_parser::LuaFileParser;
use luatools_linux::services::manifest_binding_service::ManifestPin;
use luatools_linux::services::steam_bridge_service::BridgeOverrides;
use luatools_linux::steam_bridge::{
    binary_reader::{
        find_loaded_module_mapping, scan_steam_binaries, BinaryFile, LoadedModuleMapping,
    },
    ipc::{self, IpcServer},
    memory_patch::{self, MemoryPatch, JMP_ABS64_LEN},
    pattern_db::{BinaryComponent, PatternChannel, PatternDb},
    preload_loader::is_steam_client_process,
    ticket_store::{AppTicketData, TicketStore},
    interop::{
        interop_ipc_install,
        interop_ipc_uninstall,
        register_handler,
        EIPCCommand,
        EIPCInterface,
        IpcHandlerEntry,
        AppOwnershipTicket,
        HANDLER_REGISTRY,
    },
    interop::interop_ipc_isteamuser::{
        install_iclientuser_handlers,
        ONLINE_FIX_APP_ID,
        handler_post_get_steam_id,
        handler_post_get_app_ownership_ticket,
        handler_post_request_encrypted_app_ticket,
        handler_post_get_encrypted_app_ticket,
    },
    interop::interop_ipc_isteamutils::{
        install_iclientutils_handlers,
        handler_post_get_app_id,
        handler_post_get_api_call_result,
    },
    interop::interop_misc::{
        interop_misc_install,
        resolve_app_id,
        is_online_fix_active,
        notify_networking_sockets_used,
        should_report_online_fix_app_id,
        set_suppress_app_id_flip,
        get_app_id_for_current_pipe,
        get_game_name_by_app_id,
        ensure_buffer_capacity,
        register_ipc_handlers,
    },
    netpacket::{
        userstats::{install_userstats_hooks, uninstall_userstats_hooks, is_userstats_hook_active},
        netpacket_eticket::{install_eticket_hooks, uninstall_eticket_hooks, is_eticket_hook_active},
        netpacket_ownershipticket::{install_ownershipticket_hooks, uninstall_ownershipticket_hooks, is_ownershipticket_hook_active},
        netpacket_manifestprobe::{install_manifestprobe_hooks, uninstall_manifestprobe_hooks, is_manifestprobe_hook_active},
    },
    credentials::{
        read_app_ticket,
        write_app_ticket,
        get_app_ticket,
        write_e_ticket,
        get_e_ticket,
        AppTicketData,
    },
};

const MAX_TICKET_BUFFER_BYTES: usize = 64 * 1024;
const PANIC_LOG_TMP_PATH: &str = "/tmp/steam_integration_panic.log";