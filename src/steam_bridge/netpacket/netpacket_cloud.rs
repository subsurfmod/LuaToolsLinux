//! NetPacket Cloud hook layer for Linux.
//!
//! Port of usefulsteamtools `Hooks_NetPacket_Cloud` namespace in Interop_NetPacket.cpp.

use std::collections::VecDeque;
use std::fmt;
use std::os::raw::c_void;
use std::sync::{Arc, LazyLock, Mutex};

use crate::steam_bridge::cloud_redirect::{CloudRedirectHost, is_cloud_redirect_active};
use crate::steam_bridge::interop::interop_misc::resolve_app_id;
use crate::steam_bridge::interop::ipc::{
    self, fnv1a_hash, FuncHash, HandlerFn, IpcContext, IpcInterface,
};
use crate::steam_bridge::netpacket::netpacket_core::{
    NetPacketContext, NetPacketDirection, register_netpacket_handler,
};

const NETPACKET_CLOUD_SERVICE_METHOD_CALL_FNHASH: FuncHash = fnv1a_hash("ServiceMethodCallFromClient");

/// Cloud RPC handler registration.
#[derive(Clone)]
pub struct CloudHandler {
    pub interface_id: IpcInterface,
    pub func_hash: FuncHash,
    pub name: String,
    pub fencepost: u32,
    pub argc: u32,
    pub pre_handler: Option<HandlerFn>,
    pub post_handler: Option<HandlerFn>,
}

impl fmt::Debug for CloudHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CloudHandler")
            .field("interface_id", &self.interface_id)
            .field("func_hash", &self.func_hash)
            .field("name", &self.name)
            .finish()
    }
}

static CLOUD_HANDLERS: LazyLock<Mutex<Vec<CloudHandler>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static CLOUD_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Pending Cloud responses to inject via carrier packets.
static PENDING_CLOUD_RESPONSES: LazyLock<Mutex<VecDeque<Vec<u8>>>> = LazyLock::new(|| Mutex::new(VecDeque::new()));

/// Local SteamID captured from outbound headers.
static LOCAL_STEAM_ID: LazyLock<Mutex<u64>> = LazyLock::new(|| Mutex::new(0));

fn cloud_handlers() -> &'static Mutex<Vec<CloudHandler>> {
    &CLOUD_HANDLERS
}

fn pending_responses() -> &'static Mutex<VecDeque<Vec<u8>>> {
    &PENDING_CLOUD_RESPONSES
}

fn local_steam_id() -> &'static Mutex<u64> {
    &LOCAL_STEAM_ID
}

/// Read an unsigned protobuf varint from a byte slice.
/// Returns (value, bytes_consumed) or None if truncated/malformed.
fn read_varint(data: &[u8]) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    for (idx, &byte) in data.iter().enumerate() {
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

/// Find a specific varint field by tag number.
fn find_varint_field(data: &[u8], target_field: u32) -> Option<u64> {
    let mut pos = 0;
    while pos < data.len() {
        let (tag, consumed) = read_varint(&data[pos..])?;
        pos += consumed;
        let field = (tag >> 3) as u32;
        let wire_type = (tag & 7) as u32;
        if field == target_field && wire_type == 0 {
            return read_varint(&data[pos..]).map(|(v, _)| v);
        }
        match wire_type {
            0 => { let _ = read_varint(&data[pos..])?; }
            1 => { if pos + 8 > data.len() { return None; } pos += 8; }
            2 => {
                let (len, consumed) = read_varint(&data[pos..])?;
                pos += consumed;
                if pos + len as usize > data.len() { return None; }
                pos += len as usize;
            }
            5 => { if pos + 4 > data.len() { return None; } pos += 4; }
            _ => return None,
        }
    }
    None
}

/// Extract appid from Cloud RPC request body.
/// Field 1 for all Cloud.* except ClientCommitFileUpload (field 2).
fn extract_app_id(job_name: &str, body: &[u8]) -> u32 {
    let field_num = if job_name == "Cloud.ClientCommitFileUpload#1" { 2 } else { 1 };
    find_varint_field(body, field_num).map(|v| v as u32).unwrap_or(0)
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
        if val == 0 { break; }
    }
}

/// Build a ServiceMethodResponse header protobuf.
fn build_response_header(job_id: u64, job_name: &str, eresult: i32) -> Vec<u8> {
    let mut hdr = Vec::new();
    // field 1: target_job_name (string)
    hdr.push(0x0A); // tag 1, wire type 2
    let name_bytes = job_name.as_bytes();
    encode_varint(name_bytes.len() as u64, &mut hdr);
    hdr.extend_from_slice(name_bytes);
    // field 2: jobid_target (varint)
    hdr.push(0x10); // tag 2, wire type 0
    encode_varint(job_id, &mut hdr);
    // field 3: eresult (varint, zigzag for negative but eresult is positive)
    hdr.push(0x18); // tag 3, wire type 0
    encode_varint(eresult as u64, &mut hdr);
    hdr
}

/// Handle outbound ServiceMethodCallFromClient for Cloud.* RPCs.
pub fn handle_send_cloud_rpc(ctx: &mut IpcContext) -> Result<(), String> {
    if !is_cloud_redirect_active() {
        return Ok(());
    }

    // Capture local SteamID from header if present
    if ctx.read_buffer.len() >= 8 {
        let mut hdr_buf = [0u8; 8];
        hdr_buf.copy_from_slice(&ctx.read_buffer[0..8]);
        if let Some((steam_id, _)) = read_varint(&hdr_buf) {
            if steam_id != 0 {
                *local_steam_id().lock().unwrap() = steam_id;
            }
        }
    }

    // Parse job name from header (first varint after MsgHdr)
    // We need the target_job_name field from the CMsgProtoBufHeader
    // For simplicity, we'll check if the context has job_name info
    let job_name = ctx.job_name.as_deref().unwrap_or("");
    if !job_name.starts_with("Cloud.") {
        return Ok(());
    }

    let app_id = extract_app_id(job_name, &ctx.read_buffer);
    if app_id == 0 || !CloudRedirectHost::is_app(app_id) {
        return Ok(());
    }

    let account_id = (*local_steam_id().lock().unwrap() & 0xFFFFFFFF) as u32;

    // Call CloudRedirect to handle the RPC
    let mut resp_buf = vec![0u8; 65536];
    let mut resp_len: u32 = 0;
    let mut eresult: i32 = 2; // Fail

    if !CloudRedirectHost::handle_cloud_rpc(
        job_name,
        app_id,
        account_id,
        &ctx.read_buffer,
        &mut resp_buf,
        &mut resp_len,
        &mut eresult,
    ) {
        return Ok(()); // Not handled
    }

    // Build response packet
    let job_id = ctx.job_id_source.unwrap_or(0);
    let hdr_bytes = build_response_header(job_id, job_name, eresult);

    // Construct full frame: MsgHdr + header + body
    const K_MSG_HDR_PROTO_FLAG: u32 = 0x80000000;
    const K_E_MSG_SERVICE_METHOD_RESPONSE: u32 = 147;

    let mut frame = Vec::with_capacity(4 + hdr_bytes.len() + resp_len as usize);
    frame.extend_from_slice(&(K_E_MSG_SERVICE_METHOD_RESPONSE | K_MSG_HDR_PROTO_FLAG).to_le_bytes());
    frame.extend_from_slice(&(hdr_bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(&hdr_bytes);
    frame.extend_from_slice(&resp_buf[..resp_len as usize]);

    // Queue for delivery via carrier packet
    pending_responses().lock().unwrap().push_back(frame);

    log::debug!(
        "Cloud: handled {} app={} -> queued {}-byte response (eresult={})",
        job_name, app_id, frame.len(), eresult
    );

    // Suppress original outbound
    ctx.suppress_send = true;
    Ok(())
}

/// Deliver any queued Cloud responses by borrowing the carrier packet.
pub fn drain_cloud_responses(
    carrier_ctx: &mut NetPacketContext,
    invoke_original: impl FnOnce(&mut NetPacketContext),
) {
    let mut pending = pending_responses().lock().unwrap();
    while let Some(frame) = pending.pop_front() {
        let orig_data = carrier_ctx.data;
        let orig_size = carrier_ctx.size;
        carrier_ctx.data = frame.as_ptr() as *mut u8;
        carrier_ctx.size = frame.len() as u32;
        invoke_original(carrier_ctx);
        carrier_ctx.data = orig_data;
        carrier_ctx.size = orig_size;
        log::debug!("Cloud: delivered {}-byte response", frame.len());
    }
}

/// Register Cloud handlers.
pub fn register_cloud_handlers() {
    let mut handlers = cloud_handlers().lock().unwrap();
    let iface = IpcInterface::IClientUser; // Cloud uses IClientUser pipe

    let send_post: HandlerFn = Arc::new(|ctx: &mut IpcContext| -> Result<(), String> {
        handle_send_cloud_rpc(ctx)
    });

    handlers.push(CloudHandler {
        interface_id: iface,
        func_hash: NETPACKET_CLOUD_SERVICE_METHOD_CALL_FNHASH,
        name: "Hooks_NetPacket_Cloud::HandleSend".to_string(),
        fencepost: 0,
        argc: 4,
        pre_handler: None,
        post_handler: Some(send_post.clone()),
    });

    register_netpacket_handler(
        iface,
        NETPACKET_CLOUD_SERVICE_METHOD_CALL_FNHASH,
        "Hooks_NetPacket_Cloud::HandleSend",
        NetPacketDirection::Send,
        None,
        Some(send_post),
    );
}

/// Install Cloud hooks.
pub fn install_cloud_hooks() -> Result<(), String> {
    register_cloud_handlers();
    CLOUD_ACTIVE.store(true, std::sync::atomic::Ordering::Release);
    log::info!("Installed Cloud NetPacket hooks");
    Ok(())
}

/// Uninstall Cloud hooks.
pub fn uninstall_cloud_hooks() {
    cloud_handlers().lock().unwrap().clear();
    pending_responses().lock().unwrap().clear();
    *local_steam_id().lock().unwrap() = 0;
    let iface = IpcInterface::IClientUser;
    let _ = ipc::unregister_handler(iface, NETPACKET_CLOUD_SERVICE_METHOD_CALL_FNHASH);
    CLOUD_ACTIVE.store(false, std::sync::atomic::Ordering::Release);
    log::info!("Uninstalled Cloud NetPacket hooks");
}

/// Check if Cloud hooks are active.
pub fn is_cloud_hook_active() -> bool {
    CLOUD_ACTIVE.load(std::sync::atomic::Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_varint_read_write() {
        let test_vals = [0u64, 1, 127, 128, 16383, 16384, 0xFFFFFFFF, 0x100000000];
        for val in test_vals {
            let mut buf = Vec::new();
            encode_varint(val, &mut buf);
            let (decoded, _) = read_varint(&buf).unwrap();
            assert_eq!(decoded, val, "varint roundtrip failed for {}", val);
        }
    }

    #[test]
    fn test_extract_app_id() {
        // Build a minimal protobuf with app_id in field 1
        let mut buf = Vec::new();
        buf.push(0x08); // field 1, varint
        encode_varint(730, &mut buf); // app_id = 730
        assert_eq!(extract_app_id("Cloud.Foo#1", &buf), 730);

        // ClientCommitFileUpload uses field 2
        let mut buf2 = Vec::new();
        buf2.push(0x10); // field 2, varint
        encode_varint(480, &mut buf2);
        assert_eq!(extract_app_id("Cloud.ClientCommitFileUpload#1", &buf2), 480);
    }

    #[test]
    fn test_build_response_header() {
        let hdr = build_response_header(0x123456789ABCDEFu64, "Cloud.Test#1", 1);
        assert!(!hdr.is_empty());
        // Should contain job name, jobid_target, eresult
        assert!(hdr.iter().any(|&b| b == 0x0A)); // field 1 tag
        assert!(hdr.iter().any(|&b| b == 0x10)); // field 2 tag
        assert!(hdr.iter().any(|&b| b == 0x18)); // field 3 tag
    }
}