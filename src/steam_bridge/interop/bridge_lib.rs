//! Bridge library integration - re-exports public API from bridge_lib crate

// The bridge_lib crate is a separate cdylib that exports C-compatible functions.
// We declare the extern functions here to call them.
extern "C" {
    fn get_depot_key_c(depot_id: u32) -> *const libc::c_char;
    fn is_additional_app_c(app_id: u32) -> bool;
    fn get_all_depot_ids_c(out: *mut u32, max_len: u32) -> u32;
    fn take_pending_additions_c(out: *mut u32, max_len: u32) -> u32;
    fn take_pending_removals_c(out: *mut u32, max_len: u32) -> u32;
    fn get_additional_apps_c(out: *mut u32, max_len: u32) -> u32;
}

use std::ffi::CStr;

/// Read a depot decryption key from the bridge_lib store
pub fn read_depot_key(depot_id: u32) -> Option<String> {
    let ptr = unsafe { get_depot_key_c(depot_id) };
    if ptr.is_null() {
        return None;
    }
    let cstr = unsafe { CStr::from_ptr(ptr) };
    cstr.to_str().ok().map(|s| s.to_string())
}

/// Check if an AppID is in additional_apps (Lua-unlocked, configured via addappid)
pub fn is_additional_app(app_id: u32) -> bool {
    unsafe { is_additional_app_c(app_id) }
}

/// Get all additional apps (Lua-unlocked via addappid)
pub fn get_additional_apps() -> Vec<u32> {
    // First call to get the count
    let count = unsafe { get_additional_apps_c(std::ptr::null_mut(), 0) };
    if count == 0 {
        return Vec::new();
    }

    // Allocate buffer and get the actual data
    let mut buffer = vec![0u32; count as usize];
    let actual_count = unsafe { get_additional_apps_c(buffer.as_mut_ptr(), count) };
    buffer.truncate(actual_count as usize);
    buffer
}

/// Get all depot IDs from Lua config (additional apps + depot keys)
pub fn get_all_depot_ids() -> Vec<u32> {
    // First call to get the count
    let count = unsafe { get_all_depot_ids_c(std::ptr::null_mut(), 0) };
    if count == 0 {
        return Vec::new();
    }

    // Allocate buffer and get the actual data
    let mut buffer = vec![0u32; count as usize];
    let actual_count = unsafe { get_all_depot_ids_c(buffer.as_mut_ptr(), count) };
    buffer.truncate(actual_count as usize);
    buffer
}

/// Take pending depot additions from Lua config hot-reload
pub fn take_pending_additions() -> Vec<u32> {
    // First call to get the count
    let count = unsafe { take_pending_additions_c(std::ptr::null_mut(), 0) };
    if count == 0 {
        return Vec::new();
    }

    // Allocate buffer and get the actual data
    let mut buffer = vec![0u32; count as usize];
    let actual_count = unsafe { take_pending_additions_c(buffer.as_mut_ptr(), count) };
    buffer.truncate(actual_count as usize);
    buffer
}

/// Take pending depot removals from Lua config hot-reload
pub fn take_pending_removals() -> Vec<u32> {
    // First call to get the count
    let count = unsafe { take_pending_removals_c(std::ptr::null_mut(), 0) };
    if count == 0 {
        return Vec::new();
    }

    // Allocate buffer and get the actual data
    let mut buffer = vec![0u32; count as usize];
    let actual_count = unsafe { take_pending_removals_c(buffer.as_mut_ptr(), count) };
    buffer.truncate(actual_count as usize);
    buffer
}