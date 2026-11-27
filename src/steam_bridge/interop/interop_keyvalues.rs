//! InteropKeyValues - KeyValues::ReadAsBinary and FindOrCreateKey hooks.
//!
//! Port of usefulsteamtools `Hooks_KeyValues.cpp` and `HookManager.cpp:30`.
//! In the upstream Windows BST baseline (`HookManager.cpp:30`), `Hooks_KeyValues::Install()`
//! was commented out and disabled because manifest depot patching was relocated to
//! `Hooks_Manifest::BuildDepotDependency`.
//!
//! This module preserves full ABI parity with Steam's 5-argument `KeyValues::ReadAsBinary`
//! and `FindOrCreateKey`, while remaining dormant by default matching upstream.

use std::os::raw::c_void;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::steam_bridge::memory_patch::{self, MemoryPatch, JMP_ABS64_LEN};
use crate::steam_bridge::pattern_db::{BinaryComponent, PatternChannel, PatternDb};

/// KeyValues data types matching Valve's `EKeyValuesType`.
#[allow(dead_code, non_snake_case)]
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

/// KeyValues structure matching Steam's internal 32-byte layout (`static_assert(sizeof(KeyValues) == 0x20)`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct KeyValues {
    pub value: KeyValuesValue,
    pub chain: *mut KeyValues,
    pub packed_key_and_type: u32,
    pub flags: u32,
    pub peer: *mut KeyValues,
}

/// KeyValues value union (8 bytes).
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

/// Upstream enabled state. In Windows BST baseline (`HookManager.cpp:30`), this is disabled.
static UPSTREAM_ENABLED: AtomicBool = AtomicBool::new(false);

/// Installed memory patches.
static INSTALLED_KEYVALUES_PATCHES: OnceLock<Mutex<Vec<MemoryPatch>>> = OnceLock::new();

/// Function pointer type for `KeyValues::ReadAsBinary`:
/// `bool ReadAsBinary(KeyValues* root, void* buf, int depth, bool textMode, void* symTable)`
pub type KeyValuesReadAsBinaryFn = unsafe extern "C" fn(
    root: *mut KeyValues,
    buf: *mut c_void,
    depth: i32,
    text_mode: bool,
    sym_table: *mut c_void,
) -> bool;

/// Function pointer type for `KeyValues::FindOrCreateKey`:
/// `KeyValues* FindOrCreateKey(KeyValues* parent, const char* name, bool create, KeyValues** out)`
pub type FindOrCreateKeyFn = unsafe extern "C" fn(
    parent: *mut KeyValues,
    name: *const std::os::raw::c_char,
    create: bool,
    out: *mut *mut KeyValues,
) -> *mut KeyValues;

/// Original function pointers.
static ORIGINAL_KEYVALUES_READ_AS_BINARY: OnceLock<Mutex<Option<KeyValuesReadAsBinaryFn>>> =
    OnceLock::new();
static ORIGINAL_FIND_OR_CREATE_KEY: OnceLock<Mutex<Option<FindOrCreateKeyFn>>> = OnceLock::new();

fn installed_keyvalues_patches() -> &'static Mutex<Vec<MemoryPatch>> {
    INSTALLED_KEYVALUES_PATCHES.get_or_init(|| Mutex::new(Vec::new()))
}

fn original_keyvalues_read_as_binary() -> &'static Mutex<Option<KeyValuesReadAsBinaryFn>> {
    ORIGINAL_KEYVALUES_READ_AS_BINARY.get_or_init(|| Mutex::new(None))
}

fn original_find_or_create_key() -> &'static Mutex<Option<FindOrCreateKeyFn>> {
    ORIGINAL_FIND_OR_CREATE_KEY.get_or_init(|| Mutex::new(None))
}

/// Check whether KeyValues hooks are enabled upstream.
///
/// In the Windows BST baseline (`HookManager.cpp:30`), `Hooks_KeyValues::Install()` is
/// commented out / disabled upstream because manifest patching was moved to
/// `Hooks_Manifest::BuildDepotDependency`.
pub fn is_enabled_upstream() -> bool {
    UPSTREAM_ENABLED.load(Ordering::SeqCst)
}

/// Configure whether KeyValues hooks should be enabled.
pub fn set_enabled_upstream(enabled: bool) {
    UPSTREAM_ENABLED.store(enabled, Ordering::SeqCst);
}

/// Helper matching BST `KV_FindKey(parent, name)`:
/// Calls FindOrCreateKey(parent, name, false, nullptr) to find a child key without creating it.
pub unsafe fn kv_find_key(parent: *mut KeyValues, name: &str) -> *mut KeyValues {
    let orig = *original_find_or_create_key()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(find_fn) = orig {
        if let Ok(c_name) = std::ffi::CString::new(name) {
            return find_fn(parent, c_name.as_ptr(), false, std::ptr::null_mut());
        }
    }
    std::ptr::null_mut()
}

/// Detour for `KeyValues::ReadAsBinary`.
/// 5-argument C ABI matching `Hooks_KeyValues.cpp:35`.
unsafe extern "C" fn keyvalues_read_as_binary_detour(
    root: *mut KeyValues,
    buf: *mut c_void,
    depth: i32,
    text_mode: bool,
    sym_table: *mut c_void,
) -> bool {
    panic::catch_unwind(AssertUnwindSafe(|| {
        let orig = *original_keyvalues_read_as_binary()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(orig_fn) = orig {
            orig_fn(root, buf, depth, text_mode, sym_table)
        } else {
            false
        }
    }))
    .unwrap_or(false)
}

/// Resolve function address using pattern scanning.
fn resolve_function_address(
    steam_root: &std::path::Path,
    component: BinaryComponent,
    channel: PatternChannel,
    pattern_name: &str,
) -> Result<u64, String> {
    let expected_class = if cfg!(target_pointer_width = "64") {
        crate::steam_bridge::binary_reader::ElfClass::Class64
    } else {
        crate::steam_bridge::binary_reader::ElfClass::Class32
    };

    let binaries = crate::steam_bridge::binary_reader::scan_steam_binaries(steam_root);
    let binary = binaries
        .into_iter()
        .find(|b| {
            b.class == expected_class
                && b.path
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

    if pf.is_fallback() {
        return Err("Fallback patterns not permitted for in-process memory hooks".to_string());
    }

    let entry = pf
        .patterns
        .get(pattern_name)
        .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;

    if entry.is_generic_compiler_prologue() {
        return Err(format!("Pattern '{}' is a generic compiler prologue", pattern_name));
    }

    let mod_name = binary
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| "Binary has no filename".to_string())?;
    let mapping = crate::steam_bridge::binary_reader::find_loaded_module_mapping(&mod_name)
        .ok_or_else(|| "Module not loaded".to_string())?;

    let vaddr = if let Some(pattern_hex) = &entry.pattern {
        let (pattern_bytes, mask_bytes) =
            crate::steam_bridge::manifest_hooks::parse_pattern_and_mask(
                pattern_hex,
                entry.mask.as_deref(),
            )
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
///
/// Adheres strictly to upstream parity (`HookManager.cpp:30`): if not enabled upstream,
/// logs that `Hooks_KeyValues` is disabled upstream and returns `Ok(())` remaining dormant.
pub fn install(steam_root: &std::path::Path) -> Result<(), String> {
    if !is_enabled_upstream() {
        log::info!(
            "[InteropKeyValues] Hooks_KeyValues is disabled upstream (HookManager.cpp:30) - remaining dormant"
        );
        return Ok(());
    }
    install_forced(steam_root)
}

/// Force installation of KeyValues hooks even when disabled upstream.
pub fn install_forced(steam_root: &std::path::Path) -> Result<(), String> {
    log::info!("Installing InteropKeyValues hooks...");

    // Resolve FindOrCreateKey
    if let Ok(addr) = resolve_function_address(
        steam_root,
        BinaryComponent::SteamClient,
        PatternChannel::Pattern,
        "FindOrCreateKey",
    ) {
        let fn_ptr: FindOrCreateKeyFn = unsafe { std::mem::transmute(addr as usize) };
        *original_find_or_create_key()
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(fn_ptr);
        log::info!("Resolved FindOrCreateKey at 0x{:x}", addr);
    }

    if let Ok(addr) = resolve_function_address(
        steam_root,
        BinaryComponent::SteamClient,
        PatternChannel::Pattern,
        "KeyValuesReadAsBinary",
    ) {
        let replacement = keyvalues_read_as_binary_detour as *const () as *mut u8;
        let patch = unsafe {
            memory_patch::patch_by_offset_with_callback(addr, 0, replacement, |tramp| {
                if tramp != 0 {
                    let orig_fn: KeyValuesReadAsBinaryFn = std::mem::transmute(tramp);
                    *original_keyvalues_read_as_binary()
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = Some(orig_fn);
                }
            })
        }
        .map_err(|e| format!("Failed to install KeyValuesReadAsBinary hook: {}", e))?;

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
    *original_find_or_create_key()
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

    #[test]
    fn test_keyvalues_upstream_disabled_by_default() {
        assert!(!is_enabled_upstream());
        let tmp = tempfile::tempdir().unwrap();
        // install() succeeds cleanly without attempting binary patching
        assert!(install(tmp.path()).is_ok());
        assert_eq!(installed_keyvalues_patches().lock().unwrap().len(), 0);

        set_enabled_upstream(true);
        assert!(is_enabled_upstream());
        set_enabled_upstream(false);
        assert!(!is_enabled_upstream());
    }
}
