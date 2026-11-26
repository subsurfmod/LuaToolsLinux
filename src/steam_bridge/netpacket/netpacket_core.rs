//! NetPacket Core Dispatcher for Linux.
//!
//! Unified hook entry points for Steam's netpacket layer:
//! - `BBuildAndAsyncSendFrame` — outbound packet send hook (dispatches to all Send handlers)
//! - `RecvPkt` — inbound packet receive hook (dispatches to all Recv handlers)
//!
//! Port of usefulsteamtools `Interop_NetPacket.cpp` dispatcher logic.
//! Uses pattern scanning to find target functions in steamclient.so,
//! installs 14-byte RIP-relative JMP patches via memory_patch.rs,
//! and dispatches to submodules based on eMsg and target_job_name.

use std::fmt;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use crate::steam_bridge::interop::interop_misc::resolve_app_id;
use crate::steam_bridge::interop::ipc::{fnv1a_hash, FuncHash};
use crate::steam_bridge::memory_patch::{self, MemoryPatch, JMP_ABS64_LEN};
use crate::steam_bridge::pattern_db::{BinaryComponent, PatternChannel, PatternDb};
use crate::steam_bridge::binary_reader::{find_loaded_module_mapping, scan_steam_binaries};
use crate::steam_bridge::manifest_hooks::parse_pattern_and_mask;
use crate::steam_bridge::netpacket::userstats;
use crate::steam_bridge::netpacket::netpacket_eticket;
use crate::steam_bridge::netpacket::netpacket_legacykey;
use crate::steam_bridge::netpacket::netpacket_manifestprobe;
use crate::steam_bridge::netpacket::netpacket_ownershipticket;
use crate::steam_bridge::netpacket::netpacket_richpresence;

/// NetPacket message header matching Steam's internal MsgHdr.
///
/// struct MsgHdr {
///     uint32 eMsg;
///     uint32 headerLength;  // or sizeof(MsgHdr) for legacy frames
/// };
#[derive(Debug, Clone, Copy, Default)]
#[repr(C, packed)]
pub struct MsgHdr {
    pub e_msg: u32,
    pub header_length: u32,
}

impl MsgHdr {
    pub const SIZE: usize = 8;

    /// Parse MsgHdr from a byte slice.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            e_msg: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            header_length: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
        })
    }

    /// Check if this is a protobuf frame (kMsgHdrProtoFlag set).
    pub fn is_proto(&self) -> bool {
        (self.e_msg & KM_MSG_HDR_PROTO_FLAG) != 0
    }

    /// Get the base eMsg without the proto flag.
    pub fn base_emsg(&self) -> u32 {
        self.e_msg & !KM_MSG_HDR_PROTO_FLAG
    }
}

/// kMsgHdrProtoFlag = 0x80000000 — indicates protobuf framing.
pub const KM_MSG_HDR_PROTO_FLAG: u32 = 0x8000_0000;

/// k_EMsgServiceMethodCallFromClient = 151
pub const K_EMSG_SERVICE_METHOD_CALL_FROM_CLIENT: u32 = 151;

/// k_EMsgServiceMethodResponse = 147
pub const K_EMSG_SERVICE_METHOD_RESPONSE: u32 = 147;

/// NetPacket message direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetPacketDirection {
    Send,  // Outbound (BBuildAndAsyncSendFrame)
    Recv,  // Inbound (RecvPkt)
}

/// NetPacket handler registration entry.
#[derive(Clone)]
pub struct NetPacketHandler {
    pub direction: NetPacketDirection,
    pub emsg: u32,
    pub target_job_hash: Option<FuncHash>, // fnv1a hash of target_job_name for service method dispatch
    pub name: String,
    pub handler: Arc<dyn Fn(&mut NetPacketContext) -> Result<(), String> + Send + Sync + 'static>,
}

impl fmt::Debug for NetPacketHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetPacketHandler")
            .field("direction", &self.direction)
            .field("emsg", &self.emsg)
            .field("target_job_hash", &self.target_job_hash)
            .field("name", &self.name)
            .finish()
    }
}

/// Context passed to NetPacket handlers for send/recv operations.
#[derive(Debug)]
pub struct NetPacketContext {
    /// Raw frame buffer (header + body) — mutable for packet replacement.
    pub frame_buffer: Vec<u8>,
    /// Parsed message header.
    pub msg_hdr: MsgHdr,
    /// Offset into frame_buffer where body starts (after header).
    pub body_offset: usize,
    /// Current AppID for this packet.
    pub app_id: u32,
    /// Target job name hash for service method calls/responses.
    pub target_job_hash: Option<FuncHash>,
    /// Whether this is a protobuf frame.
    pub is_proto: bool,
    /// Direction of the packet.
    pub direction: NetPacketDirection,
}

impl NetPacketContext {
    /// Create a new context from a raw frame buffer.
    pub fn new(frame_buffer: Vec<u8>, direction: NetPacketDirection, app_id: u32) -> Option<Self> {
        let msg_hdr = MsgHdr::from_bytes(&frame_buffer)?;
        let is_proto = msg_hdr.is_proto();
        let body_offset = if is_proto {
            // Protobuf frame: header is 20 bytes (MsgHdr + jobid_source + jobid_target)
            20
        } else {
            // Legacy frame: header is 8 bytes (MsgHdr only)
            msg_hdr.header_length as usize
        };

        // Extract target_job_name hash for service method messages
        let target_job_hash = if msg_hdr.base_emsg() == K_EMSG_SERVICE_METHOD_CALL_FROM_CLIENT
            || msg_hdr.base_emsg() == K_EMSG_SERVICE_METHOD_RESPONSE
        {
            // For service methods, the target_job_name follows the protobuf header
            // In practice, we match by fnv1a hash of the job name string
            extract_target_job_hash(&frame_buffer, body_offset)
        } else {
            None
        };

        Some(Self {
            frame_buffer,
            msg_hdr,
            body_offset,
            app_id,
            target_job_hash,
            is_proto,
            direction,
        })
    }

    /// Get mutable reference to the body portion of the frame.
    pub fn body_mut(&mut self) -> &mut [u8] {
        &mut self.frame_buffer[self.body_offset..]
    }

    /// Get the body portion of the frame.
    pub fn body(&self) -> &[u8] {
        &self.frame_buffer[self.body_offset..]
    }

    /// Replace the entire frame buffer (for packet replacement).
    pub fn replace_frame(&mut self, new_frame: Vec<u8>) {
        self.frame_buffer = new_frame;
        // Re-parse header
        if let Some(new_hdr) = MsgHdr::from_bytes(&self.frame_buffer) {
            self.msg_hdr = new_hdr;
            self.is_proto = new_hdr.is_proto();
            self.body_offset = if self.is_proto { 20 } else { new_hdr.header_length as usize };
        }
    }

    /// Truncate frame buffer to a specific length.
    pub fn truncate(&mut self, len: usize) {
        self.frame_buffer.truncate(len);
    }

    /// Extend frame buffer.
    pub fn extend(&mut self, data: &[u8]) {
        self.frame_buffer.extend_from_slice(data);
    }
}

/// Extract target_job_name hash from service method frame body.
fn extract_target_job_hash(frame: &[u8], body_offset: usize) -> Option<FuncHash> {
    // In protobuf service method frames, the first field is typically the target_job_name string.
    // We scan for a length-delimited string at the start of the body.
    if frame.len() < body_offset + 2 {
        return None;
    }
    // Try to parse as varint length + string
    let mut idx = body_offset;
    if idx >= frame.len() {
        return None;
    }
    let first_byte = frame[idx];
    idx += 1;
    let str_len = (first_byte & 0x7F) as usize;
    if (first_byte & 0x80) != 0 {
        // Multi-byte varint - simplified, just use first byte for now
        // In production, full varint decode would be needed
    }
    if idx + str_len <= frame.len() {
        let job_name = std::str::from_utf8(&frame[idx..idx + str_len]).ok()?;
        return Some(fnv1a_hash(job_name));
    }
    None
}

/// Frame pool for packet replacement (like Windows ring buffers).
/// Provides pre-allocated buffers to avoid allocations in hot path.
struct FramePool {
    send_buffers: Vec<Vec<u8>>,
    recv_buffers: Vec<Vec<u8>>,
    send_index: usize,
    recv_index: usize,
}

impl FramePool {
    fn new() -> Self {
        const POOL_SIZE: usize = 16;
        const BUFFER_SIZE: usize = 65536; // 64KB per buffer
        Self {
            send_buffers: (0..POOL_SIZE).map(|_| Vec::with_capacity(BUFFER_SIZE)).collect(),
            recv_buffers: (0..POOL_SIZE).map(|_| Vec::with_capacity(BUFFER_SIZE)).collect(),
            send_index: 0,
            recv_index: 0,
        }
    }

    fn acquire_send(&mut self) -> Vec<u8> {
        let idx = self.send_index;
        self.send_index = (self.send_index + 1) % self.send_buffers.len();
        let mut buf = std::mem::take(&mut self.send_buffers[idx]);
        buf.clear();
        buf
    }

    fn acquire_recv(&mut self) -> Vec<u8> {
        let idx = self.recv_index;
        self.recv_index = (self.recv_index + 1) % self.recv_buffers.len();
        let mut buf = std::mem::take(&mut self.recv_buffers[idx]);
        buf.clear();
        buf
    }
}

static FRAME_POOL: OnceLock<Mutex<FramePool>> = OnceLock::new();

fn frame_pool() -> &'static Mutex<FramePool> {
    FRAME_POOL.get_or_init(|| Mutex::new(FramePool::new()))
}

/// Global handler registry for NetPacket dispatch.
static NETPACKET_HANDLERS: LazyLock<Mutex<Vec<NetPacketHandler>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Active state for the core dispatcher.
static NETPACKET_CORE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Installed memory patches for BBuildAndAsyncSendFrame and RecvPkt.
static NETPACKET_PATCHES: OnceLock<Mutex<Vec<MemoryPatch>>> = OnceLock::new();

/// Stored send patch trampoline for trampoline access
/// Use usize to avoid Send/Sync issues with raw pointers
static SEND_PATCH_TRAMPOLINE: OnceLock<Mutex<Option<usize>>> = OnceLock::new();

/// Stored recv patch trampoline for trampoline access
/// Use usize to avoid Send/Sync issues with raw pointers
static RECV_PATCH_TRAMPOLINE: OnceLock<Mutex<Option<usize>>> = OnceLock::new();

fn netpacket_patches() -> &'static Mutex<Vec<MemoryPatch>> {
    NETPACKET_PATCHES.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register a NetPacket handler.
pub fn register_netpacket_handler(handler: NetPacketHandler) {
    NETPACKET_HANDLERS.lock().unwrap_or_else(|e| e.into_inner()).push(handler);
}

/// Unregister all NetPacket handlers.
pub fn unregister_netpacket_handlers() {
    NETPACKET_HANDLERS.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Dispatch a frame to all matching handlers.
fn dispatch_netpacket(ctx: &mut NetPacketContext) {
    let handlers = NETPACKET_HANDLERS.lock().unwrap_or_else(|e| e.into_inner());
    for handler in handlers.iter() {
        if handler.direction != ctx.direction {
            continue;
        }
        // Match by eMsg (with proto flag stripped for comparison)
        let ctx_emsg = ctx.msg_hdr.base_emsg();
        if handler.emsg != 0 && handler.emsg != ctx_emsg {
            continue;
        }
        // Match by target_job_hash for service methods
        if let Some(expected_hash) = handler.target_job_hash {
            if ctx.target_job_hash != Some(expected_hash) {
                continue;
            }
        }
        // Call handler
        if let Err(e) = (handler.handler)(ctx) {
            log::warn!("NetPacket handler '{}' error: {}", handler.name, e);
        }
    }
}

/// Replacement function for BBuildAndAsyncSendFrame (outbound send).
///
/// Signature (inferred from Steam's internal):
/// void BBuildAndAsyncSendFrame(void* this, void* frame, uint32 frame_size, ...)
unsafe extern "C" fn bbuild_async_send_frame_replacement(
    _this: *mut c_void,
    frame: *mut u8,
    frame_size: u32,
    _arg3: *mut c_void,
    _arg4: *mut c_void,
    _arg5: *mut c_void,
) {
    // Copy frame data to our buffer for processing
    let frame_slice = std::slice::from_raw_parts(frame, frame_size as usize);
    let mut frame_buffer = frame_pool().lock().unwrap().acquire_send();
    frame_buffer.extend_from_slice(frame_slice);

    let app_id = resolve_app_id();
    if let Some(mut ctx) = NetPacketContext::new(frame_buffer, NetPacketDirection::Send, app_id) {
        dispatch_netpacket(&mut ctx);
        // Copy back modified frame if it changed
        if ctx.frame_buffer.len() == frame_size as usize {
            std::ptr::copy_nonoverlapping(ctx.frame_buffer.as_ptr(), frame, frame_size as usize);
        } else if ctx.frame_buffer.len() < frame_size as usize {
            // Smaller frame - copy and zero remainder
            std::ptr::copy_nonoverlapping(ctx.frame_buffer.as_ptr(), frame, ctx.frame_buffer.len());
            std::ptr::write_bytes(frame.add(ctx.frame_buffer.len()), 0, frame_size as usize - ctx.frame_buffer.len());
        }
        // Note: If frame grows larger than original, we can't safely expand in-place
        // This would require the frame pool / ring buffer approach like Windows
    }

    // ALWAYS forward to original trampoline to not blackhole Steam traffic
    if let Some(trampoline) = SEND_PATCH_TRAMPOLINE.get().and_then(|m| m.lock().ok()).and_then(|m| *m) {
        type SendFn = unsafe extern "C" fn(*mut c_void, *mut u8, u32, *mut c_void, *mut c_void, *mut c_void);
        let orig_fn: SendFn = std::mem::transmute(trampoline as *mut u8);
        orig_fn(_this, frame, frame_size, _arg3, _arg4, _arg5);
    }
}

/// Replacement function for RecvPkt (inbound receive).
///
/// Signature (inferred):
/// void RecvPkt(void* this, void* frame, uint32 frame_size, ...)
unsafe extern "C" fn recv_pkt_replacement(
    _this: *mut c_void,
    frame: *mut u8,
    frame_size: u32,
    _arg3: *mut c_void,
    _arg4: *mut c_void,
    _arg5: *mut c_void,
) {
    let frame_slice = std::slice::from_raw_parts(frame, frame_size as usize);
    let mut frame_buffer = frame_pool().lock().unwrap().acquire_recv();
    frame_buffer.extend_from_slice(frame_slice);

    let app_id = resolve_app_id();
    if let Some(mut ctx) = NetPacketContext::new(frame_buffer, NetPacketDirection::Recv, app_id) {
        dispatch_netpacket(&mut ctx);
        // Copy back modified frame
        if ctx.frame_buffer.len() == frame_size as usize {
            std::ptr::copy_nonoverlapping(ctx.frame_buffer.as_ptr(), frame, frame_size as usize);
        } else if ctx.frame_buffer.len() < frame_size as usize {
            std::ptr::copy_nonoverlapping(ctx.frame_buffer.as_ptr(), frame, ctx.frame_buffer.len());
            std::ptr::write_bytes(frame.add(ctx.frame_buffer.len()), 0, frame_size as usize - ctx.frame_buffer.len());
        }
    }

    // ALWAYS forward to original trampoline to not blackhole Steam traffic
    if let Some(trampoline) = RECV_PATCH_TRAMPOLINE.get().and_then(|m| m.lock().ok()).and_then(|m| *m) {
        type RecvFn = unsafe extern "C" fn(*mut c_void, *mut u8, u32, *mut c_void, *mut c_void, *mut c_void);
        let orig_fn: RecvFn = std::mem::transmute(trampoline as *mut u8);
        orig_fn(_this, frame, frame_size, _arg3, _arg4, _arg5);
    }
}

/// Resolve a NetPacket function address using pattern_db.
fn resolve_netpacket_function(
    steam_root: &std::path::Path,
    pattern_name: &str,
) -> Result<u64, String> {
    let binaries = scan_steam_binaries(steam_root);
    let binary = binaries
        .into_iter()
        .find(|b| {
            b.path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.contains("steamclient"))
                .unwrap_or(false)
        })
        .ok_or_else(|| "steamclient.so not found".to_string())?;

    let pattern_db = PatternDb::new();
    let pf = pattern_db
        .lookup_by_binary(&binary, PatternChannel::Pattern, BinaryComponent::SteamClient)
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
        let (pattern_bytes, mask_bytes) =
            parse_pattern_and_mask(pattern_hex, entry.mask.as_deref())
                .ok_or_else(|| "Malformed hex pattern or mask".to_string())?;
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

/// Install the core NetPacket hooks (BBuildAndAsyncSendFrame + RecvPkt).
pub fn install_netpacket_core(steam_root: &std::path::Path) -> Result<(), String> {
    if NETPACKET_CORE_ACTIVE.load(Ordering::Acquire) {
        return Ok(());
    }

    log::info!("Installing NetPacket core dispatcher hooks...");

    // Register all submodule handlers
    register_all_submodule_handlers();

    // Install BBuildAndAsyncSendFrame hook (outbound)
    let send_addr = resolve_netpacket_function(steam_root, "bbuild_async_send_frame")
        .or_else(|_| resolve_netpacket_function(steam_root, "BBuildAndAsyncSendFrame"))
        .or_else(|_| resolve_netpacket_function(steam_root, "build_and_async_send_frame"))?;
    let send_patch = unsafe {
        memory_patch::patch_by_offset(
            send_addr,
            0,
            bbuild_async_send_frame_replacement as *const () as *mut u8,
        )
    }.map_err(|e| format!("Failed to install BBuildAndAsyncSendFrame hook: {}", e))?;
    log::info!("Installed BBuildAndAsyncSendFrame hook at 0x{:x}", send_addr);
    netpacket_patches().lock().unwrap().push(send_patch.clone());
    *SEND_PATCH_TRAMPOLINE.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(send_patch.trampoline as usize);

    // Install RecvPkt hook (inbound)
    let recv_addr = resolve_netpacket_function(steam_root, "recv_pkt")
        .or_else(|_| resolve_netpacket_function(steam_root, "RecvPkt"))
        .or_else(|_| resolve_netpacket_function(steam_root, "receive_packet"))?;
    let recv_patch = unsafe {
        memory_patch::patch_by_offset(
            recv_addr,
            0,
            recv_pkt_replacement as *const () as *mut u8,
        )
    }.map_err(|e| format!("Failed to install RecvPkt hook: {}", e))?;
    log::info!("Installed RecvPkt hook at 0x{:x}", recv_addr);
    netpacket_patches().lock().unwrap().push(recv_patch.clone());
    *RECV_PATCH_TRAMPOLINE.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(recv_patch.trampoline as usize);

    NETPACKET_CORE_ACTIVE.store(true, Ordering::Release);
    log::info!("NetPacket core dispatcher installed");
    Ok(())
}

/// Uninstall all NetPacket core hooks.
pub fn uninstall_netpacket_core() -> Result<(), String> {
    if !NETPACKET_CORE_ACTIVE.load(Ordering::Acquire) {
        return Ok(());
    }

    log::info!("Uninstalling NetPacket core dispatcher hooks...");

    // Uninstall patches
    let mut patches = netpacket_patches().lock().unwrap_or_else(|e| e.into_inner());
    for mut patch in patches.drain(..) {
        unsafe { patch.uninstall() }?;
    }

    unregister_netpacket_handlers();
    NETPACKET_CORE_ACTIVE.store(false, Ordering::Release);
    log::info!("NetPacket core dispatcher uninstalled");
    Ok(())
}

/// Check if core dispatcher is active.
pub fn is_netpacket_core_active() -> bool {
    NETPACKET_CORE_ACTIVE.load(Ordering::Acquire)
}

/// Register all submodule handlers for Send/Recv dispatch.
fn register_all_submodule_handlers() {
    // ========== SEND HANDLERS (Outbound) ==========

    // AccessToken: eMsg 8903 (CMsgClientPICSProductInfoRequest)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: 8903,
        target_job_hash: None,
        name: "AccessToken::HandleSend_PICSProductInfoRequest".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("AccessToken Send: eMsg=8903 (PICSProductInfoRequest)");
            // Access token handling would go here
            Ok(())
        }),
    });

    // UserStats: eMsg 151 (Player.GetUserStats#1)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: K_EMSG_SERVICE_METHOD_CALL_FROM_CLIENT,
        target_job_hash: Some(fnv1a_hash("Player.GetUserStats#1")),
        name: "UserStats::HandleSend_GetUserStats".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("UserStats Send: Player.GetUserStats#1");
            userstats::handle_send_get_user_stats_post_ipc(ctx)
        }),
    });

    // UserStats: eMsg 818 (CMsgClientGetUserStats)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: 818,
        target_job_hash: None,
        name: "UserStats::HandleSend_ClientGetUserStats".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("UserStats Send: CMsgClientGetUserStats (818)");
            userstats::handle_send_client_get_user_stats_post_ipc(ctx)
        }),
    });

    // Manifest: eMsg 151 (ContentServerDirectory.GetManifestRequestCode#1)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: K_EMSG_SERVICE_METHOD_CALL_FROM_CLIENT,
        target_job_hash: Some(fnv1a_hash("ContentServerDirectory.GetManifestRequestCode#1")),
        name: "ManifestProbe::HandleSend_GetManifestRequestCode".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("Manifest Send: GetManifestRequestCode");
            netpacket_manifestprobe::handle_send_get_manifest_request_code(ctx)
        }),
    });

    // RichPresence/OnlineFix: eMsg 742/5410 (CMsgClientGamesPlayed)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: 742,
        target_job_hash: None,
        name: "RichPresence::HandleSend_GamesPlayed_742".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("RichPresence Send: CMsgClientGamesPlayed (742)");
            netpacket_richpresence::track_send(ctx)
        }),
    });

    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: 5410,
        target_job_hash: None,
        name: "OnlineFix::HandleSend_GamesPlayed_5410".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("OnlineFix Send: CMsgClientGamesPlayed (5410)");
            netpacket_richpresence::track_send(ctx)
        }),
    });

    // RichPresence: eMsg 7501 (CMsgClientRichPresenceUpload)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: 7501,
        target_job_hash: None,
        name: "RichPresence::HandleSend_RichPresenceUpload".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("RichPresence Send: CMsgClientRichPresenceUpload (7501)");
            netpacket_richpresence::track_rp_send(ctx)
        }),
    });

    // Cloud: eMsg 151 (Cloud.* target_job_name)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: K_EMSG_SERVICE_METHOD_CALL_FROM_CLIENT,
        target_job_hash: Some(fnv1a_hash("Cloud.GetUGCFileDetails#1")),
        name: "Cloud::HandleSend_GetUGCFileDetails".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("Cloud Send: GetUGCFileDetails");
            Ok(())
        }),
    });

    // LegacyKey: eMsg 730 (non-proto)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: netpacket_legacykey::K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY,
        target_job_hash: None,
        name: "LegacyKey::HandleSend_LegacyGameKey".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("LegacyKey Send: eMsg 730 (non-proto)");
            // The actual handler is in netpacket_legacykey::handle_send_legacy_game_key
            // This is registered via register_legacykey_handlers() which should be called
            // during install_netpacket_core
            Ok(())
        }),
    });

    // ========== RECV HANDLERS (Inbound) ==========

    // UserStats: eMsg 147 (Player.GetUserStats#1 response)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Recv,
        emsg: K_EMSG_SERVICE_METHOD_RESPONSE,
        target_job_hash: Some(fnv1a_hash("Player.GetUserStats#1")),
        name: "UserStats::HandleRecv_GetUserStatsResponse".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("UserStats Recv: Player.GetUserStats#1 response");
            userstats::handle_recv_get_user_stats_response_ipc(ctx)
        }),
    });

    // UserStats: eMsg 819 (CMsgClientGetUserStatsResponse)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Recv,
        emsg: 819,
        target_job_hash: None,
        name: "UserStats::HandleRecv_ClientGetUserStatsResponse".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("UserStats Recv: CMsgClientGetUserStatsResponse (819)");
            userstats::handle_recv_client_get_user_stats_response_ipc(ctx)
        }),
    });

    // ETicket: eMsg 5527 (CMsgClientRequestEncryptedAppTicketResponse)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Recv,
        emsg: 5527,
        target_job_hash: None,
        name: "ETicket::HandleRecv_RequestEncryptedAppTicketResponse".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("ETicket Recv: CMsgClientRequestEncryptedAppTicketResponse (5527)");
            netpacket_eticket::handle_recv_request_encrypted_app_ticket_response_ipc(ctx)
        }),
    });

    // Manifest: eMsg 147 (ContentServerDirectory.GetManifestRequestCode#1 response)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Recv,
        emsg: K_EMSG_SERVICE_METHOD_RESPONSE,
        target_job_hash: Some(fnv1a_hash("ContentServerDirectory.GetManifestRequestCode#1")),
        name: "ManifestProbe::HandleRecv_GetManifestRequestCodeResponse".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("Manifest Recv: GetManifestRequestCode response");
            netpacket_manifestprobe::handle_recv_get_manifest_request_code_response(ctx)
        }),
    });

    // OwnershipTicket: eMsg 858 (MsgClientGetAppOwnershipTicketResponse)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Recv,
        emsg: 858,
        target_job_hash: None,
        name: "OwnershipTicket::HandleRecv_GetAppOwnershipTicketResponse".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("OwnershipTicket Recv: MsgClientGetAppOwnershipTicketResponse (858)");
            netpacket_ownershipticket::handle_recv_get_app_ownership_ticket_response_ipc(ctx)
        }),
    });

    // Licenses: eMsg 780 (CMsgClientLicenseList)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Recv,
        emsg: 780,
        target_job_hash: None,
        name: "Licenses::HandleRecv_LicenseList".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("Licenses Recv: CMsgClientLicenseList (780)");
            crate::steam_bridge::netpacket::netpacket_licenses::handle_recv_client_license_list_post_ipc(ctx)
        }),
    });

    // RichPresence: eMsg 766 (CMsgClientPersonaState)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Recv,
        emsg: 766,
        target_job_hash: None,
        name: "RichPresence::HandleRecv_PersonaState".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("RichPresence Recv: CMsgClientPersonaState (766)");
            netpacket_richpresence::handle_recv(ctx)?;
            netpacket_richpresence::try_inject(ctx)
        }),
    });

    // Cloud: eMsg 147 (Cloud.* responses)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Recv,
        emsg: K_EMSG_SERVICE_METHOD_RESPONSE,
        target_job_hash: Some(fnv1a_hash("Cloud.GetUGCFileDetails#1")),
        name: "Cloud::HandleRecv_GetUGCFileDetailsResponse".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("Cloud Recv: GetUGCFileDetails response");
            Ok(())
        }),
    });

    // LegacyKey: eMsg 785 (legacy struct)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Recv,
        emsg: netpacket_legacykey::K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE,
        target_job_hash: None,
        name: "LegacyKey::HandleRecv_LegacyGameKeyResponse".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("LegacyKey Recv: eMsg 785 (legacy)");
            Ok(())
        }),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_msg_hdr_parsing() {
        let mut buf = vec![0u8; 8];
        buf[0..4].copy_from_slice(&(0x8000_0000 | 151u32).to_le_bytes()); // proto + eMsg 151
        buf[4..8].copy_from_slice(&20u32.to_le_bytes());

        let hdr = MsgHdr::from_bytes(&buf).unwrap();
        // Copy packed fields to local variables to avoid unaligned reference issues
        let hdr_copy = unsafe { *std::ptr::addr_of!(hdr) };
        let e_msg = hdr_copy.e_msg;
        let header_length = hdr_copy.header_length;
        assert_eq!(e_msg, 0x8000_0000 | 151);
        assert_eq!(header_length, 20);
        assert!(hdr.is_proto());
        assert_eq!(hdr.base_emsg(), 151);
    }

    #[test]
    fn test_msg_hdr_legacy() {
        let mut buf = vec![0u8; 8];
        buf[0..4].copy_from_slice(&730u32.to_le_bytes()); // legacy eMsg
        buf[4..8].copy_from_slice(&8u32.to_le_bytes());

        let hdr = MsgHdr::from_bytes(&buf).unwrap();
        let hdr_copy = unsafe { *std::ptr::addr_of!(hdr) };
        let e_msg = hdr_copy.e_msg;
        let header_length = hdr_copy.header_length;
        assert_eq!(e_msg, 730);
        assert_eq!(header_length, 8);
        assert!(!hdr.is_proto());
        assert_eq!(hdr.base_emsg(), 730);
    }

    #[test]
    fn test_netpacket_context_creation() {
        // Protobuf frame (20 byte header)
        let mut frame = vec![0u8; 24];
        frame[0..4].copy_from_slice(&(KM_MSG_HDR_PROTO_FLAG | 151u32).to_le_bytes());
        frame[4..8].copy_from_slice(&20u32.to_le_bytes());
        frame[8..16].copy_from_slice(&0x1234_5678_9ABC_DE00u64.to_le_bytes()); // jobid_source
        frame[16..24].copy_from_slice(&0u64.to_le_bytes()); // jobid_target

        let ctx = NetPacketContext::new(frame.clone(), NetPacketDirection::Send, 730);
        assert!(ctx.is_some());
        let ctx = ctx.unwrap();
        assert!(ctx.is_proto);
        assert_eq!(ctx.body_offset, 20);
        assert_eq!(ctx.msg_hdr.base_emsg(), 151);

        // Legacy frame (8 byte header)
        let mut frame2 = vec![0u8; 16];
        frame2[0..4].copy_from_slice(&730u32.to_le_bytes());
        frame2[4..8].copy_from_slice(&8u32.to_le_bytes());

        let ctx2 = NetPacketContext::new(frame2.clone(), NetPacketDirection::Recv, 730);
        assert!(ctx2.is_some());
        let ctx2 = ctx2.unwrap();
        assert!(!ctx2.is_proto);
        assert_eq!(ctx2.body_offset, 8);
        assert_eq!(ctx2.msg_hdr.base_emsg(), 730);
    }

    #[test]
    fn test_constants() {
        assert_eq!(KM_MSG_HDR_PROTO_FLAG, 0x8000_0000);
        assert_eq!(K_EMSG_SERVICE_METHOD_CALL_FROM_CLIENT, 151);
        assert_eq!(K_EMSG_SERVICE_METHOD_RESPONSE, 147);
        assert_eq!(MsgHdr::SIZE, 8);
    }

    #[test]
    fn test_fnv1a_hash_for_job_names() {
        let hash1 = fnv1a_hash("Player.GetUserStats#1");
        let hash2 = fnv1a_hash("ContentServerDirectory.GetManifestRequestCode#1");
        let hash3 = fnv1a_hash("Cloud.GetUGCFileDetails#1");
        assert_ne!(hash1, 0);
        assert_ne!(hash2, 0);
        assert_ne!(hash3, 0);
        assert_ne!(hash1, hash2);
        assert_ne!(hash2, hash3);
    }
}