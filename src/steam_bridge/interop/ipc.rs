//! Interop IPC handler layer for Linux.
//!
//! Port of usefulsteamtools `Interop_IPC.cpp`.
//! Provides handler registration and dispatch keyed by `(IpcInterface, FuncHash)`.

use std::collections::HashMap;
use std::fmt;
use std::os::raw::c_void;
use std::sync::{Arc, LazyLock, Mutex};

use crate::steam_bridge::memory_patch::JMP_ABS64_LEN;
use crate::steam_bridge::pattern_db::{BinaryComponent, PatternChannel};

/// Steam IPC interface IDs (matching `EIPCInterface`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum IpcInterface {
    #[default]
    IClientUser = 1,
    IClientUtils = 2,
    IClientNetworkingSocketsSerialized = 46,
}

impl From<u32> for IpcInterface {
    fn from(v: u32) -> Self {
        match v {
            1 => IpcInterface::IClientUser,
            2 => IpcInterface::IClientUtils,
            46 => IpcInterface::IClientNetworkingSocketsSerialized,
            _ => IpcInterface::IClientUser,
        }
    }
}

/// IPC command types (matching `EIPCCommand`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum IpcCommand {
    Handshake = 0,
    InterfaceCall = 1,
}

/// Function hash type (FNV-1a of method name).
pub type FuncHash = u32;

/// Cloneable IPC message handler callback.
pub type HandlerFn = Arc<dyn Fn(&mut IpcContext) -> Result<(), String> + Send + Sync + 'static>;

/// Resolved handler entry (pre and post hooks).
#[derive(Clone)]
pub struct ResolvedHandler {
    pub interface_id: IpcInterface,
    pub func_hash: FuncHash,
    pub name: String,
    pub fencepost: u32,
    pub argc: u32,
    pub pre_handler: Option<HandlerFn>,
    pub post_handler: Option<HandlerFn>,
}

impl fmt::Debug for ResolvedHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedHandler")
            .field("interface_id", &self.interface_id)
            .field("func_hash", &self.func_hash)
            .field("name", &self.name)
            .field("fencepost", &self.fencepost)
            .field("argc", &self.argc)
            .field("has_pre", &self.pre_handler.is_some())
            .field("has_post", &self.post_handler.is_some())
            .finish()
    }
}

/// IPC context passed to handlers.
#[derive(Debug)]
pub struct IpcContext {
    pub pipe: *mut c_void,
    pub read_buffer: Vec<u8>,
    pub write_buffer: Vec<u8>,
    pub interface_id: IpcInterface,
    pub func_hash: FuncHash,
    pub app_id: u32,
}

impl Default for IpcContext {
    fn default() -> Self {
        Self {
            pipe: std::ptr::null_mut(),
            read_buffer: Vec::new(),
            write_buffer: Vec::new(),
            interface_id: IpcInterface::default(),
            func_hash: 0,
            app_id: 0,
        }
    }
}

// Safety: `pipe` is only passed as an opaque handle.
unsafe impl Send for IpcContext {}
unsafe impl Sync for IpcContext {}

static HANDLERS: LazyLock<Mutex<HashMap<(IpcInterface, FuncHash), ResolvedHandler>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn handlers() -> &'static Mutex<HashMap<(IpcInterface, FuncHash), ResolvedHandler>> {
    &HANDLERS
}

/// Find handler by interface ID + func hash.
pub fn find_handler(iface: IpcInterface, func_hash: FuncHash) -> Option<ResolvedHandler> {
    handlers()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(iface, func_hash))
        .cloned()
}

/// Register a handler for `(interface_id, func_hash)`.
pub fn register_handler(handler: ResolvedHandler) {
    let key = (handler.interface_id, handler.func_hash);
    handlers()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, handler);
}

/// Register multiple handlers at once.
pub fn register_handlers(handlers_list: Vec<ResolvedHandler>) {
    let mut map = handlers().lock().unwrap_or_else(|e| e.into_inner());
    for h in handlers_list {
        map.insert((h.interface_id, h.func_hash), h);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcMessageType {
    Handshake = 0,
    InterfaceCall = 1,
}

/// Parse raw IPC message header from read buffer.
pub fn parse_ipc_message(data: &[u8]) -> Option<(IpcMessageType, IpcInterface, FuncHash)> {
    if data.len() < 12 {
        return None;
    }
    let cmd = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let iface_id = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    let func_hash = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);

    let msg_type = match cmd {
        0 => IpcMessageType::Handshake,
        1 => IpcMessageType::InterfaceCall,
        _ => return None,
    };

    Some((msg_type, IpcInterface::from(iface_id), func_hash))
}

/// Dispatch an IPC message to registered handlers.
pub fn dispatch_ipc_message(
    read_buf: &[u8],
    write_buf: &mut Vec<u8>,
    pipe_ptr: *mut c_void,
) -> bool {
    let Some((msg_type, iface_id, func_hash)) = parse_ipc_message(read_buf) else {
        return false;
    };

    if msg_type == IpcMessageType::Handshake {
        log::debug!("IPC Handshake received on pipe {:p}", pipe_ptr);
        return false;
    }

    let Some(handler) = find_handler(iface_id, func_hash) else {
        return false;
    };

    let mut ctx = IpcContext {
        pipe: pipe_ptr,
        read_buffer: read_buf.to_vec(),
        write_buffer: write_buf.clone(),
        interface_id: iface_id,
        func_hash,
        app_id: crate::steam_bridge::interop::interop_misc::resolve_app_id(),
    };

    if let Some(pre) = &handler.pre_handler {
        if let Err(e) = pre(&mut ctx) {
            log::warn!("Pre-handler error for {}: {}", handler.name, e);
        }
    }

    write_buf.clone_from(&ctx.write_buffer);

    if let Some(post) = &handler.post_handler {
        if let Err(e) = post(&mut ctx) {
            log::warn!("Post-handler error for {}: {}", handler.name, e);
        }
    }

    write_buf.clone_from(&ctx.write_buffer);
    true
}

/// Resolve a Steam IPC function address using `pattern_db`.
pub fn resolve_ipc_function(
    component: BinaryComponent,
    channel: PatternChannel,
    pattern_name: &str,
    steam_root: &std::path::Path,
) -> Result<u64, String> {
    use crate::steam_bridge::binary_reader::{find_loaded_module_mapping, scan_steam_binaries};
    use crate::steam_bridge::pattern_db::PatternDb;

    let binaries = scan_steam_binaries(steam_root);
    let binary = binaries
        .into_iter()
        .find(|b| {
            b.path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| match component {
                    BinaryComponent::SteamClient => n.contains("steamclient"),
                    BinaryComponent::SteamUi => n.contains("steamui"),
                })
                .unwrap_or(false)
        })
        .ok_or_else(|| "Steam binary not found".to_string())?;

    let pattern_db = PatternDb::new();
    let pf = pattern_db
        .lookup_by_binary(&binary, channel, component)
        .map_err(|e| format!("Pattern lookup failed: {}", e))?;

    let entry = pf
        .patterns
        .get(pattern_name)
        .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;

    let mod_name = binary
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| "Binary has no filename".to_string())?;
    let mapping = find_loaded_module_mapping(&mod_name)
        .ok_or_else(|| "Module not loaded in current process".to_string())?;

    let vaddr = if let Some(pattern_hex) = &entry.pattern {
        let pattern_bytes = parse_hex_pattern(pattern_hex);
        let mask_bytes = entry.mask.as_ref().map(|m| parse_hex_pattern(m));
        let offset = entry.offset.unwrap_or(0);
        binary.resolve_pattern_address(&pattern_bytes, mask_bytes.as_deref(), offset, None)
    } else {
        entry.address
    }
    .ok_or_else(|| "Failed to resolve pattern address".to_string())?;

    binary
        .vaddr_to_runtime_addr(vaddr, &mapping, JMP_ABS64_LEN)
        .ok_or_else(|| "Resolved address not in executable memory".to_string())
}

fn parse_hex_pattern(hex: &str) -> Vec<u8> {
    hex.split_whitespace()
        .filter_map(|s| u8::from_str_radix(s, 16).ok())
        .collect()
}

/// Helper to create a `ResolvedHandler` entry.
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

/// Const FNV-1a 32-bit hash of a method name (matching usefulsteamtools).
pub const fn fnv1a_hash(name: &str) -> u32 {
    let bytes = name.as_bytes();
    let mut hash: u32 = 0x811c_9dc5;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u32;
        hash = hash.wrapping_mul(0x0100_0193);
        i += 1;
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fnv1a_hash() {
        assert_eq!(fnv1a_hash(""), 0x811c_9dc5);
        assert_eq!(fnv1a_hash("GetSteamID"), fnv1a_hash("GetSteamID"));
        assert_ne!(fnv1a_hash("GetSteamID"), fnv1a_hash("GetAppID"));
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
            Some(Arc::new(|_| Ok(()))),
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
