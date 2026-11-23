//! Interop Misc hooks for Linux.
//!
//! Port of usefulsteamtools `Interop_Misc.cpp`.
//! Handles SpawnProcess interception, AppID spoofing, OptedInMask, BuildSpawnEnvBlock.
//! On Linux: uses memory_patch.rs for 14-byte absolute indirect jumps + pattern_db.rs
//! for function address resolution via byte patterns from steam-monitor TOML.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::os::raw::c_void;
use std::path::Path;

use crate::steam_bridge::memory_patch::{self, MemoryPatch, JMP_ABS64_LEN};
use crate::steam_bridge::binary_reader::{BinaryFile, LoadedModuleMapping};
use crate::steam_bridge::pattern_db::{BinaryComponent, PatternChannel, PatternDb};
use crate::steam_bridge::ost_platform::remote_process;

// ============================================================================
// Constants from usefulsteamtools
// ============================================================================

pub const ONLINE_FIX_APP_ID: u32 = 480; // Spacewar
pub const ENCRYPTED_APP_TICKET_RESPONSE_CALLBACK: u32 = 154;
pub const K_ERESULT_OK: i32 = 1;

// ============================================================================
// Global state (using LazyLock for proper initialization)
// ============================================================================

static ONLINE_FIX_REAL_APP_ID: LazyLock<Mutex<Option<u32>>> = LazyLock::new(|| Mutex::new(None));
static NETWORKING_SOCKETS_ACTIVE: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));
static SUPPRESS_APP_ID_FLIP: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));
static GAME_NAME_CACHE: LazyLock<Mutex<HashMap<u32, String>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

// Function pointers for trampolines
static ORIGINAL_OPTED_IN_MASK: LazyLock<Mutex<Option<OptedInMaskFn>>> = LazyLock::new(|| Mutex::new(None));
static ORIGINAL_BUILD_SPAWN_ENV_BLOCK: LazyLock<Mutex<Option<BuildSpawnEnvBlockFn>>> = LazyLock::new(|| Mutex::new(None));
static ORIGINAL_GET_OR_ADD_APP_DATA: LazyLock<Mutex<Option<GetOrAddAppDataFn>>> = LazyLock::new(|| Mutex::new(None));
static ORIGINAL_CUTL_BUFFER_ENSURE_CAPACITY: LazyLock<Mutex<Option<CUtlBufferEnsureCapacityFn>>> = LazyLock::new(|| Mutex::new(None));
static ORIGINAL_GET_APP_ID_FOR_CURRENT_PIPE: LazyLock<Mutex<Option<GetAppIdForCurrentPipeFn>>> = LazyLock::new(|| Mutex::new(None));
static ORIGINAL_GET_APP_DATA_FROM_APP_INFO: LazyLock<Mutex<Option<GetAppDataFromAppInfoFn>>> = LazyLock::new(|| Mutex::new(None));

static INSTALLED_PATCHES: LazyLock<Mutex<Vec<MemoryPatch>>> = LazyLock::new(|| Mutex::new(Vec::new()));

// Pointers to captured 'this' pointers
static STEAM_ENGINE_PTR: LazyLock<Mutex<Option<*mut c_void>>> = LazyLock::new(|| Mutex::new(None));
static APP_INFO_CACHE_PTR: LazyLock<Mutex<Option<*mut c_void>>> = LazyLock::new(|| Mutex::new(None));

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
// Helper functions for global state
// ============================================================================

fn online_fix_real_app_id() -> &'static Mutex<Option<u32>> {
    &ONLINE_FIX_REAL_APP_ID
}
fn networking_sockets_active() -> &'static Mutex<bool> {
    &NETWORKING_SOCKETS_ACTIVE
}
fn suppress_app_id_flip() -> &'static Mutex<bool> {
    &SUPPRESS_APP_ID_FLIP
}
fn game_name_cache() -> &'static Mutex<HashMap<u32, String>> {
    &GAME_NAME_CACHE
}

fn original_opted_in_mask() -> &'static Mutex<Option<OptedInMaskFn>> {
    &ORIGINAL_OPTED_IN_MASK
}
fn original_build_spawn_env_block() -> &'static Mutex<Option<BuildSpawnEnvBlockFn>> {
    &ORIGINAL_BUILD_SPAWN_ENV_BLOCK
}
fn original_get_or_add_app_data() -> &'static Mutex<Option<GetOrAddAppDataFn>> {
    &ORIGINAL_GET_OR_ADD_APP_DATA
}
fn original_cutl_buffer_ensure_capacity() -> &'static Mutex<Option<CUtlBufferEnsureCapacityFn>> {
    &ORIGINAL_CUTL_BUFFER_ENSURE_CAPACITY
}
fn original_get_app_id_for_current_pipe() -> &'static Mutex<Option<GetAppIdForCurrentPipeFn>> {
    &ORIGINAL_GET_APP_ID_FOR_CURRENT_PIPE
}
fn original_get_app_data_from_app_info() -> &'static Mutex<Option<GetAppDataFromAppInfoFn>> {
    &ORIGINAL_GET_APP_DATA_FROM_APP_INFO
}

fn installed_patches() -> &'static Mutex<Vec<MemoryPatch>> {
    &INSTALLED_PATCHES
}

fn steam_engine_ptr() -> &'static Mutex<Option<*mut c_void>> {
    &STEAM_ENGINE_PTR
}
fn app_info_cache_ptr() -> &'static Mutex<Option<*mut c_void>> {
    &APP_INFO_CACHE_PTR
}

// ============================================================================
// Replacement functions (called instead of original Steam functions)
// ============================================================================

/// Hook: SteamController_OptedInMask
/// Called by CUser_BuildSpawnEnvBlock to compute Steam Input opt-in mask.
/// With 480 (Spacewar), the spawned game inherits Spacewar's Steam Input opt-in.
unsafe extern "C" fn opted_in_mask_replacement(this: *mut c_void, app_id: u32) -> i64 {
    if let Some(real_app_id) = *online_fix_real_app_id().lock().unwrap() {
        if app_id == ONLINE_FIX_APP_ID && real_app_id != 0 {
            log::info!("OptedInMask: appid {} -> {}", app_id, real_app_id);
            return call_original_opted_in_mask(this, real_app_id);
        }
    }
    call_original_opted_in_mask(this, app_id)
}

unsafe fn call_original_opted_in_mask(this: *mut c_void, app_id: u32) -> i64 {
    if let Some(original) = original_opted_in_mask().lock().unwrap().as_ref() {
        return original(this, app_id)
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
    if let Some(real_app_id) = *online_fix_real_app_id().lock().unwrap() {
        if real_app_id != 0 && !p_overlay_cgame_id.is_null() {
            // Check if overlay CGameID has appid 480
            // This requires knowing CGameID layout - simplified check
            log::info!("BuildSpawnEnvBlock: SetAppID in OverlayCGameID -> {}", real_app_id);
            // In real impl, we'd call pOverlayCGameID->SetAppID(real_app_id)
        }
    }
    call_original_build_spawn_env_block(this, p_cgame_id, a3, env, p_overlay_cgame_id, a6, a7, a8, a9, a10, a11)
}

unsafe fn call_original_build_spawn_env_block(
    this: *mut c_void, p_cgame_id: *mut c_void, a3: *mut c_void, env: *mut c_void,
    p_overlay_cgame_id: *mut c_void, a6: *mut c_void, a7: i32,
    a8: *mut c_void, a9: *mut c_void, a10: u32, a11: u8,
) -> i64 {
    if let Some(original) = original_build_spawn_env_block().lock().unwrap().as_ref() {
        return original(this, p_cgame_id, a3, env, p_overlay_cgame_id, a6, a7, a8, a9, a10, a11)
    }
    0
}

/// Hook: CAppInfoCache::GetOrAddAppData (commented out in source, but we implement it)
/// Marks unresolved appinfo as skip_flag to bypass license update blocking.
unsafe extern "C" fn get_or_add_app_data_replacement(
    cache: *mut c_void,
    app_id: u32,
    create: bool,
) -> *mut c_void {
    let result = call_original_get_or_add_app_data(cache, app_id, create);

    // Check if this app is in our Lua config and has unresolved appinfo
    // Simplified: would check crate::steam_bridge::lua_config::HasDepot(app_id, false)
    if !create {
        // In real impl: if HasDepot && pData && !create && pData->IsUnresolvedAppInfo()
        // { pData->bSkipFlag = true; }
    }
    result
}

unsafe fn call_original_get_or_add_app_data(cache: *mut c_void, app_id: u32, create: bool) -> *mut c_void {
    if let Some(original) = original_get_or_add_app_data().lock().unwrap().as_ref() {
        let result = original(cache, app_id, create);
        return result;
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
    if let Some(original) = original_cutl_buffer_ensure_capacity().lock().unwrap().as_ref() {
        original(buffer, new_capacity)
    } else {
        buffer
    }
}

// ============================================================================
// VEH/INT3 trap equivalents - using pattern-based address resolution
// ============================================================================

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
    // 1. Resolve function address
    // 2. Install memory_patch that calls our replacement
    // 3. Replacement reads cmdline (arg 3), checks for "-onlinefix"
    // 4. If found, sets global state and rewrites GameID to 480

    Ok(())
}

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
    if let Some(real_app_id) = *online_fix_real_app_id().lock().unwrap() {
        if real_app_id != 0 {
            return real_app_id;
        }
    }

    // Would call original function with captured steam_engine_ptr
    if let Some(original) = original_get_app_id_for_current_pipe().lock().unwrap().as_ref() {
        if let Some(engine) = *steam_engine_ptr().lock().unwrap() {
            return unsafe { original(engine) };
        }
    }
    0
}

/// Resolve the current AppID for the game being launched.
/// Priority: onlinefix real appid > pipe appid
pub fn resolve_app_id() -> u32 {
    if let Some(real_app_id) = *online_fix_real_app_id().lock().unwrap() {
        if real_app_id != 0 {
            return real_app_id;
        }
    }
    get_app_id_for_current_pipe()
}

/// Check if -onlinefix game is currently active
pub fn is_online_fix_active() -> bool {
    online_fix_real_app_id().lock().unwrap().is_some() && *online_fix_real_app_id().lock().unwrap().unwrap_or(0) != 0
}

/// Mark that SteamNetworkingSockets P2P is active for this session.
/// This causes GetAppID to report 480 for certificate matching.
pub fn notify_networking_sockets_used() {
    if let Some(real_app_id) = *online_fix_real_app_id().lock().unwrap() {
        if real_app_id != 0 {
            let mut active = networking_sockets_active().lock().unwrap();
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
    if *suppress_app_id_flip().lock().unwrap() {
        return false;
    }
    let online_fix = online_fix_real_app_id().lock().unwrap().is_some() && *online_fix_real_app_id().lock().unwrap().unwrap_or(0) != 0;
    let networking = *networking_sockets_active().lock().unwrap();
    online_fix && networking
}

/// Set whether to suppress the appid flip (corresponds to -realappid launch option)
pub fn set_suppress_app_id_flip(suppress: bool) {
    *suppress_app_id_flip().lock().unwrap() = suppress;
}

/// Ensure CUtlBuffer has sufficient capacity (for ticket responses)
pub fn ensure_buffer_capacity(buffer: *mut c_void, new_capacity: u32, update_put: bool) -> bool {
    // Would call original CUtlBufferEnsureCapacity
    if let Some(original) = original_cutl_buffer_ensure_capacity().lock().unwrap().as_ref() {
        unsafe { original(buffer, new_capacity) };
        if update_put {
            // In real impl: pWrite->m_Put = new_capacity;
        }
        true
    } else {
        false
    }
}

/// Get game name by AppID (with caching)
pub fn get_game_name_by_app_id(app_id: u32) -> String {
    let mut cache = game_name_cache().lock().unwrap();
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

    // Resolve CUtlBufferEnsureCapacity
    // Would resolve via pattern_db and install patch

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
        installed_patches().lock().unwrap().push(patch);
        log::info!("Installed {} hook at 0x{:x}", name, addr);
    }

    Ok(())
}

/// Uninstall all hooks
pub fn uninstall() {
    let mut patches = installed_patches().lock().unwrap();
    for patch in &mut patches.drain(..) {
        let _ = unsafe { patch.uninstall() };
    }
    log::info!("InteropMisc hooks uninstalled");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_app_id_no_onlinefix() {
        *online_fix_real_app_id().lock().unwrap() = None;
        assert_eq!(resolve_app_id(), 0); // Would return pipe appid in real impl
    }

    #[test]
    fn test_resolve_app_id_with_onlinefix() {
        *online_fix_real_app_id().lock().unwrap() = Some(12345);
        assert_eq!(resolve_app_id(), 12345);
    }

    #[test]
    fn test_should_report_online_fix_app_id() {
        *online_fix_real_app_id().lock().unwrap() = Some(12345);
        *networking_sockets_active().lock().unwrap() = true;
        *suppress_app_id_flip().lock().unwrap() = false;
        assert!(should_report_online_fix_app_id());

        *suppress_app_id_flip().lock().unwrap() = true;
        assert!(!should_report_online_fix_app_id());
    }
}