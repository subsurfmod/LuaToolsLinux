//! Interop IPC - Linux port of Windows `Interop_IPC`.
//!
//! Uses `memory_patch.rs` for 64-bit indirect jump hooks and `pattern_db.rs` for
//! pattern-based symbol resolution in `steamclient.so`.

use std::ffi::c_void;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock};

use crate::steam_bridge::binary_reader::BinaryFile;
use crate::steam_bridge::interop::interop_misc;
use crate::steam_bridge::interop::ipc::fnv1a_hash;
use crate::steam_bridge::ipc::{IpcMessage, IpcServer};
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

/// IPC interface IDs (matching Windows `EIPCInterface`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum EIPCInterface {
    IClientUser = 1,
    IClientUtils = 2,
    IClientNetworkingSocketsSerialized = 46,
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

static ORIGINAL_IPC_PROCESS_MESSAGE: OnceLock<IPCProcessMessageFn> = OnceLock::new();

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

/// Initialize the Interop IPC system.
pub fn install() -> Result<(), String> {
    let _ = pattern_db();

    let steam_client =
        crate::steam_bridge::ost_platform::dynamic_library::get_loaded("steamclient.so");
    let steam_client = if steam_client.is_null() {
        crate::steam_bridge::ost_platform::dynamic_library::get_loaded("steamclient64.so")
    } else {
        steam_client
    };

    if steam_client.is_null() {
        return Err("steamclient.so not loaded in process".into());
    }

    let _ = STEAM_CLIENT_BASE.set(steam_client as u64);

    let steam_ui = crate::steam_bridge::ost_platform::dynamic_library::get_loaded("steamui.so");
    let steam_ui = if steam_ui.is_null() {
        crate::steam_bridge::ost_platform::dynamic_library::get_loaded("steamui64.so")
    } else {
        steam_ui
    };
    if !steam_ui.is_null() {
        let _ = STEAM_UI_BASE.set(steam_ui as u64);
    }

    let binary = BinaryFile::open("steamclient.so")
        .map_err(|e| format!("Failed to open steamclient.so: {}", e))?;

    let pattern_file = pattern_db()
        .lookup_by_binary(&binary, PatternChannel::Ipc, BinaryComponent::SteamClient)
        .map_err(|e| format!("Failed to load IPC patterns: {}", e))?;

    let ipc_process_msg_entry = pattern_file
        .patterns
        .get("IPCProcessMessage")
        .ok_or("IPCProcessMessage pattern not found")?;

    let ipc_process_msg_addr = if let Some(addr) = ipc_process_msg_entry.address {
        addr
    } else if let Some(offset) = ipc_process_msg_entry.offset {
        STEAM_CLIENT_BASE
            .get()
            .copied()
            .unwrap_or(0)
            .saturating_add(offset as u64)
    } else if let Some(pattern) = &ipc_process_msg_entry.pattern {
        let pattern_bytes =
            hex::decode(pattern).map_err(|e| format!("Invalid pattern hex: {}", e))?;
        crate::steam_bridge::ost_platform::byte_search::find_in_file(
            std::path::Path::new("steamclient.so"),
            &pattern_bytes,
            4096,
        )
        .ok_or_else(|| "Pattern scan failed".to_string())?
    } else {
        return Err("No address, offset, or pattern for IPCProcessMessage".into());
    };

    let replacement = hook_ipc_process_message as *mut c_void as *mut u8;
    let target = ipc_process_msg_addr as *mut u8;

    let mut patch = MemoryPatch::new(target, replacement);
    unsafe { patch.install(JMP_ABS64_LEN) }?;

    if !patch.trampoline.is_null() {
        let original: IPCProcessMessageFn = unsafe { std::mem::transmute(patch.trampoline) };
        let _ = ORIGINAL_IPC_PROCESS_MESSAGE.set(original);
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

    let mut patches = ipc_patches().lock().unwrap_or_else(|e| e.into_inner());
    for mut patch in patches.drain(..) {
        unsafe { patch.uninstall() }?;
    }

    handlers().lock().unwrap_or_else(|e| e.into_inner()).clear();
    log::info!("[InteropIPC] Uninstalled");
    Ok(())
}

/// Register an IPC handler (called by feature modules).
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

    handlers()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(handler);
    Ok(())
}

fn register_default_handlers() {
    let _ = register_handler(
        "IClientUser",
        "GetSteamID",
        None,
        Some(Arc::new(handler_post_get_steam_id)),
    );
    let _ = register_handler(
        "IClientUser",
        "GetAppOwnershipTicketExtendedData",
        None,
        Some(Arc::new(handler_post_get_app_ownership_ticket)),
    );
    let _ = register_handler(
        "IClientUser",
        "RequestEncryptedAppTicket",
        None,
        Some(Arc::new(handler_post_request_encrypted_app_ticket)),
    );
    let _ = register_handler(
        "IClientUser",
        "GetEncryptedAppTicket",
        None,
        Some(Arc::new(handler_post_get_encrypted_app_ticket)),
    );
    let _ = register_handler(
        "IClientUtils",
        "GetAppID",
        None,
        Some(Arc::new(handler_post_get_app_id)),
    );
    let _ = register_handler(
        "IClientUtils",
        "GetAPICallResult",
        None,
        Some(Arc::new(handler_post_get_api_call_result)),
    );
}

unsafe extern "C" fn hook_ipc_process_message(
    p_server: *mut c_void,
    h_steam_pipe: u32,
    p_read: *mut c_void,
    p_write: *mut c_void,
) -> bool {
    if p_read.is_null() || p_write.is_null() {
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
}

/// Call the original `IPCProcessMessage` trampoline if installed.
pub fn call_original(
    p_server: *mut c_void,
    h_steam_pipe: u32,
    p_read: *mut c_void,
    p_write: *mut c_void,
) -> bool {
    if let Some(original) = ORIGINAL_IPC_PROCESS_MESSAGE.get() {
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
        Self { ptr, size: 4096 }
    }

    fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr as *const u8, self.size) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
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
        let cmd = match cmd_raw {
            1 => EIPCCommand::InterfaceCall,
            2 => EIPCCommand::Handshake,
            3 => EIPCCommand::Callback,
            _ => EIPCCommand::InterfaceCall,
        };
        let body_len = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
        if data.len() < 8 + body_len {
            return Self {
                cmd,
                data: Vec::new(),
                valid: false,
            };
        }

        Self {
            cmd,
            data: data[8..8 + body_len].to_vec(),
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
            valid: !data.is_empty(),
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
    data: Vec<u8>,
    valid: bool,
}

impl IPCInterfaceCall {
    fn parse(data: &[u8]) -> Self {
        if data.len() < 12 {
            return Self {
                data: Vec::new(),
                valid: false,
            };
        }
        Self {
            data: data.to_vec(),
            valid: true,
        }
    }

    fn ok(&self) -> bool {
        self.valid
    }

    fn interface_id(&self) -> EIPCInterface {
        if self.data.len() >= 4 {
            match u32::from_le_bytes([self.data[0], self.data[1], self.data[2], self.data[3]]) {
                1 => EIPCInterface::IClientUser,
                2 => EIPCInterface::IClientUtils,
                46 => EIPCInterface::IClientNetworkingSocketsSerialized,
                _ => EIPCInterface::IClientUser,
            }
        } else {
            EIPCInterface::IClientUser
        }
    }

    fn func_hash(&self) -> u32 {
        if self.data.len() >= 8 {
            u32::from_le_bytes([self.data[4], self.data[5], self.data[6], self.data[7]])
        } else {
            0
        }
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
        IpcMessage::SetPlayUnowned { enabled: _ } => None,
        _ => None,
    }
}

fn handler_post_get_steam_id(_pipe: *mut c_void, _read: &mut [u8], write: &mut [u8]) -> bool {
    let app_id = interop_misc::resolve_app_id();
    if app_id == 0 || write.len() < 8 {
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

    write[0..8].copy_from_slice(&spoofed.to_le_bytes());
    log::debug!(
        "[InteropIPC] GetSteamID: AppId={} spoofed to 0x{:X}",
        app_id,
        spoofed
    );
    true
}

fn handler_post_get_app_ownership_ticket(
    _pipe: *mut c_void,
    read: &mut [u8],
    write: &mut [u8],
) -> bool {
    if read.len() < 8 {
        return false;
    }
    let req_app_id = u32::from_le_bytes([read[0], read[1], read[2], read[3]]);
    let max_ticket_size = i32::from_le_bytes([read[4], read[5], read[6], read[7]]);
    if max_ticket_size < 0 {
        return false;
    }

    let app_id = if req_app_id == interop_misc::ONLINE_FIX_APP_ID {
        interop_misc::resolve_app_id()
    } else {
        req_app_id
    };

    let ticket_bytes = {
        let mut store = TicketStore::global()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        store
            .get(app_id)
            .ok()
            .and_then(|d| d.app_ticket)
            .and_then(|h| hex::decode(&h).ok())
    };
    let Some(ticket) = ticket_bytes else {
        return false;
    };

    if ticket.len() > max_ticket_size as usize || write.len() < 4 + ticket.len() {
        return false;
    }
    write[0..4].copy_from_slice(&(ticket.len() as u32).to_le_bytes());
    write[4..4 + ticket.len()].copy_from_slice(&ticket);
    true
}

fn handler_post_request_encrypted_app_ticket(
    _pipe: *mut c_void,
    _read: &mut [u8],
    write: &mut [u8],
) -> bool {
    if write.len() < 8 {
        return false;
    }
    let app_id = interop_misc::resolve_app_id();
    let async_call = u64::from_le_bytes([
        write[0], write[1], write[2], write[3], write[4], write[5], write[6], write[7],
    ]);
    let mut store = TicketStore::global()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    store.record_async_call(async_call, app_id);
    true
}

fn handler_post_get_encrypted_app_ticket(
    _pipe: *mut c_void,
    _read: &mut [u8],
    write: &mut [u8],
) -> bool {
    let app_id = interop_misc::resolve_app_id();
    let ticket = {
        let mut store = TicketStore::global()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        store.get_encrypted_ticket(app_id).unwrap_or_default()
    };
    if ticket.is_empty() || write.len() < 5 + ticket.len() {
        return false;
    }
    write[0] = 1;
    write[1..5].copy_from_slice(&(ticket.len() as u32).to_le_bytes());
    write[5..5 + ticket.len()].copy_from_slice(&ticket);
    true
}

fn handler_post_get_app_id(_pipe: *mut c_void, _read: &mut [u8], write: &mut [u8]) -> bool {
    if interop_misc::should_report_online_fix_app_id() || write.len() < 4 {
        return false;
    }
    let real_app_id = interop_misc::resolve_app_id();
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

fn handler_post_get_api_call_result(_pipe: *mut c_void, read: &mut [u8], write: &mut [u8]) -> bool {
    if read.len() < 16 || write.len() < 4 {
        return false;
    }
    let callback_id = u32::from_le_bytes([read[0], read[1], read[2], read[3]]);
    let async_call = u64::from_le_bytes([
        read[4], read[5], read[6], read[7], read[8], read[9], read[10], read[11],
    ]);
    const ENCRYPTED_APP_TICKET_RESPONSE_CALLBACK: u32 = 1010;
    if callback_id == ENCRYPTED_APP_TICKET_RESPONSE_CALLBACK {
        let app_id = {
            let mut store = TicketStore::global()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            store.take_async_call(async_call).unwrap_or(0)
        };
        if app_id == 0 {
            return false;
        }
        // EResult::OK = 1
        write[0..4].copy_from_slice(&1u32.to_le_bytes());
        return true;
    }
    false
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
}
