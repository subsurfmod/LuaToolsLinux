//! Steam client integration layer for Linux.
//!
//! This module provides the native Linux integration with the Steam client:
//! ELF binary reading, memory patching, preload loading, ticket storage,
//! pattern database lookup, inter-process communication, and manifest hooks.

pub mod binary_reader;
pub mod cloud_redirect;
pub mod denuvo_auth;
pub mod interop;
pub mod ipc;
pub mod manifest_hooks;
pub mod memory_patch;
pub mod ost_platform;
pub mod pattern_db;
pub mod preload_loader;
pub mod ticket_store;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static BRIDGE_STATE: OnceLock<BridgeState> = OnceLock::new();
static INTEGRATION_ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Default)]
pub struct BridgeState {
    pub steam_client_base: Option<u64>,
    pub steam_ui_base: Option<u64>,
    pub integration_active: bool,
}

/// Global bridge state accessor.
pub fn state() -> &'static BridgeState {
    BRIDGE_STATE.get_or_init(BridgeState::default)
}

/// Mark the Steam client as integrated.
pub fn set_integrated() {
    let _ = BRIDGE_STATE.get_or_init(|| BridgeState {
        integration_active: true,
        ..Default::default()
    });
    INTEGRATION_ACTIVE.store(true, Ordering::Release);
}

/// Check if integration is active.
pub fn is_integrated() -> bool {
    INTEGRATION_ACTIVE.load(Ordering::Acquire)
        || BRIDGE_STATE
            .get()
            .map(|s| s.integration_active)
            .unwrap_or(false)
}
