//! Interop IPC - Linux port of Windows Interop_IPC
//!
//! This module replaces Windows hooking (MinHook/DLL injection) with Linux
//! memory patching via `memory_patch.rs` and pattern-based symbol resolution
//! via `pattern_db.rs`. It intercepts Steam IPC messages between the game
//! and steamclient.so to implement the same handler logic as the Windows version.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::ffi::c_void;

use crate::steam_bridge::memory_patch::{MemoryPatch, patch_by_offset, JMP_ABS64_LEN};
use crate::steam_bridge::pattern_db::{PatternDb, PatternChannel, BinaryComponent, PatternEntry};
use crate::steam_bridge::binary_reader::BinaryFile;
use crate::steam_bridge::ipc::{IpcMessage, IpcServer, IpcClient};
use crate::steam_bridge::ticket_store::TicketStore;

/// IPC command types matching Steam's internal IPC protocol
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum EIPCCommand {
    InterfaceCall = 1,
    Handshake = 2,
    Callback = 3,
}

/// IPC interface IDs (matching Windows EIPCInterface)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum EIPCInterface {
    IClientUser = 1,
    IClientUtils = 2,
    IClientNetworkingSocketsSerialized = 46,
}

/// Function signature for IPC handlers (pre and post)
type IpcHandlerFn = Box<dyn Fn(*mut c_void, &mut [u8], &mut [u8]) -> bool + Send + Sync>;

/// Handler entry for a specific interface method
#[derive(Debug, Clone)]
pub struct IpcHandlerEntry {
    pub interface_id: EIPCInterface,
    pub method_name: String,
    pub func_hash: u32,
    pub fencepost: u32,
    pub argc: u32,
    pub pre_handler: Option<IpcHandlerFn>,
    pub post_handler: Option<IpcHandlerFn>,
}

/// Internal handler storage after pattern resolution
#[derive(Debug, Clone)]
pub struct ResolvedHandler {
    interface_id: EIPCInterface,
    func_hash: u32,
    name: String,
    fencepost: u32,
    argc: u32,
    pre_handler: Option<IpcHandlerFn>,
    post_handler: Option<IpcHandlerFn>,
}

/// Dispatch info for an incoming IPC message
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

/// Global handler registry
static HANDLER_REGISTRY: OnceLock<Mutex<Vec<ResolvedHandler>>> = OnceLock::new();

/// Global pattern database instance
static PATTERN_DB: OnceLock<PatternDb> = OnceLock::new();

/// Active memory patches for IPC hooks
static IPC_PATCHES: OnceLock<Mutex<Vec<MemoryPatch>>> = OnceLock::new();

/// Steam client base address (steamclient.so)
static STEAM_CLIENT_BASE: OnceLock<u64> = OnceLock::new();

/// Steam UI base address (steamui.so)
static STEAM_UI_BASE: OnceLock<u64> = OnceLock::new();

/// IPC server for GUI communication
static IPC_SERVER: OnceLock<Mutex<Option<IpcServer>>> = OnceLock::new();

/// Function pointer type for the original IPCProcessMessage
type IPCProcessMessageFn = unsafe extern "C" fn(
    p_server: *mut c_void,
    h_steam_pipe: u32,
    p_read: *mut c_void,
    p_write: *mut c_void,
) -> bool;

/// Calls the original IPCProcessMessage function
fn call_original(
    p_server: *mut c_void,
    h_steam_pipe: u32,
    p_read: *mut c_void,
    p_write: *mut c_void,
) -> bool {
    let orig: IPCProcessMessageFn = unsafe { std::mem::transmute(
        *crate::steam_bridge::interop::interop_ipc::PATTERN_DB.get_or_init(PatternDb::new)
    ) };
    // This is a placeholder - actual implementation would use the trampoline
    false
}

/// Original function pointer (stored after hooking)
static ORIGINAL_IPC_PROCESS_MESSAGE: OnceLock<IPCProcessMessageFn> = OnceLock::new();

/// Get or initialize the handler registry
fn handlers() -> &'static Mutex<Vec<ResolvedHandler>> {
    HANDLER_REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Get or initialize the pattern database
fn pattern_db() -> &'static PatternDb {
    PATTERN_DB.get_or_init(PatternDb::new)
}

/// Get or initialize the IPC patches list
fn ipc_patches() -> &'static Mutex<Vec<MemoryPatch>> {
    IPC_PATCHES.get_or_init(|| Mutex::new(Vec::new()))
}

/// Get or initialize the IPC server
fn ipc_server() -> &'static Mutex<Option<IpcServer>> {
    IPC_SERVER.get_or_init(|| Mutex::new(None))
}

/// Initialize the Interop IPC system
///
/// This replaces the Windows `InteropIPC::Install()` - instead of DLL hooks,
/// we use memory_patch.rs to inline hook the IPCProcessMessage function
/// in steamclient.so, located via pattern_db.rs.
pub fn install() -> Result<(), String> {
    // Initialize pattern database
    let _ = pattern_db();

    // Get steamclient.so base address
    let steam_client = crate::steam_bridge::ost_platform::dynamic_library::get_loaded("steamclient.so");
    let steam_client = if steam_client.is_null() {
        crate::steam_bridge::ost_platform::dynamic_library::get_loaded("steamclient64.so")
    } else {
        steam_client
    };

    if steam_client.is_null() {
        return Err("steamclient.so not loaded in process".into());
    }

    STEAM_CLIENT_BASE.set(steam_client as u64)
        .map_err(|_| "Failed to set steamclient base")?;

    // Get steamui.so base address
    let steam_ui = crate::steam_bridge::ost_platform::dynamic_library::get_loaded("steamui.so");
    let steam_ui = if steam_ui.is_null() {
        crate::steam_bridge::ost_platform::dynamic_library::get_loaded("steamui64.so")
    } else {
        steam_ui
    };

    if !steam_ui.is_null() {
        let _ = STEAM_UI_BASE.set(steam_ui as u64);
    }

    // Load pattern file for steamclient
    let binary = BinaryFile::open("steamclient.so")
        .map_err(|e| format!("Failed to open steamclient.so: {}", e))?;

    let pattern_file = pattern_db().lookup_by_binary(&binary, PatternChannel::Ipc, BinaryComponent::SteamClient)
        .map_err(|e| format!("Failed to load IPC patterns: {}", e))?;

    // Resolve IPCProcessMessage address from patterns
    let ipc_process_msg_entry = pattern_file.patterns.get("IPCProcessMessage")
        .ok_or("IPCProcessMessage pattern not found")?;

    let ipc_process_msg_addr = if let Some(addr) = ipc_process_msg_entry.address {
        addr
    } else if let Some(offset) = ipc_process_msg_entry.offset {
        STEAM_CLIENT_BASE.get().copied().unwrap_or(0).saturating_add(offset as u64)
    } else if let Some(pattern) = &ipc_process_msg_entry.pattern {
        // Scan for pattern (pattern string is hex-encoded)
        let pattern_bytes = hex::decode(pattern).map_err(|e| format!("Invalid pattern hex: {}", e))?;
        let found = crate::steam_bridge::ost_platform::byte_search::find_in_file(
            std::path::Path::new("steamclient.so"), &pattern_bytes, 4096)
            .ok_or_else(|| "Pattern scan failed".to_string())?;
        found
    } else {
        return Err("No address, offset, or pattern for IPCProcessMessage".into());
    };

    // Install the hook via memory_patch.rs
    let replacement = hook_ipc_process_message as *mut c_void as *mut u8;
    let target = ipc_process_msg_addr as *mut u8;

    let mut patch = unsafe { MemoryPatch::new(target, replacement) };
    unsafe { patch.install(JMP_ABS64_LEN) }?;

    // Store original function pointer
    let original: IPCProcessMessageFn = unsafe { std::mem::transmute(patch.trampoline) };
    ORIGINAL_IPC_PROCESS_MESSAGE.set(original)
        .map_err(|_| "Failed to store original IPCProcessMessage")?;

    // Save patch for cleanup
    ipc_patches().lock().unwrap().push(patch);

    // Register default handlers (IClientUser, IClientUtils)
    register_default_handlers();

    // Start IPC server for GUI communication
    start_ipc_server()?;

    log::info!("[InteropIPC] Installed successfully with {} handlers", handlers().lock().unwrap().len());

    Ok(())
}

/// Uninstall all hooks and cleanup
pub fn uninstall() -> Result<(), String> {
    // Stop IPC server
    if let Some(mut server) = ipc_server().lock().unwrap().take() {
        server.stop();
    }

    // Uninstall all patches
    let mut patches = ipc_patches().lock().unwrap();
    for patch in patches.drain(..) {
        unsafe { patch.uninstall() }?;
    }

    // Clear handlers
    handlers().lock().unwrap().clear();

    log::info!("[InteropIPC] Uninstalled");
    Ok(())
}

/// Register an IPC handler (called by feature modules)
pub fn register_handler(
    interface_name: &str,
    method_name: &str,
    pre: Option<IpcHandlerFn>,
    post: Option<IpcHandlerFn>,
) -> Result<(), String> {
    let pattern_file = get_pattern_file()?;

    // Find method in pattern file
    let method_key = format!("{}::{}", interface_name, method_name);
    let entry = pattern_file.patterns.get(&method_key)
        .ok_or_else(|| format!("No IPC spec for {}", method_key))?;

    let interface_id = match interface_name {
        "IClientUser" => EIPCInterface::IClientUser,
        "IClientUtils" => EIPCInterface::IClientUtils,
        _ => return Err(format!("Unknown interface: {}", interface_name)),
    };

    let func_hash = compute_func_hash(interface_name, method_name);

    let handler = ResolvedHandler {
        interface_id,
        func_hash,
        name: method_key,
        fencepost: entry.fencepost.unwrap_or(0) as u32,
        argc: entry.argc.unwrap_or(0) as u32,
        pre_handler: pre,
        post_handler: post,
    };

    handlers().lock().unwrap().push(handler);
    Ok(())
}

/// Register default handlers (equivalent to Windows InteropIPC_ISteamUser::Register and InteropIPC_ISteamUtils::Register)
fn register_default_handlers() {
    // IClientUser handlers from interop_ipc_isteamuser
    let _ = register_handler(
        "IClientUser",
        "GetSteamID",
        None,
        Some(Box::new(handler_post_get_steam_id)),
    );

    let _ = register_handler(
        "IClientUser",
        "GetAppOwnershipTicketExtendedData",
        None,
        Some(Box::new(handler_post_get_app_ownership_ticket)),
    );

    let _ = register_handler(
        "IClientUser",
        "RequestEncryptedAppTicket",
        None,
        Some(Box::new(handler_post_request_encrypted_app_ticket)),
    );

    let _ = register_handler(
        "IClientUser",
        "GetEncryptedAppTicket",
        None,
        Some(Box::new(handler_post_get_encrypted_app_ticket)),
    );

    // IClientUtils handlers
    let _ = register_handler(
        "IClientUtils",
        "GetAppID",
        None,
        Some(Box::new(handler_post_get_app_id)),
    );

    let _ = register_handler(
        "IClientUtils",
        "GetAPICallResult",
        None,
        Some(Box::new(handler_post_get_api_call_result)),
    );
}

/// Hook function for IPCProcessMessage - this is called instead of the original
unsafe extern "C" fn hook_ipc_process_message(
    p_server: *mut c_void,
    h_steam_pipe: u32,
    p_read: *mut c_void,
    p_write: *mut c_void,
) -> bool {
    // Parse the IPC message from p_read
    let read_buffer = IPCBuffer::from_raw(p_read);
    let write_buffer = IPCBuffer::from_raw(p_write);

    // Handle handshake
    handle_handshake(p_server, h_steam_pipe, &read_buffer);

    // Detect networking sockets usage
    detect_networking_sockets(&read_buffer);

    // Resolve dispatch
    let dispatch = resolve_dispatch(p_server, h_steam_pipe, &read_buffer);

    // If no handler, pass through to original
    if !dispatch.enabled() {
        return call_original(p_server, h_steam_pipe, p_read, p_write);
    }

    // Run pre-handler
    if let Some(handler) = &dispatch.handler {
        if let Some(pre) = &handler.pre_handler {
            pre(dispatch.pipe, read_buffer.as_mut_slice(), write_buffer.as_mut_slice());
        }
    }

    // Call original function
    let result = call_original(p_server, h_steam_pipe, p_read, p_write);

    // Run post-handler
    if result {
        if let Some(handler) = &dispatch.handler {
            if let Some(post) = &handler.post_handler {
                post(dispatch.pipe, read_buffer.as_mut_slice(), write_buffer.as_mut_slice());
            }
        }
    }

    result
}

/// Call the original IPCProcessMessage
fn call_original(
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

/// Parse the current pattern file
fn get_pattern_file() -> Result<crate::steam_bridge::pattern_db::PatternFile, String> {
    let binary = BinaryFile::open("steamclient.so")
        .map_err(|e| format!("Failed to open steamclient.so: {}", e))?;

    pattern_db().lookup_by_binary(&binary, PatternChannel::Ipc, BinaryComponent::SteamClient)
        .map_err(|e| e.to_string())
}

/// Compute FNV hash for interface::method (matching Windows FnvHash)
fn compute_func_hash(interface: &str, method: &str) -> u32 {
    use std::hash::{Hash, Hasher};

    let mut hasher = fnv::FnvHasher::default();
    format!("{}::{}", interface, method).hash(&mut hasher);
    hasher.finish() as u32
}

/// Resolve dispatch for an incoming IPC message
fn resolve_dispatch(p_server: *mut c_void, h_steam_pipe: u32, read: &IPCBuffer) -> IpcDispatch {
    let mut dispatch = IpcDispatch {
        pipe: std::ptr::null_mut(),
        handler: None,
    };

    // Get pipe client (would need integration with pipe manager)
    dispatch.pipe = get_pipe_client(p_server, h_steam_pipe);
    if dispatch.pipe.is_null() {
        return dispatch;
    }

    // Parse IPC request
    let request = IPCRequest::parse(read.as_slice());
    if !request.ok() {
        return dispatch;
    }

    if request.command() != EIPCCommand::InterfaceCall {
        return dispatch;
    }

    // Check if app is tracked (has depot in config)
    let app_id = resolve_app_id();
    if app_id == 0 || !has_depot(app_id) {
        return dispatch;
    }

    // Parse interface call
    let call = IPCInterfaceCall::parse(request.body());
    if !call.ok() {
        return dispatch;
    }

    // Lookup handler
    let handlers_list = handlers().lock().unwrap();
    for handler in handlers_list.iter() {
        if handler.interface_id == call.interface_id() && handler.func_hash == call.func_hash() {
            dispatch.handler = Some(handler.clone());
            break;
        }
    }

    dispatch
}

/// Handle handshake message
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

/// Detect SteamNetworkingSockets usage for online fix
fn detect_networking_sockets(read: &IPCBuffer) {
    if !is_online_fix_active() || should_report_online_fix_app_id() {
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
        notify_networking_sockets_used();
    }
}

/// IPC buffer wrapper for safe access
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

/// IPC Request parser
struct IPCRequest {
    data: Vec<u8>,
    valid: bool,
}

impl IPCRequest {
    fn parse(data: &[u8]) -> Self {
        if data.len() < 8 {
            return Self { data: Vec::new(), valid: false };
        }

        let body_len = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;

        if data.len() < 8 + body_len {
            return Self { data: Vec::new(), valid: false };
        }

        Self {
            data: data[8..8+body_len].to_vec(),
            valid: true,
        }
    }

    fn ok(&self) -> bool {
        self.valid
    }

    fn command(&self) -> EIPCCommand {
        EIPCCommand::InterfaceCall
    }

    fn body(&self) -> &[u8] {
        &self.data
    }
}

/// IPC Handshake Request
struct IPCHandshakeReq {
    data: Vec<u8>,
    valid: bool,
}

impl IPCHandshakeReq {
    fn parse(data: &[u8]) -> Self {
        Self { data: data.to_vec(), valid: !data.is_empty() }
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

/// IPC Interface Call
struct IPCInterfaceCall {
    data: Vec<u8>,
    valid: bool,
}

impl IPCInterfaceCall {
    fn parse(data: &[u8]) -> Self {
        if data.len() < 12 {
            return Self { data: Vec::new(), valid: false };
        }

        Self { data: data.to_vec(), valid: true }
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

/// Get pipe client (placeholder - would integrate with pipe manager)
fn get_pipe_client(_p_server: *mut c_void, _h_steam_pipe: u32) -> *mut c_void {
    std::ptr::null_mut()
}

/// Resolve current app ID (placeholder)
fn resolve_app_id() -> u32 {
    0
}

/// Check if app has depot configured (placeholder)
fn has_depot(_app_id: u32) -> bool {
    false
}

/// Check if online fix is active (placeholder)
fn is_online_fix_active() -> bool {
    false
}

/// Check if should report online fix app ID (placeholder)
fn should_report_online_fix_app_id() -> bool {
    false
}

/// Notify networking sockets used (placeholder)
fn notify_networking_sockets_used() {
    log::debug!("[InteropIPC] Networking sockets detected");
}

/// Start IPC server for GUI communication
fn start_ipc_server() -> Result<(), String> {
    let mut server = IpcServer::new();

    server.set_handler(|msg| {
        handle_gui_message(msg)
    });

    server.start().map_err(|e| format!("Failed to start IPC server: {}", e))?;

    *ipc_server().lock().unwrap() = Some(server);
    Ok(())
}

/// Handle message from GUI via IPC
fn handle_gui_message(msg: IpcMessage) -> Option<IpcMessage> {
    match msg {
        IpcMessage::Ping => Some(IpcMessage::Pong {
            active: true,
            version: env!("CARGO_PKG_VERSION").to_string(),
        }),
        IpcMessage::SetTicket { app_id, steam_id, app_ticket, e_ticket } => {
            let store = TicketStore::global();
            if let Some(ticket) = app_ticket {
                // Note: would need &mut to store
                let _ = ticket;
            }
            if let Some(ticket) = e_ticket {
                let _ = ticket;
            }
            if let Some(id) = steam_id {
                let _ = id;
            }
            None
        }
        IpcMessage::SetPlayUnowned { enabled } => {
            let _ = enabled;
            None
        }
        _ => None,
    }
}

// ════════════════════════════════════════════════════════════════
// IPC Post-Handlers (matching Windows implementation)
// ════════════════════════════════════════════════════════════════

/// Post-handler: IClientUser::GetSteamID
fn handler_post_get_steam_id(_pipe: *mut c_void, _read: &mut [u8], write: &mut [u8]) -> bool {
    let app_id = resolve_app_id();
    if app_id == 0 {
        return false;
    }

    let store = TicketStore::global();
    let spoofed = store.get_spoof_steam_id(app_id);

    if spoofed == 0 {
        return false;
    }

    let mut resp = GetSteamIDResp::new(write);
    if !resp.ok() {
        return false;
    }

    log::debug!("[InteropIPC] GetSteamID: AppId={} spoofed to 0x{:X}", app_id, spoofed);
    resp.set_return_value(spoofed);
    true
}

/// Post-handler: IClientUser::GetAppOwnershipTicketExtendedData
fn handler_post_get_app_ownership_ticket(_pipe: *mut c_void, read: &mut [u8], write: &mut [u8]) -> bool {
    let req = GetAppOwnershipTicketExtendedDataReq::parse(read);
    if !req.ok() {
        return false;
    }

    if req.max_ticket_size() < 0 {
        return false;
    }

    let app_id = if req.app_id() == 480 {
        resolve_app_id()
    } else {
        req.app_id()
    };

    let ticket = get_app_ownership_ticket(app_id)?;

    if ticket.data.len() > req.max_ticket_size() as usize {
        log::warn!("[InteropIPC] Ticket too large for buffer");
        return false;
    }

    let mut resp = GetAppOwnershipTicketExtendedDataResp::new(write, req.max_ticket_size() as usize);
    if !resp.ok() {
        return false;
    }

    resp.set_return_value(ticket.total_size as u32);
    resp.set_ticket(&ticket.data);

    log::debug!("[InteropIPC] GetAppOwnershipTicketExtendedData: AppId={}", app_id);
    true
}

/// Post-handler: IClientUser::RequestEncryptedAppTicket
fn handler_post_request_encrypted_app_ticket(_pipe: *mut c_void, read: &mut [u8], write: &mut [u8]) -> bool {
    let mut resp = RequestEncryptedAppTicketResp::new(write);
    if !resp.ok() {
        return false;
    }

    let app_id = resolve_app_id();
    let req = RequestEncryptedAppTicketReq::parse(read);
    let _nonce = req.ok().then(|| req.data()).unwrap_or_default();

    let async_call = resp.return_value();
    record_encrypted_ticket(async_call, app_id);

    log::debug!("[InteropIPC] RequestEncryptedAppTicket: AppId={} hAsyncCall=0x{:X}", app_id, async_call);
    true
}

/// Post-handler: IClientUser::GetEncryptedAppTicket
fn handler_post_get_encrypted_app_ticket(_pipe: *mut c_void, _read: &mut [u8], write: &mut [u8]) -> bool {
    let app_id = resolve_app_id();

    let ticket = get_encrypted_ticket_from_store(app_id);

    if ticket.is_empty() {
        log::debug!("[InteropIPC] GetEncryptedAppTicket: no ticket for AppId={}", app_id);
        return false;
    }

    let mut resp = GetEncryptedAppTicketResp::new(write);
    if !resp.ok() {
        return false;
    }

    let ticket_size = ticket.len() as u32;
    if !ensure_buffer_capacity(write, ticket_size) {
        return false;
    }

    resp.set_return_value(true);
    resp.set_ticket_size(ticket_size);
    resp.set_ticket(&ticket);

    log::debug!("[InteropIPC] GetEncryptedAppTicket: AppId={} size={}", app_id, ticket_size);
    true
}

/// Post-handler: IClientUtils::GetAppID
fn handler_post_get_app_id(_pipe: *mut c_void, _read: &mut [u8], write: &mut [u8]) -> bool {
    if should_report_online_fix_app_id() {
        return false;
    }

    let real_app_id = resolve_app_id();
    if real_app_id == 0 {
        return false;
    }

    let mut resp = GetAppIDResp::new(write);
    if !resp.ok() {
        return false;
    }

    let current = resp.return_value();
    if current == real_app_id {
        return false;
    }

    resp.set_return_value(real_app_id);
    log::info!("[InteropIPC] GetAppID: spoof {} -> {}", current, real_app_id);
    true
}

/// Post-handler: IClientUtils::GetAPICallResult
fn handler_post_get_api_call_result(_pipe: *mut c_void, read: &mut [u8], write: &mut [u8]) -> bool {
    let req = GetAPICallResultReq::parse(read);
    if !req.ok() {
        return false;
    }

    let callback_id = req.expected_callback();
    let async_call = req.async_call();
    let callback_capacity = req.callback_capacity();

    const ENCRYPTED_APP_TICKET_RESPONSE_CALLBACK: u32 = 1010;

    if callback_id == ENCRYPTED_APP_TICKET_RESPONSE_CALLBACK {
        let app_id = take_encrypted_ticket(async_call);
        if app_id == 0 {
            return false;
        }

        let callback = EncryptedAppTicketResponse {
            result: EResult::OK,
        };

        if !write_api_call_result(write, callback_capacity, callback) {
            record_encrypted_ticket(async_call, app_id);
            return false;
        }

        log::debug!("[InteropIPC] EncryptedAppTicketResponse: AppId={}", app_id);
        return true;
    }

    false
}

// ════════════════════════════════════════════════════════════════
// Helper types and functions (simplified implementations)
// ════════════════════════════════════════════════════════════════

#[derive(Debug)]
struct AppOwnershipTicket {
    data: Vec<u8>,
    total_size: usize,
    app_id_offset: i32,
    steam_id_offset: i32,
    signature_offset: i32,
    signature_size: i32,
}

fn get_app_ownership_ticket(_app_id: u32) -> Option<AppOwnershipTicket> {
    None
}

fn get_encrypted_ticket_from_store(_app_id: u32) -> Vec<u8> {
    Vec::new()
}

fn record_encrypted_ticket(_async_call: u64, _app_id: u32) {}

fn take_encrypted_ticket(_async_call: u64) -> u32 {
    0
}

fn ensure_buffer_capacity(_write: &mut [u8], _needed: u32) -> bool {
    true
}

fn write_api_call_result<T: Copy>(_write: &mut [u8], _capacity: u32, _callback: T) -> bool {
    true
}

// Response/Request parser stubs
struct GetSteamIDResp { _data: Vec<u8> }
impl GetSteamIDResp {
    fn new(data: &mut [u8]) -> Self { Self { _data: data.to_vec() } }
    fn ok(&self) -> bool { true }
    fn set_return_value(&mut self, _val: u64) {}
}

struct GetAppOwnershipTicketExtendedDataReq { _data: Vec<u8> }
impl GetAppOwnershipTicketExtendedDataReq {
    fn parse(_data: &[u8]) -> Self { Self { _data: Vec::new() } }
    fn ok(&self) -> bool { true }
    fn app_id(&self) -> u32 { 0 }
    fn max_ticket_size(&self) -> i32 { 1024 }
}

struct GetAppOwnershipTicketExtendedDataResp { _data: Vec<u8> }
impl GetAppOwnershipTicketExtendedDataResp {
    fn new(_data: &mut [u8], _capacity: usize) -> Self { Self { _data: Vec::new() } }
    fn ok(&self) -> bool { true }
    fn set_return_value(&mut self, _val: u32) {}
    fn set_ticket(&mut self, _data: &[u8]) {}
}

struct RequestEncryptedAppTicketReq { _data: Vec<u8> }
impl RequestEncryptedAppTicketReq {
    fn parse(_data: &[u8]) -> Self { Self { _data: Vec::new() } }
    fn ok(&self) -> bool { true }
    fn data(&self) -> &[u8] { &[] }
}

struct RequestEncryptedAppTicketResp { _data: Vec<u8> }
impl RequestEncryptedAppTicketResp {
    fn new(_data: &mut [u8]) -> Self { Self { _data: Vec::new() } }
    fn ok(&self) -> bool { true }
    fn return_value(&self) -> u64 { 0 }
}

struct GetEncryptedAppTicketResp { _data: Vec<u8> }
impl GetEncryptedAppTicketResp {
    fn new(_data: &mut [u8]) -> Self { Self { _data: Vec::new() } }
    fn ok(&self) -> bool { true }
    fn set_return_value(&mut self, _val: bool) {}
    fn set_ticket_size(&mut self, _size: u32) {}
    fn set_ticket(&mut self, _data: &[u8]) {}
}

struct GetAppIDResp { _data: Vec<u8> }
impl GetAppIDResp {
    fn new(_data: &mut [u8]) -> Self { Self { _data: Vec::new() } }
    fn ok(&self) -> bool { true }
    fn return_value(&self) -> u32 { 0 }
    fn set_return_value(&mut self, _val: u32) {}
}

struct GetAPICallResultReq { _data: Vec<u8> }
impl GetAPICallResultReq {
    fn parse(_data: &[u8]) -> Self { Self { _data: Vec::new() } }
    fn ok(&self) -> bool { true }
    fn expected_callback(&self) -> u32 { 0 }
    fn async_call(&self) -> u64 { 0 }
    fn callback_capacity(&self) -> u32 { 0 }
}

#[derive(Clone, Copy)]
struct EncryptedAppTicketResponse {
    result: EResult,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EResult {
    OK = 1,
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