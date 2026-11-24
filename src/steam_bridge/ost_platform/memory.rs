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

fn parse_maps_line(line: &str) -> Option<(usize, usize, &str, Option<&str>)> {
    let mut rest = line.trim_start();
    let ws1 = rest.find(char::is_whitespace)?;
    let range = &rest[..ws1];
    rest = rest[ws1..].trim_start();

    let ws2 = rest.find(char::is_whitespace)?;
    let perms = &rest[..ws2];
    rest = rest[ws2..].trim_start();

    let (start_str, end_str) = range.split_once('-')?;
    let start = usize::from_str_radix(start_str, 16).ok()?;
    let end = usize::from_str_radix(end_str, 16).ok()?;

    // Skip offset, dev, inode (3 fields) to get optional path
    for _ in 0..3 {
        let Some(ws) = rest.find(char::is_whitespace) else {
            return Some((start, end, perms, None));
        };
        rest = rest[ws..].trim_start();
    }

    let path = if rest.is_empty() {
        None
    } else {
        Some(rest.strip_suffix(" (deleted)").unwrap_or(rest).trim())
    };
    Some((start, end, perms, path))
}

fn perms_to_prot(perms: &str) -> libc::c_int {
    let mut prot = 0;
    if perms.contains('r') {
        prot |= libc::PROT_READ;
    }
    if perms.contains('w') {
        prot |= libc::PROT_WRITE;
    }
    if perms.contains('x') {
        prot |= libc::PROT_EXEC;
    }
    prot
}

fn query_page_protection(addr: usize) -> Option<libc::c_int> {
    let maps = fs::read_to_string("/proc/self/maps").ok()?;
    for line in maps.lines() {
        let Some((start, end, perms, _)) = parse_maps_line(line) else {
            continue;
        };
        if end <= addr {
            continue;
        }
        if start > addr {
            break;
        }
        if addr >= start && addr < end {
            return Some(perms_to_prot(perms));
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
    let mut exact_range: Option<(usize, usize)> = None;
    let mut basename_range: Option<(usize, usize)> = None;

    for line in maps.lines() {
        let Some((start, end, _perms, Some(path_clean))) = parse_maps_line(line) else {
            continue;
        };
        if end <= start {
            continue;
        }

        let exact_match = path_clean == raw_path || canon_path.as_deref() == Some(path_clean);
        if exact_match {
            exact_range = Some(match exact_range {
                Some((cur_start, cur_end)) => (cur_start.min(start), cur_end.max(end)),
                None => (start, end),
            });
            continue;
        }

        if exact_range.is_none()
            && target_name.as_deref().is_some_and(|tn| {
                std::path::Path::new(path_clean)
                    .file_name()
                    .and_then(|n| n.to_str())
                    == Some(tn)
            })
        {
            basename_range = Some(match basename_range {
                Some((cur_start, cur_end)) => (cur_start.min(start), cur_end.max(end)),
                None => (start, end),
            });
        }
    }

    exact_range
        .or(basename_range)
        .map(|(base, max_end)| ModuleImage {
            base: base as *mut u8,
            size: max_end.saturating_sub(base),
        })
}

/// Write a single byte to executable memory.
/// Uses `mprotect` to temporarily make the page writable and restores its original protection.
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
    if !page_size.is_power_of_two() {
        return false;
    }
    let page_start = addr & !(page_size - 1);
    let page_len = page_size;
    let original_prot = query_page_protection(addr).unwrap_or(libc::PROT_READ | libc::PROT_EXEC);

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

    let _ = unsafe { libc::mprotect(page_start as *mut libc::c_void, page_len, original_prot) };

    true
}

/// Check if a memory range is readable in the current process.
/// Performs a single linear pass over `/proc/self/maps` to verify every byte in `[addr, addr + bytes)`.
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

    is_readable_in_maps(&maps, start, end)
}

fn is_readable_in_maps(maps: &str, start: usize, end: usize) -> bool {
    if start >= end {
        return false;
    }

    let mut current = start;
    for line in maps.lines() {
        let Some((range_start, range_end, perms, _)) = parse_maps_line(line) else {
            continue;
        };
        if range_end <= range_start || range_end <= current {
            continue;
        }
        if range_start > current {
            return false;
        }
        if !perms.contains('r') {
            return false;
        }
        current = range_end.min(end);
        if current >= end {
            return true;
        }
    }

    false
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

    #[test]
    fn test_is_readable_in_maps_contiguous_and_gaps() {
        let maps = "\
1000-2000 r--p 00000000 00:00 0\n\
2000-3000 rw-p 00000000 00:00 0\n\
3000-4000 ---p 00000000 00:00 0\n\
5000-6000 r-xp 00000000 00:00 0 /opt/my  dir/libtest.so\n";
        assert!(is_readable_in_maps(maps, 0x1000, 0x3000));
        assert!(!is_readable_in_maps(maps, 0x2f00, 0x3100)); // crosses ---p guard page
        assert!(!is_readable_in_maps(maps, 0x4000, 0x5100)); // starts in unmapped gap
        assert!(is_readable_in_maps(maps, 0x5000, 0x6000));
    }

    #[test]
    fn test_write_executable_byte_preserves_rwx_protection() {
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

        assert!(write_executable_byte(ptr, 0xAA));
        // Original RWX protection should be preserved so direct write still works
        let prot = query_page_protection(ptr as usize).expect("mapped page prot");
        assert_eq!(prot, libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC);
        unsafe {
            ptr::write_volatile(ptr as *mut u8, 0xBB);
            assert_eq!(ptr::read_volatile(ptr as *const u8), 0xBB);
            libc::munmap(ptr, page_size);
        }
    }
}
