//! InteropCallbacks - SendCallbackToPipe hook for callback modification.
//!
//! Port of usefulsteamtools `Interop_Callbacks.cpp`.
//! Handles intercepting callbacks before they reach the Steam pipe.

use std::os::raw::c_void;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Mutex, OnceLock};

use crate::steam_bridge::memory_patch::{self, MemoryPatch, JMP_ABS64_LEN};
use crate::steam_bridge::pattern_db::{BinaryComponent, PatternChannel, PatternDb};

/// Steam pipe and user handles.
pub type HSteamPipe = u32;
pub type HSteamUser = u32;

/// Installed patches.
static INSTALLED_CALLBACKS_PATCHES: OnceLock<Mutex<Vec<MemoryPatch>>> = OnceLock::new();

/// Function pointer type for SendCallbackToPipe.
type SendCallbackToPipeFn = unsafe extern "C" fn(
    *mut c_void,   // pSteamEngine
    HSteamPipe,    // hSteamPipe
    HSteamUser,    // iClientUser
    i32,           // iCallback
    *mut c_void,   // pCallbackData
    i32,           // cubCallbackData
) -> bool;

/// Original function pointer.
static ORIGINAL_SEND_CALLBACK_TO_PIPE: OnceLock<Mutex<Option<SendCallbackToPipeFn>>> = OnceLock::new();

fn installed_callbacks_patches() -> &'static Mutex<Vec<MemoryPatch>> {
    INSTALLED_CALLBACKS_PATCHES.get_or_init(|| Mutex::new(Vec::new()))
}

fn original_send_callback_to_pipe() -> &'static Mutex<Option<SendCallbackToPipeFn>> {
    ORIGINAL_SEND_CALLBACK_TO_PIPE.get_or_init(|| Mutex::new(None))
}

/// Detour for SendCallbackToPipe.
unsafe extern "C" fn send_callback_to_pipe_detour(
    p_steam_engine: *mut c_void,
    h_steam_pipe: HSteamPipe,
    i_client_user: HSteamUser,
    i_callback: i32,
    p_callback_data: *mut c_void,
    cub_callback_data: i32,
) -> bool {
    panic::catch_unwind(AssertUnwindSafe(|| {
        // ── Callback modifier dispatch ─────────────────────────────────────────
        // Intercept callbacks before they reach the pipe and modify data in-place.
        // To add a new callback: add an else-if branch here.

        // For now, just pass through to the original function
        let orig = *original_send_callback_to_pipe()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(orig_fn) = orig {
            orig_fn(p_steam_engine, h_steam_pipe, i_client_user, i_callback, p_callback_data, cub_callback_data)
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

/// Install Callbacks hooks.
pub fn install(steam_root: &std::path::Path) -> Result<(), String> {
    log::info!("Installing InteropCallbacks hooks...");

    if let Ok(addr) = resolve_function_address(
        steam_root,
        BinaryComponent::SteamClient,
        PatternChannel::Pattern,
        "SendCallbackToPipe",
    ) {
        let replacement = send_callback_to_pipe_detour as *const () as *mut u8;
        let patch = unsafe { memory_patch::patch_by_offset(addr, 0, replacement) }
            .map_err(|e| format!("Failed to install SendCallbackToPipe hook: {}", e))?;

        if !patch.trampoline.is_null() {
            let orig_fn: SendCallbackToPipeFn = unsafe { std::mem::transmute(patch.trampoline) };
            *original_send_callback_to_pipe()
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(orig_fn);
        }
        installed_callbacks_patches()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(patch);
        log::info!("Installed SendCallbackToPipe hook at 0x{:x}", addr);
    }

    log::info!("InteropCallbacks hooks installed");
    Ok(())
}

/// Uninstall Callbacks hooks.
pub fn uninstall() -> Result<(), String> {
    *original_send_callback_to_pipe()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;

    let mut patches = installed_callbacks_patches()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for mut patch in patches.drain(..) {
        unsafe { patch.uninstall() }?;
    }

    log::info!("InteropCallbacks hooks uninstalled");
    Ok(())
}

#[cfg(test)]
mod tests {}