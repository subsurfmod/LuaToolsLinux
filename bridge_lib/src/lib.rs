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
use std::os::raw::c_void;
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
    ipc::{self, IpcServer},
    memory_patch::{self, MemoryPatch, JMP_ABS64_LEN},
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

fn read_lock<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|e| e.into_inner())
}

fn write_lock<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|e| e.into_inner())
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
        if read_lock(spoofed_apps()).contains(&app_id) {
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
        let is_spoofed = read_lock(spoofed_apps()).contains(&app_id)
            || read_lock(cached_tickets()).contains_key(&app_id);

        if is_spoofed {
            let ticket_bytes = read_lock(cached_tickets())
                .get(&app_id)
                .and_then(|d| d.app_ticket.as_deref())
                .and_then(|raw| decode_hex_bytes(raw).or_else(|| Some(raw.as_bytes().to_vec())))
                .unwrap_or_else(|| {
                    let sid =
                        resolve_spoof_steam_id(Some(app_id)).unwrap_or(76_561_197_960_287_930);
                    synthesize_ownership_ticket(app_id, sid)
                });

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
            || read_lock(cached_tickets()).contains_key(&app_id);
        if is_spoofed {
            let handle = NEXT_CALL_HANDLE.fetch_add(1, Ordering::Relaxed);
            write_lock(pending_etickets()).insert(handle, app_id);
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
        let app_opt = write_lock(pending_etickets())
            .remove(&call_handle)
            .or_else(|| {
                read_lock(cached_tickets())
                    .keys()
                    .next()
                    .copied()
                    .filter(|_| (call_handle & 0xFFFF_FFFF_0000_0000) == 0x4C55_4154_0000_0000)
            });

        if let Some(app_id) = app_opt {
            let eticket_bytes = read_lock(cached_tickets())
                .get(&app_id)
                .and_then(|d| d.e_ticket.as_deref())
                .and_then(|raw| decode_hex_bytes(raw).or_else(|| Some(raw.as_bytes().to_vec())))
                .unwrap_or_else(|| {
                    let sid =
                        resolve_spoof_steam_id(Some(app_id)).unwrap_or(76_561_197_960_287_930);
                    synthesize_ownership_ticket(app_id, sid)
                });

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
        let key_bytes_opt = read_lock(depot_keys())
            .get(&depot_id)
            .and_then(|hex| decode_hex_bytes(hex))
            .filter(|b| b.len() == 32);

        let orig_opt = *read_lock(original_depot_decrypt());
        if let Some(original) = orig_opt {
            if original(this, depot_id, buffer, size) {
                return true;
            }
        }

        if let Some(key_bytes) = key_bytes_opt {
            if !buffer.is_null() && size >= 32 {
                std::ptr::copy_nonoverlapping(key_bytes.as_ptr(), buffer, 32);
                return true;
            }
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
    let exe = fs::read_link("/proc/self/exe").ok();
    is_steam_client_process(&comm, exe.as_deref())
}

static INIT: Once = Once::new();

fn libsteam_integration_init() {
    if !should_activate_in_current_process() {
        return;
    }

    INIT.call_once(|| {
        install_panic_hook();
        let _ = thread::Builder::new()
            .name("luatools-bridge".into())
            .spawn(|| {
                let res = panic::catch_unwind(AssertUnwindSafe(run_integration));
                match res {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        eprintln!("[steam_integration] init error: {}", e);
                    }
                    Err(_) => {
                        eprintln!(
                            "[steam_integration] recovered from unexpected panic; Steam continues unaffected"
                        );
                    }
                }
            });
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
                if base_id > 0 {
                    write_lock(spoofed_apps()).insert(base_id);
                }
                for item in parsed.entries {
                    if item.has_key() {
                        if let Some(k) = item.key {
                            write_lock(depot_keys()).insert(item.id, k);
                        }
                    } else if item.id > 0 {
                        write_lock(spoofed_apps()).insert(item.id);
                    }
                    if let Some(mid) = item.manifest_id {
                        if base_id > 0 {
                            write_lock(manifest_pins()).insert((base_id, item.id), mid);
                        }
                    }
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
    let mut store = TicketStore::new(&steam_root.join("config"));
    if store.load().is_ok() {
        if let Ok(ids) = store.list_app_ids() {
            let mut ct = write_lock(cached_tickets());
            for aid in ids {
                if let Ok(data) = store.get(aid) {
                    ct.insert(aid, data);
                }
            }
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
            let pattern_bytes = parse_hex_pattern(pattern_hex);
            let mask_bytes = entry.mask.as_deref().map(parse_hex_pattern);
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
    if let Some(addr) = resolve_runtime_target("depot_key_integration")
        .or_else(|| resolve_runtime_target("depot_decrypt"))
    {
        if install_depot_decrypt_patch(addr) {
            installed_count += 1;
        }
    }

    eprintln!(
        "[steam_integration] memory integration ready ({} hook(s) active at steamclient base 0x{:x})",
        installed_count, mapping.base_addr
    );
}

fn run_integration() -> Result<(), String> {
    let steam_root = find_steam_root();
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
        thread::park_timeout(Duration::from_secs(3600));
    }
}

fn parse_hex_pattern(hex: &str) -> Vec<u8> {
    hex.split_whitespace()
        .filter_map(|s| u8::from_str_radix(s, 16).ok())
        .collect()
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
            let mut store = TicketStore::new(&config_dir);
            let _ = store.load();
            let data = AppTicketData {
                app_id,
                steam_id,
                app_ticket: app_ticket.clone(),
                e_ticket,
                last_update: Some(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                ),
            };
            write_lock(cached_tickets()).insert(app_id, data.clone());
            let _ = store.set(data);
            if let Some(tok) = app_ticket {
                sync_to_slssteam_config(|sls| {
                    sls.set_token(app_id, tok);
                });
            }
            pong_ok()
        }
        ipc::IpcMessage::RemoveTicket { app_id } => {
            write_lock(cached_tickets()).remove(&app_id);
            let config_dir = find_steam_root().join("config");
            let mut store = TicketStore::new(&config_dir);
            let _ = store.load();
            let _ = store.remove(app_id);
            sync_to_slssteam_config(|sls| {
                sls.remove_token(app_id);
            });
            pong_ok()
        }
        ipc::IpcMessage::AddPlayUnownedApp { app_id } => {
            write_lock(spoofed_apps()).insert(app_id);
            sync_to_slssteam_config(|sls| {
                sls.config.play_not_owned = true;
                sls.add_additional_app(app_id);
            });
            pong_ok()
        }
        ipc::IpcMessage::RemovePlayUnownedApp { app_id } => {
            write_lock(spoofed_apps()).remove(&app_id);
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
            pong_ok()
        }
        ipc::IpcMessage::RemoveStat { app_id, stat_name } => {
            write_lock(stats()).remove(&(app_id, stat_name));
            pong_ok()
        }
        ipc::IpcMessage::SetLegacyCdKey { app_id, key } => {
            write_lock(legacy_cdkeys()).insert(app_id, key);
            pong_ok()
        }
        ipc::IpcMessage::RemoveLegacyCdKey { app_id } => {
            write_lock(legacy_cdkeys()).remove(&app_id);
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
                let mp = read_lock(manifest_pins());
                mp.get(&(app_id, depot_id))
                    .cloned()
                    .or_else(|| {
                        mp.iter()
                            .find(|((_, did), _)| *did == depot_id)
                            .map(|(_, g)| g.clone())
                    })
                    .unwrap_or(gid)
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
            sync_to_slssteam_config(|sls| {
                sls.set_fake_app_id(app_id, spoof_id);
            });
            pong_ok()
        }
        ipc::IpcMessage::RemoveAppIdSpoof { app_id } => {
            write_lock(app_id_spoofs()).remove(&app_id);
            sync_to_slssteam_config(|sls| {
                sls.remove_fake_app_id(app_id);
            });
            pong_ok()
        }
        ipc::IpcMessage::SetManifestPin {
            app_id,
            depot_id,
            manifest_id,
            size: _,
        } => {
            write_lock(manifest_pins()).insert((app_id, depot_id), manifest_id.clone());
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
        ipc::IpcMessage::Event { kind, .. } => {
            if kind == "lua_hot_reload" {
                load_persisted_state(&find_steam_root());
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
    // Fall back to deterministically synthesized key
    let steam_id = resolve_spoof_steam_id(Some(app_id)).unwrap_or(76_561_197_960_287_930);
    Some(luatools_linux::steam_bridge::credentials::legacycdkey::generate_legacy_cdkey(app_id, steam_id))
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
    }
}
