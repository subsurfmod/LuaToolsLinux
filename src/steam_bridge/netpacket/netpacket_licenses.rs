//! NetPacket Licenses hook layer for Linux.
//!
//! Port of usefulsteamtools `Interop_NetPacket.cpp` namespace
//! `Hooks_NetPacket_Licenses` (lines 962-989).
//!
//! Handles incoming CMsgClientLicenseList (eMsg 780) - parses licenses
//! and feeds to Package 0 system via InteropPackage.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use crate::steam_bridge::interop::ipc::{
    self, fnv1a_hash, FuncHash, HandlerFn, IpcContext, IpcInterface,
};
use crate::steam_bridge::interop::{License, on_license_list, try_dump_owned_depots};

/// k_EMsgClientLicenseList = 780
pub const K_E_MSG_CLIENT_LICENSE_LIST: u32 = 780;

/// FNV-1a hash of the message name for IPC registration
const NETPACKET_LICENSES_CLIENT_LICENSE_LIST_FNHASH: FuncHash = fnv1a_hash("ClientLicenseList");

#[derive(Clone)]
pub struct LicensesHandler {
    pub interface_id: IpcInterface,
    pub func_hash: FuncHash,
    pub name: String,
    pub fencepost: u32,
    pub argc: u32,
    pub pre_handler: Option<HandlerFn>,
    pub post_handler: Option<HandlerFn>,
}

impl fmt::Debug for LicensesHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LicensesHandler")
            .field("interface_id", &self.interface_id)
            .field("func_hash", &self.func_hash)
            .field("name", &self.name)
            .finish()
    }
}

static LICENSES_HANDLERS: LazyLock<Mutex<HashMap<(IpcInterface, FuncHash), LicensesHandler>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static LICENSES_ACTIVE: AtomicBool = AtomicBool::new(false);

fn licenses_handlers() -> &'static Mutex<HashMap<(IpcInterface, FuncHash), LicensesHandler>> {
    &LICENSES_HANDLERS
}

/// Parse protobuf varint from slice
fn decode_varint(slice: &[u8]) -> Option<(u64, usize)> {
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

/// Parse CMsgClientLicenseList protobuf body.
///
/// Valve CMsgClientLicenseList structure (steam_messages.proto:272-295):
///   message CMsgClientLicenseList {
///     optional int32 eresult = 1 [default = 2];
///     repeated License licenses = 2; // canonical field 2 (also accepts field 1)
///   }
///   message License {
///     optional uint32 package_id = 1;
///     optional fixed32 time_created = 2;
///     ...
///     optional uint64 access_token = 17; // canonical field 17 (tag 0x88 0x01)
///   }
pub fn parse_client_license_list(body: &[u8]) -> Vec<License> {
    let mut licenses = Vec::new();
    let mut offset = 0;

    while offset < body.len() {
        let Some((tag, tag_bytes)) = decode_varint(&body[offset..]) else {
            break;
        };
        offset += tag_bytes;

        let field_num = (tag >> 3) as u32;
        let wire_type = (tag & 0x07) as u32;

        // Field 2 (canonical Valve licenses) or Field 1 (legacy/synthetic) = wire type 2 (length-delimited)
        if (field_num == 2 || field_num == 1) && wire_type == 2 {
            let Some((len, len_bytes)) = decode_varint(&body[offset..]) else {
                break;
            };
            offset += len_bytes;

            if offset + len as usize > body.len() {
                break;
            }

            let license_data = &body[offset..offset + len as usize];
            offset += len as usize;

            // Parse License message
            let mut license_offset = 0;
            let mut package_id = 0u32;
            let mut access_token = 0u64;

            while license_offset < license_data.len() {
                let Some((inner_tag, inner_tag_bytes)) = decode_varint(&license_data[license_offset..]) else {
                    break;
                };
                license_offset += inner_tag_bytes;

                let inner_field = (inner_tag >> 3) as u32;
                let inner_wire = (inner_tag & 0x07) as u32;

                if inner_field == 1 && inner_wire == 0 {
                    // package_id (varint)
                    let Some((val, bytes)) = decode_varint(&license_data[license_offset..]) else {
                        break;
                    };
                    package_id = val as u32;
                    license_offset += bytes;
                } else if (inner_field == 17 || inner_field == 2) && inner_wire == 0 {
                    // access_token (varint - canonical field 17 or synthetic field 2)
                    let Some((val, bytes)) = decode_varint(&license_data[license_offset..]) else {
                        break;
                    };
                    access_token = val;
                    license_offset += bytes;
                } else {
                    // Skip unknown inner field
                    match inner_wire {
                        0 => { // varint
                            if let Some((_, bytes)) = decode_varint(&license_data[license_offset..]) {
                                license_offset += bytes;
                            } else {
                                break;
                            }
                        }
                        1 => { // 64-bit
                            if license_offset + 8 <= license_data.len() {
                                license_offset += 8;
                            } else {
                                break;
                            }
                        }
                        2 => { // length-delimited
                            let Some((l, l_bytes)) = decode_varint(&license_data[license_offset..]) else {
                                break;
                            };
                            license_offset += l_bytes + l as usize;
                        }
                        5 => { // 32-bit (e.g. time_created = 2, fixed32)
                            if license_offset + 4 <= license_data.len() {
                                license_offset += 4;
                            } else {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
            }

            if package_id != 0 {
                licenses.push(License {
                    package_id,
                    access_token,
                });
            }
        } else {
            // Skip unknown top-level field (e.g. field 1 eresult varint)
            match wire_type {
                0 => { // varint
                    if let Some((_, bytes)) = decode_varint(&body[offset..]) {
                        offset += bytes;
                    } else {
                        break;
                    }
                }
                1 => { // 64-bit
                    if offset + 8 <= body.len() {
                        offset += 8;
                    } else {
                        break;
                    }
                }
                2 => { // length-delimited
                    let Some((len, len_bytes)) = decode_varint(&body[offset..]) else {
                        break;
                    };
                    offset += len_bytes + len as usize;
                }
                5 => { // 32-bit
                    if offset + 4 <= body.len() {
                        offset += 4;
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
    }

    licenses
}

/// Handle incoming CMsgClientLicenseList (recv post-handler).
///
/// The IPC context contains the protobuf body in read_buffer.
/// We parse it and feed to InteropPackage::OnLicenseList.
fn handle_recv_client_license_list_post(ctx: &mut IpcContext) -> Result<(), String> {
    // The protobuf body is in read_buffer (after the IPC header)
    // For NetPacket messages, the body typically starts after the message header
    let body = &ctx.read_buffer;

    if body.is_empty() {
        log::debug!("Licenses: empty body received");
        return Ok(());
    }

    let licenses = parse_client_license_list(body);

    if !licenses.is_empty() {
        log::info!("Licenses: parsed {} license(s) from CMsgClientLicenseList", licenses.len());
        for lic in &licenses {
            log::debug!("Licenses: package_id={}, access_token={}", lic.package_id, lic.access_token);
        }

        // Feed to Package 0 system
        on_license_list(licenses);

        // Try to dump owned depots now that we have licenses
        try_dump_owned_depots();
    } else {
        log::debug!("Licenses: no licenses found in CMsgClientLicenseList");
    }

    Ok(())
}

pub fn register_licenses_handlers() {
    let mut map = licenses_handlers().lock().unwrap_or_else(|e| e.into_inner());
    let iface = IpcInterface::IClientUser;

    let post: HandlerFn = Arc::new(|ctx: &mut IpcContext| -> Result<(), String> {
        handle_recv_client_license_list_post(ctx)
    });

    map.insert(
        (iface, NETPACKET_LICENSES_CLIENT_LICENSE_LIST_FNHASH),
        LicensesHandler {
            interface_id: iface,
            func_hash: NETPACKET_LICENSES_CLIENT_LICENSE_LIST_FNHASH,
            name: "Hooks_NetPacket_Licenses::HandleRecv_ClientLicenseList".to_string(),
            fencepost: 0,
            argc: 4,
            pre_handler: None,
            post_handler: Some(Arc::clone(&post)),
        },
    );
    drop(map);

    ipc::register_handler(ipc::make_handler(
        iface,
        NETPACKET_LICENSES_CLIENT_LICENSE_LIST_FNHASH,
        "Hooks_NetPacket_Licenses::HandleRecv_ClientLicenseList",
        0,
        4,
        None,
        Some(post),
    ));
}

pub fn unregister_licenses_handlers() {
    licenses_handlers()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    let iface = IpcInterface::IClientUser;
    let _ = ipc::unregister_handler(iface, NETPACKET_LICENSES_CLIENT_LICENSE_LIST_FNHASH);
}

pub fn install_licenses() -> Result<(), String> {
    register_licenses_handlers();
    LICENSES_ACTIVE.store(true, Ordering::Release);
    log::info!("Installed Licenses NetPacket hooks (eMsg={})", K_E_MSG_CLIENT_LICENSE_LIST);
    Ok(())
}

pub fn uninstall_licenses() {
    unregister_licenses_handlers();
    LICENSES_ACTIVE.store(false, Ordering::Release);
    log::info!("Uninstalled Licenses NetPacket hooks");
}

pub fn install_licenses_hooks() -> Result<(), String> {
    install_licenses()
}

pub fn uninstall_licenses_hooks() {
    uninstall_licenses()
}

pub fn is_licenses_hook_active() -> bool {
    LICENSES_ACTIVE.load(Ordering::Acquire)
}

/// IPC adapter for Recv: CMsgClientLicenseList (780)
/// Parses the license list from the NetPacket frame and feeds to Package 0 system.
pub fn handle_recv_client_license_list_post_ipc(ctx: &mut crate::steam_bridge::netpacket::netpacket_core::NetPacketContext) -> Result<(), String> {
    // The protobuf body starts at body_offset
    let body = &ctx.frame_buffer[ctx.body_offset..];

    if body.is_empty() {
        log::debug!("Licenses: empty body received");
        return Ok(());
    }

    let licenses = parse_client_license_list(body);

    if !licenses.is_empty() {
        log::info!("Licenses: parsed {} license(s) from CMsgClientLicenseList", licenses.len());
        for lic in &licenses {
            log::debug!("Licenses: package_id={}, access_token={}", lic.package_id, lic.access_token);
        }

        // Feed to Package 0 system
        on_license_list(licenses);

        // Try to dump owned depots now that we have licenses
        try_dump_owned_depots();
    } else {
        log::debug!("Licenses: no licenses found in CMsgClientLicenseList");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    static LICENSES_TEST_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn test_fnv1a_hash_licenses() {
        assert_eq!(
            NETPACKET_LICENSES_CLIENT_LICENSE_LIST_FNHASH,
            fnv1a_hash("ClientLicenseList")
        );
    }

    #[test]
    fn test_handler_registration_and_uninstall() {
        let _g = LICENSES_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        install_licenses().unwrap();
        assert!(is_licenses_hook_active());

        {
            let handlers = licenses_handlers().lock().unwrap();
            assert!(handlers.contains_key(&(
                IpcInterface::IClientUser,
                NETPACKET_LICENSES_CLIENT_LICENSE_LIST_FNHASH
            )));
        }
        assert!(ipc::find_handler(
            IpcInterface::IClientUser,
            NETPACKET_LICENSES_CLIENT_LICENSE_LIST_FNHASH
        ).is_some());

        uninstall_licenses();
        assert!(!is_licenses_hook_active());
        assert!(ipc::find_handler(
            IpcInterface::IClientUser,
            NETPACKET_LICENSES_CLIENT_LICENSE_LIST_FNHASH
        ).is_none());
    }

    #[test]
    fn test_decode_varint() {
        // Single byte
        assert_eq!(decode_varint(&[0x00]), Some((0, 1)));
        assert_eq!(decode_varint(&[0x7F]), Some((127, 1)));

        // Two bytes
        assert_eq!(decode_varint(&[0x80, 0x01]), Some((128, 2)));
        assert_eq!(decode_varint(&[0xFF, 0x01]), Some((255, 2)));

        // 64-bit value
        let val = 76_561_198_028_121_353u64; // SteamID64
        let mut encoded = Vec::new();
        encode_varint(val, &mut encoded);
        assert_eq!(decode_varint(&encoded), Some((val, encoded.len())));

        // Truncated varint rejected
        assert!(decode_varint(&[0x80]).is_none());
        assert!(decode_varint(&[0x80, 0x80]).is_none());

        // 10th byte overflow rejected
        assert!(decode_varint(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02]).is_none());
    }

    #[test]
    fn test_parse_client_license_list_empty() {
        let body = vec![];
        let licenses = parse_client_license_list(&body);
        assert!(licenses.is_empty());
    }

    #[test]
    fn test_parse_client_license_list_single() {
        // Build a minimal CMsgClientLicenseList with one License
        // License: package_id=123, access_token=456
        // Field 1 (licenses): tag=0x0A (field 1, wire type 2)
        // Length of license message: 4 bytes
        // License field 1 (package_id): tag=0x08, varint 123
        // License field 2 (access_token): tag=0x10, varint 456
        let mut body = Vec::new();
        body.push(0x0A); // field 1, wire type 2 (length-delimited)
        body.push(0x05); // length = 5
        body.push(0x08); // field 1, wire type 0 (varint)
        body.push(0x7B); // 123
        body.push(0x10); // field 2, wire type 0 (varint)
        body.push(0xC8); // 456 (0xC8 0x03)
        body.push(0x03); // continuation for 456 = 0xC8 0x03

        let licenses = parse_client_license_list(&body);
        assert_eq!(licenses.len(), 1);
        assert_eq!(licenses[0].package_id, 123);
        assert_eq!(licenses[0].access_token, 456);
    }

    #[test]
    fn test_parse_client_license_list_multiple() {
        // Build CMsgClientLicenseList with two licenses
        // License 1: package_id=100, access_token=200
        // License 2: package_id=300, access_token=400
        let mut body = Vec::new();

        // License 1
        body.push(0x0A); // field 1, wire type 2
        body.push(0x05); // length = 5
        body.push(0x08); body.push(0x64); // package_id = 100
        body.push(0x10); body.push(0xC8); body.push(0x01); // access_token = 200 (0xC8 0x01)

        // License 2
        body.push(0x0A); // field 1, wire type 2
        body.push(0x06); // length = 6
        body.push(0x08); body.push(0xAC); body.push(0x02); // package_id = 300 (0xAC 0x02)
        body.push(0x10); body.push(0x90); body.push(0x03); // access_token = 400 (0x90 0x03)

        let licenses = parse_client_license_list(&body);
        assert_eq!(licenses.len(), 2);
        assert_eq!(licenses[0].package_id, 100);
        assert_eq!(licenses[0].access_token, 200);
        assert_eq!(licenses[1].package_id, 300);
        assert_eq!(licenses[1].access_token, 400);
    }

    #[test]
    fn test_parse_client_license_list_zero_package_id_skipped() {
        // License with package_id=0 should be skipped
        let mut body = Vec::new();
        body.push(0x0A); // field 1
        body.push(0x04); // length = 4
        body.push(0x08); body.push(0x00); // package_id = 0
        body.push(0x10); body.push(0x01); // access_token = 1

        let licenses = parse_client_license_list(&body);
        assert!(licenses.is_empty());
    }

    #[test]
    fn test_parse_client_license_list_valve_wire_format() {
        // Build real Valve CMsgClientLicenseList:
        // field 1 (eresult): tag 0x08, value 1 (k_EResultOK)
        // field 2 (licenses): tag 0x12, length-delimited
        //   Inside License:
        //     field 1 (package_id): tag 0x08, varint 730 (0xDA, 0x05)
        //     field 2 (time_created): tag 0x15, fixed32 (0x78, 0x56, 0x34, 0x12)
        //     field 17 (access_token): tag (17<<3)|0 = 136 (0x88, 0x01), varint 0x112233445566
        let mut license_bytes = Vec::new();
        // package_id = 730
        license_bytes.push(0x08);
        encode_varint(730, &mut license_bytes);
        // time_created = 0x12345678 (fixed32: 4 bytes)
        license_bytes.push(0x15);
        license_bytes.extend_from_slice(&0x12345678_u32.to_le_bytes());
        // access_token = 0x112233445566
        encode_varint((17 << 3) | 0, &mut license_bytes); // tag for field 17 varint
        encode_varint(0x1122_3344_5566_u64, &mut license_bytes);

        let mut body = Vec::new();
        // eresult = 1
        body.push(0x08);
        body.push(0x01);
        // licenses = 2 (length-delimited)
        encode_varint((2 << 3) | 2, &mut body);
        encode_varint(license_bytes.len() as u64, &mut body);
        body.extend_from_slice(&license_bytes);

        let licenses = parse_client_license_list(&body);
        assert_eq!(licenses.len(), 1);
        assert_eq!(licenses[0].package_id, 730);
        assert_eq!(licenses[0].access_token, 0x1122_3344_5566_u64);
    }

    /// Helper to encode varint for tests
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
}