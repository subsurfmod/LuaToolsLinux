//! InteropSteamUI - SteamUI hooks for library UI manipulation.
//!
//! Port of usefulsteamtools `Interop_SteamUI.cpp`.
//! Handles FillInAppOverview, BuildCompleteAppOverviewChange, and CSteamUIAppControllerRunFrame.

use std::collections::HashMap;
use std::os::raw::c_void;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Mutex, OnceLock};

use crate::steam_bridge::memory_patch::{self, MemoryPatch, JMP_ABS64_LEN};
use crate::steam_bridge::pattern_db::{BinaryComponent, PatternChannel, PatternDb};

/// App state flags.
#[allow(dead_code)]
pub mod EAppState {
    pub const k_EAppStateUpdateRunning: u32 = 1 << 5;
    pub const k_EAppStateUpdateStarted: u32 = 1 << 6;
    pub const k_EAppStateUninstalled: u32 = 1 << 0;
}

/// App ownership flags.
#[allow(dead_code)]
pub mod EAppOwnershipFlags {
    pub const None: u32 = 0;
}

/// App change flags.
#[allow(dead_code)]
pub mod EAppChangeFlags {
    pub const AppInfoOrConfig: u32 = 1;
}

/// CSteamApp structure matching Steam's internal layout.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct CSteamApp {
    pub vfptr: *mut c_void,
    pub game_id: u64,
    pub n_app_id: u32,
    pub unknown1: u16,
    pub unknown2: u16,
    pub release_state: u32,
    pub ownership_flags: u32,
    pub app_state_flags: u32,
    pub steam_id: u64,
    pub purchased_time: u32,
    pub change_number: u32,
    pub license_expiration_time: u32,
    pub master_sub_app_id: u32,
    pub proto_app_type: u32,
    pub parent_app_id: u32,
}

/// CAppOverview_Change structure (simplified).
#[repr(C)]
pub struct CAppOverviewChange {
    pub removed_appid: *mut c_void, // RepeatedField<uint32>*
}

/// Global state.
static ACTIVE_DOWNLOADS: OnceLock<Mutex<HashMap<u32, bool>>> = OnceLock::new();
static PENDING_REMOVALS: OnceLock<Mutex<Vec<u32>>> = OnceLock::new();
static REMOVED_APP_IDS: OnceLock<Mutex<HashMap<u32, bool>>> = OnceLock::new();

/// Installed patches.
static INSTALLED_STEAMUI_PATCHES: OnceLock<Mutex<Vec<MemoryPatch>>> = OnceLock::new();

/// Function pointer types.
type RepeatedFieldUint32AddFn = unsafe extern "C" fn(*mut c_void, *const u32);
type GetAppByIDFn = unsafe extern "C" fn(*mut c_void, u32, bool) -> *mut CSteamApp;
type MarkAppChangeFn = unsafe extern "C" fn(*mut c_void, u32, u32) -> *mut c_void;
type FillInAppOverviewFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut CSteamApp) -> *mut c_void;
type BuildCompleteAppOverviewChangeFn = unsafe extern "C" fn(*mut c_void, *mut CAppOverviewChange, *mut c_void);
type CSteamUIAppControllerRunFrameFn = unsafe extern "C" fn(*mut c_void) -> *mut c_void;

/// Original function pointers.
static ORIGINAL_REPEATED_FIELD_UINT32_ADD: OnceLock<Mutex<Option<RepeatedFieldUint32AddFn>>> = OnceLock::new();
static ORIGINAL_GET_APP_BY_ID: OnceLock<Mutex<Option<GetAppByIDFn>>> = OnceLock::new();
static ORIGINAL_MARK_APP_CHANGE: OnceLock<Mutex<Option<MarkAppChangeFn>>> = OnceLock::new();
static ORIGINAL_FILL_IN_APP_OVERVIEW: OnceLock<Mutex<Option<FillInAppOverviewFn>>> = OnceLock::new();
static ORIGINAL_BUILD_COMPLETE_APP_OVERVIEW_CHANGE: OnceLock<Mutex<Option<BuildCompleteAppOverviewChangeFn>>> = OnceLock::new();
static ORIGINAL_CSTEAM_UI_APP_CONTROLLER_RUN_FRAME: OnceLock<Mutex<Option<CSteamUIAppControllerRunFrameFn>>> = OnceLock::new();

fn active_downloads() -> &'static Mutex<HashMap<u32, bool>> {
    ACTIVE_DOWNLOADS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn pending_removals() -> &'static Mutex<Vec<u32>> {
    PENDING_REMOVALS.get_or_init(|| Mutex::new(Vec::new()))
}

fn removed_app_ids() -> &'static Mutex<HashMap<u32, bool>> {
    REMOVED_APP_IDS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn installed_steamui_patches() -> &'static Mutex<Vec<MemoryPatch>> {
    INSTALLED_STEAMUI_PATCHES.get_or_init(|| Mutex::new(Vec::new()))
}

fn original_repeated_field_uint32_add() -> &'static Mutex<Option<RepeatedFieldUint32AddFn>> {
    ORIGINAL_REPEATED_FIELD_UINT32_ADD.get_or_init(|| Mutex::new(None))
}

fn original_get_app_by_id() -> &'static Mutex<Option<GetAppByIDFn>> {
    ORIGINAL_GET_APP_BY_ID.get_or_init(|| Mutex::new(None))
}

fn original_mark_app_change() -> &'static Mutex<Option<MarkAppChangeFn>> {
    ORIGINAL_MARK_APP_CHANGE.get_or_init(|| Mutex::new(None))
}

fn original_fill_in_app_overview() -> &'static Mutex<Option<FillInAppOverviewFn>> {
    ORIGINAL_FILL_IN_APP_OVERVIEW.get_or_init(|| Mutex::new(None))
}

fn original_build_complete_app_overview_change() -> &'static Mutex<Option<BuildCompleteAppOverviewChangeFn>> {
    ORIGINAL_BUILD_COMPLETE_APP_OVERVIEW_CHANGE.get_or_init(|| Mutex::new(None))
}

fn original_csteam_ui_app_controller_run_frame() -> &'static Mutex<Option<CSteamUIAppControllerRunFrameFn>> {
    ORIGINAL_CSTEAM_UI_APP_CONTROLLER_RUN_FRAME.get_or_init(|| Mutex::new(None))
}

/// Detour for FillInAppOverview.
unsafe extern "C" fn fill_in_app_overview_detour(
    this: *mut c_void,
    app_overview: *mut c_void,
    app: *mut CSteamApp,
) -> *mut c_void {
    panic::catch_unwind(AssertUnwindSafe(|| {
        if !app.is_null() {
            let app_ref = &*app;

            // Track active downloads
            let active = (app_ref.app_state_flags & (EAppState::k_EAppStateUpdateRunning | EAppState::k_EAppStateUpdateStarted)) != 0;
            {
                let mut downloads = active_downloads().lock().unwrap_or_else(|e| e.into_inner());
                if active {
                    downloads.insert(app_ref.n_app_id, true);
                } else {
                    downloads.remove(&app_ref.n_app_id);
                }
            }

            // Apply purchase time from Lua config
            if has_depot(app_ref.n_app_id) {
                if let Some(purchase_time) = get_purchase_time(app_ref.n_app_id) {
                    // We can't directly modify the struct since it's a const ref
                    // In the Windows version this modifies pApp->PurchasedTime
                    log::trace!("[InteropSteamUI] FillInAppOverview: would set PurchasedTime={} for appId={}", purchase_time, app_ref.n_app_id);
                }
            }
        }

        let orig = *original_fill_in_app_overview()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(orig_fn) = orig {
            orig_fn(this, app_overview, app)
        } else {
            std::ptr::null_mut()
        }
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// Detour for BuildCompleteAppOverviewChange.
unsafe extern "C" fn build_complete_app_overview_change_detour(
    controller: *mut c_void,
    change: *mut CAppOverviewChange,
    optional_callback_slot: *mut c_void,
) {
    panic::catch_unwind(AssertUnwindSafe(|| {
        let orig = *original_build_complete_app_overview_change()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(orig_fn) = orig {
            orig_fn(controller, change, optional_callback_slot);
        }

        // Append removed app IDs
        let repeated_field_add = *original_repeated_field_uint32_add()
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let removed = removed_app_ids().lock().unwrap_or_else(|e| e.into_inner());
        if !removed.is_empty() && !change.is_null() && repeated_field_add.is_some() {
            let field = (*change).removed_appid;
            for (app_id, _) in removed.iter() {
                unsafe { repeated_field_add.unwrap()(field, app_id) };
            }
            log::debug!("[InteropSteamUI] BuildCompleteAppOverviewChange: appended {} removed_appid entries", removed.len());
        }
    }))
    .unwrap_or(());
}

/// Detour for CSteamUIAppControllerRunFrame.
unsafe extern "C" fn csteam_ui_app_controller_run_frame_detour(
    controller: *mut c_void,
) -> *mut c_void {
    panic::catch_unwind(AssertUnwindSafe(|| {
        let get_app_by_id = *original_get_app_by_id()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mark_app_change = *original_mark_app_change()
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        if get_app_by_id.is_some() && mark_app_change.is_some() {
            let mut draining = Vec::new();
            {
                let mut pending = pending_removals().lock().unwrap_or_else(|e| e.into_inner());
                draining.append(&mut *pending);
            }

            for app_id in draining {
                if is_owned(app_id) {
                    log::debug!("[InteropSteamUI] RunFrame: appId {} is owned again, skipping removal", app_id);
                    continue;
                }
                if let Some(get_app_fn) = get_app_by_id {
                    let app_ptr = unsafe { get_app_fn(controller, app_id, false) };
                    if !app_ptr.is_null() {
                        let app_ref = &mut *app_ptr;
                        // Only remove from library if not already uninstalled
                        app_ref.ownership_flags = EAppOwnershipFlags::None;
                        if app_ref.app_state_flags == EAppState::k_EAppStateUninstalled {
                            removed_app_ids().lock().unwrap_or_else(|e| e.into_inner()).insert(app_id, true);
                        }
                    }
                }

                if let Some(mark_fn) = mark_app_change {
                    unsafe { mark_fn(controller, app_id, EAppChangeFlags::AppInfoOrConfig); }
                }
            }
        }

        let orig = *original_csteam_ui_app_controller_run_frame()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(orig_fn) = orig {
            orig_fn(controller)
        } else {
            std::ptr::null_mut()
        }
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// Check if a depot is owned (placeholder - integrates with LuaConfig).
fn has_depot(_app_id: u32) -> bool {
    // This would be populated by the Lua config system
    false
}

/// Get purchase time for an app (placeholder - integrates with LuaConfig).
fn get_purchase_time(_app_id: u32) -> Option<u32> {
    // This would be populated by the Lua config system
    None
}

/// Check if an app is owned (placeholder - integrates with LuaConfig).
fn is_owned(_app_id: u32) -> bool {
    // This would be populated by the Lua config system
    false
}

/// Resolve function address using pattern scanning.
fn resolve_function_address(
    steam_root: &std::path::Path,
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
            crate::steam_bridge::manifest_hooks::parse_pattern_and_mask(pattern_hex, entry.mask.as_deref())
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

/// Queue a removal for the UI thread to process.
pub fn queue_removal(app_id: u32) {
    log::debug!("[InteropSteamUI] QueueRemoval: {}", app_id);
    pending_removals().lock().unwrap_or_else(|e| e.into_inner()).push(app_id);
}

/// Cancel a queued removal.
pub fn cancel_removal(app_id: u32) {
    log::debug!("[InteropSteamUI] CancelRemoval: {}", app_id);
    let mut pending = pending_removals().lock().unwrap_or_else(|e| e.into_inner());
    pending.retain(|&x| x != app_id);
    removed_app_ids().lock().unwrap_or_else(|e| e.into_inner()).remove(&app_id);
}

/// Get active download count.
pub fn active_download_count() -> usize {
    active_downloads().lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// Install SteamUI hooks.
pub fn install(steam_root: &std::path::Path) -> Result<(), String> {
    log::info!("Installing InteropSteamUI hooks...");

    // Capture GetAppByID
    if let Ok(addr) = resolve_function_address(
        steam_root,
        BinaryComponent::SteamUi,
        PatternChannel::Pattern,
        "GetAppByID",
    ) {
        let fn_ptr: GetAppByIDFn = unsafe { std::mem::transmute(addr as usize) };
        *original_get_app_by_id()
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(fn_ptr);
        log::info!("Resolved GetAppByID at 0x{:x}", addr);
    }

    // Capture MarkAppChange
    if let Ok(addr) = resolve_function_address(
        steam_root,
        BinaryComponent::SteamUi,
        PatternChannel::Pattern,
        "MarkAppChange",
    ) {
        let fn_ptr: MarkAppChangeFn = unsafe { std::mem::transmute(addr as usize) };
        *original_mark_app_change()
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(fn_ptr);
        log::info!("Resolved MarkAppChange at 0x{:x}", addr);
    }

    // Capture RepeatedFieldUint32Add
    if let Ok(addr) = resolve_function_address(
        steam_root,
        BinaryComponent::SteamUi,
        PatternChannel::Pattern,
        "RepeatedFieldUint32Add",
    ) {
        let fn_ptr: RepeatedFieldUint32AddFn = unsafe { std::mem::transmute(addr as usize) };
        *original_repeated_field_uint32_add()
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(fn_ptr);
        log::info!("Resolved RepeatedFieldUint32Add at 0x{:x}", addr);
    }

    // Install FillInAppOverview hook
    if let Ok(addr) = resolve_function_address(
        steam_root,
        BinaryComponent::SteamUi,
        PatternChannel::Pattern,
        "FillInAppOverview",
    ) {
        let replacement = fill_in_app_overview_detour as *const () as *mut u8;
        let patch = unsafe { memory_patch::patch_by_offset(addr, 0, replacement) }
            .map_err(|e| format!("Failed to install FillInAppOverview hook: {}", e))?;

        if !patch.trampoline.is_null() {
            let orig_fn: FillInAppOverviewFn = unsafe { std::mem::transmute(patch.trampoline) };
            *original_fill_in_app_overview()
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(orig_fn);
        }
        installed_steamui_patches()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(patch);
        log::info!("Installed FillInAppOverview hook at 0x{:x}", addr);
    }

    // Install BuildCompleteAppOverviewChange hook
    if let Ok(addr) = resolve_function_address(
        steam_root,
        BinaryComponent::SteamUi,
        PatternChannel::Pattern,
        "BuildCompleteAppOverviewChange",
    ) {
        let replacement = build_complete_app_overview_change_detour as *const () as *mut u8;
        let patch = unsafe { memory_patch::patch_by_offset(addr, 0, replacement) }
            .map_err(|e| format!("Failed to install BuildCompleteAppOverviewChange hook: {}", e))?;

        if !patch.trampoline.is_null() {
            let orig_fn: BuildCompleteAppOverviewChangeFn = unsafe { std::mem::transmute(patch.trampoline) };
            *original_build_complete_app_overview_change()
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(orig_fn);
        }
        installed_steamui_patches()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(patch);
        log::info!("Installed BuildCompleteAppOverviewChange hook at 0x{:x}", addr);
    }

    // Install CSteamUIAppControllerRunFrame hook
    if let Ok(addr) = resolve_function_address(
        steam_root,
        BinaryComponent::SteamUi,
        PatternChannel::Pattern,
        "CSteamUIAppControllerRunFrame",
    ) {
        let replacement = csteam_ui_app_controller_run_frame_detour as *const () as *mut u8;
        let patch = unsafe { memory_patch::patch_by_offset(addr, 0, replacement) }
            .map_err(|e| format!("Failed to install CSteamUIAppControllerRunFrame hook: {}", e))?;

        if !patch.trampoline.is_null() {
            let orig_fn: CSteamUIAppControllerRunFrameFn = unsafe { std::mem::transmute(patch.trampoline) };
            *original_csteam_ui_app_controller_run_frame()
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(orig_fn);
        }
        installed_steamui_patches()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(patch);
        log::info!("Installed CSteamUIAppControllerRunFrame hook at 0x{:x}", addr);
    }

    log::info!("InteropSteamUI hooks installed");
    Ok(())
}

/// Uninstall SteamUI hooks.
pub fn uninstall() -> Result<(), String> {
    *original_repeated_field_uint32_add()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *original_get_app_by_id()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *original_mark_app_change()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *original_fill_in_app_overview()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *original_build_complete_app_overview_change()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *original_csteam_ui_app_controller_run_frame()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;

    let mut patches = installed_steamui_patches()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for mut patch in patches.drain(..) {
        unsafe { patch.uninstall() }?;
    }

    log::info!("InteropSteamUI hooks uninstalled");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_steamui_structs() {
        assert_eq!(std::mem::size_of::<CSteamApp>(), 0x48); // 72 bytes
    }

    #[test]
    fn test_queue_and_cancel_removal() {
        queue_removal(12345);
        assert_eq!(pending_removals().lock().unwrap().len(), 1);

        cancel_removal(12345);
        assert_eq!(pending_removals().lock().unwrap().len(), 0);
    }
}