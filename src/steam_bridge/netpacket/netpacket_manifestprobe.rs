//! ManifestProbe hook layer for Linux.
//!
//! Port of usefulsteamtools `Interop_NetPacket.cpp` namespace
//! `Hooks_NetPacket_ManifestProbe`.
//!
//! Coordinates client-originated `ContentServerDirectory.GetManifestRequestCode#1` probes
//! (`jobid >= 0x7E51_0000_0000_0000`) and bridges `NetPacketCore` `Send`/`Recv` events into
//! [`crate::steam_bridge::netpacket::netpacket_manifest`] with real Steam `MsgHdrProtoBuf` +
//! `CMsgProtoBufHeader` wire decoding/encoding.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::steam_bridge::netpacket::netpacket_core::NetPacketContext;
use crate::steam_bridge::netpacket::netpacket_manifest::{
    self, CMsgProtoBufHeader, GetManifestRequestCodeRequest, GetManifestRequestCodeResponse,
    GMRC_TARGET_JOB_NAME, KM_MSG_HDR_PROTO_FLAG, MSG_GET_MANIFEST_REQUEST_CODE,
    MSG_GET_MANIFEST_REQUEST_CODE_RESPONSE,
};

const JOB_ID_BASE: u64 = 0x7E51_0000_0000_0000;
const PENDING_TIMEOUT: Duration = Duration::from_secs(30);

static MANIFEST_PROBE: OnceLock<Mutex<ManifestProbeState>> = OnceLock::new();
static MANIFEST_PROBE_ACTIVE: AtomicBool = AtomicBool::new(false);

fn probe_state() -> &'static Mutex<ManifestProbeState> {
    MANIFEST_PROBE.get_or_init(|| {
        Mutex::new(ManifestProbeState {
            web_socket: None,
            send_frame: None,
            hdr_template: None,
            steam_root: None,
            pending: HashMap::new(),
            next_jobid: JOB_ID_BASE,
        })
    })
}

struct ManifestProbeState {
    web_socket: Option<usize>,
    send_frame: Option<usize>,
    hdr_template: Option<Vec<u8>>,
    steam_root: Option<PathBuf>,
    pending: HashMap<u64, Pending>,
    next_jobid: u64,
}

struct Pending {
    app_id: u32,
    depot_id: u32,
    manifest_gid: u64,
    sent_at: Instant,
    frame_hdr: Vec<u8>,
    frame_body: Vec<u8>,
}

impl ManifestProbeState {
    fn capture_context(
        &mut self,
        web_socket: *mut std::ffi::c_void,
        send_frame: *const (),
        hdr_template: Option<Vec<u8>>,
    ) {
        self.web_socket = (!web_socket.is_null()).then_some(web_socket as usize);
        self.send_frame = (!send_frame.is_null()).then_some(send_frame as usize);
        self.hdr_template = hdr_template;
    }

    fn clear_context(&mut self) {
        self.web_socket = None;
        self.send_frame = None;
        self.hdr_template = None;
        self.pending.clear();
    }

    fn resolve_steam_root(&mut self) -> Option<PathBuf> {
        if let Some(ref root) = self.steam_root {
            return Some(root.clone());
        }
        let discovered = netpacket_manifest::discover_steam_root();
        if let Some(ref root) = discovered {
            self.steam_root = Some(root.clone());
        }
        discovered
    }

    fn ready(&self) -> bool {
        self.web_socket.is_some() && self.send_frame.is_some() && self.hdr_template.is_some()
    }

    fn request(&mut self, app_id: u32, depot_id: u32, manifest_gid: u64) -> Option<u64> {
        if !self.ready() || app_id == 0 || depot_id == 0 || manifest_gid == 0 {
            return None;
        }

        self.expire_stale();

        let hdr_template = self.hdr_template.as_ref()?.clone();
        let mut proto_hdr = CMsgProtoBufHeader::parse_from_array(&hdr_template).unwrap_or_default();

        let job_id = if self.next_jobid == 0 {
            JOB_ID_BASE
        } else {
            self.next_jobid
        };
        self.next_jobid = job_id.wrapping_add(1);
        if self.next_jobid == 0 {
            self.next_jobid = JOB_ID_BASE;
        }

        proto_hdr.jobid_source = job_id;
        proto_hdr.jobid_target = 0;
        proto_hdr.target_job_name = Some(GMRC_TARGET_JOB_NAME.to_string());
        proto_hdr.eresult = None;

        let encoded_hdr = proto_hdr.encode_proto();
        let mut frame_hdr = Vec::with_capacity(8 + encoded_hdr.len());
        frame_hdr.extend_from_slice(
            &(MSG_GET_MANIFEST_REQUEST_CODE | KM_MSG_HDR_PROTO_FLAG).to_le_bytes(),
        );
        frame_hdr.extend_from_slice(&(encoded_hdr.len() as u32).to_le_bytes());
        frame_hdr.extend_from_slice(&encoded_hdr);

        let frame_body = GetManifestRequestCodeRequest {
            app_id,
            depot_id,
            manifest_gid,
            app_branch: Some("public".to_string()),
        }
        .encode_proto();

        if let (Some(ws_ptr), Some(fn_ptr)) = (self.web_socket, self.send_frame) {
            let mut wire_frame = Vec::with_capacity(frame_hdr.len() + frame_body.len());
            wire_frame.extend_from_slice(&frame_hdr);
            wire_frame.extend_from_slice(&frame_body);
            unsafe {
                let send_fn: unsafe extern "C" fn(*mut std::ffi::c_void, u32, *mut u8, u32) -> bool =
                    std::mem::transmute(fn_ptr);
                let _ = send_fn(
                    ws_ptr as *mut std::ffi::c_void,
                    0,
                    wire_frame.as_mut_ptr(),
                    wire_frame.len() as u32,
                );
            }
        }

        self.pending.insert(
            job_id,
            Pending {
                app_id,
                depot_id,
                manifest_gid,
                sent_at: Instant::now(),
                frame_hdr,
                frame_body,
            },
        );

        Some(job_id)
    }

    fn handle_recv(&mut self, job_id: u64, p_body: &[u8], p_hdr: &[u8]) -> Option<u64> {
        if job_id == 0 {
            return None;
        }

        let (decoded_target, hdr_eresult, used_proto_hdr) = decode_resp_header(p_hdr)?;
        if decoded_target == 0 || decoded_target != job_id {
            return None;
        }

        let (code, body_eresult) = decode_resp_body(p_body, used_proto_hdr)?;
        let pending_entry = self.pending.remove(&job_id);

        let eresult = hdr_eresult.or(body_eresult).unwrap_or(1);
        if eresult != 1 && eresult != 0 {
            return None;
        }
        if code == 0 {
            return None;
        }

        if let Some(entry) = pending_entry {
            log::debug!(
                "[manifestprobe] Resolved probe app={} depot={} gid={} hdr_bytes={} body_bytes={} -> code=0x{:X}",
                entry.app_id,
                entry.depot_id,
                entry.manifest_gid,
                entry.frame_hdr.len(),
                entry.frame_body.len(),
                code
            );
            netpacket_manifest::record_harvested_code(entry.depot_id, entry.manifest_gid, code);
        }

        Some(code)
    }

    fn expire_stale(&mut self) {
        let now = Instant::now();
        self.pending
            .retain(|_, entry| now.duration_since(entry.sent_at) < PENDING_TIMEOUT);
    }
}

/// Decode `(jobid_target, eresult, is_proto)` from a real `MsgHdrProtoBuf` / `CMsgProtoBufHeader`
/// (field 11 `fixed64` `0x59` / varint `0x58`, field 13 `eresult` `0x68`), falling back gracefully
/// to the legacy `[raw: u32][jobid_target: u64]` or `[MsgHdr(8)][jobid_source(8)][jobid_target(8)]`
/// layouts used by synthetic test frames.
fn decode_resp_header(p_hdr: &[u8]) -> Option<(u64, Option<i32>, bool)> {
    if let Ok(proto) = CMsgProtoBufHeader::parse_from_array(p_hdr) {
        if proto.jobid_target != 0 || proto.target_job_name.is_some() || proto.eresult.is_some() {
            return Some((proto.jobid_target, proto.eresult, true));
        }
    }

    // Legacy synthetic layout A: 20-byte [MsgHdr(8)][jobid_source(8)][jobid_target(8)]
    if p_hdr.len() >= 20 {
        let target = u64::from_le_bytes(p_hdr[12..20].try_into().ok()?);
        if target != 0 {
            return Some((target, None, false));
        }
    }

    // Legacy synthetic layout B: 12-byte [raw(4)][jobid_target(8)]
    if p_hdr.len() >= 12 {
        let target = u64::from_le_bytes(p_hdr[4..12].try_into().ok()?);
        return Some((target, None, false));
    }

    None
}

/// Decode `(manifest_request_code, optional_legacy_eresult)` from a real protobuf
/// `CContentServerDirectory_GetManifestRequestCode_Response` (`tag 0x08` varint), falling back
/// gracefully to the legacy 12-byte `[code: u64 LE][eresult: i32 LE]` test layout.
fn decode_resp_body(p_body: &[u8], prefer_proto: bool) -> Option<(u64, Option<i32>)> {
    if prefer_proto || p_body.first() == Some(&0x08) {
        if let Ok(resp) = GetManifestRequestCodeResponse::decode_proto(
            p_body,
            MSG_GET_MANIFEST_REQUEST_CODE_RESPONSE,
        ) {
            if prefer_proto || resp.manifest_request_code != 0 {
                return Some((resp.manifest_request_code, None));
            }
        }
    }

    if p_body.len() >= 12 {
        let code = u64::from_le_bytes(p_body[0..8].try_into().ok()?);
        let eresult = i32::from_le_bytes(p_body[8..12].try_into().ok()?);
        return Some((code, Some(eresult)));
    }

    if let Ok(resp) = GetManifestRequestCodeResponse::decode_proto(
        p_body,
        MSG_GET_MANIFEST_REQUEST_CODE_RESPONSE,
    ) {
        return Some((resp.manifest_request_code, None));
    }

    None
}

/// Compute the true byte offset where the protobuf body starts inside a `NetPacketContext` frame.
fn compute_frame_split_offset(ctx: &NetPacketContext) -> usize {
    if ctx.is_proto && ctx.frame_buffer.len() >= 8 {
        let hdr_len = u32::from_le_bytes([
            ctx.frame_buffer[4],
            ctx.frame_buffer[5],
            ctx.frame_buffer[6],
            ctx.frame_buffer[7],
        ]) as usize;
        if 8usize.saturating_add(hdr_len) <= ctx.frame_buffer.len() {
            return 8 + hdr_len;
        }
    }
    ctx.body_offset.min(ctx.frame_buffer.len())
}

pub fn install_manifest_probe() -> Result<(), String> {
    {
        let mut probe = probe_state().lock().unwrap_or_else(|e| e.into_inner());
        let _ = probe.resolve_steam_root();
    }
    MANIFEST_PROBE_ACTIVE.store(true, Ordering::Release);
    log::info!("Installed ManifestProbe NetPacket hooks");
    Ok(())
}

pub fn uninstall_manifest_probe() {
    MANIFEST_PROBE_ACTIVE.store(false, Ordering::Release);
    probe_state()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear_context();
    log::info!("Uninstalled ManifestProbe NetPacket hooks");
}

pub fn install_manifestprobe_hooks() -> Result<(), String> {
    install_manifest_probe()
}

pub fn uninstall_manifestprobe_hooks() {
    uninstall_manifest_probe()
}

pub fn is_manifestprobe_hook_active() -> bool {
    MANIFEST_PROBE_ACTIVE.load(Ordering::Acquire)
}

/// Explicitly configure the Steam root directory used for depotcache manifest pre-seeding.
pub fn set_manifest_probe_steam_root(steam_root: PathBuf) {
    let mut probe = probe_state().lock().unwrap_or_else(|e| e.into_inner());
    if !steam_root.as_os_str().is_empty() {
        probe.steam_root = Some(steam_root);
    }
}

pub fn request_manifest_code(app_id: u32, depot_id: u32, manifest_gid: u64) -> Option<u64> {
    let mut probe = probe_state().lock().unwrap_or_else(|e| e.into_inner());
    probe.request(app_id, depot_id, manifest_gid)
}

pub fn handle_manifest_probe_recv(job_id: u64, p_body: &[u8], p_hdr: &[u8]) -> Option<u64> {
    let mut probe = probe_state().lock().unwrap_or_else(|e| e.into_inner());
    probe.handle_recv(job_id, p_body, p_hdr)
}

pub fn capture_manifest_probe_context(
    p_object: *mut std::ffi::c_void,
    send_frame: *const (),
    p_hdr: &[u8],
    cb_hdr: usize,
) {
    let mut probe = probe_state().lock().unwrap_or_else(|e| e.into_inner());
    if cb_hdr > 0 && p_hdr.len() >= cb_hdr {
        probe.capture_context(p_object, send_frame, Some(p_hdr[..cb_hdr].to_vec()));
    } else {
        probe.capture_context(p_object, send_frame, None);
    }
}

pub fn expire_manifest_probe_stale() {
    {
        let mut probe = probe_state().lock().unwrap_or_else(|e| e.into_inner());
        probe.expire_stale();
    }
    netpacket_manifest::expire_manifest_stale();
}

/// NetPacket Send handler: `ContentServerDirectory.GetManifestRequestCode#1` (`EMsg 151`).
pub fn handle_send_get_manifest_request_code(ctx: &mut NetPacketContext) -> Result<(), String> {
    log::debug!("ManifestProbe Send: GetManifestRequestCode");

    let split_offset = compute_frame_split_offset(ctx);
    let steam_root = {
        let mut probe = probe_state().lock().unwrap_or_else(|e| e.into_inner());
        if probe.hdr_template.is_none() && split_offset >= 8 {
            probe.hdr_template = Some(ctx.frame_buffer[..split_offset].to_vec());
        }
        probe.resolve_steam_root()
    };

    let (hdr, body) = ctx.frame_buffer.split_at_mut(split_offset);
    netpacket_manifest::handle_send_get_manifest_request_code(hdr, body, steam_root);

    Ok(())
}

/// NetPacket Recv handler: `ContentServerDirectory.GetManifestRequestCode#1` response (`EMsg 147`).
pub fn handle_recv_get_manifest_request_code_response(
    ctx: &mut NetPacketContext,
) -> Result<(), String> {
    if ctx.frame_buffer.len() < 8 {
        return Ok(());
    }

    // 1. Let netpacket_manifest harvest genuine codes and patch unowned Lua depot responses.
    let mut frame_copy = ctx.frame_buffer.clone();
    if netpacket_manifest::handle_recv_get_manifest_request_code_response_frame(&mut frame_copy) {
        ctx.replace_frame(frame_copy);
        ctx.body_offset = compute_frame_split_offset(ctx);
    }

    // 2. Decode jobid_target from real CMsgProtoBufHeader (field 11 fixed64), with legacy fallback.
    let split_offset = compute_frame_split_offset(ctx);
    let hdr = &ctx.frame_buffer[..split_offset];
    let body = &ctx.frame_buffer[split_offset..];

    let job_id = decode_resp_header(hdr)
        .map(|(target, _, _)| target)
        .or_else(|| {
            if ctx.frame_buffer.len() >= 20 {
                Some(u64::from_le_bytes(ctx.frame_buffer[12..20].try_into().ok()?))
            } else {
                None
            }
        })
        .unwrap_or(0);

    if job_id != 0 {
        if let Some(request_code) = handle_manifest_probe_recv(job_id, body, hdr) {
            log::debug!("ManifestProbe Recv: Got request code 0x{:X}", request_code);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steam_bridge::netpacket::netpacket_core::NetPacketDirection;
    use crate::steam_bridge::netpacket::netpacket_manifest::{
        build_protobuf_frame, get_harvested_code,
    };

    static PROBE_TEST_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn test_request_manifest_code_no_context() {
        let _g = PROBE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        capture_manifest_probe_context(std::ptr::null_mut(), std::ptr::null(), &[], 0);
        assert!(request_manifest_code(730, 12345, 987_654_321).is_none());
    }

    #[test]
    fn test_full_probe_lifecycle_and_eresult_validation() {
        let _g = PROBE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        install_manifest_probe().unwrap();
        assert!(is_manifestprobe_hook_active());

        let mut dummy_ws = 0u8;
        let dummy_ws_ptr = (&mut dummy_ws as *mut u8).cast::<std::ffi::c_void>();
        extern "C" fn dummy_send(
            _ws: *mut std::ffi::c_void,
            _channel: u32,
            _data: *mut u8,
            _len: u32,
        ) -> bool {
            true
        }
        let dummy_fn_ptr = (dummy_send as *const ()).cast::<()>();
        let template_proto = CMsgProtoBufHeader {
            steamid: Some(76561198011122233),
            client_sessionid: Some(77),
            ..Default::default()
        }
        .encode_proto();

        capture_manifest_probe_context(
            dummy_ws_ptr,
            dummy_fn_ptr,
            &template_proto,
            template_proto.len(),
        );

        // Zero inputs rejected
        assert!(request_manifest_code(0, 12345, 999).is_none());
        let job_id = request_manifest_code(730, 12345, 987_654_321).unwrap();
        assert!(job_id >= JOB_ID_BASE);

        // Verify real CMsgProtoBufHeader (field 10 fixed64 jobid_source & field 12 job name)
        {
            let probe = probe_state().lock().unwrap_or_else(|e| e.into_inner());
            let pending = probe.pending.get(&job_id).unwrap();
            let decoded = CMsgProtoBufHeader::parse_from_array(&pending.frame_hdr).unwrap();
            assert_eq!(decoded.jobid_source, job_id);
            assert_eq!(decoded.steamid, Some(76561198011122233));
            assert_eq!(
                decoded.target_job_name.as_deref(),
                Some(GMRC_TARGET_JOB_NAME)
            );
            let decoded_req =
                GetManifestRequestCodeRequest::parse_from_array(&pending.frame_body).unwrap();
            assert_eq!(decoded_req.app_id, 730);
            assert_eq!(decoded_req.depot_id, 12345);
            assert_eq!(decoded_req.manifest_gid, 987_654_321);
        }

        // Error EResult (e.g. AccessDenied = 15) in real protobuf response must return None
        let err_hdr = CMsgProtoBufHeader {
            jobid_target: job_id,
            target_job_name: Some(GMRC_TARGET_JOB_NAME.to_string()),
            eresult: Some(15),
            ..Default::default()
        }
        .encode_proto();
        let err_body = GetManifestRequestCodeResponse {
            manifest_request_code: 0x1234,
        }
        .encode_proto();
        assert!(handle_manifest_probe_recv(job_id, &err_body, &err_hdr).is_none());

        uninstall_manifest_probe();
        assert!(!is_manifestprobe_hook_active());
        assert!(request_manifest_code(730, 12345, 987_654_321).is_none());
    }

    #[test]
    fn test_handle_manifest_probe_recv_legacy_and_proto() {
        let _g = PROBE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        // 1. Legacy 12-byte fallback compatibility
        let mut legacy_hdr = vec![0u8; 12];
        let job_id = JOB_ID_BASE + 1;
        legacy_hdr[4..12].copy_from_slice(&job_id.to_le_bytes());

        let mut legacy_body = vec![0u8; 12];
        legacy_body[0..8].copy_from_slice(&0xDEAD_BEEF_CAFE_0001u64.to_le_bytes());
        legacy_body[8..12].copy_from_slice(&1i32.to_le_bytes());

        assert_eq!(
            handle_manifest_probe_recv(job_id, &legacy_body, &legacy_hdr),
            Some(0xDEAD_BEEF_CAFE_0001)
        );
        assert!(handle_manifest_probe_recv(job_id + 1, &legacy_body, &legacy_hdr).is_none());

        // 2. Real protobuf CMsgProtoBufHeader (field 11 fixed64) + response body (tag 0x08)
        let job_id_proto = JOB_ID_BASE + 42;
        let proto_hdr = CMsgProtoBufHeader {
            jobid_target: job_id_proto,
            target_job_name: Some(GMRC_TARGET_JOB_NAME.to_string()),
            eresult: Some(1),
            ..Default::default()
        };
        let proto_body = GetManifestRequestCodeResponse {
            manifest_request_code: 0xABCD_EF01_2345_6789,
        }
        .encode_proto();
        let full_frame =
            build_protobuf_frame(MSG_GET_MANIFEST_REQUEST_CODE_RESPONSE, &proto_hdr, &proto_body);
        let split = 8 + proto_hdr.encode_proto().len();
        assert_eq!(
            handle_manifest_probe_recv(job_id_proto, &full_frame[split..], &full_frame[..split]),
            Some(0xABCD_EF01_2345_6789)
        );
    }

    #[test]
    fn test_send_and_recv_netpacket_context_end_to_end() {
        let _g = PROBE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("depotcache")).unwrap();
        set_manifest_probe_steam_root(tmp.path().to_path_buf());

        let job_id = JOB_ID_BASE + 99;
        let depot_id = 48001u32;
        let manifest_gid = 7_654_321_098_765_432u64;
        let expected_code = 0x1357_9BDF_2468_ACE0u64;

        let send_hdr = CMsgProtoBufHeader {
            steamid: Some(76561198099999999),
            client_sessionid: Some(12),
            jobid_source: job_id,
            target_job_name: Some(GMRC_TARGET_JOB_NAME.to_string()),
            ..Default::default()
        };
        let send_body = GetManifestRequestCodeRequest {
            app_id: 480,
            depot_id,
            manifest_gid,
            app_branch: Some("public".to_string()),
        }
        .encode_proto();
        let send_frame =
            build_protobuf_frame(MSG_GET_MANIFEST_REQUEST_CODE, &send_hdr, &send_body);
        let mut send_ctx =
            NetPacketContext::new(send_frame, NetPacketDirection::Send, 480).unwrap();
        handle_send_get_manifest_request_code(&mut send_ctx).unwrap();

        let recv_hdr = CMsgProtoBufHeader {
            steamid: Some(76561198099999999),
            jobid_target: job_id,
            target_job_name: Some(GMRC_TARGET_JOB_NAME.to_string()),
            eresult: Some(1),
            ..Default::default()
        };
        let recv_body = GetManifestRequestCodeResponse {
            manifest_request_code: expected_code,
        }
        .encode_proto();
        let recv_frame =
            build_protobuf_frame(MSG_GET_MANIFEST_REQUEST_CODE_RESPONSE, &recv_hdr, &recv_body);
        let mut recv_ctx =
            NetPacketContext::new(recv_frame, NetPacketDirection::Recv, 480).unwrap();
        handle_recv_get_manifest_request_code_response(&mut recv_ctx).unwrap();

        // Genuine code from server response must be harvested into netpacket_manifest!
        assert_eq!(get_harvested_code(depot_id, manifest_gid), Some(expected_code));
    }

    #[test]
    fn test_expire_manifest_probe_stale() {
        let _g = PROBE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        expire_manifest_probe_stale();
    }
}
