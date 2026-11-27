//! CloudRedirect host implementation for Linux.
//!
//! Port of usefulsteamtools `CloudRedirectHost.cpp`.
//! Provides local filesystem Steam Cloud save and achievement redirection by intercepting
//! `Cloud.*` Unified Service RPCs (`Cloud.EnumerateUserFiles#1`, `Cloud.ClientFileDownload#1`,
//! `Cloud.ClientFileUpload#1`, `Cloud.BeginFileUpload#1`, `Cloud.ClientCommitFileUpload#1`,
//! `Cloud.Delete#1`) backed by `~/.local/share/luatools/cloud_storage/<account_id>/<app_id>/`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use sha1::{Digest, Sha1};

const ERESULT_OK: i32 = 1;
const ERESULT_INVALID_PARAM: i32 = 8;
const ERESULT_FILE_NOT_FOUND: i32 = 9;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AchievementBlock {
    pub name: String,
    pub state: u8,
    pub unlock_time: u32,
}

#[derive(Debug, Clone, Default)]
struct StagedCloudUpload {
    rel_path: String,
    expected_size: u32,
    timestamp: u64,
}

/// CloudRedirect service state.
#[derive(Debug, Clone, Default)]
pub struct CloudRedirectState {
    pub enabled: bool,
    pub steam_root: Option<PathBuf>,
    pub storage_root: Option<PathBuf>,
    pub registered_apps: Vec<u32>,
    pub account_id: Option<u32>,
    pub stats_sync_enabled: bool,
    running_apps: BTreeSet<u32>,
    achievements: BTreeMap<u32, Vec<AchievementBlock>>,
    staged_uploads: BTreeMap<(u32, u32, String), StagedCloudUpload>,
}

// ============================================================================
// Protobuf Wire Helpers for Steam Cloud RPC Messages (BST-59)
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

fn encode_varint_field(field_num: u32, val: u64, out: &mut Vec<u8>) {
    encode_varint((u64::from(field_num) << 3) | 0, out);
    encode_varint(val, out);
}

fn encode_bytes_field(field_num: u32, bytes: &[u8], out: &mut Vec<u8>) {
    encode_varint((u64::from(field_num) << 3) | 2, out);
    encode_varint(bytes.len() as u64, out);
    out.extend_from_slice(bytes);
}

fn skip_wire_value(data: &[u8], wire_type: u32) -> Option<usize> {
    match wire_type {
        0 => read_varint(data).map(|(_, n)| n),
        1 => (data.len() >= 8).then_some(8),
        2 => {
            let (len, n) = read_varint(data)?;
            let len_usize = usize::try_from(len).ok()?;
            let total = n.checked_add(len_usize)?;
            (total <= data.len()).then_some(total)
        }
        5 => (data.len() >= 4).then_some(4),
        _ => None,
    }
}

fn find_varint_field(data: &[u8], target_field: u32) -> Option<u64> {
    let mut pos = 0;
    while pos < data.len() {
        if data[pos] == 0 {
            break;
        }
        let (tag, consumed) = read_varint(&data[pos..])?;
        pos += consumed;
        let field = (tag >> 3) as u32;
        let wire_type = (tag & 0x07) as u32;
        if field == target_field && wire_type == 0 {
            return read_varint(&data[pos..]).map(|(v, _)| v);
        }
        pos += skip_wire_value(&data[pos..], wire_type)?;
    }
    None
}

fn find_bytes_field(data: &[u8], target_field: u32) -> Option<&[u8]> {
    let mut pos = 0;
    while pos < data.len() {
        if data[pos] == 0 {
            break;
        }
        let (tag, consumed) = read_varint(&data[pos..])?;
        pos += consumed;
        let field = (tag >> 3) as u32;
        let wire_type = (tag & 0x07) as u32;
        if field == target_field && wire_type == 2 {
            let (len, n) = read_varint(&data[pos..])?;
            pos += n;
            let len_usize = usize::try_from(len).ok()?;
            let end = pos.checked_add(len_usize)?;
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

/// Default local cloud storage root: `~/.local/share/luatools/cloud_storage`
/// (honoring `LUATOOLS_CLOUD_DIR` when configured).
fn default_cloud_storage_root() -> PathBuf {
    if let Ok(env_dir) = std::env::var("LUATOOLS_CLOUD_DIR") {
        let trimmed = env_dir.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join("cloud_storage");
        }
    }
    let home = dirs::home_dir()
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".local/share/luatools/cloud_storage")
}

/// Strictly validate and normalize a relative Steam Cloud filename.
///
/// Rejects empty strings, NUL/control bytes, leading `/` or `\`, Windows drive prefixes,
/// and any `..` or `.` path traversal components.
fn sanitize_cloud_rel_path(raw: &str) -> Option<(String, PathBuf)> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.starts_with('\\')
        || trimmed.bytes().any(|b| b == 0 || b < 0x20)
    {
        return None;
    }

    let normalized = trimmed.replace('\\', "/");
    let mut parts = Vec::new();
    let mut clean_rel = PathBuf::new();

    for raw_seg in normalized.split('/') {
        if raw_seg.is_empty() {
            continue;
        }
        if raw_seg == "." || raw_seg == ".." || raw_seg.contains(':') {
            return None;
        }
        parts.push(raw_seg.to_string());
        clean_rel.push(raw_seg);
    }

    if parts.is_empty() {
        return None;
    }

    // Double-check via std::path::Path components
    for comp in clean_rel.components() {
        match comp {
            Component::Normal(_) => {}
            _ => return None,
        }
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

pub struct CloudRedirectService {
    state: Arc<RwLock<CloudRedirectState>>,
}

impl CloudRedirectService {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(CloudRedirectState::default())),
        }
    }

    /// Initialize the cloud redirect service.
    pub fn initialize(&self, steam_root: &Path, enabled: bool) -> bool {
        if steam_root.as_os_str().is_empty() {
            return false;
        }
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.enabled = enabled;
        state.steam_root = Some(steam_root.to_path_buf());
        true
    }

    /// Override the base cloud storage directory (`<storage_root>/<account_id>/<app_id>/`).
    /// Pass `None` to restore the default `~/.local/share/luatools/cloud_storage`.
    pub fn set_storage_root(&self, root: Option<PathBuf>) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.storage_root = root;
    }

    /// Resolve the active cloud storage root directory.
    pub fn storage_root(&self) -> PathBuf {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        state
            .storage_root
            .clone()
            .unwrap_or_else(default_cloud_storage_root)
    }

    /// Register apps for cloud redirection (deduplicated, ignoring AppID 0).
    pub fn register_apps(&self, app_ids: Vec<u32>) {
        let mut unique: Vec<u32> = app_ids.into_iter().filter(|&id| id != 0).collect();
        unique.sort_unstable();
        unique.dedup();
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.registered_apps = unique;
    }

    /// Add a single app to the redirected app set.
    pub fn add_app(&self, app_id: u32) {
        if app_id == 0 {
            return;
        }
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        if let Err(pos) = state.registered_apps.binary_search(&app_id) {
            state.registered_apps.insert(pos, app_id);
        }
    }

    /// Remove a single app from the redirected app set.
    pub fn remove_app(&self, app_id: u32) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.registered_apps.retain(|&id| id != app_id);
        state.running_apps.remove(&app_id);
    }

    /// Check if an app is actively registered for cloud redirect while the service is enabled.
    pub fn is_app_redirected(&self, app_id: u32) -> bool {
        if app_id == 0 {
            return false;
        }
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        state.enabled && state.registered_apps.contains(&app_id)
    }

    /// Set account ID for cloud operations.
    pub fn set_account_id(&self, account_id: u32) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.account_id = (account_id != 0).then_some(account_id);
    }

    /// Handle a Steam Cloud RPC call against local filesystem storage
    /// (`<storage_root>/<account_id>/<app_id>/`).
    ///
    /// Returns `None` when disabled or when `app_id` is not registered for redirection so
    /// Steam processes unregistered apps natively without interception.
    pub fn handle_cloud_rpc(
        &self,
        method: &str,
        app_id: u32,
        account_id: u32,
        req_body: &[u8],
    ) -> Option<(Vec<u8>, i32)> {
        if method.trim().is_empty() || app_id == 0 {
            return None;
        }

        let (effective_account, storage_root) = {
            let state = self.state.read().unwrap_or_else(|e| e.into_inner());
            if !state.enabled || !state.registered_apps.contains(&app_id) {
                return None;
            }
            let acc = if account_id != 0 {
                account_id
            } else {
                state.account_id.unwrap_or(0)
            };
            let root = state
                .storage_root
                .clone()
                .unwrap_or_else(default_cloud_storage_root);
            (acc, root)
        };

        log::debug!(
            "[cloud_redirect] RPC method={} app={} account={} req_bytes={}",
            method,
            app_id,
            effective_account,
            req_body.len()
        );

        let app_dir = storage_root
            .join(effective_account.to_string())
            .join(app_id.to_string());

        match method {
            "Cloud.EnumerateUserFiles#1" => {
                let max_count = find_varint_field(req_body, 3).unwrap_or(0) as usize;
                let start_idx = find_varint_field(req_body, 4).unwrap_or(0) as usize;

                let all_files = collect_app_files(&app_dir);
                let total_files = all_files.len() as u32;
                let iter = all_files.into_iter().skip(start_idx);
                let selected: Vec<_> = if max_count > 0 {
                    iter.take(max_count).collect()
                } else {
                    iter.collect()
                };

                let mut resp = Vec::new();
                for (idx, (rel_name, full_path, size, mtime)) in selected.into_iter().enumerate() {
                    let bytes = fs::read(&full_path).unwrap_or_default();
                    let mut hasher = Sha1::new();
                    hasher.update(&bytes);
                    let sha_hex = hex::encode(hasher.finalize());

                    // CCloud_UserFile
                    let mut file_msg = Vec::with_capacity(96 + rel_name.len());
                    encode_varint_field(1, u64::from(app_id), &mut file_msg);
                    encode_varint_field(2, (start_idx + idx + 1) as u64, &mut file_msg);
                    encode_bytes_field(3, rel_name.as_bytes(), &mut file_msg);
                    encode_varint_field(4, mtime, &mut file_msg);
                    encode_varint_field(5, size.min(u64::from(u32::MAX)), &mut file_msg);
                    let file_url = format!("file://{}", full_path.display());
                    encode_bytes_field(6, file_url.as_bytes(), &mut file_msg);
                    encode_bytes_field(10, sha_hex.as_bytes(), &mut file_msg);
                    encode_varint_field(11, size.min(u64::from(u32::MAX)), &mut file_msg);

                    encode_bytes_field(1, &file_msg, &mut resp);
                }
                encode_varint_field(2, u64::from(total_files), &mut resp);
                Some((resp, ERESULT_OK))
            }

            "Cloud.ClientFileDownload#1" => {
                let Some(raw_filename) = find_string_field(req_body, 2) else {
                    return Some((Vec::new(), ERESULT_INVALID_PARAM));
                };
                let Some((clean_key, clean_rel)) = sanitize_cloud_rel_path(&raw_filename) else {
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

                // CCloud_ClientFileDownload_Response
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
                encode_bytes_field(12, &data, &mut resp); // inline payload
                Some((resp, ERESULT_OK))
            }

            "Cloud.ClientFileUpload#1"
            | "Cloud.BeginFileUpload#1"
            | "Cloud.ClientBeginFileUpload#1" => {
                let filename_field6 = find_string_field(req_body, 6);
                let filename_field2 = if filename_field6.is_none() {
                    find_string_field(req_body, 2)
                } else {
                    None
                };
                let Some(raw_filename) = filename_field6.as_deref().or(filename_field2.as_deref())
                else {
                    return Some((Vec::new(), ERESULT_INVALID_PARAM));
                };
                let Some((clean_key, clean_rel)) = sanitize_cloud_rel_path(raw_filename) else {
                    return Some((Vec::new(), ERESULT_INVALID_PARAM));
                };

                let inline_data = find_bytes_field(req_body, 8)
                    .or_else(|| find_bytes_field(req_body, 7))
                    .or_else(|| {
                        if filename_field2.is_some() {
                            find_bytes_field(req_body, 3).or_else(|| find_bytes_field(req_body, 4))
                        } else {
                            None
                        }
                    });

                let expected_size = find_varint_field(req_body, 2)
                    .or_else(|| find_varint_field(req_body, 3))
                    .unwrap_or_else(|| inline_data.map_or(0, |b| b.len() as u64))
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
                    let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
                    state.staged_uploads.insert(
                        (effective_account, app_id, clean_key.clone()),
                        StagedCloudUpload {
                            rel_path: clean_key,
                            expected_size,
                            timestamp,
                        },
                    );
                }

                // CCloud_ClientBeginFileUpload_Response: encrypt_file = 1 (false)
                let mut resp = Vec::with_capacity(4);
                encode_varint_field(1, 0, &mut resp);
                Some((resp, ERESULT_OK))
            }

            "Cloud.ClientCommitFileUpload#1" | "Cloud.CommitFileUpload#1" => {
                let transfer_ok = find_varint_field(req_body, 1).unwrap_or(1) != 0;
                let Some(raw_filename) =
                    find_string_field(req_body, 4).or_else(|| find_string_field(req_body, 2))
                else {
                    return Some((Vec::new(), ERESULT_INVALID_PARAM));
                };
                let Some((clean_key, clean_rel)) = sanitize_cloud_rel_path(&raw_filename) else {
                    return Some((Vec::new(), ERESULT_INVALID_PARAM));
                };

                let staged = {
                    let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
                    state
                        .staged_uploads
                        .remove(&(effective_account, app_id, clean_key))
                };

                if !transfer_ok {
                    let mut resp = Vec::with_capacity(2);
                    encode_varint_field(1, 0, &mut resp);
                    return Some((resp, ERESULT_OK));
                }

                let dest_path = app_dir.join(&clean_rel);
                if let Some(inline_data) = find_bytes_field(req_body, 5) {
                    if let Some(parent) = dest_path.parent() {
                        if fs::create_dir_all(parent).is_err() {
                            return Some((Vec::new(), ERESULT_INVALID_PARAM));
                        }
                    }
                    let tmp_path = dest_path.with_extension("cloud.tmp");
                    if fs::write(&tmp_path, inline_data).is_err()
                        || fs::rename(&tmp_path, &dest_path).is_err()
                    {
                        let _ = fs::remove_file(&tmp_path);
                        return Some((Vec::new(), ERESULT_INVALID_PARAM));
                    }
                } else if let Some(st) = staged {
                    log::debug!(
                        "[cloud_redirect] commit finalized file={} expected_size={} ts={}",
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
                encode_varint_field(1, u64::from(committed), &mut resp);
                Some((resp, ERESULT_OK))
            }

            "Cloud.Delete#1" | "Cloud.ClientDeleteFile#1" => {
                let Some(raw_filename) =
                    find_string_field(req_body, 1).or_else(|| find_string_field(req_body, 2))
                else {
                    return Some((Vec::new(), ERESULT_INVALID_PARAM));
                };
                let Some((clean_key, clean_rel)) = sanitize_cloud_rel_path(&raw_filename) else {
                    return Some((Vec::new(), ERESULT_INVALID_PARAM));
                };

                {
                    let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
                    state
                        .staged_uploads
                        .remove(&(effective_account, app_id, clean_key));
                }

                let file_path = app_dir.join(&clean_rel);
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
                encode_varint_field(1, 1, &mut resp);
                Some((resp, ERESULT_OK))
            }

            "Cloud.GetUGCFileDetails#1" => {
                let hcontent = find_varint_field(req_body, 1).unwrap_or(0);
                let req_app_id = find_varint_field(req_body, 2).map(|v| v as u32).unwrap_or(app_id);
                let all_files = collect_app_files(&app_dir);
                let matched_file = if hcontent > 0 && (hcontent as usize) <= all_files.len() {
                    Some(&all_files[(hcontent - 1) as usize])
                } else {
                    all_files.first()
                };

                let mut resp = Vec::new();
                encode_varint_field(1, u64::from(req_app_id), &mut resp);
                if let Some((rel_name, full_path, size, mtime)) = matched_file {
                    let bytes = fs::read(full_path).unwrap_or_default();
                    let mut hasher = Sha1::new();
                    hasher.update(&bytes);
                    let sha_raw = hasher.finalize();

                    let file_url = format!("file://{}", full_path.display());
                    encode_bytes_field(2, file_url.as_bytes(), &mut resp);
                    encode_bytes_field(3, rel_name.as_bytes(), &mut resp);
                    encode_varint_field(4, (*size).min(u32::MAX as u64), &mut resp);
                    encode_bytes_field(5, &sha_raw, &mut resp);
                    encode_varint_field(6, *mtime, &mut resp);
                } else {
                    encode_bytes_field(2, b"file:///dev/null", &mut resp);
                    encode_bytes_field(3, b"", &mut resp);
                    encode_varint_field(4, 0, &mut resp);
                    encode_varint_field(6, 0, &mut resp);
                }
                Some((resp, ERESULT_OK))
            }

            "EMsgClientFSGetFileParams" | "147" | "EMsgClientFSGetFileParamsResponse" => {
                let mut resp = Vec::with_capacity(8);
                resp.extend_from_slice(&1u32.to_le_bytes());
                resp.extend_from_slice(&0u32.to_le_bytes());
                Some((resp, ERESULT_OK))
            }

            _ => Some((Vec::new(), ERESULT_OK)),
        }
    }

    /// Notify that an app is running or stopped.
    pub fn notify_app_running(&self, app_id: u32, running: bool) {
        if app_id == 0 {
            return;
        }
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        if !state.enabled || !state.registered_apps.contains(&app_id) {
            return;
        }
        if running {
            state.running_apps.insert(app_id);
        } else {
            state.running_apps.remove(&app_id);
        }
        log::debug!(
            "[cloud_redirect] App {} is {}",
            app_id,
            if running { "running" } else { "stopped" }
        );
    }

    /// Check whether a redirected app is currently marked running.
    pub fn is_app_running(&self, app_id: u32) -> bool {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        state.enabled && state.running_apps.contains(&app_id)
    }

    /// Notify that stats have been stored.
    pub fn notify_stats_stored(&self, app_id: u32) {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        if state.enabled && state.registered_apps.contains(&app_id) {
            log::debug!("[cloud_redirect] Stats stored for app {}", app_id);
        }
    }

    /// Replace the cached achievement list for an app.
    pub fn set_achievements(&self, app_id: u32, blocks: Vec<AchievementBlock>) {
        if app_id == 0 {
            return;
        }
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.achievements.insert(app_id, blocks);
    }

    /// Upsert a single achievement state for an app.
    pub fn upsert_achievement(&self, app_id: u32, block: AchievementBlock) {
        if app_id == 0 || block.name.is_empty() {
            return;
        }
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        let entry = state.achievements.entry(app_id).or_default();
        if let Some(existing) = entry.iter_mut().find(|a| a.name == block.name) {
            *existing = block;
        } else {
            entry.push(block);
        }
    }

    /// Clear cached achievements for an app.
    pub fn clear_achievements(&self, app_id: u32) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.achievements.remove(&app_id);
    }

    /// Get achievements for an app.
    pub fn get_achievements(&self, app_id: u32, max_blocks: usize) -> Vec<AchievementBlock> {
        if app_id == 0 || max_blocks == 0 {
            return Vec::new();
        }
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        if !state.enabled {
            return Vec::new();
        }
        state
            .achievements
            .get(&app_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(max_blocks)
            .collect()
    }

    /// Enable stats sync.
    pub fn enable_stats_sync(&self, enable: bool) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.stats_sync_enabled = enable;
        log::debug!(
            "[cloud_redirect] stats sync {}",
            if enable { "enabled" } else { "disabled" }
        );
    }

    /// Shutdown the service.
    pub fn shutdown(&self) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.enabled = false;
        state.running_apps.clear();
        state.staged_uploads.clear();
        log::debug!("[cloud_redirect] shutdown");
    }

    /// Get the current state.
    pub fn state(&self) -> CloudRedirectState {
        self.state.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Default for CloudRedirectService {
    fn default() -> Self {
        Self::new()
    }
}

/// High-level CloudRedirect host that the GUI and `NetPacket_Cloud` can use.
pub struct CloudRedirectHost {
    service: CloudRedirectService,
}

impl CloudRedirectHost {
    pub fn new() -> Self {
        Self {
            service: CloudRedirectService::new(),
        }
    }

    pub fn initialize(&self, steam_root: &Path, enabled: bool) -> bool {
        self.service.initialize(steam_root, enabled)
    }

    pub fn set_storage_root(&self, root: Option<PathBuf>) {
        self.service.set_storage_root(root);
    }

    pub fn storage_root(&self) -> PathBuf {
        self.service.storage_root()
    }

    pub fn is_active(&self) -> bool {
        self.service.state().enabled
    }

    pub fn sync_app_set(&self, app_ids: Vec<u32>) {
        self.service.register_apps(app_ids);
    }

    pub fn is_app(&self, app_id: u32) -> bool {
        self.service.is_app_redirected(app_id)
    }

    pub fn handle_cloud_rpc(
        &self,
        method: &str,
        app_id: u32,
        account_id: u32,
        req_body: Vec<u8>,
    ) -> Option<(Vec<u8>, i32)> {
        self.service
            .handle_cloud_rpc(method, app_id, account_id, &req_body)
    }

    pub fn set_account_id(&self, account_id: u32) {
        self.service.set_account_id(account_id);
    }

    pub fn notify_app_running(&self, app_id: u32, running: bool) {
        self.service.notify_app_running(app_id, running);
    }

    pub fn notify_stats_stored(&self, app_id: u32) {
        self.service.notify_stats_stored(app_id);
    }

    pub fn set_achievements(&self, app_id: u32, blocks: Vec<AchievementBlock>) {
        self.service.set_achievements(app_id, blocks);
    }

    pub fn get_achievements(&self, app_id: u32, max_blocks: usize) -> Vec<AchievementBlock> {
        self.service.get_achievements(app_id, max_blocks)
    }

    pub fn enable_stats_sync(&self, enable: bool) {
        self.service.enable_stats_sync(enable);
    }

    pub fn shutdown(&self) {
        self.service.shutdown();
    }
}

impl Default for CloudRedirectHost {
    fn default() -> Self {
        Self::new()
    }
}

// Global instance for inter-module access
static CLOUD_REDIRECT_HOST: std::sync::OnceLock<CloudRedirectHost> = std::sync::OnceLock::new();

/// Get or create the global CloudRedirectHost instance
pub fn get_cloud_redirect_host() -> &'static CloudRedirectHost {
    CLOUD_REDIRECT_HOST.get_or_init(CloudRedirectHost::new)
}

pub fn is_cloud_redirect_active() -> bool {
    get_cloud_redirect_host().is_active()
}

pub fn set_account_id(account_id: u32) {
    get_cloud_redirect_host().set_account_id(account_id);
}

pub fn notify_app_running(app_id: u32, running: bool) {
    get_cloud_redirect_host().notify_app_running(app_id, running);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_cloud_rpc_only_intercepts_enabled_registered_apps() {
        let svc = CloudRedirectService::new();
        assert!(!svc.initialize(Path::new(""), true));
        assert!(svc.initialize(Path::new("/tmp/steam"), true));

        svc.register_apps(vec![1245620, 0, 1245620, 480]);
        assert_eq!(svc.state().registered_apps, vec![480, 1245620]);
        assert!(svc.is_app_redirected(1245620));
        assert!(!svc.is_app_redirected(999999));

        assert_eq!(
            svc.handle_cloud_rpc("Cloud.GetUploadServerInfo", 1245620, 42, b"req"),
            Some((Vec::new(), 1))
        );
        // Unregistered app must pass through to Steam (None)
        assert!(svc
            .handle_cloud_rpc("Cloud.GetUploadServerInfo", 999999, 42, b"req")
            .is_none());
        // Empty method must not be intercepted
        assert!(svc.handle_cloud_rpc("", 1245620, 42, b"req").is_none());

        svc.shutdown();
        assert!(!svc.is_app_redirected(1245620));
        assert!(svc
            .handle_cloud_rpc("Cloud.GetUploadServerInfo", 1245620, 42, b"req")
            .is_none());
    }

    #[test]
    fn achievements_upsert_limit_and_running_state() {
        let host = CloudRedirectHost::new();
        host.initialize(Path::new("/tmp/steam"), true);
        host.sync_app_set(vec![730]);
        host.set_account_id(12345);
        host.enable_stats_sync(true);

        host.set_achievements(
            730,
            vec![
                AchievementBlock {
                    name: "FIRST_BLOOD".into(),
                    state: 1,
                    unlock_time: 1_700_000_000,
                },
                AchievementBlock {
                    name: "SHARPSHOOTER".into(),
                    state: 0,
                    unlock_time: 0,
                },
            ],
        );
        assert_eq!(host.get_achievements(730, 1).len(), 1);
        assert_eq!(host.get_achievements(730, 10).len(), 2);
        assert!(host.get_achievements(730, 0).is_empty());

        host.notify_app_running(730, true);
        assert!(host.service.is_app_running(730));
        host.notify_app_running(730, false);
        assert!(!host.service.is_app_running(730));

        host.service.upsert_achievement(
            730,
            AchievementBlock {
                name: "SHARPSHOOTER".into(),
                state: 1,
                unlock_time: 1_700_000_123,
            },
        );
        let all = host.get_achievements(730, 10);
        assert_eq!(all[1].state, 1);
        assert_eq!(all[1].unlock_time, 1_700_000_123);

        host.shutdown();
        assert!(host.get_achievements(730, 10).is_empty());
    }

    #[test]
    fn cloud_rpc_upload_enumerate_download_delete_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let host = CloudRedirectHost::new();
        host.initialize(Path::new("/tmp/steam"), true);
        host.set_storage_root(Some(tmp.path().to_path_buf()));
        host.sync_app_set(vec![1245620]);
        host.set_account_id(998877);

        let payload = b"ELDEN_RING_LOCAL_CLOUD_SAVE_DATA";

        // 1. Upload via Cloud.ClientFileUpload#1
        let mut upload_req = Vec::new();
        encode_varint_field(1, 1245620, &mut upload_req);
        encode_varint_field(2, payload.len() as u64, &mut upload_req);
        encode_bytes_field(6, b"saves\\slot0.sl2", &mut upload_req);
        encode_bytes_field(8, payload, &mut upload_req);

        let (upload_resp, upload_er) = host
            .handle_cloud_rpc("Cloud.ClientFileUpload#1", 1245620, 0, upload_req)
            .unwrap();
        assert_eq!(upload_er, ERESULT_OK);
        assert_eq!(find_varint_field(&upload_resp, 1), Some(0)); // encrypt_file = false

        let disk_file = tmp.path().join("998877/1245620/saves/slot0.sl2");
        assert_eq!(fs::read(&disk_file).unwrap(), payload);

        // 2. Enumerate via Cloud.EnumerateUserFiles#1
        let mut enum_req = Vec::new();
        encode_varint_field(1, 1245620, &mut enum_req);
        let (enum_resp, enum_er) = host
            .handle_cloud_rpc("Cloud.EnumerateUserFiles#1", 1245620, 998877, enum_req)
            .unwrap();
        assert_eq!(enum_er, ERESULT_OK);
        assert_eq!(find_varint_field(&enum_resp, 2), Some(1));
        let file_entry = find_bytes_field(&enum_resp, 1).unwrap();
        assert_eq!(
            find_string_field(file_entry, 3).as_deref(),
            Some("saves/slot0.sl2")
        );
        assert_eq!(
            find_varint_field(file_entry, 5),
            Some(payload.len() as u64)
        );

        // 3. Download via Cloud.ClientFileDownload#1
        let mut dl_req = Vec::new();
        encode_varint_field(1, 1245620, &mut dl_req);
        encode_bytes_field(2, b"saves/slot0.sl2", &mut dl_req);
        let (dl_resp, dl_er) = host
            .handle_cloud_rpc("Cloud.ClientFileDownload#1", 1245620, 998877, dl_req)
            .unwrap();
        assert_eq!(dl_er, ERESULT_OK);
        assert_eq!(find_varint_field(&dl_resp, 2), Some(payload.len() as u64));
        assert_eq!(find_bytes_field(&dl_resp, 4).unwrap().len(), 20);
        assert_eq!(find_bytes_field(&dl_resp, 12).unwrap(), payload);

        // 4. Delete via Cloud.Delete#1
        let mut del_req = Vec::new();
        encode_bytes_field(1, b"saves/slot0.sl2", &mut del_req);
        encode_varint_field(2, 1245620, &mut del_req);
        let (del_resp, del_er) = host
            .handle_cloud_rpc("Cloud.Delete#1", 1245620, 998877, del_req)
            .unwrap();
        assert_eq!(del_er, ERESULT_OK);
        assert_eq!(find_varint_field(&del_resp, 1), Some(1));
        assert!(!disk_file.exists());

        // 5. Missing file returns ERESULT_FILE_NOT_FOUND (9)
        let mut dl_missing = Vec::new();
        encode_varint_field(1, 1245620, &mut dl_missing);
        encode_bytes_field(2, b"saves/slot0.sl2", &mut dl_missing);
        assert_eq!(
            host.handle_cloud_rpc("Cloud.ClientFileDownload#1", 1245620, 998877, dl_missing),
            Some((Vec::new(), ERESULT_FILE_NOT_FOUND))
        );
    }

    #[test]
    fn cloud_rpc_begin_and_commit_file_upload() {
        let tmp = tempfile::tempdir().unwrap();
        let host = CloudRedirectHost::new();
        host.initialize(Path::new("/tmp/steam"), true);
        host.set_storage_root(Some(tmp.path().to_path_buf()));
        host.sync_app_set(vec![480]);

        let mut begin_req = Vec::new();
        encode_varint_field(1, 480, &mut begin_req);
        encode_varint_field(2, 11, &mut begin_req);
        encode_bytes_field(6, b"cfg/user.ini", &mut begin_req);
        let (begin_resp, begin_er) = host
            .handle_cloud_rpc("Cloud.BeginFileUpload#1", 480, 555, begin_req)
            .unwrap();
        assert_eq!(begin_er, ERESULT_OK);
        assert_eq!(find_varint_field(&begin_resp, 1), Some(0));

        let mut commit_req = Vec::new();
        encode_varint_field(1, 1, &mut commit_req);
        encode_varint_field(2, 480, &mut commit_req);
        encode_bytes_field(4, b"cfg/user.ini", &mut commit_req);
        encode_bytes_field(5, b"volume=0.75", &mut commit_req);
        let (commit_resp, commit_er) = host
            .handle_cloud_rpc("Cloud.ClientCommitFileUpload#1", 480, 555, commit_req)
            .unwrap();
        assert_eq!(commit_er, ERESULT_OK);
        assert_eq!(find_varint_field(&commit_resp, 1), Some(1));

        assert_eq!(
            fs::read(tmp.path().join("555/480/cfg/user.ini")).unwrap(),
            b"volume=0.75"
        );
    }

    #[test]
    fn cloud_rpc_rejects_path_traversal_and_leading_slashes() {
        let tmp = tempfile::tempdir().unwrap();
        let host = CloudRedirectHost::new();
        host.initialize(Path::new("/tmp/steam"), true);
        host.set_storage_root(Some(tmp.path().to_path_buf()));
        host.sync_app_set(vec![730]);

        let malicious_paths: &[&[u8]] = &[
            b"../escape.sav",
            b"saves/../../etc/passwd",
            b"/etc/passwd",
            b"\\Windows\\win.ini",
            b"C:\\secret.bin",
            b"bad\0name.sav",
        ];

        for &bad in malicious_paths {
            let mut up_req = Vec::new();
            encode_varint_field(1, 730, &mut up_req);
            encode_bytes_field(6, bad, &mut up_req);
            encode_bytes_field(8, b"pwned", &mut up_req);
            assert_eq!(
                host.handle_cloud_rpc("Cloud.ClientFileUpload#1", 730, 123, up_req),
                Some((Vec::new(), ERESULT_INVALID_PARAM)),
                "upload must reject {:?}",
                String::from_utf8_lossy(bad)
            );

            let mut dl_req = Vec::new();
            encode_varint_field(1, 730, &mut dl_req);
            encode_bytes_field(2, bad, &mut dl_req);
            assert_eq!(
                host.handle_cloud_rpc("Cloud.ClientFileDownload#1", 730, 123, dl_req),
                Some((Vec::new(), ERESULT_INVALID_PARAM)),
                "download must reject {:?}",
                String::from_utf8_lossy(bad)
            );

            let mut del_req = Vec::new();
            encode_bytes_field(1, bad, &mut del_req);
            encode_varint_field(2, 730, &mut del_req);
            assert_eq!(
                host.handle_cloud_rpc("Cloud.Delete#1", 730, 123, del_req),
                Some((Vec::new(), ERESULT_INVALID_PARAM)),
                "delete must reject {:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }
}
