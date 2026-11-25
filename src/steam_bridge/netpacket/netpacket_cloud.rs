//! NetPacket Cloud hook layer for Linux.
//!
//! Port of usefulsteamtools `Hooks_NetPacket_Cloud` namespace in Interop_NetPacket.cpp.

use std::collections::VecDeque;
use std::sync::{Arc, LazyLock, Mutex};

use crate::steam_bridge::cloud_redirect::{
    get_cloud_redirect_host, is_cloud_redirect_active,
};
use crate::steam_bridge::netpacket::netpacket_core::{
    NetPacketContext, NetPacketDirection, NetPacketHandler, register_netpacket_handler,
    K_EMSG_SERVICE_METHOD_CALL_FROM_CLIENT,
};

static CLOUD_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Pending Cloud responses to inject via carrier packets.
static PENDING_CLOUD_RESPONSES: LazyLock<Mutex<VecDeque<Vec<u8>>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));

/// Local SteamID captured from outbound headers.
static LOCAL_STEAM_ID: LazyLock<Mutex<u64>> = LazyLock::new(|| Mutex::new(0));

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

/// Find a specific varint field by tag number in protobuf encoded bytes.
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
            0 => {
                let (_, consumed) = read_varint(&data[pos..])?;
                pos += consumed;
            }
            1 => {
                if pos + 8 > data.len() {
                    return None;
                }
                pos += 8;
            }
            2 => {
                let (len, consumed) = read_varint(&data[pos..])?;
                pos += consumed;
                let len = len as usize;
                if pos + len > data.len() {
                    return None;
                }
                pos += len;
            }
            5 => {
                if pos + 4 > data.len() {
                    return None;
                }
                pos += 4;
            }
            _ => return None,
        }
    }
    None
}

/// Search for Cloud.* service method name in the buffer.
fn extract_cloud_job_name(data: &[u8]) -> Option<String> {
    let needle = b"Cloud.";
    for i in 0..data.len().saturating_sub(needle.len()) {
        if &data[i..i + needle.len()] == needle {
            let mut end = i + needle.len();
            while end < data.len() {
                let b = data[end];
                if b.is_ascii_alphanumeric() || b == b'.' || b == b'#' || b == b'_' {
                    end += 1;
                } else {
                    break;
                }
            }
            if let Ok(s) = std::str::from_utf8(&data[i..end]) {
                return Some(s.to_string());
            }
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
        if val == 0 {
            break;
        }
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
    // field 3: eresult (varint)
    hdr.push(0x18); // tag 3, wire type 0
    encode_varint(eresult as u64, &mut hdr);
    hdr
}

/// Handle outbound ServiceMethodCallFromClient for Cloud.* RPCs.
pub fn handle_send_cloud_rpc(ctx: &mut NetPacketContext) -> Result<(), String> {
    if !is_cloud_redirect_active() {
        return Ok(());
    }

    // Capture local SteamID from header if present
    if ctx.is_proto && ctx.frame_buffer.len() >= 20 {
        if let Some(steam_id) = find_varint_field(&ctx.frame_buffer[8..], 1) {
            if steam_id != 0 {
                *local_steam_id().lock().unwrap() = steam_id;
            }
        }
    }

    // Find Cloud job name
    let job_name = match extract_cloud_job_name(&ctx.frame_buffer) {
        Some(name) => name,
        None => return Ok(()),
    };

    let mut app_id = extract_app_id(&job_name, ctx.body());
    if app_id == 0 {
        app_id = ctx.app_id;
    }
    let cloud_host = get_cloud_redirect_host();
    if app_id == 0 || !cloud_host.is_app(app_id) {
        return Ok(());
    }

    let account_id = (*local_steam_id().lock().unwrap() & 0xFFFF_FFFF) as u32;

    // Call CloudRedirect to handle the RPC
    let (resp_data, eresult) = match cloud_host.handle_cloud_rpc(
        &job_name,
        app_id,
        account_id,
        ctx.body().to_vec(),
    ) {
        Some((data, eresult)) => (data, eresult),
        None => return Ok(()), // Not handled
    };

    // Extract job_id from header (bytes 8..16)
    let job_id = if ctx.frame_buffer.len() >= 16 {
        u64::from_le_bytes(ctx.frame_buffer[8..16].try_into().unwrap_or_default())
    } else {
        0
    };

    let hdr_bytes = build_response_header(job_id, &job_name, eresult);

    // Construct full frame: MsgHdr + header + body
    const K_MSG_HDR_PROTO_FLAG: u32 = 0x8000_0000;
    const K_E_MSG_SERVICE_METHOD_RESPONSE: u32 = 147;

    let mut frame = Vec::with_capacity(8 + hdr_bytes.len() + resp_data.len());
    frame.extend_from_slice(&(K_E_MSG_SERVICE_METHOD_RESPONSE | K_MSG_HDR_PROTO_FLAG).to_le_bytes());
    frame.extend_from_slice(&(hdr_bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(&hdr_bytes);
    frame.extend_from_slice(&resp_data);

    log::debug!(
        "Cloud: handled {} app={} -> queued {}-byte response (eresult={})",
        job_name, app_id, frame.len(), eresult
    );

    // Queue for delivery via carrier packet
    pending_responses().lock().unwrap().push_back(frame);

    // Suppress original outbound
    ctx.frame_buffer.clear();
    Ok(())
}

/// Deliver any queued Cloud responses by borrowing the carrier packet.
pub fn drain_cloud_responses<F>(
    carrier_ctx: &mut NetPacketContext,
    mut invoke_original: F,
) -> Result<(), String>
where
    F: FnMut(&mut NetPacketContext) -> Result<(), String>,
{
    loop {
        let frame = {
            let mut pending = pending_responses().lock().unwrap();
            if pending.is_empty() {
                return Ok(());
            }
            pending.pop_front()
        };

        if let Some(frame) = frame {
            let orig_frame = carrier_ctx.frame_buffer.clone();
            let orig_hdr = carrier_ctx.msg_hdr;
            let orig_body_offset = carrier_ctx.body_offset;
            let orig_is_proto = carrier_ctx.is_proto;

            carrier_ctx.replace_frame(frame);
            let res = invoke_original(carrier_ctx);

            carrier_ctx.frame_buffer = orig_frame;
            carrier_ctx.msg_hdr = orig_hdr;
            carrier_ctx.body_offset = orig_body_offset;
            carrier_ctx.is_proto = orig_is_proto;

            res?;
        }
    }
}

/// Register Cloud handlers.
pub fn register_cloud_handlers() {
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: K_EMSG_SERVICE_METHOD_CALL_FROM_CLIENT,
        target_job_hash: None,
        name: "Hooks_NetPacket_Cloud::HandleSend".to_string(),
        handler: Arc::new(|ctx| {
            handle_send_cloud_rpc(ctx)
        }),
    });
}

/// Install Cloud hooks.
pub fn install_cloud_hooks() -> Result<(), String> {
    if CLOUD_ACTIVE.load(std::sync::atomic::Ordering::Acquire) {
        return Ok(());
    }
    register_cloud_handlers();
    CLOUD_ACTIVE.store(true, std::sync::atomic::Ordering::Release);
    log::info!("Installed Cloud NetPacket hooks");
    Ok(())
}

/// Uninstall Cloud hooks.
pub fn uninstall_cloud_hooks() {
    if !CLOUD_ACTIVE.load(std::sync::atomic::Ordering::Acquire) {
        return;
    }
    pending_responses().lock().unwrap().clear();
    *local_steam_id().lock().unwrap() = 0;
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
        let test_vals = [0u64, 1, 127, 128, 16383, 16384, 0xFFFF_FFFF, 0x1_0000_0000];
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
    fn test_extract_cloud_job_name() {
        let sample = b"\x08\x01\x12\x17Cloud.GetUGCFileDetails#1\x18\x00";
        assert_eq!(
            extract_cloud_job_name(sample),
            Some("Cloud.GetUGCFileDetails#1".to_string())
        );
    }

    #[test]
    fn test_build_response_header() {
        let hdr = build_response_header(0x1234_5678_9ABC_DEF0u64, "Cloud.Test#1", 1);
        assert!(!hdr.is_empty());
        // Should contain job name, jobid_target, eresult
        assert!(hdr.iter().any(|&b| b == 0x0A)); // field 1 tag
        assert!(hdr.iter().any(|&b| b == 0x10)); // field 2 tag
        assert!(hdr.iter().any(|&b| b == 0x18)); // field 3 tag
    }
}
