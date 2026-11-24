//! Steam client integration layer for Linux.
//!
//! This module provides the native Linux integration with the Steam client:
//! ELF binary reading, memory patching, preload loading, ticket storage,
//! pattern database lookup, inter-process communication, and manifest hooks.

pub mod binary_reader;
pub mod interop;
pub mod memory_patch;
pub mod pattern_db;
pub mod preload_loader;
pub mod ticket_store;
pub mod ost_platform;
pub mod ipc;
pub mod manifest_hooks;
pub mod cloud_redirect;
pub mod denuvo_auth;
pub mod netpacket;
pub mod credentials;