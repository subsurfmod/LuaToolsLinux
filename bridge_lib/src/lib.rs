//! Steam client integration shared library.
//!
//! This cdylib is built as `libsteam_integration.so` and loaded into the Steam
//! client process via `LD_PRELOAD`. A constructor runs at load time, scans the
//! Steam client binaries for integration points, installs memory patches, and
//! starts an IPC server to receive commands from the LuaTools GUI.

#![allow(clippy::missing_safety_doc)]

use std::collections::HashMap;
use std::collections::HashSet;
use std::os::raw::c_void;
use std::sync::{Once, OnceLock, RwLock};

use ctor::ctor;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use luatools_linux::steam_bridge::{
    binary_reader::{scan_steam_binaries, BinaryFile},
    ipc::{self, IpcServer},
    memory_patch::{self, MemoryPatch},
    pattern_db::{BinaryComponent, PatternChannel, PatternDb},
    ticket_store::{AppTicketData, TicketStore},
};

type BisSubscribedAppFn = unsafe extern "C" fn(*mut c_void, u32) -> bool;
type GetAppIdFn = unsafe extern "C" fn(*mut c_void) -> u32;

static SPOOFED_APPS: OnceLock<RwLock<HashSet<u32>>> = OnceLock::new();
static APP_ID_SPOOFS: OnceLock<RwLock<HashMap<u32, u32>>> = OnceLock::new();
static DEPOT_KEYS: OnceLock<RwLock<HashMap<u32, String>>> = OnceLock::new();
static STATS: OnceLock<RwLock<HashMap<(u32, String), String>>> = OnceLock::new();
static LEGACY_CDKEYS: OnceLock<RwLock<HashMap<u32, String>>> = OnceLock::new();
static MANIFEST_CODES: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();
static MANIFEST_PINS: OnceLock<RwLock<HashMap<(u32, u32), String>>> = OnceLock::new();

static ORIGINAL_BIS: OnceLock<RwLock<Option<BisSubscribedAppFn>>> = OnceLock::new();
static ORIGINAL_GET_APP_ID: OnceLock<RwLock<Option<GetAppIdFn>>> = OnceLock::new();

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

fn manifest_codes() -> &'static RwLock<HashMap<String, String>> {
    MANIFEST_CODES.get_or_init(|| RwLock::new(HashMap::new()))
}

fn manifest_pins() -> &'static RwLock<HashMap<(u32, u32), String>> {
    MANIFEST_PINS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn original_bis() -> &'static RwLock<Option<BisSubscribedAppFn>> {
    ORIGINAL_BIS.get_or_init(|| RwLock::new(None))
}

fn original_get_app_id() -> &'static RwLock<Option<GetAppIdFn>> {
    ORIGINAL_GET_APP_ID.get_or_init(|| RwLock::new(None))
}

fn installed_patches() -> &'static RwLock<Vec<MemoryPatch>> {
    INSTALLED_PATCHES.get_or_init(|| RwLock::new(Vec::new()))
}

/// Replacement for `ISteamUser::BIsSubscribedApp` / `ISteamApps::BIsSubscribedApp`.
/// Returns true (owned) for app IDs in the spoof set; otherwise delegates to the
/// original function.
unsafe extern "C" fn bis_subscribed_app_replacement(
    this: *mut c_void,
    app_id: u32,
) -> bool {
    let spoofed = spoofed_apps().read().unwrap();
    if spoofed.contains(&app_id) {
        return true;
    }
    drop(spoofed);

    if let Some(original) = original_bis().read().unwrap().as_ref() {
        return original(this, app_id);
    }
    false
}

/// Replacement for `ISteamClient::GetSteamAppId` / similar app ID queries.
/// Returns the spoofed app ID if configured; otherwise delegates to the original.
unsafe extern "C" fn get_app_id_replacement(this: *mut c_void) -> u32 {
    // Check if this call is for a specific app that has a spoof configured.
    // The caller can check the spoof map via IPC to see if any spoofs are active.
    let spoofs = app_id_spoofs().read().unwrap();
    if !spoofs.is_empty() {
        // Return the first configured spoof as a default; individual app spoofs
        // are handled per-context by the caller checking the map.
        if let Some((&_, &spoof_id)) = spoofs.iter().next() {
            drop(spoofs);
            return spoof_id;
        }
    }
    drop(spoofs);

    if let Some(original) = original_get_app_id().read().unwrap().as_ref() {
        return original(this);
    }
    0
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

fn run_integration() -> Result<(), String> {
    let steam_root = find_steam_root();

    let binaries = scan_steam_binaries(&steam_root);

    let mut steam_client: Option<BinaryFile> = None;

    for binary in &binaries {
        let name = binary.path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.contains("steamclient") {
            steam_client = Some(binary.clone());
        }
    }

    let pattern_db = PatternDb::new();

    // Resolve BIsSubscribedApp pattern from steamclient
    if let Some(ref client) = steam_client {
        match pattern_db.lookup_by_binary(
            client,
            PatternChannel::Pattern,
            BinaryComponent::SteamClient,
        ) {
            Ok(pf) => {
                if let Some(entry) = pf.patterns.get("bis_subscribed_app") {
                    if let Some(pattern_hex) = &entry.pattern {
                        let pattern_bytes = parse_hex_pattern(pattern_hex);
                        let mask_bytes = entry
                            .mask
                            .as_ref()
                            .and_then(|m| Some(parse_hex_pattern(m)));
                        let pattern_offset = entry.offset.unwrap_or(0);

                        if let Some(target_addr) = client.resolve_pattern_address(
                            &pattern_bytes,
                            mask_bytes.as_deref(),
                            pattern_offset,
                            None,
                        ) {
                            install_bis_subscribed_app_patch(target_addr);
                        }
                    }
                }

                if let Some(entry) = pf.patterns.get("get_app_id") {
                    if let Some(pattern_hex) = &entry.pattern {
                        let pattern_bytes = parse_hex_pattern(pattern_hex);
                        let mask_bytes = entry
                            .mask
                            .as_ref()
                            .and_then(|m| Some(parse_hex_pattern(m)));
                        let pattern_offset = entry.offset.unwrap_or(0);

                        if let Some(target_addr) = client.resolve_pattern_address(
                            &pattern_bytes,
                            mask_bytes.as_deref(),
                            pattern_offset,
                            None,
                        ) {
                            install_get_app_id_patch(target_addr);
                        }
                    }
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

    // Start IPC server for GUI communication
    let mut server = IpcServer::new();
    server.set_handler(|msg| handle_ipc_message(msg));
    server.start().map_err(|e| format!("IPC server failed: {}", e))?;

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

    let result = unsafe { memory_patch::patch_by_offset(target_addr, 0, replacement_ptr) };

    match result {
        Ok(mut patch) => {
            if let Err(e) = unsafe { patch.install(14) } {
                eprintln!(
                    "[steam_integration] failed to install BIsSubscribedApp patch: {}",
                    e
                );
                return;
            }

            unsafe {
                let trampoline: BisSubscribedAppFn = std::mem::transmute(patch.trampoline);
                let mut orig = original_bis().write().unwrap();
                *orig = Some(trampoline);
            }

            installed_patches()
                .write()
                .unwrap()
                .push(patch);

            eprintln!(
                "[steam_integration] BIsSubscribedApp patch installed at 0x{:x}",
                target_addr
            );
        }
        Err(e) => {
            eprintln!(
                "[steam_integration] failed to create BIsSubscribedApp patch: {}",
                e
            );
        }
    }
}

fn install_get_app_id_patch(target_addr: u64) {
    let replacement_ptr = get_app_id_replacement as *mut c_void as *mut u8;

    let result = unsafe { memory_patch::patch_by_offset(target_addr, 0, replacement_ptr) };

    match result {
        Ok(mut patch) => {
            if let Err(e) = unsafe { patch.install(14) } {
                eprintln!(
                    "[steam_integration] failed to install GetAppId patch: {}",
                    e
                );
                return;
            }

            unsafe {
                let trampoline: GetAppIdFn = std::mem::transmute(patch.trampoline);
                let mut orig = original_get_app_id().write().unwrap();
                *orig = Some(trampoline);
            }

            installed_patches()
                .write()
                .unwrap()
                .push(patch);

            eprintln!(
                "[steam_integration] GetAppId patch installed at 0x{:x}",
                target_addr
            );
        }
        Err(e) => {
            eprintln!(
                "[steam_integration] failed to create GetAppId patch: {}",
                e
            );
        }
    }
}

fn find_steam_root() -> std::path::PathBuf {
    let candidates = [
        dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".local/share/Steam"),
        dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".steam/steam"),
    ];

    for c in &candidates {
        if c.exists() {
            return c.clone();
        }
    }

    candidates[0].clone()
}

fn handle_ipc_message(msg: ipc::IpcMessage) -> Option<ipc::IpcMessage> {
    match msg {
        ipc::IpcMessage::Ping => Some(ipc::IpcMessage::Pong {
            active: true,
            version: env!("CARGO_PKG_VERSION").to_string(),
        }),
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
            let _ = store.set(data);
            Some(ipc::IpcMessage::Pong {
                active: true,
                version: env!("CARGO_PKG_VERSION").to_string(),
            })
        }
        ipc::IpcMessage::RemoveTicket { app_id } => {
            let config_dir = find_steam_root().join("config");
            let mut store = TicketStore::new(&config_dir);
            let _ = store.load();
            let _ = store.remove(app_id);
            Some(ipc::IpcMessage::Pong {
                active: true,
                version: env!("CARGO_PKG_VERSION").to_string(),
            })
        }
        ipc::IpcMessage::AddPlayUnownedApp { app_id } => {
            spoofed_apps().write().unwrap().insert(app_id);
            Some(ipc::IpcMessage::Pong {
                active: true,
                version: env!("CARGO_PKG_VERSION").to_string(),
            })
        }
        ipc::IpcMessage::RemovePlayUnownedApp { app_id } => {
            spoofed_apps().write().unwrap().remove(&app_id);
            Some(ipc::IpcMessage::Pong {
                active: true,
                version: env!("CARGO_PKG_VERSION").to_string(),
            })
        }
        ipc::IpcMessage::SetPlayUnowned { .. } => Some(ipc::IpcMessage::Pong {
            active: true,
            version: env!("CARGO_PKG_VERSION").to_string(),
        }),
        ipc::IpcMessage::SetStat {
            app_id,
            stat_name,
            stat_value,
        } => {
            stats()
                .write()
                .unwrap()
                .insert((app_id, stat_name), stat_value);
            Some(ipc::IpcMessage::Pong {
                active: true,
                version: env!("CARGO_PKG_VERSION").to_string(),
            })
        }
        ipc::IpcMessage::SetLegacyCdKey { app_id, key } => {
            legacy_cdkeys()
                .write()
                .unwrap()
                .insert(app_id, key);
            Some(ipc::IpcMessage::Pong {
                active: true,
                version: env!("CARGO_PKG_VERSION").to_string(),
            })
        }
        ipc::IpcMessage::SetDepotKey { depot_id, key } => {
            depot_keys()
                .write()
                .unwrap()
                .insert(depot_id, key);
            Some(ipc::IpcMessage::Pong {
                active: true,
                version: env!("CARGO_PKG_VERSION").to_string(),
            })
        }
        ipc::IpcMessage::FetchManifestCode { gid: _ } => {
            // Manifest codes are resolved via the upstream API, not stored locally.
            Some(ipc::IpcMessage::Pong {
                active: true,
                version: env!("CARGO_PKG_VERSION").to_string(),
            })
        }
        ipc::IpcMessage::FetchManifestCodeEx {
            app_id: _,
            depot_id: _,
            gid: _,
        } => {
            // Manifest codes are resolved via the upstream API, not stored locally.
            Some(ipc::IpcMessage::Pong {
                active: true,
                version: env!("CARGO_PKG_VERSION").to_string(),
            })
        }
        ipc::IpcMessage::SetAppIdSpoof {
            app_id,
            spoof_id,
        } => {
            app_id_spoofs().write().unwrap().insert(app_id, spoof_id);
            Some(ipc::IpcMessage::Pong {
                active: true,
                version: env!("CARGO_PKG_VERSION").to_string(),
            })
        }
        ipc::IpcMessage::RemoveAppIdSpoof { app_id } => {
            app_id_spoofs().write().unwrap().remove(&app_id);
            Some(ipc::IpcMessage::Pong {
                active: true,
                version: env!("CARGO_PKG_VERSION").to_string(),
            })
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
        ipc::IpcMessage::RemoveManifestPin {
            app_id,
            depot_id,
        } => {
            manifest_pins().write().unwrap().remove(&(app_id, depot_id));
            Some(ipc::IpcMessage::ManifestBound {
                app_id,
                depot_id,
                manifest_id: String::new(),
                active: false,
            })
        }
        _ => Some(ipc::IpcMessage::Pong {
            active: true,
            version: env!("CARGO_PKG_VERSION").to_string(),
        }),
    }
}

#[ctor]
fn init() {
    libsteam_integration_init();
}
