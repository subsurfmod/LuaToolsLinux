//! NetPacket hook layer for Linux.
//!
//! Port of usefulsteamtools `Interop_NetPacket.cpp`.
//! Provides UserStats interception and spoofing, achievement handling,
//! CloudRedirect integration for NetPacket messages, and license handling.

pub mod netpacket_accesstoken;
pub mod netpacket_cloud;
pub mod netpacket_core;
pub mod netpacket_eticket;
pub mod netpacket_legacykey;
pub mod netpacket_licenses;
pub mod netpacket_manifest;
pub mod netpacket_manifestprobe;
pub mod netpacket_onlinefix;
pub mod netpacket_ownershipticket;
pub mod netpacket_richpresence;
pub mod userstats;

// Re-export key functions for easier access
pub use netpacket_accesstoken::{
    install_accesstoken_hooks, is_accesstoken_hook_active, uninstall_accesstoken_hooks,
};
pub use netpacket_cloud::{
    drain_cloud_responses, drain_into_carrier as drain_cloud_into_carrier,
    has_pending_responses as has_pending_cloud_responses, install_cloud_hooks,
    is_cloud_hook_active, uninstall_cloud_hooks,
};
pub use netpacket_core::{
    install_netpacket_core, is_netpacket_core_active, uninstall_netpacket_core,
    MsgHdr, KM_MSG_HDR_PROTO_FLAG, K_EMSG_SERVICE_METHOD_CALL_FROM_CLIENT, K_EMSG_SERVICE_METHOD_RESPONSE,
    NetPacketDirection, NetPacketContext, register_netpacket_handler,
};
pub use netpacket_eticket::{
    install_eticket_hooks, is_eticket_hook_active, uninstall_eticket_hooks,
};
pub use netpacket_legacykey::{
    install_legacykey_hooks, is_legacykey_hook_active, uninstall_legacykey_hooks,
    handle_send_legacy_game_key, drain_pending_responses,
    K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY, K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE,
};
pub use netpacket_licenses::{
    install_licenses_hooks, is_licenses_hook_active, uninstall_licenses_hooks, K_E_MSG_CLIENT_LICENSE_LIST,
};
pub use netpacket_manifest::{
    install_netpacket_manifest_hooks, uninstall_netpacket_manifest_hooks, is_netpacket_manifest_hook_active,
    handle_send_get_manifest_request_code, handle_recv_get_manifest_request_code_response,
    get_harvested_code, expire_manifest_stale,
};
pub use netpacket_manifestprobe::{
    install_manifestprobe_hooks, is_manifestprobe_hook_active, uninstall_manifestprobe_hooks,
};
pub use netpacket_onlinefix::{
    install_onlinefix_hooks, is_onlinefix_hook_active, uninstall_onlinefix_hooks,
};
pub use netpacket_ownershipticket::{
    install_ownershipticket_hooks, is_ownershipticket_hook_active, uninstall_ownershipticket_hooks,
};
pub use userstats::{install_userstats_hooks, is_userstats_hook_active, uninstall_userstats_hooks};
pub use netpacket_richpresence::{
    install_richpresence_hooks, is_richpresence_hook_active, uninstall_richpresence_hooks,
};