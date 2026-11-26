//! Steam client integration shared library (`libsteam_integration.so`).
//!
//! Loaded into the Steam client process via `LD_PRELOAD`. Its constructor first verifies
//! that the host process is actually the main Steam client (`steam`, excluding
//! `steamwebhelper`, shells, wrappers, and helper tools), installs a diagnostic panic hook
//! with `catch_unwind` containment so no fault can ever `SIGABRT` Steam, starts the Unix
//! domain socket IPC server, loads persisted LuaTools overrides & `.lua` manifests,
//! resolves ASLR runtime addresses in `steamclient.so` via `/proc/self/maps`, and
//! installs 64-bit indirect memory patches with safe no-op fallback when patterns are absent.

#![allow(clippy::missing_safety_doc)]

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::raw::{c_uint, c_void};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Once, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ctor::ctor;
use luatools_linux::parsers::lua_parser::LuaFileParser;
use luatools_linux::services::manifest_binding_service::ManifestPin;
use luatools_linux::services::slssteam_service::SlssteamService;
use luatools_linux::services::steam_bridge_service::BridgeOverrides;
use luatools_linux::steam_bridge::{
    binary_reader::{
        find_loaded_module_mapping, scan_steam_binaries, BinaryFile, LoadedModuleMapping,
    },
    cloud_redirect::{AchievementBlock, CloudRedirectHost},
    credentials::{
        appticket::{
            read_app_ticket_from, remove_app_ticket_from, write_app_ticket_to, write_e_ticket_to,
        },
        eticketclient::{self, EticketClient},
        legacycdkey::{self, LegacyCdKeyStore},
    },
    denuvo_auth::{DenuvoAuth, DenuvoAuthConfig},
    interop::{
        interop_callbacks, interop_decryption, interop_ipc,
        interop_ipc_isteamuser::{
            handler_post_get_app_ownership_ticket, handler_post_get_encrypted_app_ticket,
            handler_post_get_steam_id, handler_post_request_encrypted_app_ticket,
            install_iclientuser_handlers,
        },
        interop_ipc_isteamutils::{
            install_iclientutils_handlers, record_encrypted_ticket, take_encrypted_ticket,
        },
        interop_keyvalues, interop_misc, interop_package, interop_steamui,
        ipc as interop_wire_ipc,
    },
    ipc::{self, IpcServer},
    manifest_hooks::{
        apply_depot_overrides, ensure_manifest_cached, get_manifest_overrides,
        install_manifest_hooks, parse_pattern_and_mask, remove_manifest_override,
        set_manifest_override, DepotEntry, ManifestOverride,
    },
    memory_patch::{self, MemoryPatch, JMP_ABS64_LEN},
    netpacket::{
        install_cloud_hooks, install_eticket_hooks, install_licenses_hooks, install_manifestprobe_hooks,
        install_netpacket_core, install_ownershipticket_hooks, install_userstats_hooks,
        netpacket_eticket::parse_eticket_steam_id,
        netpacket_manifestprobe::expire_manifest_probe_stale,
        netpacket_ownershipticket::parse_ownershipticket_steam_id,
        userstats::{drain_achievements, fill_user_achievements_from_dir, queue_held_user_ach},
    },
    ost_platform::{
        bkv_parser::{extract_achievements, find_user_game_stats_file, parse_bkv_file},
        dynamic_library, hash, memory, steam_credential_store, thread as ost_thread, trap,
    },
    pattern_db::{BinaryComponent, PatternChannel, PatternDb},
    preload_loader::is_steam_client_process,
    ticket_store::{AppTicketData, TicketStore},
};

const MAX_TICKET_BUFFER_BYTES: usize = 64 * 1024;
const PANIC_LOG_TMP_PATH: &str = "/tmp/steam_integration_panic.log";

type BisSubscribedAppFn = unsafe extern "C" fn(*mut c_void, u32) -> bool;
type GetAppIdFn = unsafe extern "C" fn(*mut c_void) -> u32;
type GetAppOwnershipTicketFn = unsafe extern "C" fn(*mut c_void, u32, *mut c_void, u32) -> bool;
type RequestEncryptedAppTicketFn =
    unsafe extern "C" fn(*mut c_void, u32, *const c_void, u32) -> u64;
type GetEncryptedAppTicketFn =
    unsafe extern "C" fn(*mut c_void, u64, *mut c_void, *mut u32) -> bool;
type GetSteamIDFn = unsafe extern "C" fn(*mut c_void) -> u64;
type DepotKeyIntegrationFn = unsafe extern "C" fn(*mut c_void, u32, *mut u8, u32) -> bool;

static SPOOFED_APPS: OnceLock<RwLock<HashSet<u32>>> = OnceLock::new();
static APP_ID_SPOOFS: OnceLock<RwLock<HashMap<u32, u32>>> = OnceLock::new();
static DEPOT_KEYS: OnceLock<RwLock<HashMap<u32, String>>> = OnceLock::new();
static STATS: OnceLock<RwLock<HashMap<(u32, String), String>>> = OnceLock::new();
static LEGACY_CDKEYS: OnceLock<RwLock<HashMap<u32, String>>> = OnceLock::new();
static MANIFEST_PINS: OnceLock<RwLock<HashMap<(u32, u32), String>>> = OnceLock::new();
static MANIFEST_CODES: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();
static PENDING_ETICKETS: OnceLock<RwLock<HashMap<u64, u32>>> = OnceLock::new();
static CACHED_TICKETS: OnceLock<RwLock<HashMap<u32, AppTicketData>>> = OnceLock::new();
static NEXT_CALL_HANDLE: AtomicU64 = AtomicU64::new(0x4C55_4154_0000_0001);

/// Pending depot additions from Lua config hot-reload
static PENDING_ADDITIONS: OnceLock<RwLock<Vec<u32>>> = OnceLock::new();
/// Pending depot removals from Lua config hot-reload
static PENDING_REMOVALS: OnceLock<RwLock<Vec<u32>>> = OnceLock::new();

static ORIGINAL_BIS: OnceLock<RwLock<Option<BisSubscribedAppFn>>> = OnceLock::new();
static ORIGINAL_GET_APP_ID: OnceLock<RwLock<Option<GetAppIdFn>>> = OnceLock::new();
static ORIGINAL_GET_APP_OWNERSHIP_TICKET: OnceLock<RwLock<Option<GetAppOwnershipTicketFn>>> =
    OnceLock::new();
static ORIGINAL_REQUEST_ENCRYPTED_APP_TICKET: OnceLock<
    RwLock<Option<RequestEncryptedAppTicketFn>>,
> = OnceLock::new();
static ORIGINAL_GET_ENCRYPTED_APP_TICKET: OnceLock<RwLock<Option<GetEncryptedAppTicketFn>>> =
    OnceLock::new();
static ORIGINAL_GET_STEAM_ID: OnceLock<RwLock<Option<GetSteamIDFn>>> = OnceLock::new();
static ORIGINAL_DEPOT_DECRYPT: OnceLock<RwLock<Option<DepotKeyIntegrationFn>>> = OnceLock::new();

static INSTALLED_PATCHES: OnceLock<RwLock<Vec<MemoryPatch>>> = OnceLock::new();
static DENUVO_AUTH: OnceLock<RwLock<DenuvoAuth>> = OnceLock::new();
static CLOUD_REDIRECT: OnceLock<CloudRedirectHost> = OnceLock::new();

fn read_lock<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|e| e.into_inner())
}

fn write_lock<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|e| e.into_inner())
}

fn denuvo_auth() -> &'static RwLock<DenuvoAuth> {
    DENUVO_AUTH.get_or_init(|| RwLock::new(DenuvoAuth::new()))
}

fn cloud_redirect() -> &'static CloudRedirectHost {
    CLOUD_REDIRECT.get_or_init(CloudRedirectHost::new)
}

fn spoofed_apps() -> &'static RwLock<HashSet<u32>> {
    SPOOFED_APPS.get_or_init(|| RwLock::new(HashSet::new()))
}

fn app_id_spoofs() -> &'static RwLock<HashMap<u32, u32>> {
    APP_ID_SPOOFS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn depot_keys() -> &'static RwLock<HashMap<u32, String>> {
    DEPOT_KEYS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn stats() -> &'static RwLock<HashMap<(u32, String), String>> {
    STATS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn legacy_cdkeys() -> &'static RwLock<HashMap<u32, String>> {
    LEGACY_CDKEYS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn manifest_pins() -> &'static RwLock<HashMap<(u32, u32), String>> {
    MANIFEST_PINS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn manifest_codes() -> &'static RwLock<HashMap<String, String>> {
    MANIFEST_CODES.get_or_init(|| RwLock::new(HashMap::new()))
}

fn pending_etickets() -> &'static RwLock<HashMap<u64, u32>> {
    PENDING_ETICKETS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn cached_tickets() -> &'static RwLock<HashMap<u32, AppTicketData>> {
    CACHED_TICKETS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn pending_additions() -> &'static RwLock<Vec<u32>> {
    PENDING_ADDITIONS.get_or_init(|| RwLock::new(Vec::new()))
}

fn pending_removals() -> &'static RwLock<Vec<u32>> {
    PENDING_REMOVALS.get_or_init(|| RwLock::new(Vec::new()))
}

fn original_bis() -> &'static RwLock<Option<BisSubscribedAppFn>> {
    ORIGINAL_BIS.get_or_init(|| RwLock::new(None))
}

fn original_get_app_id() -> &'static RwLock<Option<GetAppIdFn>> {
    ORIGINAL_GET_APP_ID.get_or_init(|| RwLock::new(None))
}

fn original_get_app_ownership_ticket() -> &'static RwLock<Option<GetAppOwnershipTicketFn>> {
    ORIGINAL_GET_APP_OWNERSHIP_TICKET.get_or_init(|| RwLock::new(None))
}

fn original_request_encrypted_app_ticket() -> &'static RwLock<Option<RequestEncryptedAppTicketFn>> {
    ORIGINAL_REQUEST_ENCRYPTED_APP_TICKET.get_or_init(|| RwLock::new(None))
}

fn original_get_encrypted_app_ticket() -> &'static RwLock<Option<GetEncryptedAppTicketFn>> {
    ORIGINAL_GET_ENCRYPTED_APP_TICKET.get_or_init(|| RwLock::new(None))
}

fn original_get_steam_id() -> &'static RwLock<Option<GetSteamIDFn>> {
    ORIGINAL_GET_STEAM_ID.get_or_init(|| RwLock::new(None))
}

fn original_depot_decrypt() -> &'static RwLock<Option<DepotKeyIntegrationFn>> {
    ORIGINAL_DEPOT_DECRYPT.get_or_init(|| RwLock::new(None))
}

fn installed_patches() -> &'static RwLock<Vec<MemoryPatch>> {
    INSTALLED_PATCHES.get_or_init(|| RwLock::new(Vec::new()))
}

fn decode_hex_bytes(hex: &str) -> Option<Vec<u8>> {
    let trimmed = hex.trim();
    if trimmed.is_empty() || !trimmed.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(trimmed.len() / 2);
    let bytes = trimmed.as_bytes();
    for chunk in bytes.as_chunks::<2>().0 {
        let s = std::str::from_utf8(chunk).ok()?;
        out.push(u8::from_str_radix(s, 16).ok()?);
    }
    Some(out)
}

fn synthesize_ownership_ticket(app_id: u32, steam_id: u64) -> Vec<u8> {
    let mut buf = vec![0u8; 64];
    buf[0..4].copy_from_slice(&4u32.to_le_bytes()); // ticket version 4
    buf[4..12].copy_from_slice(&steam_id.to_le_bytes());
    buf[12..16].copy_from_slice(&app_id.to_le_bytes());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32;
    buf[16..20].copy_from_slice(&now.to_le_bytes());
    buf[20..24].copy_from_slice(&(now.saturating_add(86_400 * 30)).to_le_bytes());
    buf
}

fn frame_ticket_for_parser(ticket_bytes: &[u8]) -> Option<Vec<u8>> {
    let len_u32 = u32::try_from(ticket_bytes.len()).ok()?;
    let mut framed = Vec::with_capacity(16 + ticket_bytes.len());
    framed.extend_from_slice(&[0u8; 12]);
    framed.extend_from_slice(&len_u32.to_le_bytes());
    framed.extend_from_slice(ticket_bytes);
    Some(framed)
}

fn extract_ownershipticket_steam_id(ticket_bytes: &[u8]) -> Option<u64> {
    let framed = frame_ticket_for_parser(ticket_bytes)?;
    parse_ownershipticket_steam_id(&framed)
}

fn extract_eticket_steam_id(ticket_bytes: &[u8]) -> Option<u64> {
    let framed = frame_ticket_for_parser(ticket_bytes)?;
    parse_eticket_steam_id(&framed)
}

fn resolve_spoof_steam_id(app_id: Option<u32>) -> Option<u64> {
    let cache = read_lock(cached_tickets());
    if let Some(aid) = app_id {
        if let Some(sid) = cache
            .get(&aid)
            .and_then(|t| t.steam_id.as_deref())
            .and_then(|s| s.parse::<u64>().ok())
        {
            if sid >= 0x0110_0001_0000_0000 {
                return Some(sid);
            }
        }
        if let Some(pool_sid) = read_lock(denuvo_auth()).get_pool_steam_id(aid) {
            if pool_sid >= 0x0110_0001_0000_0000 {
                return Some(pool_sid);
            }
        }
    }
    for data in cache.values() {
        if let Some(sid) = data.steam_id.as_deref().and_then(|s| s.parse::<u64>().ok()) {
            if sid >= 0x0110_0001_0000_0000 {
                return Some(sid);
            }
        }
    }
    None
}

unsafe extern "C" fn bis_subscribed_app_replacement(this: *mut c_void, app_id: u32) -> bool {
    panic::catch_unwind(AssertUnwindSafe(|| {
        if read_lock(spoofed_apps()).contains(&app_id)
            || read_lock(denuvo_auth()).is_denuvo_game(app_id)
        {
            return true;
        }
        let orig_opt = *read_lock(original_bis());
        if let Some(original) = orig_opt {
            return original(this, app_id);
        }
        false
    }))
    .unwrap_or(false)
}

unsafe extern "C" fn get_app_id_replacement(this: *mut c_void) -> u32 {
    panic::catch_unwind(AssertUnwindSafe(|| {
        let orig_opt = *read_lock(original_get_app_id());
        if let Some(original) = orig_opt {
            let real_id = original(this);
            if let Some(&spoof_id) = read_lock(app_id_spoofs()).get(&real_id) {
                return spoof_id;
            }
            return real_id;
        }

        if let Some((&_, &spoof_id)) = read_lock(app_id_spoofs()).iter().next() {
            return spoof_id;
        }
        0
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn get_app_ownership_ticket_replacement(
    this: *mut c_void,
    app_id: u32,
    ticket_buffer: *mut c_void,
    ticket_buffer_size: u32,
) -> bool {
    panic::catch_unwind(AssertUnwindSafe(|| {
        let cached_has_app = read_lock(cached_tickets()).contains_key(&app_id);
        let disk_ticket = if !cached_has_app {
            read_app_ticket_from(&find_steam_root().join("config"), app_id)
                .ok()
                .flatten()
        } else {
            None
        };
        let is_spoofed = read_lock(spoofed_apps()).contains(&app_id)
            || cached_has_app
            || disk_ticket.is_some();

        if is_spoofed {
            let ticket_bytes = read_lock(cached_tickets())
                .get(&app_id)
                .and_then(|d| d.app_ticket.as_deref())
                .or_else(|| disk_ticket.as_ref().and_then(|t| t.app_ticket.as_deref()))
                .and_then(|raw| decode_hex_bytes(raw).or_else(|| Some(raw.as_bytes().to_vec())))
                .unwrap_or_else(|| {
                    let sid =
                        resolve_spoof_steam_id(Some(app_id)).unwrap_or(76_561_197_960_287_930);
                    synthesize_ownership_ticket(app_id, sid)
                });
            let _ = extract_ownershipticket_steam_id(&ticket_bytes);

            if !ticket_buffer.is_null() && ticket_buffer_size > 0 {
                let cap = (ticket_buffer_size as usize).min(MAX_TICKET_BUFFER_BYTES);
                let dst = std::slice::from_raw_parts_mut(ticket_buffer as *mut u8, cap);
                dst.fill(0);
                let n = cap.min(ticket_bytes.len());
                dst[..n].copy_from_slice(&ticket_bytes[..n]);
            }
            return true;
        }

        let orig_opt = *read_lock(original_get_app_ownership_ticket());
        if let Some(original) = orig_opt {
            return original(this, app_id, ticket_buffer, ticket_buffer_size);
        }
        false
    }))
    .unwrap_or(false)
}

unsafe extern "C" fn request_encrypted_app_ticket_replacement(
    this: *mut c_void,
    app_id: u32,
    user_data: *const c_void,
    user_data_size: u32,
) -> u64 {
    panic::catch_unwind(AssertUnwindSafe(|| {
        let is_spoofed = read_lock(spoofed_apps()).contains(&app_id)
            || read_lock(cached_tickets()).contains_key(&app_id)
            || read_lock(denuvo_auth()).is_denuvo_game(app_id);
        if is_spoofed {
            let nonce: &[u8] = if !user_data.is_null() && user_data_size > 0 {
                let len = (user_data_size as usize).min(4096);
                if memory::is_readable(user_data, len) {
                    std::slice::from_raw_parts(user_data as *const u8, len)
                } else {
                    &[]
                }
            } else {
                &[]
            };
            let sid = resolve_spoof_steam_id(Some(app_id)).unwrap_or(76_561_197_960_287_930);
            let _ = EticketClient::fetch_fresh_eticket_from(
                &find_steam_root().join("config"),
                app_id,
                nonce,
                sid,
            );

            let handle = NEXT_CALL_HANDLE.fetch_add(1, Ordering::Relaxed);
            write_lock(pending_etickets()).insert(handle, app_id);
            record_encrypted_ticket(handle, app_id);
            return handle;
        }

        let orig_opt = *read_lock(original_request_encrypted_app_ticket());
        if let Some(original) = orig_opt {
            return original(this, app_id, user_data, user_data_size);
        }
        0
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn get_encrypted_app_ticket_replacement(
    this: *mut c_void,
    call_handle: u64,
    ticket_buffer: *mut c_void,
    ticket_size: *mut u32,
) -> bool {
    panic::catch_unwind(AssertUnwindSafe(|| {
        let utils_app = {
            let id = take_encrypted_ticket(call_handle);
            (id != 0).then_some(id)
        };
        let app_opt = write_lock(pending_etickets())
            .remove(&call_handle)
            .or(utils_app)
            .or_else(|| {
                read_lock(cached_tickets())
                    .keys()
                    .next()
                    .copied()
                    .filter(|_| (call_handle & 0xFFFF_FFFF_0000_0000) == 0x4C55_4154_0000_0000)
            });

        if let Some(app_id) = app_opt {
            let sid = resolve_spoof_steam_id(Some(app_id)).unwrap_or(76_561_197_960_287_930);
            let fresh_cached = EticketClient::get_cached_fresh_eticket(app_id).inspect(|_| {
                EticketClient::clear_cached_fresh_eticket(app_id);
            });
            let eticket_bytes = fresh_cached
                .or_else(|| {
                    read_lock(cached_tickets())
                        .get(&app_id)
                        .and_then(|d| d.e_ticket.as_deref())
                        .and_then(|raw| {
                            decode_hex_bytes(raw).or_else(|| Some(raw.as_bytes().to_vec()))
                        })
                })
                .or_else(|| {
                    let fetched = EticketClient::fetch_fresh_eticket_from(
                        &find_steam_root().join("config"),
                        app_id,
                        &[],
                        sid,
                    );
                    if fetched.is_some() {
                        EticketClient::clear_cached_fresh_eticket(app_id);
                    }
                    fetched
                })
                .or_else(|| {
                    read_lock(denuvo_auth())
                        .fetch_auth_ticket(app_id, &[])
                        .filter(|res| res.success)
                        .and_then(|res| res.ticket)
                })
                .unwrap_or_else(|| synthesize_ownership_ticket(app_id, sid));
            let _ = extract_eticket_steam_id(&eticket_bytes);

            if !ticket_size.is_null() {
                let max_cap = (*ticket_size as usize).min(MAX_TICKET_BUFFER_BYTES);
                let written = if !ticket_buffer.is_null() && max_cap > 0 {
                    let n = max_cap.min(eticket_bytes.len());
                    std::ptr::copy_nonoverlapping(
                        eticket_bytes.as_ptr(),
                        ticket_buffer as *mut u8,
                        n,
                    );
                    n
                } else {
                    eticket_bytes.len()
                };
                *ticket_size = written as u32;
            }
            return true;
        }

        let orig_opt = *read_lock(original_get_encrypted_app_ticket());
        if let Some(original) = orig_opt {
            return original(this, call_handle, ticket_buffer, ticket_size);
        }
        false
    }))
    .unwrap_or(false)
}

unsafe extern "C" fn get_steam_id_replacement(this: *mut c_void) -> u64 {
    panic::catch_unwind(AssertUnwindSafe(|| {
        if let Some(sid) = resolve_spoof_steam_id(None) {
            return sid;
        }
        let orig_opt = *read_lock(original_get_steam_id());
        if let Some(original) = orig_opt {
            return original(this);
        }
        0
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn depot_decrypt_replacement(
    this: *mut c_void,
    depot_id: u32,
    buffer: *mut u8,
    size: u32,
) -> bool {
    panic::catch_unwind(AssertUnwindSafe(|| {
        eprintln!(
            "[steam_integration] depot_decrypt_replacement called: depot_id={}, buffer={:p}, size={}",
            depot_id, buffer, size
        );
        let key_bytes_opt = read_lock(depot_keys())
            .get(&depot_id)
            .and_then(|hex| decode_hex_bytes(hex))
            .filter(|b| b.len() == 32);

        let orig_opt = *read_lock(original_depot_decrypt());
        if let Some(original) = orig_opt {
            if original(this, depot_id, buffer, size) {
                eprintln!("[steam_integration] depot_decrypt: original returned true for depot {}", depot_id);
                return true;
            }
            eprintln!("[steam_integration] depot_decrypt: original returned false for depot {}", depot_id);
        }

        if let Some(key_bytes) = key_bytes_opt {
            if !buffer.is_null() && size >= 32 {
                std::ptr::copy_nonoverlapping(key_bytes.as_ptr(), buffer, 32);
                eprintln!(
                    "[steam_integration] depot_decrypt: provided key for depot {} ({} bytes)",
                    depot_id, key_bytes.len()
                );
                return true;
            }
            eprintln!(
                "[steam_integration] depot_decrypt: buffer too small or null for depot {} (size={}, buffer={:p})",
                depot_id, size, buffer
            );
        } else {
            eprintln!("[steam_integration] depot_decrypt: no key configured for depot {}", depot_id);
        }

        false
    }))
    .unwrap_or(false)
}

fn append_panic_log(entry: &str) {
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(PANIC_LOG_TMP_PATH)
    {
        let _ = writeln!(f, "{}", entry);
    }
    if let Some(home) = dirs::home_dir() {
        let telemetry_dir = home.join(".local/share/luatools/telemetry");
        if fs::create_dir_all(&telemetry_dir).is_ok() {
            let log_path = telemetry_dir.join("steam_integration_panic.log");
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&log_path) {
                let _ = writeln!(f, "{}", entry);
            }
        }
    }
}

fn install_panic_hook() {
    panic::set_hook(Box::new(|info| {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let pid = std::process::id();
        let thread_name = thread::current().name().unwrap_or("unnamed").to_string();
        let entry = format!(
            "[ts={} pid={} thread={} v={}] [steam_integration] PANIC: {}",
            ts,
            pid,
            thread_name,
            env!("CARGO_PKG_VERSION"),
            info
        );
        eprintln!("{}", entry);
        append_panic_log(&entry);
    }));
}

/// Only activate inside the actual main Steam client process (`steam`).
/// Keeps `libsteam_integration.so` completely dormant in `steamwebhelper`, `sh`/`bash`,
/// `luatools`, `srt-bwrap`, `reaper`, and child games so `LD_PRELOAD` inheritance never
/// interferes with CEF/Chromium or shell helpers.
fn should_activate_in_current_process() -> bool {
    if std::env::var("LUATOOLS_BRIDGE_DISABLE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return false;
    }
    if std::env::var("LUATOOLS_BRIDGE_FORCE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return true;
    }

    let comm = fs::read_to_string("/proc/self/comm").unwrap_or_default();
    let exe = trap::get_main_executable_path().ok();
    is_steam_client_process(&comm, exe.as_deref())
}

static INIT: Once = Once::new();

fn integration_worker_entry() -> c_uint {
    let _worker_tid = ost_thread::current_native_thread_handle();
    let res = panic::catch_unwind(AssertUnwindSafe(run_integration));
    match res {
        Ok(Ok(())) => 0,
        Ok(Err(e)) => {
            eprintln!("[steam_integration] init error: {}", e);
            1
        }
        Err(_) => {
            eprintln!(
                "[steam_integration] recovered from unexpected panic; Steam continues unaffected"
            );
            1
        }
    }
}

fn libsteam_integration_init() {
    if !should_activate_in_current_process() {
        return;
    }

    INIT.call_once(|| {
        install_panic_hook();
        let _caller_tid = ost_thread::current_native_thread_handle();
        if !ost_thread::start_detached_thread(integration_worker_entry) {
            let _ = thread::Builder::new()
                .name("luatools-bridge".into())
                .spawn(|| {
                    let _ = integration_worker_entry();
                });
        }
    });
}

fn sync_to_slssteam_config<F: FnOnce(&mut SlssteamService)>(modify: F) {
    let mut sls = SlssteamService::default();
    modify(&mut sls);
    let _ = sls.save();
}

fn load_persisted_state(steam_root: &Path) {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let data_dir = home.join(".local/share/luatools");

    // Clear existing maps on reload to avoid append-only retention of deleted entries
    write_lock(spoofed_apps()).clear();
    write_lock(app_id_spoofs()).clear();
    write_lock(legacy_cdkeys()).clear();
    write_lock(depot_keys()).clear();
    write_lock(stats()).clear();
    write_lock(manifest_pins()).clear();
    write_lock(cached_tickets()).clear();

    // 1. Load bridge_overrides.json
    if let Ok(raw) = fs::read_to_string(data_dir.join("bridge_overrides.json")) {
        if let Ok(ov) = serde_json::from_str::<BridgeOverrides>(&raw) {
            {
                let mut apps = write_lock(spoofed_apps());
                for aid in ov.redirect_apps {
                    apps.insert(aid);
                }
            }
            {
                let mut spoofs = write_lock(app_id_spoofs());
                for (aid, sid) in ov.app_id_spoofs {
                    spoofs.insert(aid, sid);
                }
            }
            {
                let mut cks = write_lock(legacy_cdkeys());
                for (aid, k) in ov.legacy_cd_keys {
                    cks.insert(aid, k);
                }
            }
            {
                let mut dks = write_lock(depot_keys());
                for (did, k) in ov.depot_keys {
                    dks.insert(did, k);
                }
            }
            {
                let mut st = write_lock(stats());
                for (aid, map) in ov.stats {
                    for (name, val) in map {
                        st.insert((aid, name), val);
                    }
                }
            }
        }
    }

    // 2. Load manifest_pins.json
    if let Ok(raw) = fs::read_to_string(data_dir.join("manifest_pins.json")) {
        if let Ok(pins) = serde_json::from_str::<Vec<ManifestPin>>(&raw) {
            let mut mp = write_lock(manifest_pins());
            for p in pins {
                mp.insert((p.app_id, p.depot_id), p.manifest_id);
            }
        }
    }

    // 3. Load from SLSsteam config.yaml (ManifestOverrides, DepotKeys, FakeAppIds, AdditionalApps, AppTokens)
    let sls = SlssteamService::default();
    if sls.config_path.is_file() {
        {
            let mut mp = write_lock(manifest_pins());
            for (&did, gid) in &sls.config.manifest_overrides {
                mp.insert((0, did), gid.clone());
            }
        }
        {
            let mut dks = write_lock(depot_keys());
            for (&did, key) in &sls.config.depot_keys {
                dks.insert(did, key.clone());
            }
        }
        {
            let mut spoofs = write_lock(app_id_spoofs());
            for (&aid, &did) in &sls.config.fake_app_ids {
                spoofs.insert(aid, did);
            }
        }
        {
            let mut apps = write_lock(spoofed_apps());
            for &aid in &sls.config.additional_apps {
                apps.insert(aid);
            }
        }
        {
            let mut ct = write_lock(cached_tickets());
            for (&aid, tok) in &sls.config.app_tokens {
                ct.entry(aid).or_insert_with(|| AppTicketData {
                    app_id: aid,
                    steam_id: None,
                    app_ticket: Some(tok.clone()),
                    e_ticket: None,
                    last_update: None,
                });
            }
        }
    }

    // 4. Import stplug-in/*.lua entitlements, depot decryption keys, active manifest pins, and tokens
    let stplug_dir = steam_root.join("config/stplug-in");
    if let Ok(entries) = fs::read_dir(&stplug_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) != Some("lua") {
                continue;
            }
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let base_id = stem
                .split('_')
                .next()
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            if let Ok(txt) = fs::read_to_string(&p) {
                let parsed = LuaFileParser::parse_text(&txt, base_id);
                let mut lua_cached_entries = Vec::new();
                if base_id > 0 {
                    write_lock(spoofed_apps()).insert(base_id);
                    lua_cached_entries.push(CachedLuaEntry {
                        depot_id: base_id,
                        os: None,
                        arch: None,
                        proton: None,
                        native: false,
                    });
                }
                for item in parsed.entries {
                    if item.has_key() {
                        if let Some(k) = item.key {
                            write_lock(depot_keys()).insert(item.id, k);
                        }
                    } else if item.id > 0 {
                        write_lock(spoofed_apps()).insert(item.id);
                    }
                    // Collect all depot IDs from Lua entries with platform info
                    if item.id > 0 {
                        lua_cached_entries.push(CachedLuaEntry {
                            depot_id: item.id,
                            os: item.os.clone(),
                            arch: item.arch.clone(),
                            proton: item.proton.clone(),
                            native: item.native,
                        });
                    }
                    if let Some(mid) = item.manifest_id {
                        if base_id > 0 {
                            write_lock(manifest_pins()).insert((base_id, item.id), mid);
                        }
                    }
                }
                // Update cached Lua entries for get_all_depot_ids
                if !lua_cached_entries.is_empty() {
                    update_cached_lua_entries(lua_cached_entries);
                }
                for (aid, tok) in parsed.tokens {
                    write_lock(cached_tickets()).entry(aid).or_insert_with(|| AppTicketData {
                        app_id: aid,
                        steam_id: None,
                        app_ticket: Some(tok),
                        e_ticket: None,
                        last_update: None,
                    });
                }
            }
        }
    }

    // 5. Load TicketStore cache from <steam_root>/config/localconfig.vdf
    let config_dir = steam_root.join("config");
    steam_credential_store::initialize(&config_dir);
    let _ = eticketclient::init();

    let mut store = TicketStore::new(&config_dir);
    if store.load().is_ok() {
        if let Ok(ids) = store.list_app_ids() {
            let mut ct = write_lock(cached_tickets());
            for aid in ids {
                if let Some(ticket) = read_app_ticket_from(&config_dir, aid).ok().flatten() {
                    ct.insert(
                        aid,
                        AppTicketData {
                            app_id: ticket.app_id,
                            steam_id: ticket.steam_id,
                            app_ticket: ticket.app_ticket,
                            e_ticket: ticket.e_ticket,
                            last_update: None,
                        },
                    );
                } else if let Ok(data) = store.get(aid) {
                    ct.insert(aid, data);
                }
            }
        }
    }

    // 6. Sync LegacyCdKeyStore overrides from <steam_root>/config/localconfig.vdf
    let mut cdkey_store = LegacyCdKeyStore::new(&config_dir);
    if cdkey_store.load().is_ok() {
        if let Ok(ids) = cdkey_store.list_app_ids() {
            let mut cks = write_lock(legacy_cdkeys());
            for aid in ids {
                if let Ok(entry) = cdkey_store.get(aid) {
                    if let Some(ref key) = entry.override_key {
                        cks.insert(aid, key.clone());
                    }
                }
            }
        }
    }

    // 7. Sync manifest pins into manifest_hooks and verify pre-seeded cache files
    let existing_hook_keys: Vec<(u32, u32)> = get_manifest_overrides().keys().copied().collect();
    for (aid, did) in existing_hook_keys {
        remove_manifest_override(aid, did);
    }
    let pin_snapshot: Vec<((u32, u32), String)> = read_lock(manifest_pins())
        .iter()
        .map(|(&k, v)| (k, v.clone()))
        .collect();
    for ((aid, did), gid_str) in pin_snapshot {
        if let Ok(gid) = gid_str.trim().parse::<u64>() {
            set_manifest_override(aid, did, ManifestOverride::new(gid, None));
            let _ = ensure_manifest_cached(steam_root, aid, did, gid);
        }
    }

    // 8. Sync DenuvoAuth & InteropMisc game titles / online-fix app spoofing from SLSsteam config
    let pool_sid = sls
        .config
        .steam_id_override
        .or_else(|| sls.config.denuvo_games.keys().next().copied())
        .or_else(|| resolve_spoof_steam_id(None));
    {
        let mut dauth = write_lock(denuvo_auth());
        dauth.configure(DenuvoAuthConfig {
            enabled: !sls.config.denuvo_games.is_empty() || !sls.config.force_denuvo.is_empty(),
            pool_steam_id: pool_sid,
            ..Default::default()
        });
        for &aid in &sls.config.force_denuvo {
            dauth.register_denuvo_app(aid);
        }
        for aids in sls.config.denuvo_games.values() {
            for &aid in aids {
                dauth.register_denuvo_app(aid);
            }
        }
    }
    for (&aid, title) in &sls.config.game_titles {
        interop_misc::set_game_name_by_app_id(aid, title.clone());
    }
    let online_fix_real = read_lock(app_id_spoofs())
        .iter()
        .find(|(_, &spoof_id)| spoof_id == interop_misc::ONLINE_FIX_APP_ID)
        .map(|(&real_id, _)| real_id)
        .unwrap_or(0);
    interop_misc::set_online_fix_real_app_id(online_fix_real);

    // 9. Sync CloudRedirectHost, local BKV achievements, and queued UserStats
    sync_cloud_redirect_and_achievements(steam_root);
}

fn sync_cloud_redirect_and_achievements(steam_root: &Path) {
    let config_dir = steam_root.join("config");
    let userdata_dir = steam_root.join("userdata");
    let apps: Vec<u32> = read_lock(spoofed_apps()).iter().copied().collect();
    let has_stats = !read_lock(stats()).is_empty();

    let host = cloud_redirect();
    host.initialize(steam_root, true);
    host.sync_app_set(apps.clone());
    host.enable_stats_sync(has_stats);

    let mut account_id = 0u32;
    let mut universe = String::new();
    if steam_credential_store::get_active_user(&mut account_id, &mut universe)
        == steam_credential_store::Status::Ok
        && account_id != 0
    {
        host.set_account_id(account_id);
    }

    for aid in apps {
        if account_id != 0 {
            if let Some(stats_path) = find_user_game_stats_file(&config_dir, account_id, aid) {
                if let Ok(bkv) = parse_bkv_file(&stats_path) {
                    let extracted = extract_achievements(&bkv);
                    if !extracted.is_empty() {
                        let blocks: Vec<AchievementBlock> = extracted
                            .into_values()
                            .map(|a| AchievementBlock {
                                name: a.name,
                                state: u8::from(a.unlocked),
                                unlock_time: a.unlock_time.unwrap_or(0),
                            })
                            .collect();
                        host.set_achievements(aid, blocks);
                    }
                }
            }
        }

        let mut ach_buf = Vec::new();
        let mut unlocked = 0u32;
        fill_user_achievements_from_dir(&userdata_dir, aid, &mut ach_buf, &mut unlocked);
        if !ach_buf.is_empty() {
            queue_held_user_ach(aid, &ach_buf);
        }
    }
}

/// Polls `/proc/self/maps` briefly for `steamclient.so` to be `dlopen`ed by Steam.
fn wait_for_steamclient_mapping() -> Option<LoadedModuleMapping> {
    if let Some(m) = find_loaded_module_mapping("steamclient.so") {
        return Some(m);
    }
    if std::env::var("LUATOOLS_BRIDGE_NO_WAIT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return None;
    }
    for _ in 0..10 {
        thread::sleep(Duration::from_millis(500));
        if let Some(m) = find_loaded_module_mapping("steamclient.so") {
            return Some(m);
        }
    }
    None
}

fn try_install_steamclient_patches(steam_root: &Path) {
    let binaries = scan_steam_binaries(steam_root);
    let steam_client: Option<BinaryFile> = binaries.into_iter().find(|b| {
        b.path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name == "steamclient.so")
    });

    let Some(client) = steam_client else {
        eprintln!(
            "[steam_integration] steamclient.so not found under {}; running in IPC no-op mode",
            steam_root.display()
        );
        return;
    };

    let Some(mapping) = wait_for_steamclient_mapping() else {
        eprintln!(
            "[steam_integration] steamclient.so is not mapped in this process; skipping memory patches (IPC active)"
        );
        return;
    };

    let _loaded_steamclient = dynamic_library::get_loaded("steamclient.so");

    if let Err(e) = install_manifest_hooks(steam_root, &mapping) {
        eprintln!(
            "[steam_integration] manifest_hooks non-fatal fallback: {}",
            e
        );
    }
    if let Err(e) = interop_ipc::install() {
        eprintln!(
            "[steam_integration] InteropIPC non-fatal fallback: {}",
            e
        );
    }
    let _ = install_iclientuser_handlers();
    let _ = install_iclientutils_handlers();
    if let Err(e) = interop_misc::install(steam_root) {
        eprintln!(
            "[steam_integration] InteropMisc non-fatal fallback: {}",
            e
        );
    }
    let _ = install_eticket_hooks();
    let _ = install_licenses_hooks();
    let _ = install_manifestprobe_hooks();
    let _ = install_ownershipticket_hooks();
    let _ = install_userstats_hooks();
    let _ = install_cloud_hooks();
    if let Err(e) = interop_callbacks::install(steam_root) {
        eprintln!("[steam_integration] InteropCallbacks non-fatal fallback: {}", e);
    }
    if let Err(e) = interop_decryption::install(steam_root) {
        eprintln!("[steam_integration] InteropDecryption non-fatal fallback: {}", e);
    }
    if let Err(e) = interop_keyvalues::install(steam_root) {
        eprintln!("[steam_integration] InteropKeyValues non-fatal fallback: {}", e);
    }
    if let Err(e) = interop_package::install(steam_root) {
        eprintln!("[steam_integration] InteropPackage non-fatal fallback: {}", e);
    }
    if let Err(e) = interop_steamui::install(steam_root) {
        eprintln!("[steam_integration] InteropSteamUI non-fatal fallback: {}", e);
    }
    if let Err(e) = install_netpacket_core(steam_root) {
        eprintln!("[steam_integration] NetPacketCore non-fatal fallback: {}", e);
    }

    let pattern_db = PatternDb::new();
    let pf = match pattern_db.lookup_by_binary(
        &client,
        PatternChannel::Pattern,
        BinaryComponent::SteamClient,
    ) {
        Ok(pf) => pf,
        Err(e) => {
            let sha_prefix = client.sha256_hex().get(..12).unwrap_or(client.sha256_hex());
            eprintln!(
                "[steam_integration] no pattern file for steamclient SHA-256 {} ({}); running in safe no-op fallback mode",
                sha_prefix, e
            );
            return;
        }
    };

    let resolve_runtime_target = |key: &str| -> Option<u64> {
        let entry = pf.patterns.get(key)?;
        let vaddr = if let Some(pattern_hex) = entry.pattern.as_ref() {
            let (pattern_bytes, mask_bytes) =
                parse_pattern_and_mask(pattern_hex, entry.mask.as_deref())?;
            let pattern_offset = entry.offset.unwrap_or(0);
            client.resolve_pattern_address(
                &pattern_bytes,
                mask_bytes.as_deref(),
                pattern_offset,
                None,
            )?
        } else {
            entry.address?
        };

        let Some(runtime_addr) = client.vaddr_to_runtime_addr(vaddr, &mapping, JMP_ABS64_LEN)
        else {
            eprintln!(
                "[steam_integration] skipping '{}': ELF vaddr 0x{:x} does not map to an executable steamclient.so page (no-op fallback)",
                key, vaddr
            );
            return None;
        };
        Some(runtime_addr)
    };

    let mut installed_count = 0usize;

    if let Some(addr) = resolve_runtime_target("bis_subscribed_app") {
        if install_bis_subscribed_app_patch(addr) {
            installed_count += 1;
        }
    }
    if let Some(addr) = resolve_runtime_target("get_app_id") {
        if install_get_app_id_patch(addr) {
            installed_count += 1;
        }
    }
    if let Some(addr) = resolve_runtime_target("get_app_ownership_ticket") {
        if install_get_app_ownership_ticket_patch(addr) {
            installed_count += 1;
        }
    }
    if let Some(addr) = resolve_runtime_target("request_encrypted_app_ticket") {
        if install_request_encrypted_app_ticket_patch(addr) {
            installed_count += 1;
        }
    }
    if let Some(addr) = resolve_runtime_target("get_encrypted_app_ticket") {
        if install_get_encrypted_app_ticket_patch(addr) {
            installed_count += 1;
        }
    }
    if let Some(addr) = resolve_runtime_target("get_steam_id") {
        if install_get_steam_id_patch(addr) {
            installed_count += 1;
        }
    }
    let depot_addr = resolve_runtime_target("depot_key_integration")
        .or_else(|| resolve_runtime_target("depot_decrypt"));
    if let Some(addr) = depot_addr {
        eprintln!("[steam_integration] installing depot_decrypt hook at runtime addr 0x{:x}", addr);
        if install_depot_decrypt_patch(addr) {
            installed_count += 1;
            eprintln!("[steam_integration] depot_decrypt hook installed successfully");
        }
    } else {
        eprintln!(
            "[steam_integration] WARNING: depot_key_integration/depot_decrypt pattern not found in pattern DB; depot decryption hook NOT installed"
        );
    }

    eprintln!(
        "[steam_integration] memory integration ready ({} hook(s) active at steamclient base 0x{:x})",
        installed_count, mapping.base_addr
    );
}

fn run_integration() -> Result<(), String> {
    let steam_root = find_steam_root();
    if let Ok(exe_sha) = hash::sha256_of_current_exe() {
        let prefix = exe_sha.get(..12).unwrap_or(&exe_sha);
        eprintln!("[steam_integration] attached to host exe sha256={}...", prefix);
    }

    // Set target platform from environment variables (set by steam-sls wrapper)
    if let (Ok(os), Ok(arch)) = (
        std::env::var("LUATOOLS_TARGET_OS"),
        std::env::var("LUATOOLS_TARGET_ARCH"),
    ) {
        set_target_platform(os, arch);
        eprintln!("[steam_integration] target platform set to {}/{}",
            std::env::var("LUATOOLS_TARGET_OS").unwrap_or_default(),
            std::env::var("LUATOOLS_TARGET_ARCH").unwrap_or_default()
        );
    }

    load_persisted_state(&steam_root);

    // Start the IPC server first so the LuaTools GUI sees a live connection immediately,
    // even while waiting for steamclient.so or running offline in no-op patch mode.
    let mut server = IpcServer::new();
    server.set_handler(handle_ipc_message);
    if let Err(e) = server.start() {
        eprintln!("[steam_integration] IPC server unavailable: {}", e);
    } else {
        luatools_linux::steam_bridge::set_integrated();
    }

    try_install_steamclient_patches(&steam_root);

    loop {
        thread::park_timeout(Duration::from_secs(60));
        expire_manifest_probe_stale();
        read_lock(denuvo_auth()).cleanup_expired(86_400);
    }
}

fn dispatch_steam_ipc_buffer(read_buf: &[u8], write_buf: &mut Vec<u8>, pipe: *mut c_void) -> bool {
    interop_wire_ipc::dispatch_ipc_message(read_buf, write_buf, pipe)
}

fn install_bis_subscribed_app_patch(target_addr: u64) -> bool {
    let replacement_ptr = bis_subscribed_app_replacement as *const () as *mut u8;
    match unsafe { memory_patch::patch_by_offset(target_addr, 0, replacement_ptr) } {
        Ok(patch) => {
            unsafe {
                let trampoline: BisSubscribedAppFn = std::mem::transmute(patch.trampoline);
                *write_lock(original_bis()) = Some(trampoline);
            }
            write_lock(installed_patches()).push(patch);
            true
        }
        Err(e) => {
            eprintln!(
                "[steam_integration] failed to install BIsSubscribedApp patch: {}",
                e
            );
            false
        }
    }
}

fn install_get_app_id_patch(target_addr: u64) -> bool {
    let replacement_ptr = get_app_id_replacement as *const () as *mut u8;
    match unsafe { memory_patch::patch_by_offset(target_addr, 0, replacement_ptr) } {
        Ok(patch) => {
            unsafe {
                let trampoline: GetAppIdFn = std::mem::transmute(patch.trampoline);
                *write_lock(original_get_app_id()) = Some(trampoline);
            }
            write_lock(installed_patches()).push(patch);
            true
        }
        Err(e) => {
            eprintln!(
                "[steam_integration] failed to install GetAppId patch: {}",
                e
            );
            false
        }
    }
}

fn install_get_app_ownership_ticket_patch(target_addr: u64) -> bool {
    let replacement_ptr = get_app_ownership_ticket_replacement as *const () as *mut u8;
    match unsafe { memory_patch::patch_by_offset(target_addr, 0, replacement_ptr) } {
        Ok(patch) => {
            unsafe {
                let trampoline: GetAppOwnershipTicketFn = std::mem::transmute(patch.trampoline);
                *write_lock(original_get_app_ownership_ticket()) = Some(trampoline);
            }
            write_lock(installed_patches()).push(patch);
            true
        }
        Err(e) => {
            eprintln!(
                "[steam_integration] failed to install GetAppOwnershipTicket patch: {}",
                e
            );
            false
        }
    }
}

fn install_request_encrypted_app_ticket_patch(target_addr: u64) -> bool {
    let replacement_ptr = request_encrypted_app_ticket_replacement as *const () as *mut u8;
    match unsafe { memory_patch::patch_by_offset(target_addr, 0, replacement_ptr) } {
        Ok(patch) => {
            unsafe {
                let trampoline: RequestEncryptedAppTicketFn = std::mem::transmute(patch.trampoline);
                *write_lock(original_request_encrypted_app_ticket()) = Some(trampoline);
            }
            write_lock(installed_patches()).push(patch);
            true
        }
        Err(e) => {
            eprintln!(
                "[steam_integration] failed to install RequestEncryptedAppTicket patch: {}",
                e
            );
            false
        }
    }
}

fn install_get_encrypted_app_ticket_patch(target_addr: u64) -> bool {
    let replacement_ptr = get_encrypted_app_ticket_replacement as *const () as *mut u8;
    match unsafe { memory_patch::patch_by_offset(target_addr, 0, replacement_ptr) } {
        Ok(patch) => {
            unsafe {
                let trampoline: GetEncryptedAppTicketFn = std::mem::transmute(patch.trampoline);
                *write_lock(original_get_encrypted_app_ticket()) = Some(trampoline);
            }
            write_lock(installed_patches()).push(patch);
            true
        }
        Err(e) => {
            eprintln!(
                "[steam_integration] failed to install GetEncryptedAppTicket patch: {}",
                e
            );
            false
        }
    }
}

fn install_get_steam_id_patch(target_addr: u64) -> bool {
    let replacement_ptr = get_steam_id_replacement as *const () as *mut u8;
    match unsafe { memory_patch::patch_by_offset(target_addr, 0, replacement_ptr) } {
        Ok(patch) => {
            unsafe {
                let trampoline: GetSteamIDFn = std::mem::transmute(patch.trampoline);
                *write_lock(original_get_steam_id()) = Some(trampoline);
            }
            write_lock(installed_patches()).push(patch);
            true
        }
        Err(e) => {
            eprintln!(
                "[steam_integration] failed to install GetSteamID patch: {}",
                e
            );
            false
        }
    }
}

fn install_depot_decrypt_patch(target_addr: u64) -> bool {
    let replacement_ptr = depot_decrypt_replacement as *const () as *mut u8;
    match unsafe { memory_patch::patch_by_offset(target_addr, 0, replacement_ptr) } {
        Ok(patch) => {
            unsafe {
                let trampoline: DepotKeyIntegrationFn = std::mem::transmute(patch.trampoline);
                *write_lock(original_depot_decrypt()) = Some(trampoline);
            }
            write_lock(installed_patches()).push(patch);
            true
        }
        Err(e) => {
            eprintln!(
                "[steam_integration] failed to install DepotKeyIntegration patch: {}",
                e
            );
            false
        }
    }
}

fn find_steam_root() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let candidates = [home.join(".local/share/Steam"), home.join(".steam/steam")];
    for c in &candidates {
        if c.exists() {
            return c.clone();
        }
    }
    candidates[0].clone()
}

fn pong_ok() -> Option<ipc::IpcMessage> {
    Some(ipc::IpcMessage::Pong {
        active: true,
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

fn dispatch_iclientuser_post(
    method: &str,
    pipe: *mut c_void,
    read_buf: &mut [u8],
    write_buf: &mut [u8],
) -> bool {
    match method {
        "GetSteamID" => handler_post_get_steam_id(pipe, read_buf, write_buf),
        "GetAppOwnershipTicketExtendedData" => {
            handler_post_get_app_ownership_ticket(pipe, read_buf, write_buf)
        }
        "RequestEncryptedAppTicket" => {
            handler_post_request_encrypted_app_ticket(pipe, read_buf, write_buf)
        }
        "GetEncryptedAppTicket" => {
            handler_post_get_encrypted_app_ticket(pipe, read_buf, write_buf)
        }
        _ => false,
    }
}

fn handle_ipc_message(msg: ipc::IpcMessage) -> Option<ipc::IpcMessage> {
    match msg {
        ipc::IpcMessage::Ping => pong_ok(),
        ipc::IpcMessage::SetTicket {
            app_id,
            steam_id,
            app_ticket,
            e_ticket,
        } => {
            let config_dir = find_steam_root().join("config");
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let mut data = AppTicketData {
                app_id,
                steam_id: steam_id.clone(),
                app_ticket: app_ticket.clone(),
                e_ticket: e_ticket.clone(),
                last_update: Some(now),
            };
            if app_ticket.is_none() && steam_id.is_none() {
                if let Some(et) = e_ticket.clone() {
                    let _ = write_e_ticket_to(&config_dir, app_id, et);
                    if let Ok(Some(merged)) = read_app_ticket_from(&config_dir, app_id) {
                        data.steam_id = merged.steam_id;
                        data.app_ticket = merged.app_ticket;
                        data.e_ticket = merged.e_ticket;
                    }
                }
            } else {
                let _ = write_app_ticket_to(&config_dir, data.clone());
            }
            EticketClient::clear_cached_fresh_eticket(app_id);
            write_lock(cached_tickets()).insert(app_id, data);
            if let Some(tok) = app_ticket {
                sync_to_slssteam_config(|sls| {
                    sls.set_token(app_id, tok);
                });
            }
            pong_ok()
        }
        ipc::IpcMessage::RemoveTicket { app_id } => {
            write_lock(cached_tickets()).remove(&app_id);
            EticketClient::clear_cached_fresh_eticket(app_id);
            let config_dir = find_steam_root().join("config");
            let _ = remove_app_ticket_from(&config_dir, app_id);
            sync_to_slssteam_config(|sls| {
                sls.remove_token(app_id);
            });
            pong_ok()
        }
        ipc::IpcMessage::AddPlayUnownedApp { app_id } => {
            write_lock(spoofed_apps()).insert(app_id);
            let apps: Vec<u32> = read_lock(spoofed_apps()).iter().copied().collect();
            cloud_redirect().sync_app_set(apps);
            sync_to_slssteam_config(|sls| {
                sls.config.play_not_owned = true;
                sls.add_additional_app(app_id);
            });
            pong_ok()
        }
        ipc::IpcMessage::RemovePlayUnownedApp { app_id } => {
            write_lock(spoofed_apps()).remove(&app_id);
            let apps: Vec<u32> = read_lock(spoofed_apps()).iter().copied().collect();
            cloud_redirect().sync_app_set(apps);
            sync_to_slssteam_config(|sls| {
                sls.remove_additional_app(app_id);
            });
            pong_ok()
        }
        ipc::IpcMessage::SetPlayUnowned { enabled } => {
            sync_to_slssteam_config(|sls| {
                sls.config.play_not_owned = enabled;
            });
            pong_ok()
        }
        ipc::IpcMessage::SetStat {
            app_id,
            stat_name,
            stat_value,
        } => {
            write_lock(stats()).insert((app_id, stat_name), stat_value);
            cloud_redirect().enable_stats_sync(true);
            cloud_redirect().notify_stats_stored(app_id);
            pong_ok()
        }
        ipc::IpcMessage::RemoveStat { app_id, stat_name } => {
            write_lock(stats()).remove(&(app_id, stat_name));
            cloud_redirect().notify_stats_stored(app_id);
            pong_ok()
        }
        ipc::IpcMessage::SetLegacyCdKey { app_id, key } => {
            let trimmed = key.trim().to_string();
            write_lock(legacy_cdkeys()).insert(app_id, trimmed.clone());
            if !trimmed.is_empty() {
                let mut cdkey_store = LegacyCdKeyStore::new(&find_steam_root().join("config"));
                let _ = cdkey_store.set_override(app_id, trimmed);
            }
            pong_ok()
        }
        ipc::IpcMessage::RemoveLegacyCdKey { app_id } => {
            write_lock(legacy_cdkeys()).remove(&app_id);
            let mut cdkey_store = LegacyCdKeyStore::new(&find_steam_root().join("config"));
            let _ = cdkey_store.remove_override(app_id);
            pong_ok()
        }
        ipc::IpcMessage::SetDepotKey { depot_id, key } => {
            write_lock(depot_keys()).insert(depot_id, key.clone());
            sync_to_slssteam_config(|sls| {
                sls.set_depot_key(depot_id, key);
            });
            pong_ok()
        }
        ipc::IpcMessage::RemoveDepotKey { depot_id } => {
            write_lock(depot_keys()).remove(&depot_id);
            sync_to_slssteam_config(|sls| {
                sls.config.depot_keys.remove(&depot_id);
            });
            pong_ok()
        }
        ipc::IpcMessage::FetchManifestCode { gid } => {
            let code = read_lock(manifest_codes())
                .get(&gid)
                .cloned()
                .unwrap_or_else(|| gid.clone());
            write_lock(manifest_codes()).insert(gid.clone(), code.clone());
            Some(ipc::IpcMessage::ManifestCodeResponse { gid, code })
        }
        ipc::IpcMessage::FetchManifestCodeEx {
            app_id,
            depot_id,
            gid,
        } => {
            let effective_gid = if gid.is_empty() {
                let mut entry = DepotEntry {
                    app_id,
                    depot_id,
                    manifest_gid: 0,
                    manifest_size: 0,
                    dlc_app_id: 0,
                };
                if apply_depot_overrides(std::slice::from_mut(&mut entry)) > 0
                    && entry.manifest_gid != 0
                {
                    entry.manifest_gid.to_string()
                } else {
                    let mp = read_lock(manifest_pins());
                    mp.get(&(app_id, depot_id))
                        .cloned()
                        .or_else(|| {
                            mp.iter()
                                .find(|((_, did), _)| *did == depot_id)
                                .map(|(_, g)| g.clone())
                        })
                        .unwrap_or(gid)
                }
            } else {
                gid
            };
            let code = read_lock(manifest_codes())
                .get(&effective_gid)
                .cloned()
                .unwrap_or_else(|| effective_gid.clone());
            write_lock(manifest_codes()).insert(effective_gid.clone(), code.clone());
            Some(ipc::IpcMessage::ManifestCodeResponse { gid: effective_gid, code })
        }
        ipc::IpcMessage::SetAppIdSpoof { app_id, spoof_id } => {
            write_lock(app_id_spoofs()).insert(app_id, spoof_id);
            if spoof_id == interop_misc::ONLINE_FIX_APP_ID {
                interop_misc::set_online_fix_real_app_id(app_id);
            }
            sync_to_slssteam_config(|sls| {
                sls.set_fake_app_id(app_id, spoof_id);
            });
            pong_ok()
        }
        ipc::IpcMessage::RemoveAppIdSpoof { app_id } => {
            let removed = write_lock(app_id_spoofs()).remove(&app_id);
            if removed == Some(interop_misc::ONLINE_FIX_APP_ID) {
                interop_misc::set_online_fix_real_app_id(0);
            }
            sync_to_slssteam_config(|sls| {
                sls.remove_fake_app_id(app_id);
            });
            pong_ok()
        }
        ipc::IpcMessage::SetManifestPin {
            app_id,
            depot_id,
            manifest_id,
            size,
        } => {
            write_lock(manifest_pins()).insert((app_id, depot_id), manifest_id.clone());
            if let Ok(gid) = manifest_id.trim().parse::<u64>() {
                let ov_size = (size > 0).then_some(size);
                set_manifest_override(app_id, depot_id, ManifestOverride::new(gid, ov_size));
                let _ = ensure_manifest_cached(&find_steam_root(), app_id, depot_id, gid);
            }
            sync_to_slssteam_config(|sls| {
                sls.set_manifest_override(depot_id, manifest_id.clone());
            });
            Some(ipc::IpcMessage::ManifestBound {
                app_id,
                depot_id,
                manifest_id,
                active: true,
            })
        }
        ipc::IpcMessage::RemoveManifestPin { app_id, depot_id } => {
            write_lock(manifest_pins()).remove(&(app_id, depot_id));
            remove_manifest_override(app_id, depot_id);
            sync_to_slssteam_config(|sls| {
                sls.remove_manifest_override(depot_id);
            });
            Some(ipc::IpcMessage::ManifestBound {
                app_id,
                depot_id,
                manifest_id: String::new(),
                active: false,
            })
        }
        ipc::IpcMessage::Event { kind, payload } => {
            match kind.as_str() {
                "lua_hot_reload" => {
                    load_persisted_state(&find_steam_root());
                }
                "cloud_redirect" => {
                    let app_id = payload
                        .get("app_id")
                        .and_then(|s| s.trim().parse::<u32>().ok())
                        .unwrap_or(0);
                    match payload.get("action").map(String::as_str) {
                        Some("running") => cloud_redirect().notify_app_running(app_id, true),
                        Some("stopped") => {
                            cloud_redirect().notify_app_running(app_id, false);
                            drain_achievements(app_id, std::ptr::null_mut(), false);
                        }
                        Some("drain") => {
                            drain_achievements(app_id, std::ptr::null_mut(), false);
                        }
                        Some("shutdown") => cloud_redirect().shutdown(),
                        _ => sync_cloud_redirect_and_achievements(&find_steam_root()),
                    }
                }
                "ipc_dispatch" => {
                    let mut read_bytes = payload
                        .get("read_hex")
                        .and_then(|h| decode_hex_bytes(h))
                        .unwrap_or_default();
                    let mut write_bytes = vec![0u8; 64];
                    if !dispatch_steam_ipc_buffer(
                        &read_bytes,
                        &mut write_bytes,
                        std::ptr::null_mut(),
                    ) {
                        if let Some(method) = payload.get("method").map(String::as_str) {
                            let _ = dispatch_iclientuser_post(
                                method,
                                std::ptr::null_mut(),
                                &mut read_bytes,
                                &mut write_bytes,
                            );
                        }
                    }
                }
                _ => {}
            }
            pong_ok()
        }
        _ => pong_ok(),
    }
}

/// Reader function for manifest pins
pub fn get_pinned_manifest(app_id: u32, depot_id: u32) -> Option<String> {
    let mp = read_lock(manifest_pins());
    mp.get(&(app_id, depot_id))
        .cloned()
        .or_else(|| {
            mp.iter()
                .find(|((_, did), _)| *did == depot_id)
                .map(|(_, gid)| gid.clone())
        })
}

/// Exported C reader for pinned manifest GIDs
#[no_mangle]
pub extern "C" fn get_pinned_manifest_id(app_id: u32, depot_id: u32) -> u64 {
    get_pinned_manifest(app_id, depot_id)
        .and_then(|gid| gid.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Reader function for legacy CD keys
pub fn get_legacy_cd_key(app_id: u32) -> Option<String> {
    if let Some(key) = read_lock(legacy_cdkeys()).get(&app_id).cloned() {
        return Some(key);
    }
    let steam_id = resolve_spoof_steam_id(Some(app_id)).unwrap_or(76_561_197_960_287_930);
    let account_id = (steam_id & 0xFFFF_FFFF) as u32;
    let mut store = LegacyCdKeyStore::new(&find_steam_root().join("config"));
    if let Ok(Some(resolved)) = legacycdkey::resolve(&mut store, app_id, account_id) {
        return Some(resolved);
    }
    Some(legacycdkey::synthesize(app_id, account_id))
}

/// Reader function for depot decryption keys
pub fn get_depot_key(depot_id: u32) -> Option<String> {
    read_lock(depot_keys()).get(&depot_id).cloned()
}

/// Alias for compatibility with interop_decryption
pub fn read_depot_key(depot_id: u32) -> Option<String> {
    get_depot_key(depot_id)
}

/// Exported C reader for depot decryption keys
/// Returns a pointer to a C string (caller does not need to free - memory is leaked)
#[no_mangle]
pub extern "C" fn get_depot_key_c(depot_id: u32) -> *const libc::c_char {
    if let Some(key) = get_depot_key(depot_id) {
        let c_str = std::ffi::CString::new(key).unwrap_or_default();
        c_str.into_raw()
    } else {
        std::ptr::null()
    }
}

/// Reader function for additional apps (Lua-unlocked via addappid)
pub fn is_additional_app(app_id: u32) -> bool {
    read_lock(spoofed_apps()).contains(&app_id)
}

/// Get all additional apps (Lua-unlocked apps)
pub fn get_additional_apps() -> Vec<u32> {
    read_lock(spoofed_apps()).iter().copied().collect()
}

/// Exported C function to get all additional apps
#[no_mangle]
pub extern "C" fn get_additional_apps_c(out: *mut u32, max_len: u32) -> u32 {
    let apps = get_additional_apps();
    let len = apps.len().min(max_len as usize);
    if !out.is_null() && len > 0 {
        unsafe {
            std::ptr::copy_nonoverlapping(apps.as_ptr(), out, len);
        }
    }
    len as u32
}

/// Target platform for depot filtering
static TARGET_PLATFORM: OnceLock<(String, String)> = OnceLock::new();

/// Set target platform for depot filtering (called from wired_service or launcher)
pub fn set_target_platform(os: String, arch: String) {
    let _ = TARGET_PLATFORM.set((os, arch));
}

/// Get target platform (os, arch)
fn get_target_platform() -> (String, String) {
    TARGET_PLATFORM
        .get_or_init(|| ("linux".to_string(), "x86_64".to_string()))
        .clone()
}

/// Get all depot IDs from Lua config filtered by target platform
/// This collects all depot IDs that should be injected into Package 0 to make them appear owned.
pub fn get_all_depot_ids() -> Vec<u32> {
    let (target_os, target_arch) = get_target_platform();
    let mut depots = Vec::new();

    // Add depot IDs from depot_keys (depots with decryption keys)
    for &did in read_lock(depot_keys()).keys() {
        if !depots.contains(&did) {
            depots.push(did);
        }
    }

    // Add app IDs from spoofed_apps (additional apps / addappid)
    for &aid in read_lock(spoofed_apps()).iter() {
        if !depots.contains(&aid) {
            depots.push(aid);
        }
    }

    // Add depot IDs from cached Lua config entries (stplug-in/*.lua files parsed at load time)
    // These are depots that have keys or are entitlements in the Lua manifests
    if let Ok(cached_entries) = read_cached_lua_entries() {
        for entry in &cached_entries {
            // Filter by platform compatibility
            if entry.matches_platform(&target_os, &target_arch) {
                if !depots.contains(&entry.depot_id) {
                    depots.push(entry.depot_id);
                }
            }
        }
    }

    depots
}

impl CachedLuaEntry {
    /// Check if this entry matches the target platform
    pub fn matches_platform(&self, target_os: &str, target_arch: &str) -> bool {
        if let Some(ref os) = self.os {
            if os != target_os {
                return false;
            }
        }
        if let Some(ref arch) = self.arch {
            if arch != target_arch {
                return false;
            }
        }
        true
    }
}

/// Cached Lua entry with platform compatibility info
#[derive(Debug, Clone, Default)]
pub struct CachedLuaEntry {
    pub depot_id: u32,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub proton: Option<String>,
    pub native: bool,
}

/// Cached depot entries from Lua config entries (stplug-in/*.lua files)
static CACHED_LUA_ENTRIES: OnceLock<RwLock<Vec<CachedLuaEntry>>> = OnceLock::new();

fn cached_lua_entries() -> &'static RwLock<Vec<CachedLuaEntry>> {
    CACHED_LUA_ENTRIES.get_or_init(|| RwLock::new(Vec::new()))
}

fn read_cached_lua_entries() -> Result<Vec<CachedLuaEntry>, String> {
    Ok(read_lock(cached_lua_entries()).clone())
}

/// Update cached Lua entries from parsed stplug-in Lua files
/// Called during load_persisted_state to sync all depot IDs from Lua configs
fn update_cached_lua_entries(entries: Vec<CachedLuaEntry>) {
    let mut cached = write_lock(cached_lua_entries());
    *cached = entries;
}

/// Take pending depot additions from Lua config hot-reload
pub fn take_pending_additions() -> Vec<u32> {
    let mut additions = write_lock(pending_additions());
    let result = additions.clone();
    additions.clear();
    result
}

/// Take pending depot removals from Lua config hot-reload
pub fn take_pending_removals() -> Vec<u32> {
    let mut removals = write_lock(pending_removals());
    let result = removals.clone();
    removals.clear();
    result
}

/// Queue a depot addition from Lua config hot-reload
pub fn queue_pending_addition(depot_id: u32) {
    let mut additions = write_lock(pending_additions());
    if !additions.contains(&depot_id) {
        additions.push(depot_id);
    }
}

/// Queue a depot removal from Lua config hot-reload
pub fn queue_pending_removal(depot_id: u32) {
    let mut removals = write_lock(pending_removals());
    if !removals.contains(&depot_id) {
        removals.push(depot_id);
    }
}

/// Exported C reader for additional apps check
#[no_mangle]
pub extern "C" fn is_additional_app_c(app_id: u32) -> bool {
    is_additional_app(app_id)
}

/// Exported C function to get all depot IDs
#[no_mangle]
pub extern "C" fn get_all_depot_ids_c(out: *mut u32, max_len: u32) -> u32 {
    let depots = get_all_depot_ids();
    let len = depots.len().min(max_len as usize);
    if !out.is_null() && len > 0 {
        unsafe {
            std::ptr::copy_nonoverlapping(depots.as_ptr(), out, len);
        }
    }
    len as u32
}

/// Exported C function to take pending additions
#[no_mangle]
pub extern "C" fn take_pending_additions_c(out: *mut u32, max_len: u32) -> u32 {
    let additions = take_pending_additions();
    let len = additions.len().min(max_len as usize);
    if !out.is_null() && len > 0 {
        unsafe {
            std::ptr::copy_nonoverlapping(additions.as_ptr(), out, len);
        }
    }
    len as u32
}

/// Exported C function to set target platform for depot filtering
#[no_mangle]
pub extern "C" fn set_target_platform_c(os: *const libc::c_char, arch: *const libc::c_char) {
    if os.is_null() || arch.is_null() {
        return;
    }
    let os_str = unsafe { std::ffi::CStr::from_ptr(os).to_string_lossy().into_owned() };
    let arch_str = unsafe { std::ffi::CStr::from_ptr(arch).to_string_lossy().into_owned() };
    set_target_platform(os_str, arch_str);
}

/// Exported C function to take pending removals
#[no_mangle]
pub extern "C" fn take_pending_removals_c(out: *mut u32, max_len: u32) -> u32 {
    let removals = take_pending_removals();
    let len = removals.len().min(max_len as usize);
    if !out.is_null() && len > 0 {
        unsafe {
            std::ptr::copy_nonoverlapping(removals.as_ptr(), out, len);
        }
    }
    len as u32
}

/// Exported C reader for legacy CD keys
#[no_mangle]
pub unsafe extern "C" fn get_legacy_cdkey(
    app_id: u32,
    buf: *mut libc::c_char,
    max_len: u32,
) -> bool {
    panic::catch_unwind(AssertUnwindSafe(|| {
        if let Some(key) = get_legacy_cd_key(app_id) {
            if !buf.is_null() && max_len > 0 {
                let bytes = key.as_bytes();
                let n = (max_len as usize - 1).min(bytes.len());
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, n);
                *buf.add(n) = 0;
                return true;
            }
        }
        false
    }))
    .unwrap_or(false)
}

/// Reader function for stats
pub fn get_stat(app_id: u32, stat_name: &str) -> Option<String> {
    read_lock(stats()).get(&(app_id, stat_name.to_string())).cloned()
}

/// Exported C reader for stats
#[no_mangle]
pub unsafe extern "C" fn get_stat_override(
    app_id: u32,
    stat_name: *const libc::c_char,
    buf: *mut libc::c_char,
    max_len: u32,
) -> bool {
    panic::catch_unwind(AssertUnwindSafe(|| {
        if stat_name.is_null() {
            return false;
        }
        let c_str = std::ffi::CStr::from_ptr(stat_name);
        let Ok(name_str) = c_str.to_str() else {
            return false;
        };
        if let Some(val) = read_lock(stats()).get(&(app_id, name_str.to_string())) {
            if !buf.is_null() && max_len > 0 {
                let bytes = val.as_bytes();
                let n = (max_len as usize - 1).min(bytes.len());
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, n);
                *buf.add(n) = 0;
                return true;
            }
        }
        false
    }))
    .unwrap_or(false)
}

#[ctor]
fn init() {
    libsteam_integration_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_readers_and_c_exports() {
        write_lock(manifest_pins()).insert((1245620, 1245621), "8888777766665555444".into());
        assert_eq!(get_pinned_manifest(1245620, 1245621), Some("8888777766665555444".into()));
        assert_eq!(get_pinned_manifest_id(1245620, 1245621), 8888777766665555444);

        write_lock(legacy_cdkeys()).insert(1245620, "XXXXX-YYYYY-ZZZZZ".into());
        assert_eq!(get_legacy_cd_key(1245620), Some("XXXXX-YYYYY-ZZZZZ".into()));
        let mut buf = [0i8; 64];
        assert!(unsafe { get_legacy_cdkey(1245620, buf.as_mut_ptr(), 64) });
        let key_str = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()).to_str().unwrap() };
        assert_eq!(key_str, "XXXXX-YYYYY-ZZZZZ");

        write_lock(stats()).insert((1245620, "KILLS".into()), "42".into());
        assert_eq!(get_stat(1245620, "KILLS"), Some("42".into()));
        let c_stat = std::ffi::CString::new("KILLS").unwrap();
        let mut stat_buf = [0i8; 16];
        assert!(unsafe { get_stat_override(1245620, c_stat.as_ptr(), stat_buf.as_mut_ptr(), 16) });
        let val_str = unsafe { std::ffi::CStr::from_ptr(stat_buf.as_ptr()).to_str().unwrap() };
        assert_eq!(val_str, "42");
    }

    #[test]
    fn test_ipc_manifest_and_slssteam_sync() {
        let resp = handle_ipc_message(ipc::IpcMessage::SetManifestPin {
            app_id: 1245630,
            depot_id: 1245631,
            manifest_id: "9999888877776666".into(),
            size: 1024,
        });
        match resp {
            Some(ipc::IpcMessage::ManifestBound { active, manifest_id, .. }) => {
                assert!(active);
                assert_eq!(manifest_id, "9999888877776666");
            }
            other => panic!("Unexpected response: {:?}", other),
        }
        assert_eq!(get_pinned_manifest(1245630, 1245631), Some("9999888877776666".into()));

        // Also verify FetchManifestCodeEx reads pinned manifest
        let code_resp = handle_ipc_message(ipc::IpcMessage::FetchManifestCodeEx {
            app_id: 1245630,
            depot_id: 1245631,
            gid: String::new(),
        });
        match code_resp {
            Some(ipc::IpcMessage::ManifestCodeResponse { gid, .. }) => {
                assert_eq!(gid, "9999888877776666");
            }
            other => panic!("Unexpected response: {:?}", other),
        }

        let rm_resp = handle_ipc_message(ipc::IpcMessage::RemoveManifestPin {
            app_id: 1245630,
            depot_id: 1245631,
        });
        assert!(matches!(
            rm_resp,
            Some(ipc::IpcMessage::ManifestBound { active: false, .. })
        ));
        assert!(!get_manifest_overrides().contains_key(&(1245630, 1245631)));
    }

    #[test]
    fn test_wired_modules_smoke() {
        assert!(parse_pattern_and_mask("48 89 ?? 24", None).is_some());
        assert!(trap::get_main_executable_path().is_ok());
        assert!(hash::sha256_of_current_exe().is_ok());
        assert!(!ost_thread::current_native_thread_handle().is_null());

        let local_val = 7u32;
        assert!(memory::is_readable(
            (&local_val as *const u32).cast(),
            std::mem::size_of::<u32>()
        ));

        let sid = 76_561_198_028_121_353u64;
        let ticket = synthesize_ownership_ticket(1245620, sid);
        assert_eq!(extract_ownershipticket_steam_id(&ticket), Some(sid));
        assert_eq!(extract_eticket_steam_id(&ticket), Some(sid));

        let mut write_buf = vec![0u8; 64];
        assert!(!dispatch_steam_ipc_buffer(
            &[0u8; 4],
            &mut write_buf,
            std::ptr::null_mut()
        ));
        let mut empty_read = [];
        assert!(!dispatch_iclientuser_post(
            "UnknownMethod",
            std::ptr::null_mut(),
            &mut empty_read,
            &mut write_buf
        ));
    }
}
