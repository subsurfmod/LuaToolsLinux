//! Bridge library integration - re-exports public API from bridge_lib crate

use std::ffi::CStr;

unsafe fn lookup_symbol<T: Copy>(name: &[u8]) -> Option<T> {
    let sym = libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr() as *const _);
    if sym.is_null() {
        None
    } else {
        Some(std::mem::transmute_copy(&sym))
    }
}

/// Read a depot decryption key from the bridge_lib store
pub fn read_depot_key(depot_id: u32) -> Option<String> {
    type FnGetDepotKey = unsafe extern "C" fn(u32) -> *const libc::c_char;
    let func: FnGetDepotKey = unsafe { lookup_symbol(b"get_depot_key_c\0") }?;
    let ptr = unsafe { func(depot_id) };
    if ptr.is_null() {
        return None;
    }
    let cstr = unsafe { CStr::from_ptr(ptr) };
    cstr.to_str().ok().map(|s| s.to_string())
}

/// Check if an AppID is in additional_apps (Lua-unlocked, configured via addappid)
pub fn is_additional_app(app_id: u32) -> bool {
    type FnIsAdditionalApp = unsafe extern "C" fn(u32) -> bool;
    if let Some(func) = unsafe { lookup_symbol::<FnIsAdditionalApp>(b"is_additional_app_c\0") } {
        unsafe { func(app_id) }
    } else {
        false
    }
}

/// Get all additional apps (Lua-unlocked via addappid)
pub fn get_additional_apps() -> Vec<u32> {
    type FnApps = unsafe extern "C" fn(*mut u32, u32) -> u32;
    let Some(func) = (unsafe { lookup_symbol::<FnApps>(b"get_additional_apps_c\0") }) else {
        return Vec::new();
    };

    // First call to get the count
    let count = unsafe { func(std::ptr::null_mut(), 0) };
    if count == 0 {
        return Vec::new();
    }

    // Allocate buffer and get the actual data
    let mut buffer = vec![0u32; count as usize];
    let actual_count = unsafe { func(buffer.as_mut_ptr(), count) };
    buffer.truncate(actual_count as usize);
    buffer
}

/// Get all depot IDs from Lua config (additional apps + depot keys)
pub fn get_all_depot_ids() -> Vec<u32> {
    type FnDepots = unsafe extern "C" fn(*mut u32, u32) -> u32;
    let Some(func) = (unsafe { lookup_symbol::<FnDepots>(b"get_all_depot_ids_c\0") }) else {
        return Vec::new();
    };

    // First call to get the count
    let count = unsafe { func(std::ptr::null_mut(), 0) };
    if count == 0 {
        return Vec::new();
    }

    // Allocate buffer and get the actual data
    let mut buffer = vec![0u32; count as usize];
    let actual_count = unsafe { func(buffer.as_mut_ptr(), count) };
    buffer.truncate(actual_count as usize);
    buffer
}

/// Take pending depot additions from Lua config hot-reload
pub fn take_pending_additions() -> Vec<u32> {
    type FnPending = unsafe extern "C" fn(*mut u32, u32) -> u32;
    let Some(func) = (unsafe { lookup_symbol::<FnPending>(b"take_pending_additions_c\0") }) else {
        return Vec::new();
    };

    // First call to get the count
    let count = unsafe { func(std::ptr::null_mut(), 0) };
    if count == 0 {
        return Vec::new();
    }

    // Allocate buffer and get the actual data
    let mut buffer = vec![0u32; count as usize];
    let actual_count = unsafe { func(buffer.as_mut_ptr(), count) };
    buffer.truncate(actual_count as usize);
    buffer
}

/// Take pending depot removals from Lua config hot-reload
pub fn take_pending_removals() -> Vec<u32> {
    type FnPending = unsafe extern "C" fn(*mut u32, u32) -> u32;
    let Some(func) = (unsafe { lookup_symbol::<FnPending>(b"take_pending_removals_c\0") }) else {
        return Vec::new();
    };

    // First call to get the count
    let count = unsafe { func(std::ptr::null_mut(), 0) };
    if count == 0 {
        return Vec::new();
    }

    // Allocate buffer and get the actual data
    let mut buffer = vec![0u32; count as usize];
    let actual_count = unsafe { func(buffer.as_mut_ptr(), count) };
    buffer.truncate(actual_count as usize);
    buffer
}
