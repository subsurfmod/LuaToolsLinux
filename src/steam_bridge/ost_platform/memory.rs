//! Linux implementation of OSTPlatform::Memory
//!
//! Provides memory operations: module info, executable memory writes, readability checks.
//! Uses `dlinfo(RTLD_DI_LINKMAP)` / `dladdr`, `/proc/self/maps`, and `mprotect`.

use std::ffi::CStr;
use std::fs;
use std::ptr;

/// Module image information (base address and size).
#[derive(Debug, Clone, Copy)]
pub struct ModuleImage {
    pub base: *mut u8,
    pub size: usize,
}

#[repr(C)]
struct LinkMap {
    l_addr: usize,
    l_name: *const libc::c_char,
}

fn resolve_module_path(module: *mut std::ffi::c_void) -> Option<String> {
    if module.is_null() {
        return None;
    }

    // 1. Prefer dlinfo(RTLD_DI_LINKMAP) for dlopen() handles (on glibc, dlopen returns a
    //    heap-allocated link_map pointer which dladdr() cannot resolve).
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
                return Some(name);
            }
            if let Ok(exe) = fs::read_link("/proc/self/exe") {
                return Some(exe.to_string_lossy().into_owned());
            }
        }
    }

    // 2. Fallback to dladdr() when caller passed an address inside a loaded shared object.
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::dladdr(module as *const _, &mut info) };
    if result != 0 && !info.dli_fname.is_null() {
        let name = unsafe { CStr::from_ptr(info.dli_fname) }
            .to_string_lossy()
            .into_owned();
        if !name.is_empty() {
            return Some(name);
        }
    }

    None
}

/// Get module image (base address and size) from a module handle or symbol address.
pub fn get_module_image(module: *mut std::ffi::c_void) -> Option<ModuleImage> {
    let raw_path = resolve_module_path(module)?;
    let canon_path = fs::canonicalize(&raw_path)
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    let target_name = std::path::Path::new(&raw_path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string);

    let maps = fs::read_to_string("/proc/self/maps").ok()?;
    let mut base_addr: Option<u64> = None;
    let mut max_end: u64 = 0;

    for line in maps.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 6 {
            continue;
        }

        let range = parts[0];
        let path_part = parts[5..].join(" ");
        let path_clean = path_part.trim_end_matches(" (deleted)").trim();

        let matches = path_clean == raw_path
            || canon_path.as_deref() == Some(path_clean)
            || target_name.as_deref().is_some_and(|tn| {
                std::path::Path::new(path_clean)
                    .file_name()
                    .and_then(|n| n.to_str())
                    == Some(tn)
            });
        if !matches {
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
/// Uses `mprotect` to temporarily make the page writable and restores `PROT_READ | PROT_EXEC`.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn write_executable_byte(target: *mut std::ffi::c_void, value: u8) -> bool {
    if target.is_null() {
        return false;
    }

    let addr = target as usize;
    let ps = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if ps <= 0 {
        return false;
    }
    let page_size = ps as usize;
    let page_start = addr & !(page_size - 1);
    let page_len = page_size;

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

    unsafe {
        ptr::write_volatile(target as *mut u8, value);
    }

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
/// Walks `/proc/self/maps` to verify every page in the range is readable.
pub fn is_readable(addr: *const std::ffi::c_void, bytes: usize) -> bool {
    if addr.is_null() || bytes == 0 {
        return false;
    }

    let start = addr as usize;
    let Some(end) = start.checked_add(bytes) else {
        return false;
    };

    let Ok(maps) = fs::read_to_string("/proc/self/maps") else {
        return false;
    };

    let mut current = start;
    while current < end {
        let mut found = false;
        for line in maps.lines() {
            let mut parts = line.split_whitespace();
            let Some(range) = parts.next() else {
                continue;
            };
            let Some(perms) = parts.next() else {
                continue;
            };
            if !perms.contains('r') {
                continue;
            }
            if let Some((start_str, end_str)) = range.split_once('-') {
                let Ok(range_start) = usize::from_str_radix(start_str, 16) else {
                    continue;
                };
                let Ok(range_end) = usize::from_str_radix(end_str, 16) else {
                    continue;
                };

                if current >= range_start && current < range_end {
                    current = range_end.min(end);
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

    #[test]
    fn test_get_module_image_libc() {
        let handle = super::super::dynamic_library::load(std::path::Path::new("libc.so.6"));
        assert!(!handle.is_null());

        let image = get_module_image(handle);
        assert!(image.is_some(), "Should get module image for libc");
        if let Some(img) = image {
            assert!(!img.base.is_null());
            assert!(img.size > 0);
        }
    }

    #[test]
    fn test_write_executable_byte() {
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

        let result = write_executable_byte(ptr, 0x42);
        assert!(result, "Should write byte to executable memory");

        let val = unsafe { ptr::read_volatile(ptr as *const u8) };
        assert_eq!(val, 0x42);

        unsafe { libc::munmap(ptr, page_size) };
    }

    #[test]
    fn test_is_readable() {
        let local_var = 42u32;
        assert!(is_readable((&local_var as *const u32).cast(), 4));
        assert!(!is_readable(ptr::null(), 4));
        assert!(!is_readable((&local_var as *const u32).cast(), 0));
    }
}
