//! NetPacket RichPresence hook layer for Linux.
//!
//! Port of usefulsteamtools `Hooks_NetPacket_RichPresence` namespace from Interop_NetPacket.cpp (lines 1461-1749).
//!
//! This module handles:
//! - **Outgoing**: `CMsgClientGamesPlayed` (eMsg 742/5410)
//!   - Track topmost running app (last in games_played array)
//!   - If topmost is Lua-unlocked (not owned/onlinefix): capture rich presence KVs
//!   - Build manufactured PersonaState packet for injection
//! - **Incoming**: `CMsgClientPersonaState` (eMsg 766)
//!   - Cache real self-pushes as template
//!   - If tracking unowned game: patch live message in-place
//!   - Deliver manufactured packet via carrier packet borrowing

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use crate::steam_bridge::interop::ONLINE_FIX_APP_ID;
use crate::steam_bridge::cloud_redirect::{set_account_id, notify_app_running};
use crate::steam_bridge::netpacket::netpacket_core::{
    NetPacketContext, NetPacketDirection, NetPacketHandler, register_netpacket_handler,
    KM_MSG_HDR_PROTO_FLAG,
};

const K_EMSG_CLIENT_GAMES_PLAYED: u32 = 742;
const K_EMSG_CLIENT_GAMES_PLAYED_WITH_DATA_BLOB: u32 = 5410;
const K_EMSG_CLIENT_PERSONA_STATE: u32 = 766;
const K_EMSG_CLIENT_RICH_PRESENCE_UPLOAD: u32 = 7501;

const K_MAX_HDR_SIZE: usize = 1024;
const K_MAX_BODY_SIZE: usize = 65536;
const K_MAX_PACKET_SIZE: usize = 8 + K_MAX_HDR_SIZE + K_MAX_BODY_SIZE;

// EClientPersonaStateFlag::k_EClientPersonaStateFlagRichPresence = 0x1000
const K_STATUS_FLAG_RICH_PRESENCE: u32 = 0x1000;

/// Global state for RichPresence tracking.
struct RichPresenceState {
    /// Currently tracked AppID (topmost Lua-unlocked game).
    playing_app_id: u32,
    /// Local SteamID captured from outbound header.
    local_steam_id: u64,
    /// Cached self PersonaState header from real server push.
    self_hdr: [u8; K_MAX_HDR_SIZE],
    /// Length of cached header.
    cb_self_hdr: u32,
    /// Cached self PersonaState body from real server push.
    self_body: [u8; K_MAX_BODY_SIZE],
    /// Length of cached body.
    cb_self_body: u32,
    /// Whether we have a cached self template.
    have_self_cached: bool,
    /// Manufactured PersonaState packet ready to inject.
    inject_pkt: [u8; K_MAX_PACKET_SIZE],
    /// Length of manufactured packet.
    cb_inject_pkt: u32,
    /// Whether injection is pending.
    inject_pending: bool,
    /// Rich presence KVs per AppId.
    rp_kvs_by_app_id: HashMap<u32, Vec<(String, String)>>,
}

impl Default for RichPresenceState {
    fn default() -> Self {
        Self {
            playing_app_id: 0,
            local_steam_id: 0,
            self_hdr: [0; K_MAX_HDR_SIZE],
            cb_self_hdr: 0,
            self_body: [0; K_MAX_BODY_SIZE],
            cb_self_body: 0,
            have_self_cached: false,
            inject_pkt: [0; K_MAX_PACKET_SIZE],
            cb_inject_pkt: 0,
            inject_pending: false,
            rp_kvs_by_app_id: HashMap::new(),
        }
    }
}

static RICH_PRESENCE_STATE: LazyLock<Mutex<RichPresenceState>> = LazyLock::new(|| Mutex::new(RichPresenceState::default()));

fn state() -> &'static Mutex<RichPresenceState> {
    &RICH_PRESENCE_STATE
}

/// Walk Steam's binary KV1 stream (a top-level "RP" struct around string KVs)
/// and collect every string KV at any depth.
/// Type 0x00 starts a struct, 0x01 a string KV, 0x08 ends a struct.
/// String KVs are null-terminated key + null-terminated value.
fn extract_string_kvs(data: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut pos = 0;
    let mut depth = 0;
    let size = data.len();

    let read_cstr = |data: &[u8], pos: &mut usize| -> Option<String> {
        let start = *pos;
        while *pos < size && data[*pos] != 0 {
            *pos += 1;
        }
        if *pos >= size {
            return None;
        }
        let s = String::from_utf8(data[start..*pos].to_vec()).ok()?;
        *pos += 1;
        Some(s)
    };

    while pos < size {
        let tp = data[pos];
        pos += 1;
        if tp == 0x08 {
            if depth > 0 {
                depth -= 1;
                continue;
            }
            break;
        }
        if tp == 0x00 {
            let _ = read_cstr(data, &mut pos);
            depth += 1;
        } else if tp == 0x01 {
            let key_opt = read_cstr(data, &mut pos);
            let val_opt = read_cstr(data, &mut pos);
            if let (Some(key), Some(value)) = (key_opt, val_opt) {
                out.push((key, value));
            }
        } else {
            return out;
        }
    }
    out
}

/// Patch the self Friend entry with the appid (0 = stopped) and per-app KVs.
/// Mask status_flags's RichPresence bit (0x1000) on appid + empty KVs so a
/// freshly launched game's first inject does not wipe the UI's m_mapRichPresence,
/// which is rebuilt from rich_presence() whenever that bit is set.
fn apply_game_fields(
    entry: &mut crate::steam_bridge::interop::interop_misc::FriendEntry,
    status_flags: &mut u32,
    appid: u32,
    rp_kvs_by_app_id: &std::collections::HashMap<u32, Vec<(String, String)>>,
) {
    if appid != 0 {
        entry.game_played_app_id = Some(appid);
        entry.gameid = Some(appid as u64);
        let name = crate::steam_bridge::interop::interop_misc::get_game_name_by_app_id(appid);
        if !name.is_empty() {
            entry.game_name = Some(name);
        }
        entry.rich_presence.clear();

        let has_kvs = rp_kvs_by_app_id.get(&appid).map_or(false, |kvs| !kvs.is_empty());

        if has_kvs {
            if let Some(kvs) = rp_kvs_by_app_id.get(&appid) {
                for (k, v) in kvs {
                    entry.rich_presence.push((k.clone(), v.clone()));
                }
            }
            *status_flags |= K_STATUS_FLAG_RICH_PRESENCE;
        } else {
            *status_flags &= !K_STATUS_FLAG_RICH_PRESENCE;
        }
    } else {
        entry.game_played_app_id = None;
        entry.gameid = None;
        entry.game_name = None;
        entry.rich_presence.clear();
        *status_flags |= K_STATUS_FLAG_RICH_PRESENCE;
    }
}

/// Build the manufactured PersonaState packet for injection.
fn build_inject(appid: u32) -> bool {
    let (have_cached, local_steam_id, cb_self_hdr, cb_self_body, self_hdr, self_body) = {
        let state = state().lock().unwrap();
        (
            state.have_self_cached,
            state.local_steam_id,
            state.cb_self_hdr,
            state.cb_self_body,
            state.self_hdr,
            state.self_body,
        )
    };

    if !have_cached {
        return false;
    }

    // Parse the cached body
    let mut msg = match crate::steam_bridge::interop::interop_misc::PersonaStateMsg::parse(&self_body[..cb_self_body as usize]) {
        Some(m) => m,
        None => return false,
    };

    // Find our entry (a self-push always contains it)
    let entry_idx = match msg.friends.iter().position(|f| f.friendid == Some(local_steam_id)) {
        Some(idx) => idx,
        None => return false,
    };

    // Extract the entry first, then apply fields
    let rp_kvs_by_app_id = {
        let state = state().lock().unwrap();
        state.rp_kvs_by_app_id.clone()
    };
    {
        let entry = &mut msg.friends[entry_idx];
        apply_game_fields(entry, &mut msg.status_flags, appid, &rp_kvs_by_app_id);
    }

    let body_bytes = msg.serialize();
    let body_size = body_bytes.len();
    let hdr_size = cb_self_hdr as usize;
    let total = 8 + hdr_size + body_size; // MsgHdr + header + body

    if total > K_MAX_PACKET_SIZE || body_size > K_MAX_BODY_SIZE {
        log::warn!("RichPresence: Inject packet too large ({} bytes)", total);
        return false;
    }

    // Build the packet
    let mut inject_pkt = [0u8; K_MAX_PACKET_SIZE];
    let mhdr = crate::steam_bridge::netpacket::netpacket_core::MsgHdr {
        e_msg: (K_EMSG_CLIENT_PERSONA_STATE | KM_MSG_HDR_PROTO_FLAG),
        header_length: cb_self_hdr,
    };

    // Write MsgHdr
    inject_pkt[0..4].copy_from_slice(&mhdr.e_msg.to_le_bytes());
    inject_pkt[4..8].copy_from_slice(&mhdr.header_length.to_le_bytes());

    // Write header
    inject_pkt[8..8 + hdr_size].copy_from_slice(&self_hdr[..hdr_size]);

    // Write body
    inject_pkt[8 + hdr_size..8 + hdr_size + body_size].copy_from_slice(&body_bytes);

    let cb_inject_pkt = total as u32;

    let mut state = state().lock().unwrap();
    state.inject_pkt = inject_pkt;
    state.cb_inject_pkt = cb_inject_pkt;

    log::info!("RichPresence: Built inject for appid {} ({} bytes)", appid, cb_inject_pkt);
    true
}

/// Decode outbound CMsgClientRichPresenceUpload into per-AppId KVs
/// and stage a fresh PersonaState inject.
pub fn track_rp_send(ctx: &mut NetPacketContext) -> Result<(), String> {
    let (local_steam_id, playing_app_id) = {
        let state = state().lock().unwrap();
        (state.local_steam_id, state.playing_app_id)
    };

    if local_steam_id == 0 || playing_app_id == 0 {
        return Ok(());
    }

    // Parse CMsgClientRichPresenceUpload
    let body = ctx.body();
    if body.is_empty() {
        return Ok(());
    }

    // Simple protobuf parsing - just find field 1 (rich_presence_kv)
    // Field 1 = tag 0x0A (wire type 2 = length-delimited)
    let mut pos = 0;
    let size = body.len();

    while pos < size {
        if pos >= size { break; }
        let tag = body[pos];
        pos += 1;
        let field_num = tag >> 3;
        let wire_type = tag & 0x07;

        if field_num == 1 && wire_type == 2 {
            // length-delimited
            let mut len = 0u64;
            let mut shift = 0;
            while pos < size {
                let b = body[pos];
                pos += 1;
                len |= ((b & 0x7F) as u64) << shift;
                if (b & 0x80) == 0 { break; }
                shift += 7;
            }
            if pos + len as usize <= size {
                let kv_data = &body[pos..pos + len as usize];
                let kvs = extract_string_kvs(kv_data);
                log::debug!(
                    "RichPresence: RP upload appid={}: kv_bytes={} extracted={} pairs",
                    playing_app_id, kv_data.len(), kvs.len()
                );
                let mut state = state().lock().unwrap();
                state.rp_kvs_by_app_id.insert(playing_app_id, kvs);
                if build_inject(playing_app_id) {
                    state.inject_pending = true;
                }
            }
            return Ok(());
        }

        // Skip other fields
        match wire_type {
            0 => { /* varint */ while pos < size && (body[pos] & 0x80) != 0 { pos += 1; } pos += 1; }
            1 => { /* 64-bit */ if pos + 8 > size { return Ok(()); } pos += 8; }
            2 => { /* length-delimited */
                let mut len = 0u64;
                let mut shift = 0;
                while pos < size {
                    let b = body[pos];
                    pos += 1;
                    len |= ((b & 0x7F) as u64) << shift;
                    if (b & 0x80) == 0 { break; }
                    shift += 7;
                }
                if pos + len as usize > size { return Ok(()); }
                pos += len as usize;
            }
            5 => { /* 32-bit */ if pos + 4 > size { return Ok(()); } pos += 4; }
            _ => return Ok(()),
        }
    }

    Ok(())
}

/// Process outbound CMsgClientGamesPlayed (eMsg 742/5410).
pub fn track_send(ctx: &mut NetPacketContext) -> Result<(), String> {
    let body = ctx.body();
    if body.is_empty() {
        return Ok(());
    }

    // Capture local SteamID from header if not already captured
    {
        let mut state = state().lock().unwrap();
        if state.local_steam_id == 0 {
            // Parse header for steamid (field 1 in CMsgProtoBufHeader)
            // The header is at ctx.body_offset - 20 for protobuf
            // But we can parse from the frame buffer directly
            let frame = &ctx.frame_buffer;
            if frame.len() >= 28 { // MsgHdr(8) + jobids(16) = 24, plus header
                // Parse CMsgProtoBufHeader from frame
                let hdr_start = if ctx.is_proto { 20 } else { ctx.msg_hdr.header_length as usize };
                if hdr_start < frame.len() {
                    let hdr_data = &frame[hdr_start..];
                    // Simple parse for steamid (field 1, wire type 0 = varint)
                    let mut pos = 0;
                    while pos < hdr_data.len() {
                        let tag = hdr_data[pos];
                        pos += 1;
                        let field_num = tag >> 3;
                        let wire_type = tag & 0x07;
                        if field_num == 1 && wire_type == 0 {
                            let mut steamid = 0u64;
                            let mut shift = 0;
                            while pos < hdr_data.len() {
                                let b = hdr_data[pos];
                                pos += 1;
                                steamid |= ((b & 0x7F) as u64) << shift;
                                if (b & 0x80) == 0 { break; }
                                shift += 7;
                            }
                            if steamid != 0 {
                                state.local_steam_id = steamid;
                                log::debug!("RichPresence: Captured local SteamID 0x{:X}", steamid);
                                set_account_id((steamid & 0xFFFFFFFF) as u32);
                            }
                            break;
                        }
                        // Skip other fields
                        match wire_type {
                            0 => { while pos < hdr_data.len() && (hdr_data[pos] & 0x80) != 0 { pos += 1; } pos += 1; }
                            1 => { if pos + 8 > hdr_data.len() { break; } pos += 8; }
                            2 => {
                                let mut len = 0u64;
                                let mut shift = 0;
                                while pos < hdr_data.len() {
                                    let b = hdr_data[pos];
                                    pos += 1;
                                    len |= ((b & 0x7F) as u64) << shift;
                                    if (b & 0x80) == 0 { break; }
                                    shift += 7;
                                }
                                if pos + len as usize > hdr_data.len() { break; }
                                pos += len as usize;
                            }
                            5 => { if pos + 4 > hdr_data.len() { break; } pos += 4; }
                            _ => break,
                        }
                    }
                }
            }
        }
    }

    // Parse CMsgClientGamesPlayed to find topmost game
    // games_played is a repeated field (field 1)
    let mut topmost = 0u32;
    let mut pos = 0;
    let size = body.len();

    while pos < size {
        if pos >= size { break; }
        let tag = body[pos];
        pos += 1;
        let field_num = tag >> 3;
        let wire_type = tag & 0x07;

        if field_num == 1 && wire_type == 2 {
            // games_played entry - length-delimited
            let mut len = 0u64;
            let mut shift = 0;
            while pos < size {
                let b = body[pos];
                pos += 1;
                len |= ((b & 0x7F) as u64) << shift;
                if (b & 0x80) == 0 { break; }
                shift += 7;
            }
            let entry_start = pos;
            let entry_end = pos + len as usize;
            if entry_end <= size {
                // Parse the CMsgClientGamesPlayed_GamePlayed entry
                // game_id is field 1 (tag 0x08 = varint)
                let mut entry_pos = entry_start;
                while entry_pos < entry_end {
                    let entry_tag = body[entry_pos];
                    entry_pos += 1;
                    let entry_field = entry_tag >> 3;
                    let entry_wire = entry_tag & 0x07;
                    if entry_field == 1 && entry_wire == 0 {
                        let mut game_id = 0u64;
                        let mut shift = 0;
                        while entry_pos < entry_end {
                            let b = body[entry_pos];
                            entry_pos += 1;
                            game_id |= ((b & 0x7F) as u64) << shift;
                            if (b & 0x80) == 0 { break; }
                            shift += 7;
                        }
                        topmost = (game_id & 0xFFFFFFFF) as u32;
                    } else {
                        match entry_wire {
                            0 => { while entry_pos < entry_end && (body[entry_pos] & 0x80) != 0 { entry_pos += 1; } entry_pos += 1; }
                            1 => { if entry_pos + 8 > entry_end { break; } entry_pos += 8; }
                            2 => {
                                let mut len = 0u64;
                                let mut shift = 0;
                                while entry_pos < entry_end {
                                    let b = body[entry_pos];
                                    entry_pos += 1;
                                    len |= ((b & 0x7F) as u64) << shift;
                                    if (b & 0x80) == 0 { break; }
                                    shift += 7;
                                }
                                if entry_pos + len as usize > entry_end { break; }
                                entry_pos += len as usize;
                            }
                            5 => { if entry_pos + 4 > entry_end { break; } entry_pos += 4; }
                            _ => break,
                        }
                    }
                }
            }
            pos = entry_end;
        } else {
            match wire_type {
                0 => { while pos < size && (body[pos] & 0x80) != 0 { pos += 1; } pos += 1; }
                1 => { if pos + 8 > size { break; } pos += 8; }
                2 => {
                    let mut len = 0u64;
                    let mut shift = 0;
                    while pos < size {
                        let b = body[pos];
                        pos += 1;
                        len |= ((b & 0x7F) as u64) << shift;
                        if (b & 0x80) == 0 { break; }
                        shift += 7;
                    }
                    if pos + len as usize > size { break; }
                    pos += len as usize;
                }
                5 => { if pos + 4 > size { break; } pos += 4; }
                _ => break,
            }
        }
    }

    // Only track when topmost is an unlocked AppId we can inject for
    // Owned games on top let the server's natural broadcast paint the cache
    // -onlinefix games are already handled by the OnlineFix path
    let new_tracked = if topmost != 0 && topmost != ONLINE_FIX_APP_ID && crate::steam_bridge::interop::interop_misc::is_lua_unlocked(topmost) {
        topmost
    } else {
        0
    };

    let (old_tracked, should_notify_old, should_notify_new) = {
        let mut state = state().lock().unwrap();
        let old = state.playing_app_id;
        if old == new_tracked {
            return Ok(());
        }
        state.playing_app_id = new_tracked;
        (old, old != 0, new_tracked != 0)
    };

    if should_notify_old {
        notify_app_running(old_tracked, false);
    }
    if should_notify_new {
        notify_app_running(new_tracked, true);
    }

    if new_tracked != 0 {
        log::info!("RichPresence: Tracking topmost appid {}", new_tracked);
        if build_inject(new_tracked) {
            state().lock().unwrap().inject_pending = true;
        }
    } else if topmost == 0 {
        // Stack went empty — inject a clear so the cache reverts
        log::debug!("RichPresence: GamesPlayed empty, scheduling cache clear");
        if build_inject(0) {
            state().lock().unwrap().inject_pending = true;
        }
    } else {
        // Topmost is owned (or -onlinefix); let the server's broadcast paint it
        log::debug!("RichPresence: Topmost is appid {} (owned or onlinefix); deferring to server", topmost);
    }

    Ok(())
}

/// Cache real self-pushes as the inject template; if we are tracking
/// an unowned game, patch the live message in place so a periodic
/// refresh does not overwrite the injected game info.
pub fn handle_recv(ctx: &mut NetPacketContext) -> Result<(), String> {
    let body = ctx.body();
    if body.is_empty() {
        return Ok(());
    }

    // Parse CMsgClientPersonaState
    let mut msg = match crate::steam_bridge::interop::interop_misc::PersonaStateMsg::parse(body) {
        Some(m) => m,
        None => return Ok(()),
    };

    // Find self entry
    let local_steam_id = {
        let state = state().lock().unwrap();
        state.local_steam_id
    };

    if local_steam_id == 0 {
        return Ok(());
    }

    let self_entry_idx = msg.friends.iter().position(|f| f.friendid == Some(local_steam_id));
    if self_entry_idx.is_none() {
        return Ok(());
    }

    let self_entry_idx = self_entry_idx.unwrap();

    log::debug!(
        "RichPresence: Recv self PersonaState: status_flags=0x{:X} friends_size={}",
        msg.status_flags, msg.friends.len()
    );

    // Cache as template
    {
        let mut state = state().lock().unwrap();
        let cb_hdr = ctx.msg_hdr.header_length as usize;
        let cb_body = body.len();

        if cb_hdr <= K_MAX_HDR_SIZE && cb_body <= K_MAX_BODY_SIZE {
            // Copy header (from frame buffer)
            let hdr_start = if ctx.is_proto { 20 } else { 8 };
            let hdr_data = &ctx.frame_buffer[hdr_start..hdr_start + cb_hdr];
            state.self_hdr[..cb_hdr].copy_from_slice(hdr_data);
            state.cb_self_hdr = cb_hdr as u32;

            // Copy body
            state.self_body[..cb_body].copy_from_slice(body);
            state.cb_self_body = cb_body as u32;
            state.have_self_cached = true;
        }
    }

    // If tracking an unowned game, patch live message in-place
    let playing_app_id = {
        let state = state().lock().unwrap();
        state.playing_app_id
    };

    if playing_app_id == 0 {
        return Ok(());
    }

    // Use block to limit the borrow scope
    let appid = playing_app_id;
    {
        let entry = &mut msg.friends[self_entry_idx];
        let rp_kvs_by_app_id = {
            let state = state().lock().unwrap();
            state.rp_kvs_by_app_id.clone()
        };
        apply_game_fields(entry, &mut msg.status_flags, appid, &rp_kvs_by_app_id);
    }

    let body_bytes = msg.serialize();
    let body_size = body_bytes.len();

    if body_size > K_MAX_BODY_SIZE {
        log::warn!("RichPresence: In-place patch too large ({} bytes)", body_size);
        return Ok(());
    }

    // Replace body in context
    ctx.truncate(ctx.body_offset);
    ctx.extend(&body_bytes);

    log::info!("RichPresence: Patched live self push with appid {}", playing_app_id);
    Ok(())
}

/// Deliver the pending manufactured PersonaState by borrowing the
/// carrier's data pointer for one RecvPkt call, then restore.
pub fn try_inject(ctx: &mut NetPacketContext) -> Result<(), String> {
    let (inject_pending, cb_inject_pkt, inject_pkt) = {
        let mut state = state().lock().unwrap();
        if !state.inject_pending || state.cb_inject_pkt == 0 {
            return Ok(());
        }
        state.inject_pending = false;
        (true, state.cb_inject_pkt, state.inject_pkt)
    };

    if !inject_pending {
        return Ok(());
    }

    // Replace the entire frame with our manufactured packet
    let new_frame = inject_pkt[..cb_inject_pkt as usize].to_vec();
    ctx.replace_frame(new_frame);

    log::info!("RichPresence: Delivered manufactured self-PersonaState ({} bytes)", cb_inject_pkt);
    Ok(())
}

/// Register RichPresence NetPacket handlers.
pub fn register_richpresence_handlers() {
    // Outgoing: CMsgClientGamesPlayed (eMsg 742)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: K_EMSG_CLIENT_GAMES_PLAYED,
        target_job_hash: None,
        name: "RichPresence::HandleSend_GamesPlayed_742".to_string(),
        handler: Arc::new(|ctx| track_send(ctx)),
    });

    // Outgoing: CMsgClientGamesPlayedWithDataBlob (eMsg 5410)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: K_EMSG_CLIENT_GAMES_PLAYED_WITH_DATA_BLOB,
        target_job_hash: None,
        name: "RichPresence::HandleSend_GamesPlayed_5410".to_string(),
        handler: Arc::new(|ctx| track_send(ctx)),
    });

    // Outgoing: CMsgClientRichPresenceUpload (eMsg 7501)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: K_EMSG_CLIENT_RICH_PRESENCE_UPLOAD,
        target_job_hash: None,
        name: "RichPresence::HandleSend_RichPresenceUpload".to_string(),
        handler: Arc::new(|ctx| track_rp_send(ctx)),
    });

    // Incoming: CMsgClientPersonaState (eMsg 766)
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Recv,
        emsg: K_EMSG_CLIENT_PERSONA_STATE,
        target_job_hash: None,
        name: "RichPresence::HandleRecv_PersonaState".to_string(),
        handler: Arc::new(|ctx| {
            handle_recv(ctx)?;
            try_inject(ctx)
        }),
    });
}

/// Initialize RichPresence module (called from core installer).
pub fn install_richpresence_hooks() -> Result<(), String> {
    register_richpresence_handlers();
    log::info!("Installed RichPresence NetPacket hooks");
    Ok(())
}

/// Uninstall RichPresence module.
pub fn uninstall_richpresence_hooks() {
    // Handlers are cleared when core dispatcher unregisters all
    let mut state = state().lock().unwrap();
    *state = RichPresenceState::default();
    log::info!("Uninstalled RichPresence NetPacket hooks");
}

/// Check if RichPresence hooks are active.
pub fn is_richpresence_hook_active() -> bool {
    // We can't easily check this without exposing NETPACKET_HANDLERS
    // Just return true if handlers were registered (this is a best effort)
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_string_kvs_simple() {
        // Binary KV1: 0x00 "RP" 0x01 "key" "value" 0x08
        let data = vec![
            0x00, b'R', b'P', 0x00,
            0x01, b'k', b'e', b'y', 0x00, b'v', b'a', b'l', b'u', b'e', 0x00,
            0x08
        ];
        let kvs = extract_string_kvs(&data);
        assert_eq!(kvs.len(), 1);
        assert_eq!(kvs[0], ("key".to_string(), "value".to_string()));
    }

    #[test]
    fn test_extract_string_kvs_multiple() {
        let data = vec![
            0x00, b'R', b'P', 0x00,
            0x01, b'k', b'e', b'y', b'1', 0x00, b'v', b'a', b'l', b'1', 0x00,
            0x01, b'k', b'e', b'y', b'2', 0x00, b'v', b'a', b'l', b'2', 0x00,
            0x08
        ];
        let kvs = extract_string_kvs(&data);
        assert_eq!(kvs.len(), 2);
        assert_eq!(kvs[0], ("key1".to_string(), "val1".to_string()));
        assert_eq!(kvs[1], ("key2".to_string(), "val2".to_string()));
    }

    #[test]
    fn test_extract_string_kvs_nested() {
        // Nested struct
        let data = vec![
            0x00, b'R', b'P', 0x00,
            0x00, b'N', b'E', b'S', b'T', 0x00,
            0x01, b'n', b'k', 0x00, b'n', b'v', 0x00,
            0x08, // end NEST
            0x01, b'k', 0x00, b'v', 0x00,
            0x08
        ];
        let kvs = extract_string_kvs(&data);
        assert_eq!(kvs.len(), 2);
        assert_eq!(kvs[0], ("nk".to_string(), "nv".to_string()));
        assert_eq!(kvs[1], ("k".to_string(), "v".to_string()));
    }

    #[test]
    fn test_constants() {
        assert_eq!(K_EMSG_CLIENT_GAMES_PLAYED, 742);
        assert_eq!(K_EMSG_CLIENT_GAMES_PLAYED_WITH_DATA_BLOB, 5410);
        assert_eq!(K_EMSG_CLIENT_PERSONA_STATE, 766);
        assert_eq!(K_EMSG_CLIENT_RICH_PRESENCE_UPLOAD, 7501);
        assert_eq!(K_STATUS_FLAG_RICH_PRESENCE, 0x1000);
    }
}