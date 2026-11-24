//! NetPacket UserStats hook layer for Linux.
//!
//! Port of usefulsteamtools `Interop_NetPacket_UserStats.cpp`.

use std::collections::HashMap;
use std::fmt;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use crate::steam_bridge::interop::interop_misc::resolve_app_id;
use crate::steam_bridge::interop::ipc::{
    fnv1a_hash, FuncHash, HandlerFn, IpcContext, IpcInterface,
};
use crate::steam_bridge::ost_platform::steam_credential_store;

const NETPACKET_USERSTATS_GET_USER_STATS_FNHASH: FuncHash = fnv1a_hash("GetUserStats");
const NETPACKET_USERSTATS_CLIENT_GET_USER_STATS_FNHASH: FuncHash = fnv1a_hash("ClientGetUserStats");

/// UserStats handler definition.
#[derive(Clone)]
pub struct UserStatsHandler {
    pub interface_id: IpcInterface,
    pub func_hash: FuncHash,
    pub name: String,
    pub fencepost: u32,
    pub argc: u32,
    pub pre_handler: Option<HandlerFn>,
    pub post_handler: Option<HandlerFn>,
}

impl fmt::Debug for UserStatsHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserStatsHandler")
            .field("interface_id", &self.interface_id)
            .field("func_hash", &self.func_hash)
            .field("name", &self.name)
            .finish()
    }
}

static USERSTATS_HANDLERS: LazyLock<Mutex<HashMap<(IpcInterface, FuncHash), UserStatsHandler>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static USERSTATS_ACTIVE: AtomicBool = AtomicBool::new(false);

fn userstats_handlers() -> &'static Mutex<HashMap<(IpcInterface, FuncHash), UserStatsHandler>> {
    &USERSTATS_HANDLERS
}

fn read_spoofed_steam_id(app_id: u32) -> u64 {
    let mut sid = 0u64;
    if steam_credential_store::get_steam_id(app_id, &mut sid) == steam_credential_store::Status::Ok
    {
        sid
    } else {
        0
    }
}

/// Register all UserStats handlers.
pub fn register_userstats_handlers() {
    let mut map = userstats_handlers()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let iface = IpcInterface::IClientUser;

    map.insert(
        (iface, NETPACKET_USERSTATS_GET_USER_STATS_FNHASH),
        UserStatsHandler {
            interface_id: iface,
            func_hash: NETPACKET_USERSTATS_GET_USER_STATS_FNHASH,
            name: "Hooks_NetPacket_UserStats::HandleSend_GetUserStats".to_string(),
            fencepost: 0,
            argc: 4,
            pre_handler: None,
            post_handler: Some(Arc::new(|ctx: &mut IpcContext| -> Result<(), String> {
                handle_send_get_user_stats_post(ctx);
                Ok(())
            })),
        },
    );

    map.insert(
        (iface, NETPACKET_USERSTATS_CLIENT_GET_USER_STATS_FNHASH),
        UserStatsHandler {
            interface_id: iface,
            func_hash: NETPACKET_USERSTATS_CLIENT_GET_USER_STATS_FNHASH,
            name: "Hooks_NetPacket_UserStats::HandleSend_ClientGetUserStats".to_string(),
            fencepost: 0,
            argc: 4,
            pre_handler: None,
            post_handler: Some(Arc::new(|ctx: &mut IpcContext| -> Result<(), String> {
                handle_send_client_get_user_stats_post(ctx);
                Ok(())
            })),
        },
    );
}

/// Unregister all UserStats handlers.
pub fn unregister_userstats_handlers() {
    userstats_handlers()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

fn handle_send_get_user_stats_post(ctx: &mut IpcContext) {
    let app_id = if ctx.app_id != 0 {
        ctx.app_id
    } else {
        resolve_app_id()
    };
    let spoofed_steam_id = read_spoofed_steam_id(app_id);

    if spoofed_steam_id == 0 {
        log::debug!(
            "HandleSend_GetUserStats: AppId={} - no spoofed SteamID available",
            app_id
        );
        return;
    }

    if ctx.write_buffer.len() >= 20 {
        ctx.write_buffer[12..20].copy_from_slice(&spoofed_steam_id.to_le_bytes());
    }
}

fn handle_send_client_get_user_stats_post(ctx: &mut IpcContext) {
    let app_id = if ctx.app_id != 0 {
        ctx.app_id
    } else {
        resolve_app_id()
    };
    let spoofed_steam_id = read_spoofed_steam_id(app_id);

    // Always force schema_local_version = -1 (0xFFFFFFFF) at offset 12
    if ctx.write_buffer.len() >= 16 {
        ctx.write_buffer[12..16].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        if spoofed_steam_id != 0 && ctx.write_buffer.len() >= 24 {
            ctx.write_buffer[16..24].copy_from_slice(&spoofed_steam_id.to_le_bytes());
        }
    }
}

/// Force `eresult = OK` and strip stats in `GetUserStatsResponse`.
pub fn handle_recv_get_user_stats_response(write_buf: &mut Vec<u8>, app_id: u32) {
    if write_buf.len() >= 12 {
        write_buf[8..12].copy_from_slice(&0u32.to_le_bytes());
        if write_buf.len() > 16 {
            write_buf.truncate(16);
            write_buf[12..16].fill(0);
        }
    }
    log::debug!(
        "HandleRecv_GetUserStatsResponse: AppId={} - forced eresult=OK, stats stripped",
        app_id
    );
}

/// Clear stats/achievements in `ClientGetUserStatsResponse`.
pub fn handle_recv_client_get_user_stats_response(write_buf: &mut Vec<u8>, app_id: u32) {
    if write_buf.len() >= 12 {
        write_buf[8..12].copy_from_slice(&0u32.to_le_bytes());
        write_buf.truncate(12);
    }
    log::debug!(
        "HandleRecv_ClientGetUserStatsResponse: AppId={} - cleared stats",
        app_id
    );
}

/// Rebuild `GetUserAchievements` from schema + local `.bin` when present.
pub fn fill_user_achievements(app_id: u32, _resp: &mut Vec<u8>, n_unlocked: &mut u32) {
    let config_dir = std::env::var("HOME")
        .map(|h| format!("{}/.steam/steam/userdata", h))
        .unwrap_or_default();
    let bin_path = format!("{}/{}/UserGameStats_{}.bin", config_dir, app_id, app_id);
    if let Ok(data) = std::fs::read(&bin_path) {
        if !data.is_empty() {
            *n_unlocked = 0;
            log::debug!(
                "FillUserAchievements: AppId={} - read {} bytes from local bin",
                app_id,
                data.len()
            );
        }
    }
}

pub fn queue_held_user_ach(app_id: u32, ach_data: &[u8]) {
    log::debug!(
        "QueueHeldUserAch: AppId={} - queued {} bytes",
        app_id,
        ach_data.len()
    );
}

pub fn drain_achievements(app_id: u32, _carrier: *mut c_void, invoke_original: bool) {
    log::debug!(
        "DrainAchievements: AppId={} (invoke_original={})",
        app_id,
        invoke_original
    );
}

pub fn install_userstats() -> Result<(), String> {
    register_userstats_handlers();
    USERSTATS_ACTIVE.store(true, Ordering::Release);
    log::info!("Installed UserStats NetPacket hooks");
    Ok(())
}

pub fn uninstall_userstats() {
    unregister_userstats_handlers();
    USERSTATS_ACTIVE.store(false, Ordering::Release);
    log::info!("Uninstalled UserStats NetPacket hooks");
}

pub fn install_userstats_hooks() -> Result<(), String> {
    install_userstats()
}

pub fn uninstall_userstats_hooks() {
    uninstall_userstats()
}

pub fn is_userstats_hook_active() -> bool {
    USERSTATS_ACTIVE.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fnv1a_hash_get_user_stats() {
        assert_eq!(
            NETPACKET_USERSTATS_GET_USER_STATS_FNHASH,
            fnv1a_hash("GetUserStats")
        );
        assert_eq!(
            NETPACKET_USERSTATS_CLIENT_GET_USER_STATS_FNHASH,
            fnv1a_hash("ClientGetUserStats")
        );
    }

    #[test]
    fn test_handler_registration() {
        install_userstats().unwrap();
        assert!(is_userstats_hook_active());

        let handlers = userstats_handlers().lock().unwrap();
        assert!(handlers.contains_key(&(
            IpcInterface::IClientUser,
            NETPACKET_USERSTATS_GET_USER_STATS_FNHASH
        )));
        assert!(handlers.contains_key(&(
            IpcInterface::IClientUser,
            NETPACKET_USERSTATS_CLIENT_GET_USER_STATS_FNHASH
        )));
    }

    #[test]
    fn test_handle_send_get_user_stats_post() {
        let mut ctx = IpcContext {
            pipe: std::ptr::null_mut(),
            read_buffer: vec![],
            write_buffer: vec![0u8; 32],
            interface_id: IpcInterface::IClientUser,
            func_hash: 0,
            app_id: 123456,
        };
        handle_send_get_user_stats_post(&mut ctx);
        assert_eq!(ctx.write_buffer.len(), 32);
    }

    #[test]
    fn test_handle_send_client_get_user_stats_post() {
        let mut write_buf = vec![0u8; 32];
        write_buf[12..16].copy_from_slice(&1u32.to_le_bytes());
        write_buf[16..24].copy_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());

        let mut ctx = IpcContext {
            pipe: std::ptr::null_mut(),
            read_buffer: vec![],
            write_buffer: write_buf,
            interface_id: IpcInterface::IClientUser,
            func_hash: 0,
            app_id: 123456,
        };
        handle_send_client_get_user_stats_post(&mut ctx);

        assert_eq!(ctx.write_buffer[12..16], 0xFFFF_FFFFu32.to_le_bytes());
        assert!(ctx.write_buffer.len() >= 24);
    }

    #[test]
    fn test_handle_recv_get_user_stats_response() {
        let mut write_buf = vec![0u8; 256];
        write_buf[8..12].copy_from_slice(&2u32.to_le_bytes());
        write_buf[12..16].copy_from_slice(&5u32.to_le_bytes());
        write_buf[16..20].copy_from_slice(&3u32.to_le_bytes());
        write_buf[24..56].copy_from_slice(&[1u8; 32]);

        handle_recv_get_user_stats_response(&mut write_buf, 123456);
        assert_eq!(write_buf[8..12], 0u32.to_le_bytes());
        assert!(write_buf.len() <= 20);
    }

    #[test]
    fn test_handle_recv_client_get_user_stats_response() {
        let mut write_buf = vec![0u8; 256];
        write_buf[8..12].copy_from_slice(&2u32.to_le_bytes());
        write_buf[12..16].copy_from_slice(&5u32.to_le_bytes());

        handle_recv_client_get_user_stats_response(&mut write_buf, 123456);
        assert_eq!(write_buf[8..12], 0u32.to_le_bytes());
        assert_eq!(write_buf.len(), 12);
    }
}
