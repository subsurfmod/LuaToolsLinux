//! InteropKeyValues - KeyValues::ReadAsBinary hook.
//!
//! Port of usefulsteamtools `Interop_KeyValues.cpp`.
//! Handles intercepting KeyValues binary reading for manifest processing.

use std::os::raw::c_void;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Mutex, OnceLock};

use crate::steam_bridge::memory_patch::{self, MemoryPatch, JMP_ABS64_LEN};
use crate::steam_bridge::pattern_db::{BinaryComponent, PatternChannel, PatternDb};

/// KeyValues data types.
#[allow(dead_code)]
pub mod EKeyValuesType {
    pub const TYPE_NONE: u32 = 0;
    pub const TYPE_STRING: u32 = 1;
    pub const TYPE_INT: u32 = 2;
    pub const TYPE_FLOAT: u32 = 3;
    pub const TYPE_PTR: u32 = 4;
    pub const TYPE_WSTRING: u32 = 5;
    pub const TYPE_COLOR: u32 = 6;
    pub const TYPE_UINT64: u32 = 7;
    pub const TYPE_INT64: u32 = 8;
}

/// KeyValues structure matching Steam's internal layout.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct KeyValues {
    pub value: KeyValuesValue,
    pub chain: *mut KeyValues,
    pub packed_key_and_type: u32,
    pub flags: u32,
    pub peer: *mut KeyValues,
}

/// KeyValues value union.
#[repr(C)]
#[derive(Clone, Copy)]
pub union KeyValuesValue {
    pub sub: *mut KeyValues,
    pub string: *mut i8,
    pub wstring: *mut u16,
    pub int_val: i32,
    pub float_val: f32,
    pub ptr: *mut c_void,
    pub uint64_val: u64,
    pub int64_val: i64,
    pub color: [u8; 8],
}

impl std::fmt::Debug for KeyValuesValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyValuesValue")
            .field("sub", unsafe { &self.sub })
            .field("string", unsafe { &self.string })
            .finish()
    }
}

impl std::fmt::Debug for KeyValues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyValues")
            .field("value", &self.value)
            .field("chain", &self.chain)
            .field("packed_key_and_type", &self.packed_key_and_type)
            .field("flags", &self.flags)
            .field("peer", &self.peer)
            .finish()
    }
}

/// Installed patches.
static INSTALLED_KEYVALUES_PATCHES: OnceLock<Mutex<Vec<MemoryPatch>>> = OnceLock::new();

/// Function pointer type for KeyValues::ReadAsBinary.
type KeyValuesReadAsBinaryFn = unsafe extern "C" fn(
    *mut c_void,   // this (KeyValues*)
    *mut u8,       // output buffer
    u32,           // buffer size
) -> u32;

/// Original function pointer.
static ORIGINAL_KEYVALUES_READ_AS_BINARY: OnceLock<Mutex<Option<KeyValuesReadAsBinaryFn>>> = OnceLock::new();

fn installed_keyvalues_patches() -> &'static Mutex<Vec<MemoryPatch>> {
    INSTALLED_KEYVALUES_PATCHES.get_or_init(|| Mutex::new(Vec::new()))
}

fn original_keyvalues_read_as_binary() -> &'static Mutex<Option<KeyValuesReadAsBinaryFn>> {
    ORIGINAL_KEYVALUES_READ_AS_BINARY.get_or_init(|| Mutex::new(None))
}

/// Detour for KeyValues::ReadAsBinary.
unsafe extern "C" fn keyvalues_read_as_binary_detour(
    this: *mut c_void,
    buffer: *mut u8,
    buffer_size: u32,
) -> u32 {
    panic::catch_unwind(AssertUnwindSafe(|| {
        // In the Windows version, this hook intercepts KeyValues binary reads
        // for manifest processing. On Linux, we can implement similar logic.

        let orig = *original_keyvalues_read_as_binary()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(orig_fn) = orig {
            orig_fn(this, buffer, buffer_size)
        } else {
            0
        }
    }))
    .unwrap_or(0)
}

/// Resolve function address using pattern scanning.
fn resolve_function_address(
    steam_root: &std::path::Path,
    component: BinaryComponent,
    channel: PatternChannel,
    pattern_name: &str,
) -> Result<u64, String> {
    let binaries = crate::steam_bridge::binary_reader::scan_steam_binaries(steam_root);
    let binary = binaries
        .into_iter()
        .find(|b| {
            b.path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| match component {
                    BinaryComponent::SteamClient => n.contains("steamclient"),
                    BinaryComponent::SteamUi => n.contains("steamui"),
                })
                .unwrap_or(false)
        })
        .ok_or_else(|| "Steam binary not found".to_string())?;

    let pattern_db = PatternDb::new();
    let pf = pattern_db
        .lookup_by_binary(&binary, channel, component)
        .map_err(|e| format!("Pattern lookup failed: {}", e))?;

    let entry = pf
        .patterns
        .get(pattern_name)
        .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;

    let mod_name = binary
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| "Binary has no filename".to_string())?;
    let mapping = crate::steam_bridge::binary_reader::find_loaded_module_mapping(&mod_name)
        .ok_or_else(|| "Module not loaded".to_string())?;

    let vaddr = if let Some(pattern_hex) = &entry.pattern {
        let (pattern_bytes, mask_bytes) =
            crate::steam_bridge::manifest_hooks::parse_pattern_and_mask(pattern_hex, entry.mask.as_deref())
                .ok_or_else(|| "Malformed hex pattern or mask".to_string())?;
        let offset = entry.offset.unwrap_or(0);
        binary.resolve_pattern_address(&pattern_bytes, mask_bytes.as_deref(), offset, None)
    } else {
        entry.address
    }
    .ok_or_else(|| "Failed to resolve pattern address".to_string())?;

    binary
        .vaddr_to_runtime_addr(vaddr, &mapping, JMP_ABS64_LEN)
        .ok_or_else(|| "Resolved address not in executable memory".to_string())
}

/// Install KeyValues hooks.
pub fn install(steam_root: &std::path::Path) -> Result<(), String> {
    log::info!("Installing InteropKeyValues hooks...");

    if let Ok(addr) = resolve_function_address(
        steam_root,
        BinaryComponent::SteamClient,
        PatternChannel::Pattern,
        "KeyValuesReadAsBinary",
    ) {
        let replacement = keyvalues_read_as_binary_detour as *const () as *mut u8;
        let patch = unsafe { memory_patch::patch_by_offset(addr, 0, replacement) }
            .map_err(|e| format!("Failed to install KeyValuesReadAsBinary hook: {}", e))?;

        if !patch.trampoline.is_null() {
            let orig_fn: KeyValuesReadAsBinaryFn = unsafe { std::mem::transmute(patch.trampoline) };
            *original_keyvalues_read_as_binary()
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(orig_fn);
        }
        installed_keyvalues_patches()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(patch);
        log::info!("Installed KeyValuesReadAsBinary hook at 0x{:x}", addr);
    }

    log::info!("InteropKeyValues hooks installed");
    Ok(())
}

/// Uninstall KeyValues hooks.
pub fn uninstall() -> Result<(), String> {
    *original_keyvalues_read_as_binary()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;

    let mut patches = installed_keyvalues_patches()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for mut patch in patches.drain(..) {
        unsafe { patch.uninstall() }?;
    }

    log::info!("InteropKeyValues hooks uninstalled");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keyvalues_struct_sizes() {
        assert_eq!(std::mem::size_of::<KeyValues>(), 32); // 0x20 bytes
    }
}