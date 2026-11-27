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

use crate::steam_bridge::cloud_redirect::{notify_app_running, set_account_id};
use crate::steam_bridge::interop::interop_misc::{
    ClanData, FriendEntry, OtherGameData, PersonaStateMsg,
};
use crate::steam_bridge::interop::ONLINE_FIX_APP_ID;
use crate::steam_bridge::netpacket::netpacket_core::{
    register_netpacket_handler, NetPacketContext, NetPacketDirection, NetPacketHandler,
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

// ============================================================================
// Standard LEB128 Protobuf Wire Helpers & PersonaStateMsg Codec (BST-35)
// ============================================================================

/// Encode an unsigned integer as standard Protobuf LEB128 varint bytes.
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

/// Decode a standard Protobuf LEB128 varint, returning `(value, bytes_consumed)`.
pub fn decode_varint(data: &[u8]) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    for (idx, &byte) in data.iter().enumerate() {
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

/// Encode a Protobuf field tag `((field_num << 3) | wire_type)` using LEB128 varint encoding.
///
/// Any field number `>= 16` produces a multi-byte varint (`[((tag & 0x7F) | 0x80), (tag >> 7), ...]`).
pub fn encode_tag(field_num: u32, wire_type: u8, out: &mut Vec<u8>) {
    let tag = (u64::from(field_num) << 3) | u64::from(wire_type & 0x07);
    encode_varint(tag, out);
}

/// Decode a Protobuf field tag varint, returning `(field_num, wire_type, bytes_consumed)`.
pub fn decode_tag(data: &[u8]) -> Option<(u32, u8, usize)> {
    let (raw, consumed) = decode_varint(data)?;
    let field_num = u32::try_from(raw >> 3).ok()?;
    if field_num == 0 {
        return None;
    }
    let wire_type = (raw & 0x07) as u8;
    Some((field_num, wire_type, consumed))
}

fn skip_wire_field(data: &[u8], wire_type: u8) -> Option<usize> {
    match wire_type {
        0 => decode_varint(data).map(|(_, n)| n),
        1 => (data.len() >= 8).then_some(8),
        2 => {
            let (len, n) = decode_varint(data)?;
            let len_usize = usize::try_from(len).ok()?;
            let total = n.checked_add(len_usize)?;
            (total <= data.len()).then_some(total)
        }
        5 => (data.len() >= 4).then_some(4),
        _ => None,
    }
}

fn encode_varint_field(field_num: u32, val: u64, out: &mut Vec<u8>) {
    encode_tag(field_num, 0, out);
    encode_varint(val, out);
}

fn encode_fixed64_field(field_num: u32, val: u64, out: &mut Vec<u8>) {
    encode_tag(field_num, 1, out);
    out.extend_from_slice(&val.to_le_bytes());
}

fn encode_bytes_field(field_num: u32, bytes: &[u8], out: &mut Vec<u8>) {
    encode_tag(field_num, 2, out);
    encode_varint(bytes.len() as u64, out);
    out.extend_from_slice(bytes);
}

fn encode_fixed32_field(field_num: u32, val: u32, out: &mut Vec<u8>) {
    encode_tag(field_num, 5, out);
    out.extend_from_slice(&val.to_le_bytes());
}

fn read_length_delimited(data: &[u8]) -> Option<(&[u8], usize)> {
    let (len, n) = decode_varint(data)?;
    let len_usize = usize::try_from(len).ok()?;
    let end = n.checked_add(len_usize)?;
    if end > data.len() {
        return None;
    }
    Some((&data[n..end], end))
}

fn parse_kv_msg(data: &[u8]) -> Option<(String, String)> {
    let mut key = String::new();
    let mut value = String::new();
    let mut pos = 0;

    while pos < data.len() {
        let (field_num, wire_type, consumed) = decode_tag(&data[pos..])?;
        pos += consumed;
        match (field_num, wire_type) {
            (1, 2) => {
                let (slice, n) = read_length_delimited(&data[pos..])?;
                key = std::str::from_utf8(slice).ok()?.to_string();
                pos += n;
            }
            (2, 2) => {
                let (slice, n) = read_length_delimited(&data[pos..])?;
                value = std::str::from_utf8(slice).ok()?.to_string();
                pos += n;
            }
            _ => {
                pos += skip_wire_field(&data[pos..], wire_type)?;
            }
        }
    }

    if key.is_empty() {
        None
    } else {
        Some((key, value))
    }
}

fn serialize_kv_msg(key: &str, value: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(key.len() + value.len() + 8);
    encode_bytes_field(1, key.as_bytes(), &mut out);
    encode_bytes_field(2, value.as_bytes(), &mut out);
    out
}

fn parse_clan_data_msg(data: &[u8]) -> Option<ClanData> {
    let mut clan = ClanData::default();
    let mut pos = 0;

    while pos < data.len() {
        let (field_num, wire_type, consumed) = decode_tag(&data[pos..])?;
        pos += consumed;
        match (field_num, wire_type) {
            (1, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                clan.ogg_app_id = Some(val as u32);
                pos += n;
            }
            (2, 1) => {
                if pos + 8 > data.len() {
                    return None;
                }
                clan.chat_group_id =
                    Some(u64::from_le_bytes(data[pos..pos + 8].try_into().ok()?));
                pos += 8;
            }
            (2, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                clan.chat_group_id = Some(val);
                pos += n;
            }
            _ => {
                pos += skip_wire_field(&data[pos..], wire_type)?;
            }
        }
    }

    Some(clan)
}

fn serialize_clan_data_msg(clan: &ClanData) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    if let Some(v) = clan.ogg_app_id {
        encode_varint_field(1, u64::from(v), &mut out);
    }
    if let Some(v) = clan.chat_group_id {
        encode_fixed64_field(2, v, &mut out);
    }
    out
}

fn parse_other_game_data_msg(data: &[u8]) -> Option<OtherGameData> {
    let mut other = OtherGameData::default();
    let mut pos = 0;

    while pos < data.len() {
        let (field_num, wire_type, consumed) = decode_tag(&data[pos..])?;
        pos += consumed;
        match (field_num, wire_type) {
            (1, 1) => {
                if pos + 8 > data.len() {
                    return None;
                }
                other.gameid = Some(u64::from_le_bytes(data[pos..pos + 8].try_into().ok()?));
                pos += 8;
            }
            (1, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                other.gameid = Some(val);
                pos += n;
            }
            (2, 2) => {
                let (slice, n) = read_length_delimited(&data[pos..])?;
                if let Some(kv) = parse_kv_msg(slice) {
                    other.rich_presence.push(kv);
                }
                pos += n;
            }
            _ => {
                pos += skip_wire_field(&data[pos..], wire_type)?;
            }
        }
    }

    Some(other)
}

fn serialize_other_game_data_msg(other: &OtherGameData) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    if let Some(v) = other.gameid {
        encode_fixed64_field(1, v, &mut out);
    }
    for (k, v) in &other.rich_presence {
        let kv_bytes = serialize_kv_msg(k, v);
        encode_bytes_field(2, &kv_bytes, &mut out);
    }
    out
}

fn parse_friend_entry(data: &[u8]) -> Option<FriendEntry> {
    let mut entry = FriendEntry::default();
    let mut pos = 0;

    while pos < data.len() {
        let (field_num, wire_type, consumed) = decode_tag(&data[pos..])?;
        pos += consumed;

        match (field_num, wire_type) {
            (1, 1) => {
                if pos + 8 > data.len() {
                    return None;
                }
                entry.friendid = Some(u64::from_le_bytes(data[pos..pos + 8].try_into().ok()?));
                pos += 8;
            }
            (1, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.friendid = Some(val);
                pos += n;
            }
            (2, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.persona_state = Some(val as u32);
                pos += n;
            }
            (3, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.game_played_app_id = Some(val as u32);
                pos += n;
            }
            (4, 5) => {
                if pos + 4 > data.len() {
                    return None;
                }
                entry.game_server_ip =
                    Some(u32::from_le_bytes(data[pos..pos + 4].try_into().ok()?));
                pos += 4;
            }
            (4, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.game_server_ip = Some(val as u32);
                pos += n;
            }
            (5, 5) => {
                if pos + 4 > data.len() {
                    return None;
                }
                entry.game_server_port =
                    Some(u32::from_le_bytes(data[pos..pos + 4].try_into().ok()?));
                pos += 4;
            }
            (5, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.game_server_port = Some(val as u32);
                pos += n;
            }
            (6, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.persona_state_flags = Some(val as u32);
                pos += n;
            }
            (7, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.online_session_instances = Some(val as u32);
                pos += n;
            }
            (10, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.persona_set_by_user = Some(val != 0);
                pos += n;
            }
            (15, 2) => {
                let (slice, n) = read_length_delimited(&data[pos..])?;
                entry.player_name = std::str::from_utf8(slice).ok().map(ToString::to_string);
                pos += n;
            }
            (20, 5) => {
                if pos + 4 > data.len() {
                    return None;
                }
                entry.query_port = Some(u32::from_le_bytes(data[pos..pos + 4].try_into().ok()?));
                pos += 4;
            }
            (20, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.query_port = Some(val as u32);
                pos += n;
            }
            (25, 1) => {
                if pos + 8 > data.len() {
                    return None;
                }
                entry.steamid_source =
                    Some(u64::from_le_bytes(data[pos..pos + 8].try_into().ok()?));
                pos += 8;
            }
            (25, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.steamid_source = Some(val);
                pos += n;
            }
            (31, 2) => {
                let (slice, n) = read_length_delimited(&data[pos..])?;
                entry.avatar_hash = Some(slice.to_vec());
                pos += n;
            }
            (45, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.last_logoff = Some(val as u32);
                pos += n;
            }
            (46, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.last_logon = Some(val as u32);
                pos += n;
            }
            (47, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.last_seen_online = Some(val as u32);
                pos += n;
            }
            (50, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.clan_rank = Some(val as u32);
                pos += n;
            }
            (55, 2) => {
                let (slice, n) = read_length_delimited(&data[pos..])?;
                entry.game_name = std::str::from_utf8(slice).ok().map(ToString::to_string);
                pos += n;
            }
            (56, 1) => {
                if pos + 8 > data.len() {
                    return None;
                }
                entry.gameid = Some(u64::from_le_bytes(data[pos..pos + 8].try_into().ok()?));
                pos += 8;
            }
            (56, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.gameid = Some(val);
                pos += n;
            }
            (60, 2) => {
                let (slice, n) = read_length_delimited(&data[pos..])?;
                entry.game_data_blob = Some(slice.to_vec());
                pos += n;
            }
            (64, 2) => {
                let (slice, n) = read_length_delimited(&data[pos..])?;
                entry.clan_data = parse_clan_data_msg(slice);
                pos += n;
            }
            (65, 2) => {
                let (slice, n) = read_length_delimited(&data[pos..])?;
                entry.clan_tag = std::str::from_utf8(slice).ok().map(ToString::to_string);
                pos += n;
            }
            (71, 2) => {
                let (slice, n) = read_length_delimited(&data[pos..])?;
                if let Some(kv) = parse_kv_msg(slice) {
                    entry.rich_presence.push(kv);
                }
                pos += n;
            }
            (72, 1) => {
                if pos + 8 > data.len() {
                    return None;
                }
                entry.broadcast_id = Some(u64::from_le_bytes(data[pos..pos + 8].try_into().ok()?));
                pos += 8;
            }
            (72, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.broadcast_id = Some(val);
                pos += n;
            }
            (73, 1) => {
                if pos + 8 > data.len() {
                    return None;
                }
                entry.game_lobby_id = Some(u64::from_le_bytes(data[pos..pos + 8].try_into().ok()?));
                pos += 8;
            }
            (73, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.game_lobby_id = Some(val);
                pos += n;
            }
            (74, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.watching_broadcast_accountid = Some(val as u32);
                pos += n;
            }
            (75, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.watching_broadcast_appid = Some(val as u32);
                pos += n;
            }
            (76, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.watching_broadcast_viewers = Some(val as u32);
                pos += n;
            }
            (77, 2) => {
                let (slice, n) = read_length_delimited(&data[pos..])?;
                entry.watching_broadcast_title =
                    std::str::from_utf8(slice).ok().map(ToString::to_string);
                pos += n;
            }
            (78, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.is_community_banned = Some(val != 0);
                pos += n;
            }
            (79, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.player_name_pending_review = Some(val != 0);
                pos += n;
            }
            (80, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.avatar_pending_review = Some(val != 0);
                pos += n;
            }
            (81, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.on_steam_deck = Some(val != 0);
                pos += n;
            }
            (82, 2) => {
                let (slice, n) = read_length_delimited(&data[pos..])?;
                if let Some(other) = parse_other_game_data_msg(slice) {
                    entry.other_game_data.push(other);
                }
                pos += n;
            }
            (83, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                entry.gaming_device_type = Some(val as u32);
                pos += n;
            }
            _ => {
                pos += skip_wire_field(&data[pos..], wire_type)?;
            }
        }
    }

    Some(entry)
}

fn serialize_friend_entry(entry: &FriendEntry) -> Vec<u8> {
    let mut out = Vec::with_capacity(128);

    if let Some(v) = entry.friendid {
        encode_fixed64_field(1, v, &mut out);
    }
    if let Some(v) = entry.persona_state {
        encode_varint_field(2, u64::from(v), &mut out);
    }
    if let Some(v) = entry.game_played_app_id {
        encode_varint_field(3, u64::from(v), &mut out);
    }
    if let Some(v) = entry.game_server_ip {
        encode_fixed32_field(4, v, &mut out);
    }
    if let Some(v) = entry.game_server_port {
        encode_fixed32_field(5, v, &mut out);
    }
    if let Some(v) = entry.persona_state_flags {
        encode_varint_field(6, u64::from(v), &mut out);
    }
    if let Some(v) = entry.online_session_instances {
        encode_varint_field(7, u64::from(v), &mut out);
    }
    if let Some(v) = entry.persona_set_by_user {
        encode_varint_field(10, u64::from(v), &mut out);
    }
    if let Some(ref v) = entry.player_name {
        encode_bytes_field(15, v.as_bytes(), &mut out);
    }
    if let Some(v) = entry.query_port {
        encode_fixed32_field(20, v, &mut out);
    }
    if let Some(v) = entry.steamid_source {
        encode_fixed64_field(25, v, &mut out);
    }
    if let Some(ref v) = entry.avatar_hash {
        encode_bytes_field(31, v, &mut out);
    }
    if let Some(v) = entry.last_logoff {
        encode_varint_field(45, u64::from(v), &mut out);
    }
    if let Some(v) = entry.last_logon {
        encode_varint_field(46, u64::from(v), &mut out);
    }
    if let Some(v) = entry.last_seen_online {
        encode_varint_field(47, u64::from(v), &mut out);
    }
    if let Some(v) = entry.clan_rank {
        encode_varint_field(50, u64::from(v), &mut out);
    }
    if let Some(ref v) = entry.game_name {
        encode_bytes_field(55, v.as_bytes(), &mut out);
    }
    if let Some(v) = entry.gameid {
        encode_fixed64_field(56, v, &mut out);
    }
    if let Some(ref v) = entry.game_data_blob {
        encode_bytes_field(60, v, &mut out);
    }
    if let Some(ref v) = entry.clan_data {
        let clan_bytes = serialize_clan_data_msg(v);
        encode_bytes_field(64, &clan_bytes, &mut out);
    }
    if let Some(ref v) = entry.clan_tag {
        encode_bytes_field(65, v.as_bytes(), &mut out);
    }
    for (k, v) in &entry.rich_presence {
        let kv_bytes = serialize_kv_msg(k, v);
        encode_bytes_field(71, &kv_bytes, &mut out);
    }
    if let Some(v) = entry.broadcast_id {
        encode_fixed64_field(72, v, &mut out);
    }
    if let Some(v) = entry.game_lobby_id {
        encode_fixed64_field(73, v, &mut out);
    }
    if let Some(v) = entry.watching_broadcast_accountid {
        encode_varint_field(74, u64::from(v), &mut out);
    }
    if let Some(v) = entry.watching_broadcast_appid {
        encode_varint_field(75, u64::from(v), &mut out);
    }
    if let Some(v) = entry.watching_broadcast_viewers {
        encode_varint_field(76, u64::from(v), &mut out);
    }
    if let Some(ref v) = entry.watching_broadcast_title {
        encode_bytes_field(77, v.as_bytes(), &mut out);
    }
    if let Some(v) = entry.is_community_banned {
        encode_varint_field(78, u64::from(v), &mut out);
    }
    if let Some(v) = entry.player_name_pending_review {
        encode_varint_field(79, u64::from(v), &mut out);
    }
    if let Some(v) = entry.avatar_pending_review {
        encode_varint_field(80, u64::from(v), &mut out);
    }
    if let Some(v) = entry.on_steam_deck {
        encode_varint_field(81, u64::from(v), &mut out);
    }
    for other in &entry.other_game_data {
        let other_bytes = serialize_other_game_data_msg(other);
        encode_bytes_field(82, &other_bytes, &mut out);
    }
    if let Some(v) = entry.gaming_device_type {
        encode_varint_field(83, u64::from(v), &mut out);
    }

    out
}

/// Parse `CMsgClientPersonaState` using spec-valid multi-byte LEB128 Protobuf tags.
pub fn parse_persona_state_msg(data: &[u8]) -> Option<PersonaStateMsg> {
    let mut msg = PersonaStateMsg::default();
    let mut pos = 0;

    while pos < data.len() {
        let (field_num, wire_type, consumed) = decode_tag(&data[pos..])?;
        pos += consumed;

        match (field_num, wire_type) {
            (1, 0) => {
                let (val, n) = decode_varint(&data[pos..])?;
                msg.status_flags = val as u32;
                pos += n;
            }
            (2, 2) => {
                let (slice, n) = read_length_delimited(&data[pos..])?;
                if let Some(friend) = parse_friend_entry(slice) {
                    msg.friends.push(friend);
                }
                pos += n;
            }
            _ => {
                pos += skip_wire_field(&data[pos..], wire_type)?;
            }
        }
    }

    Some(msg)
}

/// Serialize `CMsgClientPersonaState` using spec-valid multi-byte LEB128 Protobuf tags.
pub fn serialize_persona_state_msg(msg: &PersonaStateMsg) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + msg.friends.len() * 128);
    encode_varint_field(1, u64::from(msg.status_flags), &mut out);
    for friend in &msg.friends {
        let friend_bytes = serialize_friend_entry(friend);
        encode_bytes_field(2, &friend_bytes, &mut out);
    }
    out
}

/// Extract `(proto_hdr_slice, proto_body_slice)` safely from a `NetPacketContext`.
fn split_proto_frame(ctx: &NetPacketContext) -> (&[u8], &[u8]) {
    if ctx.is_proto && ctx.frame_buffer.len() >= 8 {
        let hdr_len = u32::from_le_bytes([
            ctx.frame_buffer[4],
            ctx.frame_buffer[5],
            ctx.frame_buffer[6],
            ctx.frame_buffer[7],
        ]) as usize;
        if 8usize.saturating_add(hdr_len) <= ctx.frame_buffer.len() {
            return (
                &ctx.frame_buffer[8..8 + hdr_len],
                &ctx.frame_buffer[8 + hdr_len..],
            );
        }
    }
    let body_off = ctx.body_offset.min(ctx.frame_buffer.len());
    let hdr_start = 8usize.min(body_off);
    (
        &ctx.frame_buffer[hdr_start..body_off],
        &ctx.frame_buffer[body_off..],
    )
}

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

static RICH_PRESENCE_STATE: LazyLock<Mutex<RichPresenceState>> =
    LazyLock::new(|| Mutex::new(RichPresenceState::default()));

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
    entry: &mut FriendEntry,
    status_flags: &mut u32,
    appid: u32,
    rp_kvs_by_app_id: &HashMap<u32, Vec<(String, String)>>,
) {
    if appid != 0 {
        entry.game_played_app_id = Some(appid);
        entry.gameid = Some(u64::from(appid));
        let name = crate::steam_bridge::interop::interop_misc::get_game_name_by_app_id(appid);
        if !name.is_empty() {
            entry.game_name = Some(name);
        }
        entry.rich_presence.clear();

        let has_kvs = rp_kvs_by_app_id
            .get(&appid)
            .map_or(false, |kvs| !kvs.is_empty());

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
    let (have_cached, local_steam_id, cb_self_hdr, cb_self_body, self_hdr, self_body, rp_kvs) = {
        let state = state().lock().unwrap_or_else(|e| e.into_inner());
        (
            state.have_self_cached,
            state.local_steam_id,
            state.cb_self_hdr,
            state.cb_self_body,
            state.self_hdr,
            state.self_body,
            state.rp_kvs_by_app_id.clone(),
        )
    };

    if !have_cached {
        return false;
    }

    let mut msg = match parse_persona_state_msg(&self_body[..cb_self_body as usize]) {
        Some(m) => m,
        None => return false,
    };

    let entry_idx = match msg
        .friends
        .iter()
        .position(|f| f.friendid == Some(local_steam_id))
    {
        Some(idx) => idx,
        None => return false,
    };

    {
        let entry = &mut msg.friends[entry_idx];
        apply_game_fields(entry, &mut msg.status_flags, appid, &rp_kvs);
    }

    let body_bytes = serialize_persona_state_msg(&msg);
    let body_size = body_bytes.len();
    let hdr_size = cb_self_hdr as usize;
    let total = 8 + hdr_size + body_size;

    if total > K_MAX_PACKET_SIZE || body_size > K_MAX_BODY_SIZE {
        log::warn!("RichPresence: Inject packet too large ({} bytes)", total);
        return false;
    }

    let mut inject_pkt = [0u8; K_MAX_PACKET_SIZE];
    let e_msg = K_EMSG_CLIENT_PERSONA_STATE | KM_MSG_HDR_PROTO_FLAG;

    inject_pkt[0..4].copy_from_slice(&e_msg.to_le_bytes());
    inject_pkt[4..8].copy_from_slice(&cb_self_hdr.to_le_bytes());
    inject_pkt[8..8 + hdr_size].copy_from_slice(&self_hdr[..hdr_size]);
    inject_pkt[8 + hdr_size..8 + hdr_size + body_size].copy_from_slice(&body_bytes);

    let cb_inject_pkt = total as u32;

    let mut state = state().lock().unwrap_or_else(|e| e.into_inner());
    state.inject_pkt = inject_pkt;
    state.cb_inject_pkt = cb_inject_pkt;

    log::info!(
        "RichPresence: Built inject for appid {} ({} bytes)",
        appid,
        cb_inject_pkt
    );
    true
}

/// Decode outbound CMsgClientRichPresenceUpload into per-AppId KVs
/// and stage a fresh PersonaState inject.
pub fn track_rp_send(ctx: &mut NetPacketContext) -> Result<(), String> {
    let (local_steam_id, playing_app_id) = {
        let state = state().lock().unwrap_or_else(|e| e.into_inner());
        (state.local_steam_id, state.playing_app_id)
    };

    if local_steam_id == 0 || playing_app_id == 0 {
        return Ok(());
    }

    let (_, body) = split_proto_frame(ctx);
    if body.is_empty() {
        return Ok(());
    }

    // Parse CMsgClientRichPresenceUpload (field 1 = rich_presence_kv bytes)
    let mut pos = 0;
    while pos < body.len() {
        let Some((field_num, wire_type, consumed)) = decode_tag(&body[pos..]) else {
            break;
        };
        pos += consumed;

        if field_num == 1 && wire_type == 2 {
            let Some((kv_data, _)) = read_length_delimited(&body[pos..]) else {
                break;
            };
            let kvs = extract_string_kvs(kv_data);
            log::debug!(
                "RichPresence: RP upload appid={}: kv_bytes={} extracted={} pairs",
                playing_app_id,
                kv_data.len(),
                kvs.len()
            );
            {
                let mut state = state().lock().unwrap_or_else(|e| e.into_inner());
                state.rp_kvs_by_app_id.insert(playing_app_id, kvs);
            }
            if build_inject(playing_app_id) {
                state()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .inject_pending = true;
            }
            return Ok(());
        }

        let Some(skipped) = skip_wire_field(&body[pos..], wire_type) else {
            break;
        };
        pos += skipped;
    }

    Ok(())
}

/// Extract `steamid` (field 1, fixed64 or varint) from `CMsgProtoBufHeader`.
fn parse_header_steam_id(hdr_data: &[u8]) -> Option<u64> {
    let mut pos = 0;
    while pos < hdr_data.len() {
        let (field_num, wire_type, consumed) = decode_tag(&hdr_data[pos..])?;
        pos += consumed;
        match (field_num, wire_type) {
            (1, 1) => {
                if pos + 8 > hdr_data.len() {
                    return None;
                }
                let sid = u64::from_le_bytes(hdr_data[pos..pos + 8].try_into().ok()?);
                return (sid != 0).then_some(sid);
            }
            (1, 0) => {
                let (sid, _) = decode_varint(&hdr_data[pos..])?;
                return (sid != 0).then_some(sid);
            }
            _ => {
                pos += skip_wire_field(&hdr_data[pos..], wire_type)?;
            }
        }
    }
    None
}

/// Process outbound CMsgClientGamesPlayed (eMsg 742/5410).
pub fn track_send(ctx: &mut NetPacketContext) -> Result<(), String> {
    let (hdr_data, body) = split_proto_frame(ctx);
    if body.is_empty() {
        return Ok(());
    }

    // Capture local SteamID from header if not already captured
    {
        let mut state = state().lock().unwrap_or_else(|e| e.into_inner());
        if state.local_steam_id == 0 {
            if let Some(steamid) = parse_header_steam_id(hdr_data) {
                state.local_steam_id = steamid;
                log::debug!("RichPresence: Captured local SteamID 0x{:X}", steamid);
                set_account_id((steamid & 0xFFFF_FFFF) as u32);
            }
        }
    }

    // Parse CMsgClientGamesPlayed to find topmost game (games_played = repeated field 1)
    let mut topmost = 0u32;
    let mut pos = 0;

    while pos < body.len() {
        let Some((field_num, wire_type, consumed)) = decode_tag(&body[pos..]) else {
            break;
        };
        pos += consumed;

        if field_num == 1 && wire_type == 2 {
            let Some((entry_slice, n)) = read_length_delimited(&body[pos..]) else {
                break;
            };
            pos += n;

            // Parse CMsgClientGamesPlayed_GamePlayed (game_id = field 2, uint64/fixed64)
            let mut entry_pos = 0;
            while entry_pos < entry_slice.len() {
                let Some((entry_field, entry_wire, e_consumed)) =
                    decode_tag(&entry_slice[entry_pos..])
                else {
                    break;
                };
                entry_pos += e_consumed;
                match (entry_field, entry_wire) {
                    (2, 0) => {
                        let Some((game_id, gn)) = decode_varint(&entry_slice[entry_pos..]) else {
                            break;
                        };
                        entry_pos += gn;
                        topmost = (game_id & 0xFFFF_FFFF) as u32;
                    }
                    (2, 1) => {
                        if entry_pos + 8 > entry_slice.len() {
                            break;
                        }
                        let game_id = u64::from_le_bytes(
                            entry_slice[entry_pos..entry_pos + 8]
                                .try_into()
                                .unwrap_or([0; 8]),
                        );
                        entry_pos += 8;
                        topmost = (game_id & 0xFFFF_FFFF) as u32;
                    }
                    _ => {
                        let Some(sk) = skip_wire_field(&entry_slice[entry_pos..], entry_wire)
                        else {
                            break;
                        };
                        entry_pos += sk;
                    }
                }
            }
        } else {
            let Some(sk) = skip_wire_field(&body[pos..], wire_type) else {
                break;
            };
            pos += sk;
        }
    }

    // Only track when topmost is an unlocked AppId we can inject for
    let new_tracked = if topmost != 0
        && topmost != ONLINE_FIX_APP_ID
        && crate::steam_bridge::interop::interop_misc::is_lua_unlocked(topmost)
    {
        topmost
    } else {
        0
    };

    let (old_tracked, should_notify_old, should_notify_new) = {
        let mut state = state().lock().unwrap_or_else(|e| e.into_inner());
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
            state()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .inject_pending = true;
        }
    } else if topmost == 0 {
        log::debug!("RichPresence: GamesPlayed empty, scheduling cache clear");
        if build_inject(0) {
            state()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .inject_pending = true;
        }
    } else {
        log::debug!(
            "RichPresence: Topmost is appid {} (owned or onlinefix); deferring to server",
            topmost
        );
    }

    Ok(())
}

/// Cache real self-pushes as the inject template; if we are tracking
/// an unowned game, patch the live message in place so a periodic
/// refresh does not overwrite the injected game info.
pub fn handle_recv(ctx: &mut NetPacketContext) -> Result<(), String> {
    let (hdr_slice, body_slice) = split_proto_frame(ctx);
    let hdr_bytes = hdr_slice.to_vec();
    let body = body_slice.to_vec();
    if body.is_empty() {
        return Ok(());
    }

    let mut msg = match parse_persona_state_msg(&body) {
        Some(m) => m,
        None => return Ok(()),
    };

    let local_steam_id = {
        let state = state().lock().unwrap_or_else(|e| e.into_inner());
        state.local_steam_id
    };

    if local_steam_id == 0 {
        return Ok(());
    }

    let Some(self_entry_idx) = msg
        .friends
        .iter()
        .position(|f| f.friendid == Some(local_steam_id))
    else {
        return Ok(());
    };

    log::debug!(
        "RichPresence: Recv self PersonaState: status_flags=0x{:X} friends_size={}",
        msg.status_flags,
        msg.friends.len()
    );

    // Cache as template
    {
        let mut state = state().lock().unwrap_or_else(|e| e.into_inner());
        let cb_hdr = hdr_bytes.len();
        let cb_body = body.len();

        if cb_hdr <= K_MAX_HDR_SIZE && cb_body <= K_MAX_BODY_SIZE {
            state.self_hdr[..cb_hdr].copy_from_slice(&hdr_bytes);
            state.cb_self_hdr = cb_hdr as u32;
            state.self_body[..cb_body].copy_from_slice(&body);
            state.cb_self_body = cb_body as u32;
            state.have_self_cached = true;
        }
    }

    let playing_app_id = {
        let state = state().lock().unwrap_or_else(|e| e.into_inner());
        state.playing_app_id
    };

    if playing_app_id == 0 {
        return Ok(());
    }

    let rp_kvs_by_app_id = {
        let state = state().lock().unwrap_or_else(|e| e.into_inner());
        state.rp_kvs_by_app_id.clone()
    };
    {
        let entry = &mut msg.friends[self_entry_idx];
        apply_game_fields(
            entry,
            &mut msg.status_flags,
            playing_app_id,
            &rp_kvs_by_app_id,
        );
    }

    let body_bytes = serialize_persona_state_msg(&msg);
    let body_size = body_bytes.len();

    if body_size > K_MAX_BODY_SIZE {
        log::warn!(
            "RichPresence: In-place patch too large ({} bytes)",
            body_size
        );
        return Ok(());
    }

    let body_start = if ctx.is_proto && 8usize.saturating_add(hdr_bytes.len()) <= ctx.frame_buffer.len()
    {
        8 + hdr_bytes.len()
    } else {
        ctx.body_offset.min(ctx.frame_buffer.len())
    };
    ctx.truncate(body_start);
    ctx.extend(&body_bytes);

    log::info!(
        "RichPresence: Patched live self push with appid {}",
        playing_app_id
    );
    Ok(())
}

/// Deliver the pending manufactured PersonaState by borrowing the
/// carrier's data pointer for one RecvPkt call, then restore.
pub fn try_inject(ctx: &mut NetPacketContext) -> Result<(), String> {
    let (inject_pending, cb_inject_pkt, inject_pkt) = {
        let mut state = state().lock().unwrap_or_else(|e| e.into_inner());
        if !state.inject_pending || state.cb_inject_pkt == 0 {
            return Ok(());
        }
        state.inject_pending = false;
        (true, state.cb_inject_pkt, state.inject_pkt)
    };

    if !inject_pending {
        return Ok(());
    }

    let new_frame = inject_pkt[..cb_inject_pkt as usize].to_vec();
    ctx.replace_frame(new_frame);

    log::info!(
        "RichPresence: Delivered manufactured self-PersonaState ({} bytes)",
        cb_inject_pkt
    );
    Ok(())
}

/// Register RichPresence NetPacket handlers.
pub fn register_richpresence_handlers() {
    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: K_EMSG_CLIENT_GAMES_PLAYED,
        target_job_hash: None,
        name: "RichPresence::HandleSend_GamesPlayed_742".to_string(),
        handler: Arc::new(track_send),
    });

    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: K_EMSG_CLIENT_GAMES_PLAYED_WITH_DATA_BLOB,
        target_job_hash: None,
        name: "RichPresence::HandleSend_GamesPlayed_5410".to_string(),
        handler: Arc::new(track_send),
    });

    register_netpacket_handler(NetPacketHandler {
        direction: NetPacketDirection::Send,
        emsg: K_EMSG_CLIENT_RICH_PRESENCE_UPLOAD,
        target_job_hash: None,
        name: "RichPresence::HandleSend_RichPresenceUpload".to_string(),
        handler: Arc::new(track_rp_send),
    });

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
    let mut state = state().lock().unwrap_or_else(|e| e.into_inner());
    *state = RichPresenceState::default();
    log::info!("Uninstalled RichPresence NetPacket hooks");
}

/// Check if RichPresence hooks are active.
pub fn is_richpresence_hook_active() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_string_kvs_simple() {
        // Binary KV1: 0x00 "RP" 0x01 "key" "value" 0x08
        let data = vec![
            0x00, b'R', b'P', 0x00, 0x01, b'k', b'e', b'y', 0x00, b'v', b'a', b'l', b'u', b'e',
            0x00, 0x08,
        ];
        let kvs = extract_string_kvs(&data);
        assert_eq!(kvs.len(), 1);
        assert_eq!(kvs[0], ("key".to_string(), "value".to_string()));
    }

    #[test]
    fn test_extract_string_kvs_multiple() {
        let data = vec![
            0x00, b'R', b'P', 0x00, 0x01, b'k', b'e', b'y', b'1', 0x00, b'v', b'a', b'l', b'1',
            0x00, 0x01, b'k', b'e', b'y', b'2', 0x00, b'v', b'a', b'l', b'2', 0x00, 0x08,
        ];
        let kvs = extract_string_kvs(&data);
        assert_eq!(kvs.len(), 2);
        assert_eq!(kvs[0], ("key1".to_string(), "val1".to_string()));
        assert_eq!(kvs[1], ("key2".to_string(), "val2".to_string()));
    }

    #[test]
    fn test_extract_string_kvs_nested() {
        let data = vec![
            0x00, b'R', b'P', 0x00, 0x00, b'N', b'E', b'S', b'T', 0x00, 0x01, b'n', b'k', 0x00,
            b'n', b'v', 0x00, 0x08, 0x01, b'k', b'0', 0x00, b'v', b'0', 0x00, 0x08,
        ];
        let kvs = extract_string_kvs(&data);
        assert_eq!(kvs.len(), 2);
        assert_eq!(kvs[0], ("nk".to_string(), "nv".to_string()));
        assert_eq!(kvs[1], ("k0".to_string(), "v0".to_string()));
    }

    #[test]
    fn test_constants() {
        assert_eq!(K_EMSG_CLIENT_GAMES_PLAYED, 742);
        assert_eq!(K_EMSG_CLIENT_GAMES_PLAYED_WITH_DATA_BLOB, 5410);
        assert_eq!(K_EMSG_CLIENT_PERSONA_STATE, 766);
        assert_eq!(K_EMSG_CLIENT_RICH_PRESENCE_UPLOAD, 7501);
        assert_eq!(K_STATUS_FLAG_RICH_PRESENCE, 0x1000);
    }

    #[test]
    fn test_leb128_tag_encoding_fields_ge_16() {
        let cases: &[(u32, u8, &[u8])] = &[
            (15, 2, &[0x7A]),       // 122 -> single byte
            (16, 0, &[0x80, 0x01]), // 128 -> first 2-byte tag
            (20, 5, &[0xA5, 0x01]), // 165 -> query_port
            (25, 1, &[0xC9, 0x01]), // 201 -> steamid_source
            (31, 2, &[0xFA, 0x01]), // 250 -> avatar_hash
            (45, 0, &[0xE8, 0x02]), // 360 -> last_logoff
            (55, 2, &[0xBA, 0x03]), // 442 -> game_name
            (56, 1, &[0xC1, 0x03]), // 449 -> gameid
            (64, 2, &[0x82, 0x04]), // 514 -> clan_data
            (71, 2, &[0xBA, 0x04]), // 570 -> rich_presence
            (80, 0, &[0x80, 0x05]), // 640 -> avatar_pending_review
            (82, 2, &[0x92, 0x05]), // 658 -> other_game_data
            (83, 0, &[0x98, 0x05]), // 664 -> gaming_device_type
        ];

        for &(field_num, wire_type, expected) in cases {
            let mut buf = Vec::new();
            encode_tag(field_num, wire_type, &mut buf);
            assert_eq!(
                buf.as_slice(),
                expected,
                "LEB128 tag mismatch for field={} wire={}",
                field_num,
                wire_type
            );
            let (dec_field, dec_wire, consumed) = decode_tag(&buf).unwrap();
            assert_eq!(dec_field, field_num);
            assert_eq!(dec_wire, wire_type);
            assert_eq!(consumed, expected.len());
        }
    }

    #[test]
    fn test_persona_state_msg_roundtrip_fields_ge_16() {
        let orig = PersonaStateMsg {
            status_flags: K_STATUS_FLAG_RICH_PRESENCE | 0x01,
            friends: vec![FriendEntry {
                friendid: Some(76561198012345678),
                persona_state: Some(1),
                game_played_app_id: Some(1245620),
                game_server_ip: Some(0x7F00_0001),
                game_server_port: Some(27015),
                persona_state_flags: Some(0x20),
                online_session_instances: Some(2),
                persona_set_by_user: Some(true),
                player_name: Some("Tarnished".to_string()),
                query_port: Some(27016),
                steamid_source: Some(76561198087654321),
                avatar_hash: Some(vec![0xAA; 20]),
                last_logoff: Some(1_700_000_001),
                last_logon: Some(1_700_000_100),
                last_seen_online: Some(1_700_000_200),
                clan_rank: Some(3),
                game_name: Some("ELDEN RING".to_string()),
                gameid: Some(1245620),
                game_data_blob: Some(vec![0xDE, 0xAD, 0xBE, 0xEF]),
                clan_data: Some(ClanData {
                    ogg_app_id: Some(1245620),
                    chat_group_id: Some(9876543210),
                }),
                clan_tag: Some("ER".to_string()),
                rich_presence: vec![
                    ("steam_display".to_string(), "#Status_InGame".to_string()),
                    ("zone".to_string(), "Limgrave".to_string()),
                ],
                broadcast_id: Some(0x1122_3344_5566_7788),
                game_lobby_id: Some(0x8877_6655_4433_2211),
                watching_broadcast_accountid: Some(4242),
                watching_broadcast_appid: Some(730),
                watching_broadcast_viewers: Some(123),
                watching_broadcast_title: Some("Boss Run".to_string()),
                is_community_banned: Some(false),
                player_name_pending_review: Some(false),
                avatar_pending_review: Some(true),
                on_steam_deck: Some(true),
                other_game_data: vec![OtherGameData {
                    gameid: Some(480),
                    rich_presence: vec![("mode".to_string(), "coop".to_string())],
                }],
                gaming_device_type: Some(2),
            }],
        };

        let encoded = serialize_persona_state_msg(&orig);

        // Verify exact 2-byte LEB128 tags appear on wire for fields >= 16
        assert!(encoded.windows(2).any(|w| w == [0xA5, 0x01])); // field 20 (query_port)
        assert!(encoded.windows(2).any(|w| w == [0xFA, 0x01])); // field 31 (avatar_hash)
        assert!(encoded.windows(2).any(|w| w == [0xE8, 0x02])); // field 45 (last_logoff)
        assert!(encoded.windows(2).any(|w| w == [0xBA, 0x03])); // field 55 (game_name)
        assert!(encoded.windows(2).any(|w| w == [0xC1, 0x03])); // field 56 (gameid)
        assert!(encoded.windows(2).any(|w| w == [0x82, 0x04])); // field 64 (clan_data)
        assert!(encoded.windows(2).any(|w| w == [0xBA, 0x04])); // field 71 (rich_presence)
        assert!(encoded.windows(2).any(|w| w == [0x80, 0x05])); // field 80 (avatar_pending_review)
        assert!(encoded.windows(2).any(|w| w == [0x92, 0x05])); // field 82 (other_game_data)
        assert!(encoded.windows(2).any(|w| w == [0x98, 0x05])); // field 83 (gaming_device_type)

        let decoded = parse_persona_state_msg(&encoded).unwrap();
        assert_eq!(decoded.status_flags, orig.status_flags);
        assert_eq!(decoded.friends.len(), 1);
        let f = &decoded.friends[0];
        let o = &orig.friends[0];
        assert_eq!(f.friendid, o.friendid);
        assert_eq!(f.persona_state, o.persona_state);
        assert_eq!(f.game_played_app_id, o.game_played_app_id);
        assert_eq!(f.player_name, o.player_name);
        assert_eq!(f.query_port, o.query_port);
        assert_eq!(f.steamid_source, o.steamid_source);
        assert_eq!(f.avatar_hash, o.avatar_hash);
        assert_eq!(f.last_logoff, o.last_logoff);
        assert_eq!(f.last_logon, o.last_logon);
        assert_eq!(f.last_seen_online, o.last_seen_online);
        assert_eq!(f.clan_rank, o.clan_rank);
        assert_eq!(f.game_name, o.game_name);
        assert_eq!(f.gameid, o.gameid);
        assert_eq!(f.game_data_blob, o.game_data_blob);
        assert_eq!(f.clan_data.as_ref().unwrap().ogg_app_id, Some(1245620));
        assert_eq!(
            f.clan_data.as_ref().unwrap().chat_group_id,
            Some(9876543210)
        );
        assert_eq!(f.clan_tag, o.clan_tag);
        assert_eq!(f.rich_presence, o.rich_presence);
        assert_eq!(f.broadcast_id, o.broadcast_id);
        assert_eq!(f.game_lobby_id, o.game_lobby_id);
        assert_eq!(
            f.watching_broadcast_accountid,
            o.watching_broadcast_accountid
        );
        assert_eq!(f.watching_broadcast_appid, o.watching_broadcast_appid);
        assert_eq!(f.watching_broadcast_viewers, o.watching_broadcast_viewers);
        assert_eq!(f.watching_broadcast_title, o.watching_broadcast_title);
        assert_eq!(f.is_community_banned, o.is_community_banned);
        assert_eq!(f.player_name_pending_review, o.player_name_pending_review);
        assert_eq!(f.avatar_pending_review, o.avatar_pending_review);
        assert_eq!(f.on_steam_deck, o.on_steam_deck);
        assert_eq!(f.other_game_data.len(), 1);
        assert_eq!(f.other_game_data[0].gameid, Some(480));
        assert_eq!(
            f.other_game_data[0].rich_presence,
            vec![("mode".to_string(), "coop".to_string())]
        );
        assert_eq!(f.gaming_device_type, o.gaming_device_type);
    }

    #[test]
    fn test_persona_state_msg_skips_unknown_multibyte_fields() {
        let mut friend_buf = Vec::new();
        encode_fixed64_field(1, 76561198000000001, &mut friend_buf);
        // Unknown varint field 99 and unknown length-delimited field 105
        encode_varint_field(99, 123456, &mut friend_buf);
        encode_bytes_field(105, b"future-steam-field", &mut friend_buf);
        encode_bytes_field(55, b"AfterUnknown", &mut friend_buf);

        let mut msg_buf = Vec::new();
        encode_varint_field(1, 0x1000, &mut msg_buf);
        encode_bytes_field(2, &friend_buf, &mut msg_buf);

        let parsed = parse_persona_state_msg(&msg_buf).unwrap();
        assert_eq!(parsed.friends.len(), 1);
        assert_eq!(parsed.friends[0].friendid, Some(76561198000000001));
        assert_eq!(
            parsed.friends[0].game_name.as_deref(),
            Some("AfterUnknown")
        );
    }
}
