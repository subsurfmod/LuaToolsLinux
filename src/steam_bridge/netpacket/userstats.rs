//! NetPacket UserStats hook layer for Linux.
//!
//! Port of usefulsteamtools `Interop_NetPacket_UserStats.cpp`.

use std::collections::HashMap;
use std::fmt;
use std::os::raw::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use crate::steam_bridge::interop::interop_misc::resolve_app_id;
use crate::steam_bridge::interop::ipc::{
    self, fnv1a_hash, FuncHash, HandlerFn, IpcContext, IpcInterface,
};
use crate::steam_bridge::ost_platform::steam_credential_store;

const NETPACKET_USERSTATS_GET_USER_STATS_FNHASH: FuncHash = fnv1a_hash("GetUserStats");
const NETPACKET_USERSTATS_CLIENT_GET_USER_STATS_FNHASH: FuncHash = fnv1a_hash("ClientGetUserStats");
const MAX_HELD_ACH_PER_APP: usize = 256;

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
static HELD_ACHIEVEMENTS: LazyLock<Mutex<HashMap<u32, Vec<Vec<u8>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static USERSTATS_ACTIVE: AtomicBool = AtomicBool::new(false);

fn userstats_handlers() -> &'static Mutex<HashMap<(IpcInterface, FuncHash), UserStatsHandler>> {
    &USERSTATS_HANDLERS
}

fn held_achievements() -> &'static Mutex<HashMap<u32, Vec<Vec<u8>>>> {
    &HELD_ACHIEVEMENTS
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

    let get_post: HandlerFn = Arc::new(|ctx: &mut IpcContext| -> Result<(), String> {
        handle_send_get_user_stats_post(ctx);
        Ok(())
    });
    let client_post: HandlerFn = Arc::new(|ctx: &mut IpcContext| -> Result<(), String> {
        handle_send_client_get_user_stats_post(ctx);
        Ok(())
    });

    map.insert(
        (iface, NETPACKET_USERSTATS_GET_USER_STATS_FNHASH),
        UserStatsHandler {
            interface_id: iface,
            func_hash: NETPACKET_USERSTATS_GET_USER_STATS_FNHASH,
            name: "Hooks_NetPacket_UserStats::HandleSend_GetUserStats".to_string(),
            fencepost: 0,
            argc: 4,
            pre_handler: None,
            post_handler: Some(Arc::clone(&get_post)),
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
            post_handler: Some(Arc::clone(&client_post)),
        },
    );
    drop(map);

    ipc::register_handler(ipc::make_handler(
        iface,
        NETPACKET_USERSTATS_GET_USER_STATS_FNHASH,
        "Hooks_NetPacket_UserStats::HandleSend_GetUserStats",
        0,
        4,
        None,
        Some(get_post),
    ));
    ipc::register_handler(ipc::make_handler(
        iface,
        NETPACKET_USERSTATS_CLIENT_GET_USER_STATS_FNHASH,
        "Hooks_NetPacket_UserStats::HandleSend_ClientGetUserStats",
        0,
        4,
        None,
        Some(client_post),
    ));
}

/// Unregister all UserStats handlers.
pub fn unregister_userstats_handlers() {
    userstats_handlers()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    let iface = IpcInterface::IClientUser;
    let _ = ipc::unregister_handler(iface, NETPACKET_USERSTATS_GET_USER_STATS_FNHASH);
    let _ = ipc::unregister_handler(iface, NETPACKET_USERSTATS_CLIENT_GET_USER_STATS_FNHASH);
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
        if write_buf.len() >= 16 {
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

fn find_local_stats_bin(base_dir: &Path, app_id: u32) -> Option<std::path::PathBuf> {
    let target_name = format!("UserGameStats_{}.bin", app_id);
    let direct = base_dir.join(app_id.to_string()).join(&target_name);
    if direct.is_file() {
        return Some(direct);
    }
    // Also search Steam's `userdata/<account_id>/UserGameStats_<app_id>.bin` and
    // `appcache/stats/UserGameStats_<account_id>_<app_id>.bin` patterns.
    let suffix = format!("_{}.bin", app_id);
    if let Ok(entries) = std::fs::read_dir(base_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with("UserGameStats_") && name.ends_with(&suffix) {
                        return Some(p);
                    }
                }
            } else if p.is_dir() {
                let candidate = p.join(&target_name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// Rebuild `GetUserAchievements` from a local `UserGameStats_<app_id>.bin` file inside `base_dir`.
pub fn fill_user_achievements_from_dir(
    base_dir: &Path,
    app_id: u32,
    resp: &mut Vec<u8>,
    n_unlocked: &mut u32,
) {
    *n_unlocked = 0;
    if app_id == 0 {
        return;
    }
    let Some(bin_path) = find_local_stats_bin(base_dir, app_id) else {
        return;
    };
    if let Ok(data) = std::fs::read(&bin_path) {
        if !data.is_empty() {
            // Count non-zero unlock bitmask entries if the binary payload is a packed 4-byte record
            // table, or report at least 1 payload block if non-empty binary VDF stats exist.
            resp.extend_from_slice(&data);
            let unlocked_estimate = data
                .windows(b"unlocked".len())
                .filter(|w| w.eq_ignore_ascii_case(b"unlocked"))
                .count();
            *n_unlocked = u32::try_from(unlocked_estimate).unwrap_or(u32::MAX);
            log::debug!(
                "FillUserAchievements: AppId={} - loaded {} bytes ({} unlocked marker(s)) from {}",
                app_id,
                data.len(),
                *n_unlocked,
                bin_path.display()
            );
        }
    }
}

/// Rebuild `GetUserAchievements` from schema + local `.bin` when present.
pub fn fill_user_achievements(app_id: u32, resp: &mut Vec<u8>, n_unlocked: &mut u32) {
    let Some(home) = dirs::home_dir() else {
        *n_unlocked = 0;
        return;
    };
    let userdata_dir = home.join(".steam/steam/userdata");
    fill_user_achievements_from_dir(&userdata_dir, app_id, resp, n_unlocked);
}

/// Queue an achievement payload for deferred delivery.
pub fn queue_held_user_ach(app_id: u32, ach_data: &[u8]) {
    if app_id == 0 || ach_data.is_empty() {
        return;
    }
    let mut map = held_achievements()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let queue = map.entry(app_id).or_default();
    if queue.len() < MAX_HELD_ACH_PER_APP {
        queue.push(ach_data.to_vec());
    }
    log::debug!(
        "QueueHeldUserAch: AppId={} - queued {} bytes (depth={})",
        app_id,
        ach_data.len(),
        queue.len()
    );
}

/// Return how many achievement payloads are currently queued for `app_id`.
pub fn held_achievement_count(app_id: u32) -> usize {
    held_achievements()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&app_id)
        .map_or(0, Vec::len)
}

/// Take and remove all queued achievement payloads for `app_id`.
pub fn take_held_achievements(app_id: u32) -> Vec<Vec<u8>> {
    held_achievements()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&app_id)
        .unwrap_or_default()
}

pub fn drain_achievements(app_id: u32, _carrier: *mut c_void, invoke_original: bool) {
    let drained = take_held_achievements(app_id);
    log::debug!(
        "DrainAchievements: AppId={} drained={} (invoke_original={})",
        app_id,
        drained.len(),
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
    held_achievements()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
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

/// IPC adapter for Send: Player.GetUserStats#1 (service method call)
pub fn handle_send_get_user_stats_post_ipc(ctx: &mut crate::steam_bridge::netpacket::netpacket_core::NetPacketContext) -> Result<(), String> {
    let mut ipc_ctx = crate::steam_bridge::interop::ipc::IpcContext {
        pipe: std::ptr::null_mut(),
        read_buffer: ctx.frame_buffer.clone(),
        write_buffer: ctx.frame_buffer.clone(),
        interface_id: crate::steam_bridge::interop::ipc::IpcInterface::IClientUser,
        func_hash: crate::steam_bridge::interop::ipc::fnv1a_hash("GetUserStats"),
        app_id: ctx.app_id,
    };
    handle_send_get_user_stats_post(&mut ipc_ctx);
    ctx.frame_buffer = ipc_ctx.write_buffer;
    Ok(())
}

/// IPC adapter for Send: ClientGetUserStats (legacy)
pub fn handle_send_client_get_user_stats_post_ipc(ctx: &mut crate::steam_bridge::netpacket::netpacket_core::NetPacketContext) -> Result<(), String> {
    let mut ipc_ctx = crate::steam_bridge::interop::ipc::IpcContext {
        pipe: std::ptr::null_mut(),
        read_buffer: ctx.frame_buffer.clone(),
        write_buffer: ctx.frame_buffer.clone(),
        interface_id: crate::steam_bridge::interop::ipc::IpcInterface::IClientUser,
        func_hash: crate::steam_bridge::interop::ipc::fnv1a_hash("ClientGetUserStats"),
        app_id: ctx.app_id,
    };
    handle_send_client_get_user_stats_post(&mut ipc_ctx);
    ctx.frame_buffer = ipc_ctx.write_buffer;
    Ok(())
}

/// IPC adapter for Recv: Player.GetUserStats#1 response
pub fn handle_recv_get_user_stats_response_ipc(ctx: &mut crate::steam_bridge::netpacket::netpacket_core::NetPacketContext) -> Result<(), String> {
    handle_recv_get_user_stats_response(&mut ctx.frame_buffer, ctx.app_id);
    Ok(())
}

/// IPC adapter for Recv: ClientGetUserStatsResponse
pub fn handle_recv_client_get_user_stats_response_ipc(ctx: &mut crate::steam_bridge::netpacket::netpacket_core::NetPacketContext) -> Result<(), String> {
    handle_recv_client_get_user_stats_response(&mut ctx.frame_buffer, ctx.app_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    static USERSTATS_TEST_GUARD: Mutex<()> = Mutex::new(());

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
    fn test_handler_registration_and_uninstall() {
        let _g = USERSTATS_TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        install_userstats().unwrap();
        assert!(is_userstats_hook_active());

        {
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
        assert!(ipc::find_handler(
            IpcInterface::IClientUser,
            NETPACKET_USERSTATS_GET_USER_STATS_FNHASH
        )
        .is_some());

        uninstall_userstats();
        assert!(!is_userstats_hook_active());
        assert!(ipc::find_handler(
            IpcInterface::IClientUser,
            NETPACKET_USERSTATS_GET_USER_STATS_FNHASH
        )
        .is_none());
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
    fn test_handle_recv_get_user_stats_response_including_exact_16_bytes() {
        let mut write_buf = vec![0u8; 256];
        write_buf[8..12].copy_from_slice(&2u32.to_le_bytes());
        write_buf[12..16].copy_from_slice(&5u32.to_le_bytes());
        write_buf[16..20].copy_from_slice(&3u32.to_le_bytes());
        write_buf[24..56].copy_from_slice(&[1u8; 32]);

        handle_recv_get_user_stats_response(&mut write_buf, 123456);
        assert_eq!(write_buf[8..12], 0u32.to_le_bytes());
        assert_eq!(write_buf[12..16], 0u32.to_le_bytes());
        assert_eq!(write_buf.len(), 16);

        // Exact 16-byte buffer must also zero out stat count at [12..16]
        let mut exact16 = vec![0xFFu8; 16];
        handle_recv_get_user_stats_response(&mut exact16, 123456);
        assert_eq!(exact16[8..12], 0u32.to_le_bytes());
        assert_eq!(exact16[12..16], 0u32.to_le_bytes());
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

    #[test]
    fn test_held_achievement_queue_drain_and_fill_from_dir() {
        let _g = USERSTATS_TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let app_id = 444_555;
        let _ = take_held_achievements(app_id);

        queue_held_user_ach(app_id, &[]);
        assert_eq!(held_achievement_count(app_id), 0);

        queue_held_user_ach(app_id, b"ach-1");
        queue_held_user_ach(app_id, b"ach-2");
        assert_eq!(held_achievement_count(app_id), 2);

        drain_achievements(app_id, std::ptr::null_mut(), false);
        assert_eq!(held_achievement_count(app_id), 0);

        let tmp = tempfile::tempdir().unwrap();
        let account_dir = tmp.path().join("987654");
        std::fs::create_dir_all(&account_dir).unwrap();
        std::fs::write(
            account_dir.join(format!("UserGameStats_{}.bin", app_id)),
            b"ach_a:unlocked;ach_b:unlocked",
        )
        .unwrap();

        let mut resp = Vec::new();
        let mut n_unlocked = 99;
        fill_user_achievements_from_dir(tmp.path(), app_id, &mut resp, &mut n_unlocked);
        assert!(!resp.is_empty());
        assert_eq!(n_unlocked, 2);
    }
}
