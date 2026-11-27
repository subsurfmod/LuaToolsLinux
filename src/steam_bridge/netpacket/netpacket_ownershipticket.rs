//! NetPacket OwnershipTicket hook layer for Linux.
//!
//! Port of usefulsteamtools `Interop_NetPacket_OwnershipTicket.cpp`.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use crate::steam_bridge::credentials::appticket::{read_app_ticket, AppOwnershipTicket};
use crate::steam_bridge::interop::interop_misc::resolve_app_id;
use crate::steam_bridge::interop::ipc::{
    self, fnv1a_hash, FuncHash, HandlerFn, IpcContext, IpcInterface,
};
use crate::steam_bridge::netpacket::netpacket_eticket::decode_varint;

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

    let basic_post: HandlerFn = Arc::new(|ctx: &mut IpcContext| -> Result<(), String> {
        handle_send_get_app_ownership_ticket_post(ctx);
        Ok(())
    });
    let ext_post: HandlerFn = Arc::new(|ctx: &mut IpcContext| -> Result<(), String> {
        handle_send_get_app_ownership_ticket_extended_data_post(ctx);
        Ok(())
    });

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
            post_handler: Some(Arc::clone(&basic_post)),
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
            post_handler: Some(Arc::clone(&ext_post)),
        },
    );
    drop(map);

    ipc::register_handler(ipc::make_handler(
        iface,
        NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_FNHASH,
        "Hooks_NetPacket_OwnershipTicket::HandleSend_GetAppOwnershipTicket",
        0,
        4,
        None,
        Some(basic_post),
    ));
    ipc::register_handler(ipc::make_handler(
        iface,
        NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_EXTENDED_DATA_FNHASH,
        "Hooks_NetPacket_OwnershipTicket::HandleSend_GetAppOwnershipTicketExtendedData",
        0,
        4,
        None,
        Some(ext_post),
    ));
}

pub fn unregister_ownershipticket_handlers() {
    ownershipticket_handlers()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    let iface = IpcInterface::IClientUser;
    let _ = ipc::unregister_handler(
        iface,
        NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_FNHASH,
    );
    let _ = ipc::unregister_handler(
        iface,
        NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_EXTENDED_DATA_FNHASH,
    );
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

    // Standard Valve AppOwnershipTicket layout: [size:4][version:4][steam_id:8]...
    // SteamID is at ticket_data_start + 8 = write_buf[24..32]
    if ticket_data_end >= 32 && write_buf.len() >= 32 {
        let sid = u64::from_le_bytes([
            write_buf[24], write_buf[25], write_buf[26], write_buf[27],
            write_buf[28], write_buf[29], write_buf[30], write_buf[31],
        ]);
        if sid >= 0x0110_0001_0000_0000 {
            return Some(sid);
        }
    }

    if let Some(varint_start) = ticket_data_start.checked_add(8) {
        if varint_start < ticket_data_end {
            if let Some((val, _)) = decode_varint(&write_buf[varint_start..ticket_data_end]) {
                if val >= 0x0110_0001_0000_0000 {
                    return Some(val);
                }
            }
        }
    }

    // Compact ticket layout: [version:4][steam_id:8]...
    // SteamID is at ticket_data_start + 4 = write_buf[20..28]
    if ticket_data_end >= 28 && write_buf.len() >= 28 {
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

/// Pure helper: force `eresult = OK` and write `[ticket_size: u32][ticket_bytes...]` starting
/// at offset 12 (matching `parse_ownershipticket_steam_id` framing).
pub fn handle_recv_get_app_ownership_ticket_response_with_ticket(
    write_buf: &mut Vec<u8>,
    spoofed_ticket: Option<&AppOwnershipTicket>,
) {
    if write_buf.len() < 12 {
        return;
    }
    write_buf[8..12].copy_from_slice(&0u32.to_le_bytes());

    let Some(ticket) = spoofed_ticket else {
        return;
    };

    if let Some(app_ticket) = ticket.app_ticket.as_deref().filter(|s| !s.is_empty()) {
        let ticket_bytes =
            hex::decode(app_ticket).unwrap_or_else(|_| app_ticket.as_bytes().to_vec());
        if let Ok(len_u32) = u32::try_from(ticket_bytes.len()) {
            write_buf.truncate(12);
            write_buf.extend_from_slice(&len_u32.to_le_bytes());
            write_buf.extend_from_slice(&ticket_bytes);
        }
    } else if let Some(steam_id) = ticket
        .steam_id
        .as_deref()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&sid| sid != 0)
    {
        // Minimal v4 ticket payload: [version=4:4][steam_id:8] prefixed by ticket_size=12
        write_buf.truncate(12);
        write_buf.extend_from_slice(&12u32.to_le_bytes());
        write_buf.extend_from_slice(&4u32.to_le_bytes());
        write_buf.extend_from_slice(&steam_id.to_le_bytes());
    }
}

/// Force `eresult = OK` and inject ownership ticket when stored.
pub fn handle_recv_get_app_ownership_ticket_response(write_buf: &mut Vec<u8>, app_id: u32) {
    let ticket = read_app_ticket(app_id).ok().flatten();
    handle_recv_get_app_ownership_ticket_response_with_ticket(write_buf, ticket.as_ref());
}

/// Pure helper: force `eresult = OK` and inject extended ownership ticket data when supplied.
pub fn handle_recv_get_app_ownership_ticket_extended_data_response_with_ticket(
    write_buf: &mut Vec<u8>,
    spoofed_ticket: Option<&AppOwnershipTicket>,
) {
    if write_buf.len() < 12 {
        return;
    }
    write_buf[8..12].copy_from_slice(&0u32.to_le_bytes());

    let Some(ticket) = spoofed_ticket else {
        return;
    };

    let mut extended_data = Vec::new();
    if let Some(app_ticket) = ticket.app_ticket.as_deref().filter(|s| !s.is_empty()) {
        let ticket_bytes =
            hex::decode(app_ticket).unwrap_or_else(|_| app_ticket.as_bytes().to_vec());
        extended_data.extend_from_slice(&ticket_bytes);
    }
    if let Some(e_ticket) = ticket.e_ticket.as_deref().filter(|s| !s.is_empty()) {
        let eticket_bytes = hex::decode(e_ticket).unwrap_or_else(|_| e_ticket.as_bytes().to_vec());
        extended_data.extend_from_slice(&eticket_bytes);
    }
    if !extended_data.is_empty() {
        write_buf.truncate(12);
        write_buf.extend_from_slice(&extended_data);
    }
}

/// Force `eresult = OK` and inject extended ownership ticket when stored.
pub fn handle_recv_get_app_ownership_ticket_extended_data_response(
    write_buf: &mut Vec<u8>,
    app_id: u32,
) {
    let ticket = read_app_ticket(app_id).ok().flatten();
    handle_recv_get_app_ownership_ticket_extended_data_response_with_ticket(
        write_buf,
        ticket.as_ref(),
    );
}

/// Encode a protobuf varint.
fn encode_varint(mut val: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (val & 0x7F) as u8;
        val >>= 7;
        if val != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if val == 0 {
            break;
        }
    }
}

/// Encode CMsgClientGetAppOwnershipTicketResponse (EMsg 858) protobuf payload.
pub fn encode_ownership_ticket_response(app_id: u32, ticket_bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + ticket_bytes.len());
    // Field 1: eresult = 1 (k_EResultOK)
    out.push(0x08);
    encode_varint(1, &mut out);
    // Field 2: app_id
    out.push(0x10);
    encode_varint(u64::from(app_id), &mut out);
    // Field 3: ticket (bytes)
    out.push(0x1A);
    encode_varint(ticket_bytes.len() as u64, &mut out);
    out.extend_from_slice(ticket_bytes);
    out
}

/// Parse CMsgClientGetAppOwnershipTicketResponse (EMsg 858) protobuf payload.
pub fn parse_ownership_ticket_response(
    body: &[u8],
) -> (Option<i32>, Option<u32>, Option<Vec<u8>>) {
    let mut idx = 0;
    let mut eresult = None;
    let mut app_id = None;
    let mut ticket = None;
    while idx < body.len() {
        let Some((tag, tag_len)) = decode_varint(&body[idx..]) else {
            break;
        };
        idx += tag_len;
        let field_num = tag >> 3;
        let wire_type = tag & 0x07;
        match (field_num, wire_type) {
            (1, 0) => {
                if let Some((val, vlen)) = decode_varint(&body[idx..]) {
                    eresult = Some(val as i32);
                    idx += vlen;
                } else {
                    break;
                }
            }
            (2, 0) => {
                if let Some((val, vlen)) = decode_varint(&body[idx..]) {
                    app_id = Some(val as u32);
                    idx += vlen;
                } else {
                    break;
                }
            }
            (3, 2) => {
                if let Some((len, vlen)) = decode_varint(&body[idx..]) {
                    idx += vlen;
                    let len = len as usize;
                    if idx + len <= body.len() {
                        ticket = Some(body[idx..idx + len].to_vec());
                        idx += len;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            (_, 0) => {
                if let Some((_, vlen)) = decode_varint(&body[idx..]) {
                    idx += vlen;
                } else {
                    break;
                }
            }
            (_, 2) => {
                if let Some((len, vlen)) = decode_varint(&body[idx..]) {
                    idx += vlen + len as usize;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    (eresult, app_id, ticket)
}

/// IPC adapter for Recv: MsgClientGetAppOwnershipTicketResponse (858) (BST-39)
pub fn handle_recv_get_app_ownership_ticket_response_ipc(
    ctx: &mut crate::steam_bridge::netpacket::netpacket_core::NetPacketContext,
) -> Result<(), String> {
    let body = ctx.body();
    let (eresult, parsed_app_id, _existing_ticket) = parse_ownership_ticket_response(body);
    if eresult == Some(1) {
        // Steam already returned OK (account genuinely owns it)
        return Ok(());
    }

    let app_id = parsed_app_id
        .filter(|&id| id != 0)
        .unwrap_or(if ctx.app_id != 0 {
            ctx.app_id
        } else {
            resolve_app_id()
        });
    if app_id == 0 {
        return Ok(());
    }

    // Retrieve ticket from credential store or forge using BST-49 exploit
    let ticket_bytes = if let Ok(Some(stored)) = read_app_ticket(app_id) {
        stored
            .app_ticket
            .as_deref()
            .and_then(|h| hex::decode(h).ok())
            .or_else(|| {
                crate::steam_bridge::credentials::appticket::forge_local_app_ownership_ticket(
                    app_id, None,
                )
            })
    } else {
        crate::steam_bridge::credentials::appticket::forge_local_app_ownership_ticket(app_id, None)
    };

    let Some(ticket_bytes) = ticket_bytes else {
        return Ok(());
    };

    let new_body = encode_ownership_ticket_response(app_id, &ticket_bytes);
    let mut new_frame = Vec::with_capacity(ctx.body_offset + new_body.len());
    new_frame.extend_from_slice(&ctx.frame_buffer[..ctx.body_offset]);
    new_frame.extend_from_slice(&new_body);
    ctx.replace_frame(new_frame);
    log::info!(
        "[OwnershipTicket] Rewrote EMsg 858 response for AppId={} (ticket len={})",
        app_id,
        ticket_bytes.len()
    );
    Ok(())
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

    static OWNERSHIP_TEST_GUARD: Mutex<()> = Mutex::new(());

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
    fn test_handler_registration_and_uninstall() {
        let _g = OWNERSHIP_TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        install_ownershipticket().unwrap();
        assert!(is_ownershipticket_hook_active());

        {
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
        assert!(ipc::find_handler(
            IpcInterface::IClientUser,
            NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_FNHASH
        )
        .is_some());

        uninstall_ownershipticket();
        assert!(!is_ownershipticket_hook_active());
        assert!(ipc::find_handler(
            IpcInterface::IClientUser,
            NETPACKET_OWNERSHIPTICKET_GET_APP_OWNERSHIP_TICKET_FNHASH
        )
        .is_none());
    }

    #[test]
    fn test_parse_ownershipticket_steam_id() {
        let mut write_buf = vec![0u8; 36];
        write_buf[8..12].copy_from_slice(&1u32.to_le_bytes());
        write_buf[12..16].copy_from_slice(&20u32.to_le_bytes());
        write_buf[16] = 0x08 | ((1245620u32 as u8) & 0x7F);
        let test_steam_id = 76_561_198_028_121_353u64;
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
    fn test_handle_recv_responses_and_framing_roundtrip() {
        let sid = 76_561_198_028_121_353u64;

        // 1. Minimal steam_id-only ticket on an exact 12-byte header roundtrips via parse_ownershipticket_steam_id
        let mut write_buf = vec![0u8; 12];
        write_buf[8..12].copy_from_slice(&2u32.to_le_bytes());
        let ticket_sid_only = AppOwnershipTicket {
            app_id: 123456,
            steam_id: Some(sid.to_string()),
            app_ticket: None,
            e_ticket: None,
        };
        handle_recv_get_app_ownership_ticket_response_with_ticket(
            &mut write_buf,
            Some(&ticket_sid_only),
        );
        assert_eq!(write_buf[8..12], 0u32.to_le_bytes());
        assert_eq!(parse_ownershipticket_steam_id(&write_buf), Some(sid));

        // 2. Full hex ticket payload does not overwrite ticket_size at [12..16]
        let mut raw_ticket = vec![0u8; 20];
        raw_ticket[0..4].copy_from_slice(&4u32.to_le_bytes());
        raw_ticket[4..12].copy_from_slice(&sid.to_le_bytes());
        let full_ticket = AppOwnershipTicket {
            app_id: 123456,
            steam_id: Some(sid.to_string()),
            app_ticket: Some(hex::encode(&raw_ticket)),
            e_ticket: Some("cafebabe".into()),
        };
        let mut buf2 = vec![0u8; 12];
        handle_recv_get_app_ownership_ticket_response_with_ticket(&mut buf2, Some(&full_ticket));
        assert_eq!(&buf2[12..16], &20u32.to_le_bytes());
        assert_eq!(&buf2[16..36], raw_ticket.as_slice());
        assert_eq!(parse_ownershipticket_steam_id(&buf2), Some(sid));

        // 3. Extended data response works on exact 12-byte buffer
        let mut ext_buf = vec![0u8; 12];
        ext_buf[8..12].copy_from_slice(&2u32.to_le_bytes());
        handle_recv_get_app_ownership_ticket_extended_data_response_with_ticket(
            &mut ext_buf,
            Some(&full_ticket),
        );
        assert_eq!(ext_buf[8..12], 0u32.to_le_bytes());
        assert_eq!(ext_buf.len(), 12 + 20 + 4);
        assert_eq!(&ext_buf[32..36], &[0xCA, 0xFE, 0xBA, 0xBE]);
    }

    #[test]
    fn test_ownership_ticket_protobuf_codec_roundtrip() {
        let app_id = 1245620u32;
        let ticket = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x42, 0x42];
        let encoded = encode_ownership_ticket_response(app_id, &ticket);
        let (eresult, parsed_app, parsed_ticket) = parse_ownership_ticket_response(&encoded);
        assert_eq!(eresult, Some(1));
        assert_eq!(parsed_app, Some(app_id));
        assert_eq!(parsed_ticket, Some(ticket));
    }
}
