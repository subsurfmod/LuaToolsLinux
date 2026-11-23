//! Steam client integration layer for Linux.
//!
//! This module provides the native Linux integration with the Steam client:
//! ELF binary reading, memory patching, preload loading, ticket storage,
//! pattern database lookup, and inter-process communication.

pub mod binary_reader;
pub mod ipc;
pub mod memory_patch;
pub mod preload_loader;
pub mod ticket_store;
pub mod pattern_db;

use std::sync::OnceLock;

static BRIDGE_STATE: OnceLock<BridgeState> = OnceLock::new();

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
    let _ = BRIDGE_STATE.get();
}

/// Check if integration is active.
pub fn is_integrated() -> bool {
    BRIDGE_STATE.get().map(|s| s.integration_active).unwrap_or(false)
}
