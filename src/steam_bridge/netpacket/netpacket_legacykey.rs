//! NetPacket LegacyKey hook layer for Linux.
//!
//! Port of usefulsteamtools `Hooks_NetPacket_LegacyKey` namespace in Interop_NetPacket.cpp (lines 1986+).
//!
//! Handles legacy third-party CD key ("Updating product key") flow:
//! - **Outgoing**: `MsgClientGetLegacyGameKey` (eMsg 730) - non-proto STRUCT message
//!   - Intercept outbound request before proto SendJob/RecvJob path
//!   - Answer locally with resolved or synthesized CD key
//!   - Suppress real send to Steam CM
//! - **RecvPkt**: Deliver synthesized `MsgClientGetLegacyGameKeyResponse` (eMsg 785) via carrier packet borrowing
//!
//! Wire layout conforms to SteamKit2 / Steam language `#pragma pack(push, 1)`:
//! - `ExtendedMsgHdr`: 36 bytes (eMsg: 4, cub_hdr: 1, hdr_version: 2, job_id_target: 8, job_id_source: 8, canary: 1, steam_id: 8, session_id: 4)
//! - `MsgClientGetLegacyGameKey`: 4 bytes (m_unAppId: 4)
//! - `MsgClientGetLegacyGameKeyResponse`: 12 bytes (m_unAppId: 4, m_eResult: 4, m_cchKey: 4) + NUL-terminated key string

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

#[allow(unused_imports)]
use crate::steam_bridge::credentials::legacycdkey;
pub use crate::steam_bridge::credentials::legacycdkey::synthesize;
use crate::steam_bridge::netpacket::netpacket_core::{
    CNetPacket, MsgHdr, NetPacketContext, NetPacketDirection, NetPacketHandler,
    register_netpacket_handler,
};

/// k_EMsgClientGetLegacyGameKey = 730 (non-proto STRUCT message)
pub const K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY: u32 = 730;

/// k_EMsgClientGetLegacyGameKeyResponse = 785 (non-proto STRUCT message)
pub const K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE: u32 = 785;

/// ExtendedMsgHdr size (matches Steam's `#pragma pack(push, 1)` ExtendedMsgHdr: 36 bytes)
pub const EXTENDED_MSG_HDR_SIZE: usize = 36;

/// MsgClientGetLegacyGameKey size (uint32 m_unAppId: 4 bytes)
pub const MSG_CLIENT_GET_LEGACY_GAME_KEY_SIZE: usize = 4;

/// MsgClientGetLegacyGameKeyResponse size (uint32 m_unAppId, EResult m_eResult, uint32 m_cchKey: 12 bytes)
pub const MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE_SIZE: usize = 12;

/// Max packet size for safety
pub const MAX_PACKET_SIZE: usize = 65536;

/// ExtendedMsgHdr wire layout matching Steam's `#pragma pack(push, 1)` ExtendedMsgHdr.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtendedMsgHdr {
    pub e_msg: u32,
    pub cub_hdr: u8,
    pub hdr_version: u16,
    pub job_id_target: u64,
    pub job_id_source: u64,
    pub hdr_canary: u8,
    pub steam_id: u64,
    pub session_id: i32,
}

impl ExtendedMsgHdr {
    #[inline]
    pub fn e_msg(&self) -> u32 {
        self.e_msg
    }

    #[inline]
    pub fn cub_hdr(&self) -> u8 {
        self.cub_hdr
    }

    #[inline]
    pub fn hdr_version(&self) -> u16 {
        self.hdr_version
    }

    #[inline]
    pub fn job_id_target(&self) -> u64 {
        self.job_id_target
    }

    #[inline]
    pub fn job_id_source(&self) -> u64 {
        self.job_id_source
    }

    #[inline]
    pub fn hdr_canary(&self) -> u8 {
        self.hdr_canary
    }

    #[inline]
    pub fn steam_id(&self) -> u64 {
        self.steam_id
    }

    #[inline]
    pub fn session_id(&self) -> i32 {
        self.session_id
    }
}

/// Pending response queue for synthesized 785 responses.
static PENDING_RESPONSES: LazyLock<Mutex<VecDeque<Vec<u8>>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));

/// Active state for LegacyKey hooks.
static LEGACYKEY_ACTIVE: AtomicBool = AtomicBool::new(false);

/// In-memory override store for custom/configured legacy CD keys.
static KEY_REGISTRY: LazyLock<Mutex<HashMap<u32, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Set a legacy CD key override in memory for the given AppID.
pub fn set_legacy_cdkey(app_id: u32, key: impl Into<String>) {
    KEY_REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(app_id, key.into());
}

/// Get a legacy CD key override from memory for the given AppID.
pub fn get_legacy_cdkey(app_id: u32) -> Option<String> {
    KEY_REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&app_id)
        .cloned()
}

/// Clear in-memory legacy CD key overrides.
pub fn clear_legacy_cdkeys() {
    KEY_REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// Check if there are any pending responses ready to drain.
pub fn has_pending_responses() -> bool {
    !PENDING_RESPONSES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty()
}

/// Pop a pending response frame from the queue.
pub fn pop_pending_response() -> Option<Vec<u8>> {
    PENDING_RESPONSES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pop_front()
}

/// Push a synthesized response frame into the pending queue.
pub fn push_pending_response(pkt: Vec<u8>) {
    let mut pending = PENDING_RESPONSES.lock().unwrap_or_else(|e| e.into_inner());
    if pending.len() < 64 {
        pending.push_back(pkt);
    }
}

/// Clear all pending synthesized responses.
pub fn clear_pending_responses() {
    PENDING_RESPONSES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// Parse ExtendedMsgHdr from wire bytes (packed 36-byte header).
pub fn parse_extended_msg_hdr(data: &[u8]) -> Result<ExtendedMsgHdr, String> {
    if data.len() < EXTENDED_MSG_HDR_SIZE {
        return Err(format!(
            "Buffer too small for ExtendedMsgHdr: {} < {}",
            data.len(),
            EXTENDED_MSG_HDR_SIZE
        ));
    }

    let e_msg = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let cub_hdr = data[4];
    let hdr_version = u16::from_le_bytes([data[5], data[6]]);
    let job_id_target = u64::from_le_bytes([
        data[7], data[8], data[9], data[10], data[11], data[12], data[13], data[14],
    ]);
    let job_id_source = u64::from_le_bytes([
        data[15], data[16], data[17], data[18], data[19], data[20], data[21], data[22],
    ]);
    let hdr_canary = data[23];
    let steam_id = u64::from_le_bytes([
        data[24], data[25], data[26], data[27], data[28], data[29], data[30], data[31],
    ]);
    let session_id = i32::from_le_bytes([data[32], data[33], data[34], data[35]]);

    Ok(ExtendedMsgHdr {
        e_msg,
        cub_hdr,
        hdr_version,
        job_id_target,
        job_id_source,
        hdr_canary,
        steam_id,
        session_id,
    })
}

/// Handle outbound MsgClientGetLegacyGameKey (eMsg 730).
///
/// Returns `true` if the request was answered locally and should be suppressed.
/// Returns `false` to let the normal send path proceed.
pub fn handle_send_legacy_game_key(ctx: &mut NetPacketContext) -> Result<bool, String> {
    // Non-proto frame check
    if ctx.is_proto {
        return Ok(false);
    }

    // Minimum frame size: ExtendedMsgHdr (36) + MsgClientGetLegacyGameKey (4) = 40 bytes
    if ctx.frame_buffer.len() < EXTENDED_MSG_HDR_SIZE + MSG_CLIENT_GET_LEGACY_GAME_KEY_SIZE {
        return Ok(false);
    }

    // Parse ExtendedMsgHdr
    let ext_hdr = parse_extended_msg_hdr(&ctx.frame_buffer[..EXTENDED_MSG_HDR_SIZE])?;
    if (ext_hdr.e_msg & !crate::steam_bridge::netpacket::netpacket_core::KM_MSG_HDR_PROTO_FLAG)
        != K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY
    {
        return Ok(false);
    }

    // Parse AppID from request body
    let body_offset = EXTENDED_MSG_HDR_SIZE;
    let app_id = u32::from_le_bytes([
        ctx.frame_buffer[body_offset],
        ctx.frame_buffer[body_offset + 1],
        ctx.frame_buffer[body_offset + 2],
        ctx.frame_buffer[body_offset + 3],
    ]);

    // Extract account ID from steam ID in extended header (lower 32 bits)
    let account_id = (ext_hdr.steam_id & 0xFFFF_FFFF) as u32;

    // Resolve key: check memory override first, then credential store
    let key = if let Some(k) = get_legacy_cdkey(app_id) {
        k
    } else {
        let mut key_store = crate::steam_bridge::credentials::legacycdkey::LegacyCdKeyStore::default();
        match crate::steam_bridge::credentials::legacycdkey::resolve(&mut key_store, app_id, account_id) {
            Ok(Some(k)) => k,
            Ok(None) => return Ok(false),
            Err(e) => {
                log::warn!("LegacyKey: failed to resolve CD key for app {}: {}", app_id, e);
                return Ok(false);
            }
        }
    };

    // Calculate total size: 36 (hdr) + 12 (resp struct) + key string + 1 (NUL)
    let cch_key = key.len() + 1;
    let total_size = EXTENDED_MSG_HDR_SIZE + MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE_SIZE + cch_key;

    if total_size > MAX_PACKET_SIZE {
        log::warn!(
            "LegacyKey: app={} response too large ({} bytes), passing through",
            app_id,
            total_size
        );
        return Ok(false);
    }

    let mut response_frame = vec![0u8; total_size];

    // Copy request's ExtendedMsgHdr as base template (cub_hdr, canary, steam_id, session_id)
    response_frame[..EXTENDED_MSG_HDR_SIZE].copy_from_slice(&ctx.frame_buffer[..EXTENDED_MSG_HDR_SIZE]);

    // eMsg = 785 (k_EMsgClientGetLegacyGameKeyResponse, non-proto)
    response_frame[0..4].copy_from_slice(&K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE.to_le_bytes());

    // m_JobIDTarget = request's m_JobIDSource (correlate response to request)
    response_frame[7..15].copy_from_slice(&ext_hdr.job_id_source.to_le_bytes());

    // m_JobIDSource = k_GIDNil (0)
    response_frame[15..23].copy_from_slice(&0u64.to_le_bytes());

    // Build MsgClientGetLegacyGameKeyResponse at offset 36..48
    // m_unAppId
    response_frame[36..40].copy_from_slice(&app_id.to_le_bytes());
    // m_eResult = k_EResultOK (1)
    response_frame[40..44].copy_from_slice(&1u32.to_le_bytes());
    // m_cchKey
    response_frame[44..48].copy_from_slice(&(cch_key as u32).to_le_bytes());

    // Append CD key payload with NUL terminator at offset 48
    response_frame[48..48 + key.len()].copy_from_slice(key.as_bytes());
    response_frame[48 + key.len()] = 0;

    // Queue synthesized response
    push_pending_response(response_frame);

    log::debug!(
        "LegacyKey: app={} answered locally ({} bytes, key='{}')",
        app_id,
        total_size,
        key
    );

    Ok(true)
}

/// Drain pending responses into carrier packet NetPacketContext.
///
/// Borrows the carrier context for each queued response frame.
pub fn drain_pending_responses<F>(
    ctx: &mut NetPacketContext,
    mut invoke_original: F,
) -> Result<usize, String>
where
    F: FnMut(&mut NetPacketContext) -> Result<(), String>,
{
    let mut count = 0;
    while let Some(frame) = pop_pending_response() {
        log::debug!("LegacyKey: delivering {}-byte response", frame.len());

        let orig_frame = ctx.frame_buffer.clone();
        let orig_hdr = ctx.msg_hdr;
        let orig_offset = ctx.body_offset;
        let orig_proto = ctx.is_proto;

        ctx.frame_buffer = frame;
        if let Some(new_hdr) = MsgHdr::from_bytes(&ctx.frame_buffer) {
            ctx.msg_hdr = new_hdr;
            ctx.is_proto = new_hdr.is_proto();
            ctx.body_offset = if ctx.is_proto { 20 } else { EXTENDED_MSG_HDR_SIZE };
        }

        let res = invoke_original(ctx);

        ctx.frame_buffer = orig_frame;
        ctx.msg_hdr = orig_hdr;
        ctx.body_offset = orig_offset;
        ctx.is_proto = orig_proto;

        res?;
        count += 1;
    }
    Ok(count)
}

/// Raw pointer carrier draining matching Valve CNetPacket carrier borrow.
///
/// Port of `Hooks_NetPacket_LegacyKey::Drain` from `Hooks_NetPacket.cpp:2068-2076`.
pub unsafe fn drain_into_carrier(
    this_ptr: *mut std::ffi::c_void,
    net_pkt: *mut CNetPacket,
    orig_recv: unsafe extern "C" fn(*mut std::ffi::c_void, *mut CNetPacket),
) {
    if net_pkt.is_null() {
        return;
    }
    while let Some(pkt) = pop_pending_response() {
        let orig_data = (*net_pkt).pub_data;
        let orig_size = (*net_pkt).pub_cub_data;

        let mut pkt_buf = pkt;
        (*net_pkt).pub_data = pkt_buf.as_mut_ptr();
        (*net_pkt).pub_cub_data = pkt_buf.len() as u32;

        orig_recv(this_ptr, net_pkt);

        (*net_pkt).pub_data = orig_data;
        (*net_pkt).pub_cub_data = orig_size;

        log::debug!(
            "LegacyKey: delivered {}-byte synthesized 785 response via carrier",
            pkt_buf.len()
        );
    }
}

/// Parsed representation of a MsgClientGetLegacyGameKeyResponse wire frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLegacyGameKeyResponse {
    pub hdr: ExtendedMsgHdr,
    pub app_id: u32,
    pub e_result: u32,
    pub cch_key: u32,
    pub cd_key: String,
}

/// Parse a MsgClientGetLegacyGameKeyResponse wire frame.
pub fn parse_legacy_game_key_response(data: &[u8]) -> Result<ParsedLegacyGameKeyResponse, String> {
    if data.len() < EXTENDED_MSG_HDR_SIZE + MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE_SIZE {
        return Err("Buffer too small for MsgClientGetLegacyGameKeyResponse".to_string());
    }
    let hdr = parse_extended_msg_hdr(&data[..EXTENDED_MSG_HDR_SIZE])?;
    let body = &data[EXTENDED_MSG_HDR_SIZE..];
    let app_id = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
    let e_result = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
    let cch_key = u32::from_le_bytes([body[8], body[9], body[10], body[11]]) as usize;

    let key_data = &body[MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE_SIZE..];
    if key_data.len() < cch_key {
        return Err(format!("Key payload too short: {} < {}", key_data.len(), cch_key));
    }
    let key_bytes = if cch_key > 0 && key_data[cch_key - 1] == 0 {
        &key_data[..cch_key - 1]
    } else {
        &key_data[..cch_key]
    };
    let cd_key = String::from_utf8_lossy(key_bytes).into_owned();

    Ok(ParsedLegacyGameKeyResponse {
        hdr,
        app_id,
        e_result,
        cch_key: cch_key as u32,
        cd_key,
    })
}

/// Helper to construct a valid outbound MsgClientGetLegacyGameKey (eMsg 730) frame.
pub fn build_legacy_game_key_request_frame(
    app_id: u32,
    steam_id: u64,
    job_id_source: u64,
) -> Vec<u8> {
    let mut frame = vec![0u8; EXTENDED_MSG_HDR_SIZE + MSG_CLIENT_GET_LEGACY_GAME_KEY_SIZE];
    frame[0..4].copy_from_slice(&K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY.to_le_bytes());
    frame[4] = 32; // m_nCubHdr
    frame[5..7].copy_from_slice(&1u16.to_le_bytes()); // m_nHdrVersion
    frame[7..15].copy_from_slice(&0u64.to_le_bytes()); // m_JobIDTarget
    frame[15..23].copy_from_slice(&job_id_source.to_le_bytes()); // m_JobIDSource
    frame[23] = 0xAA; // m_nHdrCanary
    frame[24..32].copy_from_slice(&steam_id.to_le_bytes()); // m_ulSteamID
    frame[32..36].copy_from_slice(&1i32.to_le_bytes()); // m_nSessionID
    frame[36..40].copy_from_slice(&app_id.to_le_bytes()); // m_unAppId
    frame
}

/// Inbound handler for legacy game key response (eMsg 785) and carrier delivery.
pub fn handle_recv_legacy_game_key_response(ctx: &mut NetPacketContext) -> Result<(), String> {
    if has_pending_responses() {
        let mut injected: Option<Vec<u8>> = None;
        let _ = drain_pending_responses(ctx, |carrier| {
            if injected.is_none() {
                injected = Some(carrier.frame_buffer.clone());
            }
            Ok(())
        })?;
        if let Some(frame) = injected {
            ctx.replace_frame(frame);
            if !ctx.is_proto && ctx.frame_buffer.len() >= EXTENDED_MSG_HDR_SIZE {
                ctx.body_offset = EXTENDED_MSG_HDR_SIZE;
            }
        }
        return Ok(());
    }

    if ctx.frame_buffer.len() >= EXTENDED_MSG_HDR_SIZE + MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE_SIZE {
        if let Ok(parsed) = parse_legacy_game_key_response(&ctx.frame_buffer) {
            log::debug!(
                "LegacyKey: received eMsg 785 response for app={} eresult={} key='{}'",
                parsed.app_id,
                parsed.e_result,
                parsed.cd_key
            );
        }
    }
    Ok(())
}

/// Register LegacyKey NetPacket handlers with the core dispatcher.
pub fn register_legacykey_handlers() {
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY,
        target_job_hash: None,
        name: "LegacyKey::HandleSend_LegacyGameKey".to_string(),
        handler: Arc::new(|ctx| {
            match handle_send_legacy_game_key(ctx) {
                Ok(suppress) => {
                    if suppress {
                        ctx.frame_buffer.clear();
                    }
                    Ok(())
                }
                Err(e) => {
                    log::warn!("LegacyKey Send handler error: {}", e);
                    Ok(())
                }
            }
        }),
    });

    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Recv,
        emsg: K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE,
        target_job_hash: None,
        name: "LegacyKey::HandleRecv_LegacyGameKeyResponse".to_string(),
        handler: Arc::new(|ctx| handle_recv_legacy_game_key_response(ctx)),
    });
}

/// Install LegacyKey NetPacket hooks.
pub fn install_legacykey_hooks() -> Result<(), String> {
    if LEGACYKEY_ACTIVE.load(Ordering::Acquire) {
        return Ok(());
    }

    log::info!("Installing LegacyKey NetPacket hooks...");
    register_legacykey_handlers();
    LEGACYKEY_ACTIVE.store(true, Ordering::Release);
    log::info!("LegacyKey NetPacket hooks installed (eMsg 730/785)");
    Ok(())
}

/// Uninstall LegacyKey NetPacket hooks.
pub fn uninstall_legacykey_hooks() {
    if !LEGACYKEY_ACTIVE.load(Ordering::Acquire) {
        return;
    }

    log::info!("Uninstalling LegacyKey NetPacket hooks...");
    clear_pending_responses();
    clear_legacy_cdkeys();
    LEGACYKEY_ACTIVE.store(false, Ordering::Release);
    log::info!("LegacyKey NetPacket hooks uninstalled");
}

/// Check if LegacyKey hooks are active.
pub fn is_legacykey_hook_active() -> bool {
    LEGACYKEY_ACTIVE.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        assert_eq!(K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY, 730);
        assert_eq!(K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE, 785);
        assert_eq!(EXTENDED_MSG_HDR_SIZE, 36);
        assert_eq!(MSG_CLIENT_GET_LEGACY_GAME_KEY_SIZE, 4);
        assert_eq!(MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE_SIZE, 12);
        assert_eq!(
            std::mem::size_of::<ExtendedMsgHdr>(),
            EXTENDED_MSG_HDR_SIZE
        );
    }

    #[test]
    fn test_parse_extended_msg_hdr() {
        let mut buf = vec![0u8; EXTENDED_MSG_HDR_SIZE];
        buf[0..4].copy_from_slice(&730u32.to_le_bytes()); // eMsg
        buf[4] = 32; // m_nCubHdr
        buf[5..7].copy_from_slice(&1u16.to_le_bytes()); // m_nHdrVersion
        buf[7..15].copy_from_slice(&0x1234_5678_9ABC_DE00u64.to_le_bytes()); // m_JobIDTarget
        buf[15..23].copy_from_slice(&0x0FED_CBA9_8765_4321u64.to_le_bytes()); // m_JobIDSource
        buf[23] = 0xAA; // m_nHdrCanary
        buf[24..32].copy_from_slice(&0x0110_0001_0000_0000u64.to_le_bytes()); // m_ulSteamID
        buf[32..36].copy_from_slice(&42i32.to_le_bytes()); // m_nSessionID

        let hdr = parse_extended_msg_hdr(&buf).unwrap();
        assert_eq!(hdr.e_msg(), 730);
        assert_eq!(hdr.cub_hdr(), 32);
        assert_eq!(hdr.hdr_version(), 1);
        assert_eq!(hdr.job_id_target(), 0x1234_5678_9ABC_DE00);
        assert_eq!(hdr.job_id_source(), 0x0FED_CBA9_8765_4321);
        assert_eq!(hdr.hdr_canary(), 0xAA);
        assert_eq!(hdr.steam_id(), 0x0110_0001_0000_0000);
        assert_eq!(hdr.session_id(), 42);
    }

    #[test]
    fn test_legacy_key_synthesis_and_parsing() {
        let app_id = 1245620;
        let steam_id = 0x0110_0001_01D6_C8C9u64;
        let job_id_source = 0x9988_7766_5544_3322u64;

        set_legacy_cdkey(app_id, "TEST-ABCD-1234-EFGH");

        let req_frame = build_legacy_game_key_request_frame(app_id, steam_id, job_id_source);
        let mut ctx = NetPacketContext {
            frame_buffer: req_frame,
            msg_hdr: MsgHdr {
                e_msg: 730,
                header_length: 36,
            },
            body_offset: EXTENDED_MSG_HDR_SIZE,
            app_id,
            target_job_hash: None,
            is_proto: false,
            direction: NetPacketDirection::Send,
        };

        let handled = handle_send_legacy_game_key(&mut ctx).unwrap();
        assert!(handled);

        assert!(has_pending_responses());
        let resp_frame = pop_pending_response().unwrap();

        let parsed = parse_legacy_game_key_response(&resp_frame).unwrap();
        let resp_e_msg = parsed.hdr.e_msg();
        let resp_target = parsed.hdr.job_id_target();
        let resp_source = parsed.hdr.job_id_source();
        assert_eq!(resp_e_msg, 785);
        assert_eq!(resp_target, job_id_source);
        assert_eq!(resp_source, 0);
        assert_eq!(parsed.app_id, app_id);
        assert_eq!(parsed.e_result, 1); // k_EResultOK
        assert_eq!(parsed.cd_key, "TEST-ABCD-1234-EFGH");

        clear_legacy_cdkeys();
    }
}
