//! Memory patching via 64-bit absolute indirect trampolines (`FF 25 00 00 00 00 <u64>`).
//!
//! On Linux x86-64, ASLR places `mmap` regions and shared objects > 2 GiB apart, so 32-bit
//! `E9` relative jumps can overflow. We use a 14-byte RIP-relative indirect jump
//! (`jmp qword ptr [rip + 0]` followed by an 8-byte 64-bit target address) which reaches
//! any 64-bit virtual address cleanly.

use std::arch::x86_64::_mm_mfence;
use std::ptr;

pub const JMP_ABS64_LEN: usize = 14;
const TRAMPOLINE_SIZE: usize = 64;

/// Represents an installed inline memory patch.
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

    /// Writes a 14-byte x86-64 absolute jump (`FF 25 00 00 00 00` + 8-byte address) at `dst`.
    ///
    /// # Safety
    /// `dst` must point to at least 14 bytes of writable memory.
    unsafe fn write_abs_jmp64(dst: *mut u8, target_addr: usize) {
        let stub: [u8; 6] = [0xFF, 0x25, 0x00, 0x00, 0x00, 0x00];
        ptr::copy_nonoverlapping(stub.as_ptr(), dst, stub.len());
        ptr::copy_nonoverlapping(target_addr.to_le_bytes().as_ptr(), dst.add(stub.len()), 8);
    }

    /// Installs the trampoline and overwrites `target` with a jump to `replacement`.
    ///
    /// # Safety
    /// `self.target` must point to valid mapped executable instructions of at least
    /// `min_orig_bytes.max(14)` bytes on an instruction boundary, and `self.replacement`
    /// must be a valid function pointer with a compatible ABI.
    pub unsafe fn install(&mut self, min_orig_bytes: usize) -> Result<(), String> {
        if self.active {
            return Err("patch already installed".into());
        }

        let overwrite_len = JMP_ABS64_LEN.max(min_orig_bytes);
        if overwrite_len + JMP_ABS64_LEN > TRAMPOLINE_SIZE {
            return Err("overwrite_len exceeds trampoline capacity".into());
        }

        let orig_slice = std::slice::from_raw_parts(self.target, overwrite_len);
        self.original_bytes = orig_slice.to_vec();
        self.orig_len = overwrite_len;

        let trampoline = Self::allocate_trampoline(TRAMPOLINE_SIZE)?;
        self.trampoline = trampoline;

        let ret_addr = self.target.add(overwrite_len) as usize;
        ptr::copy_nonoverlapping(self.target, trampoline, overwrite_len);
        Self::write_abs_jmp64(trampoline.add(overwrite_len), ret_addr);

        let page_size = 4096usize;
        let page_start = (self.target as usize) & !(page_size - 1);
        let page_end = ((self.target as usize) + overwrite_len + page_size - 1) & !(page_size - 1);
        let page_len = page_end - page_start;

        if libc::mprotect(
            page_start as *mut libc::c_void,
            page_len,
            libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
        ) != 0
        {
            return Err(format!("mprotect failed for target at {:p}", self.target));
        }

        Self::write_abs_jmp64(self.target, self.replacement as usize);
        for i in JMP_ABS64_LEN..overwrite_len {
            self.target.add(i).write(0x90);
        }

        _mm_mfence();
        self.active = true;
        Ok(())
    }

    /// Restores the original bytes at `self.target`.
    ///
    /// # Safety
    /// `self.target` must still be mapped in the current address space and no thread
    /// may be concurrently executing mid-stub at `self.target`.
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
        _mm_mfence();
        self.active = false;
        Ok(())
    }

    /// Invokes the saved trampoline as a `void(void)` C function.
    ///
    /// # Safety
    /// The patch must be active and the patched function must accept zero arguments.
    pub unsafe fn call_original_void(&self) {
        let tramp: unsafe extern "C" fn() = std::mem::transmute(self.trampoline);
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
    let target = (base as *mut u8).add(offset as usize);
    let mut patch = MemoryPatch::new(target, replacement);
    patch.install(JMP_ABS64_LEN)?;
    Ok(patch)
}
