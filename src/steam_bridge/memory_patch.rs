//! Memory patching via 64-bit absolute indirect trampolines (`FF 25 00 00 00 00 <u64>`).
//!
//! On Linux x86-64, ASLR places `mmap` regions and shared objects > 2 GiB apart, so 32-bit
//! `E9` relative jumps can overflow. We use a 14-byte RIP-relative indirect jump
//! (`jmp qword ptr [rip + 0]` followed by an 8-byte 64-bit target address) which reaches
//! any 64-bit virtual address cleanly.
//!
//! Before modifying any code bytes, `MemoryPatch::install` validates via `/proc/self/maps`
//! that the target range is actually mapped as readable + executable (`r-x` / `rwx`),
//! logs `errno` on any `mmap`/`mprotect` error, restores `PROT_READ | PROT_EXEC` after
//! patching (preserving W^X), and releases trampoline pages cleanly on failure or drop.

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::_mm_mfence;
#[cfg(target_arch = "x86")]
use std::arch::x86::_mm_mfence;
use std::io;
use std::ptr;

use crate::steam_bridge::ost_platform::memory::{get_module_image, is_readable};

#[cfg(target_arch = "x86_64")]
pub const JMP_ABS64_LEN: usize = 14;
#[cfg(target_arch = "x86")]
pub const JMP_ABS64_LEN: usize = 6;
#[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
pub const JMP_ABS64_LEN: usize = 14;
const TRAMPOLINE_SIZE: usize = 64;
const MIN_VALID_USER_ADDR: usize = 0x10000;

fn system_page_size() -> usize {
    let ps = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if ps > 0 && (ps as usize).is_power_of_two() {
        ps as usize
    } else {
        4096
    }
}

/// Pure parser checking whether `[addr, addr + len)` lies completely within a readable+executable
/// virtual memory mapping in `/proc/self/maps` content.
pub fn parse_maps_contains_exec_range(maps: &str, addr: usize, len: usize) -> bool {
    if addr < MIN_VALID_USER_ADDR || len == 0 {
        return false;
    }
    let Some(target_end) = addr.checked_add(len) else {
        return false;
    };

    for line in maps.lines() {
        let mut parts = line.split_whitespace();
        let Some(range_str) = parts.next() else {
            continue;
        };
        let Some(perms) = parts.next() else {
            continue;
        };
        if !perms.contains('r') || !perms.contains('x') {
            continue;
        }
        let Some((start_hex, end_hex)) = range_str.split_once('-') else {
            continue;
        };
        let Ok(start) = usize::from_str_radix(start_hex, 16) else {
            continue;
        };
        let Ok(end) = usize::from_str_radix(end_hex, 16) else {
            continue;
        };
        if addr >= start && target_end <= end {
            return true;
        }
    }
    false
}

/// Checks `/proc/self/maps` to verify `[addr, addr + len)` is mapped readable + executable.
pub fn is_executable_mapped_range(addr: usize, len: usize) -> bool {
    let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
        return false;
    };
    parse_maps_contains_exec_range(&maps, addr, len)
}

/// Validates that `[addr, addr + len)` is readable via `ost_platform::memory::is_readable`
/// and mapped readable + executable in `/proc/self/maps`.
fn validate_patch_target_range(addr: usize, len: usize) -> Result<(), String> {
    let ptr = addr as *const std::ffi::c_void;
    if !is_readable(ptr, len) || !is_executable_mapped_range(addr, len) {
        return Err(format!(
            "refusing to patch 0x{:x} ({} bytes): not inside a mapped readable+executable page",
            addr, len
        ));
    }
    Ok(())
}

/// Validates that `[addr, addr + len)` is readable+executable and lies within the loaded
/// module image identified by a `dlopen` `module_handle`.
pub fn is_range_in_module_image(
    module_handle: *mut std::ffi::c_void,
    addr: usize,
    len: usize,
) -> bool {
    if module_handle.is_null() || validate_patch_target_range(addr, len).is_err() {
        return false;
    }
    let Some(img) = get_module_image(module_handle) else {
        return false;
    };
    let img_start = img.base as usize;
    let Some(img_end) = img_start.checked_add(img.size) else {
        return false;
    };
    let Some(target_end) = addr.checked_add(len) else {
        return false;
    };
    addr >= img_start && target_end <= img_end
}

fn page_bounds(addr: usize, len: usize, page_size: usize) -> Result<(usize, usize), String> {
    let end = addr
        .checked_add(len)
        .ok_or_else(|| "target address range overflow".to_string())?;
    let page_start = addr & !(page_size - 1);
    let page_end = end
        .checked_add(page_size - 1)
        .ok_or_else(|| "page alignment overflow".to_string())?
        & !(page_size - 1);
    let page_len = page_end
        .checked_sub(page_start)
        .filter(|&l| l > 0)
        .ok_or_else(|| "computed zero mprotect page length".to_string())?;
    Ok((page_start, page_len))
}

/// Represents an installed inline memory patch.
/// Intentionally NOT `Clone` because it owns an `mmap`ed trampoline and restores bytes on `Drop`.
#[derive(Debug)]
pub struct MemoryPatch {
    pub target: *mut u8,
    pub trampoline: *mut u8,
    pub trampoline_alloc_size: usize,
    pub replacement: *mut u8,
    pub original_bytes: Vec<u8>,
    pub orig_len: usize,
    pub active: bool,
}

impl MemoryPatch {
    pub fn new(target: *mut u8, replacement: *mut u8) -> Self {
        Self {
            target,
            replacement,
            trampoline: ptr::null_mut(),
            trampoline_alloc_size: 0,
            original_bytes: Vec::new(),
            orig_len: 0,
            active: false,
        }
    }

    /// Writes an absolute jump at `dst`:
    /// - x86_64: 14-byte `FF 25 00 00 00 00 <u64>` (`jmp qword ptr [rip + 0]`)
    /// - x86 (IA-32): 6-byte `68 <u32> C3` (`push imm32; ret`), safe at any distance without over-read UB
    ///
    /// # Safety
    /// `dst` must point to at least `JMP_ABS64_LEN` bytes of writable memory.
    unsafe fn write_abs_jmp64(dst: *mut u8, target_addr: usize) {
        #[cfg(target_arch = "x86_64")]
        {
            let stub: [u8; 6] = [0xFF, 0x25, 0x00, 0x00, 0x00, 0x00];
            ptr::copy_nonoverlapping(stub.as_ptr(), dst, stub.len());
            let addr64 = target_addr as u64;
            ptr::copy_nonoverlapping(addr64.to_le_bytes().as_ptr(), dst.add(stub.len()), 8);
        }
        #[cfg(target_arch = "x86")]
        {
            dst.write(0x68); // push imm32
            let addr32 = target_addr as u32;
            ptr::copy_nonoverlapping(addr32.to_le_bytes().as_ptr(), dst.add(1), 4);
            dst.add(5).write(0xC3); // ret
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
        {
            let _ = (dst, target_addr);
        }
    }

    /// Installs the trampoline and overwrites `target` with a jump to `replacement`.
    ///
    /// # Safety
    /// `self.target` must point to valid mapped executable instructions of at least
    /// `min_orig_bytes.max(JMP_ABS64_LEN)` bytes on an instruction boundary, and `self.replacement`
    /// must be a valid function pointer with a compatible ABI.
    pub unsafe fn install(&mut self, min_orig_bytes: usize) -> Result<(), String> {
        self.install_with_callback(min_orig_bytes, |_| {})
    }

    /// Installs the trampoline, invokes `on_trampoline_ready(trampoline_addr)` BEFORE writing
    /// the hook JMP at `self.target` (eliminating install-time races), then activates the patch.
    ///
    /// # Safety
    /// Same requirements as [`Self::install`].
    pub unsafe fn install_with_callback<F>(
        &mut self,
        min_orig_bytes: usize,
        on_trampoline_ready: F,
    ) -> Result<(), String>
    where
        F: FnOnce(usize),
    {
        if self.active {
            return Err("patch already installed".into());
        }
        if self.target.is_null() || (self.target as usize) < MIN_VALID_USER_ADDR {
            return Err(format!("invalid target address {:p}", self.target));
        }
        if self.replacement.is_null() || (self.replacement as usize) < MIN_VALID_USER_ADDR {
            return Err(format!(
                "invalid replacement address {:p}",
                self.replacement
            ));
        }

        let overwrite_len = JMP_ABS64_LEN.max(min_orig_bytes);
        if overwrite_len + JMP_ABS64_LEN > TRAMPOLINE_SIZE {
            return Err("overwrite_len exceeds trampoline capacity".into());
        }

        validate_patch_target_range(self.target as usize, overwrite_len)?;

        let orig_slice = std::slice::from_raw_parts(self.target, overwrite_len);
        self.original_bytes = orig_slice.to_vec();
        self.orig_len = overwrite_len;

        let (trampoline, alloc_size) = Self::allocate_trampoline(TRAMPOLINE_SIZE)?;

        let ret_addr = (self.target as usize)
            .checked_add(overwrite_len)
            .ok_or_else(|| {
                let _ = libc::munmap(trampoline as *mut libc::c_void, alloc_size);
                "trampoline return address overflow".to_string()
            })?;
        ptr::copy_nonoverlapping(self.target, trampoline, overwrite_len);
        Self::write_abs_jmp64(trampoline.add(overwrite_len), ret_addr);

        // Lock trampoline page to R-X (W^X) before publishing.
        if libc::mprotect(
            trampoline as *mut libc::c_void,
            alloc_size,
            libc::PROT_READ | libc::PROT_EXEC,
        ) != 0
        {
            let err = io::Error::last_os_error();
            let _ = libc::munmap(trampoline as *mut libc::c_void, alloc_size);
            let msg = format!(
                "mprotect(PROT_READ|PROT_EXEC) failed for trampoline {:p} (errno={:?}: {})",
                trampoline,
                err.raw_os_error(),
                err
            );
            eprintln!("[memory_patch] {}", msg);
            return Err(msg);
        }

        let page_size = system_page_size();
        let (page_start, page_len) =
            match page_bounds(self.target as usize, overwrite_len, page_size) {
                Ok(b) => b,
                Err(e) => {
                    let _ = libc::munmap(trampoline as *mut libc::c_void, alloc_size);
                    return Err(e);
                }
            };

        if libc::mprotect(
            page_start as *mut libc::c_void,
            page_len,
            libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
        ) != 0
        {
            let err = io::Error::last_os_error();
            let _ = libc::munmap(trampoline as *mut libc::c_void, alloc_size);
            let msg = format!(
                "mprotect(RWX) failed for target {:p} [page=0x{:x}, len={}] (errno={:?}: {})",
                self.target,
                page_start,
                page_len,
                err.raw_os_error(),
                err
            );
            eprintln!("[memory_patch] {}", msg);
            return Err(msg);
        }

        // Publish trampoline pointer to caller BEFORE overwriting target bytes to avoid install-time race.
        on_trampoline_ready(trampoline as usize);

        Self::write_abs_jmp64(self.target, self.replacement as usize);
        for i in JMP_ABS64_LEN..overwrite_len {
            self.target.add(i).write(0x90);
        }
        _mm_mfence();

        // Restore W^X (PROT_READ | PROT_EXEC) on target code page.
        if libc::mprotect(
            page_start as *mut libc::c_void,
            page_len,
            libc::PROT_READ | libc::PROT_EXEC,
        ) != 0
        {
            let err = io::Error::last_os_error();
            eprintln!(
                "[memory_patch] warning: failed to restore PROT_READ|PROT_EXEC at {:p} (errno={:?}: {})",
                self.target,
                err.raw_os_error(),
                err
            );
        }

        self.trampoline = trampoline;
        self.trampoline_alloc_size = alloc_size;
        self.active = true;
        Ok(())
    }

    /// Restores the original bytes at `self.target` and releases the trampoline mapping.
    ///
    /// # Safety
    /// `self.target` must still be mapped in the current address space and no thread
    /// may be concurrently executing mid-stub at `self.target`.
    pub unsafe fn uninstall(&mut self) -> Result<(), String> {
        if !self.active {
            return Err("patch not installed".into());
        }
        if self.target.is_null()
            || validate_patch_target_range(self.target as usize, self.orig_len).is_err()
        {
            self.active = false;
            return Err(format!(
                "cannot uninstall patch at {:p}: target page no longer mapped executable",
                self.target
            ));
        }

        let page_size = system_page_size();
        let (page_start, page_len) = page_bounds(self.target as usize, self.orig_len, page_size)?;

        if libc::mprotect(
            page_start as *mut libc::c_void,
            page_len,
            libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
        ) != 0
        {
            let err = io::Error::last_os_error();
            let msg = format!(
                "mprotect(RWX) failed during uninstall at {:p} (errno={:?}: {})",
                self.target,
                err.raw_os_error(),
                err
            );
            eprintln!("[memory_patch] {}", msg);
            return Err(msg);
        }

        ptr::copy_nonoverlapping(self.original_bytes.as_ptr(), self.target, self.orig_len);
        _mm_mfence();

        if libc::mprotect(
            page_start as *mut libc::c_void,
            page_len,
            libc::PROT_READ | libc::PROT_EXEC,
        ) != 0
        {
            let err = io::Error::last_os_error();
            eprintln!(
                "[memory_patch] warning: failed to restore R-X after uninstall at {:p} (errno={:?}: {})",
                self.target,
                err.raw_os_error(),
                err
            );
        }

        if !self.trampoline.is_null() && self.trampoline_alloc_size > 0 {
            let _ = libc::munmap(
                self.trampoline as *mut libc::c_void,
                self.trampoline_alloc_size,
            );
            self.trampoline = ptr::null_mut();
            self.trampoline_alloc_size = 0;
        }

        self.active = false;
        Ok(())
    }

    /// Invokes the saved trampoline as a `void(void)` C function.
    ///
    /// # Safety
    /// The patch must be active and the patched function must accept zero arguments.
    pub unsafe fn call_original_void(&self) {
        if !self.active || self.trampoline.is_null() {
            return;
        }
        let tramp: unsafe extern "C" fn() = std::mem::transmute(self.trampoline);
        tramp()
    }

    fn allocate_trampoline(size: usize) -> Result<(*mut u8, usize), String> {
        let page_size = system_page_size();
        let alloc_size = size
            .checked_add(page_size - 1)
            .ok_or_else(|| "trampoline size overflow".to_string())?
            & !(page_size - 1);
        let ptr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                alloc_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED || ptr.is_null() {
            let err = io::Error::last_os_error();
            let msg = format!(
                "mmap failed for trampoline ({} bytes) (errno={:?}: {})",
                alloc_size,
                err.raw_os_error(),
                err
            );
            eprintln!("[memory_patch] {}", msg);
            return Err(msg);
        }
        Ok((ptr as *mut u8, alloc_size))
    }
}

unsafe impl Send for MemoryPatch {}
unsafe impl Sync for MemoryPatch {}

impl Drop for MemoryPatch {
    fn drop(&mut self) {
        if self.active {
            let _ = unsafe { self.uninstall() };
        } else if !self.trampoline.is_null() && self.trampoline_alloc_size > 0 {
            unsafe {
                let _ = libc::munmap(
                    self.trampoline as *mut libc::c_void,
                    self.trampoline_alloc_size,
                );
            }
            self.trampoline = ptr::null_mut();
            self.trampoline_alloc_size = 0;
        }
    }
}

/// Creates and installs an inline patch at `base + offset` redirecting to `replacement`.
///
/// # Safety
/// `(base + offset)` must point to valid executable instructions of at least 14 bytes,
/// and `replacement` must be a valid function pointer with matching calling convention.
pub unsafe fn patch_by_offset(
    base: u64,
    offset: u64,
    replacement: *mut u8,
) -> Result<MemoryPatch, String> {
    patch_by_offset_with_callback(base, offset, replacement, |_| {})
}

/// Creates and installs an inline patch at `base + offset`, invoking `on_trampoline_ready`
/// before writing the JMP so hook statics are populated race-free.
///
/// # Safety
/// Same requirements as [`patch_by_offset`].
pub unsafe fn patch_by_offset_with_callback<F>(
    base: u64,
    offset: u64,
    replacement: *mut u8,
    on_trampoline_ready: F,
) -> Result<MemoryPatch, String>
where
    F: FnOnce(usize),
{
    let addr = base
        .checked_add(offset)
        .ok_or_else(|| "patch_by_offset address overflow".to_string())?;
    let target = addr as usize as *mut u8;
    let mut patch = MemoryPatch::new(target, replacement);
    patch.install_with_callback(JMP_ABS64_LEN, on_trampoline_ready)?;
    Ok(patch)
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn dummy_replacement() -> u32 {
        99
    }

    #[test]
    fn rejects_null_low_and_unmapped_target_addresses_without_segfault() {
        let repl = dummy_replacement as *const () as *mut u8;

        let res_null = unsafe { patch_by_offset(0, 0, repl) };
        assert!(res_null.is_err());

        // Typical un-rebased ELF RVA (e.g. 0x45000) is NOT mapped executable in this process
        let res_rva = unsafe { patch_by_offset(0x45000, 0, repl) };
        assert!(res_rva.is_err());
    }

    #[test]
    fn parse_maps_checks_executable_permissions_and_bounds() {
        let maps = "\
00400000-00401000 r--p 00000000 08:01 1 /bin/example
00401000-00405000 r-xp 00001000 08:01 1 /bin/example
00405000-00406000 rw-p 00005000 08:01 1 /bin/example
";
        assert!(!parse_maps_contains_exec_range(maps, 0x0040_0000, 14));
        assert!(parse_maps_contains_exec_range(maps, 0x0040_1000, 14));
        assert!(!parse_maps_contains_exec_range(maps, 0x0040_4FF8, 14));
        assert!(!parse_maps_contains_exec_range(maps, 0x0040_5000, 14));
    }

    #[test]
    fn installs_and_uninstalls_patch_on_mapped_executable_code() {
        // Allocate an executable page containing a simple 16-byte function:
        // 14 NOPs + `mov eax, 42; ret` (B8 2A 00 00 00 C3)
        let page_size = system_page_size();
        let code_page = unsafe {
            libc::mmap(
                ptr::null_mut(),
                page_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(code_page, libc::MAP_FAILED);

        unsafe {
            let dst = code_page as *mut u8;
            let mut fn_bytes = vec![0x90u8; 16];
            fn_bytes.extend_from_slice(&[0xB8, 0x2A, 0x00, 0x00, 0x00, 0xC3]);
            ptr::copy_nonoverlapping(fn_bytes.as_ptr(), dst, fn_bytes.len());
            assert_eq!(
                libc::mprotect(code_page, page_size, libc::PROT_READ | libc::PROT_EXEC),
                0
            );

            let orig_fn: extern "C" fn() -> u32 = std::mem::transmute(dst);
            assert_eq!(orig_fn(), 42);

            let repl = dummy_replacement as *const () as *mut u8;
            let mut patch = patch_by_offset(dst as usize as u64, 0, repl).unwrap();
            assert_eq!(orig_fn(), 99);

            // Calling the saved trampoline still runs original prologue + body -> 42
            let tramp_fn: extern "C" fn() -> u32 = std::mem::transmute(patch.trampoline);
            assert_eq!(tramp_fn(), 42);

            patch.uninstall().unwrap();
            assert_eq!(orig_fn(), 42);

            libc::munmap(code_page, page_size);
        }
    }

    #[test]
    fn validates_module_handle_image_bounds_without_crashing_on_anon_pages() {
        assert!(!is_range_in_module_image(std::ptr::null_mut(), 0x1000, 4));
        let handle = crate::steam_bridge::ost_platform::dynamic_library::load(
            std::path::Path::new("libc.so.6"),
        );
        assert!(!handle.is_null());
        let sym = unsafe { libc::dlsym(handle, b"getpid\0".as_ptr().cast()) };
        if !sym.is_null() {
            assert!(is_range_in_module_image(handle, sym as usize, 4));
        }
        assert!(!is_range_in_module_image(handle, 0x1000, 4));
    }
}
