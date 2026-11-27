//! NetPacket Cloud hook layer for Linux.
//!
//! Port of usefulsteamtools `Hooks_NetPacket_Cloud` namespace in `Interop_NetPacket.cpp`.
//!
//! Intercepts outbound `EMsg 151` (`k_EMsgServiceMethodCallFromClient`) `Cloud.*` Unified Service
//! RPCs using spec-accurate Steam `MsgHdrProtoBuf` + `CMsgProtoBufHeader` wire tags (`steamid = 1`,
//! `client_sessionid = 2`, `jobid_source = 10`, `jobid_target = 11`, `target_job_name = 12`,
//! `eresult = 13`), delegates to `CloudRedirectHost` with local filesystem cloud storage fallback
//! (`~/.local/share/luatools/cloud_storage/<account_id>/<app_id>/` honoring `cloud_state.json`),
//! and drains synthesized `EMsg 147` responses via [`drain_cloud_responses`].

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use crate::steam_bridge::cloud_redirect::{get_cloud_redirect_host, is_cloud_redirect_active};
use crate::steam_bridge::netpacket::netpacket_core::{
    register_netpacket_handler, NetPacketContext, NetPacketDirection, NetPacketHandler,
    KM_MSG_HDR_PROTO_FLAG, K_EMSG_SERVICE_METHOD_CALL_FROM_CLIENT, K_EMSG_SERVICE_METHOD_RESPONSE,
};

static CLOUD_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Pending Cloud responses to inject via carrier packets.
static PENDING_CLOUD_RESPONSES: LazyLock<Mutex<VecDeque<Vec<u8>>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));

/// Local SteamID and session ID captured from outbound `CMsgProtoBufHeader`.
static LOCAL_STEAM_SESSION: LazyLock<Mutex<(u64, i32)>> = LazyLock::new(|| Mutex::new((0, 0)));

/// Optional carrier packet delivery callback invoked by inbound/outbound carrier traffic.
pub type CloudCarrierCallback =
    Arc<dyn Fn(&mut NetPacketContext) -> Result<(), String> + Send + Sync + 'static>;

static CARRIER_CALLBACK: LazyLock<Mutex<Option<CloudCarrierCallback>>> =
    LazyLock::new(|| Mutex::new(None));

/// Optional root directory override (for hermetic tests or custom luatools data root).
static CLOUD_DATA_ROOT_OVERRIDE: LazyLock<Mutex<Option<PathBuf>>> =
    LazyLock::new(|| Mutex::new(None));

/// Staged uploads from `BeginFileUpload` / `ClientBeginFileUpload` awaiting `ClientCommitFileUpload`.
#[derive(Debug, Clone)]
struct StagedCloudUpload {
    rel_path: String,
    expected_size: u32,
    timestamp: u64,
}

static STAGED_UPLOADS: LazyLock<Mutex<HashMap<(u32, u32, String), StagedCloudUpload>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedCloudState {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub account_id: Option<u32>,
    #[serde(default)]
    pub apps: Vec<u32>,
    #[serde(default)]
    pub storage_root: Option<PathBuf>,
    #[serde(default)]
    pub app_save_dirs: BTreeMap<u32, PathBuf>,
}

const ERESULT_OK: i32 = 1;
const ERESULT_INVALID_PARAM: i32 = 8;
const ERESULT_FILE_NOT_FOUND: i32 = 9;

fn pending_responses() -> &'static Mutex<VecDeque<Vec<u8>>> {
    &PENDING_CLOUD_RESPONSES
}

fn local_session() -> &'static Mutex<(u64, i32)> {
    &LOCAL_STEAM_SESSION
}

/// Configure a carrier delivery callback used when draining queued `EMsg 147` Cloud responses
/// on live carrier packets.
pub fn set_cloud_carrier_callback(cb: Option<CloudCarrierCallback>) {
    *CARRIER_CALLBACK.lock().unwrap_or_else(|e| e.into_inner()) = cb;
}

/// Override the base `luatools` data directory (`~/.local/share/luatools`) containing
/// `cloud_state.json` and `cloud_storage/<account_id>/<app_id>/`.
pub fn set_cloud_data_root_override(root: Option<PathBuf>) {
    *CLOUD_DATA_ROOT_OVERRIDE
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = root;
}

// ============================================================================
// Protobuf Wire Helpers & Real Steam CMsgProtoBufHeader
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

fn encode_bytes_field(field_num: u32, bytes: &[u8], out: &mut Vec<u8>) {
    encode_varint(u64::from((field_num << 3) | 2), &mut out_ref(out));
    encode_varint(bytes.len() as u64, out);
    out.extend_from_slice(bytes);
}

#[inline]
fn out_ref(v: &mut Vec<u8>) -> &mut Vec<u8> {
    v
}

fn encode_varint_field(field_num: u32, val: u64, out: &mut Vec<u8>) {
    encode_varint(u64::from(field_num << 3), out);
    encode_varint(val, out);
}

fn encode_fixed64_field(field_num: u32, val: u64, out: &mut Vec<u8>) {
    encode_varint(u64::from((field_num << 3) | 1), out);
    out.extend_from_slice(&val.to_le_bytes());
}

fn skip_wire_value(data: &[u8], wire_type: u32) -> Option<usize> {
    match wire_type {
        0 => read_varint(data).map(|(_, n)| n),
        1 => (data.len() >= 8).then_some(8),
        2 => {
            let (len, n) = read_varint(data)?;
            let total = n.checked_add(len as usize)?;
            (total <= data.len()).then_some(total)
        }
        5 => (data.len() >= 4).then_some(4),
        _ => None,
    }
}

/// Find a specific varint field by tag number in protobuf encoded bytes.
fn find_varint_field(data: &[u8], target_field: u32) -> Option<u64> {
    let mut pos = 0;
    while pos < data.len() {
        if data[pos] == 0 {
            break;
        }
        let (tag, consumed) = read_varint(&data[pos..])?;
        pos += consumed;
        let field = (tag >> 3) as u32;
        let wire_type = (tag & 7) as u32;
        if field == target_field && wire_type == 0 {
            return read_varint(&data[pos..]).map(|(v, _)| v);
        }
        pos += skip_wire_value(&data[pos..], wire_type)?;
    }
    None
}

/// Find a specific length-delimited (`bytes` / `string`) field by field number.
fn find_bytes_field(data: &[u8], target_field: u32) -> Option<&[u8]> {
    let mut pos = 0;
    while pos < data.len() {
        if data[pos] == 0 {
            break;
        }
        let (tag, consumed) = read_varint(&data[pos..])?;
        pos += consumed;
        let field = (tag >> 3) as u32;
        let wire_type = (tag & 7) as u32;
        if field == target_field && wire_type == 2 {
            let (len, n) = read_varint(&data[pos..])?;
            pos += n;
            let len = len as usize;
            let end = pos.checked_add(len)?;
            if end <= data.len() {
                return Some(&data[pos..end]);
            }
            return None;
        }
        pos += skip_wire_value(&data[pos..], wire_type)?;
    }
    None
}

fn find_string_field(data: &[u8], target_field: u32) -> Option<String> {
    let raw = find_bytes_field(data, target_field)?;
    std::str::from_utf8(raw).ok().map(ToString::to_string)
}

#[derive(Debug, Default, Clone)]
struct DecodedCloudProtoHeader {
    steam_id: u64,
    client_session_id: i32,
    job_id_source: u64,
    job_id_target: u64,
    target_job_name: Option<String>,
    eresult: Option<i32>,
}

/// Decode real Steam `CMsgProtoBufHeader` fields:
/// `steamid = 1 fixed64`, `client_sessionid = 2 int32`, `jobid_source = 10 fixed64`,
/// `jobid_target = 11 fixed64`, `target_job_name = 12 string`, `eresult = 13 int32`.
fn decode_cmsg_protobuf_header(data: &[u8]) -> Option<DecodedCloudProtoHeader> {
    let mut out = DecodedCloudProtoHeader::default();
    let mut pos = 0usize;
    while pos < data.len() {
        if data[pos] == 0 {
            break;
        }
        let (tag, n) = read_varint(&data[pos..])?;
        pos += n;
        let field = (tag >> 3) as u32;
        let wire = (tag & 7) as u32;
        if field == 0 {
            return None;
        }
        match (field, wire) {
            (1, 1) => {
                if pos + 8 > data.len() {
                    return None;
                }
                out.steam_id = u64::from_le_bytes(data[pos..pos + 8].try_into().ok()?);
                pos += 8;
            }
            (1, 0) => {
                let (v, vn) = read_varint(&data[pos..])?;
                out.steam_id = v;
                pos += vn;
            }
            (2, 0) => {
                let (v, vn) = read_varint(&data[pos..])?;
                out.client_session_id = v as i32;
                pos += vn;
            }
            (10, 1) => {
                if pos + 8 > data.len() {
                    return None;
                }
                out.job_id_source = u64::from_le_bytes(data[pos..pos + 8].try_into().ok()?);
                pos += 8;
            }
            (10, 0) => {
                let (v, vn) = read_varint(&data[pos..])?;
                out.job_id_source = v;
                pos += vn;
            }
            (11, 1) => {
                if pos + 8 > data.len() {
                    return None;
                }
                out.job_id_target = u64::from_le_bytes(data[pos..pos + 8].try_into().ok()?);
                pos += 8;
            }
            (11, 0) => {
                let (v, vn) = read_varint(&data[pos..])?;
                out.job_id_target = v;
                pos += vn;
            }
            (12, 2) => {
                let (len, ln) = read_varint(&data[pos..])?;
                pos += ln;
                let len = len as usize;
                let end = pos.checked_add(len)?;
                if end > data.len() {
                    return None;
                }
                out.target_job_name = Some(std::str::from_utf8(&data[pos..end]).ok()?.into());
                pos = end;
            }
            (13, 0) => {
                let (v, vn) = read_varint(&data[pos..])?;
                out.eresult = Some(v as i32);
                pos += vn;
            }
            _ => {
                pos += skip_wire_value(&data[pos..], wire)?;
            }
        }
    }
    Some(out)
}

/// Extract `(proto_hdr_slice, proto_body_slice)` from a `MsgHdrProtoBuf` frame.
fn split_proto_frame(ctx: &NetPacketContext) -> (&[u8], &[u8]) {
    if ctx.is_proto && ctx.frame_buffer.len() >= 8 {
        let hdr_len = u32::from_le_bytes([
            ctx.frame_buffer[4],
            ctx.frame_buffer[5],
            ctx.frame_buffer[6],
            ctx.frame_buffer[7],
        ]) as usize;
        if 8usize.saturating_add(hdr_len) <= ctx.frame_buffer.len() {
            return (
                &ctx.frame_buffer[8..8 + hdr_len],
                &ctx.frame_buffer[8 + hdr_len..],
            );
        }
    }
    let body_off = ctx.body_offset.min(ctx.frame_buffer.len());
    let hdr_start = 8usize.min(body_off);
    (
        &ctx.frame_buffer[hdr_start..body_off],
        &ctx.frame_buffer[body_off..],
    )
}

/// Search for `Cloud.*` service method name in a buffer (fallback if omitted from field 12).
fn extract_cloud_job_name(data: &[u8]) -> Option<String> {
    let needle = b"Cloud.";
    if data.len() < needle.len() {
        return None;
    }
    for i in 0..=data.len() - needle.len() {
        if &data[i..i + needle.len()] == needle {
            // 1. Honor preceding field-12 length byte (`0x62, len` or `0x0A, len`) when valid
            if i >= 2 && (data[i - 2] == 0x62 || data[i - 2] == 0x0A) {
                let declared_len = data[i - 1] as usize;
                if declared_len >= needle.len() && i + declared_len <= data.len() {
                    let candidate = &data[i..i + declared_len];
                    if candidate
                        .iter()
                        .all(|&b| b.is_ascii_alphanumeric() || b == b'.' || b == b'#' || b == b'_')
                    {
                        if let Ok(s) = std::str::from_utf8(candidate) {
                            return Some(s.to_string());
                        }
                    }
                }
            }

            // 2. Fallback scanner: once `#` version suffix starts, accept only ASCII digits
            let mut end = i + needle.len();
            let mut seen_hash = false;
            while end < data.len() {
                let b = data[end];
                if seen_hash {
                    if b.is_ascii_digit() {
                        end += 1;
                    } else {
                        break;
                    }
                } else if b == b'#' {
                    seen_hash = true;
                    end += 1;
                } else if b.is_ascii_alphanumeric() || b == b'.' || b == b'_' {
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

/// Extract `appid` from Cloud RPC request body.
/// - `Cloud.ClientCommitFileUpload#1` / `Cloud.CommitFileUpload#1`: field 2 (`transfer_succeeded` is field 1).
/// - `Cloud.Delete#1`: `CCloud_Delete_Request` has `filename = 1 (string), appid = 2 (uint32)`,
///   while `ClientDeleteFile` may pass `appid = 1 (uint32)`.
/// - All other `Cloud.*`: field 1 (`uint32`).
fn extract_app_id(job_name: &str, body: &[u8]) -> u32 {
    if job_name == "Cloud.ClientCommitFileUpload#1" || job_name == "Cloud.CommitFileUpload#1" {
        return find_varint_field(body, 2).map(|v| v as u32).unwrap_or(0);
    }
    if job_name == "Cloud.Delete#1" || job_name == "Cloud.ClientDeleteFile#1" {
        return find_varint_field(body, 1)
            .or_else(|| find_varint_field(body, 2))
            .map(|v| v as u32)
            .unwrap_or(0);
    }
    find_varint_field(body, 1).map(|v| v as u32).unwrap_or(0)
}

/// Build a real Steam `CMsgProtoBufHeader` for `k_EMsgServiceMethodResponse` (`EMsg 147`):
/// - `steamid = 1` (`fixed64`, tag `0x09`)
/// - `client_sessionid = 2` (`int32`, tag `0x10`)
/// - `jobid_source = 10` (`fixed64`, tag `0x51`, `u64::MAX` `k_GIDNil`)
/// - `jobid_target = 11` (`fixed64`, tag `0x59`)
/// - `target_job_name = 12` (`string`, tag `0x62`)
/// - `eresult = 13` (`int32`, tag `0x68`)
fn build_response_header(job_id: u64, job_name: &str, eresult: i32) -> Vec<u8> {
    let (steam_id, session_id) = *local_session().lock().unwrap_or_else(|e| e.into_inner());
    let mut hdr = Vec::with_capacity(64);
    if steam_id != 0 {
        encode_fixed64_field(1, steam_id, &mut hdr); // tag 0x09
    }
    if session_id != 0 {
        encode_varint_field(2, session_id as i64 as u64, &mut hdr); // tag 0x10
    }
    encode_fixed64_field(10, u64::MAX, &mut hdr); // tag 0x51 (k_GIDNil)
    encode_fixed64_field(11, job_id, &mut hdr); // tag 0x59 (jobid_target)
    encode_bytes_field(12, job_name.as_bytes(), &mut hdr); // tag 0x62 (target_job_name)
    encode_varint_field(13, eresult as i64 as u64, &mut hdr); // tag 0x68 (eresult)
    hdr
}

// ============================================================================
// Local Filesystem Cloud Storage Fallback (`cloud_state.json` + `cloud_storage`)
// ============================================================================

fn luatools_data_root() -> PathBuf {
    if let Some(custom) = CLOUD_DATA_ROOT_OVERRIDE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        return custom;
    }
    if let Ok(env_dir) = std::env::var("LUATOOLS_CLOUD_DIR") {
        let trimmed = env_dir.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    let home = dirs::home_dir()
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".local/share/luatools")
}

fn load_persisted_cloud_state() -> Option<PersistedCloudState> {
    let data_root = luatools_data_root();
    let primary = data_root.join("cloud_state.json");
    if let Ok(raw) = fs::read_to_string(&primary) {
        if let Ok(parsed) = serde_json::from_str::<PersistedCloudState>(&raw) {
            return Some(parsed);
        }
    }

    // If using default luatools data root, also check Steam CloudRedirect's cloud_state.json
    if CLOUD_DATA_ROOT_OVERRIDE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_none()
    {
        if let Some(home) = dirs::home_dir() {
            let fallbacks = [
                home.join(".local/share/Steam/config/cloudredirect/cloud_state.json"),
                home.join(".local/share/CloudRedirect/cloud_state.json"),
            ];
            for path in &fallbacks {
                if let Ok(raw) = fs::read_to_string(path) {
                    if let Ok(parsed) = serde_json::from_str::<PersistedCloudState>(&raw) {
                        return Some(parsed);
                    }
                }
            }
        }
    }
    None
}

fn resolve_app_cloud_dir(
    app_id: u32,
    account_id: u32,
    persisted: Option<&PersistedCloudState>,
) -> PathBuf {
    if let Some(st) = persisted {
        if let Some(ref custom_storage) = st.storage_root {
            return custom_storage
                .join(account_id.to_string())
                .join(app_id.to_string());
        }
    }
    luatools_data_root()
        .join("cloud_storage")
        .join(account_id.to_string())
        .join(app_id.to_string())
}

/// Sanitize a Steam Cloud filename into a safe relative path without `..`, leading `/`, or NULs.
fn sanitize_cloud_filename(raw: &str) -> Option<(String, PathBuf)> {
    let normalized = raw.trim().replace('\\', "/");
    let stripped = normalized.trim_start_matches('/');
    if stripped.is_empty() || stripped.contains('\0') {
        return None;
    }

    let mut clean_rel = PathBuf::new();
    let mut parts = Vec::new();
    for comp in Path::new(stripped).components() {
        match comp {
            Component::Normal(seg) => {
                let s = seg.to_str()?;
                if s.is_empty() || s == "." || s == ".." {
                    return None;
                }
                parts.push(s.to_string());
                clean_rel.push(s);
            }
            Component::CurDir => {}
            _ => return None,
        }
    }

    if parts.is_empty() {
        return None;
    }
    Some((parts.join("/"), clean_rel))
}

fn collect_app_files(base_dir: &Path) -> Vec<(String, PathBuf, u64, u64)> {
    let mut out = Vec::new();
    if !base_dir.is_dir() {
        return out;
    }
    let mut stack = vec![base_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.ends_with(".cloud.tmp") {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    stack.push(path);
                } else if meta.is_file() {
                    if let Ok(rel) = path.strip_prefix(base_dir) {
                        let rel_str = rel
                            .components()
                            .filter_map(|c| c.as_os_str().to_str())
                            .collect::<Vec<_>>()
                            .join("/");
                        let mtime = meta
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        out.push((rel_str, path, meta.len(), mtime));
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Handle `Cloud.*` RPCs locally against `~/.local/share/luatools/cloud_storage/<account_id>/<app_id>/`
/// when `CloudRedirectHost::handle_cloud_rpc` returns empty/stubbed data.
pub fn handle_local_cloud_rpc(
    method: &str,
    app_id: u32,
    account_id: u32,
    req_body: &[u8],
) -> Option<(Vec<u8>, i32)> {
    if app_id == 0 {
        return None;
    }
    let persisted = load_persisted_cloud_state();
    let effective_account = if account_id != 0 {
        account_id
    } else {
        persisted.as_ref().and_then(|s| s.account_id).unwrap_or(0)
    };
    let app_dir = resolve_app_cloud_dir(app_id, effective_account, persisted.as_ref());

    match method {
        "Cloud.EnumerateUserFiles#1" => {
            // CCloud_EnumerateUserFiles_Request: appid=1, extended_details=2, count=3, start_index=4
            let max_count = find_varint_field(req_body, 3).unwrap_or(0) as usize;
            let start_idx = find_varint_field(req_body, 4).unwrap_or(0) as usize;

            let all_files = collect_app_files(&app_dir);
            let total_files = all_files.len() as u32;
            let slice_iter = all_files.into_iter().skip(start_idx);
            let selected: Vec<_> = if max_count > 0 {
                slice_iter.take(max_count).collect()
            } else {
                slice_iter.collect()
            };

            let (steam_id, _) = *local_session().lock().unwrap_or_else(|e| e.into_inner());
            let mut resp = Vec::new();
            for (idx, (rel_name, full_path, size, mtime)) in selected.into_iter().enumerate() {
                let bytes = fs::read(&full_path).unwrap_or_default();
                let mut hasher = Sha1::new();
                hasher.update(&bytes);
                let sha_hex = hex::encode(hasher.finalize());

                // CCloud_UserFile
                let mut file_msg = Vec::with_capacity(96 + rel_name.len());
                encode_varint_field(1, u64::from(app_id), &mut file_msg); // appid = 1
                encode_varint_field(2, (start_idx + idx + 1) as u64, &mut file_msg); // ugcid = 2
                encode_bytes_field(3, rel_name.as_bytes(), &mut file_msg); // filename = 3
                encode_varint_field(4, mtime, &mut file_msg); // timestamp = 4
                encode_varint_field(5, size.min(u32::MAX as u64), &mut file_msg); // file_size = 5
                let file_url = format!("file://{}", full_path.display());
                encode_bytes_field(6, file_url.as_bytes(), &mut file_msg); // url = 6
                if steam_id != 0 {
                    encode_fixed64_field(7, steam_id, &mut file_msg); // steamid_creator = 7
                }
                encode_bytes_field(10, sha_hex.as_bytes(), &mut file_msg); // file_sha = 10
                encode_varint_field(11, size.min(u32::MAX as u64), &mut file_msg); // compressed_file_size = 11

                // CCloud_EnumerateUserFiles_Response.files = 1
                encode_bytes_field(1, &file_msg, &mut resp);
            }
            // CCloud_EnumerateUserFiles_Response.total_files = 2
            encode_varint_field(2, u64::from(total_files), &mut resp);
            Some((resp, ERESULT_OK))
        }

        "Cloud.ClientFileUpload#1"
        | "Cloud.BeginFileUpload#1"
        | "Cloud.ClientBeginFileUpload#1" => {
            // Standard CCloud_ClientBeginFileUpload_Request:
            //   appid=1, file_size=2, raw_file_size=3, file_sha=4, time_stamp=5, filename=6
            // Direct inline ClientFileUpload also accepted:
            //   appid=1, filename=6 (or 2 string), data=8 or 7 (or 3 bytes when filename in 2)
            let filename_field6 = find_string_field(req_body, 6);
            let filename_field2 = if filename_field6.is_none() {
                find_string_field(req_body, 2)
            } else {
                None
            };
            let raw_filename = filename_field6.as_deref().or(filename_field2.as_deref())?;
            let (clean_key, clean_rel) = sanitize_cloud_filename(raw_filename)?;

            let inline_data = find_bytes_field(req_body, 8)
                .or_else(|| find_bytes_field(req_body, 7))
                .or_else(|| {
                    if filename_field2.is_some() {
                        find_bytes_field(req_body, 3).or_else(|| find_bytes_field(req_body, 4))
                    } else {
                        None
                    }
                });

            let file_size = find_varint_field(req_body, 2)
                .or_else(|| find_varint_field(req_body, 3))
                .unwrap_or_else(|| inline_data.map(|b| b.len() as u64).unwrap_or(0))
                as u32;
            let timestamp = find_varint_field(req_body, 5).unwrap_or_else(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            });

            let dest_path = app_dir.join(&clean_rel);
            if let Some(parent) = dest_path.parent() {
                if fs::create_dir_all(parent).is_err() {
                    return Some((Vec::new(), ERESULT_INVALID_PARAM));
                }
            }

            if let Some(payload) = inline_data {
                let tmp_path = dest_path.with_extension("cloud.tmp");
                if fs::write(&tmp_path, payload).is_err()
                    || fs::rename(&tmp_path, &dest_path).is_err()
                {
                    let _ = fs::remove_file(&tmp_path);
                    return Some((Vec::new(), ERESULT_INVALID_PARAM));
                }
            } else {
                if !dest_path.exists() {
                    let _ = fs::write(&dest_path, &[]);
                }
                STAGED_UPLOADS
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(
                        (effective_account, app_id, clean_key.clone()),
                        StagedCloudUpload {
                            rel_path: clean_key,
                            expected_size: file_size,
                            timestamp,
                        },
                    );
            }

            // CCloud_ClientBeginFileUpload_Response: encrypt_file = 1 (false),
            // empty block_requests = 2 (indicates server satisfied locally -> proceed to commit).
            let mut resp = Vec::with_capacity(4);
            encode_varint_field(1, 0, &mut resp); // encrypt_file = false
            Some((resp, ERESULT_OK))
        }

        "Cloud.ClientCommitFileUpload#1" | "Cloud.CommitFileUpload#1" => {
            // CCloud_ClientCommitFileUpload_Request:
            //   transfer_succeeded=1, appid=2, file_sha=3, filename=4, inline_data=5 (optional)
            let transfer_ok = find_varint_field(req_body, 1).unwrap_or(1) != 0;
            let raw_filename = find_string_field(req_body, 4)
                .or_else(|| find_string_field(req_body, 2))?;
            let (clean_key, clean_rel) = sanitize_cloud_filename(&raw_filename)?;
            let dest_path = app_dir.join(&clean_rel);

            let staged = STAGED_UPLOADS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&(effective_account, app_id, clean_key));

            if !transfer_ok {
                let mut resp = Vec::with_capacity(2);
                encode_varint_field(1, 0, &mut resp); // file_committed = false
                return Some((resp, ERESULT_OK));
            }

            if let Some(inline_data) = find_bytes_field(req_body, 5) {
                if let Some(parent) = dest_path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let tmp_path = dest_path.with_extension("cloud.tmp");
                if fs::write(&tmp_path, inline_data).is_ok() {
                    let _ = fs::rename(&tmp_path, &dest_path);
                }
            } else if let Some(st) = staged {
                log::debug!(
                    "Cloud commit finalized file={} expected_size={} ts={}",
                    st.rel_path,
                    st.expected_size,
                    st.timestamp
                );
                if !dest_path.exists() {
                    if let Some(parent) = dest_path.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    let _ = fs::write(&dest_path, &[]);
                }
            }

            let committed = dest_path.is_file();
            let mut resp = Vec::with_capacity(2);
            encode_varint_field(1, u64::from(committed), &mut resp); // file_committed = 1
            Some((resp, ERESULT_OK))
        }

        "Cloud.ClientFileDownload#1" => {
            // CCloud_ClientFileDownload_Request: appid=1, filename=2, realm=3, force_proxy=4
            let Some(raw_filename) = find_string_field(req_body, 2) else {
                return Some((Vec::new(), ERESULT_INVALID_PARAM));
            };
            let Some((clean_key, clean_rel)) = sanitize_cloud_filename(&raw_filename) else {
                return Some((Vec::new(), ERESULT_INVALID_PARAM));
            };
            let file_path = app_dir.join(&clean_rel);
            let Ok(data) = fs::read(&file_path) else {
                return Some((Vec::new(), ERESULT_FILE_NOT_FOUND));
            };

            let mtime = fs::metadata(&file_path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let mut hasher = Sha1::new();
            hasher.update(&data);
            let sha_raw = hasher.finalize();
            let size_u32 = data.len().min(u32::MAX as usize) as u64;

            // CCloud_ClientFileDownload_Response:
            //   appid=1, file_size=2, raw_file_size=3, sha_file=4 (20 bytes), time_stamp=5,
            //   is_explicit_delete=6, url_host=7, url_path=8, use_https=9, encrypted=11, data=12
            let mut resp = Vec::with_capacity(80 + data.len());
            encode_varint_field(1, u64::from(app_id), &mut resp);
            encode_varint_field(2, size_u32, &mut resp);
            encode_varint_field(3, size_u32, &mut resp);
            encode_bytes_field(4, &sha_raw, &mut resp);
            encode_varint_field(5, mtime, &mut resp);
            encode_varint_field(6, 0, &mut resp); // is_explicit_delete = false
            encode_bytes_field(7, b"127.0.0.1", &mut resp);
            let url_path = format!(
                "/luatools/cloud/{}/{}/{}",
                effective_account, app_id, clean_key
            );
            encode_bytes_field(8, url_path.as_bytes(), &mut resp);
            encode_varint_field(9, 0, &mut resp); // use_https = false
            encode_varint_field(11, 0, &mut resp); // encrypted = false
            encode_bytes_field(12, &data, &mut resp); // inline file payload
            Some((resp, ERESULT_OK))
        }

        "Cloud.Delete#1" | "Cloud.ClientDeleteFile#1" => {
            // CCloud_Delete_Request: filename=1 (string), appid=2 (uint32)
            // Also accepts appid=1 (uint32), filename=2 (string)
            let Some(raw_filename) =
                find_string_field(req_body, 1).or_else(|| find_string_field(req_body, 2))
            else {
                return Some((Vec::new(), ERESULT_INVALID_PARAM));
            };
            let Some((clean_key, clean_rel)) = sanitize_cloud_filename(&raw_filename) else {
                return Some((Vec::new(), ERESULT_INVALID_PARAM));
            };
            let file_path = app_dir.join(&clean_rel);
            STAGED_UPLOADS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&(effective_account, app_id, clean_key));

            if file_path.is_file() {
                let _ = fs::remove_file(&file_path);
                let mut parent = file_path.parent();
                while let Some(p) = parent {
                    if p == app_dir || !p.starts_with(&app_dir) {
                        break;
                    }
                    if fs::remove_dir(p).is_err() {
                        break;
                    }
                    parent = p.parent();
                }
            }

            let mut resp = Vec::with_capacity(2);
            encode_varint_field(1, 1, &mut resp); // deleted = true
            Some((resp, ERESULT_OK))
        }

        "Cloud.GetUGCFileDetails#1" => {
            // CCloud_GetUGCFileDetails_Request: hcontent = 1 (uint64), appid = 2 (uint32)
            let hcontent = find_varint_field(req_body, 1).unwrap_or(0);
            let req_app_id = find_varint_field(req_body, 2).map(|v| v as u32).unwrap_or(app_id);
            let all_files = collect_app_files(&app_dir);
            let matched_file = if hcontent > 0 && (hcontent as usize) <= all_files.len() {
                Some(&all_files[(hcontent - 1) as usize])
            } else {
                all_files.first()
            };

            let mut resp = Vec::new();
            encode_varint_field(1, u64::from(req_app_id), &mut resp); // appid = 1
            if let Some((rel_name, full_path, size, mtime)) = matched_file {
                let bytes = fs::read(full_path).unwrap_or_default();
                let mut hasher = Sha1::new();
                hasher.update(&bytes);
                let sha_raw = hasher.finalize();

                let file_url = format!("file://{}", full_path.display());
                encode_bytes_field(2, file_url.as_bytes(), &mut resp); // url = 2
                encode_bytes_field(3, rel_name.as_bytes(), &mut resp); // filename = 3
                encode_varint_field(4, (*size).min(u32::MAX as u64), &mut resp); // file_size = 4
                encode_bytes_field(5, &sha_raw, &mut resp); // file_sha = 5
                encode_varint_field(6, *mtime, &mut resp); // time_stamp = 6
            } else {
                encode_bytes_field(2, b"file:///dev/null", &mut resp);
                encode_bytes_field(3, b"", &mut resp);
                encode_varint_field(4, 0, &mut resp);
                encode_varint_field(6, 0, &mut resp);
            }
            Some((resp, ERESULT_OK))
        }

        _ => None,
    }
}

fn is_app_cloud_redirected(app_id: u32) -> (bool, Option<PersistedCloudState>) {
    let persisted = load_persisted_cloud_state();
    let cloud_host = get_cloud_redirect_host();
    if is_cloud_redirect_active() && cloud_host.is_app(app_id) {
        return (true, persisted);
    }
    if let Some(ref st) = persisted {
        if st.enabled
            && (st.apps.contains(&app_id) || st.app_save_dirs.contains_key(&app_id))
        {
            return (true, persisted);
        }
    }
    (false, persisted)
}

/// Handle outbound `ServiceMethodCallFromClient` (`EMsg 151`) for `Cloud.*` RPCs.
pub fn handle_send_cloud_rpc(ctx: &mut NetPacketContext) -> Result<(), String> {
    let (proto_hdr_slice, proto_body_slice) = split_proto_frame(ctx);
    let decoded_hdr = decode_cmsg_protobuf_header(proto_hdr_slice);

    if let Some(ref hdr) = decoded_hdr {
        if hdr.steam_id != 0 || hdr.client_session_id != 0 {
            let mut sess = local_session().lock().unwrap_or_else(|e| e.into_inner());
            if hdr.steam_id != 0 {
                sess.0 = hdr.steam_id;
            }
            if hdr.client_session_id != 0 {
                sess.1 = hdr.client_session_id;
            }
        }
    }

    let job_name = decoded_hdr
        .as_ref()
        .and_then(|h| h.target_job_name.clone())
        .or_else(|| extract_cloud_job_name(&ctx.frame_buffer));

    let Some(job_name) = job_name else {
        // Non-Cloud outbound packet: if a carrier callback is registered, drain queued responses.
        try_drain_with_registered_callback(ctx)?;
        return Ok(());
    };

    if !job_name.starts_with("Cloud.") {
        try_drain_with_registered_callback(ctx)?;
        return Ok(());
    }

    let mut app_id = extract_app_id(&job_name, proto_body_slice);
    if app_id == 0 {
        app_id = ctx.app_id;
    }
    if app_id == 0 {
        return Ok(());
    }

    let (redirected, persisted) = is_app_cloud_redirected(app_id);
    if !redirected {
        return Ok(());
    }

    let (steam_id, _) = *local_session().lock().unwrap_or_else(|e| e.into_inner());
    let mut account_id = (steam_id & 0xFFFF_FFFF) as u32;
    if account_id == 0 {
        account_id = persisted.as_ref().and_then(|s| s.account_id).unwrap_or(0);
    }

    let body_vec = proto_body_slice.to_vec();
    let cloud_host = get_cloud_redirect_host();
    let host_result = if is_cloud_redirect_active() && cloud_host.is_app(app_id) {
        cloud_host.handle_cloud_rpc(&job_name, app_id, account_id, body_vec.clone())
    } else {
        None
    };

    // Use CloudRedirectHost output when non-empty; otherwise invoke real local cloud storage fallback.
    let (resp_data, eresult) = match host_result {
        Some((data, code)) if !data.is_empty() => (data, code),
        other => match handle_local_cloud_rpc(&job_name, app_id, account_id, &body_vec) {
            Some(local_res) => local_res,
            None => match other {
                Some(stub_res) => stub_res,
                None => return Ok(()),
            },
        },
    };

    let job_id = decoded_hdr
        .as_ref()
        .map(|h| h.job_id_source)
        .unwrap_or(0);

    let hdr_bytes = build_response_header(job_id, &job_name, eresult);

    let mut frame = Vec::with_capacity(8 + hdr_bytes.len() + resp_data.len());
    frame.extend_from_slice(
        &(K_EMSG_SERVICE_METHOD_RESPONSE | KM_MSG_HDR_PROTO_FLAG).to_le_bytes(),
    );
    frame.extend_from_slice(&(hdr_bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(&hdr_bytes);
    frame.extend_from_slice(&resp_data);

    log::debug!(
        "Cloud: handled {} app={} jobid=0x{:X} -> queued {}-byte response (eresult={})",
        job_name,
        app_id,
        job_id,
        frame.len(),
        eresult
    );

    pending_responses()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_back(frame);

    // Suppress original outbound packet to Steam CM
    ctx.frame_buffer.clear();
    Ok(())
}

fn try_drain_with_registered_callback(ctx: &mut NetPacketContext) -> Result<(), String> {
    if pending_responses()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty()
    {
        return Ok(());
    }
    let cb = CARRIER_CALLBACK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some(deliver_fn) = cb {
        drain_cloud_responses(ctx, |carrier| deliver_fn(carrier))?;
    }
    Ok(())
}

/// Handle inbound packets on `RecvPkt` to drain queued Cloud responses via carrier packets.
pub fn handle_recv_cloud_carrier(ctx: &mut NetPacketContext) -> Result<(), String> {
    if pending_responses()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty()
    {
        return Ok(());
    }

    let (hdr_slice, _) = split_proto_frame(ctx);
    let parsed_hdr = decode_cmsg_protobuf_header(hdr_slice);
    let is_cloud_packet = parsed_hdr
        .as_ref()
        .and_then(|h| h.target_job_name.as_deref())
        .map(|n| n.starts_with("Cloud."))
        .unwrap_or_else(|| extract_cloud_job_name(&ctx.frame_buffer).is_some());

    if !is_cloud_packet {
        if let Some(deliver_fn) = CARRIER_CALLBACK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return drain_cloud_responses(ctx, |carrier| deliver_fn(carrier));
        }
    } else if ctx.msg_hdr.base_emsg() == K_EMSG_SERVICE_METHOD_RESPONSE {
        if let Some(ref hdr) = parsed_hdr {
            log::debug!(
                "Cloud carrier replacing inbound EMsg 147 (target=0x{:X}, eresult={:?})",
                hdr.job_id_target,
                hdr.eresult
            );
        }
        // Inbound Cloud EMsg 147 carrier: inject the front queued response into this carrier frame.
        let front = {
            let mut pending = pending_responses().lock().unwrap_or_else(|e| e.into_inner());
            pending.pop_front()
        };
        if let Some(frame) = front {
            ctx.replace_frame(frame);
            if ctx.is_proto && ctx.frame_buffer.len() >= 8 {
                let hdr_len = u32::from_le_bytes([
                    ctx.frame_buffer[4],
                    ctx.frame_buffer[5],
                    ctx.frame_buffer[6],
                    ctx.frame_buffer[7],
                ]) as usize;
                if 8usize.saturating_add(hdr_len) <= ctx.frame_buffer.len() {
                    ctx.body_offset = 8 + hdr_len;
                }
            }
        }
    }
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
            let mut pending = pending_responses().lock().unwrap_or_else(|e| e.into_inner());
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
            if carrier_ctx.is_proto && carrier_ctx.frame_buffer.len() >= 8 {
                let hdr_len = u32::from_le_bytes([
                    carrier_ctx.frame_buffer[4],
                    carrier_ctx.frame_buffer[5],
                    carrier_ctx.frame_buffer[6],
                    carrier_ctx.frame_buffer[7],
                ]) as usize;
                if 8usize.saturating_add(hdr_len) <= carrier_ctx.frame_buffer.len() {
                    carrier_ctx.body_offset = 8 + hdr_len;
                }
            }

            let res = invoke_original(carrier_ctx);

            carrier_ctx.frame_buffer = orig_frame;
            carrier_ctx.msg_hdr = orig_hdr;
            carrier_ctx.body_offset = orig_body_offset;
            carrier_ctx.is_proto = orig_is_proto;

            res?;
        }
    }
}

/// Check if there are any pending synthesized Cloud responses ready to drain.
pub fn has_pending_responses() -> bool {
    !pending_responses()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty()
}

/// Clear all pending synthesized Cloud responses.
pub fn clear_pending_responses() {
    pending_responses()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// Raw pointer carrier draining matching Valve CNetPacket carrier borrow for Cloud responses.
///
/// Port of `Hooks_NetPacket_Cloud::Drain` from `Hooks_NetPacket.cpp:1960-1975`.
pub unsafe fn drain_into_carrier(
    this_ptr: *mut std::ffi::c_void,
    net_pkt: *mut crate::steam_bridge::netpacket::netpacket_core::CNetPacket,
    orig_recv: unsafe extern "C" fn(
        *mut std::ffi::c_void,
        *mut crate::steam_bridge::netpacket::netpacket_core::CNetPacket,
    ),
) {
    if net_pkt.is_null() {
        return;
    }
    while let Some(pkt) = {
        let mut pending = pending_responses().lock().unwrap_or_else(|e| e.into_inner());
        pending.pop_front()
    } {
        let orig_data = (*net_pkt).pub_data;
        let orig_size = (*net_pkt).pub_cub_data;

        let mut pkt_buf = pkt;
        (*net_pkt).pub_data = pkt_buf.as_mut_ptr();
        (*net_pkt).pub_cub_data = pkt_buf.len() as u32;

        orig_recv(this_ptr, net_pkt);

        (*net_pkt).pub_data = orig_data;
        (*net_pkt).pub_cub_data = orig_size;

        log::debug!(
            "Cloud: delivered {}-byte synthesized 147 response via carrier",
            pkt_buf.len()
        );
    }
}

/// Register Cloud Send and Recv carrier handlers with `NetPacketCore`.
pub fn register_cloud_handlers() {
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: K_EMSG_SERVICE_METHOD_CALL_FROM_CLIENT,
        target_job_hash: None,
        name: "Hooks_NetPacket_Cloud::HandleSend".to_string(),
        handler: Arc::new(handle_send_cloud_rpc),
    });

    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Recv,
        emsg: 0, // Wildcard inbound carrier hook for draining pending Cloud responses
        target_job_hash: None,
        name: "Hooks_NetPacket_Cloud::HandleRecvCarrier".to_string(),
        handler: Arc::new(handle_recv_cloud_carrier),
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
    pending_responses()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    STAGED_UPLOADS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    *local_session().lock().unwrap_or_else(|e| e.into_inner()) = (0, 0);
    set_cloud_carrier_callback(None);
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

    static CLOUD_TEST_GUARD: Mutex<()> = Mutex::new(());

    fn build_client_cloud_frame(
        steam_id: u64,
        session_id: i32,
        job_id_source: u64,
        job_name: &str,
        body: &[u8],
    ) -> Vec<u8> {
        let mut hdr = Vec::new();
        encode_fixed64_field(1, steam_id, &mut hdr); // steamid = 1 fixed64
        encode_varint_field(2, session_id as i64 as u64, &mut hdr); // client_sessionid = 2 int32
        encode_fixed64_field(10, job_id_source, &mut hdr); // jobid_source = 10 fixed64
        encode_bytes_field(12, job_name.as_bytes(), &mut hdr); // target_job_name = 12 string

        let mut frame = Vec::with_capacity(8 + hdr.len() + body.len());
        frame.extend_from_slice(
            &(K_EMSG_SERVICE_METHOD_CALL_FROM_CLIENT | KM_MSG_HDR_PROTO_FLAG).to_le_bytes(),
        );
        frame.extend_from_slice(&(hdr.len() as u32).to_le_bytes());
        frame.extend_from_slice(&hdr);
        frame.extend_from_slice(body);
        frame
    }

    #[test]
    fn test_varint_read_write() {
        let test_vals = [
            0u64,
            1,
            127,
            128,
            16383,
            16384,
            0xFFFF_FFFF,
            0x1_0000_0000,
        ];
        for val in test_vals {
            let mut buf = Vec::new();
            encode_varint(val, &mut buf);
            let (decoded, _) = read_varint(&buf).unwrap();
            assert_eq!(decoded, val, "varint roundtrip failed for {}", val);
        }
    }

    #[test]
    fn test_extract_app_id() {
        let mut buf = Vec::new();
        encode_varint_field(1, 730, &mut buf);
        assert_eq!(extract_app_id("Cloud.EnumerateUserFiles#1", &buf), 730);

        // ClientCommitFileUpload uses field 2
        let mut buf2 = Vec::new();
        encode_varint_field(1, 1, &mut buf2); // transfer_succeeded = true
        encode_varint_field(2, 480, &mut buf2);
        assert_eq!(
            extract_app_id("Cloud.ClientCommitFileUpload#1", &buf2),
            480
        );

        // Delete#1 supports CCloud_Delete_Request (filename=1, appid=2)
        let mut buf3 = Vec::new();
        encode_bytes_field(1, b"save/slot0.sav", &mut buf3);
        encode_varint_field(2, 1245620, &mut buf3);
        assert_eq!(extract_app_id("Cloud.Delete#1", &buf3), 1245620);
    }

    #[test]
    fn test_extract_cloud_job_name() {
        let sample = b"\x08\x01\x62\x19Cloud.GetUGCFileDetails#1\x68\x01";
        assert_eq!(
            extract_cloud_job_name(sample),
            Some("Cloud.GetUGCFileDetails#1".to_string())
        );
    }

    #[test]
    fn test_build_response_header_uses_real_cmsg_protobuf_header_tags() {
        let _g = CLOUD_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        *local_session().lock().unwrap() = (76561198044455566, 321);

        let job_id = 0x1234_5678_9ABC_DEF0u64;
        let hdr = build_response_header(job_id, "Cloud.EnumerateUserFiles#1", 1);

        // Verify exact Steam CMsgProtoBufHeader wire tags:
        // 0x09 (1 fixed64 steamid), 0x10 (2 int32 sessionid), 0x51 (10 fixed64 jobid_source),
        // 0x59 (11 fixed64 jobid_target), 0x62 (12 string target_job_name), 0x68 (13 int32 eresult)
        assert_eq!(hdr[0], 0x09);
        assert!(hdr.contains(&0x51));
        assert!(hdr.contains(&0x59));
        assert!(hdr.contains(&0x62));
        assert!(hdr.contains(&0x68));

        let decoded = decode_cmsg_protobuf_header(&hdr).unwrap();
        assert_eq!(decoded.steam_id, 76561198044455566);
        assert_eq!(decoded.client_session_id, 321);
        assert_eq!(decoded.job_id_source, u64::MAX);
        assert_eq!(decoded.job_id_target, job_id);
        assert_eq!(
            decoded.target_job_name.as_deref(),
            Some("Cloud.EnumerateUserFiles#1")
        );
        assert_eq!(decoded.eresult, Some(1));
    }

    #[test]
    fn test_local_cloud_rpc_upload_enumerate_download_delete_and_drain() {
        let _g = CLOUD_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        pending_responses().lock().unwrap().clear();

        let tmp = tempfile::tempdir().unwrap();
        set_cloud_data_root_override(Some(tmp.path().to_path_buf()));

        let account_id = 98765432u32;
        let steam_id = 76561197960265728u64 + u64::from(account_id);
        let app_id = 1245620u32;

        // Write cloud_state.json enabling redirection for app 1245620
        let state_json = serde_json::json!({
            "enabled": true,
            "account_id": account_id,
            "apps": [app_id],
        });
        fs::write(
            tmp.path().join("cloud_state.json"),
            serde_json::to_string_pretty(&state_json).unwrap(),
        )
        .unwrap();

        // 1. Send Cloud.ClientFileUpload#1 with inline save payload & Windows-style path
        let save_bytes = b"ELDEN_RING_SAVE_SLOT_00_PAYLOAD";
        let mut upload_req = Vec::new();
        encode_varint_field(1, u64::from(app_id), &mut upload_req);
        encode_varint_field(2, save_bytes.len() as u64, &mut upload_req);
        encode_varint_field(3, save_bytes.len() as u64, &mut upload_req);
        encode_bytes_field(6, b"saves\\ER0000.sl2", &mut upload_req);
        encode_bytes_field(8, save_bytes, &mut upload_req);

        let upload_job_id = 0xCAFE_0000_0000_0001u64;
        let upload_frame = build_client_cloud_frame(
            steam_id,
            777,
            upload_job_id,
            "Cloud.ClientFileUpload#1",
            &upload_req,
        );
        let mut send_ctx =
            NetPacketContext::new(upload_frame, NetPacketDirection::Send, app_id).unwrap();
        handle_send_cloud_rpc(&mut send_ctx).unwrap();
        assert!(
            send_ctx.frame_buffer.is_empty(),
            "outbound Cloud RPC must be suppressed"
        );

        let expected_disk_file = tmp
            .path()
            .join("cloud_storage")
            .join(account_id.to_string())
            .join(app_id.to_string())
            .join("saves/ER0000.sl2");
        assert!(expected_disk_file.is_file());
        assert_eq!(fs::read(&expected_disk_file).unwrap(), save_bytes);

        // 2. Send Cloud.EnumerateUserFiles#1
        let mut enum_req = Vec::new();
        encode_varint_field(1, u64::from(app_id), &mut enum_req);
        let enum_job_id = 0xCAFE_0000_0000_0002u64;
        let enum_frame = build_client_cloud_frame(
            steam_id,
            777,
            enum_job_id,
            "Cloud.EnumerateUserFiles#1",
            &enum_req,
        );
        let mut enum_ctx =
            NetPacketContext::new(enum_frame, NetPacketDirection::Send, app_id).unwrap();
        handle_send_cloud_rpc(&mut enum_ctx).unwrap();

        // 3. Send Cloud.ClientFileDownload#1
        let mut dl_req = Vec::new();
        encode_varint_field(1, u64::from(app_id), &mut dl_req);
        encode_bytes_field(2, b"saves/ER0000.sl2", &mut dl_req);
        let dl_job_id = 0xCAFE_0000_0000_0003u64;
        let dl_frame = build_client_cloud_frame(
            steam_id,
            777,
            dl_job_id,
            "Cloud.ClientFileDownload#1",
            &dl_req,
        );
        let mut dl_ctx =
            NetPacketContext::new(dl_frame, NetPacketDirection::Send, app_id).unwrap();
        handle_send_cloud_rpc(&mut dl_ctx).unwrap();

        // 4. Send Cloud.Delete#1 (using Valve CCloud_Delete_Request field order: filename=1, appid=2)
        let mut del_req = Vec::new();
        encode_bytes_field(1, b"saves/ER0000.sl2", &mut del_req);
        encode_varint_field(2, u64::from(app_id), &mut del_req);
        let del_job_id = 0xCAFE_0000_0000_0004u64;
        let del_frame = build_client_cloud_frame(
            steam_id,
            777,
            del_job_id,
            "Cloud.Delete#1",
            &del_req,
        );
        let mut del_ctx =
            NetPacketContext::new(del_frame, NetPacketDirection::Send, app_id).unwrap();
        handle_send_cloud_rpc(&mut del_ctx).unwrap();
        assert!(!expected_disk_file.exists());

        // 5. Drain all 4 queued EMsg 147 responses via live carrier flow (`handle_recv_cloud_carrier`)
        let delivered = Arc::new(Mutex::new(Vec::<(DecodedCloudProtoHeader, Vec<u8>)>::new()));
        let delivered_clone = Arc::clone(&delivered);
        set_cloud_carrier_callback(Some(Arc::new(move |injected_ctx| {
            let (hdr_slice, body_slice) = split_proto_frame(injected_ctx);
            let hdr = decode_cmsg_protobuf_header(hdr_slice).unwrap();
            delivered_clone
                .lock()
                .unwrap()
                .push((hdr, body_slice.to_vec()));
            Ok(())
        })));

        let mut carrier_raw = vec![0u8; 24];
        carrier_raw[0..4].copy_from_slice(&(766u32 | KM_MSG_HDR_PROTO_FLAG).to_le_bytes());
        carrier_raw[4..8].copy_from_slice(&0u32.to_le_bytes());
        let orig_carrier = carrier_raw.clone();
        let mut carrier_ctx =
            NetPacketContext::new(carrier_raw, NetPacketDirection::Recv, app_id).unwrap();

        handle_recv_cloud_carrier(&mut carrier_ctx).unwrap();
        // Carrier packet must be restored intact after draining!
        assert_eq!(carrier_ctx.frame_buffer, orig_carrier);

        let results = delivered.lock().unwrap();
        assert_eq!(results.len(), 4);

        // Upload response
        assert_eq!(results[0].0.job_id_target, upload_job_id);
        assert_eq!(results[0].0.eresult, Some(1));

        // EnumerateUserFiles response: total_files (field 2) == 1, files (field 1) contains filename
        assert_eq!(results[1].0.job_id_target, enum_job_id);
        assert_eq!(find_varint_field(&results[1].1, 2), Some(1));
        let file_entry = find_bytes_field(&results[1].1, 1).unwrap();
        assert_eq!(
            find_string_field(file_entry, 3).as_deref(),
            Some("saves/ER0000.sl2")
        );
        assert_eq!(
            find_varint_field(file_entry, 5),
            Some(save_bytes.len() as u64)
        );

        // ClientFileDownload response: file_size == save_bytes.len(), inline data == save_bytes
        assert_eq!(results[2].0.job_id_target, dl_job_id);
        assert_eq!(
            find_varint_field(&results[2].1, 2),
            Some(save_bytes.len() as u64)
        );
        assert_eq!(find_bytes_field(&results[2].1, 4).unwrap().len(), 20); // 20-byte SHA-1
        assert_eq!(find_bytes_field(&results[2].1, 12).unwrap(), save_bytes);

        // Delete response
        assert_eq!(results[3].0.job_id_target, del_job_id);
        assert_eq!(results[3].0.eresult, Some(1));
        assert_eq!(find_varint_field(&results[3].1, 1), Some(1));

        // Cleanup test overrides
        set_cloud_carrier_callback(None);
        set_cloud_data_root_override(None);
    }

    #[test]
    fn test_begin_and_commit_file_upload_workflow() {
        let _g = CLOUD_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        set_cloud_data_root_override(Some(tmp.path().to_path_buf()));

        let mut begin_req = Vec::new();
        encode_varint_field(1, 480, &mut begin_req);
        encode_varint_field(2, 16, &mut begin_req);
        encode_varint_field(3, 16, &mut begin_req);
        encode_varint_field(5, 1_700_000_000, &mut begin_req);
        encode_bytes_field(6, b"profile/config.cfg", &mut begin_req);

        let (begin_resp, begin_er) =
            handle_local_cloud_rpc("Cloud.BeginFileUpload#1", 480, 555, &begin_req).unwrap();
        assert_eq!(begin_er, ERESULT_OK);
        assert_eq!(find_varint_field(&begin_resp, 1), Some(0)); // encrypt_file = false

        let mut commit_req = Vec::new();
        encode_varint_field(1, 1, &mut commit_req); // transfer_succeeded = true
        encode_varint_field(2, 480, &mut commit_req);
        encode_bytes_field(4, b"profile/config.cfg", &mut commit_req);
        encode_bytes_field(5, b"bind SPACE +jump", &mut commit_req);

        let (commit_resp, commit_er) =
            handle_local_cloud_rpc("Cloud.ClientCommitFileUpload#1", 480, 555, &commit_req)
                .unwrap();
        assert_eq!(commit_er, ERESULT_OK);
        assert_eq!(find_varint_field(&commit_resp, 1), Some(1)); // file_committed = true

        let stored = tmp
            .path()
            .join("cloud_storage/555/480/profile/config.cfg");
        assert_eq!(fs::read(&stored).unwrap(), b"bind SPACE +jump");

        set_cloud_data_root_override(None);
    }

    #[test]
    fn test_path_traversal_rejected_in_local_cloud_rpc() {
        let _g = CLOUD_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        set_cloud_data_root_override(Some(tmp.path().to_path_buf()));

        let mut traversal_dl = Vec::new();
        encode_varint_field(1, 730, &mut traversal_dl);
        encode_bytes_field(2, b"../../etc/passwd", &mut traversal_dl);
        let res = handle_local_cloud_rpc("Cloud.ClientFileDownload#1", 730, 123, &traversal_dl);
        assert_eq!(res, Some((Vec::new(), ERESULT_INVALID_PARAM)));

        set_cloud_data_root_override(None);
    }
}
