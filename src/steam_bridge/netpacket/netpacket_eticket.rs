//! NetPacket ETicket hook layer for Linux.
//!
//! Port of usefulsteamtools `Interop_NetPacket_ETicket.cpp`.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use crate::steam_bridge::interop::interop_misc::resolve_app_id;
use crate::steam_bridge::interop::ipc::{
    self, fnv1a_hash, FuncHash, HandlerFn, IpcContext, IpcInterface,
};
use crate::steam_bridge::ost_platform::steam_credential_store;

const NETPACKET_ETICKET_GET_ENCRYPTED_APP_TICKET_FNHASH: FuncHash =
    fnv1a_hash("GetEncryptedAppTicket");
const NETPACKET_ETICKET_REQUEST_ENCRYPTED_APP_TICKET_FNHASH: FuncHash =
    fnv1a_hash("RequestEncryptedAppTicket");

#[derive(Clone)]
pub struct EticketHandler {
    pub interface_id: IpcInterface,
    pub func_hash: FuncHash,
    pub name: String,
    pub fencepost: u32,
    pub argc: u32,
    pub pre_handler: Option<HandlerFn>,
    pub post_handler: Option<HandlerFn>,
}

impl fmt::Debug for EticketHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EticketHandler")
            .field("interface_id", &self.interface_id)
            .field("func_hash", &self.func_hash)
            .field("name", &self.name)
            .finish()
    }
}

static ETICKET_HANDLERS: LazyLock<Mutex<HashMap<(IpcInterface, FuncHash), EticketHandler>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static ETICKET_ACTIVE: AtomicBool = AtomicBool::new(false);

fn eticket_handlers() -> &'static Mutex<HashMap<(IpcInterface, FuncHash), EticketHandler>> {
    &ETICKET_HANDLERS
}

fn read_spoofed_steam_id(app_id: u32) -> u64 {
    let mut sid = 0u64;
    if steam_credential_store::get_steam_id(app_id, &mut sid) == steam_credential_store::Status::Ok
    {
        sid
    } else {
        0
    }
}

pub fn register_eticket_handlers() {
    let mut map = eticket_handlers().lock().unwrap_or_else(|e| e.into_inner());
    let iface = IpcInterface::IClientUser;

    let get_post: HandlerFn = Arc::new(|ctx: &mut IpcContext| -> Result<(), String> {
        handle_recv_get_encrypted_app_ticket_post(ctx);
        Ok(())
    });
    let req_post: HandlerFn = Arc::new(|ctx: &mut IpcContext| -> Result<(), String> {
        handle_send_request_encrypted_app_ticket_post(ctx);
        Ok(())
    });

    map.insert(
        (iface, NETPACKET_ETICKET_GET_ENCRYPTED_APP_TICKET_FNHASH),
        EticketHandler {
            interface_id: iface,
            func_hash: NETPACKET_ETICKET_GET_ENCRYPTED_APP_TICKET_FNHASH,
            name: "Hooks_NetPacket_ETicket::HandleRecv_GetEncryptedAppTicket".to_string(),
            fencepost: 0,
            argc: 4,
            pre_handler: None,
            post_handler: Some(Arc::clone(&get_post)),
        },
    );

    map.insert(
        (iface, NETPACKET_ETICKET_REQUEST_ENCRYPTED_APP_TICKET_FNHASH),
        EticketHandler {
            interface_id: iface,
            func_hash: NETPACKET_ETICKET_REQUEST_ENCRYPTED_APP_TICKET_FNHASH,
            name: "Hooks_NetPacket_ETicket::HandleSend_RequestEncryptedAppTicket".to_string(),
            fencepost: 0,
            argc: 4,
            pre_handler: None,
            post_handler: Some(Arc::clone(&req_post)),
        },
    );
    drop(map);

    ipc::register_handler(ipc::make_handler(
        iface,
        NETPACKET_ETICKET_GET_ENCRYPTED_APP_TICKET_FNHASH,
        "Hooks_NetPacket_ETicket::HandleRecv_GetEncryptedAppTicket",
        0,
        4,
        None,
        Some(get_post),
    ));
    ipc::register_handler(ipc::make_handler(
        iface,
        NETPACKET_ETICKET_REQUEST_ENCRYPTED_APP_TICKET_FNHASH,
        "Hooks_NetPacket_ETicket::HandleSend_RequestEncryptedAppTicket",
        0,
        4,
        None,
        Some(req_post),
    ));
}

pub fn unregister_eticket_handlers() {
    eticket_handlers()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    let iface = IpcInterface::IClientUser;
    let _ = ipc::unregister_handler(iface, NETPACKET_ETICKET_GET_ENCRYPTED_APP_TICKET_FNHASH);
    let _ = ipc::unregister_handler(iface, NETPACKET_ETICKET_REQUEST_ENCRYPTED_APP_TICKET_FNHASH);
}

fn handle_recv_get_encrypted_app_ticket_post(ctx: &mut IpcContext) {
    let app_id = if ctx.app_id != 0 {
        ctx.app_id
    } else {
        resolve_app_id()
    };
    let spoofed_steam_id = read_spoofed_steam_id(app_id);

    if spoofed_steam_id == 0 {
        return;
    }

    if ctx.write_buffer.len() >= 28 {
        ctx.write_buffer[20..28].copy_from_slice(&spoofed_steam_id.to_le_bytes());
        log::debug!(
            "HandleRecv_GetEncryptedAppTicket: AppId={} spoofed SteamID=0x{:X}",
            app_id,
            spoofed_steam_id
        );
    }
}

fn handle_send_request_encrypted_app_ticket_post(ctx: &mut IpcContext) {
    let app_id = if ctx.app_id != 0 {
        ctx.app_id
    } else {
        resolve_app_id()
    };
    let spoofed_steam_id = read_spoofed_steam_id(app_id);
    if spoofed_steam_id != 0 {
        log::debug!(
            "HandleSend_RequestEncryptedAppTicket: AppId={} recorded for ETicket spoofing",
            app_id
        );
    }
}

/// Strictly decode an unsigned 64-bit protobuf varint from `slice`.
/// Rejects truncated varints and 10th-byte overflows (`byte > 1`).
pub(crate) fn decode_varint(slice: &[u8]) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    for (idx, &byte) in slice.iter().enumerate() {
        if idx >= 10 {
            return None;
        }
        if idx == 9 && byte > 1 {
            return None;
        }
        result |= u64::from(byte & 0x7F) << shift;
        if (byte & 0x80) == 0 {
            return Some((result, idx + 1));
        }
        shift += 7;
    }
    None
}

pub fn parse_eticket_steam_id(write_buf: &[u8]) -> Option<u64> {
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

    if let Some(varint_start) = ticket_data_start.checked_add(8) {
        if varint_start < ticket_data_end {
            if let Some((val, _)) = decode_varint(&write_buf[varint_start..ticket_data_end]) {
                if val >= 0x0110_0001_0000_0000 {
                    return Some(val);
                }
            }
        }
    }

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

/// Encode a protobuf varint correctly (MSB `0x80` set while more bytes remain).
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

/// Pure helper that forces `eresult = OK` and appends an encoded ETicket protobuf block
/// when `spoofed_steam_id != 0`.
pub fn handle_recv_get_encrypted_app_ticket_response_with_steam_id(
    write_buf: &mut Vec<u8>,
    app_id: u32,
    spoofed_steam_id: u64,
) {
    if write_buf.len() >= 12 {
        write_buf[8..12].copy_from_slice(&0u32.to_le_bytes());

        if spoofed_steam_id != 0 {
            let mut eticket_block = Vec::new();
            // Field 1 (varint): app_id
            eticket_block.push(0x08);
            encode_varint(u64::from(app_id), &mut eticket_block);
            // Field 2 (varint): steam_id
            eticket_block.push(0x10);
            encode_varint(spoofed_steam_id, &mut eticket_block);

            write_buf.truncate(12);
            write_buf.extend_from_slice(&eticket_block);
        }
    }
    log::debug!(
        "HandleRecv_GetEncryptedAppTicketResponse: AppId={} - forced eresult=OK",
        app_id
    );
}

pub fn handle_recv_get_encrypted_app_ticket_response(write_buf: &mut Vec<u8>, app_id: u32) {
    let spoofed_steam_id = read_spoofed_steam_id(app_id);
    handle_recv_get_encrypted_app_ticket_response_with_steam_id(
        write_buf,
        app_id,
        spoofed_steam_id,
    );
}

pub fn install_eticket() -> Result<(), String> {
    register_eticket_handlers();
    ETICKET_ACTIVE.store(true, Ordering::Release);
    log::info!("Installed ETicket NetPacket hooks");
    Ok(())
}

pub fn uninstall_eticket() {
    unregister_eticket_handlers();
    ETICKET_ACTIVE.store(false, Ordering::Release);
    log::info!("Uninstalled ETicket NetPacket hooks");
}

pub fn install_eticket_hooks() -> Result<(), String> {
    install_eticket()
}

pub fn uninstall_eticket_hooks() {
    uninstall_eticket()
}

pub fn is_eticket_hook_active() -> bool {
    ETICKET_ACTIVE.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;

    static ETICKET_TEST_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn test_fnv1a_hash_eticket() {
        assert_eq!(
            NETPACKET_ETICKET_GET_ENCRYPTED_APP_TICKET_FNHASH,
            fnv1a_hash("GetEncryptedAppTicket")
        );
        assert_eq!(
            NETPACKET_ETICKET_REQUEST_ENCRYPTED_APP_TICKET_FNHASH,
            fnv1a_hash("RequestEncryptedAppTicket")
        );
    }

    #[test]
    fn test_handler_registration_and_uninstall() {
        let _g = ETICKET_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        install_eticket().unwrap();
        assert!(is_eticket_hook_active());
        {
            let handlers = eticket_handlers().lock().unwrap();
            assert!(handlers.contains_key(&(
                IpcInterface::IClientUser,
                NETPACKET_ETICKET_GET_ENCRYPTED_APP_TICKET_FNHASH
            )));
            assert!(handlers.contains_key(&(
                IpcInterface::IClientUser,
                NETPACKET_ETICKET_REQUEST_ENCRYPTED_APP_TICKET_FNHASH
            )));
        }
        assert!(ipc::find_handler(
            IpcInterface::IClientUser,
            NETPACKET_ETICKET_GET_ENCRYPTED_APP_TICKET_FNHASH
        )
        .is_some());

        uninstall_eticket();
        assert!(!is_eticket_hook_active());
        assert!(ipc::find_handler(
            IpcInterface::IClientUser,
            NETPACKET_ETICKET_GET_ENCRYPTED_APP_TICKET_FNHASH
        )
        .is_none());
    }

    #[test]
    fn test_parse_eticket_steam_id_and_varint_validation() {
        let mut write_buf = vec![0u8; 36];
        write_buf[8..12].copy_from_slice(&1u32.to_le_bytes());
        write_buf[12..16].copy_from_slice(&20u32.to_le_bytes());
        write_buf[16] = 0x08 | ((1245620u32 as u8) & 0x7F);
        let test_steam_id = 76_561_198_028_121_353u64;
        write_buf[20..28].copy_from_slice(&test_steam_id.to_le_bytes());

        let result = parse_eticket_steam_id(&write_buf);
        assert_eq!(result, Some(test_steam_id));

        // Encoded varint at offset 24 inside ticket payload
        let mut varint_buf = vec![0u8; 40];
        varint_buf[12..16].copy_from_slice(&24u32.to_le_bytes());
        let mut encoded = Vec::new();
        encode_varint(test_steam_id, &mut encoded);
        varint_buf[24..24 + encoded.len()].copy_from_slice(&encoded);
        assert_eq!(parse_eticket_steam_id(&varint_buf), Some(test_steam_id));

        // Truncated varint (MSB set on final byte) and 10th-byte overflow must be rejected
        assert!(decode_varint(&[0x80, 0x80]).is_none());
        assert!(
            decode_varint(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02]).is_none()
        );
    }

    #[test]
    fn test_parse_eticket_steam_id_no_data() {
        let write_buf = vec![0u8; 10];
        assert!(parse_eticket_steam_id(&write_buf).is_none());
    }

    #[test]
    fn test_handle_recv_get_encrypted_app_ticket_response_exact_12_bytes() {
        let mut write_buf = vec![0u8; 12];
        write_buf[8..12].copy_from_slice(&2u32.to_le_bytes());
        let test_sid = 76_561_198_028_121_353u64;
        handle_recv_get_encrypted_app_ticket_response_with_steam_id(
            &mut write_buf,
            123456,
            test_sid,
        );
        assert_eq!(write_buf[8..12], 0u32.to_le_bytes());
        // Must have appended field 1 (app_id) + field 2 (steam_id) even when input was exactly 12 bytes
        assert!(write_buf.len() > 12);
        assert_eq!(write_buf[12], 0x08);
        let (decoded_app, n1) = decode_varint(&write_buf[13..]).unwrap();
        assert_eq!(decoded_app, 123456);
        assert_eq!(write_buf[13 + n1], 0x10);
        let (decoded_sid, _) = decode_varint(&write_buf[14 + n1..]).unwrap();
        assert_eq!(decoded_sid, test_sid);
    }
}
