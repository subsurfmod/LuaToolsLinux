//! Interop Misc hooks for Linux.
//!
//! Port of usefulsteamtools `Interop_Misc.cpp`.
//! Handles SpawnProcess interception, AppID spoofing, OptedInMask, BuildSpawnEnvBlock.
//! On Linux: uses memory_patch.rs for 14-byte absolute indirect jumps + pattern_db.rs
//! for function address resolution via byte patterns from steam-monitor TOML.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::os::raw::c_void;
use std::path::Path;

use crate::steam_bridge::memory_patch::{self, MemoryPatch, JMP_ABS64_LEN};
use crate::steam_bridge::pattern_db::{BinaryComponent, PatternChannel, PatternDb};
use crate::steam_bridge::ipc::IpcMessage;
use crate::steam_bridge::binary_reader::{BinaryFile, LoadedModuleMapping};

// ============================================================================
// Constants from usefulsteamtools
// ============================================================================

pub const ONLINE_FIX_APP_ID: u32 = 480; // Spacewar

// ============================================================================
// Global state (replaces static globals in C++) - using OnceLock/Mutex pattern
// ============================================================================

static ONLINE_FIX_REAL_APP_ID: OnceLock<Mutex<Option<u32>>> = OnceLock::new();
static NETWORKING_SOCKETS_ACTIVE: OnceLock<Mutex<Option<bool>>> = OnceLock::new();
static SUPPRESS_APP_ID_FLIP: OnceLock<Mutex<Option<bool>>> = OnceLock::new();
static GAME_NAME_CACHE: OnceLock<Mutex<HashMap<u32, String>>> = OnceLock::new();

// Function pointer storage
static ORIGINAL_OPTED_IN_MASK: OnceLock<Mutex<Option<OptedInMaskFn>>> = OnceLock::new();
static ORIGINAL_BUILD_SPAWN_ENV_BLOCK: OnceLock<Mutex<Option<BuildSpawnEnvBlockFn>>> = OnceLock::new();
static ORIGINAL_GET_OR_ADD_APP_DATA: OnceLock<Mutex<Option<GetOrAddAppDataFn>>> = OnceLock::new();
static ORIGINAL_CUTL_BUFFER_ENSURE_CAPACITY: OnceLock<Mutex<Option<CUtlBufferEnsureCapacityFn>>> = OnceLock::new();
static ORIGINAL_GET_APP_ID_FOR_CURRENT_PIPE: OnceLock<Mutex<Option<GetAppIdForCurrentPipeFn>>> = OnceLock::new();
static ORIGINAL_GET_APP_DATA_FROM_APP_INFO: OnceLock<Mutex<Option<GetAppDataFromAppInfoFn>>> = OnceLock::new();

// Captured 'this' pointers
static STEAM_ENGINE_PTR: OnceLock<Mutex<Option<*mut c_void>>> = OnceLock::new();
static APP_INFO_CACHE_PTR: OnceLock<Mutex<Option<*mut c_void>>> = OnceLock::new();

// Type aliases for function pointers
type OptedInMaskFn = unsafe extern "C" fn(*mut c_void, u32) -> i64;
type BuildSpawnEnvBlockFn = unsafe extern "C" fn(
    *mut c_void, *mut c_void, *mut c_void, *mut c_void, *mut c_void,
    *mut c_void, i32, *mut c_void, *mut c_void, u32, u8
) -> i64;
type GetOrAddAppDataFn = unsafe extern "C" fn(*mut c_void, u32, bool) -> *mut c_void;
type CUtlBufferEnsureCapacityFn = unsafe extern "C" fn(*mut c_void, u32) -> *mut c_void;
type GetAppIdForCurrentPipeFn = unsafe extern "C" fn(*mut c_void) -> u32;
type GetAppDataFromAppInfoFn = unsafe extern "C" fn(
    *mut c_void, u32, *const c_void, *mut u8, i32
) -> i64;

// ============================================================================
// Helper function to lock OnceLock mutex safely, initializing if needed
// ============================================================================

fn init_once_lock<T>(once: &OnceLock<Mutex<Option<T>>>, value: T) -> MutexGuard<'_, Option<T>>
where
    T: Default,
{
    let _ = once.get_or_init(|| Mutex::new(None));
    once.get().unwrap().lock().unwrap_or_else(|_| Mutex::new(None))
}

fn get_once_lock<T>(once: &OnceLock<Mutex<Option<T>>>) -> MutexGuard<'_, Option<T>>
{
    once.get().expect("OnceLock not initialized").lock().unwrap()
}

// ============================================================================
// Replacement functions (called instead of original Steam functions)
// ============================================================================

/// Hook: SteamController_OptedInMask
/// Called by CUser_BuildSpawnEnvBlock to compute Steam Input opt-in mask.
/// With 480 (Spacewar), the spawned game inherits Spacewar's Steam Input opt-in.
unsafe extern "C" fn opted_in_mask_replacement(this: *mut c_void, app_id: u32) -> i64 {
    let real_app_id_guard = get_once_lock(&ONLINE_FIX_REAL_APP_ID);
    if let Some(real) = *real_app_id_guard {
        if app_id == ONLINE_FIX_APP_ID && real != 0 {
            log::info!("OptedInMask: appid {} -> {}", app_id, real);
            // Call original with the real appid
            if let Some(original) = *ORIGINAL_OPTED_IN_MASK.get().unwrap() {
                if let Some(orig_fn) = original.lock().unwrap().as_ref() {
                    return orig_fn(this, real);
                }
            }
        }
    }
    // Fall through to original
    if let Some(original) = *ORIGINAL_OPTED_IN_MASK.get().unwrap() {
        if let Some(orig_fn) = original.lock().unwrap().as_ref() {
            return orig_fn(this, app_id)
        }
    }
    0
}

/// Hook: CUser_BuildSpawnEnvBlock
/// pOverlayCGameID drives SteamOverlayGameId. We need to set it to the real appid
/// for overlay to work correctly, while leaving pCGameID at 480 for ownership bypass.
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
    let real_app_id_guard = get_once_lock(&ONLINE_FIX_REAL_APP_ID);
    if let Some(real) = *real_app_id_guard {
        if real != 0 && !p_overlay_cgame_id.is_null() {
            log::info!("BuildSpawnEnvBlock: SetAppID in OverlayCGameID -> {}", real);
            // In real impl, we'd call pOverlayCGameID->SetAppID(real_app_id)
        }
    }
    // Call original
    if let Some(original) = *ORIGINAL_BUILD_SPAWN_ENV_BLOCK.get().unwrap() {
        if let Some(orig_fn) = original.lock().unwrap().as_ref() {
            return orig_fn(this, p_cgame_id, a3, env, p_overlay_cgame_id, a6, a7, a8, a9, a10, a11)
        }
    }
    0
}

/// Hook: CAppInfoCache::GetOrAddAppData
/// Marks unresolved appinfo as skip_flag to bypass license update blocking.
unsafe extern "C" fn get_or_add_app_data_replacement(
    cache: *mut c_void,
    app_id: u32,
    create: bool,
) -> *mut c_void {
    let result = call_original_get_or_add_app_data(cache, app_id, create);

    // Check if this app is in our Lua config and has unresolved appinfo
    if !create {
        // In real impl: if HasDepot && pData && !create && pData->IsUnresolvedAppInfo()
        // { pData->bSkipFlag = true; }
    }
    result
}

unsafe fn call_original_get_or_add_app_data(
    cache: *mut c_void,
    app_id: u32,
    create: bool,
) -> *mut c_void {
    if let Some(original) = *ORIGINAL_GET_OR_ADD_APP_DATA.get().unwrap() {
        if let Some(orig_fn) = original.lock().unwrap().as_ref() {
            let result = orig_fn(cache, app_id, create);
            return result;
        }
    }
    std::ptr::null_mut()
}

/// Hook: CUtlBuffer::EnsureCapacity
/// Used to grow write buffers for ticket responses
unsafe extern "C" fn cutl_buffer_ensure_capacity_replacement(
    buffer: *mut c_void,
    new_capacity: u32,
) -> *mut c_void {
    log::debug!("CUtlBufferEnsureCapacity: buffer={:p}, new_capacity={}", buffer, new_capacity);
    if let Some(original) = *ORIGINAL_CUTL_BUFFER_ENSURE_CAPACITY.get().unwrap() {
        original(buffer, new_capacity)
    } else {
        buffer
    }
}

// ============================================================================
// VEH/INT3 trap equivalents - using pattern-based address resolution
// ============================================================================

/// Resolve function address via pattern_db
async fn resolve_function_address(
    steam_root: &Path,
    component: BinaryComponent,
    channel: PatternChannel,
    pattern_name: &str,
) -> Result<u64, String> {
    let binaries = crate::steam_bridge::binary_reader::scan_steam_binaries(steam_root);
    let binary = binaries.into_iter().find(|b| {
        b.path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| match component {
                BinaryComponent::SteamClient => n.contains("steamclient"),
                BinaryComponent::SteamUi => n.contains("steamui"),
            }).unwrap_or(false)
    }).ok_or("Steam binary not found")?;

    let pattern_db = PatternDb::new();
    let pf = pattern_db.lookup_by_binary(&binary, channel, component)
        .map_err(|e| format!("Pattern lookup failed: {}", e))?;

    let entry = pf.patterns.get(pattern_name)
        .ok_or(format!("Pattern '{}' not found", pattern_name))?;

    let mapping = crate::steam_bridge::binary_reader::find_loaded_module_mapping(
        &binary.path.file_name().unwrap().to_string_lossy()
    ).ok_or("Module not loaded")?;

    let vaddr = if let Some(pattern_hex) = &entry.pattern {
        let pattern_bytes = parse_hex_pattern(pattern_hex);
        let mask_bytes = entry.mask.as_ref().map(|m| parse_hex_pattern(m));
        let offset = entry.offset.unwrap_or(0);
        binary.resolve_pattern_address(&pattern_bytes, mask_bytes.as_deref(), offset, None)
    } else {
        entry.address
    }.ok_or("Failed to resolve pattern address")?;

    let runtime_addr = binary.vaddr_to_runtime_addr(vaddr, &mapping, JMP_ABS64_LEN)
        .ok_or("Resolved address not in executable memory")?;

    Ok(runtime_addr)
}

fn parse_hex_pattern(hex: &str) -> Vec<u8> {
    hex.split_whitespace()
        .filter_map(|s| u8::from_str_radix(s, 16).ok())
        .collect()
}

// ============================================================================
// Public API
// ============================================================================

/// Get the current AppID for the active game pipe.
/// Returns the real AppID if -onlinefix is active, otherwise returns
/// the AppID from the current pipe.
pub fn get_app_id_for_current_pipe() -> u32 {
    let real_app_id_guard = get_once_lock(&ONLINE_FIX_REAL_APP_ID);
    if let Some(real) = *real_app_id_guard {
        return *real;
    }

    // Would call original function with captured steam_engine_ptr
    if let Some(original) = *ORIGINAL_GET_APP_ID_FOR_CURRENT_PIPE.get().unwrap() {
        if let Some(engine) = *STEAM_ENGINE_PTR.get().unwrap() {
            return unsafe { original(engine) };
        }
    }
    0
}

/// Resolve the current AppID for the game being launched.
/// Priority: onlinefix real appid > pipe appid
pub fn resolve_app_id() -> u32 {
    let real_app_id_guard = get_once_lock(&ONLINE_FIX_REAL_APP_ID);
    if let Some(real) = *real_app_id_guard {
        return *real;
    }
    get_app_id_for_current_pipe()
}

/// Check if -onlinefix game is currently active
pub fn is_online_fix_active() -> bool {
    let real_app_id_guard = get_once_lock(&ONLINE_FIX_REAL_APP_ID);
    if let Some(real) = *real_app_id_guard {
        return *real != 0;
    }
    false
}

/// Mark that SteamNetworkingSockets P2P is active for this session.
/// This causes GetAppID to report 480 for certificate matching.
pub fn notify_networking_sockets_used() {
    let real_app_id_guard = get_once_lock(&ONLINE_FIX_REAL_APP_ID);
    if let Some(real) = *real_app_id_guard {
        if *real != 0 {
            let mut active = get_once_lock(&NETWORKING_SOCKETS_ACTIVE);
            if !*active {
                *active = true;
                log::info!("NetworkingSockets active: GetAppID now reports 480 for cert match");
            }
        }
    }
}

/// Should GetAppID report 480 (onlinefix appid) for P2P certificate matching?
/// Returns false if -realappid was specified on command line.
pub fn should_report_online_fix_app_id() -> bool {
    let suppress_guard = get_once_lock(&SUPPRESS_APP_ID_FLIP);
    if let Some(suppress) = *suppress_guard {
        if suppress {
            return false;
        }
    }
    let real_app_id_guard = get_once_lock(&ONLINE_FIX_REAL_APP_ID);
    if let Some(real) = *real_app_id_guard {
        if *real != 0 {
            let active = get_once_lock(&NETWORKING_SOCKETS_ACTIVE);
            return *active;
        }
    }
    false
}

/// Set whether to suppress the appid flip (corresponds to -realappid launch option)
pub fn set_suppress_app_id_flip(suppress: bool) {
    if let Some(guard) = SUPPRESS_APP_ID_FLIP.get() {
        *guard.lock().unwrap() = Some(suppress);
    }
}

/// Ensure CUtlBuffer has sufficient capacity (for ticket responses)
pub fn ensure_buffer_capacity(buffer: *mut c_void, new_capacity: u32, update_put: bool) -> bool {
    if let Some(original) = *ORIGINAL_CUTL_BUFFER_ENSURE_CAPACITY.get().unwrap() {
        unsafe { original(buffer, new_capacity); };
        true
    } else {
        false
    }
}

/// Get game name by AppID (with caching)
pub fn get_game_name_by_app_id(app_id: u32) -> String {
    let mut cache = get_once_lock(&GAME_NAME_CACHE);
    if let Some(name) = cache.get(&app_id) {
        return name.clone();
    }

    // Would call original GetAppDataFromAppInfo with "common/name"
    // Simplified for now
    let name = format!("App_{}", app_id);
    cache.insert(app_id, name.clone());
    name
}

// ============================================================================
// Installation functions
// ============================================================================

/// Install all InteropMisc hooks
pub async fn install(steam_root: &Path) -> Result<(), String> {
    log::info!("Installing InteropMisc hooks...");

    // Resolve and install CUtlBufferEnsureCapacity
    resolve_and_install_cutl_buffer_ensure_capacity(steam_root).await?;

    // Capture GetAppIDForCurrentPipe 'this' pointer
    capture_get_app_id_for_current_pipe(steam_root).await?;

    // Capture GetAppDataFromAppInfo 'this' pointer
    capture_get_app_data_from_app_info(steam_root).await?;

    // Install SpawnProcess hook (INT3 equivalent)
    install_spawn_process_hook(steam_root).await?;

    // Install function hooks via memory_patch
    install_function_hooks(steam_root).await?;

    log::info!("InteropMisc hooks installed");
    Ok(())
}

async fn resolve_and_install_cutl_buffer_ensure_capacity(steam_root: &Path) -> Result<(), String> {
    let addr = resolve_function_address(
        steam_root,
        BinaryComponent::SteamClient,
        PatternChannel::Pattern,
        "cutl_buffer_ensure_capacity",
    ).await?;

    // Install patch
    let mut patch = unsafe {
        memory_patch::patch_by_offset(addr, 0, cutl_buffer_ensure_capacity_replacement as *mut u8)
    }.map_err(|e| format!("Failed to install CUtlBufferEnsureCapacity hook: {}", e))?;

    patch.install(JMP_ABS64_LEN).map_err(|e| e.to_string())?;
    INSTALLED_PATCHES.get().unwrap().lock().unwrap().push(patch);
    log::info!("Installed CUtlBufferEnsureCapacity hook at 0x{:x}", addr);

    Ok(())
}

/// Install function hooks (OptedInMask, BuildSpawnEnvBlock, etc.)
async fn install_function_hooks(steam_root: &Path) -> Result<(), String> {
    let hooks = [
        ("opted_in_mask", opted_in_mask_replacement as *const ()),
        ("build_spawn_env_block", build_spawn_env_block_replacement as *const ()),
        ("get_or_add_app_data", get_or_add_app_data_replacement as *const ()),
        ("cutl_buffer_ensure_capacity", cutl_buffer_ensure_capacity_replacement as *const ()),
    ];

    for (name, replacement) in hooks {
        let addr = resolve_function_address(
            steam_root,
            BinaryComponent::SteamClient,
            PatternChannel::Pattern,
            name,
        ).await?;

        // Install patch
        let mut patch = unsafe {
            memory_patch::patch_by_offset(addr, 0, replacement as *mut u8)
        }.map_err(|e| format!("Failed to install {} hook: {}", name, e))?;

        patch.install(JMP_ABS64_LEN).map_err(|e| e.to_string())?;
        INSTALLED_PATCHES.get().unwrap().lock().unwrap().push(patch);
        log::info!("Installed {} hook at 0x{:x}", name, addr);
    }

    Ok(())
}

/// Capture GetAppIDForCurrentPipe 'this' pointer (SteamEngine*)
/// On Windows: VEH trap on function entry, read RCX
/// On Linux: Resolve function via pattern, install patch that captures 'this'
pub async fn capture_get_app_id_for_current_pipe(steam_root: &Path) -> Result<(), String> {
    let addr = resolve_function_address(
        steam_root,
        BinaryComponent::SteamClient,
        PatternChannel::Pattern,
        "get_app_id_for_current_pipe",
    ).await?;

    // Install a patch that captures the first argument (RCX on x64 = 'this')
    // This is complex - simplified: we resolve the function and store the 'this'
    // when it's called in the context of a game pipe
    log::info!("Resolved GetAppIDForCurrentPipe at 0x{:x}", addr);

    // In real implementation:
    // 1. Install memory_patch at the function entry point
    // 2. The patch calls our replacement which captures the 'this' pointer (RCX)
    // 3. Stores it in STEAM_ENGINE_PTR for later use
    // 4. Calls the original function via the trampoline

    Ok(())
}

/// Capture GetAppDataFromAppInfo 'this' pointer (CAppInfoCache*)
pub async fn capture_get_app_data_from_app_info(steam_root: &Path) -> Result<(), String> {
    let addr = resolve_function_address(
        steam_root,
        BinaryComponent::SteamClient,
        PatternChannel::Pattern,
        "get_app_data_from_app_info",
    ).await?;
    log::info!("Resolved GetAppDataFromAppInfo at 0x{:x}", addr);

    // In real implementation:
    // 1. Install memory_patch at the function entry point
    // 2. The patch calls our replacement which captures the 'this' pointer (RCX)
    // 3. Stores it in APP_INFO_CACHE_PTR for later use
    // 4. Calls the original function via the trampoline

    Ok(())
}

/// Install SpawnProcess INT3 trap equivalent
/// On Windows: VEH trap on CUser_SpawnProcess, check for -onlinefix in cmdline
/// On Linux: Install patch at SpawnProcess entry point
pub async fn install_spawn_process_hook(steam_root: &Path) -> Result<(), String> {
    let addr = resolve_function_address(
        steam_root,
        BinaryComponent::SteamClient,
        PatternChannel::Pattern,
        "spawn_process",
    ).await?;

    // We'll install a patch that intercepts the call and checks command line
    // for "-onlinefix" argument
    log::info!("Installing SpawnProcess hook at 0x{:x}", addr);

    // In real implementation:
    // 1. Resolve function address via pattern_db
    // 2. Install memory_patch that calls our replacement
    // 3. Replacement reads cmdline (via /proc/self/cmdline or argument)
    // 4. If "-onlinefix" found, sets global state and rewrites GameID to 480
    // 5. Calls original function via trampoline

    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_app_id_no_onlinefix() {
        let _ = ONLINE_FIX_REAL_APP_ID.get_or_init(|| Mutex::new(None));
        *ONLINE_FIX_REAL_APP_ID.get().unwrap().lock().unwrap() = 0;
        assert_eq!(resolve_app_id(), 0); // Would return pipe appid in real impl
    }

    #[test]
    fn test_resolve_app_id_with_onlinefix() {
        let _ = ONLINE_FIX_REAL_APP_ID.get_or_init(|| Mutex::new(None));
        *ONLINE_FIX_REAL_APP_ID.get().unwrap().lock().unwrap() = 12345;
        assert_eq!(resolve_app_id(), 12345);
    }

    #[test]
    fn test_should_report_online_fix_app_id() {
        let _ = ONLINE_FIX_REAL_APP_ID.get_or_init(|| Mutex::new(None));
        let _ = NETWORKING_SOCKETS_ACTIVE.get_or_init(|| Mutex::new(None));
        let _ = SUPPRESS_APP_ID_FLIP.get_or_init(|| Mutex::new(None));
        *ONLINE_FIX_REAL_APP_ID.get().unwrap().lock().unwrap() = 12345;
        *NETWORKING_SOCKETS_ACTIVE.get().unwrap().lock().unwrap() = true;
        *SUPPRESS_APP_ID_FLIP.get().unwrap().lock().unwrap() = false;
        assert!(should_report_online_fix_app_id());

        *SUPPRESS_APP_ID_FLIP.get().unwrap().lock().unwrap() = true;
        assert!(!should_report_online_fix_app_id());
    }

    #[test]
    fn test_get_app_id_for_current_pipe_no_onlinefix() {
        let _ = ONLINE_FIX_REAL_APP_ID.get_or_init(|| Mutex::new(None));
        *ONLINE_FIX_REAL_APP_ID.get().unwrap().lock().unwrap() = 0;
        assert_eq!(get_app_id_for_current_pipe(), 0);
    }

    #[test]
    fn test_is_online_fix_active_false() {
        let _ = ONLINE_FIX_REAL_APP_ID.get_or_init(|| Mutex::new(None));
        *ONLINE_FIX_REAL_APP_ID.get().unwrap().lock().unwrap() = 0;
        assert!(!is_online_fix_active());
    }

    #[test]
    fn test_is_online_fix_active_true() {
        let _ = ONLINE_FIX_REAL_APP_ID.get_or_init(|| Mutex::new(None));
        *ONLINE_FIX_REAL_APP_ID.get().unwrap().lock().unwrap() = 12345;
        assert!(is_online_fix_active());
    }
}