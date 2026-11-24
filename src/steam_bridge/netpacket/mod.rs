//! NetPacket hook layer for Linux.
//!
//! Port of usefulsteamtools `Interop_NetPacket.cpp`.
//! Provides UserStats interception and spoofing, achievement handling,
//! CloudRedirect integration for NetPacket messages.

pub mod netpacket_eticket;
pub mod netpacket_manifestprobe;
pub mod netpacket_ownershipticket;
pub mod userstats;

// Re-export key functions for easier access
pub use netpacket_eticket::{
    install_eticket_hooks, is_eticket_hook_active, uninstall_eticket_hooks,
};
pub use netpacket_manifestprobe::{
    install_manifestprobe_hooks, is_manifestprobe_hook_active, uninstall_manifestprobe_hooks,
};
pub use netpacket_ownershipticket::{
    install_ownershipticket_hooks, is_ownershipticket_hook_active, uninstall_ownershipticket_hooks,
};
pub use userstats::{install_userstats_hooks, is_userstats_hook_active, uninstall_userstats_hooks};
