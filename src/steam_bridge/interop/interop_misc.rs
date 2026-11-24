//! Interop Misc hooks for Linux.
//!
//! Port of usefulsteamtools `Interop_Misc.cpp`.
//! Handles SpawnProcess interception, AppID spoofing, OptedInMask, and BuildSpawnEnvBlock.

use std::collections::HashMap;
use std::os::raw::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

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

unsafe extern "C" fn opted_in_mask_replacement(this: *mut c_void, app_id: u32) -> i64 {
    let real = ONLINE_FIX_REAL_APP_ID.load(Ordering::Acquire);
    let target_app = if app_id == ONLINE_FIX_APP_ID && real != 0 {
        log::info!("OptedInMask: appid {} -> {}", app_id, real);
        real
    } else {
        app_id
    };

    if let Some(m) = ORIGINAL_OPTED_IN_MASK.get() {
        if let Some(orig_fn) = *m.lock().unwrap_or_else(|e| e.into_inner()) {
            return orig_fn(this, target_app);
        }
    }
    0
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
    let real = ONLINE_FIX_REAL_APP_ID.load(Ordering::Acquire);
    if real != 0 && !p_overlay_cgame_id.is_null() {
        log::info!("BuildSpawnEnvBlock: SetAppID in OverlayCGameID -> {}", real);
    }
    if let Some(m) = ORIGINAL_BUILD_SPAWN_ENV_BLOCK.get() {
        if let Some(orig_fn) = *m.lock().unwrap_or_else(|e| e.into_inner()) {
            return orig_fn(
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
            );
        }
    }
    0
}

unsafe extern "C" fn get_or_add_app_data_replacement(
    cache: *mut c_void,
    app_id: u32,
    create: bool,
) -> *mut c_void {
    if let Some(m) = ORIGINAL_GET_OR_ADD_APP_DATA.get() {
        if let Some(orig_fn) = *m.lock().unwrap_or_else(|e| e.into_inner()) {
            return orig_fn(cache, app_id, create);
        }
    }
    std::ptr::null_mut()
}

unsafe extern "C" fn cutl_buffer_ensure_capacity_replacement(
    buffer: *mut c_void,
    new_capacity: u32,
) -> *mut c_void {
    log::debug!(
        "CUtlBufferEnsureCapacity: buffer={:p}, new_capacity={}",
        buffer,
        new_capacity
    );
    if let Some(m) = ORIGINAL_CUTL_BUFFER_ENSURE_CAPACITY.get() {
        if let Some(orig_fn) = *m.lock().unwrap_or_else(|e| e.into_inner()) {
            return orig_fn(buffer, new_capacity);
        }
    }
    buffer
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
        let pattern_bytes = parse_hex_pattern(pattern_hex);
        let mask_bytes = entry.mask.as_ref().map(|m| parse_hex_pattern(m));
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

fn parse_hex_pattern(hex: &str) -> Vec<u8> {
    hex.split_whitespace()
        .filter_map(|s| u8::from_str_radix(s, 16).ok())
        .collect()
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
        if let Some(m) = ORIGINAL_GET_APP_ID_FOR_CURRENT_PIPE.get() {
            if let Some(orig_fn) = *m.lock().unwrap_or_else(|e| e.into_inner()) {
                return unsafe { orig_fn(engine) };
            }
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
    if let Some(m) = ORIGINAL_CUTL_BUFFER_ENSURE_CAPACITY.get() {
        if let Some(orig_fn) = *m.lock().unwrap_or_else(|e| e.into_inner()) {
            unsafe {
                orig_fn(buffer, new_capacity);
            }
            return true;
        }
    }
    false
}

/// Get game name by AppID (with caching).
pub fn get_game_name_by_app_id(app_id: u32) -> String {
    let mut cache = game_name_cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(name) = cache.get(&app_id) {
        return name.clone();
    }
    let name = format!("App_{}", app_id);
    cache.insert(app_id, name.clone());
    name
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

        // Reset shared globals
        set_online_fix_real_app_id(0);
        set_suppress_app_id_flip(false);
    }
}
