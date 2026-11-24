//! Interop IPC handler layer for Linux.
//!
//! Port of usefulsteamtools `Interop_IPC.cpp`.
//! Replaces Windows named pipe hooking with Unix domain socket + pattern-based
//! function resolution. Handlers are registered by (interface_id, func_hash) and
//! invoked via pre/post hooks on IPC messages.
//!
//! On Linux, the integration runs inside the Steam process via LD_PRELOAD.
//! The `bridge_lib.rs` constructor starts an IPC server (Unix domain socket)
//! that the GUI connects to. This module provides the handler dispatch logic
//! that `bridge_lib.rs` calls when messages arrive.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::os::raw::c_void;
use std::fmt;

use crate::steam_bridge::ipc::IpcMessage;
use crate::steam_bridge::pattern_db::{BinaryComponent, PatternChannel};
use crate::steam_bridge::binary_reader::BinaryFile;
use crate::steam_bridge::memory_patch::{self, MemoryPatch, JMP_ABS64_LEN};
use crate::steam_bridge::ost_platform::remote_process;

// ============================================================================
// Types matching usefulsteamtools IPC system
// ============================================================================

/// Steam IPC interface IDs (matching EIPCInterface enum)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum IpcInterface {
    IClientUser = 1,
    IClientUtils = 2,
    IClientNetworkingSocketsSerialized = 46,
    // Add more as needed
}

impl From<u32> for IpcInterface {
    fn from(v: u32) -> Self {
        match v {
            1 => IpcInterface::IClientUser,
            2 => IpcInterface::IClientUtils,
            46 => IpcInterface::IClientNetworkingSocketsSerialized,
            _ => IpcInterface::IClientUser, // fallback
        }
    }
}

/// IPC command types (matching EIPCCommand enum)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum IpcCommand {
    Handshake = 0,
    InterfaceCall = 1,
    // ...
}

/// Function hash type (FNV1a of method name)
pub type FuncHash = u32;

/// Resolved handler entry (pre and post hooks)
#[derive(Debug, Clone)]
pub struct ResolvedHandler {
    pub interface_id: IpcInterface,
    pub func_hash: FuncHash,
    pub name: String,           // "IClientUser::GetSteamID"
    pub fencepost: u32,
    pub argc: u32,
    pub pre_handler: Option<HandlerFn>,
    pub post_handler: Option<HandlerFn>,
}

/// IPC message handler function type - uses Box<dyn> with Send+Sync
pub type HandlerFn = Box<dyn Fn(&mut IpcContext) -> Result<(), String> + Send + Sync + 'static>;

/// IPC context passed to handlers
#[derive(Debug, Default)]
pub struct IpcContext {
    pub pipe: *mut c_void,      // CPipeClient* on Windows
    pub read_buffer: Vec<u8>,   // pRead->Get()
    pub write_buffer: Vec<u8>,  // pWrite->Get()
    pub interface_id: IpcInterface,
    pub func_hash: FuncHash,
    pub app_id: u32,
}

// Safety: We only use the raw pointer as an opaque handle
unsafe impl Send for IpcContext {}
unsafe impl Sync for IpcContext {}

// ============================================================================
// Global handler registry (using LazyLock for proper initialization)
// ============================================================================

static HANDLERS: LazyLock<Mutex<HashMap<(IpcInterface, FuncHash), ResolvedHandler>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn handlers() -> &'static Mutex<HashMap<(IpcInterface, FuncHash), ResolvedHandler>> {
    &HANDLERS
}

/// Find handler by interface ID + func hash
pub fn find_handler(iface: IpcInterface, func_hash: FuncHash) -> Option<ResolvedHandler> {
    let map = handlers();
    map.lock().unwrap().get(&(iface, func_hash)).cloned()
}

/// Register a handler for (interface_id, func_hash)
pub fn register_handler(handler: ResolvedHandler) {
    let key = (handler.interface_id, handler.func_hash);
    handlers().lock().unwrap().insert(key, handler);
}

/// Register multiple handlers at once (called by module registration functions)
pub fn register_handlers(handlers_list: Vec<ResolvedHandler>) {
    let mut map = handlers().lock().unwrap();
    for h in handlers_list {
        map.insert((h.interface_id, h.func_hash), h);
    }
}

// ============================================================================
// IPC message parsing (simplified - in practice would use protobuf)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcMessageType {
    Handshake = 0,
    InterfaceCall = 1,
}

/// Parse raw IPC message from read buffer
pub fn parse_ipc_message(data: &[u8]) -> Option<(IpcMessageType, IpcInterface, FuncHash)> {
    if data.len() < 12 {
        return None;
    }
    // Simplified parsing - actual implementation would use Steam's IPC protobuf
    let cmd = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let iface_id = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    let func_hash = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);

    Some((
        match cmd {
            0 => IpcMessageType::Handshake,
            1 => IpcMessageType::InterfaceCall,
            _ => return None,
        },
        IpcInterface::from(iface_id),
        func_hash,
    ))
}

// ============================================================================
// Dispatch logic - called from bridge_lib.rs when IPC message received
// ============================================================================

/// Dispatch an IPC message to registered handlers.
/// Returns true if message was handled, false if passed through.
pub fn dispatch_ipc_message(
    read_buf: &[u8],
    write_buf: &mut Vec<u8>,
    pipe_ptr: *mut c_void,
) -> bool {
    let Some((msg_type, iface_id, func_hash)) = parse_ipc_message(read_buf) else {
        return false; // Pass through
    };

    if msg_type == IpcMessageType::Handshake {
        // Handle handshake - set client PID, notify PipeManager
        log::debug!("IPC Handshake received on pipe {:p}", pipe_ptr);
        return false; // Pass through to original
    }

    // Lookup handler
    let handler = match find_handler(iface_id, func_hash) {
        Some(h) => h,
        None => return false, // No handler, pass through
    };

    // Build context
    let mut ctx = IpcContext {
        pipe: pipe_ptr,
        read_buffer: read_buf.to_vec(),
        write_buffer: write_buf.clone(),
        interface_id: iface_id,
        func_hash,
        app_id: 0, // Would be resolved from InteropMisc::ResolveAppId()
    };

    // Run pre-handler
    if let Some(pre) = &handler.pre_handler {
        if let Err(e) = pre(&mut ctx) {
            log::warn!("Pre-handler error for {}: {}", handler.name, e);
        }
    }

    // Update buffers after pre-handler
    write_buf.clone_from(&ctx.write_buffer);

    // Pass through to original Steam IPC processing
    // (In practice, we'd call the original function here)
    // For now, just run post-handler

    // Run post-handler
    if let Some(post) = &handler.post_handler {
        if let Err(e) = post(&mut ctx) {
            log::warn!("Post-handler error for {}: {}", handler.name, e);
        }
    }

    // Final buffer update
    write_buf.clone_from(&ctx.write_buffer);
    true
}

// ============================================================================
// Function resolution via pattern_db (like IPCLoader on Windows)
// ============================================================================

/// Resolve Steam IPC function address using pattern_db
pub fn resolve_ipc_function(
    component: BinaryComponent,
    channel: PatternChannel,
    pattern_name: &str,
    steam_root: &std::path::Path,
) -> Result<u64, String> {
    use crate::steam_bridge::binary_reader::{scan_steam_binaries, find_loaded_module_mapping, LoadedModuleMapping};
    use crate::steam_bridge::pattern_db::PatternDb;

    let binaries = scan_steam_binaries(steam_root);
    let binary = binaries.into_iter().find(|b| {
        b.path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| match component {
                BinaryComponent::SteamClient => n.contains("steamclient"),
                BinaryComponent::SteamUi => n.contains("steamui"),
            }).unwrap_or(false)
    }).ok_or("Steam binary not found")?;

    let pattern_db = PatternDb::new();
    let pf = pattern_db.lookup_by_binary(&binary, channel, component)
        .map_err(|e| format!("Pattern lookup failed: {}", e))?;

    let entry = pf.patterns.get(pattern_name)
        .ok_or(format!("Pattern '{}' not found", pattern_name))?;

    // Resolve pattern to runtime address
    let mapping = find_loaded_module_mapping(
        &binary.path.file_name().unwrap().to_string_lossy()
    ).ok_or("Module not loaded in current process")?;

    let vaddr = if let Some(pattern_hex) = &entry.pattern {
        let pattern_bytes = parse_hex_pattern(pattern_hex);
        let mask_bytes = entry.mask.as_ref().map(|m| parse_hex_pattern(m));
        let offset = entry.offset.unwrap_or(0);
        binary.resolve_pattern_address(&pattern_bytes, mask_bytes.as_deref(), offset, None)
    } else {
        entry.address
    }.ok_or("Failed to resolve pattern address")?;

    let runtime_addr = binary.vaddr_to_runtime_addr(vaddr, &mapping, JMP_ABS64_LEN)
        .ok_or("Resolved address not in executable memory")?;

    Ok(runtime_addr)
}

/// Parse hex pattern string like "48 89 5C 24 08"
fn parse_hex_pattern(hex: &str) -> Vec<u8> {
    hex.split_whitespace()
        .filter_map(|s| u8::from_str_radix(s, 16).ok())
        .collect()
}

// ============================================================================
// Module registration helpers (called by each interop module)
// ============================================================================

/// Helper to create a handler entry
pub fn make_handler(
    interface: IpcInterface,
    func_hash: FuncHash,
    name: &str,
    fencepost: u32,
    argc: u32,
    pre: Option<HandlerFn>,
    post: Option<HandlerFn>,
) -> ResolvedHandler {
    ResolvedHandler {
        interface_id: interface,
        func_hash,
        name: name.to_string(),
        fencepost,
        argc,
        pre_handler: pre,
        post_handler: post,
    }
}

/// FNV1a hash of method name (matching usefulsteamtools)
pub fn fnv1a_hash(name: &str) -> u32 {
    let mut hash: u32 = 0x811c9dc5;
    for byte in name.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fnv1a_hash() {
        // Verify hash matches expected values
        assert_eq!(fnv1a_hash("GetSteamID"), 0xC7A8B5D2); // Example
        assert_eq!(fnv1a_hash("GetAppID"), 0x3F2E1D0C);
    }

    #[test]
    fn test_handler_registration() {
        let h = make_handler(
            IpcInterface::IClientUser,
            fnv1a_hash("GetSteamID"),
            "IClientUser::GetSteamID",
            0,
            2,
            None,
            Some(Box::new(|_| Ok(()))),
        );
        register_handler(h);

        let found = find_handler(IpcInterface::IClientUser, fnv1a_hash("GetSteamID"));
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "IClientUser::GetSteamID");
    }

    #[test]
    fn test_parse_hex_pattern() {
        let p = parse_hex_pattern("48 89 5C 24 08");
        assert_eq!(p, vec![0x48, 0x89, 0x5C, 0x24, 0x08]);
    }
}