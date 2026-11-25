//! NetPacket OnlineFix hook layer for Linux.
//!
//! Port of usefulsteamtools `Interop_NetPacket.cpp` namespace
//! `Hooks_NetPacket_OnlineFix` (lines 1752-1812).
//!
//! Handles outgoing `CMsgClientGamesPlayed` (eMsg 742/5410) -
//! when game launched with `-onlinefix` (appid 480/Spacewar),
//! replaces `game_extra_info` with real game's localized name.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use crate::steam_bridge::interop::interop_misc::{
    get_game_name_by_app_id, resolve_app_id, ONLINE_FIX_APP_ID,
};
use crate::steam_bridge::interop::ipc::{
    self, fnv1a_hash, FuncHash, HandlerFn, IpcContext, IpcInterface,
};
use crate::steam_bridge::netpacket::netpacket_core::{register_netpacket_handler, NetPacketDirection};

/// Steam message ID for CMsgClientGamesPlayed (legacy/protobuf).
const K_EMSG_CLIENT_GAMES_PLAYED: u32 = 742;
/// Steam message ID for CMsgClientGamesPlayed (protobuf).
const K_EMSG_CLIENT_GAMES_PLAYED_PB: u32 = 5410;

/// FuncHash for CMsgClientGamesPlayed send handler.
const NETPACKET_ONLINEFIX_GAMES_PLAYED_FNHASH: FuncHash = fnv1a_hash("CMsgClientGamesPlayed");

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
/// Returns (value, bytes_consumed) or None if truncated/overflow.
fn decode_varint(slice: &[u8]) -> Option<(u64, usize)> {
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

/// Encode a protobuf varint correctly (MSB `0x80` set while more bytes remain).
fn encode_varint(mut val: u64, out: &mut Vec<u8>) {
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
/// Returns (field_data, bytes_consumed) or None if invalid.
fn parse_length_delimited(slice: &[u8]) -> Option<(&[u8], usize)> {
    let (len, consumed) = decode_varint(slice)?;
    let total = consumed + len as usize;
    if slice.len() < total {
        return None;
    }
    Some((&slice[consumed..total], total))
}

/// CMsgClientGamesPlayed message fields.
/// Based on Steam's CMsgClientGamesPlayed:
/// message CMsgClientGamesPlayed {
///     repeated GamePlayed games_played = 1;
/// }
/// message GamePlayed {
///     uint32 game_id = 1;
///     string game_extra_info = 2;
///     // ... other fields
/// }
#[derive(Debug, Clone)]
struct GamePlayed {
    game_id: u32,
    game_extra_info: Option<String>,
    raw_data: Vec<u8>,  // Original raw data for reconstruction
}

impl GamePlayed {
    /// Parse a GamePlayed from protobuf wire format.
    fn parse(data: &[u8]) -> Option<Self> {
        let mut game_id = 0u32;
        let mut game_extra_info = None;
        let mut offset = 0;

        while offset < data.len() {
            let (tag, tag_len) = decode_varint(&data[offset..])?;
            offset += tag_len;

            let field_num = tag >> 3;
            let wire_type = tag & 0x7;

            match (field_num, wire_type) {
                (1, 0) => {  // game_id (varint)
                    let (val, consumed) = decode_varint(&data[offset..])?;
                    game_id = val as u32;
                    offset += consumed;
                }
                (2, 2) => {  // game_extra_info (length-delimited string)
                    let (extra_data, consumed) = parse_length_delimited(&data[offset..])?;
                    game_extra_info = String::from_utf8(extra_data.to_vec()).ok();
                    offset += consumed;
                }
                _ => {
                    // Skip unknown fields
                    match wire_type {
                        0 => { // varint
                            let (_, consumed) = decode_varint(&data[offset..])?;
                            offset += consumed;
                        }
                        1 => { // 64-bit
                            if offset + 8 <= data.len() {
                                offset += 8;
                            } else {
                                return None;
                            }
                        }
                        2 => { // length-delimited
                            let (_, consumed) = parse_length_delimited(&data[offset..])?;
                            offset += consumed;
                        }
                        5 => { // 32-bit
                            if offset + 4 <= data.len() {
                                offset += 4;
                            } else {
                                return None;
                            }
                        }
                        _ => return None,
                    }
                }
            }
        }

        Some(GamePlayed {
            game_id,
            game_extra_info,
            raw_data: data.to_vec(),
        })
    }

    /// Serialize the GamePlayed back to protobuf wire format.
    fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();

        // Field 1: game_id (varint)
        out.push(0x08);  // field 1, wire type 0
        encode_varint(self.game_id as u64, &mut out);

        // Field 2: game_extra_info (length-delimited) if present
        if let Some(extra_info) = &self.game_extra_info {
            out.push(0x12);  // field 2, wire type 2
            let extra_bytes = extra_info.as_bytes();
            encode_varint(extra_bytes.len() as u64, &mut out);
            out.extend_from_slice(extra_bytes);
        }

        out
    }
}

/// Parse the CMsgClientGamesPlayed message body.
/// Returns a vector of GamePlayed for each game in the request.
fn parse_games_played(body: &[u8]) -> Option<Vec<GamePlayed>> {
    let mut games = Vec::new();
    let mut offset = 0;

    while offset < body.len() {
        let (tag, tag_len) = decode_varint(&body[offset..])?;
        offset += tag_len;

        let field_num = tag >> 3;
        let wire_type = tag & 0x7;

        match (field_num, wire_type) {
            (1, 2) => {  // games_played (repeated length-delimited)
                let (game_data, consumed) = parse_length_delimited(&body[offset..])?;
                if let Some(game) = GamePlayed::parse(game_data) {
                    games.push(game);
                }
                offset += consumed;
            }
            _ => {
                // Skip unknown fields
                match wire_type {
                    0 => { let (_, c) = decode_varint(&body[offset..])?; offset += c; }
                    1 => { if offset + 8 <= body.len() { offset += 8; } else { return None; } }
                    2 => { let (_, c) = parse_length_delimited(&body[offset..])?; offset += c; }
                    5 => { if offset + 4 <= body.len() { offset += 4; } else { return None; } }
                    _ => return None,
                }
            }
        }
    }

    Some(games)
}

/// Serialize the CMsgClientGamesPlayed message body.
fn serialize_games_played(games: &[GamePlayed]) -> Vec<u8> {
    let mut out = Vec::new();

    for game in games {
        // Field 1: games_played (repeated length-delimited)
        out.push(0x0A);  // field 1, wire type 2
        let game_data = game.serialize();
        encode_varint(game_data.len() as u64, &mut out);
        out.extend_from_slice(&game_data);
    }

    out
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

    // Also register as NetPacket send handler for eMsg 742 and 5410
    register_netpacket_send_handlers();
}

/// Unregister all OnlineFix handlers.
pub fn unregister_onlinefix_handlers() {
    onlinefix_handlers().lock().unwrap_or_else(|e| e.into_inner()).clear();
    let iface = IpcInterface::IClientUser;
    let _ = ipc::unregister_handler(iface, NETPACKET_ONLINEFIX_GAMES_PLAYED_FNHASH);
}

/// Register NetPacket send handlers for CMsgClientGamesPlayed.
fn register_netpacket_send_handlers() {
    // eMsg 742 - legacy CMsgClientGamesPlayed
    register_netpacket_handler(crate::steam_bridge::netpacket::netpacket_core::NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: K_EMSG_CLIENT_GAMES_PLAYED,
        target_job_hash: None,
        name: "OnlineFix::HandleSend_GamesPlayed_742".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("OnlineFix Send: CMsgClientGamesPlayed (742)");
            handle_send_games_played_netpacket(ctx);
            Ok(())
        }),
    });

    // eMsg 5410 - protobuf CMsgClientGamesPlayed
    register_netpacket_handler(crate::steam_bridge::netpacket::netpacket_core::NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: K_EMSG_CLIENT_GAMES_PLAYED_PB,
        target_job_hash: None,
        name: "OnlineFix::HandleSend_GamesPlayed_5410".to_string(),
        handler: Arc::new(|ctx| {
            log::debug!("OnlineFix Send: CMsgClientGamesPlayed (5410)");
            handle_send_games_played_netpacket(ctx);
            Ok(())
        }),
    });
}

/// Handle outgoing CMsgClientGamesPlayed (IPC handler).
/// When online fix is active (appid 480/Spacewar), replace game_extra_info with real game name.
fn handle_send_games_played_post(ctx: &mut IpcContext) {
    // Check if online fix is active
    let real_app_id = resolve_app_id();
    if real_app_id == 0 || real_app_id == ONLINE_FIX_APP_ID {
        // Online fix not active, or we're already at the real app
        return;
    }

    // The message body starts after the IPC header (12 bytes)
    if ctx.write_buffer.len() < 12 {
        return;
    }

    let body_start = 12;  // Skip IPC header
    if ctx.write_buffer.len() <= body_start {
        return;
    }

    let body = &ctx.write_buffer[body_start..];

    // Parse the request
    let Some(mut games) = parse_games_played(body) else {
        log::debug!("OnlineFix: Failed to parse CMsgClientGamesPlayed");
        return;
    };

    // Check each game - if game_id is 480 (Spacewar/OnlineFix), replace game_extra_info
    let mut needs_patch = false;
    for game in &mut games {
        if game.game_id == ONLINE_FIX_APP_ID {
            // Get the real game's localized name
            let real_name = get_game_name_by_app_id(real_app_id);

            if game.game_extra_info.as_ref() != Some(&real_name) {
                game.game_extra_info = Some(real_name.clone());
                needs_patch = true;
                log::info!(
                    "OnlineFix: Replacing game_extra_info for appId=480 -> '{}' (real appId={})",
                    real_name, real_app_id
                );
            }
        }
    }

    // If any games needed patching, serialize the modified request back
    if needs_patch {
        let new_body = serialize_games_played(&games);

        // Replace the body in write_buffer
        ctx.write_buffer.truncate(body_start);
        ctx.write_buffer.extend_from_slice(&new_body);

        log::debug!(
            "OnlineFix: Patched CMsgClientGamesPlayed - new body size: {}",
            new_body.len()
        );
    }
}

/// Handle outgoing CMsgClientGamesPlayed (NetPacket handler).
/// When online fix is active (appid 480/Spacewar), replace game_extra_info with real game name.
fn handle_send_games_played_netpacket(ctx: &mut crate::steam_bridge::netpacket::netpacket_core::NetPacketContext) {
    // Check if online fix is active
    let real_app_id = resolve_app_id();
    if real_app_id == 0 || real_app_id == ONLINE_FIX_APP_ID {
        // Online fix not active, or we're already at the real app
        return;
    }

    // Get the body from the frame buffer
    let body = ctx.body();
    if body.is_empty() {
        return;
    }

    // Parse the request
    let Some(mut games) = parse_games_played(body) else {
        log::debug!("OnlineFix: Failed to parse CMsgClientGamesPlayed (NetPacket)");
        return;
    };

    // Check each game - if game_id is 480 (Spacewar/OnlineFix), replace game_extra_info
    let mut needs_patch = false;
    for game in &mut games {
        if game.game_id == ONLINE_FIX_APP_ID {
            // Get the real game's localized name
            let real_name = get_game_name_by_app_id(real_app_id);

            if game.game_extra_info.as_ref() != Some(&real_name) {
                game.game_extra_info = Some(real_name.clone());
                needs_patch = true;
                log::info!(
                    "OnlineFix: Replacing game_extra_info for appId=480 -> '{}' (real appId={})",
                    real_name, real_app_id
                );
            }
        }
    }

    // If any games needed patching, serialize the modified request back
    if needs_patch {
        let new_body = serialize_games_played(&games);

        // Replace the frame body
        ctx.replace_frame({
            let mut new_frame = Vec::with_capacity(ctx.frame_buffer.len());
            // Copy header
            new_frame.extend_from_slice(&ctx.frame_buffer[..ctx.body_offset]);
            // Add new body
            new_frame.extend_from_slice(&new_body);
            new_frame
        });

        log::debug!(
            "OnlineFix: Patched CMsgClientGamesPlayed (NetPacket) - new body size: {}",
            new_body.len()
        );
    }
}

/// Delegate to RichPresence::TrackSend for presence tracking.
/// This is called after modifying the GamesPlayed message.
fn track_rich_presence_send(app_id: u32) {
    // Delegate to RichPresence tracking
    // In the C++ code, this calls Hooks_NetPacket_RichPresence::TrackSend
    // Here we just log for now - the actual RichPresence module would handle this
    log::debug!("OnlineFix: Delegating to RichPresence::TrackSend for appId={}", app_id);
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

    static ONLINEFIX_TEST_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn test_fnv1a_hash_onlinefix() {
        assert_eq!(
            NETPACKET_ONLINEFIX_GAMES_PLAYED_FNHASH,
            fnv1a_hash("CMsgClientGamesPlayed")
        );
    }

    #[test]
    fn test_decode_varint() {
        // Single byte
        assert_eq!(decode_varint(&[0x01]), Some((1, 1)));
        assert_eq!(decode_varint(&[0x7F]), Some((127, 1)));

        // Multi-byte
        assert_eq!(decode_varint(&[0x80, 0x01]), Some((128, 2)));
        assert_eq!(decode_varint(&[0xFF, 0xFF, 0xFF, 0x7F]), Some((268435455, 4)));

        // Truncated
        assert!(decode_varint(&[0x80]).is_none());

        // 10th byte overflow
        assert!(decode_varint(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02]).is_none());
    }

    #[test]
    fn test_encode_varint() {
        let mut out = Vec::new();
        encode_varint(1, &mut out);
        assert_eq!(out, vec![0x01]);

        out.clear();
        encode_varint(127, &mut out);
        assert_eq!(out, vec![0x7F]);

        out.clear();
        encode_varint(128, &mut out);
        assert_eq!(out, vec![0x80, 0x01]);

        out.clear();
        encode_varint(300, &mut out);
        assert_eq!(out, vec![0xAC, 0x02]);

        out.clear();
        encode_varint(268435455, &mut out);
        assert_eq!(out, vec![0xFF, 0xFF, 0xFF, 0x7F]);
    }

    #[test]
    fn test_parse_length_delimited() {
        // String "hello" (5 bytes)
        let data = vec![0x05, b'h', b'e', b'l', b'l', b'o'];
        let (result, consumed) = parse_length_delimited(&data).unwrap();
        assert_eq!(result, b"hello");
        assert_eq!(consumed, 6);

        // Empty string
        let data = vec![0x00];
        let (result, consumed) = parse_length_delimited(&data).unwrap();
        assert_eq!(result, b"");
        assert_eq!(consumed, 1);

        // Truncated
        assert!(parse_length_delimited(&[0x05, b'h', b'e']).is_none());
    }

    #[test]
    fn test_game_played_parse_and_serialize() {
        // Build a GamePlayed with game_id=480 and game_extra_info="Spacewar"
        let game = GamePlayed {
            game_id: 480,
            game_extra_info: Some("Spacewar".to_string()),
            raw_data: vec![],
        };

        let serialized = game.serialize();

        // Parse it back
        let parsed = GamePlayed::parse(&serialized).unwrap();
        assert_eq!(parsed.game_id, 480);
        assert_eq!(parsed.game_extra_info, Some("Spacewar".to_string()));

        // Test without game_extra_info
        let game_no_extra = GamePlayed {
            game_id: 480,
            game_extra_info: None,
            raw_data: vec![],
        };
        let serialized = game_no_extra.serialize();
        let parsed = GamePlayed::parse(&serialized).unwrap();
        assert_eq!(parsed.game_id, 480);
        assert_eq!(parsed.game_extra_info, None);
    }

    #[test]
    fn test_parse_games_played() {
        // Create a request with two games
        let game1 = GamePlayed {
            game_id: 480,
            game_extra_info: Some("Spacewar".to_string()),
            raw_data: vec![],
        };
        let game2 = GamePlayed {
            game_id: 730,
            game_extra_info: Some("Counter-Strike 2".to_string()),
            raw_data: vec![],
        };

        let body = serialize_games_played(&[game1, game2]);
        let parsed = parse_games_played(&body).unwrap();

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].game_id, 480);
        assert_eq!(parsed[0].game_extra_info, Some("Spacewar".to_string()));
        assert_eq!(parsed[1].game_id, 730);
        assert_eq!(parsed[1].game_extra_info, Some("Counter-Strike 2".to_string()));
    }

    #[test]
    fn test_serialize_games_played() {
        let game1 = GamePlayed {
            game_id: 480,
            game_extra_info: Some("Spacewar".to_string()),
            raw_data: vec![],
        };
        let game2 = GamePlayed {
            game_id: 730,
            game_extra_info: Some("CS2".to_string()),
            raw_data: vec![],
        };

        let body = serialize_games_played(&[game1, game2]);
        assert!(!body.is_empty());

        // Verify it parses back correctly
        let parsed = parse_games_played(&body).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].game_id, 480);
        assert_eq!(parsed[0].game_extra_info, Some("Spacewar".to_string()));
        assert_eq!(parsed[1].game_id, 730);
        assert_eq!(parsed[1].game_extra_info, Some("CS2".to_string()));
    }

    #[test]
    fn test_onlinefix_handler_registration_and_uninstall() {
        let _g = ONLINEFIX_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        install_onlinefix().unwrap();
        assert!(is_onlinefix_hook_active());

        {
            let handlers = onlinefix_handlers().lock().unwrap();
            assert!(!handlers.is_empty());
            assert!(handlers.iter().any(|h| h.name.contains("OnlineFix")));
        }
        assert!(ipc::find_handler(
            IpcInterface::IClientUser,
            NETPACKET_ONLINEFIX_GAMES_PLAYED_FNHASH
        ).is_some());

        uninstall_onlinefix();
        assert!(!is_onlinefix_hook_active());
        assert!(ipc::find_handler(
            IpcInterface::IClientUser,
            NETPACKET_ONLINEFIX_GAMES_PLAYED_FNHASH
        ).is_none());
    }
}