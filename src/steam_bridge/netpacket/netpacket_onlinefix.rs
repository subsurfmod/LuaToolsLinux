//! NetPacket OnlineFix hook layer for Linux (BST-34).
//!
//! Port of UsefulSteamTools / BetterSteamTools `Hooks_NetPacket.cpp` namespace
//! `Hooks_NetPacket_OnlineFix` (lines 1752-1812).
//!
//! Handles outgoing `CMsgClientGamesPlayed` (eMsg 742 / 5410):
//! When a game launched with `-onlinefix` (reporting Spacewar AppID 480) sends its
//! active games list, replaces `game_extra_info` (canonical field 7 string) with the
//! real game's title, allowing friends to see the genuine game name.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use crate::steam_bridge::interop::interop_misc::{
    get_game_name_by_app_id, resolve_app_id, ONLINE_FIX_APP_ID,
};
use crate::steam_bridge::interop::ipc::{
    self, fnv1a_hash, FuncHash, HandlerFn, IpcContext, IpcInterface,
};
use crate::steam_bridge::netpacket::netpacket_core::{
    register_netpacket_handler, NetPacketContext, NetPacketDirection,
};

/// Steam message ID for CMsgClientGamesPlayed (legacy/protobuf).
pub const K_EMSG_CLIENT_GAMES_PLAYED: u32 = 742;
/// Steam message ID for CMsgClientGamesPlayed (protobuf).
pub const K_EMSG_CLIENT_GAMES_PLAYED_PB: u32 = 5410;

/// FuncHash for CMsgClientGamesPlayed send handler.
pub const NETPACKET_ONLINEFIX_GAMES_PLAYED_FNHASH: FuncHash =
    fnv1a_hash("CMsgClientGamesPlayed");

/// OnlineFix handler definition.
#[derive(Clone)]
pub struct OnlineFixHandler {
    pub interface_id: IpcInterface,
    pub func_hash: FuncHash,
    pub name: String,
    pub fencepost: u32,
    pub argc: u32,
    pub pre_handler: Option<HandlerFn>,
    pub post_handler: Option<HandlerFn>,
}

impl fmt::Debug for OnlineFixHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OnlineFixHandler")
            .field("interface_id", &self.interface_id)
            .field("func_hash", &self.func_hash)
            .field("name", &self.name)
            .finish()
    }
}

static ONLINEFIX_HANDLERS: LazyLock<Mutex<Vec<OnlineFixHandler>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static ONLINEFIX_ACTIVE: AtomicBool = AtomicBool::new(false);

fn onlinefix_handlers() -> &'static Mutex<Vec<OnlineFixHandler>> {
    &ONLINEFIX_HANDLERS
}

/// Parse protobuf varint from slice.
pub fn decode_varint(slice: &[u8]) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    for (idx, &byte) in slice.iter().enumerate() {
        if idx >= 10 {
            return None;
        }
        if idx == 9 && byte > 1 {
            return None;
        }
        result |= u64::from(byte & 0x7F) << shift;
        if (byte & 0x80) == 0 {
            return Some((result, idx + 1));
        }
        shift += 7;
    }
    None
}

/// Encode a protobuf varint.
pub fn encode_varint(mut val: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (val & 0x7F) as u8;
        val >>= 7;
        if val != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if val == 0 {
            break;
        }
    }
}

/// Parse a length-delimited field from protobuf wire format.
pub fn parse_length_delimited(slice: &[u8]) -> Option<(&[u8], usize)> {
    let (len, consumed) = decode_varint(slice)?;
    let total = consumed + len as usize;
    if slice.len() < total {
        return None;
    }
    Some((&slice[consumed..total], total))
}

/// Canonical `CMsgClientGamesPlayed.GamePlayed` message:
/// - `steam_id_gs`: field 1, varint (uint64)
/// - `game_id`: field 2, fixed64 (wire type 1, 8 bytes)
/// - `game_extra_info`: field 7, length-delimited string (wire type 2)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GamePlayed {
    pub steam_id_gs: u64,
    pub game_id: u64,
    pub game_extra_info: Option<String>,
    pub raw_unknown: Vec<u8>,
}

impl GamePlayed {
    pub fn new(game_id: u64) -> Self {
        Self {
            steam_id_gs: 0,
            game_id,
            game_extra_info: None,
            raw_unknown: Vec::new(),
        }
    }

    pub fn with_extra_info(game_id: u64, extra: &str) -> Self {
        Self {
            steam_id_gs: 0,
            game_id,
            game_extra_info: Some(extra.to_string()),
            raw_unknown: Vec::new(),
        }
    }

    /// Extract the 32-bit AppID from the 64-bit GameID.
    pub fn app_id(&self) -> u32 {
        (self.game_id & 0xFFFF_FFFF) as u32
    }

    /// Parse a `GamePlayed` entry from wire bytes.
    pub fn parse(data: &[u8]) -> Option<Self> {
        let mut steam_id_gs = 0u64;
        let mut game_id = 0u64;
        let mut game_extra_info = None;
        let mut raw_unknown = Vec::new();
        let mut offset = 0;

        while offset < data.len() {
            let field_start = offset;
            let (tag, tag_len) = decode_varint(&data[offset..])?;
            offset += tag_len;

            let field_num = tag >> 3;
            let wire_type = tag & 0x7;

            match (field_num, wire_type) {
                (1, 0) => {
                    // steam_id_gs (varint)
                    let (val, consumed) = decode_varint(&data[offset..])?;
                    steam_id_gs = val;
                    offset += consumed;
                }
                (2, 1) => {
                    // canonical Valve fixed64 game_id
                    if offset + 8 <= data.len() {
                        let bytes: [u8; 8] = data[offset..offset + 8].try_into().ok()?;
                        game_id = u64::from_le_bytes(bytes);
                        offset += 8;
                    } else {
                        return None;
                    }
                }
                // Fallbacks: varint game_id
                (2, 0) if game_id == 0 => {
                    let (val, consumed) = decode_varint(&data[offset..])?;
                    game_id = val;
                    offset += consumed;
                }
                (7, 2) => {
                    // canonical Valve string game_extra_info
                    let (extra_data, consumed) = parse_length_delimited(&data[offset..])?;
                    game_extra_info = String::from_utf8(extra_data.to_vec()).ok();
                    offset += consumed;
                }
                // Fallbacks: field 2 string
                (2, 2) => {
                    let (extra_data, consumed) = parse_length_delimited(&data[offset..])?;
                    game_extra_info = String::from_utf8(extra_data.to_vec()).ok();
                    offset += consumed;
                }
                _ => {
                    match wire_type {
                        0 => {
                            let (_, consumed) = decode_varint(&data[offset..])?;
                            offset += consumed;
                        }
                        1 => {
                            if offset + 8 <= data.len() {
                                offset += 8;
                            } else {
                                return None;
                            }
                        }
                        2 => {
                            let (_, consumed) = parse_length_delimited(&data[offset..])?;
                            offset += consumed;
                        }
                        5 => {
                            if offset + 4 <= data.len() {
                                offset += 4;
                            } else {
                                return None;
                            }
                        }
                        _ => return None,
                    }
                    raw_unknown.extend_from_slice(&data[field_start..offset]);
                }
            }
        }

        Some(Self {
            steam_id_gs,
            game_id,
            game_extra_info,
            raw_unknown,
        })
    }

    /// Serialize to canonical Valve wire format:
    /// - field 1 (if != 0): steam_id_gs varint
    /// - field 2: fixed64 game_id (wire type 1)
    /// - field 7: length-delimited game_extra_info (wire type 2)
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();

        if self.steam_id_gs != 0 {
            out.push(0x08); // field 1, wire type 0
            encode_varint(self.steam_id_gs, &mut out);
        }

        // Field 2: game_id as fixed64 (tag = (2 << 3) | 1 = 0x11)
        out.push(0x11);
        out.extend_from_slice(&self.game_id.to_le_bytes());

        // Field 7: game_extra_info as length-delimited (tag = (7 << 3) | 2 = 0x3A)
        if let Some(extra_info) = &self.game_extra_info {
            out.push(0x3A);
            let extra_bytes = extra_info.as_bytes();
            encode_varint(extra_bytes.len() as u64, &mut out);
            out.extend_from_slice(extra_bytes);
        }

        out.extend_from_slice(&self.raw_unknown);
        out
    }
}

/// Canonical `CMsgClientGamesPlayed` message (eMsg 742 / 5410):
/// - `games_played`: repeated field 1, length-delimited
/// - `client_os_type`: field 2, uint32 varint
/// - `cloud_gaming_platform`: field 3, uint32 varint
/// - `recent_reauthentication`: field 4, bool varint
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GamesPlayedMsg {
    pub games_played: Vec<GamePlayed>,
    pub client_os_type: Option<u32>,
    pub cloud_gaming_platform: Option<u32>,
    pub recent_reauthentication: Option<bool>,
    pub raw_unknown: Vec<u8>,
}

/// Parse `CMsgClientGamesPlayed` from protobuf body bytes.
pub fn parse_games_played(body: &[u8]) -> Option<GamesPlayedMsg> {
    let mut msg = GamesPlayedMsg::default();
    let mut offset = 0;

    while offset < body.len() {
        let field_start = offset;
        let (tag, tag_len) = decode_varint(&body[offset..])?;
        offset += tag_len;

        let field_num = tag >> 3;
        let wire_type = tag & 0x7;

        match (field_num, wire_type) {
            (1, 2) => {
                let (game_data, consumed) = parse_length_delimited(&body[offset..])?;
                if let Some(game) = GamePlayed::parse(game_data) {
                    msg.games_played.push(game);
                }
                offset += consumed;
            }
            (2, 0) => {
                let (val, consumed) = decode_varint(&body[offset..])?;
                msg.client_os_type = Some(val as u32);
                offset += consumed;
            }
            (3, 0) => {
                let (val, consumed) = decode_varint(&body[offset..])?;
                msg.cloud_gaming_platform = Some(val as u32);
                offset += consumed;
            }
            (4, 0) => {
                let (val, consumed) = decode_varint(&body[offset..])?;
                msg.recent_reauthentication = Some(val != 0);
                offset += consumed;
            }
            _ => {
                match wire_type {
                    0 => {
                        let (_, c) = decode_varint(&body[offset..])?;
                        offset += c;
                    }
                    1 => {
                        if offset + 8 <= body.len() {
                            offset += 8;
                        } else {
                            return None;
                        }
                    }
                    2 => {
                        let (_, c) = parse_length_delimited(&body[offset..])?;
                        offset += c;
                    }
                    5 => {
                        if offset + 4 <= body.len() {
                            offset += 4;
                        } else {
                            return None;
                        }
                    }
                    _ => return None,
                }
                msg.raw_unknown.extend_from_slice(&body[field_start..offset]);
            }
        }
    }

    Some(msg)
}

/// Serialize `CMsgClientGamesPlayed` to protobuf body bytes.
pub fn serialize_games_played(msg: &GamesPlayedMsg) -> Vec<u8> {
    let mut out = Vec::new();

    // Field 1: games_played (repeated length-delimited)
    for game in &msg.games_played {
        out.push(0x0A); // tag = (1 << 3) | 2 = 0x0A
        let game_bytes = game.serialize();
        encode_varint(game_bytes.len() as u64, &mut out);
        out.extend_from_slice(&game_bytes);
    }

    // Field 2: client_os_type
    if let Some(val) = msg.client_os_type {
        out.push(0x10);
        encode_varint(val as u64, &mut out);
    }

    // Field 3: cloud_gaming_platform
    if let Some(val) = msg.cloud_gaming_platform {
        out.push(0x18);
        encode_varint(val as u64, &mut out);
    }

    // Field 4: recent_reauthentication
    if let Some(val) = msg.recent_reauthentication {
        out.push(0x20);
        encode_varint(if val { 1 } else { 0 }, &mut out);
    }

    out.extend_from_slice(&msg.raw_unknown);
    out
}

/// Handle outgoing `CMsgClientGamesPlayed` (eMsg 742 / 5410) over NetPacket.
/// Restores real game title in `game_extra_info` when spoofing Spacewar 480 multiplayer (BST-34).
pub fn handle_send_games_played_netpacket(ctx: &mut NetPacketContext) -> Result<(), String> {
    // 1. Trigger RichPresence tracking (matching Hooks_NetPacket.cpp:1773)
    let _ = crate::steam_bridge::netpacket::netpacket_richpresence::track_send(ctx);

    let body = ctx.body();
    if body.is_empty() {
        return Ok(());
    }

    let Some(mut msg) = parse_games_played(body) else {
        log::debug!("[BST-34] Failed to parse CMsgClientGamesPlayed");
        return Ok(());
    };

    let real_app_id = resolve_app_id();
    let mut needs_patch = false;

    for game in &mut msg.games_played {
        let appid = game.app_id();
        if appid == ONLINE_FIX_APP_ID && real_app_id != 0 && real_app_id != ONLINE_FIX_APP_ID {
            let real_name = get_game_name_by_app_id(real_app_id);
            if game.game_extra_info.as_deref() != Some(&real_name) {
                game.game_extra_info = Some(real_name.clone());
                needs_patch = true;
                log::info!(
                    "[BST-34] Restored game_extra_info: 480 -> '{}' (real appid {})",
                    real_name,
                    real_app_id
                );
            }
        }
    }

    if needs_patch {
        let new_body = serialize_games_played(&msg);
        let mut new_frame = Vec::with_capacity(ctx.body_offset + new_body.len());
        new_frame.extend_from_slice(&ctx.frame_buffer[..ctx.body_offset]);
        new_frame.extend_from_slice(&new_body);
        ctx.replace_frame(new_frame);
        log::debug!("[BST-34] Successfully patched CMsgClientGamesPlayed wire frame");
    }

    Ok(())
}

/// Handle outgoing `CMsgClientGamesPlayed` over IPC.
pub fn handle_send_games_played_post(ctx: &mut IpcContext) {
    let real_app_id = resolve_app_id();
    if real_app_id == 0 || real_app_id == ONLINE_FIX_APP_ID {
        return;
    }

    if ctx.write_buffer.len() < 12 {
        return;
    }
    let body_start = 12;
    if ctx.write_buffer.len() <= body_start {
        return;
    }

    let body = &ctx.write_buffer[body_start..];
    let Some(mut msg) = parse_games_played(body) else {
        return;
    };

    let mut needs_patch = false;
    for game in &mut msg.games_played {
        if game.app_id() == ONLINE_FIX_APP_ID {
            let real_name = get_game_name_by_app_id(real_app_id);
            if game.game_extra_info.as_deref() != Some(&real_name) {
                game.game_extra_info = Some(real_name);
                needs_patch = true;
            }
        }
    }

    if needs_patch {
        let new_body = serialize_games_played(&msg);
        ctx.write_buffer.truncate(body_start);
        ctx.write_buffer.extend_from_slice(&new_body);
    }
}

/// Register all OnlineFix handlers.
pub fn register_onlinefix_handlers() {
    let iface = IpcInterface::IClientUser;

    let games_played_post: HandlerFn = Arc::new(|ctx: &mut IpcContext| -> Result<(), String> {
        handle_send_games_played_post(ctx);
        Ok(())
    });

    let handler = OnlineFixHandler {
        interface_id: iface,
        func_hash: NETPACKET_ONLINEFIX_GAMES_PLAYED_FNHASH,
        name: "Hooks_NetPacket_OnlineFix::HandleSend_CMsgClientGamesPlayed".to_string(),
        fencepost: 0,
        argc: 4,
        pre_handler: None,
        post_handler: Some(Arc::clone(&games_played_post)),
    };

    onlinefix_handlers().lock().unwrap_or_else(|e| e.into_inner()).push(handler);

    ipc::register_handler(ipc::make_handler(
        iface,
        NETPACKET_ONLINEFIX_GAMES_PLAYED_FNHASH,
        "Hooks_NetPacket_OnlineFix::HandleSend_CMsgClientGamesPlayed",
        0,
        4,
        None,
        Some(games_played_post),
    ));

    // Register NetPacket send handlers for eMsg 742 and 5410
    register_netpacket_handler(crate::steam_bridge::netpacket::netpacket_core::NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: K_EMSG_CLIENT_GAMES_PLAYED,
        target_job_hash: None,
        name: "OnlineFix::HandleSend_GamesPlayed_742".to_string(),
        handler: Arc::new(|ctx| handle_send_games_played_netpacket(ctx)),
    });

    register_netpacket_handler(crate::steam_bridge::netpacket::netpacket_core::NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: K_EMSG_CLIENT_GAMES_PLAYED_PB,
        target_job_hash: None,
        name: "OnlineFix::HandleSend_GamesPlayed_5410".to_string(),
        handler: Arc::new(|ctx| handle_send_games_played_netpacket(ctx)),
    });
}

/// Unregister all OnlineFix handlers.
pub fn unregister_onlinefix_handlers() {
    onlinefix_handlers().lock().unwrap_or_else(|e| e.into_inner()).clear();
    let iface = IpcInterface::IClientUser;
    let _ = ipc::unregister_handler(iface, NETPACKET_ONLINEFIX_GAMES_PLAYED_FNHASH);
}

pub fn install_onlinefix() -> Result<(), String> {
    register_onlinefix_handlers();
    ONLINEFIX_ACTIVE.store(true, Ordering::Release);
    log::info!("Installed OnlineFix NetPacket hooks");
    Ok(())
}

pub fn uninstall_onlinefix() {
    unregister_onlinefix_handlers();
    ONLINEFIX_ACTIVE.store(false, Ordering::Release);
    log::info!("Uninstalled OnlineFix NetPacket hooks");
}

pub fn install_onlinefix_hooks() -> Result<(), String> {
    install_onlinefix()
}

pub fn uninstall_onlinefix_hooks() {
    uninstall_onlinefix()
}

pub fn is_onlinefix_hook_active() -> bool {
    ONLINEFIX_ACTIVE.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn test_game_played_canonical_fixed64_and_field7_string() {
        let game = GamePlayed {
            steam_id_gs: 0x0110_0001_0000_1234,
            game_id: 480, // Lower 32-bit = 480
            game_extra_info: Some("ELDEN RING".to_string()),
            raw_unknown: vec![],
        };

        let bytes = game.serialize();
        let parsed = GamePlayed::parse(&bytes).unwrap();

        assert_eq!(parsed.steam_id_gs, 0x0110_0001_0000_1234);
        assert_eq!(parsed.game_id, 480);
        assert_eq!(parsed.app_id(), 480);
        assert_eq!(parsed.game_extra_info, Some("ELDEN RING".to_string()));
    }

    #[test]
    fn test_games_played_msg_roundtrip() {
        let msg = GamesPlayedMsg {
            games_played: vec![
                GamePlayed::with_extra_info(480, "Spacewar"),
                GamePlayed::with_extra_info(730, "Counter-Strike 2"),
            ],
            client_os_type: Some(16),
            cloud_gaming_platform: Some(0),
            recent_reauthentication: Some(false),
            raw_unknown: vec![],
        };

        let body = serialize_games_played(&msg);
        let parsed = parse_games_played(&body).unwrap();

        assert_eq!(parsed.games_played.len(), 2);
        assert_eq!(parsed.games_played[0].app_id(), 480);
        assert_eq!(parsed.games_played[0].game_extra_info.as_deref(), Some("Spacewar"));
        assert_eq!(parsed.games_played[1].app_id(), 730);
        assert_eq!(parsed.games_played[1].game_extra_info.as_deref(), Some("Counter-Strike 2"));
        assert_eq!(parsed.client_os_type, Some(16));
    }

    #[test]
    fn test_onlinefix_title_restoration_netpacket() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        // Set real AppID to Elden Ring (1245620)
        crate::steam_bridge::interop::interop_misc::set_online_fix_real_app_id(1245620);
        crate::steam_bridge::interop::interop_misc::set_game_name_by_app_id(
            1245620,
            "ELDEN RING".to_string(),
        );

        let initial_msg = GamesPlayedMsg {
            games_played: vec![GamePlayed::with_extra_info(480, "Spacewar")],
            client_os_type: Some(16),
            cloud_gaming_platform: None,
            recent_reauthentication: None,
            raw_unknown: vec![],
        };

        let body = serialize_games_played(&initial_msg);
        let mut frame = Vec::new();
        // MsgHdr (8 bytes: eMsg 742, headerLength 20) + job ids (12 bytes) = 20 bytes
        frame.extend_from_slice(&[0xE6, 0x02, 0x00, 0x80]); // eMsg = 742 | 0x80000000
        frame.extend_from_slice(&20u32.to_le_bytes());       // header_length = 20
        frame.extend_from_slice(&[0u8; 12]);                // job ids
        frame.extend_from_slice(&body);

        let mut ctx = NetPacketContext::new(frame, NetPacketDirection::Send, 480).unwrap();
        let res = handle_send_games_played_netpacket(&mut ctx);
        assert!(res.is_ok());

        // Verify that game_extra_info was patched to "ELDEN RING"
        let modified = parse_games_played(ctx.body()).unwrap();
        assert_eq!(modified.games_played[0].app_id(), 480);
        assert_eq!(
            modified.games_played[0].game_extra_info.as_deref(),
            Some("ELDEN RING")
        );

        // Reset
        crate::steam_bridge::interop::interop_misc::set_online_fix_real_app_id(0);
    }
}
