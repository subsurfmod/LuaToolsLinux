//! InteropCallbacks - SendCallbackToPipe hook for callback modification (BST-30).
//!
//! Port of UsefulSteamTools / BetterSteamTools `Hooks_CallBack.cpp`.
//! Handles intercepting callbacks before they reach client IPC pipes and modifying or monitoring data in-place.

use std::collections::HashMap;
use std::os::raw::c_void;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Mutex, OnceLock};

use crate::steam_bridge::memory_patch::{self, MemoryPatch, JMP_ABS64_LEN};
use crate::steam_bridge::pattern_db::{BinaryComponent, PatternChannel, PatternDb};

/// Steam pipe and user handles.
pub type HSteamPipe = u32;
pub type HSteamUser = u32;

/// Installed patches.
static INSTALLED_CALLBACKS_PATCHES: OnceLock<Mutex<Vec<MemoryPatch>>> = OnceLock::new();

/// Function pointer type for SendCallbackToPipe.
pub type SendCallbackToPipeFn = unsafe extern "C" fn(
    *mut c_void,   // pSteamEngine
    HSteamPipe,    // hSteamPipe
    HSteamUser,    // iClientUser
    i32,           // iCallback
    *mut c_void,   // pCallbackData
    i32,           // cubCallbackData
) -> bool;

/// Original function pointer.
static ORIGINAL_SEND_CALLBACK_TO_PIPE: OnceLock<Mutex<Option<SendCallbackToPipeFn>>> = OnceLock::new();

/// Action returned by a callback modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackAction {
    /// Pass through to original SendCallbackToPipe
    PassThrough,
    /// Callback data was modified in-place; forward to pipe
    Modified,
    /// Suppress callback delivery to pipe
    Suppress,
}

/// Callback modifier closure signature.
pub type CallbackModifier = Arc<dyn Fn(HSteamPipe, HSteamUser, i32, *mut c_void, i32) -> CallbackAction + Send + Sync>;

/// Registered callback modifiers keyed by callback ID (0 = wildcard / all callbacks).
static CALLBACK_MODIFIERS: OnceLock<Mutex<HashMap<i32, Vec<CallbackModifier>>>> = OnceLock::new();

/// Callback invocation history log for audit and test assertions (pipe, user, callback_id, data_size).
static CALLBACK_HISTORY: OnceLock<Mutex<Vec<(HSteamPipe, HSteamUser, i32, i32)>>> = OnceLock::new();

fn installed_callbacks_patches() -> &'static Mutex<Vec<MemoryPatch>> {
    INSTALLED_CALLBACKS_PATCHES.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn original_send_callback_to_pipe() -> &'static Mutex<Option<SendCallbackToPipeFn>> {
    ORIGINAL_SEND_CALLBACK_TO_PIPE.get_or_init(|| Mutex::new(None))
}

fn callback_modifiers() -> &'static Mutex<HashMap<i32, Vec<CallbackModifier>>> {
    CALLBACK_MODIFIERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn callback_history() -> &'static Mutex<Vec<(HSteamPipe, HSteamUser, i32, i32)>> {
    CALLBACK_HISTORY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register a callback modifier/interceptor for `callback_id` (use 0 for wildcard/all callbacks).
pub fn register_callback_modifier<F>(callback_id: i32, modifier: F)
where
    F: Fn(HSteamPipe, HSteamUser, i32, *mut c_void, i32) -> CallbackAction + Send + Sync + 'static,
{
    let mut table = callback_modifiers().lock().unwrap_or_else(|e| e.into_inner());
    table.entry(callback_id).or_default().push(Arc::new(modifier));
}

/// Clear all registered callback modifiers.
pub fn clear_callback_modifiers() {
    let mut table = callback_modifiers().lock().unwrap_or_else(|e| e.into_inner());
    table.clear();
}

/// Get a snapshot of all dispatched callbacks recorded by the hook.
pub fn get_callback_history() -> Vec<(HSteamPipe, HSteamUser, i32, i32)> {
    callback_history().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Clear recorded callback history.
pub fn clear_callback_history() {
    callback_history().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Detour for SendCallbackToPipe.
pub unsafe extern "C" fn send_callback_to_pipe_detour(
    p_steam_engine: *mut c_void,
    h_steam_pipe: HSteamPipe,
    i_client_user: HSteamUser,
    i_callback: i32,
    p_callback_data: *mut c_void,
    cub_callback_data: i32,
) -> bool {
    panic::catch_unwind(AssertUnwindSafe(|| {
        // 1. Record in history
        {
            let mut hist = callback_history().lock().unwrap_or_else(|e| e.into_inner());
            if hist.len() > 1000 {
                hist.remove(0);
            }
            hist.push((h_steam_pipe, i_client_user, i_callback, cub_callback_data));
        }

        // 2. Dispatch to registered callback modifiers
        let mut suppress = false;
        let (specific_modifiers, wildcard_modifiers) = {
            let table = callback_modifiers().lock().unwrap_or_else(|e| e.into_inner());
            (
                table.get(&i_callback).cloned().unwrap_or_default(),
                table.get(&0).cloned().unwrap_or_default(),
            )
        };
        for modifier in specific_modifiers {
            match modifier(h_steam_pipe, i_client_user, i_callback, p_callback_data, cub_callback_data) {
                CallbackAction::Suppress => {
                    suppress = true;
                    break;
                }
                CallbackAction::Modified | CallbackAction::PassThrough => {}
            }
        }
        if !suppress {
            for modifier in wildcard_modifiers {
                match modifier(h_steam_pipe, i_client_user, i_callback, p_callback_data, cub_callback_data) {
                    CallbackAction::Suppress => {
                        suppress = true;
                        break;
                    }
                    CallbackAction::Modified | CallbackAction::PassThrough => {}
                }
            }
        }

        if suppress {
            log::debug!("[interop_callbacks] Suppressed callback {} for pipe {}", i_callback, h_steam_pipe);
            return true;
        }

        // 3. Forward to original function if present
        let orig = *original_send_callback_to_pipe()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(orig_fn) = orig {
            orig_fn(
                p_steam_engine,
                h_steam_pipe,
                i_client_user,
                i_callback,
                p_callback_data,
                cub_callback_data,
            )
        } else {
            // When unhooked/simulated, default to successful delivery
            true
        }
    }))
    .unwrap_or(false)
}

/// Helper to simulate or manually invoke SendCallbackToPipe dispatch.
pub unsafe fn dispatch_callback(
    p_steam_engine: *mut c_void,
    h_steam_pipe: HSteamPipe,
    i_client_user: HSteamUser,
    i_callback: i32,
    p_callback_data: *mut c_void,
    cub_callback_data: i32,
) -> bool {
    send_callback_to_pipe_detour(
        p_steam_engine,
        h_steam_pipe,
        i_client_user,
        i_callback,
        p_callback_data,
        cub_callback_data,
    )
}

/// Resolve function address using dynamic symbol export or pattern scanning.
fn resolve_function_address(
    steam_root: &std::path::Path,
    component: BinaryComponent,
    channel: PatternChannel,
    pattern_name: &str,
) -> Result<u64, String> {
    // 1. Try dynamic dlsym resolution first
    if let Ok(sym) = std::ffi::CString::new(pattern_name) {
        let addr = unsafe { libc::dlsym(libc::RTLD_DEFAULT, sym.as_ptr()) };
        if !addr.is_null() {
            return Ok(addr as u64);
        }
    }

    // 2. Fall back to pattern scanning
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

/// Install Callbacks hooks.
pub fn install(steam_root: &std::path::Path) -> Result<(), String> {
    log::info!("Installing InteropCallbacks hooks (BST-30)...");

    if let Ok(addr) = resolve_function_address(
        steam_root,
        BinaryComponent::SteamClient,
        PatternChannel::Pattern,
        "SendCallbackToPipe",
    ) {
        let replacement = send_callback_to_pipe_detour as *const () as *mut u8;
        let patch = unsafe { memory_patch::patch_by_offset_with_callback(addr, 0, replacement, |trampoline| {
            let orig_fn: SendCallbackToPipeFn = unsafe { std::mem::transmute(trampoline as *const ()) };
            *original_send_callback_to_pipe().lock().unwrap_or_else(|e| e.into_inner()) = Some(orig_fn);
        }) }
            .map_err(|e| format!("Failed to install SendCallbackToPipe hook: {}", e))?;
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
mod tests {
    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[repr(C)]
    struct MockAppTicketResponse {
        pub app_id: u32,
        pub result: i32,
    }

    #[test]
    fn test_callback_passthrough_and_history() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_callback_modifiers();
        clear_callback_history();

        let mut data = MockAppTicketResponse { app_id: 480, result: 1 };
        let ok = unsafe {
            dispatch_callback(
                std::ptr::null_mut(),
                100,
                200,
                154, // EncryptedAppTicketResponse_t
                (&mut data as *mut MockAppTicketResponse).cast(),
                std::mem::size_of::<MockAppTicketResponse>() as i32,
            )
        };
        assert!(ok);

        let hist = get_callback_history();
        assert!(hist.iter().any(|entry| entry == &(100, 200, 154, std::mem::size_of::<MockAppTicketResponse>() as i32)));

        clear_callback_history();
    }

    #[test]
    fn test_callback_modifier_in_place_modification() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_callback_modifiers();
        clear_callback_history();

        // Register modifier that rewrites app_id from 480 to 730
        register_callback_modifier(154, |_pipe, _user, _cb, ptr, _len| {
            if !ptr.is_null() {
                let resp = unsafe { &mut *ptr.cast::<MockAppTicketResponse>() };
                resp.app_id = 730;
                resp.result = 0; // Success
                CallbackAction::Modified
            } else {
                CallbackAction::PassThrough
            }
        });

        let mut data = MockAppTicketResponse { app_id: 480, result: 1 };
        let ok = unsafe {
            dispatch_callback(
                std::ptr::null_mut(),
                1,
                2,
                154,
                (&mut data as *mut MockAppTicketResponse).cast(),
                std::mem::size_of::<MockAppTicketResponse>() as i32,
            )
        };
        assert!(ok);
        assert_eq!(data.app_id, 730);
        assert_eq!(data.result, 0);

        clear_callback_modifiers();
    }

    #[test]
    fn test_callback_suppression() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_callback_modifiers();
        clear_callback_history();

        // Register modifier that suppresses callback 999
        register_callback_modifier(999, |_pipe, _user, _cb, _ptr, _len| {
            CallbackAction::Suppress
        });

        let mut data = 42u32;
        let ok = unsafe {
            dispatch_callback(
                std::ptr::null_mut(),
                10,
                20,
                999,
                (&mut data as *mut u32).cast(),
                4,
            )
        };
        assert!(ok); // Handled

        // Unregistered callback 1000 passes through unmodified
        let ok2 = unsafe {
            dispatch_callback(
                std::ptr::null_mut(),
                10,
                20,
                1000,
                (&mut data as *mut u32).cast(),
                4,
            )
        };
        assert!(ok2);

        let hist = get_callback_history();
        assert!(hist.iter().any(|entry| entry.2 == 999));
        assert!(hist.iter().any(|entry| entry.2 == 1000));

        clear_callback_modifiers();
        clear_callback_history();
    }
}
