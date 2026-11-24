//! NetPacket hook layer for Linux.
//!
//! Port of usefulsteamtools `Interop_NetPacket.cpp`.
//! Provides UserStats interception and spoofing, achievement handling,
//! CloudRedirect integration for NetPacket messages.

pub mod userstats;
pub mod netpacket_eticket;
pub mod netpacket_ownershipticket;
pub mod netpacket_manifestprobe;

// Re-export key functions for easier access
pub use userstats::{
    install_userstats_hooks, uninstall_userstats_hooks, is_userstats_hook_active,
};
pub use netpacket_eticket::{
    install_eticket_hooks, uninstall_eticket_hooks, is_eticket_hook_active,
};
pub use netpacket_ownershipticket::{
    install_ownershipticket_hooks, uninstall_ownershipticket_hooks, is_ownershipticket_hook_active,
};
pub use netpacket_manifestprobe::{
    install_manifestprobe_hooks, uninstall_manifestprobe_hooks, is_manifestprobe_hook_active,
};