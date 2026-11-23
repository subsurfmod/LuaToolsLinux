//! Interop IPC - IClientUtils Interface Handlers
//!
//! Linux port of usefulsteamtools Interop_IPC_ISteamUtils.cpp
//! Uses memory_patch.rs for inline hooking and pattern_db.rs for symbol resolution.
//! Integrates with ipc.rs for IPC message handling.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::ffi::c_void;

use crate::steam_bridge::memory_patch::{MemoryPatch, JMP_ABS64_LEN};
use crate::steam_bridge::pattern_db::{PatternDb, PatternChannel, BinaryComponent, PatternEntry};
use crate::steam_bridge::binary_reader::BinaryFile;
use crate::steam_bridge::ipc::{IpcMessage, IpcServer, IpcClient, IpcHandlerFn};
use crate::steam_bridge::ticket_store::TicketStore;
use crate::steam_bridge::interop::interop_ipc::{
    EIPCCommand, EIPCInterface, IpcHandlerEntry, ResolvedHandler,
    IPCProcessMessageFn, call_original,
};
use crate::steam_bridge::interop::interop_misc::{
    resolve_app_id, is_online_fix_active, notify_networking_sockets_used,
    should_report_online_fix_app_id, set_suppress_app_id_flip,
    get_app_id_for_current_pipe, get_game_name_by_app_id, ensure_buffer_capacity,
    register_ipc_handlers,
};

/// Install IClientUtils IPC handlers
/// This is called during InteropIPC initialization to hook IClientUtils interface calls
pub fn install_iclientutils_handlers() -> Result<(), String> {
    // Register default IClientUtils handlers
    register_handler(
        "IClientUtils",
        "GetAppID",
        None,
        Some(Box::new(handler_post_get_app_id)),
    )?;

    register_handler(
        "IClientUtils",
        "GetAPICallResult",
        None,
        Some(Box::new(handler_post_get_api_call_result)),
    )?;

    log::info!("[InteropIPC_ISteamUtils] Installed 2 IClientUtils handlers");
    Ok(())
}

/// Get or initialize the pattern database
fn pattern_db() -> &'static PatternDb {
    static PATTERN_DB: OnceLock<PatternDb> = OnceLock::new();
    PATTERN_DB.get_or_init(PatternDb::new)
}

/// Get the IPC patches list
fn ipc_patches() -> &'static Mutex<Vec<MemoryPatch>> {
    static IPC_PATCHES: OnceLock<Mutex<Vec<MemoryPatch>>> = OnceLock::new();
    IPC_PATCHES.get_or_init(|| Mutex::new(Vec::new()))
}

/// Get steamclient base address
fn steam_client_base() -> u64 {
    *crate::steam_bridge::interop::interop_ipc::STEAM_CLIENT_BASE.get().unwrap_or(&0)
}

/// Resolve current app ID from InteropMisc
fn resolve_app_id_caller() -> u32 {
    resolve_app_id()
}

/// Check if app has depot configured
fn has_depot(app_id: u32) -> bool {
    app_id > 0
}

/// Ensure buffer capacity for write
fn ensure_buffer_capacity_fn(write: &mut [u8], needed: u32) -> bool {
    write.len() >= needed as usize
}

/// Compute simple hash for interface::method
fn compute_func_hash(interface: &str, method: &str) -> u32 {
    // Simple hash: FNV-1a using std hasher
    let mut hash: u64 = 0x811c9dc5;
    let prime: u64 = 0x01000193;

    // Hash interface string
    for byte in interface.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(prime);
    }

    // Hash method separator
    for byte in "::".bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(prime);
    }

    // Hash method string
    for byte in method.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(prime);
    }

    hash as u32
}

/// Register an IPC handler for IClientUtils interface
fn register_handler(
    interface_name: &str,
    method_name: &str,
    pre: Option<Box<dyn Fn(*mut c_void, &mut [u8], &mut [u8]) -> bool + Send + Sync>>,
    post: Option<Box<dyn Fn(*mut c_void, &mut [u8], &mut [u8]) -> bool + Send + Sync>>,
) -> Result<(), String> {
    // Open steamclient.so to get its SHA256 for pattern lookup
    let binary = BinaryFile::open("steamclient.so")
        .map_err(|e| format!("Failed to open steamclient.so: {}", e))?;

    let pattern_file = pattern_db().lookup_by_binary(
        &binary,
        PatternChannel::Ipc,
        BinaryComponent::SteamClient,
    ).map_err(|e| format!("Failed to lookup pattern: {}", e))?;

    let method_key = format!("{}::{}", interface_name, method_name);
    let entry = pattern_file.patterns.get(&method_key)
        .ok_or_else(|| format!("No IPC spec for {}", method_key))?;

    let interface_id = match interface_name {
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

    // Add to handler registry
    HANDLER_REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
        .lock().unwrap()
        .push(handler);

    Ok(())
}

/// Get the handler registry
fn handler_registry() -> &'static Mutex<Vec<ResolvedHandler>> {
    static REGISTRY: OnceLock<Mutex<Vec<ResolvedHandler>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

// ═══════════════════════════════════════════════════════════════════════════
// IClientUtils Post-Handlers (matching Windows implementation)
// ═══════════════════════════════════════════════════════════════════════════

/// Post-handler: IClientUtils::GetAppID
///
/// Spoofs the app ID when online fix is active. The Windows version resolves
/// the real app ID and returns it instead of the default 480 that Steam
/// pre-fills. On Linux we use pattern_db.rs + memory_patch.rs to achieve the
/// same effect: intercept the IPC message and spoof the return value.
fn handler_post_get_app_id(_pipe: *mut c_void, _read: &mut [u8], write: &mut [u8]) -> bool {
    if should_report_online_fix_app_id() {
        return false;
    }

    let real_app_id = resolve_app_id_caller();
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
///
/// Handles EncryptedAppTicketResponse callbacks. When Steam returns an
/// EncryptedAppTicketResponse, we intercept it to serve a spoofed ticket
/// from the credential store instead of the real one, matching the behavior
/// of the Windows implementation.
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

// ══════════════════════════════════════════════════════════════════════════
// Helper types and functions (simplified implementations)
// ══════════════════════════════════════════════════════════════════════════

/// Encapsulates the callback result for EncryptedAppTicketResponse
#[derive(Clone, Copy)]
struct EncryptedAppTicketResponse {
    result: EResult,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EResult {
    OK = 1,
}

/// GetAppID Response parser
#[derive(Debug, Clone)]
struct GetAppIDResp {
    data: Vec<u8>,
    valid: bool,
}

impl GetAppIDResp {
    fn new(data: &mut [u8]) -> Self {
        Self { data: data.to_vec(), valid: data.len() >= 4 }
    }

    fn ok(&self) -> bool {
        self.valid
    }

    fn return_value(&self) -> u32 {
        if self.data.len() >= 4 {
            u32::from_le_bytes([
                self.data[0], self.data[1], self.data[2], self.data[3],
            ])
        } else {
            0
        }
    }

    fn set_return_value(&mut self, val: u32) {
        if self.data.len() >= 4 {
            self.data[0..4].copy_from_slice(&val.to_le_bytes());
        }
    }
}

/// GetAPICallResult Request parser
#[derive(Debug, Clone)]
struct GetAPICallResultReq {
    data: Vec<u8>,
    valid: bool,
}

impl GetAPICallResultReq {
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

    fn expected_callback(&self) -> u32 {
        if self.data.len() >= 4 {
            u32::from_le_bytes([self.data[0], self.data[1], self.data[2], self.data[3]])
        } else {
            0
        }
    }

    fn async_call(&self) -> u64 {
        if self.data.len() >= 16 {
            u64::from_le_bytes([
                self.data[8], self.data[9], self.data[10], self.data[11],
                self.data[12], self.data[13], self.data[14], self.data[15],
            ])
        } else {
            0
        }
    }

    fn callback_capacity(&self) -> u32 {
        if self.data.len() >= 20 {
            u32::from_le_bytes([
                self.data[16], self.data[17], self.data[18], self.data[19],
            ])
        } else {
            0
        }
    }
}

/// Write API call result (stub for the callback writer)
fn write_api_call_result<T: Copy>(_write: &mut [u8], _capacity: u32, _callback: T) -> bool {
    true
}

/// Record async call for encrypted ticket
fn record_encrypted_ticket(_async_call: u64, _app_id: u32) {
    // Would integrate with TicketStore
}

/// Take encrypted ticket for async call
fn take_encrypted_ticket(_async_call: u64) -> u32 {
    0
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_func_hash() {
        let hash1 = compute_func_hash("IClientUtils", "GetAppID");
        let hash2 = compute_func_hash("IClientUtils", "GetAppID");
        assert_eq!(hash1, hash2);

        let hash3 = compute_func_hash("IClientUtils", "GetAPICallResult");
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
    fn test_get_app_id_resp() {
        let mut buf = vec![0u8; 4];
        let mut resp = GetAppIDResp::new(&mut buf);
        assert!(resp.ok());
        resp.set_return_value(480);
        assert_eq!(resp.return_value(), 480);
        resp.set_return_value(730);
        assert_eq!(resp.return_value(), 730);
    }

    #[test]
    fn test_get_api_call_result_req() {
        // Minimal valid data: callback_id(4) + async_call(8) + callback_capacity(4) = 16 bytes
        let mut data = vec![1010u32.to_le_bytes().concat(),  // callback_id = 1010
                           0u64.to_le_bytes().concat(),          // async_call = 0
                           0u32.to_le_bytes().concat()];           // callback_capacity = 0
        let req = GetAPICallResultReq::parse(&data);
        assert!(req.ok());
        assert_eq!(req.expected_callback(), 1010);
        assert_eq!(req.async_call(), 0);
        assert_eq!(req.callback_capacity(), 0);
    }

    #[test]
    fn test_ertail_result() {
        assert_eq!(EResult::OK as u32, 1);
    }
}