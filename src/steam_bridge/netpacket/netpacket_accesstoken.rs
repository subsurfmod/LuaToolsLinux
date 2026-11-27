//! NetPacket AccessToken hook layer for Linux (BST-32).
//!
//! Port of UsefulSteamTools / BetterSteamTools `Hooks_NetPacket.cpp` namespace
//! `Hooks_NetPacket_AccessToken` (lines 287-346).
//!
//! Handles outgoing `CMsgClientPICSProductInfoRequest` (eMsg 8903):
//! Injects 64-bit PICS `access_token` into outgoing app requests (field 2) and package
//! requests (field 1) for titles configured in Lua config / token store.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use crate::steam_bridge::interop::ipc::{
    self, fnv1a_hash, FuncHash, HandlerFn, IpcContext, IpcInterface,
};
use crate::steam_bridge::netpacket::netpacket_core::{
    register_netpacket_handler, NetPacketContext, NetPacketDirection,
};

/// FuncHash for CMsgClientPICSProductInfoRequest send handler.
pub const NETPACKET_ACCESSTOKEN_PICS_PRODUCT_INFO_REQUEST_FNHASH: FuncHash =
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

/// App access token store: app_id -> uint64 access_token.
static APP_ACCESS_TOKENS: LazyLock<Mutex<HashMap<u32, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Package access token store: package_id -> uint64 access_token.
static PACKAGE_ACCESS_TOKENS: LazyLock<Mutex<HashMap<u32, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn accesstoken_handlers() -> &'static Mutex<HashMap<(IpcInterface, FuncHash), AccessTokenHandler>> {
    &ACCESSTOKEN_HANDLERS
}

/// Configure an app access token (BST-32).
pub fn set_app_access_token(app_id: u32, token: u64) {
    if app_id != 0 && token != 0 {
        APP_ACCESS_TOKENS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(app_id, token);
    }
}

/// Retrieve an app access token.
pub fn get_app_access_token(app_id: u32) -> Option<u64> {
    APP_ACCESS_TOKENS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&app_id)
        .copied()
}

/// Configure a package access token (BST-32).
pub fn set_package_access_token(package_id: u32, token: u64) {
    if package_id != 0 && token != 0 {
        PACKAGE_ACCESS_TOKENS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(package_id, token);
    }
}

/// Retrieve a package access token.
pub fn get_package_access_token(package_id: u32) -> Option<u64> {
    PACKAGE_ACCESS_TOKENS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&package_id)
        .copied()
}

/// Clear all registered access tokens.
pub fn clear_access_tokens() {
    APP_ACCESS_TOKENS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    PACKAGE_ACCESS_TOKENS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// Parse protobuf varint from slice.
/// Returns (value, bytes_consumed) or None if truncated/overflow.
pub fn decode_varint(slice: &[u8]) -> Option<(u64, usize)> {
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

/// Encode a protobuf varint (MSB `0x80` set while more bytes remain).
pub fn encode_varint(mut val: u64, out: &mut Vec<u8>) {
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
pub fn parse_length_delimited(slice: &[u8]) -> Option<(&[u8], usize)> {
    let (len, consumed) = decode_varint(slice)?;
    let total = consumed + len as usize;
    if slice.len() < total {
        return None;
    }
    Some((&slice[consumed..total], total))
}

/// Valve AppInfo structure inside `CMsgClientPICSProductInfoRequest.apps` (field 2):
/// - `appid`: field 1, varint (uint32)
/// - `access_token`: field 2, varint (uint64)
/// - `only_public_obsolete`: field 3, varint (bool)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    pub appid: u32,
    pub access_token: Option<u64>,
    pub only_public_obsolete: Option<bool>,
    pub raw_unknown: Vec<u8>,
}

impl AppInfo {
    pub fn new(appid: u32) -> Self {
        Self {
            appid,
            access_token: None,
            only_public_obsolete: None,
            raw_unknown: Vec::new(),
        }
    }

    pub fn with_token(appid: u32, token: u64) -> Self {
        Self {
            appid,
            access_token: Some(token),
            only_public_obsolete: None,
            raw_unknown: Vec::new(),
        }
    }

    pub fn parse(data: &[u8]) -> Option<Self> {
        let mut appid = 0u32;
        let mut access_token = None;
        let mut only_public_obsolete = None;
        let mut raw_unknown = Vec::new();
        let mut offset = 0;

        while offset < data.len() {
            let field_start = offset;
            let (tag, tag_len) = decode_varint(&data[offset..])?;
            offset += tag_len;

            let field_num = tag >> 3;
            let wire_type = tag & 0x7;

            match (field_num, wire_type) {
                (1, 0) => {
                    let (val, consumed) = decode_varint(&data[offset..])?;
                    appid = val as u32;
                    offset += consumed;
                }
                (2, 0) => {
                    let (val, consumed) = decode_varint(&data[offset..])?;
                    access_token = Some(val);
                    offset += consumed;
                }
                (3, 0) => {
                    let (val, consumed) = decode_varint(&data[offset..])?;
                    only_public_obsolete = Some(val != 0);
                    offset += consumed;
                }
                _ => {
                    match wire_type {
                        0 => {
                            let (_, c) = decode_varint(&data[offset..])?;
                            offset += c;
                        }
                        1 => {
                            if offset + 8 <= data.len() {
                                offset += 8;
                            } else {
                                return None;
                            }
                        }
                        2 => {
                            let (_, c) = parse_length_delimited(&data[offset..])?;
                            offset += c;
                        }
                        5 => {
                            if offset + 4 <= data.len() {
                                offset += 4;
                            } else {
                                return None;
                            }
                        }
                        _ => return None,
                    }
                    raw_unknown.extend_from_slice(&data[field_start..offset]);
                }
            }
        }

        Some(Self {
            appid,
            access_token,
            only_public_obsolete,
            raw_unknown,
        })
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();

        // Field 1: appid (varint)
        out.push(0x08);
        encode_varint(self.appid as u64, &mut out);

        // Field 2: access_token (varint) if present
        if let Some(tok) = self.access_token {
            out.push(0x10);
            encode_varint(tok, &mut out);
        }

        // Field 3: only_public_obsolete (varint) if present
        if let Some(obs) = self.only_public_obsolete {
            out.push(0x18);
            encode_varint(if obs { 1 } else { 0 }, &mut out);
        }

        // Unknown preserved fields
        out.extend_from_slice(&self.raw_unknown);
        out
    }
}

/// Valve PackageInfo structure inside `CMsgClientPICSProductInfoRequest.packages` (field 1):
/// - `packageid`: field 1, varint (uint32)
/// - `access_token`: field 2, varint (uint64)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageInfo {
    pub packageid: u32,
    pub access_token: Option<u64>,
    pub raw_unknown: Vec<u8>,
}

impl PackageInfo {
    pub fn new(packageid: u32) -> Self {
        Self {
            packageid,
            access_token: None,
            raw_unknown: Vec::new(),
        }
    }

    pub fn with_token(packageid: u32, token: u64) -> Self {
        Self {
            packageid,
            access_token: Some(token),
            raw_unknown: Vec::new(),
        }
    }

    pub fn parse(data: &[u8]) -> Option<Self> {
        let mut packageid = 0u32;
        let mut access_token = None;
        let mut raw_unknown = Vec::new();
        let mut offset = 0;

        while offset < data.len() {
            let field_start = offset;
            let (tag, tag_len) = decode_varint(&data[offset..])?;
            offset += tag_len;

            let field_num = tag >> 3;
            let wire_type = tag & 0x7;

            match (field_num, wire_type) {
                (1, 0) => {
                    let (val, consumed) = decode_varint(&data[offset..])?;
                    packageid = val as u32;
                    offset += consumed;
                }
                (2, 0) => {
                    let (val, consumed) = decode_varint(&data[offset..])?;
                    access_token = Some(val);
                    offset += consumed;
                }
                _ => {
                    match wire_type {
                        0 => {
                            let (_, c) = decode_varint(&data[offset..])?;
                            offset += c;
                        }
                        1 => {
                            if offset + 8 <= data.len() {
                                offset += 8;
                            } else {
                                return None;
                            }
                        }
                        2 => {
                            let (_, c) = parse_length_delimited(&data[offset..])?;
                            offset += c;
                        }
                        5 => {
                            if offset + 4 <= data.len() {
                                offset += 4;
                            } else {
                                return None;
                            }
                        }
                        _ => return None,
                    }
                    raw_unknown.extend_from_slice(&data[field_start..offset]);
                }
            }
        }

        Some(Self {
            packageid,
            access_token,
            raw_unknown,
        })
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();

        // Field 1: packageid (varint)
        out.push(0x08);
        encode_varint(self.packageid as u64, &mut out);

        // Field 2: access_token (varint) if present
        if let Some(tok) = self.access_token {
            out.push(0x10);
            encode_varint(tok, &mut out);
        }

        // Unknown preserved fields
        out.extend_from_slice(&self.raw_unknown);
        out
    }
}

/// Canonical `CMsgClientPICSProductInfoRequest` (eMsg 8903):
/// - `packages`: repeated field 1, length-delimited
/// - `apps`: repeated field 2, length-delimited
/// - `meta_data_only`: field 3, bool
/// - `num_prev_failed`: field 4, uint32
/// - `sequence_number`: field 6, uint32
/// - `single_response`: field 7, bool
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PicsProductInfoRequest {
    pub packages: Vec<PackageInfo>,
    pub apps: Vec<AppInfo>,
    pub meta_data_only: Option<bool>,
    pub num_prev_failed: Option<u32>,
    pub sequence_number: Option<u32>,
    pub single_response: Option<bool>,
    pub raw_unknown: Vec<u8>,
}

/// Parse `CMsgClientPICSProductInfoRequest` from protobuf body bytes.
pub fn parse_pics_product_info_request(body: &[u8]) -> Option<PicsProductInfoRequest> {
    let mut req = PicsProductInfoRequest::default();
    let mut offset = 0;

    while offset < body.len() {
        let field_start = offset;
        let (tag, tag_len) = decode_varint(&body[offset..])?;
        offset += tag_len;

        let field_num = tag >> 3;
        let wire_type = tag & 0x7;

        match (field_num, wire_type) {
            (1, 2) => {
                let (pkg_bytes, consumed) = parse_length_delimited(&body[offset..])?;
                if let Some(pkg) = PackageInfo::parse(pkg_bytes) {
                    req.packages.push(pkg);
                }
                offset += consumed;
            }
            (2, 2) => {
                let (app_bytes, consumed) = parse_length_delimited(&body[offset..])?;
                if let Some(app) = AppInfo::parse(app_bytes) {
                    req.apps.push(app);
                }
                offset += consumed;
            }
            (3, 0) => {
                let (val, consumed) = decode_varint(&body[offset..])?;
                req.meta_data_only = Some(val != 0);
                offset += consumed;
            }
            (4, 0) => {
                let (val, consumed) = decode_varint(&body[offset..])?;
                req.num_prev_failed = Some(val as u32);
                offset += consumed;
            }
            (6, 0) => {
                let (val, consumed) = decode_varint(&body[offset..])?;
                req.sequence_number = Some(val as u32);
                offset += consumed;
            }
            (7, 0) => {
                let (val, consumed) = decode_varint(&body[offset..])?;
                req.single_response = Some(val != 0);
                offset += consumed;
            }
            _ => {
                match wire_type {
                    0 => {
                        let (_, c) = decode_varint(&body[offset..])?;
                        offset += c;
                    }
                    1 => {
                        if offset + 8 <= body.len() {
                            offset += 8;
                        } else {
                            return None;
                        }
                    }
                    2 => {
                        let (_, c) = parse_length_delimited(&body[offset..])?;
                        offset += c;
                    }
                    5 => {
                        if offset + 4 <= body.len() {
                            offset += 4;
                        } else {
                            return None;
                        }
                    }
                    _ => return None,
                }
                req.raw_unknown.extend_from_slice(&body[field_start..offset]);
            }
        }
    }

    Some(req)
}

/// Serialize `CMsgClientPICSProductInfoRequest` to protobuf body bytes.
pub fn serialize_pics_product_info_request(req: &PicsProductInfoRequest) -> Vec<u8> {
    let mut out = Vec::new();

    // Field 1: packages (repeated length-delimited)
    for pkg in &req.packages {
        out.push(0x0A);
        let pkg_bytes = pkg.serialize();
        encode_varint(pkg_bytes.len() as u64, &mut out);
        out.extend_from_slice(&pkg_bytes);
    }

    // Field 2: apps (repeated length-delimited)
    for app in &req.apps {
        out.push(0x12);
        let app_bytes = app.serialize();
        encode_varint(app_bytes.len() as u64, &mut out);
        out.extend_from_slice(&app_bytes);
    }

    // Field 3: meta_data_only (varint)
    if let Some(mdo) = req.meta_data_only {
        out.push(0x18);
        encode_varint(if mdo { 1 } else { 0 }, &mut out);
    }

    // Field 4: num_prev_failed (varint)
    if let Some(npf) = req.num_prev_failed {
        out.push(0x20);
        encode_varint(npf as u64, &mut out);
    }

    // Field 6: sequence_number (varint)
    if let Some(seq) = req.sequence_number {
        out.push(0x30);
        encode_varint(seq as u64, &mut out);
    }

    // Field 7: single_response (varint)
    if let Some(sr) = req.single_response {
        out.push(0x38);
        encode_varint(if sr { 1 } else { 0 }, &mut out);
    }

    // Unknown preserved fields
    out.extend_from_slice(&req.raw_unknown);
    out
}

/// NetPacket outbound send handler for `CMsgClientPICSProductInfoRequest` (eMsg 8903).
/// Injects access tokens into requested apps and packages matching UsefulSteamTools line 289-344.
pub fn handle_send_pics_product_info_request_netpacket(
    ctx: &mut NetPacketContext,
) -> Result<(), String> {
    let body = ctx.body();
    if body.is_empty() {
        return Ok(());
    }

    let Some(mut req) = parse_pics_product_info_request(body) else {
        log::debug!("[BST-32] Failed to parse CMsgClientPICSProductInfoRequest");
        return Ok(());
    };

    let mut needs_patch = false;
    let mut injected_apps = 0usize;
    let mut injected_pkgs = 0usize;

    // 1. App access token injection
    for app in &mut req.apps {
        if app.appid == 0 {
            continue;
        }
        if let Some(token) = get_app_access_token(app.appid) {
            if app.access_token != Some(token) {
                log::debug!(
                    "[BST-32] Injected App access_token: appid={} token={}",
                    app.appid,
                    token
                );
                app.access_token = Some(token);
                needs_patch = true;
                injected_apps += 1;
            }
        }
    }

    // 2. Package access token injection
    for pkg in &mut req.packages {
        if pkg.packageid == 0 {
            continue;
        }
        if let Some(token) = get_package_access_token(pkg.packageid) {
            if pkg.access_token != Some(token) {
                log::debug!(
                    "[BST-32] Injected Package access_token: pkgid={} token={}",
                    pkg.packageid,
                    token
                );
                pkg.access_token = Some(token);
                needs_patch = true;
                injected_pkgs += 1;
            }
        }
    }

    // 3. If any token was injected, re-serialize and replace frame
    if needs_patch {
        let new_body = serialize_pics_product_info_request(&req);
        let mut new_frame = Vec::with_capacity(ctx.body_offset + new_body.len());
        new_frame.extend_from_slice(&ctx.frame_buffer[..ctx.body_offset]);
        new_frame.extend_from_slice(&new_body);
        ctx.replace_frame(new_frame);
        log::info!(
            "[BST-32] Injected PICS access tokens: {} app(s), {} package(s)",
            injected_apps,
            injected_pkgs
        );
    }

    Ok(())
}

/// IPC post-handler for CMsgClientPICSProductInfoRequest.
pub fn handle_send_pics_product_info_request_post(ctx: &mut IpcContext) {
    if ctx.write_buffer.len() < 12 {
        return;
    }
    let body_start = 12;
    if ctx.write_buffer.len() <= body_start {
        return;
    }

    let body = &ctx.write_buffer[body_start..];
    let Some(mut req) = parse_pics_product_info_request(body) else {
        return;
    };

    let mut needs_patch = false;
    for app in &mut req.apps {
        if app.appid == 0 {
            continue;
        }
        if let Some(token) = get_app_access_token(app.appid) {
            if app.access_token != Some(token) {
                app.access_token = Some(token);
                needs_patch = true;
            }
        }
    }

    for pkg in &mut req.packages {
        if pkg.packageid == 0 {
            continue;
        }
        if let Some(token) = get_package_access_token(pkg.packageid) {
            if pkg.access_token != Some(token) {
                pkg.access_token = Some(token);
                needs_patch = true;
            }
        }
    }

    if needs_patch {
        let new_body = serialize_pics_product_info_request(&req);
        ctx.write_buffer.truncate(body_start);
        ctx.write_buffer.extend_from_slice(&new_body);
    }
}

/// Register AccessToken NetPacket and IPC handlers.
pub fn register_accesstoken_handlers() {
    let iface = IpcInterface::IClientUser;

    let pics_post: HandlerFn = Arc::new(|ctx: &mut IpcContext| -> Result<(), String> {
        handle_send_pics_product_info_request_post(ctx);
        Ok(())
    });

    let handler = AccessTokenHandler {
        interface_id: iface,
        func_hash: NETPACKET_ACCESSTOKEN_PICS_PRODUCT_INFO_REQUEST_FNHASH,
        name: "Hooks_NetPacket_AccessToken::HandleSend_CMsgClientPICSProductInfoRequest".to_string(),
        fencepost: 0,
        argc: 4,
        pre_handler: None,
        post_handler: Some(Arc::clone(&pics_post)),
    };

    accesstoken_handlers()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert((iface, NETPACKET_ACCESSTOKEN_PICS_PRODUCT_INFO_REQUEST_FNHASH), handler);

    ipc::register_handler(ipc::make_handler(
        iface,
        NETPACKET_ACCESSTOKEN_PICS_PRODUCT_INFO_REQUEST_FNHASH,
        "Hooks_NetPacket_AccessToken::HandleSend_CMsgClientPICSProductInfoRequest",
        0,
        4,
        None,
        Some(pics_post),
    ));

    // Also register in NetPacket core dispatcher for eMsg 8903
    register_netpacket_handler(crate::steam_bridge::netpacket::netpacket_core::NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: 8903,
        target_job_hash: None,
        name: "AccessToken::HandleSend_PICSProductInfoRequest".to_string(),
        handler: Arc::new(|ctx| {
            handle_send_pics_product_info_request_netpacket(ctx)
        }),
    });
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

    static TEST_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn test_appinfo_and_packageinfo_canonical_wire_codec() {
        let app = AppInfo {
            appid: 1245620,
            access_token: Some(0x1122_3344_5566_7788),
            only_public_obsolete: Some(true),
            raw_unknown: vec![],
        };
        let bytes = app.serialize();
        let parsed = AppInfo::parse(&bytes).unwrap();
        assert_eq!(parsed.appid, 1245620);
        assert_eq!(parsed.access_token, Some(0x1122_3344_5566_7788));
        assert_eq!(parsed.only_public_obsolete, Some(true));

        let pkg = PackageInfo {
            packageid: 54321,
            access_token: Some(9988776655),
            raw_unknown: vec![],
        };
        let pkg_bytes = pkg.serialize();
        let parsed_pkg = PackageInfo::parse(&pkg_bytes).unwrap();
        assert_eq!(parsed_pkg.packageid, 54321);
        assert_eq!(parsed_pkg.access_token, Some(9988776655));
    }

    #[test]
    fn test_pics_product_info_request_roundtrip_with_tokens() {
        let req = PicsProductInfoRequest {
            packages: vec![
                PackageInfo::with_token(100, 1000),
                PackageInfo::new(200),
            ],
            apps: vec![
                AppInfo::new(730),
                AppInfo::with_token(1245620, 88888888),
            ],
            meta_data_only: Some(true),
            num_prev_failed: Some(0),
            sequence_number: Some(42),
            single_response: Some(false),
            raw_unknown: vec![],
        };

        let body = serialize_pics_product_info_request(&req);
        let parsed = parse_pics_product_info_request(&body).unwrap();

        assert_eq!(parsed.packages.len(), 2);
        assert_eq!(parsed.packages[0].packageid, 100);
        assert_eq!(parsed.packages[0].access_token, Some(1000));
        assert_eq!(parsed.packages[1].packageid, 200);
        assert_eq!(parsed.packages[1].access_token, None);

        assert_eq!(parsed.apps.len(), 2);
        assert_eq!(parsed.apps[0].appid, 730);
        assert_eq!(parsed.apps[0].access_token, None);
        assert_eq!(parsed.apps[1].appid, 1245620);
        assert_eq!(parsed.apps[1].access_token, Some(88888888));

        assert_eq!(parsed.meta_data_only, Some(true));
        assert_eq!(parsed.sequence_number, Some(42));
    }

    #[test]
    fn test_netpacket_accesstoken_injection() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        clear_access_tokens();

        set_app_access_token(730, 0xAA_BB_CC_DD_EE);
        set_package_access_token(200, 0x12_34_56_78);

        let initial_req = PicsProductInfoRequest {
            packages: vec![PackageInfo::new(200)],
            apps: vec![AppInfo::new(730), AppInfo::new(480)],
            meta_data_only: Some(false),
            num_prev_failed: None,
            sequence_number: Some(1),
            single_response: None,
            raw_unknown: vec![],
        };

        let body = serialize_pics_product_info_request(&initial_req);
        let mut frame = Vec::new();
        // MsgHdr (8 bytes: eMsg 8903, headerLength 20) + job ids (12 bytes) = 20 bytes header
        frame.extend_from_slice(&[0x87, 0x22, 0x00, 0x80]); // eMsg = 8903 | 0x80000000
        frame.extend_from_slice(&20u32.to_le_bytes());       // header_length = 20
        frame.extend_from_slice(&[0u8; 12]);                // job ids
        frame.extend_from_slice(&body);

        let mut ctx = NetPacketContext::new(frame, NetPacketDirection::Send, 730).unwrap();
        let res = handle_send_pics_product_info_request_netpacket(&mut ctx);
        assert!(res.is_ok());

        // Verify the injected tokens in the modified frame
        let modified = parse_pics_product_info_request(ctx.body()).unwrap();
        assert_eq!(modified.apps[0].appid, 730);
        assert_eq!(modified.apps[0].access_token, Some(0xAA_BB_CC_DD_EE));
        assert_eq!(modified.apps[1].appid, 480);
        assert_eq!(modified.apps[1].access_token, None); // Not in token store
        assert_eq!(modified.packages[0].packageid, 200);
        assert_eq!(modified.packages[0].access_token, Some(0x12_34_56_78));

        clear_access_tokens();
    }
}
