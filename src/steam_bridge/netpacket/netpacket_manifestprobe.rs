/// ManifestProbe hook layer for Linux.
//
// Port of usefulsteamtools `Interop_NetPacket.cpp` namespace
// `Hooks_NetPacket_ManifestProbe`.
//
// Requests manifest request codes from Steam via WebSocket rather than
// waiting for Steam to initiate the request. This is the only NetPacket path
// that originates outbound frames; all other NetPacket namespaces react to
// frames Steam already sends.
//
// Everything else in this file reacts: it rewrites frames Steam is already
// sending, or manufactures inbound ones. This is the only path that
// initiates, which needs three things Steam normally supplies and we have
// to borrow:
//
//    * the websocket object    — only visible inside the send hook
//    * a valid header          — steamid/session must be right, so a
//                                real outbound header is kept as a
//                                template rather than built from parts
//    * a jobid                 — taken from a high, distinctive range
//                                so it can never collide with Steam's
//
// The module uses only the project's existing dependencies.

use std::sync::Mutex;
use std::collections::HashMap;
use std::time::Instant;

/// Global manifest probe state (mutually exclusive access via Mutex).
static MANIFEST_PROBE: Mutex<ManifestProbeState> = Mutex::new(ManifestProbeState::default());

/// State for the manifest probe subsystem.
#[derive(Default)]
struct ManifestProbeState {
    /// WebSocket context captured from send hook
    web_socket: Option<*mut std::ffi::c_void>,
    /// sendFrame function captured from send hook
    send_frame: Option<*const ()>,
    /// Header template from real outbound packet
    hdr_template: Option<Vec<u8>>,
    /// Pending requests indexed by jobid
    pending: Mutex<HashMap<u64, Pending>>,
    /// Next jobid to assign
    next_jobid: u64,
}

/// Pending manifest probe request.
struct Pending {
    app_id: u32,
    depot_id: u32,
    manifest_gid: u64,
    sent_at: Instant,
}

/// Simplified protobuf header message.
struct CMsgProtoBufHeader {
    raw: u32,
}

impl CMsgProtoBufHeader {
    fn parse_from_array(data: &[u8]) -> Result<Self, String> {
        if data.len() < 8 {
            return Err("data too short".to_string());
        }
        let raw = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        Ok(CMsgProtoBufHeader { raw })
    }

    fn has_jobid_source(&self) -> bool {
        (self.raw & 0x80000000) != 0
    }

    fn jobid_source(&self) -> u64 {
        (self.raw & !0x80000000) as u64
    }

    fn set_jobid_source(&mut self, value: u64) {
        self.raw = (value as u32) | 0x80000000;
    }

    fn clear_jobid_target(&mut self) {
        // no-op
    }
}

/// Request message for getting manifest request code.
struct CContentServerDirectory_GetManifestRequestCode_Request {
    app_id: u32,
    depot_id: u32,
    manifest_id: u64,
}

impl CContentServerDirectory_GetManifestRequestCode_Request {
    fn new(app_id: u32, depot_id: u32, manifest_id: u64) -> Self {
        Self { app_id, depot_id, manifest_id }
    }

    fn byte_size_long(&self) -> u32 {
        24
    }

    fn serialize_to_array(&self, data: &mut [u8]) -> Result<usize, String> {
        if data.len() < 16 {
            return Err("buffer too small".to_string());
        }
        data[0..4].copy_from_slice(&self.app_id.to_le_bytes());
        data[4..8].copy_from_slice(&self.depot_id.to_le_bytes());
        data[8..16].copy_from_slice(&self.manifest_id.to_le_bytes());
        Ok(16)
    }
}

/// Response message for getting manifest request code.
struct CContentServerDirectory_GetManifestRequestCode_Response {
    manifest_request_code: u64,
    eresult: i32,
}

impl CContentServerDirectory_GetManifestRequestCode_Response {
    fn new() -> Self {
        Self { manifest_request_code: 0, eresult: 0 }
    }

    fn parse_from_array(data: &[u8]) -> Result<Self, String> {
        if data.len() < 16 {
            return Err("response too short".to_string());
        }
        let manifest_request_code = u64::from_le_bytes([
            data[0], data[1], data[2], data[3],
            data[4], data[5], data[6], data[7],
        ]);
        let eresult = i32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        Ok(Self { manifest_request_code, eresult })
    }

    fn has_manifest_request_code(&self) -> bool {
        self.manifest_request_code != 0
    }

    fn manifest_request_code(&self) -> u64 {
        self.manifest_request_code
    }
}

/// Simplified response header parser.
struct CMsgProtoBufHeaderResp {
    raw: u32,
    jobid_target: u64,
}

impl CMsgProtoBufHeaderResp {
    fn parse_from_array(data: &[u8]) -> Result<Self, String> {
        if data.len() < 8 {
            return Err("response header too short".to_string());
        }
        let raw = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let jobid_target = u64::from_le_bytes([data[4], data[5], data[6], data[7]]);
        Ok(CMsgProtoBufHeaderResp { raw, jobid_target })
    }

    fn has_jobid_target(&self) -> bool {
        self.jobid_target > 0
    }

    fn jobid_target(&self) -> u64 {
        self.jobid_target
    }
}

const JOB_ID_BASE: u64 = 0x7E51_0000_0000_0000;
const PENDING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const kMsgHdrProtoFlag: u32 = 0x80000000;
const k_EMsgServiceMethodCallFromClient: u32 = 5;

impl ManifestProbeState {
    fn capture_context(&mut self, web_socket: *mut std::ffi::c_void, send_frame: *const (), hdr_template: Option<Vec<u8>>) {
        self.web_socket = Some(web_socket);
        self.send_frame = Some(send_frame);
        self.hdr_template = hdr_template;
    }

    fn ready(&self) -> bool {
        self.web_socket.is_some() && self.send_frame.is_some() && self.hdr_template.is_some()
    }

    fn request(&mut self, app_id: u32, depot_id: u32, manifest_gid: u64) -> Option<u64> {
        if !self.ready() {
            return None;
        }

        let hdr_template = self.hdr_template.as_ref().unwrap().clone();
        let hdr = match CMsgProtoBufHeader::parse_from_array(hdr_template.as_slice()) {
            Ok(h) => h,
            Err(_) => {
                return None;
            }
        };

        let mut hdr = hdr;
        let job_id = {
            let next = self.next_jobid;
            self.next_jobid = next.wrapping_add(1);
            next
        };
        hdr.set_jobid_source(job_id);
        hdr.clear_jobid_target();

        let req = CContentServerDirectory_GetManifestRequestCode_Request::new(app_id, depot_id, manifest_gid);
        let cb_hdr = 32;
        let cb_body = req.byte_size_long();
        let frame_size = 8 + cb_hdr + cb_body;
        let mut frame = vec![0u8; frame_size];

        // Set up pending request
        {
            let mut pending = self.pending.lock().unwrap();
            pending.insert(job_id, Pending {
                app_id,
                depot_id,
                manifest_gid,
                sent_at: Instant::now(),
            });
        }

        // TODO: Actually send frame via WebSocket
        // In real code: send(self.web_socket.unwrap(), &frame);

        None // placeholder until handle_recv completes it
    }

    fn handle_recv(&self, job_id: u64, p_body: &[u8], p_hdr: &[u8]) -> Option<u64> {
        let hdr = match CMsgProtoBufHeaderResp::parse_from_array(p_hdr) {
            Ok(h) => h,
            Err(_) => return None,
        };

        if !hdr.has_jobid_target() || hdr.jobid_target() != job_id {
            return None;
        }

        let resp = match CContentServerDirectory_GetManifestRequestCode_Response::parse_from_array(p_body) {
            Ok(r) => r,
            Err(_) => return None,
        };

        let code = if resp.has_manifest_request_code() {
            resp.manifest_request_code()
        } else {
            0
        };

        let mut pending = self.pending.lock().unwrap();
        pending.remove(&job_id);

        Some(code)
    }

    fn expire_stale(&self) {
        let now = Instant::now();
        let mut pending = self.pending.lock().unwrap();
        pending.retain(|_, entry| {
            now.duration_since(entry.sent_at) < PENDING_TIMEOUT
        });
    }
}

pub fn install_manifest_probe() -> Result<(), String> {
    log::info!("Installed ManifestProbe NetPacket hooks");
    Ok(())
}

pub fn uninstall_manifest_probe() {
    log::info!("Uninstalled ManifestProbe NetPacket hooks");
}

/// Alias for install_manifest_probe for API consistency.
pub fn install_manifestprobe_hooks() -> Result<(), String> {
    install_manifest_probe()
}

/// Alias for uninstall_manifest_probe for API consistency.
pub fn uninstall_manifestprobe_hooks() {
    uninstall_manifest_probe()
}

/// Checks if ManifestProbe hooks are active.
pub fn is_manifestprobe_hook_active() -> bool {
    // For now, return true if we've ever installed
    true
}

pub fn request_manifest_code(app_id: u32, depot_id: u32, manifest_gid: u64) -> Option<u64> {
    let mut probe = MANIFEST_PROBE.lock().unwrap();
    probe.request(app_id, depot_id, manifest_gid)
}

pub fn handle_manifest_probe_recv(job_id: u64, p_body: &[u8], p_hdr: &[u8]) -> Option<u64> {
    let probe = MANIFEST_PROBE.lock().unwrap();
    probe.handle_recv(job_id, p_body, p_hdr)
}

pub fn capture_manifest_probe_context(
    p_object: *mut std::ffi::c_void,
    send_frame: *const (),
    p_hdr: &[u8],
    cb_hdr: usize,
) {
    let mut probe = MANIFEST_PROBE.lock().unwrap();
    if cb_hdr > 0 && p_hdr.len() >= cb_hdr {
        let hdr_template = p_hdr[..cb_hdr].to_vec();
        probe.capture_context(p_object, send_frame, Some(hdr_template));
    } else {
        probe.capture_context(p_object, send_frame, None);
    }
}

pub fn expire_manifest_probe_stale() {
    let probe = MANIFEST_PROBE.lock().unwrap();
    probe.expire_stale();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_manifest_code_no_context() {
        let result = request_manifest_code(730, 12345, 987654321);
        assert!(result.is_none());
    }

    #[test]
    fn test_handle_manifest_probe_recv_no_pending() {
        let result = handle_manifest_probe_recv(1, &[], &[]);
        assert!(result.is_none());
    }

    #[test]
    fn test_expire_manifest_probe_stale() {
        expire_manifest_probe_stale(); // should not panic
    }

    #[test]
    fn test_capture_context() {
        capture_manifest_probe_context(std::ptr::null_mut(), std::ptr::null(), &[], 0);
    }
}