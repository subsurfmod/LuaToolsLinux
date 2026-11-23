//! Memory patching via trampolines.
//!
//! On Linux x86-64 we overwrite the first bytes of a target function with a
//! relative jump to our replacement, copy the overwritten bytes into a
//! trampoline buffer (with a jump back appended), and call the trampoline to
//! reach the original code.
//!
//! This is intended to be installed at process startup via the preload loader.

use std::arch::x86_64::_mm_mfence;
use std::ptr;

const JMP_REL32_LEN: usize = 14;
const TRAMPOLINE_SIZE: usize = 32;

/// Represents an installed memory patch.
#[derive(Debug)]
pub struct MemoryPatch {
    pub target: *mut u8,
    pub trampoline: *mut u8,
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
            original_bytes: Vec::new(),
            orig_len: 0,
            active: false,
        }
    }

    pub unsafe fn install(&mut self, min_orig_bytes: usize) -> Result<(), String> {
        if self.active {
            return Err("patch already installed".into());
        }

        let overwrite_len = JMP_REL32_LEN.max(min_orig_bytes);

        let orig_slice = std::slice::from_raw_parts(self.target, overwrite_len);
        self.original_bytes = orig_slice.to_vec();
        self.orig_len = overwrite_len;

        let trampoline = Self::allocate_trampoline(TRAMPOLINE_SIZE)?;
        self.trampoline = trampoline;

        let ret_addr = self.target.add(overwrite_len);
        let back_disp = (ret_addr as isize) - (trampoline as isize) - JMP_REL32_LEN as isize;

        ptr::copy_nonoverlapping(self.target, trampoline, overwrite_len);
        let tramp_jmp_ptr = trampoline.add(overwrite_len);
        tramp_jmp_ptr.write(0xE9);
        let disp_ptr = tramp_jmp_ptr.add(1) as *mut i32;
        disp_ptr.write(back_disp as i32);

        let page_size = 4096usize;
        let page_start = (self.target as usize) & !(page_size - 1);
        let page_end =
            ((self.target as usize) + overwrite_len + page_size - 1) & !(page_size - 1);
        let page_len = page_end - page_start;

        if libc::mprotect(
            page_start as *mut libc::c_void,
            page_len,
            libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
        ) != 0
        {
            return Err(format!("mprotect failed for target at {:p}", self.target));
        }

        let disp = (self.replacement as isize) - (self.target as isize) - JMP_REL32_LEN as isize;
        self.target.write(0xE9);
        let disp_ptr = self.target.add(1) as *mut i32;
        disp_ptr.write(disp as i32);

        for i in 5..overwrite_len {
            self.target.add(i).write(0x90);
        }

        _mm_mfence();
        self.active = true;
        Ok(())
    }

    pub unsafe fn uninstall(&mut self) -> Result<(), String> {
        if !self.active {
            return Err("patch not installed".into());
        }
        let page_size = 4096usize;
        let page_start = (self.target as usize) & !(page_size - 1);
        let page_end = ((self.target as usize) + self.orig_len + page_size - 1) & !(page_size - 1);
        let page_len = page_end - page_start;

        if libc::mprotect(
            page_start as *mut libc::c_void,
            page_len,
            libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
        ) != 0
        {
            return Err("mprotect failed during removal".into());
        }

        ptr::copy_nonoverlapping(self.original_bytes.as_ptr(), self.target, self.orig_len);
        self.active = false;
        Ok(())
    }

    pub unsafe fn call_original_void(&self) {
        let tramp: extern "C" fn() = std::mem::transmute(self.trampoline);
        tramp()
    }

    fn allocate_trampoline(size: usize) -> Result<*mut u8, String> {
        let page_size = 4096usize;
        let alloc_size = (size + page_size - 1) & !(page_size - 1);
        let ptr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                alloc_size,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err("mmap failed for trampoline".into());
        }
        Ok(ptr as *mut u8)
    }
}

unsafe impl Send for MemoryPatch {}

unsafe impl Sync for MemoryPatch {}

impl Drop for MemoryPatch {
    fn drop(&mut self) {
        if self.active {
            let _ = unsafe { self.uninstall() };
        }
    }
}

pub unsafe fn patch_by_offset(
    base: u64,
    offset: u64,
    replacement: *mut u8,
) -> Result<MemoryPatch, String> {
    let target = (base as *mut u8).add(offset as usize);
    let mut patch = MemoryPatch::new(target, replacement);
    patch.install(0)?;
    Ok(patch)
}
