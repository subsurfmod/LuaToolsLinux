//! NetPacket LegacyKey hook layer for Linux.
//!
//! Port of usefulsteamtools `Hooks_NetPacket_LegacyKey` namespace in Interop_NetPacket.cpp (lines 1986+).
//!
//! Handles legacy third-party CD key ("Updating product key") flow:
//! - **Outgoing**: `MsgClientGetLegacyGameKey` (eMsg 730) - non-proto STRUCT message
//!   - Catch outbound request before proto SendJob/RecvJob path
//!   - Answer locally with resolved CD key
//!   - Suppress real send
//! - **RecvPkt**: Deliver synthesized `MsgClientGetLegacyGameKeyResponse` (eMsg 785) via carrier packet
//!
//! Key structures:
//! - `g_pending`: deque of ready-to-inject 785 struct frames
//! - Varint/struct parsing for non-proto messages
//!
//! Integration:
//! - NetPacket core dispatcher for carrier injection
//! - Credential store for CD key resolution

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

#[allow(unused_imports)]
use crate::steam_bridge::credentials::legacycdkey;
use crate::steam_bridge::netpacket::netpacket_core::{
    NetPacketContext, NetPacketDirection, NetPacketHandler, register_netpacket_handler,
};

// Re-export for tests
#[allow(unused_imports)]
use crate::steam_bridge::credentials::legacycdkey::synthesize;

/// k_EMsgClientGetLegacyGameKey = 730 (non-proto STRUCT message)
pub const K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY: u32 = 730;

/// k_EMsgClientGetLegacyGameKeyResponse = 785 (non-proto STRUCT message)
pub const K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE: u32 = 785;

/// ExtendedMsgHdr size (matches Steam's ExtendedMsgHdr)
const EXTENDED_MSG_HDR_SIZE: usize = 40;

/// MsgClientGetLegacyGameKey size (uint32 m_unAppId)
const MSG_CLIENT_GET_LEGACY_GAME_KEY_SIZE: usize = 4;

/// MsgClientGetLegacyGameKeyResponse size (uint32 m_unAppId, EResult m_eResult, uint32 m_cchKey)
const MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE_SIZE: usize = 12;

/// Max packet size for safety
const MAX_PACKET_SIZE: usize = 65536;

/// Pending response queue for synthesized 785 responses.
static PENDING_RESPONSES: LazyLock<Mutex<VecDeque<Vec<u8>>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));

/// Active state for LegacyKey hooks.
static LEGACYKEY_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Handle outbound MsgClientGetLegacyGameKey (eMsg 730).
///
/// Returns `true` if the request was answered locally and should be suppressed.
/// Returns `false` to let the normal send path proceed.
pub fn handle_send_legacy_game_key(ctx: &mut NetPacketContext) -> Result<bool, String> {
    // Verify this is a non-proto frame
    if ctx.is_proto {
        return Ok(false);
    }

    // Verify minimum frame size: ExtendedMsgHdr + MsgClientGetLegacyGameKey
    if ctx.frame_buffer.len() < EXTENDED_MSG_HDR_SIZE + MSG_CLIENT_GET_LEGACY_GAME_KEY_SIZE {
        return Ok(false);
    }

    // Parse ExtendedMsgHdr from frame
    let ext_hdr = parse_extended_msg_hdr(&ctx.frame_buffer[..EXTENDED_MSG_HDR_SIZE])?;

    // Parse MsgClientGetLegacyGameKey body
    let body_offset = EXTENDED_MSG_HDR_SIZE;
    if ctx.frame_buffer.len() < body_offset + MSG_CLIENT_GET_LEGACY_GAME_KEY_SIZE {
        return Ok(false);
    }
    let app_id = u32::from_le_bytes([
        ctx.frame_buffer[body_offset],
        ctx.frame_buffer[body_offset + 1],
        ctx.frame_buffer[body_offset + 2],
        ctx.frame_buffer[body_offset + 3],
    ]);

    // Extract account ID from steam ID in extended header (lower 32 bits)
    let account_id = (ext_hdr.steam_id & 0xFFFF_FFFF) as u32;

    // Resolve the legacy CD key using the credential store
    let mut key_store = crate::steam_bridge::credentials::legacycdkey::LegacyCdKeyStore::default();
    let key_result = crate::steam_bridge::credentials::legacycdkey::resolve(&mut key_store, app_id, account_id);

    let key = match key_result {
        Ok(Some(k)) => k,
        Ok(None) => {
            // No key resolved - not a managed app, let Steam handle it
            return Ok(false);
        }
        Err(e) => {
            log::warn!("LegacyKey: failed to resolve CD key for app {}: {}", app_id, e);
            return Ok(false);
        }
    };

    // Build the synthesized response frame
    // Steam stores the legacy key as a NUL-terminated string; length counts the terminator
    let cch_key = key.len() + 1; // +1 for NUL terminator
    let total_size = EXTENDED_MSG_HDR_SIZE + MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE_SIZE + cch_key;

    if total_size > MAX_PACKET_SIZE {
        log::warn!(
            "LegacyKey: app={} response too large ({} bytes), passing through",
            app_id, total_size
        );
        return Ok(false);
    }

    let mut response_frame = Vec::with_capacity(total_size);

    // Copy the request's ExtendedMsgHdr as template
    response_frame.extend_from_slice(&ctx.frame_buffer[..EXTENDED_MSG_HDR_SIZE]);

    // Modify the header for response
    // eMsg = 785 (non-proto, flag stays clear)
    response_frame[0..4].copy_from_slice(&K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE.to_le_bytes());
    // m_JobIDTarget = request's m_JobIDSource (correlate response to request)
    response_frame[8..16].copy_from_slice(&ext_hdr.job_id_source.to_le_bytes());
    // m_JobIDSource = k_GIDNil (0)
    response_frame[16..24].copy_from_slice(&0u64.to_le_bytes());

    // Build MsgClientGetLegacyGameKeyResponse
    // m_unAppId
    response_frame.extend_from_slice(&app_id.to_le_bytes());
    // m_eResult = k_EResultOK (1)
    response_frame.extend_from_slice(&1u32.to_le_bytes());
    // m_cchKey
    response_frame.extend_from_slice(&(cch_key as u32).to_le_bytes());

    // Append the CD key payload (including NUL terminator)
    response_frame.extend_from_slice(key.as_bytes());
    response_frame.push(0); // NUL terminator

    // Queue the response for delivery from RecvPkt hook
    {
        let mut pending = PENDING_RESPONSES.lock().unwrap_or_else(|e| e.into_inner());
        if pending.len() < 64 {
            pending.push_back(response_frame);
        }
    }

    log::debug!(
        "LegacyKey: app={} answered locally ({} bytes, key='{}')",
        app_id, total_size, key
    );

    // Return true to suppress the real send
    Ok(true)
}

/// Drain pending responses by injecting them into the carrier packet.
///
/// This is called from the RecvPkt hook context. It borrows the carrier packet
/// for one oRecvPkt call per response — identical to Hooks_NetPacket_Cloud::Drain.
pub fn drain_pending_responses<F>(ctx: &mut NetPacketContext, mut invoke_original: F) -> Result<(), String>
where
    F: FnMut(&mut NetPacketContext) -> Result<(), String>,
{
    loop {
        let response_frame = {
            let mut pending = PENDING_RESPONSES.lock().unwrap_or_else(|e| e.into_inner());
            if pending.is_empty() {
                return Ok(());
            }
            pending.pop_front()
        };

        if let Some(frame) = response_frame {
            log::debug!("LegacyKey: delivering {}-byte response", frame.len());

            // Save original frame
            let original_frame = ctx.frame_buffer.clone();
            let original_hdr = ctx.msg_hdr;
            let original_body_offset = ctx.body_offset;
            let original_is_proto = ctx.is_proto;

            // Replace with our synthesized frame
            ctx.frame_buffer = frame;

            // Re-parse the new frame header
            if let Some(new_hdr) = crate::steam_bridge::netpacket::netpacket_core::MsgHdr::from_bytes(&ctx.frame_buffer) {
                ctx.msg_hdr = new_hdr;
                ctx.is_proto = new_hdr.is_proto();
                ctx.body_offset = if ctx.is_proto { 20 } else { new_hdr.header_length as usize };
            }

            // Invoke the original RecvPkt with our frame
            invoke_original(ctx)?;

            // Restore original frame
            ctx.frame_buffer = original_frame;
            ctx.msg_hdr = original_hdr;
            ctx.body_offset = original_body_offset;
            ctx.is_proto = original_is_proto;
        }
    }
}

/// Handle inbound MsgClientGetLegacyGameKeyResponse (eMsg 785).
///
/// This handler receives the actual network responses (if any). Our synthesized
/// responses are delivered via `drain_pending_responses` from the RecvPkt hook.
pub fn handle_recv_legacy_game_key_response(_ctx: &mut NetPacketContext) -> Result<(), String> {
    // Actual network responses would be handled here if needed.
    // Our synthesized responses bypass this by being injected directly in RecvPkt.
    Ok(())
}

/// Register LegacyKey NetPacket handlers with the core dispatcher.
pub fn register_legacykey_handlers() {
    // SEND: Handle outbound MsgClientGetLegacyGameKey (eMsg 730)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY,
        target_job_hash: None,
        name: "LegacyKey::HandleSend_LegacyGameKey".to_string(),
        handler: Arc::new(|ctx| {
            match handle_send_legacy_game_key(ctx) {
                Ok(suppress) => {
                    if suppress {
                        // Signal to core dispatcher to suppress the send
                        // We do this by clearing the frame buffer - the dispatcher
                        // will see empty frame and skip sending
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

    // RECV: Handle inbound MsgClientGetLegacyGameKeyResponse (eMsg 785)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Recv,
        emsg: K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE,
        target_job_hash: None,
        name: "LegacyKey::HandleRecv_LegacyGameKeyResponse".to_string(),
        handler: Arc::new(|ctx| {
            handle_recv_legacy_game_key_response(ctx)
        }),
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

    // Clear pending responses
    PENDING_RESPONSES.lock().unwrap_or_else(|e| e.into_inner()).clear();

    LEGACYKEY_ACTIVE.store(false, Ordering::Release);
    log::info!("LegacyKey NetPacket hooks uninstalled");
}

/// Check if LegacyKey hooks are active.
pub fn is_legacykey_hook_active() -> bool {
    LEGACYKEY_ACTIVE.load(Ordering::Acquire)
}

/// Parse ExtendedMsgHdr from bytes.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
struct ExtendedMsgHdr {
    e_msg: u32,
    header_version: u16,
    job_id_target: u64,
    job_id_source: u64,
    header_canary: u8,
    steam_id: u64,
    session_id: i32,
}

fn parse_extended_msg_hdr(data: &[u8]) -> Result<ExtendedMsgHdr, String> {
    if data.len() < EXTENDED_MSG_HDR_SIZE {
        return Err("Buffer too small for ExtendedMsgHdr".to_string());
    }

    let e_msg = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let header_version = u16::from_le_bytes([data[5], data[6]]);
    let job_id_target = u64::from_le_bytes([
        data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
    ]);
    let job_id_source = u64::from_le_bytes([
        data[16], data[17], data[18], data[19], data[20], data[21], data[22], data[23],
    ]);
    let header_canary = data[24];
    let steam_id = u64::from_le_bytes([
        data[25], data[26], data[27], data[28], data[29], data[30], data[31], data[32],
    ]);
    let session_id = i32::from_le_bytes([data[33], data[34], data[35], data[36]]);

    Ok(ExtendedMsgHdr {
        e_msg,
        header_version,
        job_id_target,
        job_id_source,
        header_canary,
        steam_id,
        session_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        assert_eq!(K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY, 730);
        assert_eq!(K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE, 785);
        assert_eq!(EXTENDED_MSG_HDR_SIZE, 40);
        assert_eq!(MSG_CLIENT_GET_LEGACY_GAME_KEY_SIZE, 4);
        assert_eq!(MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE_SIZE, 12);
    }

    #[test]
    fn test_parse_extended_msg_hdr() {
        let mut buf = vec![0u8; EXTENDED_MSG_HDR_SIZE];
        buf[0..4].copy_from_slice(&730u32.to_le_bytes()); // eMsg
        buf[4] = 1; // m_nCubHdr
        buf[5..7].copy_from_slice(&1u16.to_le_bytes()); // m_nHdrVersion
        buf[8..16].copy_from_slice(&0x1234_5678_9ABC_DE00u64.to_le_bytes()); // m_JobIDTarget
        buf[16..24].copy_from_slice(&0x0FED_CBA9_8765_4321u64.to_le_bytes()); // m_JobIDSource
        buf[24] = 0xAA; // m_nHdrCanary
        buf[25..33].copy_from_slice(&0x0110_0001_0000_0000u64.to_le_bytes()); // m_ulSteamID
        buf[33..37].copy_from_slice(&42i32.to_le_bytes()); // m_nSessionID

        let hdr = parse_extended_msg_hdr(&buf).unwrap();
        assert_eq!(hdr.e_msg, 730);
        assert_eq!(hdr.header_version, 1);
        assert_eq!(hdr.job_id_target, 0x1234_5678_9ABC_DE00);
        assert_eq!(hdr.job_id_source, 0x0FED_CBA9_8765_4321);
        assert_eq!(hdr.header_canary, 0xAA);
        assert_eq!(hdr.steam_id, 0x0110_0001_0000_0000);
        assert_eq!(hdr.session_id, 42);
    }

    #[test]
    fn test_account_id_extraction() {
        let steam_id = 0x0110_0001_0000_0000u64;
        let account_id = (steam_id & 0xFFFF_FFFF) as u32;
        assert_eq!(account_id, 0x0000_0000); // Bottom 32 bits

        let steam_id2 = 0x0110_0001_01D6_C8C9u64; // Example: 76561198028121353
        let account_id2 = (steam_id2 & 0xFFFF_FFFF) as u32;
        assert_eq!(account_id2, 0x01D6_C8C9); // 30810313
    }

    #[test]
    fn test_synthesize_key() {
        let key1 = synthesize(1245620, 28121353);
        let key2 = synthesize(1245620, 28121353);
        assert_eq!(key1, key2);
        assert!(key1.contains('-'));
        assert_eq!(key1.len(), 19);

        let key3 = synthesize(1245620, 28121354);
        assert_ne!(key1, key3);
    }

    #[test]
    fn test_install_uninstall() {
        install_legacykey_hooks().unwrap();
        assert!(is_legacykey_hook_active());
        uninstall_legacykey_hooks();
        assert!(!is_legacykey_hook_active());
    }
}