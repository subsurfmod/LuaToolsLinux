//! Interop IPC - Linux port of Windows `Interop_IPC`.
//!
//! Uses `memory_patch.rs` for 64-bit indirect jump hooks and `pattern_db.rs` for
//! pattern-based symbol resolution in `steamclient.so`.

use std::ffi::c_void;
use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use crate::steam_bridge::binary_reader::{find_loaded_module_mapping, BinaryFile};
use crate::steam_bridge::interop::interop_ipc_isteamuser::install_iclientuser_handlers;
use crate::steam_bridge::interop::interop_ipc_isteamutils::install_iclientutils_handlers;
use crate::steam_bridge::interop::interop_misc;
use crate::steam_bridge::interop::ipc::fnv1a_hash;
use crate::steam_bridge::ipc::{IpcMessage, IpcServer};
use crate::steam_bridge::manifest_hooks::{self, parse_pattern_and_mask, ManifestOverride};
use crate::steam_bridge::memory_patch::{MemoryPatch, JMP_ABS64_LEN};
use crate::steam_bridge::pattern_db::{BinaryComponent, PatternChannel, PatternDb};
use crate::steam_bridge::ticket_store::TicketStore;

/// IPC command types matching Steam's internal IPC protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum EIPCCommand {
    InterfaceCall = 1,
    Handshake = 2,
    Callback = 3,
}

impl EIPCCommand {
    fn from_u32(v: u32) -> Option<Self> {
        match v {
            1 => Some(EIPCCommand::InterfaceCall),
            2 => Some(EIPCCommand::Handshake),
            3 => Some(EIPCCommand::Callback),
            _ => None,
        }
    }
}

/// IPC interface IDs (matching Windows `EIPCInterface`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum EIPCInterface {
    IClientUser = 1,
    IClientUtils = 2,
    IClientNetworkingSocketsSerialized = 46,
}

impl EIPCInterface {
    fn from_u32(v: u32) -> Option<Self> {
        match v {
            1 => Some(EIPCInterface::IClientUser),
            2 => Some(EIPCInterface::IClientUtils),
            46 => Some(EIPCInterface::IClientNetworkingSocketsSerialized),
            _ => None,
        }
    }
}

/// Function signature for IPC handlers (pre and post).
pub type IpcHandlerFn = Arc<dyn Fn(*mut c_void, &mut [u8], &mut [u8]) -> bool + Send + Sync>;

/// Handler entry for a specific interface method.
#[derive(Clone)]
pub struct IpcHandlerEntry {
    pub interface_id: EIPCInterface,
    pub method_name: String,
    pub func_hash: u32,
    pub fencepost: u32,
    pub argc: u32,
    pub pre_handler: Option<IpcHandlerFn>,
    pub post_handler: Option<IpcHandlerFn>,
}

impl fmt::Debug for IpcHandlerEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IpcHandlerEntry")
            .field("interface_id", &self.interface_id)
            .field("method_name", &self.method_name)
            .field("func_hash", &self.func_hash)
            .field("fencepost", &self.fencepost)
            .field("argc", &self.argc)
            .finish()
    }
}

/// Internal handler storage after registration.
#[derive(Clone)]
pub struct ResolvedHandler {
    interface_id: EIPCInterface,
    func_hash: u32,
    name: String,
    fencepost: u32,
    argc: u32,
    pre_handler: Option<IpcHandlerFn>,
    post_handler: Option<IpcHandlerFn>,
}

impl fmt::Debug for ResolvedHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedHandler")
            .field("interface_id", &self.interface_id)
            .field("func_hash", &self.func_hash)
            .field("name", &self.name)
            .field("fencepost", &self.fencepost)
            .field("argc", &self.argc)
            .finish()
    }
}

#[derive(Debug)]
struct IpcDispatch {
    pipe: *mut c_void,
    handler: Option<ResolvedHandler>,
}

impl IpcDispatch {
    fn enabled(&self) -> bool {
        !self.pipe.is_null() && self.handler.is_some()
    }
}

static HANDLER_REGISTRY: OnceLock<Mutex<Vec<ResolvedHandler>>> = OnceLock::new();
static PATTERN_DB: OnceLock<PatternDb> = OnceLock::new();
static IPC_PATCHES: OnceLock<Mutex<Vec<MemoryPatch>>> = OnceLock::new();
pub(crate) static STEAM_CLIENT_BASE: OnceLock<u64> = OnceLock::new();
static STEAM_UI_BASE: OnceLock<u64> = OnceLock::new();
static IPC_SERVER: OnceLock<Mutex<Option<IpcServer>>> = OnceLock::new();

/// Function pointer type for the original `IPCProcessMessage`.
pub type IPCProcessMessageFn = unsafe extern "C" fn(
    p_server: *mut c_void,
    h_steam_pipe: u32,
    p_read: *mut c_void,
    p_write: *mut c_void,
) -> bool;

static ORIGINAL_IPC_PROCESS_MESSAGE: OnceLock<Mutex<Option<IPCProcessMessageFn>>> = OnceLock::new();

fn handlers() -> &'static Mutex<Vec<ResolvedHandler>> {
    HANDLER_REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

fn pattern_db() -> &'static PatternDb {
    PATTERN_DB.get_or_init(PatternDb::new)
}

pub(crate) fn ipc_patches() -> &'static Mutex<Vec<MemoryPatch>> {
    IPC_PATCHES.get_or_init(|| Mutex::new(Vec::new()))
}

fn ipc_server() -> &'static Mutex<Option<IpcServer>> {
    IPC_SERVER.get_or_init(|| Mutex::new(None))
}

fn original_ipc_process_message() -> &'static Mutex<Option<IPCProcessMessageFn>> {
    ORIGINAL_IPC_PROCESS_MESSAGE.get_or_init(|| Mutex::new(None))
}

fn find_loaded_module_file_path(candidates: &[&str]) -> Option<(String, PathBuf)> {
    let loaded = crate::steam_bridge::ost_platform::dynamic_library::list_loaded_modules();
    for &name in candidates {
        if let Some(path) = loaded
            .iter()
            .find(|p| p.file_name().and_then(|n| n.to_str()) == Some(name))
        {
            return Some((name.to_string(), path.clone()));
        }
    }
    None
}

/// Initialize the Interop IPC system.
pub fn install() -> Result<(), String> {
    let _ = pattern_db();

    let (client_mod_name, client_path) =
        find_loaded_module_file_path(&["steamclient.so", "steamclient64.so"])
            .ok_or_else(|| "steamclient.so not loaded in process".to_string())?;

    let mapping = find_loaded_module_mapping(&client_mod_name)
        .ok_or_else(|| format!("{} mapping not found in /proc/self/maps", client_mod_name))?;
    let _ = STEAM_CLIENT_BASE.set(mapping.base_addr);

    if let Some((ui_name, _)) = find_loaded_module_file_path(&["steamui.so", "steamui64.so"]) {
        if let Some(ui_map) = find_loaded_module_mapping(&ui_name) {
            let _ = STEAM_UI_BASE.set(ui_map.base_addr);
        }
    }

    let binary = BinaryFile::open(&client_path)
        .map_err(|e| format!("Failed to open {}: {}", client_path.display(), e))?;

    let pattern_file = pattern_db()
        .lookup_by_binary(&binary, PatternChannel::Ipc, BinaryComponent::SteamClient)
        .map_err(|e| format!("Failed to load IPC patterns: {}", e))?;

    let ipc_process_msg_entry = pattern_file
        .patterns
        .get("IPCProcessMessage")
        .ok_or("IPCProcessMessage pattern not found")?;

    let ipc_process_msg_addr = if let Some(pattern) = &ipc_process_msg_entry.pattern {
        let (pattern_bytes, mask_bytes) =
            parse_pattern_and_mask(pattern, ipc_process_msg_entry.mask.as_deref())
                .ok_or_else(|| "Invalid IPCProcessMessage hex pattern or mask".to_string())?;
        let offset = ipc_process_msg_entry.offset.unwrap_or(0);
        let vaddr = binary
            .resolve_pattern_address(&pattern_bytes, mask_bytes.as_deref(), offset, None)
            .ok_or_else(|| "Pattern scan failed for IPCProcessMessage".to_string())?;
        binary
            .vaddr_to_runtime_addr(vaddr, &mapping, JMP_ABS64_LEN)
            .ok_or_else(|| "Resolved IPCProcessMessage address is not executable".to_string())?
    } else if let Some(vaddr) = ipc_process_msg_entry.address {
        binary
            .vaddr_to_runtime_addr(vaddr, &mapping, JMP_ABS64_LEN)
            .ok_or_else(|| "IPCProcessMessage address is not in executable memory".to_string())?
    } else if let Some(offset) = ipc_process_msg_entry.offset {
        let addr = mapping
            .base_addr
            .checked_add_signed(offset)
            .ok_or_else(|| "IPCProcessMessage offset overflow".to_string())?;
        if !mapping.contains_executable_range(addr, JMP_ABS64_LEN) {
            return Err("IPCProcessMessage offset is not in executable memory".into());
        }
        addr
    } else {
        return Err("No address, offset, or pattern for IPCProcessMessage".into());
    };

    let replacement = hook_ipc_process_message as *mut c_void as *mut u8;
    let target = ipc_process_msg_addr as *mut u8;

    let mut patch = MemoryPatch::new(target, replacement);
    unsafe { patch.install(JMP_ABS64_LEN) }?;

    if !patch.trampoline.is_null() {
        let original: IPCProcessMessageFn = unsafe { std::mem::transmute(patch.trampoline) };
        *original_ipc_process_message()
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(original);
    }

    ipc_patches()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(patch);

    register_default_handlers();
    start_ipc_server()?;

    log::info!(
        "[InteropIPC] Installed successfully with {} handlers",
        handlers().lock().unwrap_or_else(|e| e.into_inner()).len()
    );

    Ok(())
}

/// Uninstall all hooks and cleanup.
pub fn uninstall() -> Result<(), String> {
    if let Some(mut server) = ipc_server()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        server.stop();
    }

    // Clear saved trampoline before freeing trampoline pages inside `patch.uninstall()`.
    *original_ipc_process_message()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;

    let mut patches = ipc_patches().lock().unwrap_or_else(|e| e.into_inner());
    for mut patch in patches.drain(..) {
        unsafe { patch.uninstall() }?;
    }

    handlers().lock().unwrap_or_else(|e| e.into_inner()).clear();
    log::info!("[InteropIPC] Uninstalled");
    Ok(())
}

/// Register an IPC handler (replaces any existing handler for the same interface + method).
pub fn register_handler(
    interface_name: &str,
    method_name: &str,
    pre: Option<IpcHandlerFn>,
    post: Option<IpcHandlerFn>,
) -> Result<(), String> {
    let interface_id = match interface_name {
        "IClientUser" => EIPCInterface::IClientUser,
        "IClientUtils" => EIPCInterface::IClientUtils,
        "IClientNetworkingSocketsSerialized" => EIPCInterface::IClientNetworkingSocketsSerialized,
        _ => return Err(format!("Unknown interface: {}", interface_name)),
    };

    let method_key = format!("{}::{}", interface_name, method_name);
    let func_hash = compute_func_hash(interface_name, method_name);

    let handler = ResolvedHandler {
        interface_id,
        func_hash,
        name: method_key,
        fencepost: 0,
        argc: 0,
        pre_handler: pre,
        post_handler: post,
    };

    let mut list = handlers().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = list
        .iter_mut()
        .find(|h| h.interface_id == interface_id && h.func_hash == func_hash)
    {
        *existing = handler;
    } else {
        list.push(handler);
    }
    Ok(())
}

fn register_default_handlers() {
    let _ = install_iclientuser_handlers();
    let _ = install_iclientutils_handlers();
}

unsafe extern "C" fn hook_ipc_process_message(
    p_server: *mut c_void,
    h_steam_pipe: u32,
    p_read: *mut c_void,
    p_write: *mut c_void,
) -> bool {
    panic::catch_unwind(AssertUnwindSafe(|| {
        // Guard against null or aliased buffers (aliasing &mut slices would be UB).
        if p_read.is_null() || p_write.is_null() || p_read == p_write {
            return call_original(p_server, h_steam_pipe, p_read, p_write);
        }

        let mut read_buffer = IPCBuffer::from_raw(p_read);
        let mut write_buffer = IPCBuffer::from_raw(p_write);

        handle_handshake(p_server, h_steam_pipe, &read_buffer);
        detect_networking_sockets(&read_buffer);

        let dispatch = resolve_dispatch(p_server, h_steam_pipe, &read_buffer);
        if !dispatch.enabled() {
            return call_original(p_server, h_steam_pipe, p_read, p_write);
        }

        if let Some(handler) = &dispatch.handler {
            if let Some(pre) = &handler.pre_handler {
                pre(
                    dispatch.pipe,
                    read_buffer.as_mut_slice(),
                    write_buffer.as_mut_slice(),
                );
            }
        }

        let result = call_original(p_server, h_steam_pipe, p_read, p_write);

        if result {
            if let Some(handler) = &dispatch.handler {
                if let Some(post) = &handler.post_handler {
                    post(
                        dispatch.pipe,
                        read_buffer.as_mut_slice(),
                        write_buffer.as_mut_slice(),
                    );
                }
            }
        }

        result
    }))
    .unwrap_or(false)
}

/// Call the original `IPCProcessMessage` trampoline if installed.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn call_original(
    p_server: *mut c_void,
    h_steam_pipe: u32,
    p_read: *mut c_void,
    p_write: *mut c_void,
) -> bool {
    let orig = *original_ipc_process_message()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(original) = orig {
        unsafe { original(p_server, h_steam_pipe, p_read, p_write) }
    } else {
        false
    }
}

/// Compute FNV-1a hash for `interface::method`.
pub fn compute_func_hash(interface: &str, method: &str) -> u32 {
    let key = format!("{}::{}", interface, method);
    fnv1a_hash(&key)
}

fn resolve_dispatch(p_server: *mut c_void, h_steam_pipe: u32, read: &IPCBuffer) -> IpcDispatch {
    let mut dispatch = IpcDispatch {
        pipe: std::ptr::null_mut(),
        handler: None,
    };

    dispatch.pipe = get_pipe_client(p_server, h_steam_pipe);
    if dispatch.pipe.is_null() {
        return dispatch;
    }

    let request = IPCRequest::parse(read.as_slice());
    if !request.ok() || request.command() != EIPCCommand::InterfaceCall {
        return dispatch;
    }

    let app_id = interop_misc::resolve_app_id();
    if app_id == 0 {
        return dispatch;
    }

    let call = IPCInterfaceCall::parse(request.body());
    if !call.ok() {
        return dispatch;
    }

    let handlers_list = handlers().lock().unwrap_or_else(|e| e.into_inner());
    for handler in handlers_list.iter() {
        if handler.interface_id == call.interface_id() && handler.func_hash == call.func_hash() {
            dispatch.handler = Some(handler.clone());
            break;
        }
    }

    dispatch
}

fn handle_handshake(p_server: *mut c_void, h_steam_pipe: u32, read: &IPCBuffer) {
    let request = IPCRequest::parse(read.as_slice());
    if !request.ok() || request.command() != EIPCCommand::Handshake {
        return;
    }

    let handshake = IPCHandshakeReq::parse(request.body());
    if !handshake.ok() {
        return;
    }

    let pipe = get_pipe_client(p_server, h_steam_pipe);
    if pipe.is_null() {
        return;
    }

    log::debug!("[InteropIPC] Handshake from pid={}", handshake.pid());
}

fn detect_networking_sockets(read: &IPCBuffer) {
    if !interop_misc::is_online_fix_active() || interop_misc::should_report_online_fix_app_id() {
        return;
    }

    let request = IPCRequest::parse(read.as_slice());
    if !request.ok() || request.command() != EIPCCommand::InterfaceCall {
        return;
    }

    let call = IPCInterfaceCall::parse(request.body());
    if !call.ok() {
        return;
    }

    if call.interface_id() == EIPCInterface::IClientNetworkingSocketsSerialized {
        interop_misc::notify_networking_sockets_used();
    }
}

struct IPCBuffer {
    ptr: *mut c_void,
    size: usize,
}

impl IPCBuffer {
    fn from_raw(ptr: *mut c_void) -> Self {
        let size = if ptr.is_null() { 0 } else { 4096 };
        Self { ptr, size }
    }

    fn as_slice(&self) -> &[u8] {
        if self.ptr.is_null() || self.size == 0 {
            return &[];
        }
        unsafe { std::slice::from_raw_parts(self.ptr as *const u8, self.size) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        if self.ptr.is_null() || self.size == 0 {
            return &mut [];
        }
        unsafe { std::slice::from_raw_parts_mut(self.ptr as *mut u8, self.size) }
    }
}

struct IPCRequest {
    cmd: EIPCCommand,
    data: Vec<u8>,
    valid: bool,
}

impl IPCRequest {
    fn parse(data: &[u8]) -> Self {
        if data.len() < 8 {
            return Self {
                cmd: EIPCCommand::InterfaceCall,
                data: Vec::new(),
                valid: false,
            };
        }

        let cmd_raw = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let Some(cmd) = EIPCCommand::from_u32(cmd_raw) else {
            return Self {
                cmd: EIPCCommand::InterfaceCall,
                data: Vec::new(),
                valid: false,
            };
        };
        let body_len = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let Some(end) = 8usize.checked_add(body_len) else {
            return Self {
                cmd,
                data: Vec::new(),
                valid: false,
            };
        };
        if data.len() < end {
            return Self {
                cmd,
                data: Vec::new(),
                valid: false,
            };
        }

        Self {
            cmd,
            data: data[8..end].to_vec(),
            valid: true,
        }
    }

    fn ok(&self) -> bool {
        self.valid
    }

    fn command(&self) -> EIPCCommand {
        self.cmd
    }

    fn body(&self) -> &[u8] {
        &self.data
    }
}

struct IPCHandshakeReq {
    data: Vec<u8>,
    valid: bool,
}

impl IPCHandshakeReq {
    fn parse(data: &[u8]) -> Self {
        Self {
            data: data.to_vec(),
            valid: data.len() >= 4,
        }
    }

    fn ok(&self) -> bool {
        self.valid
    }

    fn pid(&self) -> u32 {
        if self.data.len() >= 4 {
            u32::from_le_bytes([self.data[0], self.data[1], self.data[2], self.data[3]])
        } else {
            0
        }
    }
}

struct IPCInterfaceCall {
    iface: EIPCInterface,
    func_hash: u32,
    valid: bool,
}

impl IPCInterfaceCall {
    fn parse(data: &[u8]) -> Self {
        if data.len() < 12 {
            return Self {
                iface: EIPCInterface::IClientUser,
                func_hash: 0,
                valid: false,
            };
        }
        let iface_raw = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let Some(iface) = EIPCInterface::from_u32(iface_raw) else {
            return Self {
                iface: EIPCInterface::IClientUser,
                func_hash: 0,
                valid: false,
            };
        };
        let func_hash = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        Self {
            iface,
            func_hash,
            valid: true,
        }
    }

    fn ok(&self) -> bool {
        self.valid
    }

    fn interface_id(&self) -> EIPCInterface {
        self.iface
    }

    fn func_hash(&self) -> u32 {
        self.func_hash
    }
}

fn get_pipe_client(_p_server: *mut c_void, _h_steam_pipe: u32) -> *mut c_void {
    std::ptr::null_mut()
}

fn start_ipc_server() -> Result<(), String> {
    let mut server = IpcServer::new();
    server.set_handler(handle_gui_message);
    server
        .start()
        .map_err(|e| format!("Failed to start IPC server: {}", e))?;
    *ipc_server().lock().unwrap_or_else(|e| e.into_inner()) = Some(server);
    Ok(())
}

fn handle_gui_message(msg: IpcMessage) -> Option<IpcMessage> {
    match msg {
        IpcMessage::Ping => Some(IpcMessage::Pong {
            active: true,
            version: env!("CARGO_PKG_VERSION").to_string(),
        }),
        IpcMessage::SetTicket {
            app_id,
            steam_id,
            app_ticket,
            e_ticket,
        } => {
            let mut store = TicketStore::global()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let mut data = store.get(app_id).unwrap_or_default();
            data.app_id = app_id;
            if let Some(id) = steam_id {
                data.steam_id = Some(id);
            }
            if let Some(ticket) = app_ticket {
                data.app_ticket = Some(ticket);
            }
            if let Some(ticket) = e_ticket {
                data.e_ticket = Some(ticket);
            }
            let _ = store.set(data);
            None
        }
        IpcMessage::RemoveTicket { app_id } => {
            let mut store = TicketStore::global()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _ = store.remove(app_id);
            None
        }
        IpcMessage::SetAppIdSpoof { app_id, spoof_id } => {
            if spoof_id == interop_misc::ONLINE_FIX_APP_ID {
                interop_misc::set_online_fix_real_app_id(app_id);
            }
            None
        }
        IpcMessage::RemoveAppIdSpoof { .. } => {
            interop_misc::set_online_fix_real_app_id(0);
            None
        }
        IpcMessage::SetManifestPin {
            app_id,
            depot_id,
            manifest_id,
            size,
        } => {
            let active = if let Ok(gid) = manifest_id.trim().parse::<u64>() {
                manifest_hooks::set_manifest_override(
                    app_id,
                    depot_id,
                    ManifestOverride::new(gid, (size > 0).then_some(size)),
                );
                true
            } else {
                false
            };
            Some(IpcMessage::ManifestBound {
                app_id,
                depot_id,
                manifest_id,
                active,
            })
        }
        IpcMessage::RemoveManifestPin { app_id, depot_id } => {
            manifest_hooks::remove_manifest_override(app_id, depot_id);
            None
        }
        IpcMessage::SetPlayUnowned { enabled: _ } => None,
        _ => None,
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

        let hash3 = compute_func_hash("IClientUtils", "GetAppID");
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_ipc_command_values() {
        assert_eq!(EIPCCommand::InterfaceCall as u32, 1);
        assert_eq!(EIPCCommand::Handshake as u32, 2);
    }

    #[test]
    fn test_interface_values() {
        assert_eq!(EIPCInterface::IClientUser as u32, 1);
        assert_eq!(EIPCInterface::IClientUtils as u32, 2);
        assert_eq!(EIPCInterface::IClientNetworkingSocketsSerialized as u32, 46);
    }

    #[test]
    fn test_ipc_request_and_interface_call_reject_malformed_and_unknown() {
        // Unknown command ID (99) must not parse as valid InterfaceCall
        let mut bad_cmd = Vec::new();
        bad_cmd.extend_from_slice(&99u32.to_le_bytes());
        bad_cmd.extend_from_slice(&0u32.to_le_bytes());
        assert!(!IPCRequest::parse(&bad_cmd).ok());

        // Overflowing body_len must be rejected cleanly
        let mut overflow = Vec::new();
        overflow.extend_from_slice(&1u32.to_le_bytes());
        overflow.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(!IPCRequest::parse(&overflow).ok());

        // Valid InterfaceCall request + unknown interface ID (7) must be rejected
        let mut bad_iface_body = Vec::new();
        bad_iface_body.extend_from_slice(&7u32.to_le_bytes());
        bad_iface_body.extend_from_slice(&0x1234u32.to_le_bytes());
        bad_iface_body.extend_from_slice(&0u32.to_le_bytes());
        assert!(!IPCInterfaceCall::parse(&bad_iface_body).ok());

        // Valid IClientNetworkingSocketsSerialized call
        let mut good_body = Vec::new();
        good_body.extend_from_slice(&46u32.to_le_bytes());
        good_body.extend_from_slice(&0xCAFE_BABEu32.to_le_bytes());
        good_body.extend_from_slice(&0u32.to_le_bytes());
        let call = IPCInterfaceCall::parse(&good_body);
        assert!(call.ok());
        assert_eq!(
            call.interface_id(),
            EIPCInterface::IClientNetworkingSocketsSerialized
        );
        assert_eq!(call.func_hash(), 0xCAFE_BABE);

        // Null IPCBuffer must safely return empty slices
        let mut null_buf = IPCBuffer::from_raw(std::ptr::null_mut());
        assert!(null_buf.as_slice().is_empty());
        assert!(null_buf.as_mut_slice().is_empty());
    }

    #[test]
    fn test_register_handler_replaces_duplicates_and_gui_manifest_pin() {
        assert!(register_handler("UnknownIface", "Foo", None, None).is_err());

        register_handler("IClientUser", "DupMethodTest", None, None).unwrap();
        let count_after_first = handlers()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|h| h.name == "IClientUser::DupMethodTest")
            .count();
        register_handler("IClientUser", "DupMethodTest", None, None).unwrap();
        let count_after_second = handlers()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|h| h.name == "IClientUser::DupMethodTest")
            .count();
        assert_eq!(count_after_first, 1);
        assert_eq!(count_after_second, 1);

        let reply = handle_gui_message(IpcMessage::SetManifestPin {
            app_id: 55501,
            depot_id: 55502,
            manifest_id: "987654321".into(),
            size: 4096,
        })
        .unwrap();
        assert!(matches!(
            reply,
            IpcMessage::ManifestBound {
                app_id: 55501,
                depot_id: 55502,
                active: true,
                ..
            }
        ));
        assert!(manifest_hooks::get_manifest_overrides().contains_key(&(55501, 55502)));

        handle_gui_message(IpcMessage::RemoveManifestPin {
            app_id: 55501,
            depot_id: 55502,
        });
        assert!(!manifest_hooks::get_manifest_overrides().contains_key(&(55501, 55502)));
    }
}
