//! Bridge library integration - re-exports public API from bridge_lib crate

// The bridge_lib crate is a separate cdylib that exports C-compatible functions.
// We declare the extern functions here to call them.
extern "C" {
    fn get_depot_key_c(depot_id: u32) -> *const libc::c_char;
    fn is_additional_app_c(app_id: u32) -> bool;
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