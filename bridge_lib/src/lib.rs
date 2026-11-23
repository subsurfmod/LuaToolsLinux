//! Steam client integration shared library (`libsteam_integration.so`).
//!
//! Loaded into the Steam client process via `LD_PRELOAD`. Its constructor loads persisted
//! LuaTools overrides & `.lua` manifests, scans Steam binaries for integration points,
//! installs 64-bit ASLR-safe inline memory patches, and runs the Unix domain socket IPC server.

#![allow(clippy::missing_safety_doc)]

use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::raw::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Once, OnceLock, RwLock};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use ctor::ctor;
use luatools_linux::parsers::lua_parser::LuaFileParser;
use luatools_linux::services::manifest_binding_service::ManifestPin;
use luatools_linux::services::steam_bridge_service::BridgeOverrides;
use luatools_linux::steam_bridge::{
    binary_reader::{scan_steam_binaries, BinaryFile},
    ipc::{self, IpcServer},
    memory_patch::{self, MemoryPatch},
    pattern_db::{BinaryComponent, PatternChannel, PatternDb},
    ticket_store::{AppTicketData, TicketStore},
};

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
    let cache = cached_tickets().read().unwrap();
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
    if spoofed_apps().read().unwrap().contains(&app_id) {
        return true;
    }

    if let Some(original) = original_bis().read().unwrap().as_ref() {
        return original(this, app_id);
    }
    false
}

unsafe extern "C" fn get_app_id_replacement(this: *mut c_void) -> u32 {
    if let Some(original) = original_get_app_id().read().unwrap().as_ref() {
        let real_id = original(this);
        if let Some(&spoof_id) = app_id_spoofs().read().unwrap().get(&real_id) {
            return spoof_id;
        }
        return real_id;
    }

    if let Some((&_, &spoof_id)) = app_id_spoofs().read().unwrap().iter().next() {
        return spoof_id;
    }
    0
}

unsafe extern "C" fn get_app_ownership_ticket_replacement(
    this: *mut c_void,
    app_id: u32,
    ticket_buffer: *mut c_void,
    ticket_buffer_size: u32,
) -> bool {
    let is_spoofed = spoofed_apps().read().unwrap().contains(&app_id)
        || cached_tickets().read().unwrap().contains_key(&app_id);

    if is_spoofed {
        let ticket_bytes = cached_tickets()
            .read()
            .unwrap()
            .get(&app_id)
            .and_then(|d| d.app_ticket.as_deref())
            .and_then(|raw| decode_hex_bytes(raw).or_else(|| Some(raw.as_bytes().to_vec())))
            .unwrap_or_else(|| {
                let sid = resolve_spoof_steam_id(Some(app_id)).unwrap_or(76_561_197_960_287_930);
                synthesize_ownership_ticket(app_id, sid)
            });

        if !ticket_buffer.is_null() && ticket_buffer_size > 0 {
            let cap = ticket_buffer_size as usize;
            let dst = std::slice::from_raw_parts_mut(ticket_buffer as *mut u8, cap);
            dst.fill(0);
            let n = cap.min(ticket_bytes.len());
            dst[..n].copy_from_slice(&ticket_bytes[..n]);
        }
        return true;
    }

    if let Some(original) = original_get_app_ownership_ticket().read().unwrap().as_ref() {
        return original(this, app_id, ticket_buffer, ticket_buffer_size);
    }
    false
}

unsafe extern "C" fn request_encrypted_app_ticket_replacement(
    this: *mut c_void,
    app_id: u32,
    user_data: *const c_void,
    user_data_size: u32,
) -> u64 {
    let is_spoofed = spoofed_apps().read().unwrap().contains(&app_id)
        || cached_tickets().read().unwrap().contains_key(&app_id);
    if is_spoofed {
        let handle = NEXT_CALL_HANDLE.fetch_add(1, Ordering::Relaxed);
        pending_etickets().write().unwrap().insert(handle, app_id);
        return handle;
    }

    if let Some(original) = original_request_encrypted_app_ticket()
        .read()
        .unwrap()
        .as_ref()
    {
        return original(this, app_id, user_data, user_data_size);
    }
    0
}

unsafe extern "C" fn get_encrypted_app_ticket_replacement(
    this: *mut c_void,
    call_handle: u64,
    ticket_buffer: *mut c_void,
    ticket_size: *mut u32,
) -> bool {
    let app_opt = pending_etickets()
        .write()
        .unwrap()
        .remove(&call_handle)
        .or_else(|| {
            cached_tickets()
                .read()
                .unwrap()
                .keys()
                .next()
                .copied()
                .filter(|_| (call_handle & 0xFFFF_FFFF_0000_0000) == 0x4C55_4154_0000_0000)
        });

    if let Some(app_id) = app_opt {
        let eticket_bytes = cached_tickets()
            .read()
            .unwrap()
            .get(&app_id)
            .and_then(|d| d.e_ticket.as_deref())
            .and_then(|raw| decode_hex_bytes(raw).or_else(|| Some(raw.as_bytes().to_vec())))
            .unwrap_or_else(|| {
                let sid = resolve_spoof_steam_id(Some(app_id)).unwrap_or(76_561_197_960_287_930);
                synthesize_ownership_ticket(app_id, sid)
            });

        if !ticket_size.is_null() {
            let max_cap = (*ticket_size) as usize;
            let written = if !ticket_buffer.is_null() && max_cap > 0 {
                let n = max_cap.min(eticket_bytes.len());
                std::ptr::copy_nonoverlapping(eticket_bytes.as_ptr(), ticket_buffer as *mut u8, n);
                n
            } else {
                eticket_bytes.len()
            };
            *ticket_size = written as u32;
        }
        return true;
    }

    if let Some(original) = original_get_encrypted_app_ticket().read().unwrap().as_ref() {
        return original(this, call_handle, ticket_buffer, ticket_size);
    }
    false
}

unsafe extern "C" fn get_steam_id_replacement(this: *mut c_void) -> u64 {
    if let Some(sid) = resolve_spoof_steam_id(None) {
        return sid;
    }
    if let Some(original) = original_get_steam_id().read().unwrap().as_ref() {
        return original(this);
    }
    0
}

unsafe extern "C" fn depot_decrypt_replacement(
    this: *mut c_void,
    depot_id: u32,
    buffer: *mut u8,
    size: u32,
) -> bool {
    let key_bytes_opt = depot_keys()
        .read()
        .unwrap()
        .get(&depot_id)
        .and_then(|hex| decode_hex_bytes(hex))
        .filter(|b| b.len() == 32);

    if let Some(original) = original_depot_decrypt().read().unwrap().as_ref() {
        if original(this, depot_id, buffer, size) {
            return true;
        }
    }

    if let Some(key_bytes) = key_bytes_opt {
        if !buffer.is_null() && size == 32 {
            std::ptr::copy_nonoverlapping(key_bytes.as_ptr(), buffer, 32);
            return true;
        }
    }

    false
}

static INIT: Once = Once::new();

fn libsteam_integration_init() {
    INIT.call_once(|| {
        thread::spawn(|| {
            if let Err(e) = run_integration() {
                eprintln!("[steam_integration] init error: {}", e);
            }
        });
    });
}

fn load_persisted_state(steam_root: &Path) {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let data_dir = home.join(".local/share/luatools");

    // 1. Load bridge_overrides.json
    if let Ok(raw) = fs::read_to_string(data_dir.join("bridge_overrides.json")) {
        if let Ok(ov) = serde_json::from_str::<BridgeOverrides>(&raw) {
            {
                let mut apps = spoofed_apps().write().unwrap();
                for aid in ov.redirect_apps {
                    apps.insert(aid);
                }
            }
            {
                let mut spoofs = app_id_spoofs().write().unwrap();
                for (aid, sid) in ov.app_id_spoofs {
                    spoofs.insert(aid, sid);
                }
            }
            {
                let mut cks = legacy_cdkeys().write().unwrap();
                for (aid, k) in ov.legacy_cd_keys {
                    cks.insert(aid, k);
                }
            }
            {
                let mut dks = depot_keys().write().unwrap();
                for (did, k) in ov.depot_keys {
                    dks.insert(did, k);
                }
            }
            {
                let mut st = stats().write().unwrap();
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
            let mut mp = manifest_pins().write().unwrap();
            for p in pins {
                mp.insert((p.app_id, p.depot_id), p.manifest_id);
            }
        }
    }

    // 3. Import stplug-in/*.lua entitlements, depot decryption keys, and active manifest pins
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
                    spoofed_apps().write().unwrap().insert(base_id);
                }
                for item in parsed.entries {
                    if item.has_key() {
                        if let Some(k) = item.key {
                            depot_keys().write().unwrap().insert(item.id, k);
                        }
                    } else if item.id > 0 {
                        spoofed_apps().write().unwrap().insert(item.id);
                    }
                    if let Some(mid) = item.manifest_id {
                        if base_id > 0 {
                            manifest_pins()
                                .write()
                                .unwrap()
                                .insert((base_id, item.id), mid);
                        }
                    }
                }
            }
        }
    }

    // 4. Load TicketStore cache from <steam_root>/config/localconfig.vdf
    let mut store = TicketStore::new(&steam_root.join("config"));
    if store.load().is_ok() {
        if let Ok(ids) = store.list_app_ids() {
            let mut ct = cached_tickets().write().unwrap();
            for aid in ids {
                if let Ok(data) = store.get(aid) {
                    ct.insert(aid, data);
                }
            }
        }
    }
}

fn run_integration() -> Result<(), String> {
    let steam_root = find_steam_root();
    load_persisted_state(&steam_root);

    let binaries = scan_steam_binaries(&steam_root);
    let mut steam_client: Option<BinaryFile> = None;

    for binary in &binaries {
        let name = binary
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if name.contains("steamclient") {
            steam_client = Some(binary.clone());
        }
    }

    let pattern_db = PatternDb::new();

    if let Some(ref client) = steam_client {
        match pattern_db.lookup_by_binary(
            client,
            PatternChannel::Pattern,
            BinaryComponent::SteamClient,
        ) {
            Ok(pf) => {
                let resolve_target = |key: &str| -> Option<u64> {
                    let entry = pf.patterns.get(key)?;
                    let pattern_hex = entry.pattern.as_ref()?;
                    let pattern_bytes = parse_hex_pattern(pattern_hex);
                    let mask_bytes = entry.mask.as_deref().map(parse_hex_pattern);
                    let pattern_offset = entry.offset.unwrap_or(0);
                    client.resolve_pattern_address(
                        &pattern_bytes,
                        mask_bytes.as_deref(),
                        pattern_offset,
                        None,
                    )
                };

                if let Some(addr) = resolve_target("bis_subscribed_app") {
                    install_bis_subscribed_app_patch(addr);
                }
                if let Some(addr) = resolve_target("get_app_id") {
                    install_get_app_id_patch(addr);
                }
                if let Some(addr) = resolve_target("get_app_ownership_ticket") {
                    install_get_app_ownership_ticket_patch(addr);
                }
                if let Some(addr) = resolve_target("request_encrypted_app_ticket") {
                    install_request_encrypted_app_ticket_patch(addr);
                }
                if let Some(addr) = resolve_target("get_encrypted_app_ticket") {
                    install_get_encrypted_app_ticket_patch(addr);
                }
                if let Some(addr) = resolve_target("get_steam_id") {
                    install_get_steam_id_patch(addr);
                }
                if let Some(addr) = resolve_target("depot_key_integration")
                    .or_else(|| resolve_target("depot_decrypt"))
                {
                    install_depot_decrypt_patch(addr);
                }
            }
            Err(e) => {
                eprintln!(
                    "[steam_integration] steamclient pattern lookup failed: {}",
                    e
                );
            }
        }
    }

    let mut server = IpcServer::new();
    server.set_handler(handle_ipc_message);
    server
        .start()
        .map_err(|e| format!("IPC server failed: {}", e))?;

    loop {
        std::thread::park_timeout(std::time::Duration::from_secs(3600));
    }
}

fn parse_hex_pattern(hex: &str) -> Vec<u8> {
    hex.split_whitespace()
        .filter_map(|s| u8::from_str_radix(s, 16).ok())
        .collect()
}

fn install_bis_subscribed_app_patch(target_addr: u64) {
    let replacement_ptr = bis_subscribed_app_replacement as *mut c_void as *mut u8;
    match unsafe { memory_patch::patch_by_offset(target_addr, 0, replacement_ptr) } {
        Ok(patch) => {
            unsafe {
                let trampoline: BisSubscribedAppFn = std::mem::transmute(patch.trampoline);
                *original_bis().write().unwrap() = Some(trampoline);
            }
            installed_patches().write().unwrap().push(patch);
        }
        Err(e) => {
            eprintln!(
                "[steam_integration] failed to install BIsSubscribedApp patch: {}",
                e
            );
        }
    }
}

fn install_get_app_id_patch(target_addr: u64) {
    let replacement_ptr = get_app_id_replacement as *mut c_void as *mut u8;
    match unsafe { memory_patch::patch_by_offset(target_addr, 0, replacement_ptr) } {
        Ok(patch) => {
            unsafe {
                let trampoline: GetAppIdFn = std::mem::transmute(patch.trampoline);
                *original_get_app_id().write().unwrap() = Some(trampoline);
            }
            installed_patches().write().unwrap().push(patch);
        }
        Err(e) => {
            eprintln!(
                "[steam_integration] failed to install GetAppId patch: {}",
                e
            );
        }
    }
}

fn install_get_app_ownership_ticket_patch(target_addr: u64) {
    let replacement_ptr = get_app_ownership_ticket_replacement as *mut c_void as *mut u8;
    match unsafe { memory_patch::patch_by_offset(target_addr, 0, replacement_ptr) } {
        Ok(patch) => {
            unsafe {
                let trampoline: GetAppOwnershipTicketFn = std::mem::transmute(patch.trampoline);
                *original_get_app_ownership_ticket().write().unwrap() = Some(trampoline);
            }
            installed_patches().write().unwrap().push(patch);
        }
        Err(e) => {
            eprintln!(
                "[steam_integration] failed to install GetAppOwnershipTicket patch: {}",
                e
            );
        }
    }
}

fn install_request_encrypted_app_ticket_patch(target_addr: u64) {
    let replacement_ptr = request_encrypted_app_ticket_replacement as *mut c_void as *mut u8;
    match unsafe { memory_patch::patch_by_offset(target_addr, 0, replacement_ptr) } {
        Ok(patch) => {
            unsafe {
                let trampoline: RequestEncryptedAppTicketFn = std::mem::transmute(patch.trampoline);
                *original_request_encrypted_app_ticket().write().unwrap() = Some(trampoline);
            }
            installed_patches().write().unwrap().push(patch);
        }
        Err(e) => {
            eprintln!(
                "[steam_integration] failed to install RequestEncryptedAppTicket patch: {}",
                e
            );
        }
    }
}

fn install_get_encrypted_app_ticket_patch(target_addr: u64) {
    let replacement_ptr = get_encrypted_app_ticket_replacement as *mut c_void as *mut u8;
    match unsafe { memory_patch::patch_by_offset(target_addr, 0, replacement_ptr) } {
        Ok(patch) => {
            unsafe {
                let trampoline: GetEncryptedAppTicketFn = std::mem::transmute(patch.trampoline);
                *original_get_encrypted_app_ticket().write().unwrap() = Some(trampoline);
            }
            installed_patches().write().unwrap().push(patch);
        }
        Err(e) => {
            eprintln!(
                "[steam_integration] failed to install GetEncryptedAppTicket patch: {}",
                e
            );
        }
    }
}

fn install_get_steam_id_patch(target_addr: u64) {
    let replacement_ptr = get_steam_id_replacement as *mut c_void as *mut u8;
    match unsafe { memory_patch::patch_by_offset(target_addr, 0, replacement_ptr) } {
        Ok(patch) => {
            unsafe {
                let trampoline: GetSteamIDFn = std::mem::transmute(patch.trampoline);
                *original_get_steam_id().write().unwrap() = Some(trampoline);
            }
            installed_patches().write().unwrap().push(patch);
        }
        Err(e) => {
            eprintln!(
                "[steam_integration] failed to install GetSteamID patch: {}",
                e
            );
        }
    }
}

fn install_depot_decrypt_patch(target_addr: u64) {
    let replacement_ptr = depot_decrypt_replacement as *mut c_void as *mut u8;
    match unsafe { memory_patch::patch_by_offset(target_addr, 0, replacement_ptr) } {
        Ok(patch) => {
            unsafe {
                let trampoline: DepotKeyIntegrationFn = std::mem::transmute(patch.trampoline);
                *original_depot_decrypt().write().unwrap() = Some(trampoline);
            }
            installed_patches().write().unwrap().push(patch);
        }
        Err(e) => {
            eprintln!(
                "[steam_integration] failed to install DepotKeyIntegration patch: {}",
                e
            );
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
                app_ticket,
                e_ticket,
                last_update: Some(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                ),
            };
            cached_tickets()
                .write()
                .unwrap()
                .insert(app_id, data.clone());
            let _ = store.set(data);
            pong_ok()
        }
        ipc::IpcMessage::RemoveTicket { app_id } => {
            cached_tickets().write().unwrap().remove(&app_id);
            let config_dir = find_steam_root().join("config");
            let mut store = TicketStore::new(&config_dir);
            let _ = store.load();
            let _ = store.remove(app_id);
            pong_ok()
        }
        ipc::IpcMessage::AddPlayUnownedApp { app_id } => {
            spoofed_apps().write().unwrap().insert(app_id);
            pong_ok()
        }
        ipc::IpcMessage::RemovePlayUnownedApp { app_id } => {
            spoofed_apps().write().unwrap().remove(&app_id);
            pong_ok()
        }
        ipc::IpcMessage::SetPlayUnowned { .. } => pong_ok(),
        ipc::IpcMessage::SetStat {
            app_id,
            stat_name,
            stat_value,
        } => {
            stats()
                .write()
                .unwrap()
                .insert((app_id, stat_name), stat_value);
            pong_ok()
        }
        ipc::IpcMessage::RemoveStat { app_id, stat_name } => {
            stats().write().unwrap().remove(&(app_id, stat_name));
            pong_ok()
        }
        ipc::IpcMessage::SetLegacyCdKey { app_id, key } => {
            legacy_cdkeys().write().unwrap().insert(app_id, key);
            pong_ok()
        }
        ipc::IpcMessage::RemoveLegacyCdKey { app_id } => {
            legacy_cdkeys().write().unwrap().remove(&app_id);
            pong_ok()
        }
        ipc::IpcMessage::SetDepotKey { depot_id, key } => {
            depot_keys().write().unwrap().insert(depot_id, key);
            pong_ok()
        }
        ipc::IpcMessage::RemoveDepotKey { depot_id } => {
            depot_keys().write().unwrap().remove(&depot_id);
            pong_ok()
        }
        ipc::IpcMessage::FetchManifestCode { gid } => {
            let code = manifest_codes()
                .read()
                .unwrap()
                .get(&gid)
                .cloned()
                .unwrap_or_else(|| gid.clone());
            manifest_codes()
                .write()
                .unwrap()
                .insert(gid.clone(), code.clone());
            Some(ipc::IpcMessage::ManifestCodeResponse { gid, code })
        }
        ipc::IpcMessage::FetchManifestCodeEx {
            app_id: _,
            depot_id: _,
            gid,
        } => {
            let code = manifest_codes()
                .read()
                .unwrap()
                .get(&gid)
                .cloned()
                .unwrap_or_else(|| gid.clone());
            manifest_codes()
                .write()
                .unwrap()
                .insert(gid.clone(), code.clone());
            Some(ipc::IpcMessage::ManifestCodeResponse { gid, code })
        }
        ipc::IpcMessage::SetAppIdSpoof { app_id, spoof_id } => {
            app_id_spoofs().write().unwrap().insert(app_id, spoof_id);
            pong_ok()
        }
        ipc::IpcMessage::RemoveAppIdSpoof { app_id } => {
            app_id_spoofs().write().unwrap().remove(&app_id);
            pong_ok()
        }
        ipc::IpcMessage::SetManifestPin {
            app_id,
            depot_id,
            manifest_id,
            size: _,
        } => {
            manifest_pins()
                .write()
                .unwrap()
                .insert((app_id, depot_id), manifest_id.clone());
            Some(ipc::IpcMessage::ManifestBound {
                app_id,
                depot_id,
                manifest_id,
                active: true,
            })
        }
        ipc::IpcMessage::RemoveManifestPin { app_id, depot_id } => {
            manifest_pins().write().unwrap().remove(&(app_id, depot_id));
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

#[ctor]
fn init() {
    libsteam_integration_init();
}
