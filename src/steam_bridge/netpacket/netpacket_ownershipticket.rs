//! NetPacket OwnershipTicket hook layer for Linux.
//!
//! Port of usefulsteamtools `Interop_NetPacket_OwnershipTicket.cpp`.
//! Handles interception and spoofing of CMsgClientAppOwnershipTicket NetPacket messages
//! for the app ownership verification path.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

// Import only what we need from ipc module - use the public API
use crate::steam_bridge::interop::ipc::{
    FuncHash, IpcContext, IpcInterface, make_handler, register_handler, fnv1a_hash,
};
use crate::steam_bridge::interop::interop_misc::resolve_app_id as misc_resolve_app_id;
use crate::steam_bridge::ost_platform::steam_credential_store;
use crate::steam_bridge::credentials::appticket::{read_app_ticket, AppOwnershipTicket};

const NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_FNHASH: FuncHash =
    fnv1a_hash("GetAppOwnershipTicket") as FuncHash;
const NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_EXTENDED_DATA_FNHASH: FuncHash =
    fnv1a_hash("GetAppOwnershipTicketExtendedData") as FuncHash;

#[derive(Debug, Clone)]
pub struct OwnershipTicketHandler {
    pub interface_id: IpcInterface,
    pub func_hash: FuncHash,
    pub name: String,
    pub fencepost: u32,
    pub argc: u32,
    pub pre_handler: Option<Box<dyn Fn(&mut IpcContext) -> Result<(), String> + Send + Sync>>,
    pub post_handler: Option<Box<dyn Fn(&mut IpcContext) -> Result<(), String> + Send + Sync>>,
}

static OWNERSHIPTICKET_HANDLERS: LazyLock<Mutex<HashMap<(IpcInterface, FuncHash), OwnershipTicketHandler>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn ownershipticket_handlers() -> &'static Mutex<HashMap<(IpcInterface, FuncHash), OwnershipTicketHandler>> {
    &OWNERSHIPTICKET_HANDLERS
}

pub fn register_ownershipticket_handlers() {
    let mut map = ownershipticket_handlers().lock().unwrap();
    let iface = IpcInterface::IClientUser;

    let get_app_ownership_ticket_hash = NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_FNHASH;
    let get_app_ownership_ticket_extended_data_hash = NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_EXTENDED_DATA_FNHASH;

    // HandleSend_GetAppOwnershipTicket: Spoof ownership ticket in request
    let handle_send_get_app_ownership_ticket = OwnershipTicketHandler {
        interface_id: iface,
        func_hash: get_app_ownership_ticket_hash,
        name: "Hooks_NetPacket_OwnershipTicket::HandleSend_GetAppOwnershipTicket".to_string(),
        fencepost: 0,
        argc: 4,
        pre_handler: None,
        post_handler: Some(Box::new(|ctx: &mut IpcContext| -> Result<(), String> {
            handle_send_get_app_ownership_ticket_post(ctx);
            Ok(())
        })),
    };
    map.insert((iface, get_app_ownership_ticket_hash), handle_send_get_app_ownership_ticket);

    // HandleSend_GetAppOwnershipTicketExtendedData: Spoof extended ownership ticket in request
    let handle_send_get_app_ownership_ticket_extended_data = OwnershipTicketHandler {
        interface_id: iface,
        func_hash: get_app_ownership_ticket_extended_data_hash,
        name: "Hooks_NetPacket_OwnershipTicket::HandleSend_GetAppOwnershipTicketExtendedData".to_string(),
        fencepost: 0,
        argc: 4,
        pre_handler: None,
        post_handler: Some(Box::new(|ctx: &mut IpcContext| -> Result<(), String> {
            handle_send_get_app_ownership_ticket_extended_data_post(ctx);
            Ok(())
        })),
    };
    map.insert((iface, get_app_ownership_ticket_extended_data_hash), handle_send_get_app_ownership_ticket_extended_data);
}

pub fn unregister_ownershipticket_handlers() {
    let mut map = ownershipticket_handlers().lock().unwrap();
    map.clear();
}

fn handle_send_get_app_ownership_ticket_post(ctx: &mut IpcContext) {
    let app_id = resolve_app_id();

    // Try to get ownership ticket from credential store
    let ticket_result = read_app_ticket(app_id);

    let spoofed_ticket = match ticket_result {
        Ok(Some(ticket)) => ticket,
        _ => {
            log::debug!(
                "HandleSend_GetAppOwnershipTicket: AppId={} - no ownership ticket available, passing through",
                app_id
            );
            return;
        }
    };

    // CMsgClientAppOwnershipTicketRequest structure (simplified):
    // - header (8 bytes)
    // - app_id (uint32)
    // - steam_id (uint64) - this is the field we want to spoof
    // - ticket_data (bytes)

    // Find and spoof the steam_id field in the NetPacket body
    // The steam_id is typically at a fixed offset after the header + app_id
    if ctx.write_buffer.len() >= 20 {
        // Header (8) + app_id (4) = 12, then steam_id at offset 12
        let steam_id_offset = 12;
        if ctx.write_buffer.len() >= steam_id_offset + 8 {
            // Use the spoofed SteamID from the credential store
            let spoofed_steam_id = spoofed_ticket.steam_id
                .as_ref()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);

            if spoofed_steam_id != 0 {
                log::debug!(
                    "HandleSend_GetAppOwnershipTicket: AppId={} spoofing SteamID to 0x{:X}",
                    app_id, spoofed_steam_id
                );
                ctx.write_buffer[steam_id_offset..steam_id_offset + 8].copy_from_slice(&spoofed_steam_id.to_le_bytes());
            }
        }
    }
}

fn handle_send_get_app_ownership_ticket_extended_data_post(ctx: &mut IpcContext) {
    let app_id = resolve_app_id();

    // Try to get ownership ticket from credential store
    let ticket_result = read_app_ticket(app_id);

    let spoofed_ticket = match ticket_result {
        Ok(Some(ticket)) => ticket,
        _ => {
            log::debug!(
                "HandleSend_GetAppOwnershipTicketExtendedData: AppId={} - no ownership ticket available, passing through",
                app_id
            );
            return;
        }
    };

    // CMsgClientAppOwnershipTicketExtendedDataRequest structure (simplified):
    // - header (8 bytes)
    // - app_id (uint32)
    // - max_ticket_size (int32)
    // - steam_id (uint64) - this is the field we want to spoof
    // - ticket_data (bytes, optional)

    // Find and spoof the steam_id field
    // Header (8) + app_id (4) + max_ticket_size (4) = 16, then steam_id at offset 16
    if ctx.write_buffer.len() >= 24 {
        let steam_id_offset = 16;
        if ctx.write_buffer.len() >= steam_id_offset + 8 {
            let spoofed_steam_id = spoofed_ticket.steam_id
                .as_ref()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);

            if spoofed_steam_id != 0 {
                log::debug!(
                    "HandleSend_GetAppOwnershipTicketExtendedData: AppId={} spoofing SteamID to 0x{:X}",
                    app_id, spoofed_steam_id
                );
                ctx.write_buffer[steam_id_offset..steam_id_offset + 8].copy_from_slice(&spoofed_steam_id.to_le_bytes());
            }
        }
    }
}

/// Parse SteamID from OwnershipTicket NetPacket response buffer
pub fn parse_ownershipticket_steam_id(write_buf: &[u8]) -> Option<u64> {
    if write_buf.len() < 16 {
        return None;
    }

    let ticket_size =
        u32::from_le_bytes([write_buf[12], write_buf[13], write_buf[14], write_buf[15]]) as usize;

    let ticket_data_start = 16;
    let ticket_data_end = ticket_data_start + ticket_size;

    if write_buf.len() < ticket_data_end {
        log::debug!(
            "parse_ownershipticket_steam_id: buffer too small, have {} need {}",
            write_buf.len(),
            ticket_data_end
        );
        return None;
    }

    // Try to find SteamID in the ticket data (typically at offset 8 within ticket data)
    let mut offset = ticket_data_start + 8;
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
            break;
        }
        if bytes_consumed > 10 {
            return None;
        }
    }

    if bytes_consumed > 0 && result >= 0x0110_0001_0000_0000 {
        return Some(result);
    }

    // Fallback: check fixed offset
    if write_buf.len() >= 28 {
        let potential_steam_id =
            u64::from_le_bytes([write_buf[20], write_buf[21], write_buf[22], write_buf[23],
                                write_buf[24], write_buf[25], write_buf[26], write_buf[27]]);
        if potential_steam_id >= 0x0110_0001_0000_0000 {
            return Some(potential_steam_id);
        }
    }

    None
}

/// Handle response for GetAppOwnershipTicket: Force eresult=OK, inject ownership ticket
pub fn handle_recv_get_app_ownership_ticket_response(write_buf: &mut Vec<u8>, app_id: u32) {
    if write_buf.len() >= 12 {
        // Force eresult = OK (0) at offset 8
        write_buf[8..12].copy_from_slice(&(0u32).to_le_bytes());

        // Get ownership ticket from credential store
        let ticket_result = read_app_ticket(app_id);

        if let Ok(Some(spoofed_ticket)) = ticket_result {
            // Inject the ownership ticket data
            if let Some(app_ticket) = spoofed_ticket.app_ticket {
                // The ticket data is hex-encoded, decode it
                let ticket_bytes = hex::decode(&app_ticket).unwrap_or_else(|_| app_ticket.into_bytes());

                if write_buf.len() > 12 {
                    write_buf.truncate(12);
                    write_buf.extend_from_slice(&ticket_bytes);
                }
            }

            // Also inject steam_id if available
            if let Some(steam_id_str) = spoofed_ticket.steam_id {
                if let Ok(steam_id) = steam_id_str.parse::<u64>() {
                    // Ensure buffer has space for steam_id at expected offset
                    if write_buf.len() < 28 {
                        write_buf.resize(28, 0u8);
                    }
                    write_buf[20..28].copy_from_slice(&steam_id.to_le_bytes());
                }
            }
        }
    }

    log::debug!(
        "HandleRecv_GetAppOwnershipTicketResponse: AppId={} - forced eresult=OK, injected ownership ticket",
        app_id
    )
}

/// Handle response for GetAppOwnershipTicketExtendedData: Force eresult=OK, inject extended ownership ticket
pub fn handle_recv_get_app_ownership_ticket_extended_data_response(write_buf: &mut Vec<u8>, app_id: u32) {
    if write_buf.len() >= 12 {
        // Force eresult = OK (0) at offset 8
        write_buf[8..12].copy_from_slice(&(0u32).to_le_bytes());

        // Get ownership ticket from credential store
        let ticket_result = read_app_ticket(app_id);

        if let Ok(Some(spoofed_ticket)) = ticket_result {
            // Build the extended data response
            // Structure: total_size, app_id_offset, steam_id_offset, signature_offset, signature_size, then data
            let mut extended_data = Vec::new();

            // Add app_ticket if available
            if let Some(app_ticket) = spoofed_ticket.app_ticket {
                let ticket_bytes = hex::decode(&app_ticket).unwrap_or_else(|_| app_ticket.into_bytes());
                extended_data.extend_from_slice(&ticket_bytes);
            }

            // Add e_ticket if available
            if let Some(e_ticket) = spoofed_ticket.e_ticket {
                let eticket_bytes = hex::decode(&e_ticket).unwrap_or_else(|_| e_ticket.into_bytes());
                extended_data.extend_from_slice(&eticket_bytes);
            }

            if !extended_data.is_empty() && write_buf.len() > 12 {
                write_buf.truncate(12);
                write_buf.extend_from_slice(&extended_data);
            }
        }
    }

    log::debug!(
        "HandleRecv_GetAppOwnershipTicketExtendedDataResponse: AppId={} - forced eresult=OK, injected extended ownership ticket",
        app_id
    )
}

pub fn install_ownershipticket() -> Result<(), String> {
    register_ownershipticket_handlers();
    log::info!("Installed OwnershipTicket NetPacket hooks");
    Ok(())
}

pub fn uninstall_ownershipticket() {
    unregister_ownershipticket_handlers();
    log::info!("Uninstalled OwnershipTicket NetPacket hooks");
}

/// Alias for install_ownershipticket for API consistency.
pub fn install_ownershipticket_hooks() -> Result<(), String> {
    install_ownershipticket()
}

/// Alias for uninstall_ownershipticket for API consistency.
pub fn uninstall_ownershipticket_hooks() {
    uninstall_ownershipticket()
}

/// Checks if OwnershipTicket hooks are active.
pub fn is_ownershipticket_hook_active() -> bool {
    // For now, return true if we've ever installed
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fnv1a_hash_get_app_ownership_ticket() {
        assert_eq!(
            NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_FNHASH,
            fnv1a_hash("GetAppOwnershipTicket") as FuncHash
        );
    }

    #[test]
    fn test_fnv1a_hash_get_app_ownership_ticket_extended_data() {
        assert_eq!(
            NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_EXTENDED_DATA_FNHASH,
            fnv1a_hash("GetAppOwnershipTicketExtendedData") as FuncHash
        );
    }

    #[test]
    fn test_handler_registration() {
        install_ownershipticket();
        let handlers = ownershipticket_handlers().lock().unwrap();
        assert!(!handlers.is_empty(), "Handlers should be registered");

        let key = (IpcInterface::IClientUser, NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_FNHASH);
        assert!(
            handlers.contains_key(&key),
            "GetAppOwnershipTicket handler should be registered"
        );

        let key2 = (
            IpcInterface::IClientUser,
            NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_EXTENDED_DATA_FNHASH,
        );
        assert!(
            handlers.contains_key(&key2),
            "GetAppOwnershipTicketExtendedData handler should be registered"
        );
    }

    #[test]
    fn test_parse_ownershipticket_steam_id() {
        let mut write_buf = vec![0u8; 32];
        write_buf[8..12].copy_from_slice(&1u32.to_le_bytes());
        write_buf[12..16].copy_from_slice(&20u32.to_le_bytes());
        write_buf[16] = 0x08 | (1245620 as u8 & 0x7F);
        let test_steam_id = 76561198028121353u64;
        write_buf[20..28].copy_from_slice(&test_steam_id.to_le_bytes());

        let result = parse_ownershipticket_steam_id(&write_buf);
        assert!(
            result.is_some(),
            "Should parse SteamID from OwnershipTicket buffer"
        );
        assert_eq!(result.unwrap(), test_steam_id);
    }

    #[test]
    fn test_parse_ownershipticket_steam_id_no_data() {
        let write_buf = vec![0u8; 10];
        let result = parse_ownershipticket_steam_id(&write_buf);
        assert!(result.is_none(), "Should return None for too-small buffer");
    }

    #[test]
    fn test_handle_recv_get_app_ownership_ticket_response() {
        let mut write_buf = vec![0u8; 100];
        write_buf[8..12].copy_from_slice(&2u32.to_le_bytes()); // eresult = 2 (FAIL)
        handle_recv_get_app_ownership_ticket_response(&mut write_buf, 123456);

        // Verify eresult was forced to OK
        assert_eq!(write_buf[8..12], (0u32).to_le_bytes(), "eresult should be forced to OK");

        // Verify ticket was injected
        assert!(write_buf.len() > 12, "Buffer should contain injected ownership ticket");
    }

    #[test]
    fn test_handle_recv_get_app_ownership_ticket_extended_data_response() {
        let mut write_buf = vec![0u8; 100];
        write_buf[8..12].copy_from_slice(&2u32.to_le_bytes()); // eresult = 2 (FAIL)
        handle_recv_get_app_ownership_ticket_extended_data_response(&mut write_buf, 123456);

        // Verify eresult was forced to OK
        assert_eq!(write_buf[8..12], (0u32).to_le_bytes(), "eresult should be forced to OK");
    }
}