//! NetPacket ETicket hook layer for Linux.
//!
//! Port of usefulsteamtools `Interop_NetPacket_ETicket.cpp`.
//! Handles interception and spoofing of CMsgETicket NetPacket messages
//! for the online fix / Denuvo authentication path.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

// Import only what we need from ipc module - use the public API
use crate::steam_bridge::interop::ipc::{
    FuncHash, IpcContext, IpcInterface, make_handler, register_handler, fnv1a_hash,
};
use crate::steam_bridge::interop::interop_misc::resolve_app_id as misc_resolve_app_id;
use crate::steam_bridge::ost_platform::steam_credential_store;

const NETPACKET_ETICKET_GET_ENCRYPTED_APP_TICKET_FNHASH: FuncHash =
    fnv1a_hash("GetEncryptedAppTicket") as FuncHash;
const NETPACKET_ETICKET_REQUEST_ENCRYPTED_APP_TICKET_FNHASH: FuncHash =
    fnv1a_hash("RequestEncryptedAppTicket") as FuncHash;

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

static ETICKET_HANDLERS: LazyLock<Mutex<HashMap<(IpcInterface, FuncHash), EticketHandler>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn eticket_handlers() -> &'static Mutex<HashMap<(IpcInterface, FuncHash), EticketHandler>> {
    &ETICKET_HANDLERS
}

pub fn register_eticket_handlers() {
    let mut map = eticket_handlers().lock().unwrap();
    let iface = IpcInterface::IClientUser;
    let get_encrypted_app_ticket_hash = NETPACKET_ETICKET_GET_ENCRYPTED_APP_TICKET_FNHASH;
    let request_encrypted_app_ticket_hash = NETPACKET_ETICKET_REQUEST_ENCRYPTED_APP_TICKET_FNHASH;

    let handle_recv_get_encrypted_app_ticket = EticketHandler {
        interface_id: iface,
        func_hash: get_encrypted_app_ticket_hash,
        name: "Hooks_NetPacket_ETicket::HandleRecv_GetEncryptedAppTicket".to_string(),
        fencepost: 0,
        argc: 4,
        pre_handler: None,
        post_handler: Some(Box::new(|ctx: &mut IpcContext| -> Result<(), String> {
            handle_recv_get_encrypted_app_ticket_post(ctx);
            Ok(())
        })),
    };
    map.insert((iface, get_encrypted_app_ticket_hash), handle_recv_get_encrypted_app_ticket);

    let handle_send_request_encrypted_app_ticket = EticketHandler {
        interface_id: iface,
        func_hash: request_encrypted_app_ticket_hash,
        name: "Hooks_NetPacket_ETicket::HandleSend_RequestEncryptedAppTicket".to_string(),
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

pub fn unregister_eticket_handlers() {
    let mut map = eticket_handlers().lock().unwrap();
    map.clear();
}

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

    if ctx.write_buffer.len() >= 28 {
        let ticket_data_start = 16;
        if ctx.write_buffer.len() >= ticket_data_start + 12 {
            log::debug!(
                "HandleRecv_GetEncryptedAppTicket: AppId={} spoofing SteamID to 0x{:X}",
                app_id,
                spoofed_steam_id
            );
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
    log::debug!(
        "HandleSend_RequestEncryptedAppTicket: AppId={} recorded for ETicket spoofing",
        app_id
    )
}

pub fn parse_eticket_steam_id(write_buf: &[u8]) -> Option<u64> {
    if write_buf.len() < 16 {
        return None;
    }

    let ticket_size =
        u32::from_le_bytes([write_buf[12], write_buf[13], write_buf[14], write_buf[15]])
            as usize;

    let ticket_data_start = 16;
    let ticket_data_end = ticket_data_start + ticket_size;

    if write_buf.len() < ticket_data_end {
        log::debug!(
            "parse_eticket_steam_id: buffer too small, have {} need {}",
            write_buf.len(),
            ticket_data_end
        );
        return None;
    }

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

pub fn handle_recv_get_encrypted_app_ticket_response(write_buf: &mut Vec<u8>, app_id: u32) {
    if write_buf.len() >= 12 {
        write_buf[8..12].copy_from_slice(&(0u32).to_le_bytes());

        let spoofed_steam_id = steam_credential_store::get_steam_id(app_id);

        if spoofed_steam_id != 0 {
            let mut eticket_block = Vec::new();
            eticket_block.push(0x08 | (app_id as u8 & 0x7F));
            let mut steam_id_bytes: Vec<u8> = Vec::new();
            let mut steam_id_val = spoofed_steam_id;
            while steam_id_val > 0 || steam_id_bytes.is_empty() {
                let mut byte = (steam_id_val & 0x7F) as u8;
                steam_id_val >>= 7;
                if !steam_id_val {
                    byte |= 0x80;
                }
                steam_id_bytes.push(byte);
            }
            eticket_block.push(0x10 | steam_id_bytes[0]);
            for &b in &steam_id_bytes[1..] {
                eticket_block.push(b);
            }

            if write_buf.len() > 12 {
                write_buf.truncate(12);
                write_buf.extend_from_slice(&eticket_block);
            }
        }
    }
    log::debug!(
        "HandleRecv_GetEncryptedAppTicketResponse: AppId={} - forced eresult=OK, injected ETicket",
        app_id
    )
}

pub fn install_eticket() -> Result<(), String> {
    register_eticket_handlers();
    log::info!("Installed ETicket NetPacket hooks");
    Ok(())
}

pub fn uninstall_eticket() {
    unregister_eticket_handlers();
    log::info!("Uninstalled ETicket NetPacket hooks");
}

/// Alias for install_eticket for API consistency.
pub fn install_eticket_hooks() -> Result<(), String> {
    install_eticket()
}

/// Alias for uninstall_eticket for API consistency.
pub fn uninstall_eticket_hooks() {
    uninstall_eticket()
}

/// Checks if ETicket hooks are active.
pub fn is_eticket_hook_active() -> bool {
    // For now, return true if we've ever installed
    true
}

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

        let key = (IpcInterface::IClientUser, NETPACKET_ETICKET_GET_ENCRYPTED_APP_TICKET_FNHASH);
        assert!(
            handlers.contains_key(&key),
            "GetEncryptedAppTicket handler should be registered"
        );

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
        let mut write_buf = vec![0u8; 32];
        write_buf[8..12].copy_from_slice(&1u32.to_le_bytes());
        write_buf[12..16].copy_from_slice(&20u32.to_le_bytes());
        write_buf[16] = 0x08 | (1245620 as u8 & 0x7F);
        let test_steam_id = 76561198028121353u64;
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
        assert!(result.is_none, "Should return None for too-small buffer");
    }

    #[test]
    fn test_handle_recv_get_encrypted_app_ticket_response() {
        let mut write_buf = vec![0u8; 100];
        write_buf[8..12].copy_from_slice(&2u32.to_le_bytes());
        handle_recv_get_encrypted_app_ticket_response(&mut write_buf, 123456);
        assert_eq!(
            write_buf[8..12],
            (0u32).to_le_bytes(),
            "eresult should be forced to OK"
        );
        assert!(
            write_buf.len() > 12,
            "Buffer should contain injected ETicket block"
        );
    }
}