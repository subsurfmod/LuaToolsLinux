//! InteropPackage - Package 0 license synthesis and CheckAppOwnership hook.
//!
//! Port of usefulsteamtools `Interop_Package.cpp`.
//! Handles injecting depots into Steam's Package 0 (free sub) to make games appear owned.

use std::os::raw::c_void;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::steam_bridge::memory_patch::{self, MemoryPatch, JMP_ABS64_LEN};
use crate::steam_bridge::pattern_db::{BinaryComponent, PatternChannel, PatternDb};

/// Package ID for the free sub (Package 0).
pub const K_INJECTED_PACKAGE_ID: u32 = 0;
/// Hardcoded access token for the injected package.
pub const K_INJECTED_PKG_ACCESS_TOKEN: u64 = 10_660_652_434_190_618_804;

/// Global state for Package 0 license injection.
static LICENSE_INITIALIZED: AtomicBool = AtomicBool::new(false);
static LICENSE_REFRESH_PENDING: AtomicBool = AtomicBool::new(false);
static PENDING_LICENSES: OnceLock<Mutex<Vec<License>>> = OnceLock::new();
static OWNED_DEPOTS: OnceLock<Mutex<Vec<u32>>> = OnceLock::new();
static OWNED_DEPOTS_READY: AtomicBool = AtomicBool::new(false);
static OWNED_DEPOTS_DUMPED: AtomicBool = AtomicBool::new(false);
static C_USER_PTR: OnceLock<Mutex<Option<usize>>> = OnceLock::new();
static INJECTED_PKG_INFO_PTR: OnceLock<Mutex<Option<usize>>> = OnceLock::new();

/// Installed patches for uninstallation.
static INSTALLED_PACKAGE_PATCHES: OnceLock<Mutex<Vec<MemoryPatch>>> = OnceLock::new();

/// Function pointer types.
type CUtlMemoryGrowFn = unsafe extern "C" fn(usize, i32) -> usize;
type MarkLicenseAsChangedFn = unsafe extern "C" fn(usize, u32, bool) -> i64;
type ProcessPendingLicenseUpdatesFn = unsafe extern "C" fn(usize) -> bool;
type GetPackageInfoFn = unsafe extern "C" fn(usize, u32, u64) -> usize;
type CheckAppOwnershipFn = unsafe extern "C" fn(usize, u32, *mut AppOwnership) -> bool;

/// Original function pointers.
static ORIGINAL_CUTL_MEMORY_GROW: OnceLock<Mutex<Option<CUtlMemoryGrowFn>>> = OnceLock::new();
static ORIGINAL_MARK_LICENSE_AS_CHANGED: OnceLock<Mutex<Option<MarkLicenseAsChangedFn>>> = OnceLock::new();
static ORIGINAL_PROCESS_PENDING_LICENSE_UPDATES: OnceLock<Mutex<Option<ProcessPendingLicenseUpdatesFn>>> = OnceLock::new();
static ORIGINAL_GET_PACKAGE_INFO: OnceLock<Mutex<Option<GetPackageInfoFn>>> = OnceLock::new();
static ORIGINAL_CHECK_APP_OWNERSHIP: OnceLock<Mutex<Option<CheckAppOwnershipFn>>> = OnceLock::new();

/// License entry from CMsgClientLicenseList.
#[derive(Debug, Clone)]
pub struct License {
    pub package_id: u32,
    pub access_token: u64,
}

/// AppOwnership structure matching Steam's internal layout.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AppOwnership {
    pub package_id: u32,
    pub release_state: u32,
    pub steam_id32: u32,
    pub master_subscription_app_id: u32,
    pub trial_seconds: u32,
    pub exist_in_package_nums: u32,
    pub purchase_country_code: [u8; 4],
    pub time_stamp: u32,
    pub time_expire: u32,
    pub b_owns_license: bool,
    pub b_license_expired: bool,
    pub b_is_permanent: bool,
    pub b_low_violence: bool,
    pub b_free_license: bool,
    pub b_region_restricted: bool,
    pub b_license_pending: bool,
    pub b_retail_license: bool,
    pub b_auto_grant: bool,
    pub b_license_permanent: bool,
    pub b_guest_pass: bool,
    pub b_borrowed: bool,
    pub b_any_site_license: bool,
    pub b_all_site_licenses: bool,
    pub b_all_activation_required: bool,
    pub b_family_shared: bool,
}

/// CUtlVector structure for AppId_t.
#[repr(C)]
pub struct CUtlVectorAppId {
    pub memory: CUtlMemoryAppId,
    pub size: u32,
}

/// CUtlMemory structure for AppId_t.
#[repr(C)]
pub struct CUtlMemoryAppId {
    pub memory: *mut u32,
    pub allocation_count: u32,
    pub grow_size: u32,
}

/// PackageInfo structure matching Steam's internal layout.
#[repr(C)]
pub struct PackageInfo {
    pub package_id: u32,
    pub change_number: i32,
    pub pics_token: u64,
    pub billing_type: u32,
    pub license_type: u32,
    pub status: u32,
    pub sha1_hash: [u8; 20],
    pub p_package_info_node_begin: *mut c_void,
    pub p_extend_node_begin: *mut c_void,
    pub app_id_vec: CUtlVectorAppId,
    pub depot_id_vec: CUtlVectorAppId,
}

/// EPackageStatus enum.
#[allow(dead_code)]
pub mod EPackageStatus {
    pub const Available: u32 = 1;
    pub const Expired: u32 = 2;
    pub const Revoked: u32 = 3;
}

/// EAppReleaseState enum.
#[allow(dead_code)]
pub mod EAppReleaseState {
    pub const Released: u32 = 1;
    pub const PreLoadOnly: u32 = 2;
    pub const Beta: u32 = 3;
    pub const Alpha: u32 = 4;
    pub const RC: u32 = 5;
    pub const Internal: u32 = 6;
}

/// K_EAppOwnershipFlags enum.
#[allow(dead_code)]
pub mod EAppOwnershipFlags {
    pub const None: u32 = 0;
}

fn pending_licenses() -> &'static Mutex<Vec<License>> {
    PENDING_LICENSES.get_or_init(|| Mutex::new(Vec::new()))
}

fn owned_depots() -> &'static Mutex<Vec<u32>> {
    OWNED_DEPOTS.get_or_init(|| Mutex::new(Vec::new()))
}

fn c_user_ptr() -> &'static Mutex<Option<usize>> {
    C_USER_PTR.get_or_init(|| Mutex::new(None))
}

fn injected_pkg_info_ptr() -> &'static Mutex<Option<usize>> {
    INJECTED_PKG_INFO_PTR.get_or_init(|| Mutex::new(None))
}

fn installed_package_patches() -> &'static Mutex<Vec<MemoryPatch>> {
    INSTALLED_PACKAGE_PATCHES.get_or_init(|| Mutex::new(Vec::new()))
}

fn original_cutl_memory_grow() -> &'static Mutex<Option<CUtlMemoryGrowFn>> {
    ORIGINAL_CUTL_MEMORY_GROW.get_or_init(|| Mutex::new(None))
}

fn original_mark_license_as_changed() -> &'static Mutex<Option<MarkLicenseAsChangedFn>> {
    ORIGINAL_MARK_LICENSE_AS_CHANGED.get_or_init(|| Mutex::new(None))
}

fn original_process_pending_license_updates() -> &'static Mutex<Option<ProcessPendingLicenseUpdatesFn>> {
    ORIGINAL_PROCESS_PENDING_LICENSE_UPDATES.get_or_init(|| Mutex::new(None))
}

fn original_get_package_info() -> &'static Mutex<Option<GetPackageInfoFn>> {
    ORIGINAL_GET_PACKAGE_INFO.get_or_init(|| Mutex::new(None))
}

fn original_check_app_ownership() -> &'static Mutex<Option<CheckAppOwnershipFn>> {
    ORIGINAL_CHECK_APP_OWNERSHIP.get_or_init(|| Mutex::new(None))
}

/// Called when CMsgClientLicenseList is received.
pub fn on_license_list(licenses: Vec<License>) {
    let mut pending = pending_licenses().lock().unwrap_or_else(|e| e.into_inner());
    *pending = licenses;
    OWNED_DEPOTS_DUMPED.store(false, Ordering::Release);
}

/// Try to dump owned depots from pending licenses.
pub fn try_dump_owned_depots() {
    if OWNED_DEPOTS_DUMPED.load(Ordering::Acquire) {
        return;
    }
    let pending = pending_licenses().lock().unwrap_or_else(|e| e.into_inner());
    if pending.is_empty() {
        return;
    }
    if original_get_package_info().lock().unwrap_or_else(|e| e.into_inner()).is_none() {
        return;
    }

    OWNED_DEPOTS_DUMPED.store(true, Ordering::Release);
    dump_owned_depots(&pending);
}

fn dump_owned_depots(licenses: &[License]) {
    let mut total_depots = 0usize;
    let mut resolved = 0usize;
    let mut unresolved = 0usize;
    let mut owned = Vec::new();

    let get_pkg_info_opt = *original_get_package_info()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let Some(get_pkg_info) = get_pkg_info_opt else {
        return;
    };

    for lic in licenses {
        let pkg_ptr = unsafe { get_pkg_info(0, lic.package_id, lic.access_token) };
        if pkg_ptr == 0 {
            unresolved += 1;
            log::debug!("[InteropPackage] OwnedDepots: package {} did not resolve (token={})", lic.package_id, lic.access_token);
            continue;
        }

        resolved += 1;
        let pkg = unsafe { &*(pkg_ptr as *const PackageInfo) };
        total_depots += pkg.depot_id_vec.size as usize;

        for i in 0..pkg.depot_id_vec.size {
            let depot_id = unsafe { *pkg.depot_id_vec.memory.memory.add(i as usize) };
            if depot_id != 0 {
                owned.push(depot_id);
            }
        }

        let app_ids: Vec<u32> = (0..pkg.app_id_vec.size)
            .map(|i| unsafe { *pkg.app_id_vec.memory.memory.add(i as usize) })
            .collect();
        let depot_ids: Vec<u32> = (0..pkg.depot_id_vec.size)
            .map(|i| unsafe { *pkg.depot_id_vec.memory.memory.add(i as usize) })
            .collect();

        log::info!(
            "[InteropPackage] OwnedDepots: package {} apps={} depots={} | appids=[{}] | depotids=[{}]",
            lic.package_id,
            pkg.app_id_vec.size,
            pkg.depot_id_vec.size,
            app_ids.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", "),
            depot_ids.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", ")
        );
    }

    owned.sort();
    owned.dedup();

    {
        let mut depots = owned_depots().lock().unwrap_or_else(|e| e.into_inner());
        *depots = owned.clone();
        OWNED_DEPOTS_READY.store(true, Ordering::Release);
    }

    log::info!(
        "[InteropPackage] OwnedDepots: done — {} package(s) resolved, {} unresolved, {} depot entr(ies), {} unique",
        resolved, unresolved, total_depots, owned.len()
    );
}

/// Get owned depots for the ManifestDonor.
pub fn get_owned_depots() -> (bool, Vec<u32>) {
    let depots = owned_depots().lock().unwrap_or_else(|e| e.into_inner());
    (OWNED_DEPOTS_READY.load(Ordering::Acquire), depots.clone())
}

/// Check if a depot is owned by the user.
/// Checks both the owned depots cache (from license list) and Package 0's DepotIdVec.
pub fn has_depot(depot_id: u32) -> bool {
    // First check the owned depots cache
    {
        let depots = owned_depots().lock().unwrap_or_else(|e| e.into_inner());
        if depots.contains(&depot_id) {
            return true;
        }
    }

    // Also check Package 0's DepotIdVec directly (injected depots)
    let get_pkg_info_opt = *original_get_package_info()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(get_pkg_info) = get_pkg_info_opt {
        let pkg_ptr = unsafe { get_pkg_info(0, K_INJECTED_PACKAGE_ID, K_INJECTED_PKG_ACCESS_TOKEN) };
        if pkg_ptr != 0 {
            let pkg = unsafe { &*(pkg_ptr as *const PackageInfo) };
            for i in 0..pkg.depot_id_vec.size {
                let id = unsafe { *pkg.depot_id_vec.memory.memory.add(i as usize) };
                if id == depot_id {
                    return true;
                }
            }
        }
    }

    false
}

/// Check if an AppID is in additional_apps (Lua-unlocked, configured via addappid).
/// This checks the bridge_lib's additional apps store.
pub fn is_additional_app(app_id: u32) -> bool {
    crate::steam_bridge::interop::bridge_lib::is_additional_app(app_id)
}

/// Detour for CUtlMemoryGrow.
unsafe extern "C" fn cutl_memory_grow_detour(buffer: usize, grow_size: i32) -> usize {
    panic::catch_unwind(AssertUnwindSafe(|| {
        let orig = *original_cutl_memory_grow()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(orig_fn) = orig {
            return orig_fn(buffer, grow_size);
        }
        buffer
    }))
    .unwrap_or(buffer)
}

/// Detour for MarkLicenseAsChanged.
unsafe extern "C" fn mark_license_as_changed_detour(this: usize, package_id: u32, unk: bool) -> i64 {
    panic::catch_unwind(AssertUnwindSafe(|| {
        let orig = *original_mark_license_as_changed()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(orig_fn) = orig {
            return orig_fn(this, package_id, unk);
        }
        0
    }))
    .unwrap_or(0)
}

/// Detour for ProcessPendingLicenseUpdates.
unsafe extern "C" fn process_pending_license_updates_detour(this: usize) -> bool {
    panic::catch_unwind(AssertUnwindSafe(|| {
        let orig = *original_process_pending_license_updates()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(orig_fn) = orig {
            return orig_fn(this);
        }
        false
    }))
    .unwrap_or(false)
}

/// Detour for CheckAppOwnership.
unsafe extern "C" fn check_app_ownership_detour(
    this: usize,
    app_id: u32,
    p_own: *mut AppOwnership,
) -> bool {
    panic::catch_unwind(AssertUnwindSafe(|| {
        // Capture CUser pointer on first call
        {
            let mut c_user = c_user_ptr().lock().unwrap_or_else(|e| e.into_inner());
            if c_user.is_none() {
                *c_user = Some(this);
                log::debug!("[InteropPackage] CheckAppOwnership: captured CUser {:x}", this);
            }
        }

        // Try to initialize fake license
        try_init_fake_license_once();

        // Try to dump owned depots
        try_dump_owned_depots();

        // Check if we have a depot for this app
        if has_depot(app_id) {
            let orig = *original_check_app_ownership()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let result = if let Some(orig_fn) = orig {
                orig_fn(this, app_id, p_own)
            } else {
                false
            };

            if result && !p_own.is_null() {
                let own = &mut *p_own;
                if own.exist_in_package_nums > 1 {
                    // Actually owned — record so has_depot excludes it going forward
                    own.release_state = EAppReleaseState::Released;
                } else {
                    own.package_id = K_INJECTED_PACKAGE_ID;
                    own.release_state = EAppReleaseState::Released;
                    own.b_owns_license = true;
                    own.b_free_license = false;
                    return true;
                }
            }
            return result;
        }

        let orig = *original_check_app_ownership()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(orig_fn) = orig {
            orig_fn(this, app_id, p_own)
        } else {
            false
        }
    }))
    .unwrap_or(false)
}

fn try_init_fake_license_once() {
    if LICENSE_INITIALIZED.load(Ordering::Acquire) {
        return;
    }

    let get_pkg_info = match *original_get_package_info()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
    {
        Some(f) => f,
        None => return,
    };

    let pkg_ptr = unsafe { get_pkg_info(0, K_INJECTED_PACKAGE_ID, K_INJECTED_PKG_ACCESS_TOKEN) };
    if pkg_ptr == 0 {
        log::warn!("[InteropPackage] TryInitFakeLicenseOnce: GetPackageInfo returned null for injected package");
        return;
    }

    {
        let mut ptr = injected_pkg_info_ptr().lock().unwrap_or_else(|e| e.into_inner());
        *ptr = Some(pkg_ptr);
    }

    init_fake_license_once(pkg_ptr);
}

fn init_fake_license_once(pkg_ptr: usize) -> bool {
    let pkg = unsafe { &mut *(pkg_ptr as *mut PackageInfo) };

    if pkg.status != EPackageStatus::Available {
        log::warn!(
            "[InteropPackage] InitFakeLicenseOnce: package status is not Available ({}), skipping injection",
            pkg.status
        );
        return false;
    }

    // Get all depot IDs from Lua config (depot keys + additional apps + Lua entries)
    let depot_ids = get_all_depot_ids();
    if !depot_ids.is_empty() {
        // Add to AppIdVec (for license ownership checks)
        let old_app_size = pkg.app_id_vec.size;
        let num_app_to_add = depot_ids.len() as i32;
        log::info!(
            "[InteropPackage] InitFakeLicense(PackageId={}): adding {} apps to AppIdVec, oldSize={}",
            K_INJECTED_PACKAGE_ID, num_app_to_add, old_app_size
        );

        let cutl_grow = match *original_cutl_memory_grow()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
        {
            Some(f) => f,
            None => {
                log::warn!("[InteropPackage] InitFakeLicense: CUtlMemoryGrow not ready");
                return false;
            }
        };

        let vec_ptr = &mut pkg.app_id_vec as *mut _ as usize;
        let result = unsafe { cutl_grow(vec_ptr, num_app_to_add) };
        if result == 0 {
            log::warn!("[InteropPackage] InitFakeLicense: failed to grow AppId vector");
            return false;
        }

        for (i, &app_id) in depot_ids.iter().enumerate() {
            unsafe {
                *pkg.app_id_vec.memory.memory.add(old_app_size as usize + i) = app_id;
            }
        }

        // Also add to DepotIdVec (for content system to recognize downloadable depots)
        let old_depot_size = pkg.depot_id_vec.size;
        let num_depot_to_add = depot_ids.len() as i32;
        log::info!(
            "[InteropPackage] InitFakeLicense(PackageId={}): adding {} depots to DepotIdVec, oldSize={}",
            K_INJECTED_PACKAGE_ID, num_depot_to_add, old_depot_size
        );

        let depot_vec_ptr = &mut pkg.depot_id_vec as *mut _ as usize;
        let result = unsafe { cutl_grow(depot_vec_ptr, num_depot_to_add) };
        if result == 0 {
            log::warn!("[InteropPackage] InitFakeLicense: failed to grow DepotId vector");
            return false;
        }

        for (i, &depot_id) in depot_ids.iter().enumerate() {
            unsafe {
                *pkg.depot_id_vec.memory.memory.add(old_depot_size as usize + i) = depot_id;
            }
        }
    }

    LICENSE_INITIALIZED.store(true, Ordering::Release);
    LICENSE_REFRESH_PENDING.store(true, Ordering::Release);
    try_process_pending_license_refresh();
    true
}

fn try_process_pending_license_refresh() {
    if !LICENSE_REFRESH_PENDING.load(Ordering::Acquire) {
        return;
    }
    if mark_license_as_changed_and_process_updates() {
        LICENSE_REFRESH_PENDING.store(false, Ordering::Release);
    }
}

fn mark_license_as_changed_and_process_updates() -> bool {
    let c_user = match *c_user_ptr().lock().unwrap_or_else(|e| e.into_inner()) {
        Some(p) => p,
        None => {
            log::warn!("[InteropPackage] MarkLicenseAsChangedAndProcessUpdates: CUser not ready");
            return false;
        }
    };

    let mark_changed = match *original_mark_license_as_changed()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
    {
        Some(f) => f,
        None => {
            log::warn!("[InteropPackage] MarkLicenseAsChangedAndProcessUpdates: MarkLicenseAsChanged not ready");
            return false;
        }
    };

    let process_updates = match *original_process_pending_license_updates()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
    {
        Some(f) => f,
        None => {
            log::warn!("[InteropPackage] MarkLicenseAsChangedAndProcessUpdates: ProcessPendingLicenseUpdates not ready");
            return false;
        }
    };

    unsafe {
        mark_changed(c_user, K_INJECTED_PACKAGE_ID, true);
        process_updates(c_user);
    }
    log::debug!(
        "[InteropPackage] MarkLicenseAsChangedAndProcessUpdates: marked package {} as changed and processed updates",
        K_INJECTED_PACKAGE_ID
    );
    true
}

/// Notify license changed (called when .lua files are added/removed).
pub fn notify_license_changed() {
    let pkg_ptr = match *injected_pkg_info_ptr().lock().unwrap_or_else(|e| e.into_inner()) {
        Some(p) => p,
        None => {
            log::warn!("[InteropPackage] NotifyLicenseChanged: injected PackageInfo not ready");
            return;
        }
    };

    let pkg = unsafe { &mut *(pkg_ptr as *mut PackageInfo) };

    // Process removals
    let removals = take_pending_removals();
    log::debug!("[InteropPackage] NotifyLicenseChanged: processing {} removals", removals.len());
    let mut removed_count = 0;
    for id in &removals {
        if pkg.app_id_vec.memory.memory.is_null() || pkg.app_id_vec.size == 0 {
            continue;
        }
        for i in 0..pkg.app_id_vec.size {
            if unsafe { *pkg.app_id_vec.memory.memory.add(i as usize) } == *id {
                // FastRemove equivalent: swap with last and shrink
                unsafe {
                    *pkg.app_id_vec.memory.memory.add(i as usize) =
                        *pkg.app_id_vec.memory.memory.add((pkg.app_id_vec.size - 1) as usize);
                }
                pkg.app_id_vec.size -= 1;
                removed_count += 1;
                log::debug!("[InteropPackage] NotifyLicenseChanged: removed AppId {}", id);
                break;
            }
        }
        // Also remove from DepotIdVec
        if pkg.depot_id_vec.memory.memory.is_null() || pkg.depot_id_vec.size == 0 {
            continue;
        }
        for i in 0..pkg.depot_id_vec.size {
            if unsafe { *pkg.depot_id_vec.memory.memory.add(i as usize) } == *id {
                unsafe {
                    *pkg.depot_id_vec.memory.memory.add(i as usize) =
                        *pkg.depot_id_vec.memory.memory.add((pkg.depot_id_vec.size - 1) as usize);
                }
                pkg.depot_id_vec.size -= 1;
                log::debug!("[InteropPackage] NotifyLicenseChanged: removed DepotId {}", id);
                break;
            }
        }
    }

    // Process additions
    let additions = take_pending_additions();
    let mut added_ids = std::collections::HashSet::new();
    log::debug!("[InteropPackage] NotifyLicenseChanged: processing {} additions", additions.len());
    if !additions.is_empty() {
        // Add to AppIdVec
        let old_app_size = pkg.app_id_vec.size;
        let cutl_grow = match *original_cutl_memory_grow()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
        {
            Some(f) => f,
            None => {
                log::warn!("[InteropPackage] NotifyLicenseChanged: failed to grow AppId vector for additions");
                return;
            }
        };

        let vec_ptr = &mut pkg.app_id_vec as *mut _ as usize;
        let result = unsafe { cutl_grow(vec_ptr, additions.len() as i32) };
        if result != 0 {
            for (i, &app_id) in additions.iter().enumerate() {
                unsafe {
                    *pkg.app_id_vec.memory.memory.add(old_app_size as usize + i) = app_id;
                }
                added_ids.insert(app_id);
                log::debug!("[InteropPackage] NotifyLicenseChanged: inserted AppId {} at [{}]", app_id, old_app_size + i as u32);
            }
        } else {
            log::warn!("[InteropPackage] NotifyLicenseChanged: failed to grow AppId vector for additions");
        }

        // Also add to DepotIdVec
        let old_depot_size = pkg.depot_id_vec.size;
        let depot_vec_ptr = &mut pkg.depot_id_vec as *mut _ as usize;
        let result = unsafe { cutl_grow(depot_vec_ptr, additions.len() as i32) };
        if result != 0 {
            for (i, &depot_id) in additions.iter().enumerate() {
                unsafe {
                    *pkg.depot_id_vec.memory.memory.add(old_depot_size as usize + i) = depot_id;
                }
                log::debug!("[InteropPackage] NotifyLicenseChanged: inserted DepotId {} at [{}]", depot_id, old_depot_size + i as u32);
            }
        } else {
            log::warn!("[InteropPackage] NotifyLicenseChanged: failed to grow DepotId vector for additions");
        }
    }

    if added_ids.is_empty() && removed_count == 0 {
        log::debug!("[InteropPackage] NotifyLicenseChanged: no changes");
        return;
    }

    if !mark_license_as_changed_and_process_updates() {
        log::warn!("[InteropPackage] NotifyLicenseChanged: failed to mark license as changed");
        return;
    }

    log::info!(
        "[InteropPackage] NotifyLicenseChanged: {} added, {} removed",
        added_ids.len(),
        removed_count
    );

    // Queue UI removals (would integrate with InteropSteamUI)
    let mut queued_removal_count = 0;
    for id in &removals {
        if !added_ids.contains(id) {
            // InteropSteamUI::QueueRemoval(id);
            queued_removal_count += 1;
        }
    }
    log::debug!(
        "[InteropPackage] NotifyLicenseChanged: queued {} UI removals, skipped {} transient removals",
        queued_removal_count,
        removals.len() - queued_removal_count
    );
}

/// Get depot IDs from Lua config (addappid directives and SLSsteam config).
/// Returns AppIDs that should be injected into Package 0 to make them appear owned.
fn get_all_depot_ids() -> Vec<u32> {
    crate::steam_bridge::interop::bridge_lib::get_all_depot_ids()
}

/// Take pending removals from Lua config system.
/// Returns AppIDs that should be removed from Package 0.
fn take_pending_removals() -> Vec<u32> {
    crate::steam_bridge::interop::bridge_lib::take_pending_removals()
}

/// Take pending additions from Lua config system.
/// Returns AppIDs that should be added to Package 0.
fn take_pending_additions() -> Vec<u32> {
    crate::steam_bridge::interop::bridge_lib::take_pending_additions()
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

/// Install Package hooks.
pub fn install(steam_root: &std::path::Path) -> Result<(), String> {
    log::info!("Installing InteropPackage hooks...");

    // Resolve CUtlMemoryGrow
    if let Ok(addr) = resolve_function_address(
        steam_root,
        BinaryComponent::SteamClient,
        PatternChannel::Pattern,
        "CUtlMemoryGrow",
    ) {
        let replacement = cutl_memory_grow_detour as *const () as *mut u8;
        let patch = unsafe { memory_patch::patch_by_offset(addr, 0, replacement) }
            .map_err(|e| format!("Failed to install CUtlMemoryGrow hook: {}", e))?;

        if !patch.trampoline.is_null() {
            let orig_fn: CUtlMemoryGrowFn = unsafe { std::mem::transmute(patch.trampoline) };
            *original_cutl_memory_grow()
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(orig_fn);
        }
        installed_package_patches()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(patch);
        log::info!("Installed CUtlMemoryGrow hook at 0x{:x}", addr);
    }

    // Resolve MarkLicenseAsChanged
    if let Ok(addr) = resolve_function_address(
        steam_root,
        BinaryComponent::SteamClient,
        PatternChannel::Pattern,
        "MarkLicenseAsChanged",
    ) {
        let replacement = mark_license_as_changed_detour as *const () as *mut u8;
        let patch = unsafe { memory_patch::patch_by_offset(addr, 0, replacement) }
            .map_err(|e| format!("Failed to install MarkLicenseAsChanged hook: {}", e))?;

        if !patch.trampoline.is_null() {
            let orig_fn: MarkLicenseAsChangedFn = unsafe { std::mem::transmute(patch.trampoline) };
            *original_mark_license_as_changed()
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(orig_fn);
        }
        installed_package_patches()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(patch);
        log::info!("Installed MarkLicenseAsChanged hook at 0x{:x}", addr);
    }

    // Resolve ProcessPendingLicenseUpdates
    if let Ok(addr) = resolve_function_address(
        steam_root,
        BinaryComponent::SteamClient,
        PatternChannel::Pattern,
        "ProcessPendingLicenseUpdates",
    ) {
        let replacement = process_pending_license_updates_detour as *const () as *mut u8;
        let patch = unsafe { memory_patch::patch_by_offset(addr, 0, replacement) }
            .map_err(|e| format!("Failed to install ProcessPendingLicenseUpdates hook: {}", e))?;

        if !patch.trampoline.is_null() {
            let orig_fn: ProcessPendingLicenseUpdatesFn = unsafe { std::mem::transmute(patch.trampoline) };
            *original_process_pending_license_updates()
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(orig_fn);
        }
        installed_package_patches()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(patch);
        log::info!("Installed ProcessPendingLicenseUpdates hook at 0x{:x}", addr);
    }

    // Capture GetPackageInfo (no hook, just resolve for calling)
    if let Ok(addr) = resolve_function_address(
        steam_root,
        BinaryComponent::SteamClient,
        PatternChannel::Pattern,
        "GetPackageInfo",
    ) {
        let fn_ptr: GetPackageInfoFn = unsafe { std::mem::transmute(addr as usize) };
        *original_get_package_info()
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(fn_ptr);
        log::info!("Resolved GetPackageInfo at 0x{:x}", addr);
    }

    // Install CheckAppOwnership hook
    if let Ok(addr) = resolve_function_address(
        steam_root,
        BinaryComponent::SteamClient,
        PatternChannel::Pattern,
        "CheckAppOwnership",
    ) {
        let replacement = check_app_ownership_detour as *const () as *mut u8;
        let patch = unsafe { memory_patch::patch_by_offset(addr, 0, replacement) }
            .map_err(|e| format!("Failed to install CheckAppOwnership hook: {}", e))?;

        if !patch.trampoline.is_null() {
            let orig_fn: CheckAppOwnershipFn = unsafe { std::mem::transmute(patch.trampoline) };
            *original_check_app_ownership()
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(orig_fn);
        }
        installed_package_patches()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(patch);
        log::info!("Installed CheckAppOwnership hook at 0x{:x}", addr);
    }

    log::info!("InteropPackage hooks installed");
    Ok(())
}

/// Uninstall Package hooks.
pub fn uninstall() -> Result<(), String> {
    *original_cutl_memory_grow()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *original_mark_license_as_changed()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *original_process_pending_license_updates()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *original_get_package_info()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *original_check_app_ownership()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;

    let mut patches = installed_package_patches()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for mut patch in patches.drain(..) {
        unsafe { patch.uninstall() }?;
    }

    log::info!("InteropPackage hooks uninstalled");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_package_structs() {
        // Verify structure layouts match expectations
        assert_eq!(std::mem::size_of::<PackageInfo>(), 112);
        assert_eq!(std::mem::size_of::<AppOwnership>(), 52);
    }
}