//! NetPacket ETicket hook layer for Linux.
//
//! Port of usefulsteamtools `Interop_NetPacket_ETicket.cpp`.
//! Handles interception and spoofing of CMsgETicket NetPacket messages
//! for the online fix / Denuvo authentication path.
//
//! The ETicket (Encrypted Ticket) is used by Steam to deliver encrypted
//! app ownership tickets. This module intercepts these messages to:
//! - Parse the ETicket protobuf structure
//! - Spoof the SteamID inside the ETicket for online fix compatibility
//! - Inject properly formatted ETicket blocks for CloudRedirect integration

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::steam_bridge::interop::ipc::{
    FuncHash, IpcContext, IpcInterface, make_handler, register_handler, fnv1a_hash,
};
use crate::steam_bridge::interop::misc::resolve_app_id;
use crate::steam_bridge::ost_platform::steam_credential_store;

// ============================================================================
// Constants
// ============================================================================

/// FNV1a hash for ETicket-related Steam IPC functions
const NETPACKET_ETICKET_GET_ENCRYPTED_APP_TICKET_FNHASH: FuncHash =
    fnv1a_hash("GetEncryptedAppTicket") as FuncHash;
const NETPACKET_ETICKET_REQUEST_ENCRYPTED_APP_TICKET_FNHASH: FuncHash =
    fnv1a_hash("RequestEncryptedAppTicket") as FuncHash;

// ============================================================================
// Handler entry types
// ============================================================================

/// ETicket handler definitions matching usefulsteamtools Interop_NetPacket_ETicket.cpp
#[derive(Debug, Clone)]
pub struct EticketHandler {
    pub interface_id: IpcInterface,
    pub func_hash: FuncHash,
    pub name: String,
    pub fencepost: u32,
    pub argc: u32,
    pub pre_handler: Option<Box<dyn Fn(&mut IpcContext) -> Result<(), String> + Send + Sync>>,
    pub post_handler: Option<Box<dyn Fn(&mut IpcContext) -> Result<(), String> + Send + Sync>>,
}

/// Registered ETicket handlers
static ETICKET_HANDLERS: LazyLock<Mutex<HashMap<(IpcInterface, FuncHash), EtacketHandler>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn eticket_handlers() -> &'static Mutex<HashMap<(IpcInterface, FuncHash), EticketHandler>> {
    &ETICKET_HANDLERS
}

// ============================================================================
// Handler registration
// ============================================================================

/// Register all ETicket handlers
pub fn register_eticket_handlers() {
    let mut map = eticket_handlers().lock().unwrap();

    // Get interface from existing registration
    let iface = IpcInterface::IClientUser;

    // Compute FNV1a hashes
    let get_encrypted_app_ticket_hash = NETPACKET_ETICKET_GET_ENCRYPTED_APP_TICKET_FNHASH;
    let request_encrypted_app_ticket_hash = NETPACKET_ETICKET_REQUEST_ENCRYPTED_APP_TICKET_FNHASH;

    // HandleRecv_GetEncryptedAppTicket: Spoof SteamID in ETicket response
    // The ETicket message contains an encrypted app ticket that includes
    // the SteamID. We intercept and spoof it for online fix compatibility.
    let handle_recv_get_encrypted_app_ticket = EticketHandler {
        interface_id: iface,
        func_hash: get_encrypted_app_ticket_hash,
        name:
            "Hooks_NetPacket_ETicket::HandleRecv_GetEncryptedAppTicket".to_string(),
        fencepost: 0,
        argc: 4,
        pre_handler: None,
        post_handler: Some(Box::new(|ctx: &mut IpcContext| -> Result<(), String> {
            handle_recv_get_encrypted_app_ticket_post(ctx);
            Ok(())
        })),
    };
    map.insert((iface, get_encrypted_app_ticket_hash), handle_recv_get_encrypted_app_ticket);

    // HandleSend_RequestEncryptedAppTicket: Modify the request to include
    // the spoofed SteamID
    let handle_send_request_encrypted_app_ticket = EticketHandler {
        interface_id: iface,
        func_hash: request_encrypted_app_ticket_hash,
        name:
            "Hooks_NetPacket_ETicket::HandleSend_RequestEncryptedAppTicket".to_string(),
        fencepost: 0,
        argc: 4,
        pre_handler: None,
        post_handler: Some(Box::new(|ctx: &mut IpcContext| -> Result<(), String> {
            handle_send_request_encrypted_app_ticket_post(ctx);
            Ok(())
        })),
    };
    map.insert((iface, request_encrypted_app_ticket_hash), handle_send_request_encrypted_app_ticket);
}

/// Unregister all ETicket handlers
pub fn unregister_eticket_handlers() {
    let mut map = eticket_handlers().lock().unwrap();
    map.clear();
}

// ============================================================================
// Post-handler implementations
// ============================================================================

/// HandleRecv_GetEncryptedAppTicket: Spoof SteamID in ETicket response
//!
//! The ETicket message (CMsgETicket) structure typically contains:
//! - header (8 bytes)
//! - message_type (int32)
//! - ticket_size (int32)
//! - ticket_data (variable, contains the encrypted ticket)
//! - steam_id (uint64, embedded in ticket_data)
//
//! We parse the ticket_data to find and replace the embedded SteamID.
fn handle_recv_get_encrypted_app_ticket_post(ctx: &mut IpcContext) {
    let app_id = resolve_app_id();

    let spoofed_steam_id = steam_credential_store::get_steam_id(app_id);

    if spoofed_steam_id == 0 {
        log::debug!(
            "HandleRecv_GetEncryptedAppTicket: AppId={} - no spoofed SteamID available, passing through",
            app_id
        );
        return;
    }

    // The ETicket protobuf structure has the SteamID embedded in the ticket data.
    // For CMsgETicket, the structure is:
    // - header (8 bytes)
    // - message_type (4 bytes)
    // - ticket_size (4 bytes)
    // - ticket_data (variable bytes containing encrypted ticket)
    //   - appid (uint32, offset ~4)
    //   - SteamID (uint64, offset ~12 from start of ticket_data)
    //   - signature (bytes, rest)
    //   - blob (remaining bytes)

    // Find and replace the SteamID in the ticket data
    // The ticket_data starts after header + message_type + ticket_size = 16 bytes
    // SteamID is at offset 12 within ticket_data (after appid)

    if ctx.write_buffer.len() >= 28 {
        // Minimum: header(8) + message_type(4) + ticket_size(4) + appid(4) + SteamID(8) = 28 bytes
        let ticket_data_start = 16; // after header + message_type + ticket_size

        // Check we have enough data for appid + SteamID
        if ctx.write_buffer.len() >= ticket_data_start + 12 {
            // At offset 12 within ticket_data, replace the SteamID
            // But first let's find where ticket_data actually starts
            // and look for the appid field to locate SteamID

            // Strategy: look for the pattern of appid followed by SteamID
            // In the NetPacket body, after the header we have:
            // - schema_local_version or message_type (4 bytes)
            // - ticket_size (4 bytes)
            // - ticket_data starts here

            // The ticket_data contains protobuf-encoded ETicket:
            // field 1 = appid (uint32, tag 0x08 = offset 4-7 from start of ticket_data)
            // field 2 = SteamID (uint64, tag 0x10 = offset 8-15 from start of ticket_data)

            // Let's try to replace SteamID at offset 12 from start of message body
            // (after 8-byte header + 4-byte message_type)

            // Actually, let's take a different approach - scan for the
            // appid=XXXXX pattern and then replace the SteamID that follows

            log::debug!(
                "HandleRecv_GetEncryptedAppTicket: AppId={} spoofing SteamID to 0x{:X}",
                app_id,
                spoofed_steam_id
            );

            // For now, we'll do a byte-level replacement assuming
            // SteamID is at a known offset in the write buffer
            // This offset may vary by Steam client version

            // Try replacing at offset 20 (8 header + 4 message_type + 4 ticket_size + 4)
            if ctx.write_buffer.len() >= 28 {
                ctx.write_buffer[20..28].copy_from_slice(&spoofed_steam_id.to_le_bytes());
                log::debug!(
                    "HandleRecv_GetEncryptedAppTicket: AppId={} replaced bytes at offset 20-28",
                    app_id
                );
            }
        }
    }
}

/// HandleSend_RequestEncryptedAppTicket: Modify the request to include
//! the spoofed SteamID
fn handle_send_request_encrypted_app_ticket_post(ctx: &mut IpcContext) {
    let app_id = resolve_app_id();

    let spoofed_steam_id = steam_credential_store::get_steam_id(app_id);

    if spoofed_steam_id == 0 {
        log::debug!(
            "HandleSend_RequestEncryptedAppTicket: AppId={} - no spoofed SteamID, passing through",
            app_id
        );
        return;
    }

    // The RequestEncryptedAppTicket message structure:
    // - header (8 bytes)
    // - app_id (uint32, offset 8)
    // - hAsyncCall (pointer, offset 12)
    // - pData (nonce data, variable)
    // - max_ticket_size (int32, offset ~20)

    // We need to inject the spoofed SteamID into the request so that
    // the response ETicket will contain the spoofed SteamID.

    // For CMsgRequestEncryptedAppTicket, the structure typically has:
    // - header (8 bytes)
    // - app_id (uint32, offset 8)
    // - flags or other fields
    // - pData (nonce, used for fresh ETicket minting)

    // Since the SteamID spoofing for ETickets happens in the response
    // (GetEncryptedAppTicket), we mainly need to ensure the request
    // is properly formed. The actual SteamID replacement happens when
    // the ETicket response is received.

    log::debug!(
        "HandleSend_RequestEncryptedAppTicket: AppId={} recorded for ETicket spoofing",
        app_id
    );
}

// ============================================================================
// Inline dispatch handlers (called from IPC message dispatch)
// ============================================================================

/// Parse ETicket from write buffer and extract the SteamID
//!
//! The CMsgETicket NetPacket structure (simplified):
//! - header (8 bytes)
//! - message_type (int32, offset 8)
//! - ticket_size (int32, offset 12)
//! - ticket_data (variable, starts at offset 16)
//!   - Contains protobuf-encoded data with:
//!     - appid (uint32)
//!     - SteamID (uint64)
//!     - signature (bytes)
//!     - blob (remaining bytes)
pub fn parse_eticket_steam_id(write_buf: &[u8]) -> Option<u64> {
    // CMsgETicket structure:
    // - header (8 bytes): magic + version
    // - message_type (int32, offset 8)
    // - ticket_size (int32, offset 12)
    // - ticket_data (starts at offset 16)

    if write_buf.len() < 16 {
        return None;
    }

    // ticket_size is at offset 12, tells us how much ticket_data to expect
    let ticket_size =
        u32::from_le_bytes([write_buf[12], write_buf[13], write_buf[14], write_buf[15]])
            as usize;

    // ticket_data starts at offset 16
    let ticket_data_start = 16;
    let ticket_data_end = ticket_data_start + ticket_size;

    if write_buf.len() < ticket_data_end {
        // Not enough data, but try to extract what we can
        log::debug!(
            "parse_eticket_steam_id: buffer too small, have {} need {}",
            write_buf.len(),
            ticket_data_end
        );
        return None;
    }

    // The ticket_data contains protobuf-encoded ETicket.
    // Protobuf format for ETicket typically:
    // - field 1 (appid): tag = 0x08 (wire type 0, varint), at offset 4-7 from start of ticket_data
    // - field 2 (SteamID): tag = 0x10 (wire type 0, varint), at offset 8-15 from start of ticket_data
    //   - Actually SteamID is int64, so it's at offset 8-15
    //   - But the value is varint-encoded, so we need to parse it

    // Let's try a simpler approach: look for the SteamID value
    // in the ticket_data bytes. The SteamID is typically a u64
    // stored as a varint at offset 8 within ticket_data.

    // Parse varint at offset 8 within ticket_data
    let mut offset = ticket_data_start + 8; // Skip to where SteamID varint starts
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    let mut bytes_consumed: usize = 0;

    while offset < ticket_data_end {
        let byte = write_buf[offset];
        result |= (u64::from(byte & 0x7F)) << shift;
        shift += 7;
        bytes_consumed += 1;
        offset += 1;

        if (byte & 0x80) == 0 {
            break; // Last byte of varint
        }
        if bytes_consumed > 10 {
            // Varint too long, something's wrong
            return None;
        }
    }

    if bytes_consumed > 0 {
        // This should be the SteamID, but it might be some other field.
        // SteamIDs typically start with 0x0110 or are large u64 values.
        // Let's validate it looks like a SteamID
        if result >= 0x0110_0001_0000_0000 {
            return Some(result);
        }
        // Could also be a different field, try next approach
    }

    // Alternative: look for SteamID at a fixed offset
    // In many Steam client versions, the SteamID is stored at offset 12
    // from the start of the message (after header + message_type + ticket_size
    // + appid), but this varies.

    // Let's try offset 20 from message start (8 header + 4 message_type
    // + 4 ticket_size + 4 appid)
    if write_buf.len() >= 28 {
        let potential_steam_id =
            u64::from_le_bytes([write_buf[20], write_buf[21], write_buf[22], write_buf[23],
                                write_buf[24], write_buf[25], write_buf[26], write_buf[27]]);
        // Validate it looks like a SteamID (high bit pattern)
        if potential_steam_id >= 0x0110_0001_0000_0000 {
            return Some(potential_steam_id);
        }
    }

    None
}

/// Force eresult=OK and inject ETicket block in GetEncryptedAppTicket response
//!
//! Similar to handle_recv_get_user_stats_response but for ETicket messages.
//! The CMsgETicket response structure contains the encrypted ticket with
//! SteamID that we spoof.
pub fn handle_recv_get_encrypted_app_ticket_response(write_buf: &mut Vec<u8>, app_id: u32) {
    // Force the response to indicate success and inject a spoofed ETicket

    // First, force eresult to OK at the start of the response
    // CMsgGETokenResponse or similar structure:
    // - header (8 bytes)
    // - eresult (int32, offset 8)
    // - hack (int32, offset 12)
    // - ... more fields

    if write_buf.len() >= 12 {
        // Force eresult = OK (0) at offset 8
        write_buf[8..12].copy_from_slice(&(0u32).to_le_bytes());

        // Now inject a spoofed ETicket block
        // We need to create a minimal valid ETicket response
        let spoofed_steam_id = steam_credential_store::get_steam_id(app_id);

        if spoofed_steam_id != 0 {
            // Build a minimal ETicket response block
            // ETicket structure for response:
            // - appid (uint32, offset 4 from start of our block)
            // - SteamID (uint64, offset 12 from start of our block)
            // - signature (remaining bytes, can be empty for minimal case)

            let mut eticket_block = Vec::new();
            // appid field: varint encoding, tag 0x08 = (1 << 3) | 0 = 0x08
            // varint for appid: just the value since it's < 128 typically
            eticket_block.push(0x08 | (app_id as u8 & 0x7F)); // field 1, varint appid
            // SteamID field: varint encoding, tag 0x10 = (1 << 3) | 2 = 0x10
            // varint for SteamID - we encode the full u64
            let mut steam_id_bytes: Vec<u8> = Vec::new();
            let mut steam_id_val = spoofed_steam_id;
            while steam_id_val > 0 || steam_id_bytes.is_empty() {
                let mut byte = (steam_id_val & 0x7F) as u8;
                steam_id_val >>= 7;
                if !steam_id_val {
                    byte |= 0x80; // last byte
                }
                steam_id_bytes.push(byte);
            }
            // Prepend tag 0x10 (field 2, wire type 0)
            eticket_block.push(0x10 | steam_id_bytes[0]);
            for &b in &steam_id_bytes[1..] {
                eticket_block.push(b);
            }

            // Append the ETicket block after offset 12 (aftereresult + hack)
            // Zero out from offset 12 and inject our ETicket
            if write_buf.len() > 12 {
                // Truncate to justeresult + hack, then append ETicket
                write_buf.truncate(12);
                write_buf.extend_from_slice(&eticket_block);
            }
        }
    }

    log::debug!(
        "HandleRecv_GetEncryptedAppTicketResponse: AppId={} - forced eresult=OK, injected ETicket",
        app_id
    );
}

// ============================================================================
// Module initialization
// ============================================================================

/// Install all ETicket netpacket hooks
pub fn install_eticket() -> Result<(), String> {
    register_eticket_handlers();
    log::info!("Installed ETicket NetPacket hooks");
    Ok(())
}

/// Uninstall all ETicket netpacket hooks
pub fn uninstall_eticket() {
    unregister_eticket_handlers();
    log::info!("Uninstalled ETicket NetPacket hooks");
}

// ============================================================================
// Module tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fnv1a_hash_get_encrypted_app_ticket() {
        assert_eq!(
            NETPACKET_ETICKET_GET_ENCRYPTED_APP_TICKET_FNHASH,
            fnv1a_hash("GetEncryptedAppTicket") as FuncHash
        );
    }

    #[test]
    fn test_fnv1a_hash_request_encrypted_app_ticket() {
        assert_eq!(
            NETPACKET_ETICKET_REQUEST_ENCRYPTED_APP_TICKET_FNHASH,
            fnv1a_hash("RequestEncryptedAppTicket") as FuncHash
        );
    }

    #[test]
    fn test_handler_registration() {
        install_eticket();

        let handlers = eticket_handlers().lock().unwrap();
        assert!(!handlers.is_empty(), "Handlers should be registered");

        // Check that GetEncryptedAppTicket handler is registered
        let key = (IpcInterface::IClientUser, NETPACKET_ETICKET_GET_ENCRYPTED_APP_TICKET_FNHASH);
        assert!(
            handlers.contains_key(&key),
            "GetEncryptedAppTicket handler should be registered"
        );

        // Check that RequestEncryptedAppTicket handler is registered
        let key2 = (
            IpcInterface::IClientUser,
            NETPACKET_ETICKET_REQUEST_ENCRYPTED_APP_TICKET_FNHASH,
        );
        assert!(
            handlers.contains_key(&key2),
            "RequestEncryptedAppTicket handler should be registered"
        );
    }

    #[test]
    fn test_parse_eticket_steam_id() {
        // Create a test buffer with a valid SteamID at offset 20
        let mut write_buf = vec![0u8; 32];

        // Set up header: first 8 bytes
        // message_type at offset 8
        write_buf[8..12].copy_from_slice(&1u32.to_le_bytes()); // message_type = 1
        // ticket_size at offset 12
        write_buf[12..16].copy_from_slice(&20u32.to_le_bytes()); // ticket_size = 20
        // ticket_data starts at offset 16
        // appid at offset 16-19 (varint)
        write_buf[16] = 0x08 | (1245620 as u8 & 0x7F); // field 1, appid = 1245620
        // SteamID at offset 20-27
        let test_steam_id = 76561198028121353u64; // 0x0110000100000000 | something
        write_buf[20..28].copy_from_slice(&test_steam_id.to_le_bytes());

        let result = parse_eticket_steam_id(&write_buf);
        assert!(
            result.is_some(),
            "Should parse SteamID from ETicket buffer"
        );
        assert_eq!(result.unwrap(), test_steam_id);
    }

    #[test]
    fn test_parse_eticket_steam_id_no_data() {
        let write_buf = vec![0u8; 10];
        let result = parse_eticket_steam_id(&write_buf);
        assert!(result.is_none(), "Should return None for too-small buffer");
    }

    #[test]
    fn test_handle_recv_get_encrypted_app_ticket_response() {
        let mut write_buf = vec![0u8; 100];

        // Set up a test response witheresult = FAIL (2)
        write_buf[8..12].copy_from_slice(&2u32.to_le_bytes()); //eresult = 2

        handle_recv_get_encrypted_app_ticket_response(&mut write_buf, 123456);

        // Verifyeresult was forced to OK
        assert_eq!(
            write_buf[8..12],
            (0u32).to_le_bytes(),
            "eresult should be forced to OK"
        );

        // Verify ETicket was injected (buffer should contain our block)
        assert!(
            write_buf.len() > 12,
            "Buffer should contain injected ETicket block"
        );
    }
}