//! Steam client integration layer for Linux.
//!
//! This module provides the native Linux integration with the Steam client:
//! ELF binary reading, memory patching, preload loading, ticket storage,
//! pattern database lookup, inter-process communication, and manifest hooks.

pub mod binary_reader;
pub mod cloud_redirect;
pub mod config_file_watcher;
// Deprecated (Fix 8): Legacy Windows OpenSteamTool credentials module; superseded by native slsteam-moon hooks
pub mod credentials;
pub mod denuvo_auth;
pub mod extract_tickets;
pub mod injection;
// Deprecated (Fix 8): Legacy Windows OpenSteamTool 32-bit IPC wrappers; superseded by 64-bit steamrt64 slsteam-moon
pub mod interop;
pub mod ipc;
pub mod manifest_donor;
pub mod manifest_hooks;
pub mod memory_patch;
// Deprecated (Fix 8): Legacy Windows OpenSteamTool NetPacket hooks; superseded by slsteam-moon native NetPacket hooks
pub mod netpacket;
pub mod ost_config;
// Deprecated (Fix 8): Legacy Windows OpenSteamTool platform memory helpers; superseded by native Linux hooks
pub mod ost_platform;
pub mod pattern_db;
pub mod preload_loader;
pub mod ticket_store;
pub mod token_bridge;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;

static BRIDGE_STATE: OnceLock<BridgeState> = OnceLock::new();
static INTEGRATION_ACTIVE: AtomicBool = AtomicBool::new(false);
static STEAM_CLIENT_BASE_ADDR: AtomicU64 = AtomicU64::new(0);
static STEAM_UI_BASE_ADDR: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BridgeState {
    pub steam_client_base: Option<u64>,
    pub steam_ui_base: Option<u64>,
    pub integration_active: bool,
}

/// Global bridge state accessor (static initial snapshot).
pub fn state() -> &'static BridgeState {
    BRIDGE_STATE.get_or_init(|| BridgeState {
        steam_client_base: match STEAM_CLIENT_BASE_ADDR.load(Ordering::Acquire) {
            0 => None,
            v => Some(v),
        },
        steam_ui_base: match STEAM_UI_BASE_ADDR.load(Ordering::Acquire) {
            0 => None,
            v => Some(v),
        },
        integration_active: INTEGRATION_ACTIVE.load(Ordering::Acquire),
    })
}

/// Returns a live snapshot of bridge state, reflecting updates made even after `state()` was first read.
pub fn snapshot() -> BridgeState {
    let client_base = match STEAM_CLIENT_BASE_ADDR.load(Ordering::Acquire) {
        0 => BRIDGE_STATE.get().and_then(|s| s.steam_client_base),
        v => Some(v),
    };
    let ui_base = match STEAM_UI_BASE_ADDR.load(Ordering::Acquire) {
        0 => BRIDGE_STATE.get().and_then(|s| s.steam_ui_base),
        v => Some(v),
    };
    BridgeState {
        steam_client_base: client_base,
        steam_ui_base: ui_base,
        integration_active: is_integrated(),
    }
}

/// Record resolved runtime module bases for `steamclient.so` and `steamui.so`.
pub fn set_module_bases(steam_client_base: Option<u64>, steam_ui_base: Option<u64>) {
    STEAM_CLIENT_BASE_ADDR.store(steam_client_base.unwrap_or(0), Ordering::Release);
    STEAM_UI_BASE_ADDR.store(steam_ui_base.unwrap_or(0), Ordering::Release);
}

/// Mark the Steam client as integrated.
pub fn set_integrated() {
    INTEGRATION_ACTIVE.store(true, Ordering::Release);
    let _ = BRIDGE_STATE.get_or_init(|| BridgeState {
        steam_client_base: match STEAM_CLIENT_BASE_ADDR.load(Ordering::Acquire) {
            0 => None,
            v => Some(v),
        },
        steam_ui_base: match STEAM_UI_BASE_ADDR.load(Ordering::Acquire) {
            0 => None,
            v => Some(v),
        },
        integration_active: true,
    });
}

/// Clear the active integration flag.
pub fn clear_integrated() {
    INTEGRATION_ACTIVE.store(false, Ordering::Release);
}

/// Check if integration is active.
pub fn is_integrated() -> bool {
    INTEGRATION_ACTIVE.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static STATE_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn snapshot_reflects_live_integration_and_module_bases() {
        let _guard = STATE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_active = is_integrated();
        let prev_snap = snapshot();

        clear_integrated();
        set_module_bases(None, None);
        assert!(!is_integrated());
        let s0 = snapshot();
        assert!(!s0.integration_active);

        set_module_bases(Some(0x7f00_1000_0000), Some(0x7f00_2000_0000));
        set_integrated();
        assert!(is_integrated());
        let s1 = snapshot();
        assert!(s1.integration_active);
        assert_eq!(s1.steam_client_base, Some(0x7f00_1000_0000));
        assert_eq!(s1.steam_ui_base, Some(0x7f00_2000_0000));

        clear_integrated();
        assert!(!is_integrated());

        // Restore previous state
        set_module_bases(prev_snap.steam_client_base, prev_snap.steam_ui_base);
        if prev_active {
            set_integrated();
        }
    }
}
