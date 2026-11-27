//! InteropDecryption - ConfigStoreGetBinary hook for depot decryption keys.
//!
//! Port of usefulsteamtools `Interop_Decryption.cpp`.
//! Handles intercepting Steam's ConfigStoreGetBinary to provide depot decryption keys from Lua config.

use std::collections::HashSet;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Mutex, OnceLock};

use crate::steam_bridge::memory_patch::{self, MemoryPatch, JMP_ABS64_LEN};
use crate::steam_bridge::pattern_db::{BinaryComponent, PatternChannel, PatternDb};

/// Robustly decode a hex string to bytes, handling common formats:
/// - Plain hex: "0123456789abcdef..."
/// - With 0x prefix: "0x0123456789abcdef..."
/// - With whitespace: "01 23 45 67 89 ab cd ef ..."
fn decode_hex_robust(hex: &str) -> Option<Vec<u8>> {
    let trimmed = hex.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Strip 0x/0X prefix if present
    let without_prefix = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);

    // Remove all whitespace
    let cleaned: String = without_prefix.chars().filter(|c| !c.is_whitespace()).collect();

    if cleaned.is_empty() || !cleaned.len().is_multiple_of(2) {
        return None;
    }

    // Decode hex pairs
    let mut out = Vec::with_capacity(cleaned.len() / 2);
    let bytes = cleaned.as_bytes();
    for chunk in bytes.as_chunks::<2>().0 {
        let s = std::str::from_utf8(chunk).ok()?;
        out.push(u8::from_str_radix(s, 16).ok()?);
    }
    Some(out)
}

/// ConfigStore type enum.
#[allow(dead_code, non_snake_case, non_upper_case_globals)]
pub mod EConfigStore {
    pub const k_EConfigStoreInvalid: u32 = 0;
    pub const k_EConfigStoreUserLocal: u32 = 1;
    pub const k_EConfigStoreSystem: u32 = 2;
    pub const k_EConfigStoreUserRoaming: u32 = 3;
}

/// Global state for ConfigStore.
// Use usize instead of *mut c_void for thread safety
static CONFIG_STORE_LOCAL_PTR: OnceLock<Mutex<Option<usize>>> = OnceLock::new();
static SEEN_APP_TICKET_IDS: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();

/// Installed patches.
static INSTALLED_DECRYPTION_PATCHES: OnceLock<Mutex<Vec<MemoryPatch>>> = OnceLock::new();

/// Function pointer type for ConfigStoreGetBinary.
type ConfigStoreGetBinaryFn = unsafe extern "C" fn(
    usize,         // pObject
    u32,           // eConfigStore
    *const i8,     // KeyName
    *mut i8,       // Key (output buffer)
    u32,           // KeySize
) -> i32;

/// Original function pointer.
static ORIGINAL_CONFIG_STORE_GET_BINARY: OnceLock<Mutex<Option<ConfigStoreGetBinaryFn>>> = OnceLock::new();

fn config_store_local_ptr() -> &'static Mutex<Option<usize>> {
    CONFIG_STORE_LOCAL_PTR.get_or_init(|| Mutex::new(None))
}

fn seen_app_ticket_ids() -> &'static Mutex<HashSet<u32>> {
    SEEN_APP_TICKET_IDS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn installed_decryption_patches() -> &'static Mutex<Vec<MemoryPatch>> {
    INSTALLED_DECRYPTION_PATCHES.get_or_init(|| Mutex::new(Vec::new()))
}

fn original_config_store_get_binary() -> &'static Mutex<Option<ConfigStoreGetBinaryFn>> {
    ORIGINAL_CONFIG_STORE_GET_BINARY.get_or_init(|| Mutex::new(None))
}

/// Detour for ConfigStoreGetBinary.
unsafe extern "C" fn config_store_get_binary_detour(
    p_object: usize,
    e_config_store: u32,
    key_name: *const i8,
    key: *mut i8,
    key_size: u32,
) -> i32 {
    panic::catch_unwind(AssertUnwindSafe(|| {
        // Capture local ConfigStore pointer
        if e_config_store == EConfigStore::k_EConfigStoreUserLocal && p_object != 0 {
            let mut store = config_store_local_ptr().lock().unwrap_or_else(|e| e.into_inner());
            if store.is_none() {
                *store = Some(p_object);
                log::debug!("[InteropDecryption] ConfigStoreGetBinary: captured local ConfigStore at {:x}", p_object);
            }
        }

        let name = if key_name.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(key_name).to_string_lossy().to_string()
        };

        eprintln!(
            "[InteropDecryption] ConfigStoreGetBinary called: pObject={:x}, eConfigStore={}, KeyName='{}', key_size={}",
            p_object, e_config_store, name, key_size
        );

        // Record appticket keys for forge fallback
        if e_config_store == EConfigStore::k_EConfigStoreUserLocal {
            let norm = name.replace('/', "\\");
            const APP_TICKET_PREFIX: &str = "apptickets\\";
            if norm.starts_with(APP_TICKET_PREFIX) {
                let rest = &norm[APP_TICKET_PREFIX.len()..];
                if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                    if let Ok(app_id) = rest.parse::<u32>() {
                        seen_app_ticket_ids()
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(app_id);
                    }
                }
            }
        }

        // Check for depot decryption key request
        // Expected shape: ".../<DepotId>\DecryptionKey" or ".../<DepotId>/DecryptionKey"
        let norm_name = name.replace('/', "\\");
        let lower_norm = norm_name.to_ascii_lowercase();
        if let Some(last) = lower_norm.rfind("\\decryptionkey") {
            let depot_id_str = match lower_norm[..last].rfind('\\') {
                Some(start) => &norm_name[start + 1..last],
                None => &norm_name[..last],
            };
            if let Ok(depot_id) = depot_id_str.parse::<u32>() {
                    if let Some(decryption_key) = get_decryption_key(depot_id) {
                        if key.is_null() {
                            return 0;
                        }
                        if key_size as usize >= decryption_key.len() {
                            eprintln!(
                                "[InteropDecryption] Providing decryption key for depot {}: {}",
                                depot_id,
                                hex::encode(&decryption_key)
                            );
                            std::ptr::copy_nonoverlapping(
                                decryption_key.as_ptr(),
                                key as *mut u8,
                                decryption_key.len(),
                            );
                            return decryption_key.len() as i32;
                        }
                        eprintln!(
                            "[InteropDecryption] Decryption key for depot {} is too large ({} bytes) for buffer ({} bytes)",
                            depot_id,
                            decryption_key.len(),
                            key_size
                        );
                    } else {
                        eprintln!(
                            "[InteropDecryption] No decryption key available for depot {} (not in Lua config or bridge_lib depot_keys)",
                            depot_id
                        );
                    }
                }
            }

        // Call original function
        let orig = *original_config_store_get_binary()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(orig_fn) = orig {
            orig_fn(p_object, e_config_store, key_name, key, key_size)
        } else {
            0
        }
    }))
    .unwrap_or(0)
}

/// Read a binary value from the local ConfigStore.
pub fn read_config_store_local_binary(key_name: &str) -> Vec<u8> {
    let store = match *config_store_local_ptr().lock().unwrap_or_else(|e| e.into_inner()) {
        Some(p) => p,
        None => {
            log::warn!("[InteropDecryption] GetConfigStoreLocalBinary: ConfigStoreGetBinary not ready");
            return Vec::new();
        }
    };

    let orig = match *original_config_store_get_binary().lock().unwrap_or_else(|e| e.into_inner()) {
        Some(f) => f,
        None => {
            log::warn!("[InteropDecryption] GetConfigStoreLocalBinary: ConfigStoreGetBinary not ready");
            return Vec::new();
        }
    };

    let mut value = vec![0u8; 1024];
    let result = unsafe {
        orig(
            store,
            EConfigStore::k_EConfigStoreUserLocal,
            std::ffi::CString::new(key_name).unwrap().as_ptr(),
            value.as_mut_ptr() as *mut i8,
            value.len() as u32,
        )
    };

    if result <= 0 {
        log::debug!("[InteropDecryption] GetConfigStoreLocalBinary: failed to read KeyName='{}'", key_name);
        return Vec::new();
    }

    value.truncate(result as usize);
    log::debug!(
        "[InteropDecryption] GetConfigStoreLocalBinary: got value for KeyName='{}' ({} bytes)",
        key_name, value.len()
    );
    value
}

/// Get decryption key for a depot (integrates with LuaConfig / bridge_lib depot_keys / SLSsteam config).
fn get_decryption_key(depot_id: u32) -> Option<Vec<u8>> {
    // 1. Try to get from bridge_lib's depot_keys store
    if let Some(hex) = crate::steam_bridge::interop::bridge_lib::read_depot_key(depot_id) {
        if let Some(key_bytes) = decode_hex_robust(&hex).filter(|b| b.len() == 32) {
            return Some(key_bytes);
        }
    }

    // 2. Fallback: check SLSsteam config (~/.config/SLSsteam/config.yaml)
    if let Some(home) = dirs::home_dir() {
        let sls_cfg = home.join(".config/SLSsteam/config.yaml");
        if let Ok(content) = std::fs::read_to_string(&sls_cfg) {
            let needle = format!("{}:", depot_id);
            for line in content.lines() {
                let line_trim = line.trim();
                if line_trim.starts_with(&needle) {
                    let hex_val = line_trim[needle.len()..]
                        .trim()
                        .trim_matches(|c| c == '"' || c == '\'');
                    if let Some(key_bytes) = decode_hex_robust(hex_val).filter(|b| b.len() == 32) {
                        return Some(key_bytes);
                    }
                }
            }
        }

        // 3. Fallback: check Steam config.vdf
        for vdf_rel in &["config/config.vdf", ".steam/steam/config/config.vdf", ".local/share/Steam/config/config.vdf"] {
            let vdf_path = home.join(vdf_rel);
            if let Ok(content) = std::fs::read_to_string(&vdf_path) {
                let needle = format!("\"{}\"", depot_id);
                if content.contains(&needle) {
                    let mut in_depot = false;
                    for line in content.lines() {
                        let line_trim = line.trim();
                        if line_trim.contains(&needle) {
                            in_depot = true;
                        } else if in_depot {
                            if line_trim.to_ascii_lowercase().contains("decryptionkey") {
                                let parts: Vec<&str> = line_trim.split('"').filter(|s| !s.trim().is_empty()).collect();
                                if parts.len() >= 2 {
                                    let hex_val = parts[parts.len() - 1];
                                    if let Some(key_bytes) = decode_hex_robust(hex_val).filter(|b| b.len() == 32) {
                                        return Some(key_bytes);
                                    }
                                }
                            } else if line_trim == "}" {
                                in_depot = false;
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

/// Check if a depot decryption key is available (public API for verification).
pub fn has_depot_decryption_key(depot_id: u32) -> bool {
    get_decryption_key(depot_id).is_some()
}

/// Get cached app ownership ticket.
pub fn get_cache_app_ownership_ticket(app_id: u32) -> Vec<u8> {
    let ticket = read_config_store_local_binary(&format!("apptickets\\{}", app_id));
    if ticket.is_empty() {
        read_config_store_local_binary(&format!("apptickets/{}", app_id))
    } else {
        ticket
    }
}

/// Get seen app ticket IDs (for forge fallback).
pub fn get_seen_app_ticket_ids() -> HashSet<u32> {
    seen_app_ticket_ids().lock().unwrap_or_else(|e| e.into_inner()).clone()
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

/// Install Decryption hooks.
pub fn install(steam_root: &std::path::Path) -> Result<(), String> {
    log::info!("Installing InteropDecryption hooks...");

    if let Ok(addr) = resolve_function_address(
        steam_root,
        BinaryComponent::SteamClient,
        PatternChannel::Pattern,
        "ConfigStoreGetBinary",
    ) {
        let replacement = config_store_get_binary_detour as *const () as *mut u8;
        let patch = unsafe { memory_patch::patch_by_offset_with_callback(addr, 0, replacement, |trampoline| {
            let orig_fn: ConfigStoreGetBinaryFn = std::mem::transmute(trampoline as *const ());
            *original_config_store_get_binary().lock().unwrap_or_else(|e| e.into_inner()) = Some(orig_fn);
        }) }
            .map_err(|e| format!("Failed to install ConfigStoreGetBinary hook: {}", e))?;
        installed_decryption_patches()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(patch);
        log::info!("Installed ConfigStoreGetBinary hook at 0x{:x}", addr);
        eprintln!("[InteropDecryption] ConfigStoreGetBinary hook installed at runtime address 0x{:x}", addr);
    } else {
        log::warn!("ConfigStoreGetBinary pattern not found; ConfigStoreGetBinary hook NOT installed");
        eprintln!("[InteropDecryption] WARNING: ConfigStoreGetBinary pattern not found; ConfigStoreGetBinary hook NOT installed");
    }

    log::info!("InteropDecryption hooks installed");
    Ok(())
}

/// Uninstall Decryption hooks.
pub fn uninstall() -> Result<(), String> {
    *original_config_store_get_binary()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;

    let mut patches = installed_decryption_patches()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for mut patch in patches.drain(..) {
        unsafe { patch.uninstall() }?;
    }

    log::info!("InteropDecryption hooks uninstalled");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_store_types() {
        assert_eq!(EConfigStore::k_EConfigStoreUserLocal, 1);
    }

    #[test]
    fn test_decode_hex_robust() {
        // Standard 64-char hex string (32 bytes)
        let raw_hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let bytes = decode_hex_robust(raw_hex).unwrap();
        assert_eq!(bytes.len(), 32);

        // With 0x prefix and spaces
        let prefixed = "0x01 23 45 67 89 ab cd ef 01 23 45 67 89 ab cd ef 01 23 45 67 89 ab cd ef 01 23 45 67 89 ab cd ef";
        let bytes2 = decode_hex_robust(prefixed).unwrap();
        assert_eq!(bytes, bytes2);

        // Invalid hex
        assert!(decode_hex_robust("invalid").is_none());
        assert!(decode_hex_robust("123").is_none()); // Odd length
    }

    #[test]
    fn test_depot_key_path_normalization() {
        let test_paths = [
            "depots\\123456\\DecryptionKey",
            "depots/123456/DecryptionKey",
            "123456\\DecryptionKey",
            "123456/decryptionkey",
        ];
        for path in test_paths {
            let norm_name = path.replace('/', "\\");
            let lower_norm = norm_name.to_ascii_lowercase();
            assert!(lower_norm.contains("\\decryptionkey"));
            let last = lower_norm.rfind("\\decryptionkey").unwrap();
            let depot_id_str = match lower_norm[..last].rfind('\\') {
                Some(start) => &norm_name[start + 1..last],
                None => &norm_name[..last],
            };
            assert_eq!(depot_id_str, "123456");
        }
    }
}