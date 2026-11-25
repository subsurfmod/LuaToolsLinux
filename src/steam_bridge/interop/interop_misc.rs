//! Interop Misc hooks for Linux.
//!
//! Port of usefulsteamtools `Interop_Misc.cpp`.
//! Handles SpawnProcess interception, AppID spoofing, OptedInMask, and BuildSpawnEnvBlock.

use std::collections::HashMap;
use std::os::raw::c_void;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::steam_bridge::manifest_hooks::parse_pattern_and_mask;
use crate::steam_bridge::memory_patch::{self, MemoryPatch, JMP_ABS64_LEN};
use crate::steam_bridge::pattern_db::{BinaryComponent, PatternChannel, PatternDb};

/// Online fix app ID (Spacewar).
pub const ONLINE_FIX_APP_ID: u32 = 480;

// Lock-free atomic state for hot-path IPC/hook reads
static ONLINE_FIX_REAL_APP_ID: AtomicU32 = AtomicU32::new(0);
static NETWORKING_SOCKETS_ACTIVE: AtomicBool = AtomicBool::new(false);
static SUPPRESS_APP_ID_FLIP: AtomicBool = AtomicBool::new(false);
static STEAM_ENGINE_PTR: AtomicUsize = AtomicUsize::new(0);
static APP_INFO_CACHE_PTR: AtomicUsize = AtomicUsize::new(0);

static GAME_NAME_CACHE: OnceLock<Mutex<HashMap<u32, String>>> = OnceLock::new();
static INSTALLED_PATCHES: OnceLock<Mutex<Vec<MemoryPatch>>> = OnceLock::new();

// Function pointer storage
static ORIGINAL_OPTED_IN_MASK: OnceLock<Mutex<Option<OptedInMaskFn>>> = OnceLock::new();
static ORIGINAL_BUILD_SPAWN_ENV_BLOCK: OnceLock<Mutex<Option<BuildSpawnEnvBlockFn>>> =
    OnceLock::new();
static ORIGINAL_GET_OR_ADD_APP_DATA: OnceLock<Mutex<Option<GetOrAddAppDataFn>>> = OnceLock::new();
static ORIGINAL_CUTL_BUFFER_ENSURE_CAPACITY: OnceLock<Mutex<Option<CUtlBufferEnsureCapacityFn>>> =
    OnceLock::new();
static ORIGINAL_GET_APP_ID_FOR_CURRENT_PIPE: OnceLock<Mutex<Option<GetAppIdForCurrentPipeFn>>> =
    OnceLock::new();

type OptedInMaskFn = unsafe extern "C" fn(*mut c_void, u32) -> i64;
type BuildSpawnEnvBlockFn = unsafe extern "C" fn(
    *mut c_void,
    *mut c_void,
    *mut c_void,
    *mut c_void,
    *mut c_void,
    *mut c_void,
    i32,
    *mut c_void,
    *mut c_void,
    u32,
    u8,
) -> i64;
type GetOrAddAppDataFn = unsafe extern "C" fn(*mut c_void, u32, bool) -> *mut c_void;
type CUtlBufferEnsureCapacityFn = unsafe extern "C" fn(*mut c_void, u32) -> *mut c_void;
type GetAppIdForCurrentPipeFn = unsafe extern "C" fn(*mut c_void) -> u32;

fn game_name_cache() -> &'static Mutex<HashMap<u32, String>> {
    GAME_NAME_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn installed_patches() -> &'static Mutex<Vec<MemoryPatch>> {
    INSTALLED_PATCHES.get_or_init(|| Mutex::new(Vec::new()))
}

fn original_opted_in_mask() -> &'static Mutex<Option<OptedInMaskFn>> {
    ORIGINAL_OPTED_IN_MASK.get_or_init(|| Mutex::new(None))
}

fn original_build_spawn_env_block() -> &'static Mutex<Option<BuildSpawnEnvBlockFn>> {
    ORIGINAL_BUILD_SPAWN_ENV_BLOCK.get_or_init(|| Mutex::new(None))
}

fn original_get_or_add_app_data() -> &'static Mutex<Option<GetOrAddAppDataFn>> {
    ORIGINAL_GET_OR_ADD_APP_DATA.get_or_init(|| Mutex::new(None))
}

fn original_cutl_buffer_ensure_capacity() -> &'static Mutex<Option<CUtlBufferEnsureCapacityFn>> {
    ORIGINAL_CUTL_BUFFER_ENSURE_CAPACITY.get_or_init(|| Mutex::new(None))
}

fn original_get_app_id_for_current_pipe() -> &'static Mutex<Option<GetAppIdForCurrentPipeFn>> {
    ORIGINAL_GET_APP_ID_FOR_CURRENT_PIPE.get_or_init(|| Mutex::new(None))
}

unsafe extern "C" fn opted_in_mask_replacement(this: *mut c_void, app_id: u32) -> i64 {
    panic::catch_unwind(AssertUnwindSafe(|| {
        let real = ONLINE_FIX_REAL_APP_ID.load(Ordering::Acquire);
        let target_app = if app_id == ONLINE_FIX_APP_ID && real != 0 {
            log::info!("OptedInMask: appid {} -> {}", app_id, real);
            real
        } else {
            app_id
        };

        let orig = *original_opted_in_mask()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(orig_fn) = orig {
            return unsafe { orig_fn(this, target_app) };
        }
        0
    }))
    .unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn build_spawn_env_block_replacement(
    this: *mut c_void,
    p_cgame_id: *mut c_void,
    a3: *mut c_void,
    env: *mut c_void,
    p_overlay_cgame_id: *mut c_void,
    a6: *mut c_void,
    a7: i32,
    a8: *mut c_void,
    a9: *mut c_void,
    a10: u32,
    a11: u8,
) -> i64 {
    panic::catch_unwind(AssertUnwindSafe(|| {
        let real = ONLINE_FIX_REAL_APP_ID.load(Ordering::Acquire);
        if real != 0 && !p_overlay_cgame_id.is_null() {
            log::info!("BuildSpawnEnvBlock: SetAppID in OverlayCGameID -> {}", real);
        }
        let orig = *original_build_spawn_env_block()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(orig_fn) = orig {
            return unsafe {
                orig_fn(
                    this,
                    p_cgame_id,
                    a3,
                    env,
                    p_overlay_cgame_id,
                    a6,
                    a7,
                    a8,
                    a9,
                    a10,
                    a11,
                )
            };
        }
        0
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn get_or_add_app_data_replacement(
    cache: *mut c_void,
    app_id: u32,
    create: bool,
) -> *mut c_void {
    panic::catch_unwind(AssertUnwindSafe(|| {
        let orig = *original_get_or_add_app_data()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(orig_fn) = orig {
            return unsafe { orig_fn(cache, app_id, create) };
        }
        std::ptr::null_mut()
    }))
    .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn cutl_buffer_ensure_capacity_replacement(
    buffer: *mut c_void,
    new_capacity: u32,
) -> *mut c_void {
    panic::catch_unwind(AssertUnwindSafe(|| {
        log::debug!(
            "CUtlBufferEnsureCapacity: buffer={:p}, new_capacity={}",
            buffer,
            new_capacity
        );
        let orig = *original_cutl_buffer_ensure_capacity()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(orig_fn) = orig {
            return unsafe { orig_fn(buffer, new_capacity) };
        }
        buffer
    }))
    .unwrap_or(buffer)
}

fn resolve_function_address(
    steam_root: &Path,
    component: BinaryComponent,
    channel: PatternChannel,
    pattern_name: &str,
) -> Result<u64, String> {
    let binaries = crate::steam_bridge::binary_reader::scan_steam_binaries(steam_root);
    let binary = binaries
        .into_iter()
        .find(|b| {
            b.path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| match component {
                    BinaryComponent::SteamClient => n.contains("steamclient"),
                    BinaryComponent::SteamUi => n.contains("steamui"),
                })
                .unwrap_or(false)
        })
        .ok_or_else(|| "Steam binary not found".to_string())?;

    let pattern_db = PatternDb::new();
    let pf = pattern_db
        .lookup_by_binary(&binary, channel, component)
        .map_err(|e| format!("Pattern lookup failed: {}", e))?;

    let entry = pf
        .patterns
        .get(pattern_name)
        .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;

    let mod_name = binary
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| "Binary has no filename".to_string())?;
    let mapping = crate::steam_bridge::binary_reader::find_loaded_module_mapping(&mod_name)
        .ok_or_else(|| "Module not loaded".to_string())?;

    let vaddr = if let Some(pattern_hex) = &entry.pattern {
        let (pattern_bytes, mask_bytes) =
            parse_pattern_and_mask(pattern_hex, entry.mask.as_deref())
                .ok_or_else(|| "Malformed hex pattern or mask".to_string())?;
        let offset = entry.offset.unwrap_or(0);
        binary.resolve_pattern_address(&pattern_bytes, mask_bytes.as_deref(), offset, None)
    } else {
        entry.address
    }
    .ok_or_else(|| "Failed to resolve pattern address".to_string())?;

    binary
        .vaddr_to_runtime_addr(vaddr, &mapping, JMP_ABS64_LEN)
        .ok_or_else(|| "Resolved address not in executable memory".to_string())
}

// ============================================================================
// Public API
// ============================================================================

/// Set the real AppID when `-onlinefix` is active (`0` clears it).
pub fn set_online_fix_real_app_id(app_id: u32) {
    ONLINE_FIX_REAL_APP_ID.store(app_id, Ordering::Release);
    if app_id == 0 {
        NETWORKING_SOCKETS_ACTIVE.store(false, Ordering::Release);
    }
}

/// Record captured SteamEngine pointer.
pub fn set_steam_engine_ptr(ptr: *mut c_void) {
    STEAM_ENGINE_PTR.store(ptr as usize, Ordering::Release);
}

/// Record captured CAppInfoCache pointer.
pub fn set_app_info_cache_ptr(ptr: *mut c_void) {
    APP_INFO_CACHE_PTR.store(ptr as usize, Ordering::Release);
}

/// Get the current AppID for the active game pipe.
pub fn get_app_id_for_current_pipe() -> u32 {
    let real = ONLINE_FIX_REAL_APP_ID.load(Ordering::Acquire);
    if real != 0 {
        return real;
    }

    let engine = STEAM_ENGINE_PTR.load(Ordering::Acquire) as *mut c_void;
    if !engine.is_null() {
        let orig = *original_get_app_id_for_current_pipe()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(orig_fn) = orig {
            return unsafe { orig_fn(engine) };
        }
    }
    0
}

/// Resolve the current AppID for the game being launched.
pub fn resolve_app_id() -> u32 {
    let real = ONLINE_FIX_REAL_APP_ID.load(Ordering::Acquire);
    if real != 0 {
        return real;
    }
    get_app_id_for_current_pipe()
}

/// Check if `-onlinefix` game is currently active.
pub fn is_online_fix_active() -> bool {
    ONLINE_FIX_REAL_APP_ID.load(Ordering::Acquire) != 0
}

/// Mark that SteamNetworkingSockets P2P is active for this session.
pub fn notify_networking_sockets_used() {
    if ONLINE_FIX_REAL_APP_ID.load(Ordering::Acquire) != 0
        && !NETWORKING_SOCKETS_ACTIVE.swap(true, Ordering::AcqRel)
    {
        log::info!("NetworkingSockets active: GetAppID now reports 480 for cert match");
    }
}

/// Should `GetAppID` report 480 for P2P certificate matching?
pub fn should_report_online_fix_app_id() -> bool {
    if SUPPRESS_APP_ID_FLIP.load(Ordering::Acquire) {
        return false;
    }
    ONLINE_FIX_REAL_APP_ID.load(Ordering::Acquire) != 0
        && NETWORKING_SOCKETS_ACTIVE.load(Ordering::Acquire)
}

/// Set whether to suppress the appid flip (`-realappid`).
pub fn set_suppress_app_id_flip(suppress: bool) {
    SUPPRESS_APP_ID_FLIP.store(suppress, Ordering::Release);
}

/// Ensure CUtlBuffer has sufficient capacity (for ticket responses).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn ensure_buffer_capacity(buffer: *mut c_void, new_capacity: u32, _update_put: bool) -> bool {
    if buffer.is_null() {
        return false;
    }
    let orig = *original_cutl_buffer_ensure_capacity()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(orig_fn) = orig {
        unsafe {
            orig_fn(buffer, new_capacity);
        }
        return true;
    }
    false
}

/// Cache a known display name for an AppID.
pub fn set_game_name_by_app_id(app_id: u32, name: String) {
    if app_id == 0 || name.trim().is_empty() {
        return;
    }
    game_name_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(app_id, name);
}

/// Get game name by AppID (with caching).
pub fn get_game_name_by_app_id(app_id: u32) -> String {
    if app_id == 0 {
        return "App_0".to_string();
    }
    let mut cache = game_name_cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(name) = cache.get(&app_id) {
        return name.clone();
    }
    let name = format!("App_{}", app_id);
    cache.insert(app_id, name.clone());
    name
}

/// Check if an AppID is Lua-unlocked (in additional_apps, i.e., configured via addappid).
/// This mirrors LuaConfig::HasDepot(appId) from Windows.
/// Returns true if the app is either:
/// 1. Actually owned (has_depot returns true)
/// 2. In additional_apps (configured via addappid to be playable without ownership)
pub fn is_lua_unlocked(app_id: u32) -> bool {
    // First check if actually owned
    if crate::steam_bridge::interop::interop_package::has_depot(app_id) {
        return true;
    }
    // Then check if in additional_apps (Lua-unlocked)
    crate::steam_bridge::interop::interop_package::is_additional_app(app_id)
}

/// Friend entry in CMsgClientPersonaState.
#[derive(Debug, Clone, Default)]
pub struct FriendEntry {
    pub friendid: Option<u64>,
    pub persona_state: Option<u32>,
    pub game_played_app_id: Option<u32>,
    pub game_server_ip: Option<u32>,
    pub game_server_port: Option<u32>,
    pub persona_state_flags: Option<u32>,
    pub online_session_instances: Option<u32>,
    pub persona_set_by_user: Option<bool>,
    pub player_name: Option<String>,
    pub query_port: Option<u32>,
    pub steamid_source: Option<u64>,
    pub avatar_hash: Option<Vec<u8>>,
    pub last_logoff: Option<u32>,
    pub last_logon: Option<u32>,
    pub last_seen_online: Option<u32>,
    pub clan_rank: Option<u32>,
    pub game_name: Option<String>,
    pub gameid: Option<u64>,
    pub game_data_blob: Option<Vec<u8>>,
    pub clan_data: Option<ClanData>,
    pub clan_tag: Option<String>,
    pub rich_presence: Vec<(String, String)>,
    pub broadcast_id: Option<u64>,
    pub game_lobby_id: Option<u64>,
    pub watching_broadcast_accountid: Option<u32>,
    pub watching_broadcast_appid: Option<u32>,
    pub watching_broadcast_viewers: Option<u32>,
    pub watching_broadcast_title: Option<String>,
    pub is_community_banned: Option<bool>,
    pub player_name_pending_review: Option<bool>,
    pub avatar_pending_review: Option<bool>,
    pub on_steam_deck: Option<bool>,
    pub other_game_data: Vec<OtherGameData>,
    pub gaming_device_type: Option<u32>,
}

/// Clan data in CMsgClientPersonaState.
#[derive(Debug, Clone, Default)]
pub struct ClanData {
    pub ogg_app_id: Option<u32>,
    pub chat_group_id: Option<u64>,
}

/// Other game data in CMsgClientPersonaState.
#[derive(Debug, Clone, Default)]
pub struct OtherGameData {
    pub gameid: Option<u64>,
    pub rich_presence: Vec<(String, String)>,
}

/// CMsgClientPersonaState message for RichPresence handling.
#[derive(Debug, Clone, Default)]
pub struct PersonaStateMsg {
    pub status_flags: u32,
    pub friends: Vec<FriendEntry>,
}

impl PersonaStateMsg {
    /// Parse CMsgClientPersonaState from protobuf body.
    pub fn parse(data: &[u8]) -> Option<Self> {
        let mut msg = Self::default();
        let mut pos = 0;
        let size = data.len();

        while pos < size {
            if pos >= size { break; }
            let tag = data[pos];
            pos += 1;
            let field_num = tag >> 3;
            let wire_type = tag & 0x07;

            if field_num == 1 && wire_type == 0 {
                // status_flags (varint)
                let (val, bytes) = decode_varint(&data[pos..])?;
                msg.status_flags = val as u32;
                pos += bytes;
            } else if field_num == 2 && wire_type == 2 {
                // friends (repeated length-delimited)
                let (len, len_bytes) = decode_varint(&data[pos..])?;
                pos += len_bytes;
                if pos + len as usize > size {
                    break;
                }
                let friend_data = &data[pos..pos + len as usize];
                pos += len as usize;
                if let Some(friend) = Self::parse_friend(friend_data) {
                    msg.friends.push(friend);
                }
            } else {
                // Skip unknown field
                match wire_type {
                    0 => { let _ = decode_varint(&data[pos..]); }
                    1 => { if pos + 8 <= size { pos += 8; } }
                    2 => {
                        let (len, len_bytes) = decode_varint(&data[pos..])?;
                        pos += len_bytes + len as usize;
                    }
                    5 => { if pos + 4 <= size { pos += 4; } }
                    _ => break,
                }
            }
        }
        Some(msg)
    }

    fn parse_friend(data: &[u8]) -> Option<FriendEntry> {
        let mut entry = FriendEntry::default();
        let mut pos = 0;
        let size = data.len();

        while pos < size {
            if pos >= size { break; }
            let tag = data[pos];
            pos += 1;
            let field_num = tag >> 3;
            let wire_type = tag & 0x07;

            match (field_num, wire_type) {
                (1, 0) => { // friendid (fixed64)
                    let (val, bytes) = decode_varint(&data[pos..])?;
                    entry.friendid = Some(val);
                    pos += bytes;
                }
                (2, 0) => { // persona_state
                    let (val, bytes) = decode_varint(&data[pos..])?;
                    entry.persona_state = Some(val as u32);
                    pos += bytes;
                }
                (3, 0) => { // game_played_app_id
                    let (val, bytes) = decode_varint(&data[pos..])?;
                    entry.game_played_app_id = Some(val as u32);
                    pos += bytes;
                }
                (4, 5) => { // game_server_ip
                    if pos + 4 <= size {
                        entry.game_server_ip = Some(u32::from_le_bytes([
                            data[pos], data[pos+1], data[pos+2], data[pos+3]
                        ]));
                        pos += 4;
                    }
                }
                (5, 5) => { // game_server_port
                    if pos + 4 <= size {
                        entry.game_server_port = Some(u32::from_le_bytes([
                            data[pos], data[pos+1], data[pos+2], data[pos+3]
                        ]));
                        pos += 4;
                    }
                }
                (6, 0) => { // persona_state_flags
                    let (val, bytes) = decode_varint(&data[pos..])?;
                    entry.persona_state_flags = Some(val as u32);
                    pos += bytes;
                }
                (7, 0) => { // online_session_instances
                    let (val, bytes) = decode_varint(&data[pos..])?;
                    entry.online_session_instances = Some(val as u32);
                    pos += bytes;
                }
                (10, 0) => { // persona_set_by_user
                    let (val, bytes) = decode_varint(&data[pos..])?;
                    entry.persona_set_by_user = Some(val != 0);
                    pos += bytes;
                }
                (15, 2) => { // player_name
                    let (len, len_bytes) = decode_varint(&data[pos..])?;
                    pos += len_bytes;
                    if pos + len as usize <= size {
                        entry.player_name = String::from_utf8(data[pos..pos+len as usize].to_vec()).ok();
                        pos += len as usize;
                    }
                }
                (20, 5) => { // query_port
                    if pos + 4 <= size {
                        entry.query_port = Some(u32::from_le_bytes([
                            data[pos], data[pos+1], data[pos+2], data[pos+3]
                        ]));
                        pos += 4;
                    }
                }
                (25, 1) => { // steamid_source (fixed64)
                    if pos + 8 <= size {
                        entry.steamid_source = Some(u64::from_le_bytes([
                            data[pos], data[pos+1], data[pos+2], data[pos+3],
                            data[pos+4], data[pos+5], data[pos+6], data[pos+7],
                        ]));
                        pos += 8;
                    }
                }
                (31, 2) => { // avatar_hash
                    let (len, len_bytes) = decode_varint(&data[pos..])?;
                    pos += len_bytes;
                    if pos + len as usize <= size {
                        entry.avatar_hash = Some(data[pos..pos+len as usize].to_vec());
                        pos += len as usize;
                    }
                }
                (45, 0) => { // last_logoff
                    let (val, bytes) = decode_varint(&data[pos..])?;
                    entry.last_logoff = Some(val as u32);
                    pos += bytes;
                }
                (46, 0) => { // last_logon
                    let (val, bytes) = decode_varint(&data[pos..])?;
                    entry.last_logon = Some(val as u32);
                    pos += bytes;
                }
                (47, 0) => { // last_seen_online
                    let (val, bytes) = decode_varint(&data[pos..])?;
                    entry.last_seen_online = Some(val as u32);
                    pos += bytes;
                }
                (50, 0) => { // clan_rank
                    let (val, bytes) = decode_varint(&data[pos..])?;
                    entry.clan_rank = Some(val as u32);
                    pos += bytes;
                }
                (55, 2) => { // game_name
                    let (len, len_bytes) = decode_varint(&data[pos..])?;
                    pos += len_bytes;
                    if pos + len as usize <= size {
                        entry.game_name = String::from_utf8(data[pos..pos+len as usize].to_vec()).ok();
                        pos += len as usize;
                    }
                }
                (56, 1) => { // gameid (fixed64)
                    if pos + 8 <= size {
                        entry.gameid = Some(u64::from_le_bytes([
                            data[pos], data[pos+1], data[pos+2], data[pos+3],
                            data[pos+4], data[pos+5], data[pos+6], data[pos+7],
                        ]));
                        pos += 8;
                    }
                }
                (60, 2) => { // game_data_blob
                    let (len, len_bytes) = decode_varint(&data[pos..])?;
                    pos += len_bytes;
                    if pos + len as usize <= size {
                        entry.game_data_blob = Some(data[pos..pos+len as usize].to_vec());
                        pos += len as usize;
                    }
                }
                (64, 2) => { // clan_data
                    let (len, len_bytes) = decode_varint(&data[pos..])?;
                    pos += len_bytes;
                    if pos + len as usize <= size {
                        entry.clan_data = Self::parse_clan_data(&data[pos..pos+len as usize]);
                        pos += len as usize;
                    }
                }
                (65, 2) => { // clan_tag
                    let (len, len_bytes) = decode_varint(&data[pos..])?;
                    pos += len_bytes;
                    if pos + len as usize <= size {
                        entry.clan_tag = String::from_utf8(data[pos..pos+len as usize].to_vec()).ok();
                        pos += len as usize;
                    }
                }
                (71, 2) => { // rich_presence (repeated KV)
                    let (len, len_bytes) = decode_varint(&data[pos..])?;
                    pos += len_bytes;
                    if pos + len as usize <= size {
                        let kv_data = &data[pos..pos+len as usize];
                        if let Some((k, v)) = Self::parse_kv(kv_data) {
                            entry.rich_presence.push((k, v));
                        }
                        pos += len as usize;
                    }
                }
                (72, 1) => { // broadcast_id
                    if pos + 8 <= size {
                        entry.broadcast_id = Some(u64::from_le_bytes([
                            data[pos], data[pos+1], data[pos+2], data[pos+3],
                            data[pos+4], data[pos+5], data[pos+6], data[pos+7],
                        ]));
                        pos += 8;
                    }
                }
                (73, 1) => { // game_lobby_id
                    if pos + 8 <= size {
                        entry.game_lobby_id = Some(u64::from_le_bytes([
                            data[pos], data[pos+1], data[pos+2], data[pos+3],
                            data[pos+4], data[pos+5], data[pos+6], data[pos+7],
                        ]));
                        pos += 8;
                    }
                }
                (74, 0) => { // watching_broadcast_accountid
                    let (val, bytes) = decode_varint(&data[pos..])?;
                    entry.watching_broadcast_accountid = Some(val as u32);
                    pos += bytes;
                }
                (75, 0) => { // watching_broadcast_appid
                    let (val, bytes) = decode_varint(&data[pos..])?;
                    entry.watching_broadcast_appid = Some(val as u32);
                    pos += bytes;
                }
                (76, 0) => { // watching_broadcast_viewers
                    let (val, bytes) = decode_varint(&data[pos..])?;
                    entry.watching_broadcast_viewers = Some(val as u32);
                    pos += bytes;
                }
                (77, 2) => { // watching_broadcast_title
                    let (len, len_bytes) = decode_varint(&data[pos..])?;
                    pos += len_bytes;
                    if pos + len as usize <= size {
                        entry.watching_broadcast_title = String::from_utf8(data[pos..pos+len as usize].to_vec()).ok();
                        pos += len as usize;
                    }
                }
                (78, 0) => { // is_community_banned
                    let (val, bytes) = decode_varint(&data[pos..])?;
                    entry.is_community_banned = Some(val != 0);
                    pos += bytes;
                }
                (79, 0) => { // player_name_pending_review
                    let (val, bytes) = decode_varint(&data[pos..])?;
                    entry.player_name_pending_review = Some(val != 0);
                    pos += bytes;
                }
                (80, 0) => { // avatar_pending_review
                    let (val, bytes) = decode_varint(&data[pos..])?;
                    entry.avatar_pending_review = Some(val != 0);
                    pos += bytes;
                }
                (81, 0) => { // on_steam_deck
                    let (val, bytes) = decode_varint(&data[pos..])?;
                    entry.on_steam_deck = Some(val != 0);
                    pos += bytes;
                }
                (82, 2) => { // other_game_data (repeated)
                    let (len, len_bytes) = decode_varint(&data[pos..])?;
                    pos += len_bytes;
                    if pos + len as usize <= size {
                        if let Some(other) = Self::parse_other_game_data(&data[pos..pos+len as usize]) {
                            entry.other_game_data.push(other);
                        }
                        pos += len as usize;
                    }
                }
                (83, 0) => { // gaming_device_type
                    let (val, bytes) = decode_varint(&data[pos..])?;
                    entry.gaming_device_type = Some(val as u32);
                    pos += bytes;
                }
                _ => {
                    // Skip unknown field
                    match wire_type {
                        0 => { let _ = decode_varint(&data[pos..]); }
                        1 => { if pos + 8 <= size { pos += 8; } }
                        2 => {
                            let (len, len_bytes) = decode_varint(&data[pos..])?;
                            pos += len_bytes + len as usize;
                        }
                        5 => { if pos + 4 <= size { pos += 4; } }
                        _ => break,
                    }
                }
            }
        }
        Some(entry)
    }

    fn parse_clan_data(data: &[u8]) -> Option<ClanData> {
        let mut clan = ClanData::default();
        let mut pos = 0;
        let size = data.len();

        while pos < size {
            if pos >= size { break; }
            let tag = data[pos];
            pos += 1;
            let field_num = tag >> 3;
            let wire_type = tag & 0x07;

            match (field_num, wire_type) {
                (1, 0) => { // ogg_app_id
                    let (val, bytes) = decode_varint(&data[pos..])?;
                    clan.ogg_app_id = Some(val as u32);
                    pos += bytes;
                }
                (2, 1) => { // chat_group_id (fixed64)
                    if pos + 8 <= size {
                        clan.chat_group_id = Some(u64::from_le_bytes([
                            data[pos], data[pos+1], data[pos+2], data[pos+3],
                            data[pos+4], data[pos+5], data[pos+6], data[pos+7],
                        ]));
                        pos += 8;
                    }
                }
                _ => {
                    match wire_type {
                        0 => { let _ = decode_varint(&data[pos..]); }
                        1 => { if pos + 8 <= size { pos += 8; } }
                        2 => {
                            let (len, len_bytes) = decode_varint(&data[pos..])?;
                            pos += len_bytes + len as usize;
                        }
                        5 => { if pos + 4 <= size { pos += 4; } }
                        _ => break,
                    }
                }
            }
        }
        Some(clan)
    }

    fn parse_kv(data: &[u8]) -> Option<(String, String)> {
        let mut key = String::new();
        let mut value = String::new();
        let mut pos = 0;
        let size = data.len();

        while pos < size {
            if pos >= size { break; }
            let tag = data[pos];
            pos += 1;
            let field_num = tag >> 3;
            let wire_type = tag & 0x07;

            if field_num == 1 && wire_type == 2 {
                // key
                let (len, len_bytes) = decode_varint(&data[pos..])?;
                pos += len_bytes;
                if pos + len as usize <= size {
                    key = String::from_utf8(data[pos..pos+len as usize].to_vec()).ok()?;
                    pos += len as usize;
                }
            } else if field_num == 2 && wire_type == 2 {
                // value
                let (len, len_bytes) = decode_varint(&data[pos..])?;
                pos += len_bytes;
                if pos + len as usize <= size {
                    value = String::from_utf8(data[pos..pos+len as usize].to_vec()).ok()?;
                    pos += len as usize;
                }
            } else {
                match wire_type {
                    0 => { let _ = decode_varint(&data[pos..]); }
                    1 => { if pos + 8 <= size { pos += 8; } }
                    2 => {
                        let (len, len_bytes) = decode_varint(&data[pos..])?;
                        pos += len_bytes + len as usize;
                    }
                    5 => { if pos + 4 <= size { pos += 4; } }
                    _ => break,
                }
            }
        }
        if !key.is_empty() {
            Some((key, value))
        } else {
            None
        }
    }

    fn parse_other_game_data(data: &[u8]) -> Option<OtherGameData> {
        let mut other = OtherGameData::default();
        let mut pos = 0;
        let size = data.len();

        while pos < size {
            if pos >= size { break; }
            let tag = data[pos];
            pos += 1;
            let field_num = tag >> 3;
            let wire_type = tag & 0x07;

            if field_num == 1 && wire_type == 1 {
                // gameid (fixed64)
                if pos + 8 <= size {
                    other.gameid = Some(u64::from_le_bytes([
                        data[pos], data[pos+1], data[pos+2], data[pos+3],
                        data[pos+4], data[pos+5], data[pos+6], data[pos+7],
                    ]));
                    pos += 8;
                }
            } else if field_num == 2 && wire_type == 2 {
                // rich_presence (repeated KV)
                let (len, len_bytes) = decode_varint(&data[pos..])?;
                pos += len_bytes;
                if pos + len as usize <= size {
                    let kv_data = &data[pos..pos+len as usize];
                    if let Some((k, v)) = Self::parse_kv(kv_data) {
                        other.rich_presence.push((k, v));
                    }
                    pos += len as usize;
                }
            } else {
                match wire_type {
                    0 => { let _ = decode_varint(&data[pos..]); }
                    1 => { if pos + 8 <= size { pos += 8; } }
                    2 => {
                        let (len, len_bytes) = decode_varint(&data[pos..])?;
                        pos += len_bytes + len as usize;
                    }
                    5 => { if pos + 4 <= size { pos += 4; } }
                    _ => break,
                }
            }
        }
        Some(other)
    }

    /// Serialize PersonaStateMsg to protobuf bytes.
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();

        // status_flags (field 1, varint)
        out.push(0x08); // tag = 1 << 3 | 0
        encode_varint(self.status_flags as u64, &mut out);

        // friends (field 2, repeated length-delimited)
        for friend in &self.friends {
            let friend_bytes = friend.serialize();
            out.push(0x12); // tag = 2 << 3 | 2
            encode_varint(friend_bytes.len() as u64, &mut out);
            out.extend_from_slice(&friend_bytes);
        }

        out
    }
}

impl FriendEntry {
    /// Serialize FriendEntry to protobuf bytes.
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();

        if let Some(v) = self.friendid {
            out.push(0x08); // tag = 1 << 3 | 0
            encode_varint(v, &mut out);
        }
        if let Some(v) = self.persona_state {
            out.push(0x10); // tag = 2 << 3 | 0
            encode_varint(v as u64, &mut out);
        }
        if let Some(v) = self.game_played_app_id {
            out.push(0x18); // tag = 3 << 3 | 0
            encode_varint(v as u64, &mut out);
        }
        if let Some(v) = self.game_server_ip {
            out.push(0x25); // tag = 4 << 3 | 5
            out.extend_from_slice(&v.to_le_bytes());
        }
        if let Some(v) = self.game_server_port {
            out.push(0x2D); // tag = 5 << 3 | 5
            out.extend_from_slice(&v.to_le_bytes());
        }
        if let Some(v) = self.persona_state_flags {
            out.push(0x30); // tag = 6 << 3 | 0
            encode_varint(v as u64, &mut out);
        }
        if let Some(v) = self.online_session_instances {
            out.push(0x38); // tag = 7 << 3 | 0
            encode_varint(v as u64, &mut out);
        }
        if let Some(v) = self.persona_set_by_user {
            out.push(0x50); // tag = 10 << 3 | 0
            encode_varint(if v { 1 } else { 0 }, &mut out);
        }
        if let Some(v) = &self.player_name {
            out.push(0x7A); // tag = 15 << 3 | 2
            encode_varint(v.len() as u64, &mut out);
            out.extend_from_slice(v.as_bytes());
        }
        if let Some(v) = self.query_port {
            out.push(0xA5); // tag = 20 << 3 | 5
            out.extend_from_slice(&v.to_le_bytes());
        }
        if let Some(v) = self.steamid_source {
            out.push(0xC9); // tag = 25 << 3 | 1
            out.extend_from_slice(&v.to_le_bytes());
        }
        if let Some(v) = &self.avatar_hash {
            out.push(0xF2); // tag = 31 << 3 | 2
            encode_varint(v.len() as u64, &mut out);
            out.extend_from_slice(v);
        }
        if let Some(v) = self.last_logoff {
            // tag = 45 << 3 | 0 = 360 = 0x168
            out.push(0x68); out.push(0x02);
            encode_varint(v as u64, &mut out);
        }
        if let Some(v) = self.last_logon {
            // tag = 46 << 3 | 0 = 368 = 0x170
            out.push(0x70); out.push(0x02);
            encode_varint(v as u64, &mut out);
        }
        if let Some(v) = self.last_seen_online {
            // tag = 47 << 3 | 0 = 376 = 0x178
            out.push(0x78); out.push(0x02);
            encode_varint(v as u64, &mut out);
        }
        if let Some(v) = self.clan_rank {
            // tag = 50 << 3 | 0 = 400 = 0x190
            out.push(0x90); out.push(0x03);
            encode_varint(v as u64, &mut out);
        }
        if let Some(v) = &self.game_name {
            // tag = 55 << 3 | 2 = 442 = 0x1BA
            out.push(0xBA); out.push(0x03);
            encode_varint(v.len() as u64, &mut out);
            out.extend_from_slice(v.as_bytes());
        }
        if let Some(v) = self.gameid {
            // tag = 56 << 3 | 1 = 449 = 0x1C1
            out.push(0xC1); out.push(0x03);
            out.extend_from_slice(&v.to_le_bytes());
        }
        if let Some(v) = &self.game_data_blob {
            // tag = 60 << 3 | 2 = 482 = 0x1E2
            out.push(0xE2); out.push(0x03);
            encode_varint(v.len() as u64, &mut out);
            out.extend_from_slice(v);
        }
        if let Some(v) = &self.clan_data {
            let clan_bytes = v.serialize();
            // tag = 64 << 3 | 2 = 514 = 0x202
            out.push(0x02); out.push(0x02);
            encode_varint(clan_bytes.len() as u64, &mut out);
            out.extend_from_slice(&clan_bytes);
        }
        if let Some(v) = &self.clan_tag {
            // tag = 65 << 3 | 2 = 522 = 0x20A
            out.push(0x0A); out.push(0x02);
            encode_varint(v.len() as u64, &mut out);
            out.extend_from_slice(v.as_bytes());
        }
        for (k, v) in &self.rich_presence {
            let kv_bytes = Self::serialize_kv(k, v);
            // tag = 71 << 3 | 2 = 570 = 0x23A
            out.push(0x3A); out.push(0x02);
            encode_varint(kv_bytes.len() as u64, &mut out);
            out.extend_from_slice(&kv_bytes);
        }
        if let Some(v) = self.broadcast_id {
            // tag = 72 << 3 | 1 = 577 = 0x241
            out.push(0x41); out.push(0x02);
            out.extend_from_slice(&v.to_le_bytes());
        }
        if let Some(v) = self.game_lobby_id {
            // tag = 73 << 3 | 1 = 585 = 0x249
            out.push(0x49); out.push(0x02);
            out.extend_from_slice(&v.to_le_bytes());
        }
        if let Some(v) = self.watching_broadcast_accountid {
            // tag = 74 << 3 | 0 = 592 = 0x250
            out.push(0x50); out.push(0x02);
            encode_varint(v as u64, &mut out);
        }
        if let Some(v) = self.watching_broadcast_appid {
            // tag = 75 << 3 | 0 = 600 = 0x258
            out.push(0x58); out.push(0x02);
            encode_varint(v as u64, &mut out);
        }
        if let Some(v) = self.watching_broadcast_viewers {
            // tag = 76 << 3 | 0 = 608 = 0x260
            out.push(0x60); out.push(0x02);
            encode_varint(v as u64, &mut out);
        }
        if let Some(v) = &self.watching_broadcast_title {
            // tag = 77 << 3 | 2 = 618 = 0x26A
            out.push(0x6A); out.push(0x02);
            encode_varint(v.len() as u64, &mut out);
            out.extend_from_slice(v.as_bytes());
        }
        if let Some(v) = self.is_community_banned {
            // tag = 78 << 3 | 0 = 624 = 0x270
            out.push(0x70); out.push(0x02);
            encode_varint(if v { 1 } else { 0 }, &mut out);
        }
        if let Some(v) = self.player_name_pending_review {
            // tag = 79 << 3 | 0 = 632 = 0x278
            out.push(0x78); out.push(0x02);
            encode_varint(if v { 1 } else { 0 }, &mut out);
        }
        if let Some(v) = self.avatar_pending_review {
            // tag = 80 << 3 | 0 = 640 = 0x280
            out.push(0x80); out.push(0x02);
            encode_varint(if v { 1 } else { 0 }, &mut out);
        }
        if let Some(v) = self.on_steam_deck {
            // tag = 81 << 3 | 0 = 648 = 0x288
            out.push(0x88); out.push(0x02);
            encode_varint(if v { 1 } else { 0 }, &mut out);
        }
        for other in &self.other_game_data {
            let other_bytes = other.serialize();
            // tag = 82 << 3 | 2 = 658 = 0x292
            out.push(0x92); out.push(0x02);
            encode_varint(other_bytes.len() as u64, &mut out);
            out.extend_from_slice(&other_bytes);
        }
        if let Some(v) = self.gaming_device_type {
            // tag = 83 << 3 | 0 = 664 = 0x298
            out.push(0x98); out.push(0x02);
            encode_varint(v as u64, &mut out);
        }

        out
    }

    fn serialize_kv(key: &str, value: &str) -> Vec<u8> {
        let mut out = Vec::new();
        // key (field 1, length-delimited)
        out.push(0x0A); // tag = 1 << 3 | 2
        encode_varint(key.len() as u64, &mut out);
        out.extend_from_slice(key.as_bytes());
        // value (field 2, length-delimited)
        out.push(0x12); // tag = 2 << 3 | 2
        encode_varint(value.len() as u64, &mut out);
        out.extend_from_slice(value.as_bytes());
        out
    }
}

impl ClanData {
    fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(v) = self.ogg_app_id {
            out.push(0x08); // tag = 1 << 3 | 0
            encode_varint(v as u64, &mut out);
        }
        if let Some(v) = self.chat_group_id {
            out.push(0x11); // tag = 2 << 3 | 1
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }
}

impl OtherGameData {
    fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(v) = self.gameid {
            out.push(0x09); // tag = 1 << 3 | 1
            out.extend_from_slice(&v.to_le_bytes());
        }
        for (k, v) in &self.rich_presence {
            let kv_bytes = FriendEntry::serialize_kv(k, v);
            out.push(0x12); // tag = 2 << 3 | 2
            encode_varint(kv_bytes.len() as u64, &mut out);
            out.extend_from_slice(&kv_bytes);
        }
        out
    }
}

/// Encode a protobuf tag (field_num << 3 | wire_type).
fn encode_tag(field_num: u32, wire_type: u8, out: &mut Vec<u8>) {
    let tag = (field_num << 3) | u32::from(wire_type);
    encode_varint(tag as u64, out);
}

/// Encode a varint.
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

/// Decode a varint from slice.
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

/// Register misc IPC handlers (safe no-op hook point).
pub fn register_ipc_handlers() -> Result<(), String> {
    log::debug!("[InteropMisc] IPC handlers registered");
    Ok(())
}

/// Install all InteropMisc hooks (safe no-op when patterns are unavailable).
pub fn install(steam_root: &Path) -> Result<(), String> {
    log::info!("Installing InteropMisc hooks...");
    let _ = capture_get_app_id_for_current_pipe(steam_root);
    let _ = capture_get_app_data_from_app_info(steam_root);
    let _ = install_spawn_process_hook(steam_root);
    install_function_hooks(steam_root)?;
    log::info!("InteropMisc hooks installed");
    Ok(())
}

/// Uninstall all InteropMisc memory patches and clear saved trampolines.
pub fn uninstall() -> Result<(), String> {
    *original_opted_in_mask()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *original_build_spawn_env_block()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *original_get_or_add_app_data()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *original_cutl_buffer_ensure_capacity()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *original_get_app_id_for_current_pipe()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;

    let mut patches = installed_patches()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for mut patch in patches.drain(..) {
        unsafe { patch.uninstall() }?;
    }
    Ok(())
}

fn install_function_hooks(steam_root: &Path) -> Result<(), String> {
    let hooks: [(&str, *const ()); 4] = [
        ("opted_in_mask", opted_in_mask_replacement as *const ()),
        (
            "build_spawn_env_block",
            build_spawn_env_block_replacement as *const (),
        ),
        (
            "get_or_add_app_data",
            get_or_add_app_data_replacement as *const (),
        ),
        (
            "cutl_buffer_ensure_capacity",
            cutl_buffer_ensure_capacity_replacement as *const (),
        ),
    ];

    for (name, replacement) in hooks {
        let Ok(addr) = resolve_function_address(
            steam_root,
            BinaryComponent::SteamClient,
            PatternChannel::Pattern,
            name,
        ) else {
            continue;
        };

        // `patch_by_offset` installs the patch once — do not call `patch.install()` a second time.
        let patch = unsafe { memory_patch::patch_by_offset(addr, 0, replacement as *mut u8) }
            .map_err(|e| format!("Failed to install {} hook: {}", name, e))?;

        // Persist the trampoline pointer so each replacement delegates back to the original Steam function.
        if !patch.trampoline.is_null() {
            unsafe {
                match name {
                    "opted_in_mask" => {
                        *original_opted_in_mask()
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) =
                            Some(std::mem::transmute::<*mut u8, OptedInMaskFn>(
                                patch.trampoline,
                            ));
                    }
                    "build_spawn_env_block" => {
                        *original_build_spawn_env_block()
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) =
                            Some(std::mem::transmute::<*mut u8, BuildSpawnEnvBlockFn>(
                                patch.trampoline,
                            ));
                    }
                    "get_or_add_app_data" => {
                        *original_get_or_add_app_data()
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) =
                            Some(std::mem::transmute::<*mut u8, GetOrAddAppDataFn>(
                                patch.trampoline,
                            ));
                    }
                    "cutl_buffer_ensure_capacity" => {
                        *original_cutl_buffer_ensure_capacity()
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) =
                            Some(std::mem::transmute::<*mut u8, CUtlBufferEnsureCapacityFn>(
                                patch.trampoline,
                            ));
                    }
                    _ => {}
                }
            }
        }

        installed_patches()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(patch);
        log::info!("Installed {} hook at 0x{:x}", name, addr);
    }

    Ok(())
}

pub fn capture_get_app_id_for_current_pipe(steam_root: &Path) -> Result<(), String> {
    let addr = resolve_function_address(
        steam_root,
        BinaryComponent::SteamClient,
        PatternChannel::Pattern,
        "get_app_id_for_current_pipe",
    )?;
    let fn_ptr: GetAppIdForCurrentPipeFn =
        unsafe { std::mem::transmute::<usize, GetAppIdForCurrentPipeFn>(addr as usize) };
    *original_get_app_id_for_current_pipe()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(fn_ptr);
    log::info!("Resolved GetAppIDForCurrentPipe at 0x{:x}", addr);
    Ok(())
}

pub fn capture_get_app_data_from_app_info(steam_root: &Path) -> Result<(), String> {
    let addr = resolve_function_address(
        steam_root,
        BinaryComponent::SteamClient,
        PatternChannel::Pattern,
        "get_app_data_from_app_info",
    )?;
    log::info!("Resolved GetAppDataFromAppInfo at 0x{:x}", addr);
    Ok(())
}

pub fn install_spawn_process_hook(steam_root: &Path) -> Result<(), String> {
    let addr = resolve_function_address(
        steam_root,
        BinaryComponent::SteamClient,
        PatternChannel::Pattern,
        "spawn_process",
    )?;
    log::info!("Resolved SpawnProcess hook at 0x{:x}", addr);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn test_resolve_app_id_and_online_fix_state() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        set_online_fix_real_app_id(0);
        set_suppress_app_id_flip(false);
        assert_eq!(resolve_app_id(), 0);
        assert_eq!(get_app_id_for_current_pipe(), 0);
        assert!(!is_online_fix_active());
        assert!(!should_report_online_fix_app_id());

        set_online_fix_real_app_id(12345);
        assert_eq!(resolve_app_id(), 12345);
        assert!(is_online_fix_active());
        assert!(!should_report_online_fix_app_id());

        notify_networking_sockets_used();
        assert!(should_report_online_fix_app_id());

        set_suppress_app_id_flip(true);
        assert!(!should_report_online_fix_app_id());

        // Resetting real app ID to 0 must also clear NETWORKING_SOCKETS_ACTIVE
        set_suppress_app_id_flip(false);
        set_online_fix_real_app_id(0);
        set_online_fix_real_app_id(12345);
        assert!(!should_report_online_fix_app_id());

        // Reset shared globals
        set_online_fix_real_app_id(0);
        set_suppress_app_id_flip(false);
    }

    #[test]
    fn test_game_name_cache_and_null_buffer_guard() {
        assert!(!ensure_buffer_capacity(std::ptr::null_mut(), 1024, false));
        assert_eq!(get_game_name_by_app_id(0), "App_0");
        set_game_name_by_app_id(1245620, "ELDEN RING".into());
        assert_eq!(get_game_name_by_app_id(1245620), "ELDEN RING");
        assert!(uninstall().is_ok());
    }
}
