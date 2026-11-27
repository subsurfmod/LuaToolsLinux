//! NetPacket UserStats hook layer for Linux.
//!
//! Port of usefulsteamtools `Interop_NetPacket_UserStats.cpp`.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::os::raw::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use crate::steam_bridge::interop::interop_misc::resolve_app_id;
use crate::steam_bridge::interop::ipc::{
    self, fnv1a_hash, FuncHash, HandlerFn, IpcContext, IpcInterface,
};
use crate::steam_bridge::ost_platform::keyvalues1::{
    extract_achievements_with_schema, find_user_game_stats_schema_file, parse_kv1_bytes, Kv1Value,
};
use crate::steam_bridge::ost_platform::steam_credential_store;

const NETPACKET_USERSTATS_GET_USER_STATS_FNHASH: FuncHash = fnv1a_hash("GetUserStats");
const NETPACKET_USERSTATS_CLIENT_GET_USER_STATS_FNHASH: FuncHash = fnv1a_hash("ClientGetUserStats");
const MAX_HELD_ACH_PER_APP: usize = 256;
const ERESULT_OK: i32 = 1;

/// Callback invoked whenever queued achievement payloads are drained for delivery.
pub type AchievementDeliveryCallback = Arc<dyn Fn(u32, &[u8]) + Send + Sync + 'static>;

/// UserStats handler definition.
#[derive(Clone)]
pub struct UserStatsHandler {
    pub interface_id: IpcInterface,
    pub func_hash: FuncHash,
    pub name: String,
    pub fencepost: u32,
    pub argc: u32,
    pub pre_handler: Option<HandlerFn>,
    pub post_handler: Option<HandlerFn>,
}

impl fmt::Debug for UserStatsHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserStatsHandler")
            .field("interface_id", &self.interface_id)
            .field("func_hash", &self.func_hash)
            .field("name", &self.name)
            .finish()
    }
}

static USERSTATS_HANDLERS: LazyLock<Mutex<HashMap<(IpcInterface, FuncHash), UserStatsHandler>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static HELD_ACHIEVEMENTS: LazyLock<Mutex<HashMap<u32, Vec<Vec<u8>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static ACHIEVEMENT_DELIVERY_CALLBACK: LazyLock<Mutex<Option<AchievementDeliveryCallback>>> =
    LazyLock::new(|| Mutex::new(None));
static USERSTATS_ACTIVE: AtomicBool = AtomicBool::new(false);

fn userstats_handlers() -> &'static Mutex<HashMap<(IpcInterface, FuncHash), UserStatsHandler>> {
    &USERSTATS_HANDLERS
}

fn held_achievements() -> &'static Mutex<HashMap<u32, Vec<Vec<u8>>>> {
    &HELD_ACHIEVEMENTS
}

/// Register or clear a live delivery callback invoked when [`drain_achievements`] drains payloads.
pub fn set_achievement_delivery_callback(cb: Option<AchievementDeliveryCallback>) {
    *ACHIEVEMENT_DELIVERY_CALLBACK
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = cb;
}

// ============================================================================
// Steam `CMsgClientGetUserStatsResponse` Protobuf Wire Encoding (BST-41)
// ============================================================================

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

fn encode_fixed64_field(field_num: u32, val: u64, out: &mut Vec<u8>) {
    encode_varint((u64::from(field_num) << 3) | 1, out);
    out.extend_from_slice(&val.to_le_bytes());
}

fn encode_bytes_field(field_num: u32, bytes: &[u8], out: &mut Vec<u8>) {
    encode_varint((u64::from(field_num) << 3) | 2, out);
    encode_varint(bytes.len() as u64, out);
    out.extend_from_slice(bytes);
}

fn encode_fixed32_field(field_num: u32, val: u32, out: &mut Vec<u8>) {
    encode_varint((u64::from(field_num) << 3) | 5, out);
    out.extend_from_slice(&val.to_le_bytes());
}

/// Standard IEEE 802.3 CRC-32 for `CMsgClientGetUserStatsResponse.crc_stats`.
fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Extract numeric `(stat_id -> stat_value)` entries from a Steam `UserGameStats` KV1 tree.
fn extract_numeric_stats(kv1: &Kv1Value) -> BTreeMap<u32, u32> {
    let mut stats = BTreeMap::new();
    let Kv1Value::Object(root) = kv1 else {
        return stats;
    };
    for (k, v) in root {
        let Ok(stat_id) = k.parse::<u32>() else {
            continue;
        };
        let val = match v {
            Kv1Value::Int32(n) => Some(*n as u32),
            Kv1Value::UInt64(n) => Some(*n as u32),
            Kv1Value::Int64(n) => Some(*n as u32),
            Kv1Value::Float(f) => Some(f.to_bits()),
            _ => None,
        };
        if let Some(u) = val {
            stats.insert(stat_id, u);
        }
    }
    stats
}

/// Extract Steam `Achievement_Blocks` (`achievement_id -> unlock_time[0..N]`) from KV1 + optional schema.
fn extract_achievement_blocks(
    kv1: &Kv1Value,
    schema_kv1: Option<&Kv1Value>,
    named_achievements: &BTreeMap<
        String,
        crate::steam_bridge::ost_platform::keyvalues1::AchievementData,
    >,
) -> Vec<(u32, Vec<u32>)> {
    // Map: stat_id -> (bit_idx -> unlock_time)
    let mut block_bits: BTreeMap<u32, BTreeMap<u32, u32>> = BTreeMap::new();

    if let Kv1Value::Object(root) = kv1 {
        // 1. Pull explicit per-bit unlock timestamps from `AchievementTimes -> <stat_id> -> <bit_idx>`
        if let Some(Kv1Value::Object(times_root)) = root.get("AchievementTimes") {
            for (stat_key, stat_val) in times_root {
                let Ok(stat_id) = stat_key.parse::<u32>() else {
                    continue;
                };
                let Kv1Value::Object(bits_map) = stat_val else {
                    continue;
                };
                let entry = block_bits.entry(stat_id).or_default();
                for (bit_key, ts_val) in bits_map {
                    let Ok(bit_idx) = bit_key.parse::<u32>() else {
                        continue;
                    };
                    if bit_idx >= 32 {
                        continue;
                    }
                    let ts = match ts_val {
                        Kv1Value::Int32(t) if *t > 0 => *t as u32,
                        Kv1Value::UInt64(t) if *t > 0 => u32::try_from(*t).unwrap_or(u32::MAX),
                        Kv1Value::Int64(t) if *t > 0 => u32::try_from(*t).unwrap_or(u32::MAX),
                        _ => 0,
                    };
                    entry.insert(bit_idx, ts);
                }
            }
        }

        // 2. Cross-reference schema achievement bit definitions with user bitmasks
        if let Some(Kv1Value::Object(schema_root)) = schema_kv1 {
            let stats_obj = schema_root
                .get("stats")
                .and_then(|v| match v {
                    Kv1Value::Object(o) => Some(o),
                    _ => None,
                })
                .unwrap_or(schema_root);

            for (stat_key, stat_val) in stats_obj {
                let Ok(stat_id) = stat_key.parse::<u32>() else {
                    continue;
                };
                let Kv1Value::Object(stat_map) = stat_val else {
                    continue;
                };
                let Some(Kv1Value::Object(bits)) = stat_map.get("bits") else {
                    continue;
                };

                let mask = root
                    .get(stat_key)
                    .and_then(|v| match v {
                        Kv1Value::Int32(n) => Some(*n as u32),
                        Kv1Value::UInt64(n) => Some(*n as u32),
                        Kv1Value::Int64(n) => Some(*n as u32),
                        _ => None,
                    })
                    .unwrap_or(0);

                let entry = block_bits.entry(stat_id).or_default();
                for bit_key in bits.keys() {
                    let Ok(bit_idx) = bit_key.parse::<u32>() else {
                        continue;
                    };
                    if bit_idx >= 32 {
                        continue;
                    }
                    let is_unlocked = ((mask >> bit_idx) & 1) != 0;
                    entry
                        .entry(bit_idx)
                        .and_modify(|ts| {
                            if is_unlocked && *ts == 0 {
                                *ts = 1;
                            }
                        })
                        .or_insert(if is_unlocked { 1 } else { 0 });
                }
            }
        }
    }

    let mut out = Vec::new();
    for (stat_id, bits) in block_bits {
        let Some(&max_bit) = bits.keys().max() else {
            continue;
        };
        let len = (max_bit as usize + 1).min(32);
        let mut unlock_times = vec![0u32; len];
        for (bit_idx, ts) in bits {
            if (bit_idx as usize) < len {
                unlock_times[bit_idx as usize] = ts;
            }
        }
        out.push((stat_id, unlock_times));
    }

    // 3. Fallback for named `{ name, unlocked, unlockTime }` KV1 trees without numeric bitmasks
    if out.is_empty() && !named_achievements.is_empty() {
        let mut unlock_times = Vec::with_capacity(named_achievements.len());
        for ach in named_achievements.values() {
            if ach.unlocked {
                unlock_times.push(ach.unlock_time.filter(|&t| t > 0).unwrap_or(1));
            } else {
                unlock_times.push(0);
            }
        }
        out.push((1, unlock_times));
    }

    out
}

/// Encode a spec-valid Steam `CMsgClientGetUserStatsResponse` protobuf payload:
/// - `game_id = 1` (`fixed64`)
/// - `eresult = 2` (`int32`, `1 = k_EResultOK`)
/// - `crc_stats = 3` (`fixed32`)
/// - `schema = 4` (`bytes`, raw `UserGameStatsSchema_<app_id>.bin` when present)
/// - `stats = 5` (repeated `Stats { stat_id = 1 uint32, stat_value = 2 uint32 }`)
/// - `achievement_blocks = 6` (repeated `Achievement_Blocks { achievement_id = 1 uint32, unlock_time = 2 repeated fixed32 }`)
pub fn encode_cmsg_client_get_user_stats_response(
    app_id: u32,
    crc_stats: u32,
    schema_bytes: Option<&[u8]>,
    stats: &BTreeMap<u32, u32>,
    achievement_blocks: &[(u32, Vec<u32>)],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + schema_bytes.map_or(0, <[u8]>::len));
    encode_fixed64_field(1, u64::from(app_id), &mut out);
    encode_varint_field(2, ERESULT_OK as u64, &mut out);
    encode_fixed32_field(3, crc_stats, &mut out);

    if let Some(schema) = schema_bytes {
        if !schema.is_empty() {
            encode_bytes_field(4, schema, &mut out);
        }
    }

    for (&stat_id, &stat_value) in stats {
        let mut stat_msg = Vec::with_capacity(12);
        encode_varint_field(1, u64::from(stat_id), &mut stat_msg);
        encode_varint_field(2, u64::from(stat_value), &mut stat_msg);
        encode_bytes_field(5, &stat_msg, &mut out);
    }

    for (ach_id, unlock_times) in achievement_blocks {
        let mut block_msg = Vec::with_capacity(8 + unlock_times.len() * 5);
        encode_varint_field(1, u64::from(*ach_id), &mut block_msg);
        for &ts in unlock_times {
            encode_fixed32_field(2, ts, &mut block_msg);
        }
        encode_bytes_field(6, &block_msg, &mut out);
    }

    out
}

fn read_spoofed_steam_id(app_id: u32) -> u64 {
    let mut sid = 0u64;
    if steam_credential_store::get_steam_id(app_id, &mut sid) == steam_credential_store::Status::Ok
    {
        sid
    } else {
        0
    }
}

/// Register all UserStats handlers.
pub fn register_userstats_handlers() {
    let mut map = userstats_handlers()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let iface = IpcInterface::IClientUser;

    let get_post: HandlerFn = Arc::new(|ctx: &mut IpcContext| -> Result<(), String> {
        handle_send_get_user_stats_post(ctx);
        Ok(())
    });
    let client_post: HandlerFn = Arc::new(|ctx: &mut IpcContext| -> Result<(), String> {
        handle_send_client_get_user_stats_post(ctx);
        Ok(())
    });

    map.insert(
        (iface, NETPACKET_USERSTATS_GET_USER_STATS_FNHASH),
        UserStatsHandler {
            interface_id: iface,
            func_hash: NETPACKET_USERSTATS_GET_USER_STATS_FNHASH,
            name: "Hooks_NetPacket_UserStats::HandleSend_GetUserStats".to_string(),
            fencepost: 0,
            argc: 4,
            pre_handler: None,
            post_handler: Some(Arc::clone(&get_post)),
        },
    );

    map.insert(
        (iface, NETPACKET_USERSTATS_CLIENT_GET_USER_STATS_FNHASH),
        UserStatsHandler {
            interface_id: iface,
            func_hash: NETPACKET_USERSTATS_CLIENT_GET_USER_STATS_FNHASH,
            name: "Hooks_NetPacket_UserStats::HandleSend_ClientGetUserStats".to_string(),
            fencepost: 0,
            argc: 4,
            pre_handler: None,
            post_handler: Some(Arc::clone(&client_post)),
        },
    );
    drop(map);

    ipc::register_handler(ipc::make_handler(
        iface,
        NETPACKET_USERSTATS_GET_USER_STATS_FNHASH,
        "Hooks_NetPacket_UserStats::HandleSend_GetUserStats",
        0,
        4,
        None,
        Some(get_post),
    ));
    ipc::register_handler(ipc::make_handler(
        iface,
        NETPACKET_USERSTATS_CLIENT_GET_USER_STATS_FNHASH,
        "Hooks_NetPacket_UserStats::HandleSend_ClientGetUserStats",
        0,
        4,
        None,
        Some(client_post),
    ));
}

/// Unregister all UserStats handlers.
pub fn unregister_userstats_handlers() {
    userstats_handlers()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    let iface = IpcInterface::IClientUser;
    let _ = ipc::unregister_handler(iface, NETPACKET_USERSTATS_GET_USER_STATS_FNHASH);
    let _ = ipc::unregister_handler(iface, NETPACKET_USERSTATS_CLIENT_GET_USER_STATS_FNHASH);
}

fn handle_send_get_user_stats_post(ctx: &mut IpcContext) {
    let app_id = if ctx.app_id != 0 {
        ctx.app_id
    } else {
        resolve_app_id()
    };
    let spoofed_steam_id = read_spoofed_steam_id(app_id);

    if spoofed_steam_id == 0 {
        log::debug!(
            "HandleSend_GetUserStats: AppId={} - no spoofed SteamID available",
            app_id
        );
        return;
    }

    if ctx.write_buffer.len() >= 20 {
        ctx.write_buffer[12..20].copy_from_slice(&spoofed_steam_id.to_le_bytes());
    }
}

fn handle_send_client_get_user_stats_post(ctx: &mut IpcContext) {
    let app_id = if ctx.app_id != 0 {
        ctx.app_id
    } else {
        resolve_app_id()
    };
    let spoofed_steam_id = read_spoofed_steam_id(app_id);

    // Always force schema_local_version = -1 (0xFFFFFFFF) at offset 12
    if ctx.write_buffer.len() >= 16 {
        ctx.write_buffer[12..16].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        if spoofed_steam_id != 0 && ctx.write_buffer.len() >= 24 {
            ctx.write_buffer[16..24].copy_from_slice(&spoofed_steam_id.to_le_bytes());
        }
    }
}

/// Force `eresult = OK`, strip server stats, and deliver any queued held achievements
/// in `GetUserStatsResponse`.
pub fn handle_recv_get_user_stats_response(write_buf: &mut Vec<u8>, app_id: u32) {
    if write_buf.len() >= 12 {
        write_buf[8..12].copy_from_slice(&0u32.to_le_bytes());
        if write_buf.len() >= 16 {
            write_buf.truncate(16);
            write_buf[12..16].fill(0);
        }
    }

    let drained = drain_achievements(app_id, std::ptr::null_mut(), true);
    if let Some((_, payload)) = drained.into_iter().last() {
        if write_buf.len() >= 16 {
            write_buf[12..16].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        }
        write_buf.extend_from_slice(&payload);
    }

    log::debug!(
        "HandleRecv_GetUserStatsResponse: AppId={} - forced eresult=OK, len={}",
        app_id,
        write_buf.len()
    );
}

/// Clear server stats/achievements and deliver any queued held achievements
/// in `ClientGetUserStatsResponse`.
pub fn handle_recv_client_get_user_stats_response(write_buf: &mut Vec<u8>, app_id: u32) {
    if write_buf.len() >= 12 {
        write_buf[8..12].copy_from_slice(&0u32.to_le_bytes());
        write_buf.truncate(12);
    }

    let drained = drain_achievements(app_id, std::ptr::null_mut(), true);
    if let Some((_, payload)) = drained.into_iter().last() {
        write_buf.extend_from_slice(&payload);
    }

    log::debug!(
        "HandleRecv_ClientGetUserStatsResponse: AppId={} - len={}",
        app_id,
        write_buf.len()
    );
}

fn find_local_stats_bin(base_dir: &Path, app_id: u32) -> Option<std::path::PathBuf> {
    let target_name = format!("UserGameStats_{}.bin", app_id);
    let direct = base_dir.join(app_id.to_string()).join(&target_name);
    if direct.is_file() {
        return Some(direct);
    }
    // Also search Steam's `userdata/<account_id>/UserGameStats_<app_id>.bin` and
    // `appcache/stats/UserGameStats_<account_id>_<app_id>.bin` patterns.
    let suffix = format!("_{}.bin", app_id);
    let mut search_dirs = vec![base_dir.to_path_buf()];
    if let Some(parent) = base_dir.parent() {
        search_dirs.push(parent.join("appcache").join("stats"));
    }
    search_dirs.push(base_dir.join("appcache").join("stats"));

    for dir in search_dirs {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file() {
                    if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                        if name.starts_with("UserGameStats_")
                            && !name.starts_with("UserGameStatsSchema_")
                            && name.ends_with(&suffix)
                        {
                            return Some(p);
                        }
                    }
                } else if p.is_dir() {
                    let candidate = p.join(&target_name);
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
        }
    }
    None
}

/// Rebuild a spec-valid Steam `CMsgClientGetUserStatsResponse` protobuf payload from a local
/// `UserGameStats_<app_id>.bin` file inside `base_dir`, paired with `UserGameStatsSchema_<app_id>.bin`
/// when available.
pub fn fill_user_achievements_from_dir(
    base_dir: &Path,
    app_id: u32,
    resp: &mut Vec<u8>,
    n_unlocked: &mut u32,
) {
    *n_unlocked = 0;
    if app_id == 0 {
        return;
    }
    let Some(bin_path) = find_local_stats_bin(base_dir, app_id) else {
        return;
    };
    if let Ok(data) = std::fs::read(&bin_path) {
        if !data.is_empty() {
            if let Ok(kv1) = parse_kv1_bytes(&data) {
                let schema_path = find_user_game_stats_schema_file(base_dir, app_id).or_else(|| {
                    bin_path
                        .parent()
                        .and_then(|dir| find_user_game_stats_schema_file(dir, app_id))
                });
                let schema_bytes = schema_path
                    .as_ref()
                    .and_then(|sp| std::fs::read(sp).ok())
                    .filter(|b| !b.is_empty());
                let schema_kv1 = schema_bytes
                    .as_deref()
                    .and_then(|raw| parse_kv1_bytes(raw).ok());

                let achievements = extract_achievements_with_schema(&kv1, schema_kv1.as_ref());
                *n_unlocked = achievements.values().filter(|ach| ach.unlocked).count() as u32;

                let stats = extract_numeric_stats(&kv1);
                let achievement_blocks =
                    extract_achievement_blocks(&kv1, schema_kv1.as_ref(), &achievements);
                let crc_stats = crc32_ieee(&data);

                let proto_payload = encode_cmsg_client_get_user_stats_response(
                    app_id,
                    crc_stats,
                    schema_bytes.as_deref(),
                    &stats,
                    &achievement_blocks,
                );
                resp.extend_from_slice(&proto_payload);

                log::debug!(
                    "FillUserAchievements: AppId={} - encoded CMsgClientGetUserStatsResponse ({} bytes, {} achievements, {} unlocked, {} stats, {} blocks) from {}",
                    app_id,
                    proto_payload.len(),
                    achievements.len(),
                    *n_unlocked,
                    stats.len(),
                    achievement_blocks.len(),
                    bin_path.display()
                );
            }
        }
    }
}

/// Rebuild `CMsgClientGetUserStatsResponse` from schema + local `.bin` when present.
pub fn fill_user_achievements(app_id: u32, resp: &mut Vec<u8>, n_unlocked: &mut u32) {
    let Some(home) = dirs::home_dir() else {
        *n_unlocked = 0;
        return;
    };
    let userdata_dir = home.join(".steam/steam/userdata");
    fill_user_achievements_from_dir(&userdata_dir, app_id, resp, n_unlocked);
}

/// Queue an achievement payload for deferred delivery.
pub fn queue_held_user_ach(app_id: u32, ach_data: &[u8]) {
    if app_id == 0 || ach_data.is_empty() {
        return;
    }
    let mut map = held_achievements()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let queue = map.entry(app_id).or_default();
    if queue.len() < MAX_HELD_ACH_PER_APP {
        queue.push(ach_data.to_vec());
    }
    log::debug!(
        "QueueHeldUserAch: AppId={} - queued {} bytes (depth={})",
        app_id,
        ach_data.len(),
        queue.len()
    );
}

/// Return how many achievement payloads are currently queued for `app_id`.
pub fn held_achievement_count(app_id: u32) -> usize {
    held_achievements()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&app_id)
        .map_or(0, Vec::len)
}

/// Take and remove all queued achievement payloads for `app_id`.
pub fn take_held_achievements(app_id: u32) -> Vec<Vec<u8>> {
    held_achievements()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&app_id)
        .unwrap_or_default()
}

/// Drain queued achievement payloads for `app_id` (or all apps when `app_id == 0`),
/// deliver each payload to any registered delivery callback, and return the drained
/// `(app_id, payload)` entries to the caller instead of discarding them.
pub fn drain_achievements(
    app_id: u32,
    _carrier: *mut c_void,
    invoke_original: bool,
) -> Vec<(u32, Vec<u8>)> {
    let drained: Vec<(u32, Vec<u8>)> = if app_id != 0 {
        take_held_achievements(app_id)
            .into_iter()
            .map(|payload| (app_id, payload))
            .collect()
    } else {
        let mut map = held_achievements()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut keys: Vec<u32> = map.keys().copied().collect();
        keys.sort_unstable();
        let mut all = Vec::new();
        for aid in keys {
            if let Some(items) = map.remove(&aid) {
                for payload in items {
                    all.push((aid, payload));
                }
            }
        }
        all
    };

    if !drained.is_empty() {
        let cb = ACHIEVEMENT_DELIVERY_CALLBACK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(deliver) = cb {
            for (aid, payload) in &drained {
                deliver(*aid, payload);
            }
        }
    }

    log::debug!(
        "DrainAchievements: AppId={} drained={} (invoke_original={})",
        app_id,
        drained.len(),
        invoke_original
    );
    drained
}

pub fn install_userstats() -> Result<(), String> {
    register_userstats_handlers();
    USERSTATS_ACTIVE.store(true, Ordering::Release);
    log::info!("Installed UserStats NetPacket hooks");
    Ok(())
}

pub fn uninstall_userstats() {
    unregister_userstats_handlers();
    held_achievements()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    set_achievement_delivery_callback(None);
    USERSTATS_ACTIVE.store(false, Ordering::Release);
    log::info!("Uninstalled UserStats NetPacket hooks");
}

pub fn install_userstats_hooks() -> Result<(), String> {
    install_userstats()
}

pub fn uninstall_userstats_hooks() {
    uninstall_userstats()
}

pub fn is_userstats_hook_active() -> bool {
    USERSTATS_ACTIVE.load(Ordering::Acquire)
}

/// IPC adapter for Send: Player.GetUserStats#1 (service method call)
pub fn handle_send_get_user_stats_post_ipc(
    ctx: &mut crate::steam_bridge::netpacket::netpacket_core::NetPacketContext,
) -> Result<(), String> {
    let body = ctx.body().to_vec();
    let mut ipc_ctx = IpcContext {
        pipe: std::ptr::null_mut(),
        read_buffer: body.clone(),
        write_buffer: body,
        interface_id: IpcInterface::IClientUser,
        func_hash: fnv1a_hash("GetUserStats"),
        app_id: ctx.app_id,
    };
    handle_send_get_user_stats_post(&mut ipc_ctx);
    ctx.truncate(ctx.body_offset);
    ctx.extend(&ipc_ctx.write_buffer);
    Ok(())
}

/// IPC adapter for Send: ClientGetUserStats (legacy)
pub fn handle_send_client_get_user_stats_post_ipc(
    ctx: &mut crate::steam_bridge::netpacket::netpacket_core::NetPacketContext,
) -> Result<(), String> {
    let body = ctx.body().to_vec();
    let mut ipc_ctx = IpcContext {
        pipe: std::ptr::null_mut(),
        read_buffer: body.clone(),
        write_buffer: body,
        interface_id: IpcInterface::IClientUser,
        func_hash: fnv1a_hash("ClientGetUserStats"),
        app_id: ctx.app_id,
    };
    handle_send_client_get_user_stats_post(&mut ipc_ctx);
    ctx.truncate(ctx.body_offset);
    ctx.extend(&ipc_ctx.write_buffer);
    Ok(())
}

/// Replace protobuf body of a `NetPacketContext` with the drained `CMsgClientGetUserStatsResponse`
/// payload when `ctx.is_proto` and queued achievements exist for `ctx.app_id`.
fn deliver_held_into_proto_ctx(
    ctx: &mut crate::steam_bridge::netpacket::netpacket_core::NetPacketContext,
) -> bool {
    if !ctx.is_proto || ctx.app_id == 0 || ctx.frame_buffer.len() < 8 {
        return false;
    }
    let drained = drain_achievements(ctx.app_id, std::ptr::null_mut(), true);
    let Some((_, payload)) = drained.into_iter().last() else {
        return false;
    };
    let hdr_len = u32::from_le_bytes([
        ctx.frame_buffer[4],
        ctx.frame_buffer[5],
        ctx.frame_buffer[6],
        ctx.frame_buffer[7],
    ]) as usize;
    let body_start = if 8usize.saturating_add(hdr_len) <= ctx.frame_buffer.len() {
        8 + hdr_len
    } else {
        ctx.body_offset.min(ctx.frame_buffer.len())
    };
    ctx.truncate(body_start);
    ctx.extend(&payload);
    true
}

/// IPC adapter for Recv: Player.GetUserStats#1 response
pub fn handle_recv_get_user_stats_response_ipc(
    ctx: &mut crate::steam_bridge::netpacket::netpacket_core::NetPacketContext,
) -> Result<(), String> {
    if !deliver_held_into_proto_ctx(ctx) {
        let mut body = ctx.body().to_vec();
        handle_recv_get_user_stats_response(&mut body, ctx.app_id);
        ctx.truncate(ctx.body_offset);
        ctx.extend(&body);
    }
    Ok(())
}

/// IPC adapter for Recv: ClientGetUserStatsResponse
pub fn handle_recv_client_get_user_stats_response_ipc(
    ctx: &mut crate::steam_bridge::netpacket::netpacket_core::NetPacketContext,
) -> Result<(), String> {
    if !deliver_held_into_proto_ctx(ctx) {
        let mut body = ctx.body().to_vec();
        handle_recv_client_get_user_stats_response(&mut body, ctx.app_id);
        ctx.truncate(ctx.body_offset);
        ctx.extend(&body);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    static USERSTATS_TEST_GUARD: Mutex<()> = Mutex::new(());

    fn decode_varint(data: &[u8]) -> Option<(u64, usize)> {
        let mut res = 0u64;
        let mut shift = 0u32;
        for (i, &b) in data.iter().enumerate() {
            if i >= 10 {
                return None;
            }
            res |= u64::from(b & 0x7F) << shift;
            if (b & 0x80) == 0 {
                return Some((res, i + 1));
            }
            shift += 7;
        }
        None
    }

    #[derive(Debug, Default)]
    struct DecodedUserStatsResp {
        game_id: Option<u64>,
        eresult: Option<i32>,
        crc_stats: Option<u32>,
        schema: Option<Vec<u8>>,
        stats: Vec<(u32, u32)>,
        achievement_blocks: Vec<(u32, Vec<u32>)>,
    }

    fn decode_cmsg_user_stats_response(data: &[u8]) -> DecodedUserStatsResp {
        let mut out = DecodedUserStatsResp::default();
        let mut pos = 0;
        while pos < data.len() {
            let (tag, n) = decode_varint(&data[pos..]).expect("valid tag");
            pos += n;
            let field = (tag >> 3) as u32;
            let wire = (tag & 0x07) as u8;
            match (field, wire) {
                (1, 1) => {
                    out.game_id = Some(u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap()));
                    pos += 8;
                }
                (2, 0) => {
                    let (v, vn) = decode_varint(&data[pos..]).unwrap();
                    out.eresult = Some(v as i32);
                    pos += vn;
                }
                (3, 5) => {
                    out.crc_stats =
                        Some(u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()));
                    pos += 4;
                }
                (4, 2) => {
                    let (len, ln) = decode_varint(&data[pos..]).unwrap();
                    pos += ln;
                    let len_usize = usize::try_from(len).unwrap_or(0);
                    let end = pos.checked_add(len_usize).unwrap_or(data.len());
                    if end > data.len() {
                        break;
                    }
                    out.schema = Some(data[pos..end].to_vec());
                    pos = end;
                }
                (5, 2) => {
                    let (len, ln) = decode_varint(&data[pos..]).unwrap();
                    pos += ln;
                    let sub = &data[pos..pos + len as usize];
                    pos += len as usize;
                    let mut sp = 0;
                    let (mut sid, mut sval) = (0u32, 0u32);
                    while sp < sub.len() {
                        let (stag, sn) = decode_varint(&sub[sp..]).unwrap();
                        sp += sn;
                        let (v, vn) = decode_varint(&sub[sp..]).unwrap();
                        sp += vn;
                        match stag >> 3 {
                            1 => sid = v as u32,
                            2 => sval = v as u32,
                            _ => {}
                        }
                    }
                    out.stats.push((sid, sval));
                }
                (6, 2) => {
                    let (len, ln) = decode_varint(&data[pos..]).unwrap();
                    pos += ln;
                    let sub = &data[pos..pos + len as usize];
                    pos += len as usize;
                    let mut sp = 0;
                    let mut ach_id = 0u32;
                    let mut times = Vec::new();
                    while sp < sub.len() {
                        let (stag, sn) = decode_varint(&sub[sp..]).unwrap();
                        sp += sn;
                        match (stag >> 3, stag & 0x07) {
                            (1, 0) => {
                                let (v, vn) = decode_varint(&sub[sp..]).unwrap();
                                ach_id = v as u32;
                                sp += vn;
                            }
                            (2, 5) => {
                                times.push(u32::from_le_bytes(sub[sp..sp + 4].try_into().unwrap()));
                                sp += 4;
                            }
                            other => panic!("unexpected achievement_block wire {:?}", other),
                        }
                    }
                    out.achievement_blocks.push((ach_id, times));
                }
                other => panic!("unexpected field/wire {:?}", other),
            }
        }
        out
    }

    #[test]
    fn test_fnv1a_hash_get_user_stats() {
        assert_eq!(
            NETPACKET_USERSTATS_GET_USER_STATS_FNHASH,
            fnv1a_hash("GetUserStats")
        );
        assert_eq!(
            NETPACKET_USERSTATS_CLIENT_GET_USER_STATS_FNHASH,
            fnv1a_hash("ClientGetUserStats")
        );
    }

    #[test]
    fn test_handler_registration_and_uninstall() {
        let _g = USERSTATS_TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        install_userstats().unwrap();
        assert!(is_userstats_hook_active());

        {
            let handlers = userstats_handlers().lock().unwrap();
            assert!(handlers.contains_key(&(
                IpcInterface::IClientUser,
                NETPACKET_USERSTATS_GET_USER_STATS_FNHASH
            )));
            assert!(handlers.contains_key(&(
                IpcInterface::IClientUser,
                NETPACKET_USERSTATS_CLIENT_GET_USER_STATS_FNHASH
            )));
        }
        assert!(ipc::find_handler(
            IpcInterface::IClientUser,
            NETPACKET_USERSTATS_GET_USER_STATS_FNHASH
        )
        .is_some());

        uninstall_userstats();
        assert!(!is_userstats_hook_active());
        assert!(ipc::find_handler(
            IpcInterface::IClientUser,
            NETPACKET_USERSTATS_GET_USER_STATS_FNHASH
        )
        .is_none());
    }

    #[test]
    fn test_handle_send_get_user_stats_post() {
        let mut ctx = IpcContext {
            pipe: std::ptr::null_mut(),
            read_buffer: vec![],
            write_buffer: vec![0u8; 32],
            interface_id: IpcInterface::IClientUser,
            func_hash: 0,
            app_id: 123456,
        };
        handle_send_get_user_stats_post(&mut ctx);
        assert_eq!(ctx.write_buffer.len(), 32);
    }

    #[test]
    fn test_handle_send_client_get_user_stats_post() {
        let mut write_buf = vec![0u8; 32];
        write_buf[12..16].copy_from_slice(&1u32.to_le_bytes());
        write_buf[16..24].copy_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());

        let mut ctx = IpcContext {
            pipe: std::ptr::null_mut(),
            read_buffer: vec![],
            write_buffer: write_buf,
            interface_id: IpcInterface::IClientUser,
            func_hash: 0,
            app_id: 123456,
        };
        handle_send_client_get_user_stats_post(&mut ctx);

        assert_eq!(ctx.write_buffer[12..16], 0xFFFF_FFFFu32.to_le_bytes());
        assert!(ctx.write_buffer.len() >= 24);
    }

    #[test]
    fn test_handle_recv_get_user_stats_response_including_exact_16_bytes() {
        let _g = USERSTATS_TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _ = take_held_achievements(123456);

        let mut write_buf = vec![0u8; 256];
        write_buf[8..12].copy_from_slice(&2u32.to_le_bytes());
        write_buf[12..16].copy_from_slice(&5u32.to_le_bytes());
        write_buf[16..20].copy_from_slice(&3u32.to_le_bytes());
        write_buf[24..56].copy_from_slice(&[1u8; 32]);

        handle_recv_get_user_stats_response(&mut write_buf, 123456);
        assert_eq!(write_buf[8..12], 0u32.to_le_bytes());
        assert_eq!(write_buf[12..16], 0u32.to_le_bytes());
        assert_eq!(write_buf.len(), 16);

        // Exact 16-byte buffer must also zero out stat count at [12..16]
        let mut exact16 = vec![0xFFu8; 16];
        handle_recv_get_user_stats_response(&mut exact16, 123456);
        assert_eq!(exact16[8..12], 0u32.to_le_bytes());
        assert_eq!(exact16[12..16], 0u32.to_le_bytes());
    }

    #[test]
    fn test_handle_recv_client_get_user_stats_response() {
        let _g = USERSTATS_TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _ = take_held_achievements(123456);

        let mut write_buf = vec![0u8; 256];
        write_buf[8..12].copy_from_slice(&2u32.to_le_bytes());
        write_buf[12..16].copy_from_slice(&5u32.to_le_bytes());

        handle_recv_client_get_user_stats_response(&mut write_buf, 123456);
        assert_eq!(write_buf[8..12], 0u32.to_le_bytes());
        assert_eq!(write_buf.len(), 12);
    }

    #[test]
    fn test_held_achievement_queue_drain_and_fill_from_dir() {
        let _g = USERSTATS_TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let app_id = 444_555;
        let _ = take_held_achievements(app_id);

        queue_held_user_ach(app_id, &[]);
        assert_eq!(held_achievement_count(app_id), 0);

        queue_held_user_ach(app_id, b"ach-1");
        queue_held_user_ach(app_id, b"ach-2");
        assert_eq!(held_achievement_count(app_id), 2);

        let drained = drain_achievements(app_id, std::ptr::null_mut(), false);
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0], (app_id, b"ach-1".to_vec()));
        assert_eq!(drained[1], (app_id, b"ach-2".to_vec()));
        assert_eq!(held_achievement_count(app_id), 0);

        use crate::steam_bridge::ost_platform::keyvalues1::write_kv1;

        let tmp = tempfile::tempdir().unwrap();
        let steam_root = tmp.path().join("Steam");
        let userdata_dir = steam_root.join("userdata");
        let account_dir = userdata_dir.join("987654");
        let appcache_stats = steam_root.join("appcache/stats");
        std::fs::create_dir_all(&account_dir).unwrap();
        std::fs::create_dir_all(&appcache_stats).unwrap();

        let mut bit0 = BTreeMap::new();
        bit0.insert("name".into(), Kv1Value::String("ACH_ALPHA".into()));
        let mut bit1 = BTreeMap::new();
        bit1.insert("name".into(), Kv1Value::String("ACH_BETA".into()));
        let mut bits = BTreeMap::new();
        bits.insert("0".into(), Kv1Value::Object(bit0));
        bits.insert("1".into(), Kv1Value::Object(bit1));
        let mut stat1 = BTreeMap::new();
        stat1.insert("bits".into(), Kv1Value::Object(bits));
        let mut schema_stats = BTreeMap::new();
        schema_stats.insert("1".into(), Kv1Value::Object(stat1));
        let mut schema_root = BTreeMap::new();
        schema_root.insert("stats".into(), Kv1Value::Object(schema_stats));
        let schema_bytes = write_kv1(&app_id.to_string(), &Kv1Value::Object(schema_root)).unwrap();
        std::fs::write(
            appcache_stats.join(format!("UserGameStatsSchema_{}.bin", app_id)),
            &schema_bytes,
        )
        .unwrap();

        let mut user_root = BTreeMap::new();
        user_root.insert("1".into(), Kv1Value::Int32(0b11));
        std::fs::write(
            account_dir.join(format!("UserGameStats_{}.bin", app_id)),
            write_kv1("UserGameStats", &Kv1Value::Object(user_root)).unwrap(),
        )
        .unwrap();

        let mut resp = Vec::new();
        let mut n_unlocked = 99;
        fill_user_achievements_from_dir(&userdata_dir, app_id, &mut resp, &mut n_unlocked);
        assert!(!resp.is_empty());
        assert_eq!(n_unlocked, 2);
        // Raw KV1 schema in protobuf field 4 carries canonical names
        assert!(resp.windows(b"ACH_ALPHA".len()).any(|w| w == b"ACH_ALPHA"));
        assert!(resp.windows(b"ACH_BETA".len()).any(|w| w == b"ACH_BETA"));

        let decoded = decode_cmsg_user_stats_response(&resp);
        assert_eq!(decoded.game_id, Some(u64::from(app_id)));
        assert_eq!(decoded.eresult, Some(ERESULT_OK));
        assert_eq!(decoded.schema.as_deref(), Some(schema_bytes.as_slice()));
        assert_eq!(decoded.stats, vec![(1, 0b11)]);
        assert_eq!(decoded.achievement_blocks, vec![(1, vec![1, 1])]);
    }

    #[test]
    fn test_cmsg_client_get_user_stats_response_protobuf_wire_with_timestamps() {
        use crate::steam_bridge::ost_platform::keyvalues1::write_kv1;

        let app_id = 1245620u32;
        let tmp = tempfile::tempdir().unwrap();
        let steam_root = tmp.path().join("Steam");
        let userdata_dir = steam_root.join("userdata");
        let account_dir = userdata_dir.join("112233");
        let appcache_stats = steam_root.join("appcache/stats");
        std::fs::create_dir_all(&account_dir).unwrap();
        std::fs::create_dir_all(&appcache_stats).unwrap();

        let mut bits = BTreeMap::new();
        for (bit, name) in [("0", "ACH_0"), ("1", "ACH_1"), ("2", "ACH_2")] {
            let mut b = BTreeMap::new();
            b.insert("name".into(), Kv1Value::String(name.into()));
            bits.insert(bit.into(), Kv1Value::Object(b));
        }
        let mut stat1 = BTreeMap::new();
        stat1.insert("bits".into(), Kv1Value::Object(bits));
        let mut schema_stats = BTreeMap::new();
        schema_stats.insert("1".into(), Kv1Value::Object(stat1));
        let mut schema_root = BTreeMap::new();
        schema_root.insert("stats".into(), Kv1Value::Object(schema_stats));
        let schema_raw = write_kv1(&app_id.to_string(), &Kv1Value::Object(schema_root)).unwrap();
        std::fs::write(
            appcache_stats.join(format!("UserGameStatsSchema_{}.bin", app_id)),
            &schema_raw,
        )
        .unwrap();

        let mut times_stat1 = BTreeMap::new();
        times_stat1.insert("0".into(), Kv1Value::Int32(1_700_000_100));
        times_stat1.insert("2".into(), Kv1Value::Int32(1_700_000_200));
        let mut ach_times = BTreeMap::new();
        ach_times.insert("1".into(), Kv1Value::Object(times_stat1));
        let mut user_root = BTreeMap::new();
        user_root.insert("1".into(), Kv1Value::Int32(0b101));
        user_root.insert("42".into(), Kv1Value::Int32(999));
        user_root.insert("AchievementTimes".into(), Kv1Value::Object(ach_times));
        let user_raw = write_kv1("UserGameStats", &Kv1Value::Object(user_root)).unwrap();
        std::fs::write(
            account_dir.join(format!("UserGameStats_{}.bin", app_id)),
            &user_raw,
        )
        .unwrap();

        let mut resp = Vec::new();
        let mut n_unlocked = 0;
        fill_user_achievements_from_dir(&userdata_dir, app_id, &mut resp, &mut n_unlocked);
        assert_eq!(n_unlocked, 2);

        let decoded = decode_cmsg_user_stats_response(&resp);
        assert_eq!(decoded.game_id, Some(u64::from(app_id)));
        assert_eq!(decoded.eresult, Some(1));
        assert_eq!(decoded.crc_stats, Some(crc32_ieee(&user_raw)));
        assert_eq!(decoded.schema.as_deref(), Some(schema_raw.as_slice()));
        assert_eq!(decoded.stats, vec![(1, 0b101), (42, 999)]);
        assert_eq!(
            decoded.achievement_blocks,
            vec![(1, vec![1_700_000_100, 0, 1_700_000_200])]
        );
    }

    #[test]
    fn test_drain_achievements_non_discarding_and_live_recv_delivery() {
        use crate::steam_bridge::netpacket::netpacket_core::{
            NetPacketContext, NetPacketDirection, KM_MSG_HDR_PROTO_FLAG,
        };

        let _g = USERSTATS_TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let app_a = 700_001;
        let app_b = 700_002;
        let _ = take_held_achievements(app_a);
        let _ = take_held_achievements(app_b);

        let delivered_cb = Arc::new(Mutex::new(Vec::<(u32, Vec<u8>)>::new()));
        let cb_clone = Arc::clone(&delivered_cb);
        set_achievement_delivery_callback(Some(Arc::new(move |aid, bytes| {
            cb_clone.lock().unwrap().push((aid, bytes.to_vec()));
        })));

        let proto_a = encode_cmsg_client_get_user_stats_response(
            app_a,
            0x1234_5678,
            None,
            &BTreeMap::from([(1, 1)]),
            &[(1, vec![1_700_000_001])],
        );
        let proto_b = encode_cmsg_client_get_user_stats_response(
            app_b,
            0x8765_4321,
            None,
            &BTreeMap::from([(2, 7)]),
            &[(2, vec![1_700_000_002])],
        );

        queue_held_user_ach(app_a, &proto_a);
        queue_held_user_ach(app_b, &proto_b);

        // Live inbound proto delivery drains app_a into the NetPacketContext body
        let mut frame = Vec::new();
        frame.extend_from_slice(&(818u32 | KM_MSG_HDR_PROTO_FLAG).to_le_bytes());
        frame.extend_from_slice(&4u32.to_le_bytes()); // 4-byte proto header
        frame.extend_from_slice(&[0x08, 0x01, 0x10, 0x02]); // dummy proto header
        frame.extend_from_slice(&[0u8; 16]); // old server body (total >= 20 bytes)
        let mut ctx = NetPacketContext::new(frame, NetPacketDirection::Recv, app_a).unwrap();
        handle_recv_client_get_user_stats_response_ipc(&mut ctx).unwrap();

        assert_eq!(held_achievement_count(app_a), 0);
        assert_eq!(&ctx.frame_buffer[12..], proto_a.as_slice());

        // Live inbound buffer delivery drains app_b into write_buf
        let mut legacy_buf = vec![0u8; 32];
        handle_recv_client_get_user_stats_response(&mut legacy_buf, app_b);
        assert_eq!(held_achievement_count(app_b), 0);
        assert_eq!(&legacy_buf[12..], proto_b.as_slice());

        // Callback received both deliveries
        {
            let observed = delivered_cb.lock().unwrap();
            assert_eq!(observed.len(), 2);
            assert_eq!(observed[0], (app_a, proto_a));
            assert_eq!(observed[1], (app_b, proto_b));
        }

        // Wildcard drain (app_id == 0) returns all remaining queued apps
        queue_held_user_ach(app_a, b"tail-a");
        queue_held_user_ach(app_b, b"tail-b");
        let all = drain_achievements(0, std::ptr::null_mut(), false);
        assert_eq!(
            all,
            vec![(app_a, b"tail-a".to_vec()), (app_b, b"tail-b".to_vec())]
        );

        {
            let observed = delivered_cb.lock().unwrap();
            assert_eq!(observed.len(), 4);
        }

        set_achievement_delivery_callback(None);
    }
}
