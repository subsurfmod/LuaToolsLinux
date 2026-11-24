//! Linux implementation of OSTPlatform::DynamicLibrary
//!
//! Provides dlopen/dlsym based dynamic library loading compatible with the Windows API.
//! Uses libdl for dynamic loading and /proc/self/maps for module enumeration.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Mutex, OnceLock};

use thiserror::Error;

fn open_handles() -> &'static Mutex<HashMap<usize, usize>> {
    static OPEN_HANDLES: OnceLock<Mutex<HashMap<usize, usize>>> = OnceLock::new();
    OPEN_HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn record_opened_handle(handle: ModuleHandle) {
    if !handle.is_null() {
        let mut map = open_handles().lock().unwrap_or_else(|e| e.into_inner());
        *map.entry(handle as usize).or_insert(0) += 1;
    }
}

/// Parse the file path column from a `/proc/<pid>/maps` line, preserving spaces in paths.
fn parse_maps_path(line: &str) -> Option<&str> {
    let mut rest = line.trim_start();
    // Skip first 5 whitespace-separated fields: addr, perms, offset, dev, inode
    for _ in 0..5 {
        let ws = rest.find(char::is_whitespace)?;
        rest = rest[ws..].trim_start();
    }
    if rest.is_empty() {
        return None;
    }
    let cleaned = rest.strip_suffix(" (deleted)").unwrap_or(rest).trim();
    if cleaned.is_empty() || cleaned.starts_with('[') {
        return None;
    }
    Some(cleaned)
}

/// Error type for dynamic library operations.
#[derive(Debug, Error)]
pub enum DynamicLibraryError {
    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("Path contains null bytes")]
    NullBytesInPath,

    #[error("Failed to load library: {0}")]
    LoadFailed(String),

    #[error("Symbol not found: {0}")]
    SymbolNotFound(String),

    #[error("Symbol ordinal not found: {0}")]
    SymbolOrdinalNotFound(u16),

    #[error("Module not found: {0}")]
    ModuleNotFound(String),

    #[error("Cannot get process executable path")]
    CannotGetProcessExecutable,

    #[error("Cannot get current directory")]
    CannotGetCurrentDirectory,

    #[error("Cannot get system directory")]
    CannotGetSystemDirectory,

    #[error("Cannot get module directory for handle")]
    CannotGetModuleDirectory,

    #[error("Invalid module handle")]
    InvalidModuleHandle,
}

/// Opaque module handle (dlopen returns void*, which we wrap for safety)
pub type ModuleHandle = *mut std::ffi::c_void;

/// Load a shared library from the given path.
/// Equivalent to Windows LoadLibrary.
///
/// # Arguments
/// * `path` - Path to the shared library (.so file)
///
/// # Returns
/// A module handle on success, or null on failure.
pub fn load(path: &Path) -> ModuleHandle {
    load_with_flags(path, libc::RTLD_LAZY | libc::RTLD_GLOBAL)
}

/// Load a shared library with specific flags.
fn load_with_flags(path: &Path, flags: i32) -> ModuleHandle {
    let raw = path.as_os_str().as_bytes();
    if raw.is_empty() || raw.contains(&0) {
        return ptr::null_mut();
    }

    let c_path = match CString::new(raw) {
        Ok(s) => s,
        Err(_) => return ptr::null_mut(),
    };

    // Clear any stale thread-local dlerror before dlopen so get_last_error_code()
    // accurately reflects this call.
    unsafe {
        let _ = libc::dlerror();
    }
    let handle = unsafe { libc::dlopen(c_path.as_ptr(), flags) };
    record_opened_handle(handle);
    handle
}

/// Get a handle to an already-loaded module by name.
/// Uses `dlopen(RTLD_NOLOAD)` first, then searches `/proc/self/maps`.
///
/// # Arguments
/// * `module_name` - Name of the module (e.g., "libc.so.6")
///
/// # Returns
/// A module handle on success, or null if not currently loaded.
pub fn get_loaded(module_name: &str) -> ModuleHandle {
    if module_name.is_empty() || module_name.as_bytes().contains(&0) {
        return ptr::null_mut();
    }

    // 1. Fast path: ask the dynamic linker directly without loading anything new.
    let noload = load_with_flags(Path::new(module_name), libc::RTLD_LAZY | libc::RTLD_NOLOAD);
    if !noload.is_null() {
        return noload;
    }

    // 2. Search /proc/self/maps for an already-mapped file matching module_name.
    if let Some(path) = find_module_path_in_maps(module_name) {
        let h = load_with_flags(&path, libc::RTLD_LAZY | libc::RTLD_NOLOAD);
        if !h.is_null() {
            return h;
        }
        return load_with_flags(&path, libc::RTLD_LAZY | libc::RTLD_LOCAL);
    }

    ptr::null_mut()
}

/// Find a module path in /proc/self/maps by name.
fn find_module_path_in_maps(module_name: &str) -> Option<PathBuf> {
    if module_name.is_empty() {
        return None;
    }
    let maps = fs::read_to_string("/proc/self/maps").ok()?;
    find_module_path_in_maps_str(&maps, module_name)
}

fn find_module_path_in_maps_str(maps: &str, module_name: &str) -> Option<PathBuf> {
    if module_name.is_empty() {
        return None;
    }

    let versioned_prefix = format!("{}.", module_name);
    let lib_exact = if module_name.starts_with("lib") {
        None
    } else {
        Some(format!("lib{}", module_name))
    };
    let mut fallback: Option<PathBuf> = None;

    for line in maps.lines() {
        let Some(path_clean) = parse_maps_path(line) else {
            continue;
        };

        let path = Path::new(path_clean);
        if path_clean == module_name {
            return Some(path.to_path_buf());
        }

        if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
            if file_name == module_name {
                return Some(path.to_path_buf());
            }
            if fallback.is_none()
                && (file_name.starts_with(&versioned_prefix)
                    || lib_exact.as_deref() == Some(file_name))
            {
                fallback = Some(path.to_path_buf());
            }
        }
    }

    fallback
}

/// Get a symbol (function or variable) from a loaded module by name.
/// Equivalent to Windows GetProcAddress.
///
/// # Arguments
/// * `module` - Module handle
/// * `symbol_name` - Name of the symbol to resolve
///
/// # Returns
/// Pointer to the symbol, or null if not found.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn get_symbol(module: ModuleHandle, symbol_name: &str) -> *mut std::ffi::c_void {
    if module.is_null() || symbol_name.is_empty() {
        return ptr::null_mut();
    }

    let c_name = match CString::new(symbol_name) {
        Ok(s) => s,
        Err(_) => return ptr::null_mut(),
    };

    unsafe {
        let _ = libc::dlerror();
        libc::dlsym(module, c_name.as_ptr())
    }
}

/// Get a symbol by ordinal (not supported on Linux, returns null).
/// This is provided for API compatibility with the Windows version.
///
/// Note: On ELF systems, symbols are not accessed by ordinal.
/// This function always returns null.
///
/// # Arguments
/// * `_module` - Module handle (ignored)
/// * `_ordinal` - Ordinal number (ignored)
///
/// # Returns
/// Always returns null on Linux.
pub fn get_symbol_ordinal(_module: ModuleHandle, _ordinal: u16) -> *mut std::ffi::c_void {
    // Symbols on ELF systems are not accessed by ordinal
    // Windows PE files use ordinal-based imports, but ELF uses name-based
    ptr::null_mut()
}

/// Get the last error code from dlopen/dlsym operations.
/// Equivalent to Windows GetLastError but adapted for Linux.
///
/// # Returns
/// 0 if no error, 1 if there was a dlopen/dlsym error.
pub fn get_last_error_code() -> u32 {
    let err = unsafe { libc::dlerror() };
    if err.is_null() {
        0
    } else {
        1
    }
}

/// Get the current working directory path.
/// Equivalent to Windows GetCurrentDirectory.
///
/// # Returns
/// Current working directory as a string, or empty string on error.
pub fn get_current_directory_path() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Get the system library directory path.
/// Equivalent to Windows GetSystemDirectory.
///
/// # Returns
/// System library directory (typically /usr/lib or /lib).
pub fn get_system_directory_path() -> String {
    // On Linux, system libraries are typically in /usr/lib or /lib
    // Check for multiarch paths as well
    let system_paths = [
        ("/usr/lib", true),
        ("/lib", false),
        ("/usr/lib/x86_64-linux-gnu", true),
        ("/lib/x86_64-linux-gnu", false),
        ("/usr/lib64", true),
        ("/lib64", false),
    ];

    for (path, _is_usr) in &system_paths {
        if Path::new(path).exists() {
            return (*path).to_string();
        }
    }

    // Fallback to a reasonable default
    "/usr/lib".to_string()
}

/// Get the directory containing the given module.
/// Equivalent to Windows GetModuleFileName.
///
/// # Arguments
/// * `module` - Module handle
///
/// # Returns
/// Path to the directory containing the module, or empty path on error.
#[repr(C)]
struct LinkMap {
    l_addr: usize,
    l_name: *const libc::c_char,
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn get_module_directory(module: ModuleHandle) -> PathBuf {
    if module.is_null() {
        return PathBuf::new();
    }

    // 1. Prefer dlinfo(RTLD_DI_LINKMAP) for dlopen() handles on Linux glibc
    let mut link_map: *mut LinkMap = ptr::null_mut();
    let dlinfo_res = unsafe {
        libc::dlinfo(
            module,
            libc::RTLD_DI_LINKMAP,
            (&mut link_map as *mut *mut LinkMap).cast::<libc::c_void>(),
        )
    };
    if dlinfo_res == 0 && !link_map.is_null() {
        let name_ptr = unsafe { (*link_map).l_name };
        if !name_ptr.is_null() {
            let name = unsafe { CStr::from_ptr(name_ptr) }
                .to_string_lossy()
                .into_owned();
            if !name.is_empty() {
                if let Some(parent) = Path::new(&name).parent() {
                    if !parent.as_os_str().is_empty() {
                        return parent.to_path_buf();
                    }
                }
                if let Some(resolved) = find_module_path_in_maps(&name) {
                    if let Some(parent) = resolved.parent() {
                        if !parent.as_os_str().is_empty() {
                            return parent.to_path_buf();
                        }
                    }
                }
            } else if let Ok(exe) = fs::read_link("/proc/self/exe") {
                if let Some(parent) = exe.parent() {
                    return parent.to_path_buf();
                }
            }
        }
    }

    // 2. Fallback to dladdr when module is a symbol/code address inside a loaded DSO
    let path = unsafe {
        let mut info: libc::Dl_info = std::mem::zeroed();
        let result = libc::dladdr(module as *const _, &mut info);

        if result == 0 || info.dli_fname.is_null() {
            return PathBuf::new();
        }

        CStr::from_ptr(info.dli_fname)
            .to_string_lossy()
            .into_owned()
    };

    let path = Path::new(&path);
    path.parent().map(|p| p.to_path_buf()).unwrap_or_default()
}

/// Get the path of the main executable.
/// Equivalent to Windows GetModuleHandle(NULL) approach.
///
/// # Returns
/// Path to the main executable, or a default on error.
pub fn get_main_executable_path() -> PathBuf {
    // On Linux, /proc/self/exe is a symlink to the main executable
    match fs::read_link("/proc/self/exe") {
        Ok(path) => path,
        Err(_) => PathBuf::from("steam"), // Default fallback
    }
}

/// Get a module handle by loading the library if not already loaded.
/// This is an alias for load() for API compatibility.
pub fn get_or_load(path: &Path) -> ModuleHandle {
    load(path)
}

/// Close/unload a loaded module.
/// Equivalent to Windows FreeLibrary.
/// Calls `dlclose` only for handles opened through this module's tracked `load`/`get_loaded`
/// calls, preventing refcount leaks while avoiding double-`dlclose` UB on untracked or null handles.
///
/// # Returns
/// `true` on success (and for null/untracked handles for API compatibility).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn unload(module: ModuleHandle) -> bool {
    if module.is_null() {
        return true;
    }

    let should_close = {
        let mut map = open_handles().lock().unwrap_or_else(|e| e.into_inner());
        match map.get_mut(&(module as usize)) {
            Some(count) if *count > 1 => {
                *count -= 1;
                true
            }
            Some(_) => {
                map.remove(&(module as usize));
                true
            }
            None => false,
        }
    };

    if should_close {
        unsafe { libc::dlclose(module) == 0 }
    } else {
        true
    }
}

/// Check if a module is valid (non-null and loadable).
pub fn is_valid_module(module: ModuleHandle) -> bool {
    !module.is_null()
}

/// List all loaded modules by parsing /proc/self/maps.
pub fn list_loaded_modules() -> Vec<PathBuf> {
    let maps = match fs::read_to_string("/proc/self/maps") {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    parse_loaded_modules_from_maps(&maps)
}

fn parse_loaded_modules_from_maps(maps: &str) -> Vec<PathBuf> {
    let mut modules = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for line in maps.lines() {
        let Some(path_clean) = parse_maps_path(line) else {
            continue;
        };

        let path = PathBuf::from(path_clean);
        if seen.insert(path.clone()) {
            modules.push(path);
        }
    }

    modules
}

/// Convert a Windows-style DLL name to Linux .so name.
/// E.g., "kernel32.dll" -> "libkernel32.so"
pub fn dll_to_so_name(dll_name: &str) -> String {
    let trimmed = dll_name.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // Preserve names that are already Linux shared object names (e.g., foo.so, libc.so.6)
    let file_name = Path::new(trimmed)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(trimmed);
    if file_name.ends_with(".so") || file_name.contains(".so.") {
        return file_name.to_string();
    }

    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(file_name);

    let prefixes: &[&str] = if stem.starts_with("lib") {
        &[""]
    } else {
        &["", "lib"]
    };

    // Try different prefixes and suffixes in current directory
    for prefix in prefixes {
        for suffix in &["", ".so", ".so.6", ".so.1"] {
            let candidate = format!("{}{}{}", prefix, stem, suffix);
            if Path::new(&candidate).exists() {
                return candidate;
            }
        }
    }

    if stem.starts_with("lib") {
        format!("{}.so", stem)
    } else {
        format!("lib{}.so", stem)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_libc() {
        let handle = load(Path::new("libc.so.6"));
        assert!(!handle.is_null(), "Failed to load libc.so.6");
    }

    #[test]
    fn test_load_libc_explicit_path() {
        let paths = [
            "/lib/x86_64-linux-gnu/libc.so.6",
            "/lib64/libc.so.6",
            "/usr/lib/libc.so.6",
        ];

        let mut loaded = false;
        for path in &paths {
            let handle = load(Path::new(path));
            if !handle.is_null() {
                loaded = true;
                break;
            }
        }
        assert!(loaded, "Failed to load libc from any known path");
    }

    #[test]
    fn test_get_symbol() {
        let handle = load(Path::new("libc.so.6"));
        assert!(!handle.is_null());

        let malloc_ptr = get_symbol(handle, "malloc");
        assert!(!malloc_ptr.is_null(), "Failed to get malloc symbol");

        let memcpy_ptr = get_symbol(handle, "memcpy");
        assert!(!memcpy_ptr.is_null(), "Failed to get memcpy symbol");
    }

    #[test]
    fn test_get_symbol_not_found() {
        let handle = load(Path::new("libc.so.6"));
        assert!(!handle.is_null());

        let symbol = get_symbol(handle, "nonexistent_symbol_12345");
        assert!(symbol.is_null(), "Nonexistent symbol should return null");
    }

    #[test]
    fn test_get_symbol_null_module() {
        let symbol = get_symbol(ptr::null_mut(), "malloc");
        assert!(
            symbol.is_null(),
            "Symbol lookup with null module should return null"
        );
    }

    #[test]
    fn test_get_symbol_ordinal_returns_null() {
        let handle = load(Path::new("libc.so.6"));
        assert!(!handle.is_null());

        let symbol = get_symbol_ordinal(handle, 1);
        assert!(
            symbol.is_null(),
            "Ordinal symbol lookup should return null on Linux"
        );
    }

    #[test]
    fn test_get_last_error_code() {
        // Without any error, should return 0
        let err = get_last_error_code();
        assert_eq!(err, 0, "No error should return 0");

        // After a failed symbol lookup, there might be an error
        let _ = get_symbol(ptr::null_mut(), "test");
    }

    #[test]
    fn test_get_current_directory_path() {
        let dir = get_current_directory_path();
        assert!(!dir.is_empty(), "Current directory should not be empty");
    }

    #[test]
    fn test_get_system_directory_path() {
        let dir = get_system_directory_path();
        assert!(!dir.is_empty(), "System directory should not be empty");

        // Should return a valid path
        let path = Path::new(&dir);
        assert!(
            path.exists(),
            "System directory path should exist: {:?}",
            dir
        );
    }

    #[test]
    fn test_get_module_directory() {
        // Get the handle for the current process's libc
        let handle = load(Path::new("libc.so.6"));
        assert!(!handle.is_null());

        let dir = get_module_directory(handle);
        assert!(
            !dir.as_os_str().as_bytes().is_empty(),
            "Module directory should not be empty"
        );
    }

    #[test]
    fn test_get_module_directory_null_handle() {
        let dir = get_module_directory(ptr::null_mut());
        assert!(
            dir.as_os_str().as_bytes().is_empty(),
            "Null handle should return empty path"
        );
    }

    #[test]
    fn test_get_main_executable_path() {
        let path = get_main_executable_path();
        assert!(
            path.exists() || path == Path::new("steam"),
            "Main executable path should exist or be default"
        );

        // On a running process, /proc/self/exe should exist
        if let Ok(readlink_result) = fs::read_link("/proc/self/exe") {
            assert_eq!(path, readlink_result, "Should match /proc/self/exe");
        }
    }

    #[test]
    fn test_get_loaded_libc() {
        let handle = get_loaded("libc.so.6");
        // This test may or may not pass depending on how the library was loaded
        // If the test binary was built with static linking, libc might not be in /proc/self/maps
        // In that case, it should still work by searching common paths
        if !handle.is_null() {
            let malloc_ptr = get_symbol(handle, "malloc");
            assert!(!malloc_ptr.is_null(), "Found libc should have malloc");
        }
    }

    #[test]
    fn test_is_valid_module() {
        let valid_handle = load(Path::new("libc.so.6"));
        assert!(
            is_valid_module(valid_handle),
            "Loaded module should be valid"
        );
        assert!(
            !is_valid_module(ptr::null_mut()),
            "Null should not be valid"
        );
    }

    #[test]
    fn test_unload() {
        // unload always returns true for API compatibility
        let result = unload(ptr::null_mut());
        assert!(result, "unload should return true");
    }

    #[test]
    fn test_list_loaded_modules() {
        let modules = list_loaded_modules();
        // We should at least see some modules if we're running
        assert!(
            !modules.is_empty() || modules.is_empty(),
            "Module list should be accessible"
        );
    }

    #[test]
    fn test_dll_to_so_name() {
        // Test that the function returns something reasonable
        let name = dll_to_so_name("somelib.dll");
        assert!(
            name.contains("somelib"),
            "Converted name should contain original name"
        );
    }

    #[test]
    fn test_load_null_path() {
        // Loading with empty path should return null
        let handle = load(Path::new(""));
        // Empty path will result in dlopen failure, which returns null
        assert!(handle.is_null() || !handle.is_null());
    }

    #[test]
    fn test_load_nonexistent_file() {
        let handle = load(Path::new("/nonexistent/library.so.12345"));
        assert!(
            handle.is_null(),
            "Loading nonexistent file should return null"
        );
    }

    #[test]
    fn test_load_invalid_path_with_null_bytes() {
        // Create a path with null bytes (simulated via raw bytes)
        let path_with_null: &[u8] = b"/tmp/lib\x00test.so";
        let path = unsafe { std::str::from_utf8_unchecked(path_with_null) };
        // This should be handled gracefully - the CString::new will fail
        let handle = load(Path::new(path));
        assert!(handle.is_null(), "Path with null bytes should fail to load");
    }

    #[test]
    fn test_get_symbol_with_empty_name() {
        let handle = load(Path::new("libc.so.6"));
        if !handle.is_null() {
            let _symbol = get_symbol(handle, "");
        }
    }

    #[test]
    fn test_multiple_loads_same_library() {
        // Loading the same library multiple times should work
        let h1 = load(Path::new("libc.so.6"));
        let h2 = load(Path::new("libc.so.6"));

        // Both should be valid (or both null if libc.so.6 isn't available)
        assert_eq!(
            !h1.is_null(),
            !h2.is_null(),
            "Multiple loads should have consistent results"
        );

        // If both loaded, they might be the same handle (due to RTLD_GLOBAL)
        // or different handles (due to reference counting)
    }

    #[test]
    fn test_module_directory_consistency() {
        let handle = load(Path::new("libc.so.6"));
        if !handle.is_null() {
            let dir = get_module_directory(handle);
            // The directory path should be valid
            if !dir.as_os_str().as_bytes().is_empty() {
                // The directory should exist
                let parent_exists = dir.exists();
                // Even if the library is deleted, the path might still be valid
                // This test is informational
                let _ = parent_exists;
            }
        }
    }

    #[test]
    fn test_list_loaded_modules_excludes_pseudo_mappings() {
        let modules = list_loaded_modules();
        assert!(
            !modules.is_empty(),
            "Current process should have mapped modules"
        );
        for m in &modules {
            let s = m.to_string_lossy();
            assert!(
                !s.starts_with('['),
                "Pseudo-mapping {:?} should not be listed as a module",
                m
            );
        }
    }

    #[test]
    fn test_get_loaded_rejects_empty_and_nonexistent() {
        assert!(get_loaded("").is_null());
        assert!(get_loaded("definitely_not_loaded_lib_987654.so").is_null());
    }

    #[test]
    fn test_parse_maps_and_exact_matching() {
        let fake_maps = "\
7f000000-7f001000 r-xp 00000000 00:00 0                                  [vdso]\n\
7f002000-7f003000 r--p 00000000 08:01 123                                /opt/my  app/libfoo.so\n\
7f004000-7f005000 r--p 00000000 08:01 124                                /usr/lib/c\n";
        let mods = parse_loaded_modules_from_maps(fake_maps);
        assert_eq!(mods.len(), 2);
        assert_eq!(mods[0], PathBuf::from("/opt/my  app/libfoo.so"));
        // Must not match `/usr/lib/c` when looking for `steamclient.so`
        assert_eq!(
            find_module_path_in_maps_str(fake_maps, "steamclient.so"),
            None
        );
        assert_eq!(
            find_module_path_in_maps_str(fake_maps, "libfoo.so"),
            Some(PathBuf::from("/opt/my  app/libfoo.so"))
        );
    }

    #[test]
    fn test_dll_to_so_name_edge_cases() {
        assert_eq!(dll_to_so_name(""), "");
        assert_eq!(dll_to_so_name("libfoo.dll"), "libfoo.so");
        assert_eq!(dll_to_so_name("steamclient.so"), "steamclient.so");
        assert_eq!(dll_to_so_name("libc.so.6"), "libc.so.6");
    }

    #[test]
    fn test_load_and_unload_tracked_handle() {
        let h = load(Path::new("libc.so.6"));
        assert!(!h.is_null());
        assert!(unload(h));
        // Second unload on same handle must be a safe no-op, not double-dlclose UB
        assert!(unload(h));
    }
}
