//! NetPacket OwnershipTicket hook layer for Linux.
//!
//! Port of usefulsteamtools `Interop_NetPacket_OwnershipTicket.cpp`.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use crate::steam_bridge::credentials::appticket::read_app_ticket;
use crate::steam_bridge::interop::interop_misc::resolve_app_id;
use crate::steam_bridge::interop::ipc::{
    fnv1a_hash, FuncHash, HandlerFn, IpcContext, IpcInterface,
};

const NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_FNHASH: FuncHash =
    fnv1a_hash("GetAppOwnershipTicket");
const NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_EXTENDED_DATA_FNHASH: FuncHash =
    fnv1a_hash("GetAppOwnershipTicketExtendedData");

#[derive(Clone)]
pub struct OwnershipTicketHandler {
    pub interface_id: IpcInterface,
    pub func_hash: FuncHash,
    pub name: String,
    pub fencepost: u32,
    pub argc: u32,
    pub pre_handler: Option<HandlerFn>,
    pub post_handler: Option<HandlerFn>,
}

impl fmt::Debug for OwnershipTicketHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnershipTicketHandler")
            .field("interface_id", &self.interface_id)
            .field("func_hash", &self.func_hash)
            .field("name", &self.name)
            .finish()
    }
}

static OWNERSHIPTICKET_HANDLERS: LazyLock<
    Mutex<HashMap<(IpcInterface, FuncHash), OwnershipTicketHandler>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));
static OWNERSHIPTICKET_ACTIVE: AtomicBool = AtomicBool::new(false);

fn ownershipticket_handlers(
) -> &'static Mutex<HashMap<(IpcInterface, FuncHash), OwnershipTicketHandler>> {
    &OWNERSHIPTICKET_HANDLERS
}

pub fn register_ownershipticket_handlers() {
    let mut map = ownershipticket_handlers()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let iface = IpcInterface::IClientUser;

    map.insert(
        (
            iface,
            NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_FNHASH,
        ),
        OwnershipTicketHandler {
            interface_id: iface,
            func_hash: NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_FNHASH,
            name: "Hooks_NetPacket_OwnershipTicket::HandleSend_GetAppOwnershipTicket".to_string(),
            fencepost: 0,
            argc: 4,
            pre_handler: None,
            post_handler: Some(Arc::new(|ctx: &mut IpcContext| -> Result<(), String> {
                handle_send_get_app_ownership_ticket_post(ctx);
                Ok(())
            })),
        },
    );

    map.insert(
        (
            iface,
            NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_EXTENDED_DATA_FNHASH,
        ),
        OwnershipTicketHandler {
            interface_id: iface,
            func_hash: NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_EXTENDED_DATA_FNHASH,
            name: "Hooks_NetPacket_OwnershipTicket::HandleSend_GetAppOwnershipTicketExtendedData"
                .to_string(),
            fencepost: 0,
            argc: 4,
            pre_handler: None,
            post_handler: Some(Arc::new(|ctx: &mut IpcContext| -> Result<(), String> {
                handle_send_get_app_ownership_ticket_extended_data_post(ctx);
                Ok(())
            })),
        },
    );
}

pub fn unregister_ownershipticket_handlers() {
    ownershipticket_handlers()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

fn handle_send_get_app_ownership_ticket_post(ctx: &mut IpcContext) {
    let app_id = if ctx.app_id != 0 {
        ctx.app_id
    } else {
        resolve_app_id()
    };

    let Ok(Some(spoofed_ticket)) = read_app_ticket(app_id) else {
        return;
    };

    if ctx.write_buffer.len() >= 20 {
        let spoofed_steam_id = spoofed_ticket
            .steam_id
            .as_deref()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        if spoofed_steam_id != 0 {
            ctx.write_buffer[12..20].copy_from_slice(&spoofed_steam_id.to_le_bytes());
        }
    }
}

fn handle_send_get_app_ownership_ticket_extended_data_post(ctx: &mut IpcContext) {
    let app_id = if ctx.app_id != 0 {
        ctx.app_id
    } else {
        resolve_app_id()
    };

    let Ok(Some(spoofed_ticket)) = read_app_ticket(app_id) else {
        return;
    };

    if ctx.write_buffer.len() >= 24 {
        let spoofed_steam_id = spoofed_ticket
            .steam_id
            .as_deref()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        if spoofed_steam_id != 0 {
            ctx.write_buffer[16..24].copy_from_slice(&spoofed_steam_id.to_le_bytes());
        }
    }
}

/// Parse SteamID from OwnershipTicket NetPacket response buffer.
pub fn parse_ownershipticket_steam_id(write_buf: &[u8]) -> Option<u64> {
    if write_buf.len() < 16 {
        return None;
    }

    let ticket_size =
        u32::from_le_bytes([write_buf[12], write_buf[13], write_buf[14], write_buf[15]]) as usize;
    let ticket_data_start: usize = 16;
    let ticket_data_end = ticket_data_start.checked_add(ticket_size)?;

    if write_buf.len() < ticket_data_end {
        return None;
    }

    let mut offset = ticket_data_start + 8;
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    let mut bytes_consumed: usize = 0;

    while offset < ticket_data_end && shift < 64 {
        let byte = write_buf[offset];
        result |= u64::from(byte & 0x7F) << shift;
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
        let potential_steam_id = u64::from_le_bytes([
            write_buf[20],
            write_buf[21],
            write_buf[22],
            write_buf[23],
            write_buf[24],
            write_buf[25],
            write_buf[26],
            write_buf[27],
        ]);
        if potential_steam_id >= 0x0110_0001_0000_0000 {
            return Some(potential_steam_id);
        }
    }

    None
}

/// Force `eresult = OK` and inject ownership ticket when stored.
pub fn handle_recv_get_app_ownership_ticket_response(write_buf: &mut Vec<u8>, app_id: u32) {
    if write_buf.len() >= 12 {
        write_buf[8..12].copy_from_slice(&0u32.to_le_bytes());

        if let Ok(Some(spoofed_ticket)) = read_app_ticket(app_id) {
            if let Some(app_ticket) = spoofed_ticket.app_ticket {
                let ticket_bytes =
                    hex::decode(&app_ticket).unwrap_or_else(|_| app_ticket.into_bytes());
                if write_buf.len() > 12 {
                    write_buf.truncate(12);
                    write_buf.extend_from_slice(&ticket_bytes);
                }
            }

            if let Some(steam_id_str) = spoofed_ticket.steam_id {
                if let Ok(steam_id) = steam_id_str.parse::<u64>() {
                    if write_buf.len() < 28 {
                        write_buf.resize(28, 0u8);
                    }
                    write_buf[20..28].copy_from_slice(&steam_id.to_le_bytes());
                }
            }
        }
    }
}

/// Force `eresult = OK` and inject extended ownership ticket when stored.
pub fn handle_recv_get_app_ownership_ticket_extended_data_response(
    write_buf: &mut Vec<u8>,
    app_id: u32,
) {
    if write_buf.len() >= 12 {
        write_buf[8..12].copy_from_slice(&0u32.to_le_bytes());

        if let Ok(Some(spoofed_ticket)) = read_app_ticket(app_id) {
            let mut extended_data = Vec::new();
            if let Some(app_ticket) = spoofed_ticket.app_ticket {
                let ticket_bytes =
                    hex::decode(&app_ticket).unwrap_or_else(|_| app_ticket.into_bytes());
                extended_data.extend_from_slice(&ticket_bytes);
            }
            if let Some(e_ticket) = spoofed_ticket.e_ticket {
                let eticket_bytes =
                    hex::decode(&e_ticket).unwrap_or_else(|_| e_ticket.into_bytes());
                extended_data.extend_from_slice(&eticket_bytes);
            }
            if !extended_data.is_empty() && write_buf.len() > 12 {
                write_buf.truncate(12);
                write_buf.extend_from_slice(&extended_data);
            }
        }
    }
}

pub fn install_ownershipticket() -> Result<(), String> {
    register_ownershipticket_handlers();
    OWNERSHIPTICKET_ACTIVE.store(true, Ordering::Release);
    log::info!("Installed OwnershipTicket NetPacket hooks");
    Ok(())
}

pub fn uninstall_ownershipticket() {
    unregister_ownershipticket_handlers();
    OWNERSHIPTICKET_ACTIVE.store(false, Ordering::Release);
    log::info!("Uninstalled OwnershipTicket NetPacket hooks");
}

pub fn install_ownershipticket_hooks() -> Result<(), String> {
    install_ownershipticket()
}

pub fn uninstall_ownershipticket_hooks() {
    uninstall_ownershipticket()
}

pub fn is_ownershipticket_hook_active() -> bool {
    OWNERSHIPTICKET_ACTIVE.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fnv1a_hash_ownershipticket() {
        assert_eq!(
            NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_FNHASH,
            fnv1a_hash("GetAppOwnershipTicket")
        );
        assert_eq!(
            NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_EXTENDED_DATA_FNHASH,
            fnv1a_hash("GetAppOwnershipTicketExtendedData")
        );
    }

    #[test]
    fn test_handler_registration() {
        install_ownershipticket().unwrap();
        assert!(is_ownershipticket_hook_active());

        let handlers = ownershipticket_handlers().lock().unwrap();
        assert!(handlers.contains_key(&(
            IpcInterface::IClientUser,
            NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_FNHASH
        )));
        assert!(handlers.contains_key(&(
            IpcInterface::IClientUser,
            NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_EXTENDED_DATA_FNHASH
        )));
    }

    #[test]
    fn test_parse_ownershipticket_steam_id() {
        let mut write_buf = vec![0u8; 36];
        write_buf[8..12].copy_from_slice(&1u32.to_le_bytes());
        write_buf[12..16].copy_from_slice(&20u32.to_le_bytes());
        write_buf[16] = 0x08 | ((1245620u32 as u8) & 0x7F);
        let test_steam_id = 76561198028121353u64;
        write_buf[20..28].copy_from_slice(&test_steam_id.to_le_bytes());

        assert_eq!(
            parse_ownershipticket_steam_id(&write_buf),
            Some(test_steam_id)
        );
    }

    #[test]
    fn test_parse_ownershipticket_steam_id_no_data() {
        let write_buf = vec![0u8; 10];
        assert!(parse_ownershipticket_steam_id(&write_buf).is_none());
    }

    #[test]
    fn test_handle_recv_responses() {
        let mut write_buf = vec![0u8; 100];
        write_buf[8..12].copy_from_slice(&2u32.to_le_bytes());
        handle_recv_get_app_ownership_ticket_response(&mut write_buf, 123456);
        assert_eq!(write_buf[8..12], 0u32.to_le_bytes());

        let mut ext_buf = vec![0u8; 100];
        ext_buf[8..12].copy_from_slice(&2u32.to_le_bytes());
        handle_recv_get_app_ownership_ticket_extended_data_response(&mut ext_buf, 123456);
        assert_eq!(ext_buf[8..12], 0u32.to_le_bytes());
    }
}
