//! NetPacket hook layer for Linux.
//!
//! Port of usefulsteamtools `Interop_NetPacket.cpp`.
//! Provides UserStats interception and spoofing, achievement handling, and
//! CloudRedirect integration for NetPacket messages.

pub mod userstats;
pub mod netpacket_eticket;

/// Install all NetPacket hooks
pub async fn install_all(steam_root: &std::path::Path) -> Result<(), String> {
    crate::steam_bridge::netpacket::userstats::install_userstats()?;
    Ok(())
}

/// Uninstall all NetPacket hooks
pub fn uninstall_all() {
    crate::steam_bridge::netpacket::userstats::uninstall_userstats();
}