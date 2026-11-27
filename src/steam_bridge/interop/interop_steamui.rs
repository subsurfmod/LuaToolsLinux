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
    pub vfptr: *mut c_void,         // C++ polymorphic vtable pointer at offset 0x00
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
            let app_ref = unsafe { &mut *app };

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
                    if purchase_time > 0 {
                        app_ref.purchased_time = purchase_time;
                        log::trace!(
                            "[InteropSteamUI] FillInAppOverview: set PurchasedTime={} for appId={}",
                            purchase_time,
                            app_ref.n_app_id
                        );
                    }
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

static PURCHASE_TIMES: OnceLock<Mutex<HashMap<u32, u32>>> = OnceLock::new();

fn purchase_times() -> &'static Mutex<HashMap<u32, u32>> {
    PURCHASE_TIMES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Set purchased time for an app ID.
pub fn set_purchase_time(app_id: u32, time: u32) {
    let mut map = purchase_times().lock().unwrap_or_else(|e| e.into_inner());
    if time > 0 {
        let entry = map.entry(app_id).or_insert(0);
        if time > *entry {
            *entry = time;
        }
    } else {
        map.remove(&app_id);
    }
}

/// Get purchase time for an app ID.
pub fn get_purchase_time(app_id: u32) -> Option<u32> {
    purchase_times()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&app_id)
        .copied()
        .filter(|&t| t > 0)
}

/// Clear all registered purchase times.
pub fn clear_purchase_times() {
    purchase_times().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Check if a depot is owned (integrates with interop_package).
pub fn has_depot(app_id: u32) -> bool {
    crate::steam_bridge::interop::interop_package::has_depot_ex(app_id, false)
}

/// Check if an app is owned (integrates with interop_package).
pub fn is_owned(app_id: u32) -> bool {
    crate::steam_bridge::interop::interop_package::is_owned(app_id)
}

/// Resolve function address using pattern scanning.
fn resolve_function_address(
    steam_root: &std::path::Path,
    component: BinaryComponent,
    channel: PatternChannel,
    pattern_name: &str,
) -> Result<u64, String> {
    let expected_class = if cfg!(target_pointer_width = "64") {
        crate::steam_bridge::binary_reader::ElfClass::Class64
    } else {
        crate::steam_bridge::binary_reader::ElfClass::Class32
    };

    let binaries = crate::steam_bridge::binary_reader::scan_steam_binaries(steam_root);
    let binary = binaries
        .into_iter()
        .find(|b| {
            b.class == expected_class
                && b.path
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

    if pf.is_fallback() {
        return Err("Fallback patterns not permitted for in-process memory hooks".to_string());
    }

    let entry = pf
        .patterns
        .get(pattern_name)
        .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;

    if entry.is_generic_compiler_prologue() {
        return Err(format!("Pattern '{}' is a generic compiler prologue", pattern_name));
    }

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
        let patch = unsafe { memory_patch::patch_by_offset_with_callback(addr, 0, replacement, |trampoline| {
            let orig_fn: FillInAppOverviewFn = unsafe { std::mem::transmute(trampoline as *const ()) };
            *original_fill_in_app_overview().lock().unwrap_or_else(|e| e.into_inner()) = Some(orig_fn);
        }) }
            .map_err(|e| format!("Failed to install FillInAppOverview hook: {}", e))?;
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
        let patch = unsafe { memory_patch::patch_by_offset_with_callback(addr, 0, replacement, |trampoline| {
            let orig_fn: BuildCompleteAppOverviewChangeFn = unsafe { std::mem::transmute(trampoline as *const ()) };
            *original_build_complete_app_overview_change().lock().unwrap_or_else(|e| e.into_inner()) = Some(orig_fn);
        }) }
            .map_err(|e| format!("Failed to install BuildCompleteAppOverviewChange hook: {}", e))?;
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
        let patch = unsafe { memory_patch::patch_by_offset_with_callback(addr, 0, replacement, |trampoline| {
            let orig_fn: CSteamUIAppControllerRunFrameFn = unsafe { std::mem::transmute(trampoline as *const ()) };
            *original_csteam_ui_app_controller_run_frame().lock().unwrap_or_else(|e| e.into_inner()) = Some(orig_fn);
        }) }
            .map_err(|e| format!("Failed to install CSteamUIAppControllerRunFrame hook: {}", e))?;
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

    #[test]
    fn test_purchase_time_registry_and_csteamapp_write() {
        clear_purchase_times();
        assert_eq!(get_purchase_time(480), None);

        set_purchase_time(480, 1609459200);
        assert_eq!(get_purchase_time(480), Some(1609459200));

        // Setting a larger time updates it, smaller does not overwrite
        set_purchase_time(480, 1600000000);
        assert_eq!(get_purchase_time(480), Some(1609459200));
        set_purchase_time(480, 1700000000);
        assert_eq!(get_purchase_time(480), Some(1700000000));

        // Create a mock CSteamApp and simulate FillInAppOverview modifying PurchasedTime
        let mut app = CSteamApp {
            vfptr: std::ptr::null_mut(),
            game_id: 480,
            n_app_id: 480,
            unknown1: 0,
            unknown2: 0,
            release_state: 1,
            ownership_flags: 1,
            app_state_flags: 0,
            steam_id: 76561198000000000,
            purchased_time: 0,
            change_number: 1,
            license_expiration_time: 0,
            master_sub_app_id: 0,
            proto_app_type: 0,
            parent_app_id: 0,
        };

        // Inject depot so has_depot(480) returns true
        crate::steam_bridge::interop::interop_package::add_owned_depot_id(480);
        assert!(has_depot(480));

        // Call fill_in_app_overview_detour with app pointer
        unsafe {
            fill_in_app_overview_detour(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut app as *mut CSteamApp,
            );
        }

        // Must have directly modified app.purchased_time in memory
        assert_eq!(app.purchased_time, 1700000000);

        clear_purchase_times();
    }
}