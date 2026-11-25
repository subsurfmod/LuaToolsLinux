//! NetPacket AccessToken hook layer for Linux.
//!
//! Port of usefulsteamtools `Interop_NetPacket.cpp` namespace
//! `Hooks_NetPacket_AccessToken` (lines 287-346).
//!
//! Handles outgoing `CMsgClientPICSProductInfoRequest` (eMsg 8903) -
//! injects access_token for apps in Lua config.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use crate::steam_bridge::interop::ipc::{
    self, fnv1a_hash, FuncHash, HandlerFn, IpcContext, IpcInterface,
};

/// Steam message ID for CMsgClientPICSProductInfoRequest.
const K_EMSG_CLIENT_PICS_PRODUCT_INFO_REQUEST: u32 = 8903;
/// Maximum body size for NetPacket messages.
const K_MAX_BODY_SIZE: usize = 65536;

/// FuncHash for CMsgClientPICSProductInfoRequest send handler.
const NETPACKET_ACCESSTOKEN_PICS_PRODUCT_INFO_REQUEST_FNHASH: FuncHash =
    fnv1a_hash("CMsgClientPICSProductInfoRequest");

/// AccessToken handler definition.
#[derive(Clone)]
pub struct AccessTokenHandler {
    pub interface_id: IpcInterface,
    pub func_hash: FuncHash,
    pub name: String,
    pub fencepost: u32,
    pub argc: u32,
    pub pre_handler: Option<HandlerFn>,
    pub post_handler: Option<HandlerFn>,
}

impl fmt::Debug for AccessTokenHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessTokenHandler")
            .field("interface_id", &self.interface_id)
            .field("func_hash", &self.func_hash)
            .field("name", &self.name)
            .finish()
    }
}

static ACCESSTOKEN_HANDLERS: LazyLock<Mutex<HashMap<(IpcInterface, FuncHash), AccessTokenHandler>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static ACCESSTOKEN_ACTIVE: AtomicBool = AtomicBool::new(false);

fn accesstoken_handlers() -> &'static Mutex<HashMap<(IpcInterface, FuncHash), AccessTokenHandler>> {
    &ACCESSTOKEN_HANDLERS
}

/// Register all AccessToken handlers.
pub fn register_accesstoken_handlers() {
    let mut map = accesstoken_handlers()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let iface = IpcInterface::IClientUser;

    let pics_post: HandlerFn = Arc::new(|ctx: &mut IpcContext| -> Result<(), String> {
        handle_send_pics_product_info_request_post(ctx);
        Ok(())
    });

    map.insert(
        (iface, NETPACKET_ACCESSTOKEN_PICS_PRODUCT_INFO_REQUEST_FNHASH),
        AccessTokenHandler {
            interface_id: iface,
            func_hash: NETPACKET_ACCESSTOKEN_PICS_PRODUCT_INFO_REQUEST_FNHASH,
            name: "Hooks_NetPacket_AccessToken::HandleSend_CMsgClientPICSProductInfoRequest".to_string(),
            fencepost: 0,
            argc: 4,
            pre_handler: None,
            post_handler: Some(Arc::clone(&pics_post)),
        },
    );
    drop(map);

    ipc::register_handler(ipc::make_handler(
        iface,
        NETPACKET_ACCESSTOKEN_PICS_PRODUCT_INFO_REQUEST_FNHASH,
        "Hooks_NetPacket_AccessToken::HandleSend_CMsgClientPICSProductInfoRequest",
        0,
        4,
        None,
        Some(pics_post),
    ));
}

/// Unregister all AccessToken handlers.
pub fn unregister_accesstoken_handlers() {
    accesstoken_handlers()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    let iface = IpcInterface::IClientUser;
    let _ = ipc::unregister_handler(iface, NETPACKET_ACCESSTOKEN_PICS_PRODUCT_INFO_REQUEST_FNHASH);
}

/// Parse protobuf varint from slice.
/// Returns (value, bytes_consumed) or None if truncated/overflow.
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

/// Parse a length-delimited field from protobuf wire format.
/// Returns (field_data, bytes_consumed) or None if invalid.
fn parse_length_delimited(slice: &[u8]) -> Option<(&[u8], usize)> {
    let (len, consumed) = decode_varint(slice)?;
    let total = consumed + len as usize;
    if slice.len() < total {
        return None;
    }
    Some((&slice[consumed..total], total))
}

/// PICS Product Info Request protobuf message fields.
/// Based on Steam's CMsgClientPICSProductInfoRequest:
/// - message CMsgClientPICSProductInfoRequest {
///     repeated ProductInfoRequest apps = 1;
///     // ...
///   }
/// - message ProductInfoRequest {
///     uint32 appid = 1;
///     // ...
///     string access_token = 10;  // Field 10 for access token
///   }
struct ProductInfoRequest {
    appid: u32,
    access_token: Option<String>,
    raw_data: Vec<u8>,  // Original raw data for reconstruction
}

impl ProductInfoRequest {
    /// Parse a ProductInfoRequest from protobuf wire format.
    fn parse(data: &[u8]) -> Option<Self> {
        let mut appid = 0u32;
        let mut access_token = None;
        let mut offset = 0;

        while offset < data.len() {
            let (tag, tag_len) = decode_varint(&data[offset..])?;
            offset += tag_len;

            let field_num = tag >> 3;
            let wire_type = tag & 0x7;

            match (field_num, wire_type) {
                (1, 0) => {  // appid (varint)
                    let (val, consumed) = decode_varint(&data[offset..])?;
                    appid = val as u32;
                    offset += consumed;
                }
                (10, 2) => {  // access_token (length-delimited string)
                    let (token_data, consumed) = parse_length_delimited(&data[offset..])?;
                    access_token = String::from_utf8(token_data.to_vec()).ok();
                    offset += consumed;
                }
                _ => {
                    // Skip unknown fields
                    match wire_type {
                        0 => { // varint
                            let (_, consumed) = decode_varint(&data[offset..])?;
                            offset += consumed;
                        }
                        1 => { // 64-bit
                            if offset + 8 <= data.len() {
                                offset += 8;
                            } else {
                                return None;
                            }
                        }
                        2 => { // length-delimited
                            let (_, consumed) = parse_length_delimited(&data[offset..])?;
                            offset += consumed;
                        }
                        5 => { // 32-bit
                            if offset + 4 <= data.len() {
                                offset += 4;
                            } else {
                                return None;
                            }
                        }
                        _ => return None,
                    }
                }
            }
        }

        Some(ProductInfoRequest {
            appid,
            access_token,
            raw_data: data.to_vec(),
        })
    }

    /// Serialize the ProductInfoRequest back to protobuf wire format.
    fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();

        // Field 1: appid (varint)
        out.push(0x08);  // field 1, wire type 0
        encode_varint(self.appid as u64, &mut out);

        // Field 10: access_token (length-delimited) if present
        if let Some(token) = &self.access_token {
            out.push(0x52);  // field 10, wire type 2
            let token_bytes = token.as_bytes();
            encode_varint(token_bytes.len() as u64, &mut out);
            out.extend_from_slice(token_bytes);
        }

        out
    }
}

/// Parse the CMsgClientPICSProductInfoRequest message body.
/// Returns a vector of ProductInfoRequest for each app in the request.
fn parse_pics_product_info_request(body: &[u8]) -> Option<Vec<ProductInfoRequest>> {
    let mut requests = Vec::new();
    let mut offset = 0;

    while offset < body.len() {
        let (tag, tag_len) = decode_varint(&body[offset..])?;
        offset += tag_len;

        let field_num = tag >> 3;
        let wire_type = tag & 0x7;

        match (field_num, wire_type) {
            (1, 2) => {  // apps (repeated length-delimited)
                let (app_data, consumed) = parse_length_delimited(&body[offset..])?;
                if let Some(req) = ProductInfoRequest::parse(app_data) {
                    requests.push(req);
                }
                offset += consumed;
            }
            _ => {
                // Skip unknown fields
                match wire_type {
                    0 => { let (_, c) = decode_varint(&body[offset..])?; offset += c; }
                    1 => { if offset + 8 <= body.len() { offset += 8; } else { return None; } }
                    2 => { let (_, c) = parse_length_delimited(&body[offset..])?; offset += c; }
                    5 => { if offset + 4 <= body.len() { offset += 4; } else { return None; } }
                    _ => return None,
                }
            }
        }
    }

    Some(requests)
}

/// Serialize the CMsgClientPICSProductInfoRequest message body.
fn serialize_pics_product_info_request(requests: &[ProductInfoRequest]) -> Vec<u8> {
    let mut out = Vec::new();

    for req in requests {
        // Field 1: apps (repeated length-delimited)
        out.push(0x0A);  // field 1, wire type 2
        let app_data = req.serialize();
        encode_varint(app_data.len() as u64, &mut out);
        out.extend_from_slice(&app_data);
    }

    out
}

/// Check if an app has a depot configured in Lua config.
/// Uses interop_package::has_depot which checks owned depots from LicenseList.
fn lua_config_has_depot(app_id: u32) -> bool {
    crate::steam_bridge::interop::interop_package::has_depot(app_id)
}

/// Get access token for an app from Lua config.
/// Currently returns None as LuaConfig integration is a placeholder.
/// TODO: Integrate with Lua parser's tokens map (addtoken directive).
fn lua_config_get_access_token(_app_id: u32) -> Option<String> {
    // Placeholder - will be wired up when LuaConfig is fully implemented
    // The tokens are parsed from addtoken(appid, "token") in lua_parser.rs
    // and stored in LuaContents::tokens
    None
}

/// Handle outgoing CMsgClientPICSProductInfoRequest (eMsg 8903) send.
/// Injects access_token into each app that has a depot configured and an access token.
fn handle_send_pics_product_info_request_post(ctx: &mut IpcContext) {
    // The message body starts after the IPC header (12 bytes) and protobuf header
    // For NetPacket messages, the body is typically in write_buffer
    if ctx.write_buffer.len() < 12 {
        return;
    }

    // Parse the body as CMsgClientPICSProductInfoRequest
    let body_start = 12;  // Skip IPC header
    if ctx.write_buffer.len() <= body_start {
        return;
    }

    let body = &ctx.write_buffer[body_start..];

    // Parse the request
    let Some(mut requests) = parse_pics_product_info_request(body) else {
        log::debug!("AccessToken: Failed to parse PICS ProductInfoRequest");
        return;
    };

    // Check each app - if it has a depot and an access token, inject it
    let mut needs_patch = false;
    for req in &mut requests {
        if req.appid == 0 {
            continue;
        }

        // Check if this app has a depot configured in Lua config
        if lua_config_has_depot(req.appid) {
            // Get access token from Lua config
            if let Some(token) = lua_config_get_access_token(req.appid) {
                if req.access_token.is_none() || req.access_token.as_ref() != Some(&token) {
                    req.access_token = Some(token);
                    needs_patch = true;
                    log::debug!(
                        "AccessToken: Injecting access_token for appId={}",
                        req.appid
                    );
                }
            }
        }
    }

    // If any apps needed patching, serialize the modified request back
    if needs_patch {
        let new_body = serialize_pics_product_info_request(&requests);

        // Replace the body in write_buffer (equivalent to g_SendNewBody in C++)
        ctx.write_buffer.truncate(body_start);
        ctx.write_buffer.extend_from_slice(&new_body);

        log::debug!(
            "AccessToken: Patched PICS ProductInfoRequest - new body size: {}",
            new_body.len()
        );
    }
}

pub fn install_accesstoken() -> Result<(), String> {
    register_accesstoken_handlers();
    ACCESSTOKEN_ACTIVE.store(true, Ordering::Release);
    log::info!("Installed AccessToken NetPacket hooks");
    Ok(())
}

pub fn uninstall_accesstoken() {
    unregister_accesstoken_handlers();
    ACCESSTOKEN_ACTIVE.store(false, Ordering::Release);
    log::info!("Uninstalled AccessToken NetPacket hooks");
}

pub fn install_accesstoken_hooks() -> Result<(), String> {
    install_accesstoken()
}

pub fn uninstall_accesstoken_hooks() {
    uninstall_accesstoken()
}

pub fn is_accesstoken_hook_active() -> bool {
    ACCESSTOKEN_ACTIVE.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;

    static ACCESSTOKEN_TEST_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn test_fnv1a_hash_accesstoken() {
        assert_eq!(
            NETPACKET_ACCESSTOKEN_PICS_PRODUCT_INFO_REQUEST_FNHASH,
            fnv1a_hash("CMsgClientPICSProductInfoRequest")
        );
    }

    #[test]
    fn test_handler_registration_and_uninstall() {
        let _g = ACCESSTOKEN_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        install_accesstoken().unwrap();
        assert!(is_accesstoken_hook_active());

        {
            let handlers = accesstoken_handlers().lock().unwrap();
            assert!(handlers.contains_key(&(
                IpcInterface::IClientUser,
                NETPACKET_ACCESSTOKEN_PICS_PRODUCT_INFO_REQUEST_FNHASH
            )));
        }
        assert!(ipc::find_handler(
            IpcInterface::IClientUser,
            NETPACKET_ACCESSTOKEN_PICS_PRODUCT_INFO_REQUEST_FNHASH
        ).is_some());

        uninstall_accesstoken();
        assert!(!is_accesstoken_hook_active());
        assert!(ipc::find_handler(
            IpcInterface::IClientUser,
            NETPACKET_ACCESSTOKEN_PICS_PRODUCT_INFO_REQUEST_FNHASH
        ).is_none());
    }

    #[test]
    fn test_decode_varint() {
        // Single byte
        assert_eq!(decode_varint(&[0x01]), Some((1, 1)));
        assert_eq!(decode_varint(&[0x7F]), Some((127, 1)));

        // Multi-byte
        assert_eq!(decode_varint(&[0x80, 0x01]), Some((128, 2)));
        // 268435455 = 0xFFFFFFF encoded as varint: [0xFF, 0xFF, 0xFF, 0x7F] (4 bytes)
        assert_eq!(decode_varint(&[0xFF, 0xFF, 0xFF, 0x7F]), Some((268435455, 4)));

        // Truncated
        assert!(decode_varint(&[0x80]).is_none());

        // 10th byte overflow
        assert!(decode_varint(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02]).is_none());
    }

    #[test]
    fn test_encode_varint() {
        let mut out = Vec::new();
        encode_varint(1, &mut out);
        assert_eq!(out, vec![0x01]);

        out.clear();
        encode_varint(127, &mut out);
        assert_eq!(out, vec![0x7F]);

        out.clear();
        encode_varint(128, &mut out);
        assert_eq!(out, vec![0x80, 0x01]);

        out.clear();
        encode_varint(300, &mut out);
        assert_eq!(out, vec![0xAC, 0x02]);

        out.clear();
        encode_varint(268435455, &mut out);
        assert_eq!(out, vec![0xFF, 0xFF, 0xFF, 0x7F]);
    }

    #[test]
    fn test_parse_length_delimited() {
        // String "hello" (5 bytes)
        let data = vec![0x05, b'h', b'e', b'l', b'l', b'o'];
        let (result, consumed) = parse_length_delimited(&data).unwrap();
        assert_eq!(result, b"hello");
        assert_eq!(consumed, 6);

        // Empty string
        let data = vec![0x00];
        let (result, consumed) = parse_length_delimited(&data).unwrap();
        assert_eq!(result, b"");
        assert_eq!(consumed, 1);

        // Truncated
        assert!(parse_length_delimited(&[0x05, b'h', b'e']).is_none());
    }

    #[test]
    fn test_product_info_request_parse_and_serialize() {
        // Build a ProductInfoRequest with appid=730 and access_token="test_token"
        let mut req = ProductInfoRequest {
            appid: 730,
            access_token: Some("test_token".to_string()),
            raw_data: vec![],
        };

        let serialized = req.serialize();

        // Parse it back
        let parsed = ProductInfoRequest::parse(&serialized).unwrap();
        assert_eq!(parsed.appid, 730);
        assert_eq!(parsed.access_token, Some("test_token".to_string()));

        // Test without access_token
        let req_no_token = ProductInfoRequest {
            appid: 730,
            access_token: None,
            raw_data: vec![],
        };
        let serialized = req_no_token.serialize();
        let parsed = ProductInfoRequest::parse(&serialized).unwrap();
        assert_eq!(parsed.appid, 730);
        assert_eq!(parsed.access_token, None);
    }

    #[test]
    fn test_parse_pics_product_info_request() {
        // Create a request with two apps
        let mut req1 = ProductInfoRequest {
            appid: 730,
            access_token: Some("token1".to_string()),
            raw_data: vec![],
        };
        let mut req2 = ProductInfoRequest {
            appid: 570,
            access_token: None,
            raw_data: vec![],
        };

        let body = serialize_pics_product_info_request(&[req1, req2]);
        let parsed = parse_pics_product_info_request(&body).unwrap();

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].appid, 730);
        assert_eq!(parsed[0].access_token, Some("token1".to_string()));
        assert_eq!(parsed[1].appid, 570);
        assert_eq!(parsed[1].access_token, None);
    }

    #[test]
    fn test_serialize_pics_product_info_request() {
        let mut req1 = ProductInfoRequest {
            appid: 730,
            access_token: Some("token1".to_string()),
            raw_data: vec![],
        };
        let mut req2 = ProductInfoRequest {
            appid: 570,
            access_token: Some("token2".to_string()),
            raw_data: vec![],
        };

        let body = serialize_pics_product_info_request(&[req1, req2]);
        assert!(!body.is_empty());

        // Verify it parses back correctly
        let parsed = parse_pics_product_info_request(&body).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].appid, 730);
        assert_eq!(parsed[0].access_token, Some("token1".to_string()));
        assert_eq!(parsed[1].appid, 570);
        assert_eq!(parsed[1].access_token, Some("token2".to_string()));
    }
}