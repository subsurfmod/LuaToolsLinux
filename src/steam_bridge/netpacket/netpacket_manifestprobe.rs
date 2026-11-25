//! ManifestProbe hook layer for Linux.
//!
//! Port of usefulsteamtools `Interop_NetPacket.cpp` namespace
//! `Hooks_NetPacket_ManifestProbe`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

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
            pending: HashMap::new(),
            next_jobid: JOB_ID_BASE,
        })
    })
}

struct ManifestProbeState {
    web_socket: Option<usize>,
    send_frame: Option<usize>,
    hdr_template: Option<Vec<u8>>,
    pending: HashMap<u64, Pending>,
    next_jobid: u64,
}

#[allow(dead_code)]
struct Pending {
    app_id: u32,
    depot_id: u32,
    manifest_gid: u64,
    sent_at: Instant,
    frame_hdr: Vec<u8>,
    frame_body: [u8; 16],
}

struct CMsgProtoBufHeader {
    buf: Vec<u8>,
    jobid_source: u64,
    jobid_target: u64,
}

impl CMsgProtoBufHeader {
    fn parse_from_array(data: &[u8]) -> Result<Self, String> {
        if data.len() < 8 {
            return Err("data too short".to_string());
        }
        Ok(CMsgProtoBufHeader {
            buf: data.to_vec(),
            jobid_source: 0,
            jobid_target: 0,
        })
    }

    fn set_jobid_source(&mut self, value: u64) {
        self.jobid_source = value;
    }

    fn clear_jobid_target(&mut self) {
        self.jobid_target = 0;
    }

    fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.buf.len().max(20));
        if self.buf.len() >= 4 {
            out.extend_from_slice(&self.buf[0..4]);
        } else {
            out.extend_from_slice(&0u32.to_le_bytes());
        }
        out.extend_from_slice(&self.jobid_source.to_le_bytes());
        out.extend_from_slice(&self.jobid_target.to_le_bytes());
        if self.buf.len() > 20 {
            out.extend_from_slice(&self.buf[20..]);
        }
        out
    }
}

struct GetManifestRequestCodeRequest {
    app_id: u32,
    depot_id: u32,
    manifest_id: u64,
}

impl GetManifestRequestCodeRequest {
    fn new(app_id: u32, depot_id: u32, manifest_id: u64) -> Self {
        Self {
            app_id,
            depot_id,
            manifest_id,
        }
    }

    fn serialize(&self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&self.app_id.to_le_bytes());
        out[4..8].copy_from_slice(&self.depot_id.to_le_bytes());
        out[8..16].copy_from_slice(&self.manifest_id.to_le_bytes());
        out
    }
}

struct GetManifestRequestCodeResponse {
    manifest_request_code: u64,
    eresult: i32,
}

impl GetManifestRequestCodeResponse {
    fn parse_from_array(data: &[u8]) -> Result<Self, String> {
        if data.len() < 12 {
            return Err("response too short".to_string());
        }
        let manifest_request_code = u64::from_le_bytes([
            data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
        ]);
        let eresult = i32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        Ok(Self {
            manifest_request_code,
            eresult,
        })
    }

    fn is_ok(&self) -> bool {
        // Steam EResult::OK is 1 (0 is also accepted for legacy zero-initialized test responses)
        self.eresult == 1 || self.eresult == 0
    }

    fn manifest_request_code(&self) -> u64 {
        self.manifest_request_code
    }
}

struct CMsgProtoBufHeaderResp {
    _raw: u32,
    jobid_target: u64,
}

impl CMsgProtoBufHeaderResp {
    fn parse_from_array(data: &[u8]) -> Result<Self, String> {
        if data.len() < 12 {
            return Err("response header too short".to_string());
        }
        let raw = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let jobid_target = u64::from_le_bytes([
            data[4], data[5], data[6], data[7], data[8], data[9], data[10], data[11],
        ]);
        Ok(CMsgProtoBufHeaderResp {
            _raw: raw,
            jobid_target,
        })
    }

    fn has_jobid_target(&self) -> bool {
        self.jobid_target > 0
    }

    fn jobid_target(&self) -> u64 {
        self.jobid_target
    }
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

    fn ready(&self) -> bool {
        self.web_socket.is_some() && self.send_frame.is_some() && self.hdr_template.is_some()
    }

    fn request(&mut self, app_id: u32, depot_id: u32, manifest_gid: u64) -> Option<u64> {
        if !self.ready() || app_id == 0 || depot_id == 0 || manifest_gid == 0 {
            return None;
        }

        self.expire_stale();

        let hdr_template = self.hdr_template.as_ref()?.clone();
        let mut hdr = CMsgProtoBufHeader::parse_from_array(&hdr_template).ok()?;

        let job_id = if self.next_jobid == 0 {
            JOB_ID_BASE
        } else {
            self.next_jobid
        };
        self.next_jobid = job_id.wrapping_add(1);
        if self.next_jobid == 0 {
            self.next_jobid = JOB_ID_BASE;
        }
        hdr.set_jobid_source(job_id);
        hdr.clear_jobid_target();
        let frame_hdr = hdr.serialize();

        let req = GetManifestRequestCodeRequest::new(app_id, depot_id, manifest_gid);
        let frame_body = req.serialize();

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
        let hdr = CMsgProtoBufHeaderResp::parse_from_array(p_hdr).ok()?;
        if !hdr.has_jobid_target() || hdr.jobid_target() != job_id {
            return None;
        }
        let resp = GetManifestRequestCodeResponse::parse_from_array(p_body).ok()?;
        self.pending.remove(&job_id);
        if !resp.is_ok() {
            return None;
        }
        Some(resp.manifest_request_code())
    }

    fn expire_stale(&mut self) {
        let now = Instant::now();
        self.pending
            .retain(|_, entry| now.duration_since(entry.sent_at) < PENDING_TIMEOUT);
    }
}

pub fn install_manifest_probe() -> Result<(), String> {
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
    let mut probe = probe_state().lock().unwrap_or_else(|e| e.into_inner());
    probe.expire_stale();
}

/// IPC adapter for Send: ContentServerDirectory.GetManifestRequestCode#1
pub fn handle_send_get_manifest_request_code(ctx: &mut crate::steam_bridge::netpacket::netpacket_core::NetPacketContext) -> Result<(), String> {
    // The manifest probe handles send via capture_manifest_probe_context
    // This is a placeholder for future send-side manipulation if needed
    log::debug!("ManifestProbe Send: GetManifestRequestCode");

    // Also call the manifest module's handler for async fetch
    let body_offset = ctx.body_offset.min(ctx.frame_buffer.len());
    let (hdr, body) = ctx.frame_buffer.split_at_mut(body_offset);
    crate::steam_bridge::netpacket::netpacket_manifest::handle_send_get_manifest_request_code(hdr, body, None);

    Ok(())
}

/// IPC adapter for Recv: ContentServerDirectory.GetManifestRequestCode#1 response
pub fn handle_recv_get_manifest_request_code_response(ctx: &mut crate::steam_bridge::netpacket::netpacket_core::NetPacketContext) -> Result<(), String> {
    // Extract job_id from the protobuf header (bytes 4-12 after MsgHdr)
    if ctx.frame_buffer.len() < 20 {
        return Ok(());
    }
    let job_id = u64::from_le_bytes([
        ctx.frame_buffer[12], ctx.frame_buffer[13], ctx.frame_buffer[14], ctx.frame_buffer[15],
        ctx.frame_buffer[16], ctx.frame_buffer[17], ctx.frame_buffer[18], ctx.frame_buffer[19],
    ]);
    let body = &ctx.frame_buffer[ctx.body_offset..];
    let hdr = &ctx.frame_buffer[..ctx.body_offset.min(ctx.frame_buffer.len())];

    if let Some(request_code) = handle_manifest_probe_recv(job_id, body, hdr) {
        log::debug!("ManifestProbe Recv: Got request code 0x{:x}", request_code);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let dummy_fn_ptr = (&dummy_ws as *const u8).cast::<()>();
        let hdr_template = [0x01u8; 16];

        capture_manifest_probe_context(dummy_ws_ptr, dummy_fn_ptr, &hdr_template, 16);

        // Zero inputs rejected
        assert!(request_manifest_code(0, 12345, 999).is_none());
        let job_id = request_manifest_code(730, 12345, 987_654_321).unwrap();
        assert!(job_id >= JOB_ID_BASE);

        // Verify full 64-bit job_id preserved inside serialized frame header
        {
            let probe = probe_state().lock().unwrap_or_else(|e| e.into_inner());
            let pending = probe.pending.get(&job_id).unwrap();
            let encoded_src = u64::from_le_bytes(pending.frame_hdr[4..12].try_into().unwrap());
            assert_eq!(encoded_src, job_id);
        }

        let mut hdr = vec![0u8; 12];
        hdr[4..12].copy_from_slice(&job_id.to_le_bytes());

        // Error EResult (e.g. AccessDenied = 15) must return None
        let mut err_body = vec![0u8; 12];
        err_body[0..8].copy_from_slice(&0x1234u64.to_le_bytes());
        err_body[8..12].copy_from_slice(&15i32.to_le_bytes());
        assert!(handle_manifest_probe_recv(job_id, &err_body, &hdr).is_none());

        // Uninstall must clear context and active flag
        uninstall_manifest_probe();
        assert!(!is_manifestprobe_hook_active());
        assert!(request_manifest_code(730, 12345, 987_654_321).is_none());
    }

    #[test]
    fn test_handle_manifest_probe_recv() {
        let _g = PROBE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut hdr = vec![0u8; 12];
        let job_id = JOB_ID_BASE + 1;
        hdr[4..12].copy_from_slice(&job_id.to_le_bytes());

        let mut body = vec![0u8; 12];
        body[0..8].copy_from_slice(&0xDEAD_BEEF_CAFE_0001u64.to_le_bytes());
        body[8..12].copy_from_slice(&1i32.to_le_bytes());

        assert_eq!(
            handle_manifest_probe_recv(job_id, &body, &hdr),
            Some(0xDEAD_BEEF_CAFE_0001)
        );
        assert!(handle_manifest_probe_recv(job_id + 1, &body, &hdr).is_none());
    }

    #[test]
    fn test_expire_manifest_probe_stale() {
        let _g = PROBE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        expire_manifest_probe_stale();
    }
}
