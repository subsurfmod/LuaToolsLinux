//! Linux implementation of OSTPlatform::Memory
//!
//! Provides memory operations: module info, executable memory writes, readability checks.
//! Uses /proc/self/maps and mprotect for memory manipulation.

use std::fs;
use std::ptr;

/// Module image information (base address and size).
#[derive(Debug, Clone, Copy)]
pub struct ModuleImage {
    pub base: *mut u8,
    pub size: usize,
}

/// Get module image (base address and size) from a module handle.
/// On Linux, the module handle is the dlopen return value.
pub fn get_module_image(module: *mut std::ffi::c_void) -> Option<ModuleImage> {
    if module.is_null() {
        return None;
    }

    // Use dladdr to get the path, then find it in /proc/self/maps
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::dladdr(module as *const _, &mut info) };
    if result == 0 {
        return None;
    }

    let path = unsafe { std::ffi::CStr::from_ptr(info.dli_fname) }
        .to_string_lossy()
        .to_string();

    // Find this module in /proc/self/maps
    let maps = fs::read_to_string("/proc/self/maps").ok()?;
    let mut base_addr: Option<u64> = None;
    let mut max_end: u64 = 0;

    for line in maps.lines() {
        if !line.contains(&path) {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 6 {
            continue;
        }

        let range = parts[0];
        let path_part = parts[5..].join(" ");
        let path_clean = path_part.trim_end_matches(" (deleted)").trim();

        if path_clean != path {
            continue;
        }

        if let Some((start_str, end_str)) = range.split_once('-') {
            let start = u64::from_str_radix(start_str, 16).ok()?;
            let end = u64::from_str_radix(end_str, 16).ok()?;

            base_addr = Some(match base_addr {
                Some(cur) => cur.min(start),
                None => start,
            });
            max_end = max_end.max(end);
        }
    }

    base_addr.map(|base| ModuleImage {
        base: base as *mut u8,
        size: (max_end.saturating_sub(base)) as usize,
    })
}

/// Write a single byte to executable memory.
/// Uses mprotect to temporarily make the page writable.
pub fn write_executable_byte(target: *mut std::ffi::c_void, value: u8) -> bool {
    if target.is_null() {
        return false;
    }

    let addr = target as usize;
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    if page_size == 0 {
        return false;
    }

    let page_start = addr & !(page_size - 1);
    let page_len = page_size;

    // Make page writable
    let result = unsafe {
        libc::mprotect(
            page_start as *mut libc::c_void,
            page_len,
            libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
        )
    };

    if result != 0 {
        return false;
    }

    // Write the byte
    unsafe {
        ptr::write_volatile(target as *mut u8, value);
    }

    // Restore read+execute (W^X)
    let _ = unsafe {
        libc::mprotect(
            page_start as *mut libc::c_void,
            page_len,
            libc::PROT_READ | libc::PROT_EXEC,
        )
    };

    true
}

/// Check if a memory range is readable in the current process.
/// Walks /proc/self/maps to verify every page in the range is readable.
pub fn is_readable(addr: *const std::ffi::c_void, bytes: usize) -> bool {
    if addr.is_null() || bytes == 0 {
        return false;
    }

    let start = addr as usize;
    let end = match start.checked_add(bytes) {
        Some(e) => e,
        None => return false,
    };

    let maps = match fs::read_to_string("/proc/self/maps") {
        Ok(m) => m,
        Err(_) => return false,
    };

    let mut current = start;
    while current < end {
        let mut found = false;
        for line in maps.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 2 {
                continue;
            }

            let range = parts[0];
            let perms = parts[1];

            if !perms.contains('r') {
                continue;
            }

            if let Some((start_str, end_str)) = range.split_once('-') {
                let range_start = u64::from_str_radix(start_str, 16).unwrap_or(0) as usize;
                let range_end = u64::from_str_radix(end_str, 16).unwrap_or(0) as usize;

                if current >= range_start && current < range_end {
                    // This page is readable, advance to end of this range or our end
                    current = current.min(range_end).min(end);
                    found = true;
                    break;
                }
            }
        }

        if !found {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn test_get_module_image_libc() {
        let handle = crate::dynamic_library::load(std::path::Path::new("libc.so.6"));
        assert!(!handle.is_null());

        let image = get_module_image(handle);
        assert!(image.is_some(), "Should get module image for libc");
        if let Some(img) = image {
            println!("libc: base={:p}, size={}", img.base, img.size);
            assert!(!img.base.is_null());
            assert!(img.size > 0);
        }
    }

    #[test]
    fn test_write_executable_byte() {
        // Allocate an executable page
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let ptr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                page_size,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(ptr, libc::MAP_FAILED);

        // Write a byte
        let result = write_executable_byte(ptr as *mut _, 0x42);
        assert!(result, "Should write byte to executable memory");

        // Verify
        let val = unsafe { ptr::read_volatile(ptr as *const u8) };
        assert_eq!(val, 0x42);

        // Cleanup
        unsafe { libc::munmap(ptr, page_size) };
    }

    #[test]
    fn test_is_readable() {
        // Test with a known readable address (stack)
        let mut local_var = 42u32;
        assert!(is_readable(&local_var as *const _ as *const _, 4));

        // Test with null
        assert!(!is_readable(ptr::null(), 4));

        // Test with zero bytes
        assert!(!is_readable(&local_var as *const _ as *const _, 0));
    }
}