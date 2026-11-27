//! NetPacket Manifest hook layer for Linux.
//!
//! Port of usefulsteamtools `Interop_NetPacket.cpp` namespace
//! `Hooks_NetPacket_Manifest` (lines 1227-1458).
//!
//! Handles async manifest request code fetching with real Steam `MsgHdrProtoBuf` +
//! `CMsgProtoBufHeader` wire decoding/encoding:
//! - **Outgoing** (`EMsg 151` `ContentServerDirectory.GetManifestRequestCode#1` and
//!   `EMsg 5461` `CMsgClientGetManifestRequestCode`):
//!   - Track `jobid_source -> (depot_id, manifest_gid, app_id, is_lua_depot, is_owned)`
//!   - Auto-discover `steam_root` when `None` and pre-seed `depotcache` manifest (`EnsureCached`)
//!   - For Lua depots NOT genuinely owned: reuse harvested donor code or launch async HTTP fetch
//!   - Passive capture: track all requests for donor code harvesting
//! - **Incoming** (`EMsg 147` `k_EMsgServiceMethodResponse` and `EMsg 5462`):
//!   - Passive capture: harvest genuine codes from owned depots
//!   - For tracked Lua depot requests: resolve async fetch / donor code
//!   - Patch `CMsgProtoBufHeader.eresult = 1` (`OK`) + response body `manifest_request_code` (`tag 0x08`)

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::steam_bridge::interop::interop_misc::resolve_app_id;
use crate::steam_bridge::manifest_hooks::{
    ensure_manifest_cached, get_seen_depot, report_missing_manifest,
};

const PENDING_TIMEOUT: Duration = Duration::from_secs(12);

/// `kMsgHdrProtoFlag` = `0x8000_0000`
pub const KM_MSG_HDR_PROTO_FLAG: u32 = 0x8000_0000;
/// `k_EMsgServiceMethodCallFromClient` = `151`
pub const MSG_GET_MANIFEST_REQUEST_CODE: u32 = 151;
/// `k_EMsgServiceMethodResponse` = `147`
pub const MSG_GET_MANIFEST_REQUEST_CODE_RESPONSE: u32 = 147;
/// `k_EMsgClientGetManifestRequestCode` = `5461`
pub const MSG_CLIENT_GET_MANIFEST_REQUEST_CODE: u32 = 5461;
/// `k_EMsgClientGetManifestRequestCodeResponse` = `5462`
pub const MSG_CLIENT_GET_MANIFEST_REQUEST_CODE_RESPONSE: u32 = 5462;
/// Canonical Unified Service target job name for GMRC.
pub const GMRC_TARGET_JOB_NAME: &str = "ContentServerDirectory.GetManifestRequestCode#1";

static MANIFEST_STATE: OnceLock<Mutex<ManifestState>> = OnceLock::new();
static MANIFEST_ACTIVE: AtomicBool = AtomicBool::new(false);

fn manifest_state() -> &'static Mutex<ManifestState> {
    MANIFEST_STATE.get_or_init(|| Mutex::new(ManifestState::new()))
}

struct ManifestState {
    /// `jobid_source -> SentRequest`
    sent_requests: HashMap<u64, SentRequest>,
    /// Pending async fetches: `job_id -> (depot_id, manifest_gid, receiver, started_at)`
    pending_fetches: HashMap<u64, (u32, u64, mpsc::Receiver<u64>, Instant)>,
    /// Passive capture: harvested genuine codes from owned depots `(depot_id, manifest_gid) -> code`
    harvested_codes: HashMap<(u32, u64), u64>,
    /// Steam root for depotcache operations
    steam_root: Option<PathBuf>,
}

#[derive(Clone, Copy)]
struct SentRequest {
    depot_id: u32,
    manifest_gid: u64,
    app_id: u32,
    is_lua_depot: bool,
    is_owned: bool,
    sent_at: Instant,
}

impl ManifestState {
    fn new() -> Self {
        Self {
            sent_requests: HashMap::new(),
            pending_fetches: HashMap::new(),
            harvested_codes: HashMap::new(),
            steam_root: None,
        }
    }

    fn set_steam_root(&mut self, root: PathBuf) {
        if !root.as_os_str().is_empty() {
            self.steam_root = Some(root);
        }
    }

    fn ensure_steam_root(&mut self) {
        if self.steam_root.is_none() {
            if let Some(root) = discover_steam_root() {
                self.steam_root = Some(root);
            }
        }
    }

    fn is_lua_depot(&self, depot_id: u32) -> bool {
        crate::steam_bridge::interop::interop_package::has_depot(depot_id)
    }

    fn is_owned_depot(&self, depot_id: u32) -> bool {
        let (ready, owned) = crate::steam_bridge::interop::interop_package::get_owned_depots();
        ready && owned.contains(&depot_id)
    }

    fn lookup_depot(&self, depot_id: u32) -> Option<(u32, u64)> {
        get_seen_depot(0, depot_id)
    }

    fn pre_seed_manifest(&self, app_id: u32, depot_id: u32, manifest_gid: u64) {
        if let Some(ref steam_root) = self.steam_root {
            if !ensure_manifest_cached(steam_root, app_id, depot_id, manifest_gid) {
                report_missing_manifest(depot_id, manifest_gid);
            }
        }
    }
}

/// Auto-discover the active Linux Steam installation root (`~/.local/share/Steam`,
/// `~/.steam/steam`, Flatpak, or `STEAM_ROOT` / `STEAM_DIR` environment override).
pub fn discover_steam_root() -> Option<PathBuf> {
    for env_key in ["STEAM_ROOT", "STEAM_DIR"] {
        if let Ok(val) = std::env::var(env_key) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                let p = PathBuf::from(trimmed);
                if p.is_dir() {
                    return Some(p);
                }
            }
        }
    }

    let home = dirs::home_dir().or_else(|| std::env::var_os("HOME").map(PathBuf::from))?;
    let candidates = [
        home.join(".local/share/Steam"),
        home.join(".steam/steam"),
        home.join(".steam/root"),
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        home.join(".var/app/com.valvesoftware.Steam/.steam/steam"),
        home.join("snap/steam/common/.local/share/Steam"),
    ];

    if let Some(found) = candidates.iter().find(|p| {
        p.is_dir()
            && (p.join("depotcache").is_dir()
                || p.join("steamapps").is_dir()
                || p.join("config").is_dir())
    }) {
        return Some(found.clone());
    }

    candidates.into_iter().find(|p| p.is_dir())
}

// ============================================================================
// Protobuf Wire Primitives & Real Steam CMsgProtoBufHeader / GMRC Messages
// ============================================================================

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

/// Real Steam `CMsgProtoBufHeader` (`steammessages_base.proto`):
/// - `steamid = 1` (`fixed64`, tag `0x09`)
/// - `client_sessionid = 2` (`int32`, tag `0x10`)
/// - `jobid_source = 10` (`fixed64`, tag `0x51`)
/// - `jobid_target = 11` (`fixed64`, tag `0x59`)
/// - `target_job_name = 12` (`string`, tag `0x62`)
/// - `eresult = 13` (`int32`, tag `0x68`)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CMsgProtoBufHeader {
    pub steamid: Option<u64>,
    pub client_sessionid: Option<i32>,
    pub jobid_source: u64,
    pub jobid_target: u64,
    pub target_job_name: Option<String>,
    pub eresult: Option<i32>,
    pub emsg: u32,
    pub unknown_fields: Vec<(u64, Vec<u8>)>,
}

impl CMsgProtoBufHeader {
    /// Decode a raw `CMsgProtoBufHeader` protobuf slice (without the 8-byte `MsgHdrProtoBuf` prefix).
    pub fn decode_proto(data: &[u8], emsg: u32) -> Result<Self, String> {
        let mut hdr = Self {
            emsg,
            ..Self::default()
        };
        let mut pos = 0usize;
        while pos < data.len() {
            if data[pos] == 0 {
                // Zero-byte padding at end of scratch slice; field 0 is never valid in protobuf.
                break;
            }
            let (tag, tag_len) =
                read_varint(&data[pos..]).ok_or_else(|| "truncated protobuf tag".to_string())?;
            let field_num = (tag >> 3) as u32;
            let wire_type = (tag & 0x07) as u8;
            if field_num == 0 {
                return Err("invalid protobuf field number 0".to_string());
            }
            pos += tag_len;

            match (field_num, wire_type) {
                (1, 1) => {
                    if pos + 8 > data.len() {
                        return Err("truncated steamid fixed64".to_string());
                    }
                    let v = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
                    hdr.steamid = Some(v);
                    pos += 8;
                }
                (1, 0) => {
                    let (v, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "truncated steamid varint".to_string())?;
                    hdr.steamid = Some(v);
                    pos += n;
                }
                (2, 0) => {
                    let (v, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "truncated client_sessionid".to_string())?;
                    hdr.client_sessionid = Some(v as i32);
                    pos += n;
                }
                (10, 1) => {
                    if pos + 8 > data.len() {
                        return Err("truncated jobid_source fixed64".to_string());
                    }
                    hdr.jobid_source = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
                    pos += 8;
                }
                (10, 0) => {
                    let (v, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "truncated jobid_source varint".to_string())?;
                    hdr.jobid_source = v;
                    pos += n;
                }
                (11, 1) => {
                    if pos + 8 > data.len() {
                        return Err("truncated jobid_target fixed64".to_string());
                    }
                    hdr.jobid_target = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
                    pos += 8;
                }
                (11, 0) => {
                    let (v, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "truncated jobid_target varint".to_string())?;
                    hdr.jobid_target = v;
                    pos += n;
                }
                (12, 2) => {
                    let (len, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "truncated target_job_name length".to_string())?;
                    pos += n;
                    let len = len as usize;
                    let end = pos.checked_add(len)
                        .ok_or_else(|| "overflow target_job_name length".to_string())?;
                    if end > data.len() {
                        return Err("truncated target_job_name string".to_string());
                    }
                    let s = std::str::from_utf8(&data[pos..end])
                        .map_err(|_| "invalid utf-8 in target_job_name".to_string())?;
                    hdr.target_job_name = Some(s.to_string());
                    pos = end;
                }
                (13, 0) => {
                    let (v, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "truncated eresult varint".to_string())?;
                    hdr.eresult = Some(v as i32);
                    pos += n;
                }
                (_, 0) => {
                    let (_, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "truncated unknown varint".to_string())?;
                    hdr.unknown_fields.push((tag, data[pos..pos + n].to_vec()));
                    pos += n;
                }
                (_, 1) => {
                    if pos + 8 > data.len() {
                        return Err("truncated unknown fixed64".to_string());
                    }
                    hdr.unknown_fields.push((tag, data[pos..pos + 8].to_vec()));
                    pos += 8;
                }
                (_, 2) => {
                    let (len, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "truncated unknown length-delimited".to_string())?;
                    let total = n.saturating_add(len as usize);
                    if pos + total > data.len() {
                        return Err("truncated unknown bytes payload".to_string());
                    }
                    hdr.unknown_fields
                        .push((tag, data[pos..pos + total].to_vec()));
                    pos += total;
                }
                (_, 5) => {
                    if pos + 4 > data.len() {
                        return Err("truncated unknown fixed32".to_string());
                    }
                    hdr.unknown_fields.push((tag, data[pos..pos + 4].to_vec()));
                    pos += 4;
                }
                _ => return Err(format!("unsupported wire type {}", wire_type)),
            }
        }
        Ok(hdr)
    }

    /// Encode `CMsgProtoBufHeader` into wire-compliant protobuf bytes.
    pub fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        if let Some(steamid) = self.steamid {
            out.push(0x09); // field 1, fixed64
            out.extend_from_slice(&steamid.to_le_bytes());
        }
        if let Some(session_id) = self.client_sessionid {
            out.push(0x10); // field 2, int32 varint
            encode_varint(session_id as i64 as u64, &mut out);
        }
        if self.jobid_source != 0 {
            out.push(0x51); // field 10, fixed64
            out.extend_from_slice(&self.jobid_source.to_le_bytes());
        }
        if self.jobid_target != 0 {
            out.push(0x59); // field 11, fixed64
            out.extend_from_slice(&self.jobid_target.to_le_bytes());
        }
        if let Some(ref name) = self.target_job_name {
            out.push(0x62); // field 12, string
            let bytes = name.as_bytes();
            encode_varint(bytes.len() as u64, &mut out);
            out.extend_from_slice(bytes);
        }
        if let Some(eresult) = self.eresult {
            out.push(0x68); // field 13, int32 varint
            encode_varint(eresult as i64 as u64, &mut out);
        }
        for (tag, raw_val) in &self.unknown_fields {
            encode_varint(*tag, &mut out);
            out.extend_from_slice(raw_val);
        }
        out
    }

    /// Clear `error_message` (field 14, tag `0x72`) when patching an error header to `eresult = 1`.
    pub fn clear_error_message(&mut self) {
        self.unknown_fields.retain(|(tag, _)| (*tag >> 3) != 14);
    }

    /// Parse from either an 8-byte `MsgHdrProtoBuf`-prefixed slice (`[emsg | 0x8000_0000][hdr_len][proto_hdr]`)
    /// or a raw `CMsgProtoBufHeader` protobuf slice.
    pub fn parse_from_array(data: &[u8]) -> Result<Self, String> {
        if data.len() >= 8 {
            let raw_emsg = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            let hdr_len = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
            let base_emsg = raw_emsg & !KM_MSG_HDR_PROTO_FLAG;
            if ((raw_emsg & KM_MSG_HDR_PROTO_FLAG) != 0
                || matches!(
                    base_emsg,
                    MSG_GET_MANIFEST_REQUEST_CODE
                        | MSG_GET_MANIFEST_REQUEST_CODE_RESPONSE
                        | MSG_CLIENT_GET_MANIFEST_REQUEST_CODE
                        | MSG_CLIENT_GET_MANIFEST_REQUEST_CODE_RESPONSE
                ))
                && 8usize.saturating_add(hdr_len) <= data.len()
            {
                return Self::decode_proto(&data[8..8 + hdr_len], base_emsg);
            }
        }
        if data.is_empty() {
            return Err("empty protobuf header".to_string());
        }
        Self::decode_proto(data, 0)
    }
}

/// Parse either a full `MsgHdrProtoBuf` frame (`p_body` empty) or split `(p_hdr, p_body)` slices
/// (including splits at `8 + hdr_len`, splits at `NetPacketContext::body_offset = 20`, or raw
/// `(proto_hdr, proto_body)` slices without the 8-byte `MsgHdrProtoBuf` prefix).
fn parse_packet_slices<'a>(
    p_hdr: &'a [u8],
    p_body: &'a [u8],
    default_emsg: u32,
) -> Result<(u32, CMsgProtoBufHeader, Cow<'a, [u8]>, bool), String> {
    if p_hdr.len() >= 8 {
        let raw_emsg = u32::from_le_bytes([p_hdr[0], p_hdr[1], p_hdr[2], p_hdr[3]]);
        let hdr_len = u32::from_le_bytes([p_hdr[4], p_hdr[5], p_hdr[6], p_hdr[7]]) as usize;
        let base_emsg = raw_emsg & !KM_MSG_HDR_PROTO_FLAG;
        let looks_like_msg_hdr = (raw_emsg & KM_MSG_HDR_PROTO_FLAG) != 0
            || (matches!(
                base_emsg,
                MSG_GET_MANIFEST_REQUEST_CODE
                    | MSG_GET_MANIFEST_REQUEST_CODE_RESPONSE
                    | MSG_CLIENT_GET_MANIFEST_REQUEST_CODE
                    | MSG_CLIENT_GET_MANIFEST_REQUEST_CODE_RESPONSE
            ) && 8usize.saturating_add(hdr_len) <= p_hdr.len().saturating_add(p_body.len()));

        if looks_like_msg_hdr {
            let hdr_end = 8usize.saturating_add(hdr_len);
            if hdr_end <= p_hdr.len() {
                let parsed_hdr = CMsgProtoBufHeader::decode_proto(&p_hdr[8..hdr_end], base_emsg)?;
                let body_cow = if !p_body.is_empty() {
                    Cow::Borrowed(p_body)
                } else {
                    Cow::Borrowed(&p_hdr[hdr_end..])
                };
                return Ok((base_emsg, parsed_hdr, body_cow, true));
            } else if hdr_end <= p_hdr.len().saturating_add(p_body.len()) {
                // Split happened mid-header (e.g. NetPacketContext fixed body_offset = 20)
                let spill = hdr_end - p_hdr.len();
                let mut combined_hdr = Vec::with_capacity(hdr_len);
                combined_hdr.extend_from_slice(&p_hdr[8..]);
                combined_hdr.extend_from_slice(&p_body[..spill]);
                let parsed_hdr = CMsgProtoBufHeader::decode_proto(&combined_hdr, base_emsg)?;
                return Ok((base_emsg, parsed_hdr, Cow::Borrowed(&p_body[spill..]), true));
            }
            return Err("MsgHdrProtoBuf header_length exceeds packet size".to_string());
        }
    }

    let parsed_hdr = CMsgProtoBufHeader::decode_proto(p_hdr, default_emsg)?;
    Ok((default_emsg, parsed_hdr, Cow::Borrowed(p_body), false))
}

/// Build a complete Steam `MsgHdrProtoBuf` packet:
/// `[base_emsg | 0x8000_0000: u32 LE][hdr_len: u32 LE][proto_hdr][proto_body]`.
pub fn build_protobuf_frame(base_emsg: u32, hdr: &CMsgProtoBufHeader, body: &[u8]) -> Vec<u8> {
    let hdr_bytes = hdr.encode_proto();
    let mut frame = Vec::with_capacity(8 + hdr_bytes.len() + body.len());
    frame.extend_from_slice(&(base_emsg | KM_MSG_HDR_PROTO_FLAG).to_le_bytes());
    frame.extend_from_slice(&(hdr_bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(&hdr_bytes);
    frame.extend_from_slice(body);
    frame
}

/// Decoded `CContentServerDirectory_GetManifestRequestCode_Request` (`EMsg 151`)
/// and `CMsgClientGetManifestRequestCode` (`EMsg 5461`):
/// - `app_id = 1` (`uint32`)
/// - `depot_id = 2` (`uint32`)
/// - `manifest_id = 3` (`uint64`)
/// - `app_branch = 4` (`string`, optional)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GetManifestRequestCodeRequest {
    pub app_id: u32,
    pub depot_id: u32,
    pub manifest_gid: u64,
    pub app_branch: Option<String>,
}

impl GetManifestRequestCodeRequest {
    pub fn parse_from_array(data: &[u8]) -> Result<Self, String> {
        let mut req = Self::default();
        let mut pos = 0usize;
        while pos < data.len() {
            if data[pos] == 0 {
                break;
            }
            let (tag, tag_len) =
                read_varint(&data[pos..]).ok_or_else(|| "truncated request tag".to_string())?;
            let field_num = (tag >> 3) as u32;
            let wire_type = (tag & 0x07) as u8;
            if field_num == 0 {
                return Err("invalid request field 0".to_string());
            }
            pos += tag_len;

            match (field_num, wire_type) {
                (1, 0) => {
                    let (v, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "truncated app_id".to_string())?;
                    req.app_id = v as u32;
                    pos += n;
                }
                (2, 0) => {
                    let (v, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "truncated depot_id".to_string())?;
                    req.depot_id = v as u32;
                    pos += n;
                }
                (3, 0) => {
                    let (v, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "truncated manifest_id".to_string())?;
                    req.manifest_gid = v;
                    pos += n;
                }
                (3, 1) => {
                    if pos + 8 > data.len() {
                        return Err("truncated manifest_id fixed64".to_string());
                    }
                    req.manifest_gid = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
                    pos += 8;
                }
                (4, 2) => {
                    let (len, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "truncated app_branch length".to_string())?;
                    pos += n;
                    let len = len as usize;
                    let end = pos.checked_add(len)
                        .ok_or_else(|| "overflow app_branch length".to_string())?;
                    if end > data.len() {
                        return Err("truncated app_branch string".to_string());
                    }
                    let branch = std::str::from_utf8(&data[pos..end])
                        .map_err(|_| "invalid utf-8 in app_branch".to_string())?;
                    req.app_branch = Some(branch.to_string());
                    pos = end;
                }
                (_, 0) => {
                    let (_, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "truncated unknown request varint".to_string())?;
                    pos += n;
                }
                (_, 1) => {
                    if pos + 8 > data.len() {
                        return Err("truncated unknown request fixed64".to_string());
                    }
                    pos += 8;
                }
                (_, 2) => {
                    let (len, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "truncated unknown request bytes".to_string())?;
                    let end = pos.saturating_add(n).saturating_add(len as usize);
                    if end > data.len() {
                        return Err("truncated unknown request payload".to_string());
                    }
                    pos = end;
                }
                (_, 5) => {
                    if pos + 4 > data.len() {
                        return Err("truncated unknown request fixed32".to_string());
                    }
                    pos += 4;
                }
                _ => return Err(format!("unsupported request wire type {}", wire_type)),
            }
        }

        if req.depot_id == 0 && req.manifest_gid == 0 {
            return Err("missing depot_id and manifest_id in request".to_string());
        }
        Ok(req)
    }

    pub fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32);
        if self.app_id != 0 {
            out.push(0x08); // field 1, uint32
            encode_varint(u64::from(self.app_id), &mut out);
        }
        if self.depot_id != 0 {
            out.push(0x10); // field 2, uint32
            encode_varint(u64::from(self.depot_id), &mut out);
        }
        if self.manifest_gid != 0 {
            out.push(0x18); // field 3, uint64
            encode_varint(self.manifest_gid, &mut out);
        }
        if let Some(ref branch) = self.app_branch {
            if !branch.is_empty() {
                out.push(0x22); // field 4, string
                encode_varint(branch.len() as u64, &mut out);
                out.extend_from_slice(branch.as_bytes());
            }
        }
        out
    }
}

/// Decoded `CContentServerDirectory_GetManifestRequestCode_Response` (`EMsg 147`)
/// / `CMsgClientGetManifestRequestCodeResponse` (`EMsg 5462`):
/// - `manifest_request_code = 1` (`uint64`, wire tag `0x08`)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GetManifestRequestCodeResponse {
    pub manifest_request_code: u64,
}

impl GetManifestRequestCodeResponse {
    pub fn parse_from_array(data: &[u8]) -> Result<Self, String> {
        Self::decode_proto(data, MSG_GET_MANIFEST_REQUEST_CODE_RESPONSE)
    }

    pub fn decode_proto(data: &[u8], emsg: u32) -> Result<Self, String> {
        let mut field1: Option<u64> = None;
        let mut field2: Option<u64> = None;
        let mut pos = 0usize;

        while pos < data.len() {
            if data[pos] == 0 {
                break;
            }
            let (tag, tag_len) =
                read_varint(&data[pos..]).ok_or_else(|| "truncated response tag".to_string())?;
            let field_num = (tag >> 3) as u32;
            let wire_type = (tag & 0x07) as u8;
            if field_num == 0 {
                return Err("invalid response field 0".to_string());
            }
            pos += tag_len;

            match (field_num, wire_type) {
                (1, 0) => {
                    let (v, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "truncated manifest_request_code varint".to_string())?;
                    field1 = Some(v);
                    pos += n;
                }
                (1, 1) => {
                    if pos + 8 > data.len() {
                        return Err("truncated manifest_request_code fixed64".to_string());
                    }
                    field1 = Some(u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap()));
                    pos += 8;
                }
                (2, 0) => {
                    let (v, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "truncated response field 2".to_string())?;
                    field2 = Some(v);
                    pos += n;
                }
                (2, 1) => {
                    if pos + 8 > data.len() {
                        return Err("truncated response field 2 fixed64".to_string());
                    }
                    field2 = Some(u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap()));
                    pos += 8;
                }
                (_, 0) => {
                    let (_, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "truncated unknown response varint".to_string())?;
                    pos += n;
                }
                (_, 1) => {
                    if pos + 8 > data.len() {
                        return Err("truncated unknown response fixed64".to_string());
                    }
                    pos += 8;
                }
                (_, 2) => {
                    let (len, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "truncated unknown response bytes".to_string())?;
                    let end = pos.saturating_add(n).saturating_add(len as usize);
                    if end > data.len() {
                        return Err("truncated unknown response payload".to_string());
                    }
                    pos = end;
                }
                (_, 5) => {
                    if pos + 4 > data.len() {
                        return Err("truncated unknown response fixed32".to_string());
                    }
                    pos += 4;
                }
                _ => return Err(format!("unsupported response wire type {}", wire_type)),
            }
        }

        let code = if emsg == MSG_CLIENT_GET_MANIFEST_REQUEST_CODE_RESPONSE {
            field2.filter(|&c| c > 0).or(field1).unwrap_or(0)
        } else {
            field1.unwrap_or(0)
        };

        Ok(Self {
            manifest_request_code: code,
        })
    }

    /// Encode `CContentServerDirectory_GetManifestRequestCode_Response` (`tag 0x08` + varint `uint64`).
    pub fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(11);
        out.push(0x08); // field 1, uint64 varint
        encode_varint(self.manifest_request_code, &mut out);
        out
    }
}

/// ManifestClient - handles async HTTP fetch for manifest request codes using background thread.
pub struct ManifestClient;

impl ManifestClient {
    /// Fetch manifest request code via HTTP (spawns background thread with channel).
    fn fetch_manifest_request_code(
        depot_id: u32,
        manifest_gid: u64,
        hubcap_api_key: Option<String>,
    ) -> mpsc::Receiver<u64> {
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            let code =
                Self::fetch_manifest_request_code_sync(depot_id, manifest_gid, hubcap_api_key);
            let _ = tx.send(code.unwrap_or(0));
        });

        rx
    }

    /// Synchronous manifest request code fetch using blocking HTTP with multi-endpoint fallback.
    pub fn fetch_manifest_request_code_sync(
        depot_id: u32,
        manifest_gid: u64,
        hubcap_api_key: Option<String>,
    ) -> Option<u64> {
        if depot_id == 0 || manifest_gid == 0 {
            return None;
        }
        if let Some(donor_code) = get_harvested_code(depot_id, manifest_gid) {
            return Some(donor_code);
        }

        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(5))
            .build();

        // 1. If Hubcap API key is available, query Hubcap endpoint first
        if let Some(ref api_key) = hubcap_api_key {
            let key = api_key.trim();
            if !key.is_empty() {
                let hubcap_urls = [
                    format!(
                        "https://api.hubcap.app/v1/depots/{}/manifests/{}/request_code",
                        depot_id, manifest_gid
                    ),
                    format!(
                        "https://manifest.opensteamtool.com/{}/{}",
                        depot_id, manifest_gid
                    ),
                ];
                for url in &hubcap_urls {
                    let req = agent
                        .get(url)
                        .set("Authorization", &format!("Bearer {}", key));
                    if let Ok(resp) = req.call() {
                        if let Ok(text) = resp.into_string() {
                            if let Some(code) = Self::parse_manifest_request_code(&text) {
                                log::info!(
                                    "[netpacket_manifest] Resolved request code {} from Hubcap/OST: {}",
                                    code,
                                    url
                                );
                                return Some(code);
                            }
                        }
                    }
                }
            }
        }

        // 2. Standard fallback endpoints: manifest.opensteamtool.com, gmrc.wudrm.com, manifest.steam.run
        let endpoints = [
            format!(
                "https://manifest.opensteamtool.com/{}/{}",
                depot_id, manifest_gid
            ),
            format!("https://manifest.opensteamtool.com/{}", manifest_gid),
            format!("http://gmrc.wudrm.com/manifest/{}", manifest_gid),
            format!("https://manifest.steam.run/api/manifest/{}", manifest_gid),
        ];

        for url in &endpoints {
            if let Ok(resp) = agent.get(url).call() {
                if let Ok(text) = resp.into_string() {
                    if let Some(code) = Self::parse_manifest_request_code(&text) {
                        log::info!(
                            "[netpacket_manifest] Resolved request code {} from {}",
                            code,
                            url
                        );
                        return Some(code);
                    }
                }
            }
        }

        log::warn!(
            "[netpacket_manifest] Failed to resolve manifest request code for depot={} gid={}",
            depot_id,
            manifest_gid
        );
        None
    }

    /// Parse manifest request code from string (decimal, hex with 0x prefix, or JSON object).
    pub fn parse_manifest_request_code(raw: &str) -> Option<u64> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }

        if trimmed.starts_with('{') {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(n) = val
                    .get("content")
                    .or_else(|| val.get("code"))
                    .or_else(|| val.get("request_code"))
                    .or_else(|| val.get("manifest_request_code"))
                {
                    if let Some(num) = n.as_u64() {
                        if num > 0 {
                            return Some(num);
                        }
                    }
                    if let Some(s) = n.as_str() {
                        if let Some(parsed) = Self::parse_manifest_request_code(s) {
                            return Some(parsed);
                        }
                    }
                }
            }
        }

        if let Some(hex_part) = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
        {
            u64::from_str_radix(hex_part.trim(), 16)
                .ok()
                .filter(|&c| c > 0)
        } else if let Ok(dec) = trimmed.parse::<u64>() {
            (dec > 0).then_some(dec)
        } else if trimmed.len() == 16 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
            u64::from_str_radix(trimmed, 16).ok().filter(|&c| c > 0)
        } else {
            None
        }
    }
}

fn is_gmrc_job_name(name: &str) -> bool {
    name == GMRC_TARGET_JOB_NAME
        || name.starts_with("ContentServerDirectory.GetManifestRequestCode")
}

/// Handle outgoing NetPacket message (`EMsg 151` `ContentServerDirectory.GetManifestRequestCode#1`
/// or `EMsg 5461` `CMsgClientGetManifestRequestCode`).
///
/// Supports both full `MsgHdrProtoBuf` frames (`p_body` empty) and split `(p_hdr, p_body)` slices.
pub fn handle_send_get_manifest_request_code(
    p_hdr: &mut [u8],
    p_body: &mut [u8],
    steam_root: Option<PathBuf>,
) -> bool {
    let Ok((base_emsg, hdr, body_bytes, _has_prefix)) =
        parse_packet_slices(p_hdr, p_body, MSG_GET_MANIFEST_REQUEST_CODE)
    else {
        return false;
    };

    match base_emsg {
        MSG_GET_MANIFEST_REQUEST_CODE => {
            if let Some(ref job_name) = hdr.target_job_name {
                if !is_gmrc_job_name(job_name) {
                    return false;
                }
            }
        }
        MSG_CLIENT_GET_MANIFEST_REQUEST_CODE => {}
        _ => return false,
    }

    let Ok(req) = GetManifestRequestCodeRequest::parse_from_array(&body_bytes) else {
        return false;
    };

    let depot_id = req.depot_id;
    let manifest_gid = req.manifest_gid;
    if depot_id == 0 || manifest_gid == 0 {
        return false;
    }

    let mut state = manifest_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(root) = steam_root {
        state.set_steam_root(root);
    } else {
        state.ensure_steam_root();
    }

    let app_id = if req.app_id != 0 {
        req.app_id
    } else if let Some((seen_app, _)) = state.lookup_depot(depot_id) {
        if seen_app != 0 {
            seen_app
        } else {
            resolve_app_id()
        }
    } else {
        resolve_app_id()
    };

    let job_id = hdr.jobid_source;
    let is_lua_depot = state.is_lua_depot(depot_id);
    let is_owned = state.is_owned_depot(depot_id);

    // Pre-seed depotcache manifest from archive / local depotcache check
    state.pre_seed_manifest(app_id, depot_id, manifest_gid);

    if job_id != 0 {
        state.sent_requests.insert(
            job_id,
            SentRequest {
                depot_id,
                manifest_gid,
                app_id,
                is_lua_depot,
                is_owned,
                sent_at: Instant::now(),
            },
        );

        // For Lua depots NOT genuinely owned: reuse harvested donor code or launch async HTTP fetch
        if is_lua_depot && !is_owned {
            let receiver = if let Some(&donor_code) =
                state.harvested_codes.get(&(depot_id, manifest_gid))
            {
                let (tx, rx) = mpsc::channel();
                let _ = tx.send(donor_code);
                rx
            } else {
                let hubcap_key = crate::models::config::SlssteamConfig::default()
                    .extra_fields
                    .get("hubcap_api_key")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string);
                ManifestClient::fetch_manifest_request_code(depot_id, manifest_gid, hubcap_key)
            };

            state
                .pending_fetches
                .insert(job_id, (depot_id, manifest_gid, receiver, Instant::now()));

            log::debug!(
                "[netpacket_manifest] Outgoing eMsg {}: jobid=0x{:X} depot={} gid={} app={} branch={:?} lua={} owned={} -> async fetch launched",
                base_emsg,
                job_id,
                depot_id,
                manifest_gid,
                app_id,
                req.app_branch,
                is_lua_depot,
                is_owned
            );
        } else {
            log::debug!(
                "[netpacket_manifest] Outgoing eMsg {}: jobid=0x{:X} depot={} gid={} app={} lua={} owned={} (passive capture)",
                base_emsg,
                job_id,
                depot_id,
                manifest_gid,
                app_id,
                is_lua_depot,
                is_owned
            );
        }
    }

    false // Do not block the original outgoing packet
}

/// Handle incoming NetPacket response (`EMsg 147` `k_EMsgServiceMethodResponse` or `EMsg 5462`).
///
/// Harvests genuine request codes from owned depots, and for tracked unowned Lua depot requests
/// patches `CMsgProtoBufHeader.eresult = 1` (`0x68 0x01`) and response body `manifest_request_code`
/// (`0x08` + varint `uint64`).
///
/// Supports both full `MsgHdrProtoBuf` frames (`p_body` empty) and split `(p_hdr, p_body)` slices.
pub fn handle_recv_get_manifest_request_code_response(
    p_hdr: &mut [u8],
    p_body: &mut [u8],
) -> bool {
    let Ok((base_emsg, mut hdr, body_bytes, has_prefix)) =
        parse_packet_slices(p_hdr, p_body, MSG_GET_MANIFEST_REQUEST_CODE_RESPONSE)
    else {
        return false;
    };

    if base_emsg != MSG_GET_MANIFEST_REQUEST_CODE_RESPONSE
        && base_emsg != MSG_CLIENT_GET_MANIFEST_REQUEST_CODE_RESPONSE
    {
        return false;
    }

    if let Some(ref job_name) = hdr.target_job_name {
        if !job_name.is_empty() && !is_gmrc_job_name(job_name) {
            return false;
        }
    }

    let job_id = hdr.jobid_target;
    if job_id == 0 {
        return false;
    }

    let Ok(resp) = GetManifestRequestCodeResponse::decode_proto(&body_bytes, base_emsg) else {
        return false;
    };

    let hdr_is_ok = matches!(hdr.eresult, Some(1) | None);

    // Extract tracking state while holding lock briefly, then drop lock before channel wait.
    let (sent_request, pending_entry, donor_fallback) = {
        let mut state = manifest_state().lock().unwrap_or_else(|e| e.into_inner());
        let sent_request = state.sent_requests.remove(&job_id);

        // Passive capture: harvest genuine codes from owned depots (or genuine server OK responses)
        if let Some(ref req) = sent_request {
            if (req.is_owned || !req.is_lua_depot) && hdr_is_ok && resp.manifest_request_code != 0 {
                state
                    .harvested_codes
                    .insert((req.depot_id, req.manifest_gid), resp.manifest_request_code);
                log::debug!(
                    "[netpacket_manifest] Harvested genuine code: app={} depot={} gid={} code=0x{:X}",
                    req.app_id,
                    req.depot_id,
                    req.manifest_gid,
                    resp.manifest_request_code
                );
            }
        }

        let pending_entry = state.pending_fetches.remove(&job_id);
        let donor_fallback = sent_request.and_then(|req| {
            if req.is_lua_depot && !req.is_owned {
                state
                    .harvested_codes
                    .get(&(req.depot_id, req.manifest_gid))
                    .copied()
            } else {
                None
            }
        });

        (sent_request, pending_entry, donor_fallback)
    };

    // Resolve code from pending async fetch or harvested donor code table
    let resolved_code = if let Some((depot_id, manifest_gid, receiver, _started_at)) = pending_entry
    {
        match receiver.recv_timeout(PENDING_TIMEOUT) {
            Ok(code) if code != 0 => Some(code),
            Ok(_) => {
                log::warn!(
                    "[netpacket_manifest] Async fetch returned 0 for jobid=0x{:X} (depot={} gid={})",
                    job_id,
                    depot_id,
                    manifest_gid
                );
                donor_fallback.filter(|&c| c != 0)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                log::warn!(
                    "[netpacket_manifest] Async fetch timed out for jobid=0x{:X} (depot={} gid={})",
                    job_id,
                    depot_id,
                    manifest_gid
                );
                donor_fallback.filter(|&c| c != 0)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                log::warn!(
                    "[netpacket_manifest] Async fetch channel disconnected for jobid=0x{:X}",
                    job_id
                );
                donor_fallback.filter(|&c| c != 0)
            }
        }
    } else if sent_request
        .map(|r| r.is_lua_depot && !r.is_owned && (!hdr_is_ok || resp.manifest_request_code == 0))
        .unwrap_or(false)
    {
        donor_fallback.filter(|&c| c != 0)
    } else {
        None
    };

    let Some(fetched_code) = resolved_code else {
        return false;
    };

    // Patch CMsgProtoBufHeader (eresult = 1 OK, strip error_message) + response body (tag 0x08 varint)
    hdr.eresult = Some(1);
    hdr.clear_error_message();
    let new_hdr_proto = hdr.encode_proto();
    let new_body_proto = GetManifestRequestCodeResponse {
        manifest_request_code: fetched_code,
    }
    .encode_proto();

    if p_body.is_empty() && has_prefix {
        // Full-frame mode inside p_hdr: [emsg | 0x8000_0000][hdr_len][proto_hdr][proto_body]
        let needed = 8 + new_hdr_proto.len() + new_body_proto.len();
        if p_hdr.len() < needed {
            return false;
        }
        p_hdr[0..4].copy_from_slice(&(base_emsg | KM_MSG_HDR_PROTO_FLAG).to_le_bytes());
        p_hdr[4..8].copy_from_slice(&(new_hdr_proto.len() as u32).to_le_bytes());
        p_hdr[8..8 + new_hdr_proto.len()].copy_from_slice(&new_hdr_proto);
        let body_start = 8 + new_hdr_proto.len();
        p_hdr[body_start..body_start + new_body_proto.len()].copy_from_slice(&new_body_proto);
        if p_hdr.len() > needed {
            p_hdr[needed..].fill(0);
        }
    } else if has_prefix {
        let needed_hdr = 8 + new_hdr_proto.len();
        if p_hdr.len() < needed_hdr || p_body.len() < new_body_proto.len() {
            return false;
        }
        p_hdr[0..4].copy_from_slice(&(base_emsg | KM_MSG_HDR_PROTO_FLAG).to_le_bytes());
        p_hdr[4..8].copy_from_slice(&(new_hdr_proto.len() as u32).to_le_bytes());
        p_hdr[8..needed_hdr].copy_from_slice(&new_hdr_proto);
        if p_hdr.len() > needed_hdr {
            p_hdr[needed_hdr..].fill(0);
        }
        p_body[..new_body_proto.len()].copy_from_slice(&new_body_proto);
        if p_body.len() > new_body_proto.len() {
            p_body[new_body_proto.len()..].fill(0);
        }
    } else {
        if p_hdr.len() < new_hdr_proto.len() || p_body.len() < new_body_proto.len() {
            return false;
        }
        p_hdr[..new_hdr_proto.len()].copy_from_slice(&new_hdr_proto);
        if p_hdr.len() > new_hdr_proto.len() {
            p_hdr[new_hdr_proto.len()..].fill(0);
        }
        p_body[..new_body_proto.len()].copy_from_slice(&new_body_proto);
        if p_body.len() > new_body_proto.len() {
            p_body[new_body_proto.len()..].fill(0);
        }
    }

    log::info!(
        "[netpacket_manifest] Patched response for jobid=0x{:X}: eresult=1 code=0x{:X}",
        job_id,
        fetched_code
    );
    true
}

/// Resizable full-frame wrapper around [`handle_recv_get_manifest_request_code_response`] for
/// `NetPacketContext` buffers (where an incoming `AccessDenied` response may have a 0-byte body
/// that expands up to 11 bytes when `manifest_request_code` is patched).
pub fn handle_recv_get_manifest_request_code_response_frame(frame: &mut Vec<u8>) -> bool {
    if frame.len() < 8 {
        return false;
    }
    let raw_emsg = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]);
    let hdr_len = u32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]) as usize;
    if (raw_emsg & KM_MSG_HDR_PROTO_FLAG) == 0 || 8usize.saturating_add(hdr_len) > frame.len() {
        return false;
    }

    let hdr_end = 8 + hdr_len;
    // Provide spare capacity in scratch slices; hdr_scratch[4..8] still stores exact hdr_len,
    // and trailing 0x00 bytes in body_scratch terminate protobuf tag parsing cleanly.
    let mut hdr_scratch = Vec::with_capacity(hdr_end + 16);
    hdr_scratch.extend_from_slice(&frame[..hdr_end]);
    hdr_scratch.resize(hdr_end + 16, 0);

    let orig_body_len = frame.len() - hdr_end;
    let body_cap = orig_body_len.max(16);
    let mut body_scratch = Vec::with_capacity(body_cap);
    body_scratch.extend_from_slice(&frame[hdr_end..]);
    body_scratch.resize(body_cap, 0);

    if !handle_recv_get_manifest_request_code_response(&mut hdr_scratch, &mut body_scratch) {
        return false;
    }

    let new_hdr_len = u32::from_le_bytes([
        hdr_scratch[4],
        hdr_scratch[5],
        hdr_scratch[6],
        hdr_scratch[7],
    ]) as usize;
    if 8usize.saturating_add(new_hdr_len) > hdr_scratch.len() {
        return false;
    }

    // Measure exact protobuf length of patched body (`0x08` + varint)
    let new_body_len = if body_scratch.first() == Some(&0x08) {
        read_varint(&body_scratch[1..])
            .map(|(_, n)| 1 + n)
            .unwrap_or(0)
    } else {
        0
    };
    if new_body_len == 0 || new_body_len > body_scratch.len() {
        return false;
    }

    frame.clear();
    frame.reserve(8 + new_hdr_len + new_body_len);
    frame.extend_from_slice(&hdr_scratch[..8 + new_hdr_len]);
    frame.extend_from_slice(&body_scratch[..new_body_len]);
    true
}

/// Get a harvested code for a `(depot_id, manifest_gid)` pair (for donor code reuse).
pub fn get_harvested_code(depot_id: u32, manifest_gid: u64) -> Option<u64> {
    let state = manifest_state().lock().unwrap_or_else(|e| e.into_inner());
    state.harvested_codes.get(&(depot_id, manifest_gid)).copied()
}

/// Record a harvested or probed manifest request code for donor reuse.
pub fn record_harvested_code(depot_id: u32, manifest_gid: u64, code: u64) {
    if depot_id == 0 || manifest_gid == 0 || code == 0 {
        return;
    }
    let mut state = manifest_state().lock().unwrap_or_else(|e| e.into_inner());
    state.harvested_codes.insert((depot_id, manifest_gid), code);
    drop(state);

    // BST-45: Passive capture for ManifestDonor
    crate::steam_bridge::manifest_donor::submit_captured_code(depot_id, manifest_gid, code);
}

/// Install Manifest NetPacket hooks.
pub fn install_manifest_hooks(steam_root: PathBuf) -> Result<(), String> {
    let mut state = manifest_state().lock().unwrap_or_else(|e| e.into_inner());
    if steam_root.as_os_str().is_empty() {
        state.ensure_steam_root();
    } else {
        state.set_steam_root(steam_root);
    }

    MANIFEST_ACTIVE.store(true, Ordering::Release);
    log::info!("Installed Manifest NetPacket hooks");
    Ok(())
}

/// Uninstall Manifest NetPacket hooks.
pub fn uninstall_manifest_hooks() {
    MANIFEST_ACTIVE.store(false, Ordering::Release);
    let mut state = manifest_state().lock().unwrap_or_else(|e| e.into_inner());
    state.sent_requests.clear();
    state.pending_fetches.clear();
    log::info!("Uninstalled Manifest NetPacket hooks");
}

pub fn install_netpacket_manifest_hooks(steam_root: PathBuf) -> Result<(), String> {
    install_manifest_hooks(steam_root)
}

pub fn uninstall_netpacket_manifest_hooks() {
    uninstall_manifest_hooks()
}

pub fn is_netpacket_manifest_hook_active() -> bool {
    MANIFEST_ACTIVE.load(Ordering::Acquire)
}

/// Expire stale pending requests and abandoned async fetches (called periodically).
pub fn expire_manifest_stale() {
    let mut state = manifest_state().lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    state
        .sent_requests
        .retain(|_, req| now.duration_since(req.sent_at) < PENDING_TIMEOUT);
    state
        .pending_fetches
        .retain(|_, (_, _, _, started)| now.duration_since(*started) < PENDING_TIMEOUT);
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_GUARD: Mutex<()> = Mutex::new(());

    fn reset_state_for_test() {
        let mut state = manifest_state().lock().unwrap_or_else(|e| e.into_inner());
        state.sent_requests.clear();
        state.pending_fetches.clear();
        state.harvested_codes.clear();
    }

    #[test]
    fn test_real_cmsg_protobuf_header_roundtrip() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        let hdr = CMsgProtoBufHeader {
            steamid: Some(76561198012345678),
            client_sessionid: Some(4242),
            jobid_source: 0x0123_4567_89AB_CDEF,
            jobid_target: 0xFEDC_BA98_7654_3210,
            target_job_name: Some(GMRC_TARGET_JOB_NAME.to_string()),
            eresult: Some(1),
            emsg: MSG_GET_MANIFEST_REQUEST_CODE,
            unknown_fields: Vec::new(),
        };

        let proto_bytes = hdr.encode_proto();
        // Verify genuine Steam field tags: 0x09 (1 fixed64), 0x10 (2 int32), 0x51 (10 fixed64),
        // 0x59 (11 fixed64), 0x62 (12 string), 0x68 (13 int32)
        assert_eq!(proto_bytes[0], 0x09);
        assert!(proto_bytes.contains(&0x51));
        assert!(proto_bytes.contains(&0x59));
        assert!(proto_bytes.contains(&0x62));
        assert!(proto_bytes.contains(&0x68));

        let mut framed = Vec::new();
        framed.extend_from_slice(
            &(MSG_GET_MANIFEST_REQUEST_CODE | KM_MSG_HDR_PROTO_FLAG).to_le_bytes(),
        );
        framed.extend_from_slice(&(proto_bytes.len() as u32).to_le_bytes());
        framed.extend_from_slice(&proto_bytes);

        let parsed = CMsgProtoBufHeader::parse_from_array(&framed).unwrap();
        assert_eq!(parsed.emsg, MSG_GET_MANIFEST_REQUEST_CODE);
        assert_eq!(parsed.steamid, Some(76561198012345678));
        assert_eq!(parsed.client_sessionid, Some(4242));
        assert_eq!(parsed.jobid_source, 0x0123_4567_89AB_CDEF);
        assert_eq!(parsed.jobid_target, 0xFEDC_BA98_7654_3210);
        assert_eq!(
            parsed.target_job_name.as_deref(),
            Some(GMRC_TARGET_JOB_NAME)
        );
        assert_eq!(parsed.eresult, Some(1));
    }

    #[test]
    fn test_request_and_response_protobuf_wire_encoding() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        let req = GetManifestRequestCodeRequest {
            app_id: 730,
            depot_id: 731,
            manifest_gid: 8_449_221_100_998_877_665,
            app_branch: Some("public".to_string()),
        };
        let encoded_req = req.encode_proto();
        assert_eq!(encoded_req[0], 0x08); // app_id = 1
        let parsed_req = GetManifestRequestCodeRequest::parse_from_array(&encoded_req).unwrap();
        assert_eq!(parsed_req, req);

        let resp = GetManifestRequestCodeResponse {
            manifest_request_code: 0xDEAD_BEEF_CAFE_0001,
        };
        let encoded_resp = resp.encode_proto();
        assert_eq!(encoded_resp[0], 0x08); // manifest_request_code = 1 varint
        let parsed_resp = GetManifestRequestCodeResponse::parse_from_array(&encoded_resp).unwrap();
        assert_eq!(parsed_resp.manifest_request_code, 0xDEAD_BEEF_CAFE_0001);
    }

    #[test]
    fn test_donor_code_harvesting_on_full_and_split_frames() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset_state_for_test();

        let tmp = tempfile::tempdir().unwrap();
        let depotcache = tmp.path().join("depotcache");
        std::fs::create_dir_all(&depotcache).unwrap();
        std::fs::write(depotcache.join("481_998877665544332211.manifest"), b"manifest").unwrap();

        let job_id = 0x55AA_1122_3344_7788u64;
        let send_hdr = CMsgProtoBufHeader {
            steamid: Some(76561198000000001),
            client_sessionid: Some(99),
            jobid_source: job_id,
            target_job_name: Some(GMRC_TARGET_JOB_NAME.to_string()),
            ..Default::default()
        };
        let send_req = GetManifestRequestCodeRequest {
            app_id: 480,
            depot_id: 481,
            manifest_gid: 998_877_665_544_332_211,
            app_branch: Some("public".to_string()),
        };
        // Test full-frame mode (`p_body` empty) with EMsg 5461
        let mut full_send = build_protobuf_frame(
            MSG_CLIENT_GET_MANIFEST_REQUEST_CODE,
            &send_hdr,
            &send_req.encode_proto(),
        );
        assert!(!handle_send_get_manifest_request_code(
            &mut full_send,
            &mut [],
            Some(tmp.path().to_path_buf()),
        ));

        // Incoming genuine owned response (EMsg 147) harvests donor code
        let recv_hdr = CMsgProtoBufHeader {
            steamid: Some(76561198000000001),
            jobid_target: job_id,
            target_job_name: Some(GMRC_TARGET_JOB_NAME.to_string()),
            eresult: Some(1),
            ..Default::default()
        };
        let genuine_code = 0xCAFE_BABE_1234_5678u64;
        let recv_body = GetManifestRequestCodeResponse {
            manifest_request_code: genuine_code,
        }
        .encode_proto();
        let mut full_recv =
            build_protobuf_frame(MSG_GET_MANIFEST_REQUEST_CODE_RESPONSE, &recv_hdr, &recv_body);
        let split_idx = 8 + recv_hdr.encode_proto().len();
        let (hdr_slice, body_slice) = full_recv.split_at_mut(split_idx);
        assert!(!handle_recv_get_manifest_request_code_response(
            hdr_slice, body_slice
        ));
        assert_eq!(
            get_harvested_code(481, 998_877_665_544_332_211),
            Some(genuine_code)
        );
    }

    #[test]
    fn test_unowned_lua_depot_response_patching_resizes_access_denied_frame() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset_state_for_test();

        let job_id = 0x7777_8888_9999_AAAAu64;
        let depot_id = 228989u32;
        let manifest_gid = 5_544_332_211_009_988_776u64;
        let resolved_code = 0x9988_7766_5544_3322u64;

        // Seed pending Lua depot request + async resolved channel
        {
            let mut state = manifest_state().lock().unwrap_or_else(|e| e.into_inner());
            state.sent_requests.insert(
                job_id,
                SentRequest {
                    depot_id,
                    manifest_gid,
                    app_id: 1245620,
                    is_lua_depot: true,
                    is_owned: false,
                    sent_at: Instant::now(),
                },
            );
            let (tx, rx) = mpsc::channel();
            tx.send(resolved_code).unwrap();
            state
                .pending_fetches
                .insert(job_id, (depot_id, manifest_gid, rx, Instant::now()));
        }

        // Server returns AccessDenied (eresult = 15) with an EMPTY 0-byte body!
        let mut denied_hdr = CMsgProtoBufHeader {
            steamid: Some(76561198055555555),
            jobid_target: job_id,
            target_job_name: Some(GMRC_TARGET_JOB_NAME.to_string()),
            eresult: Some(15),
            ..Default::default()
        };
        // Include an error_message (field 14, tag 0x72) to verify it gets stripped
        let mut err_field = Vec::new();
        let msg = b"Access Denied";
        encode_varint(msg.len() as u64, &mut err_field);
        err_field.extend_from_slice(msg);
        denied_hdr.unknown_fields.push((0x72, err_field));

        let mut frame =
            build_protobuf_frame(MSG_GET_MANIFEST_REQUEST_CODE_RESPONSE, &denied_hdr, &[]);
        assert!(handle_recv_get_manifest_request_code_response_frame(
            &mut frame
        ));

        // Verify patched frame has eresult = 1 in CMsgProtoBufHeader and tag 0x08 code in body
        let new_hdr_len = u32::from_le_bytes(frame[4..8].try_into().unwrap()) as usize;
        let patched_hdr = CMsgProtoBufHeader::parse_from_array(&frame[..8 + new_hdr_len]).unwrap();
        assert_eq!(patched_hdr.eresult, Some(1));
        assert_eq!(patched_hdr.jobid_target, job_id);
        assert!(patched_hdr.unknown_fields.is_empty());

        let patched_body = &frame[8 + new_hdr_len..];
        assert_eq!(patched_body[0], 0x08);
        let parsed_resp = GetManifestRequestCodeResponse::parse_from_array(patched_body).unwrap();
        assert_eq!(parsed_resp.manifest_request_code, resolved_code);
    }

    #[test]
    fn test_manifest_request_code_parsing() {
        assert_eq!(
            ManifestClient::parse_manifest_request_code("123456789012345678"),
            Some(123456789012345678)
        );
        assert_eq!(
            ManifestClient::parse_manifest_request_code("0xDEADBEEFCAFE0001"),
            Some(0xDEADBEEFCAFE0001)
        );
        assert_eq!(
            ManifestClient::parse_manifest_request_code(r#"{"content": "9876543210123456"}"#),
            Some(9876543210123456)
        );
        assert_eq!(
            ManifestClient::parse_manifest_request_code(r#"{"code": 1122334455}"#),
            Some(1122334455)
        );
        assert_eq!(ManifestClient::parse_manifest_request_code(""), None);
        assert_eq!(
            ManifestClient::parse_manifest_request_code("invalid_text"),
            None
        );
    }
}
