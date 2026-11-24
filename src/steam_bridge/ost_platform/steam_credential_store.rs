//! Linux implementation of OSTPlatform::SteamCredentialStore
//!
//! Provides thread-safe Steam credential storage using `localconfig.vdf`
//! (Linux equivalent of the Windows registry store). Wraps `TicketStore` while
//! preserving existing fields when updating individual values.

use std::path::Path;
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard, OnceLock};

use super::super::ticket_store::{AppTicketData, TicketStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    NotFound,
    Unsupported,
    Failed,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Ok => "Ok",
            Status::NotFound => "NotFound",
            Status::Unsupported => "Unsupported",
            Status::Failed => "Failed",
        }
    }
}

pub fn to_string(status: Status) -> &'static str {
    status.as_str()
}

static STORE: OnceLock<Mutex<TicketStore>> = OnceLock::new();

fn lock_store() -> MutexGuard<'static, TicketStore> {
    STORE
        .get_or_init(|| {
            let mut store = TicketStore::new(&TicketStore::default_path());
            let _ = store.load();
            Mutex::new(store)
        })
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Initialize the store with a custom config directory.
pub fn initialize(config_dir: &Path) {
    let mut store = lock_store();
    *store = TicketStore::new(config_dir);
    let _ = store.load();
}

fn decode_ticket_hex(ticket_str: Option<&str>, out: &mut Vec<u8>) -> Status {
    out.clear();
    let Some(raw) = ticket_str else {
        return Status::NotFound;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Status::NotFound;
    }
    match hex::decode(trimmed) {
        Ok(decoded) if !decoded.is_empty() => {
            *out = decoded;
            Status::Ok
        }
        Ok(_) => Status::NotFound,
        Err(_) => Status::Failed,
    }
}

fn universe_from_steam_id(sid: u64) -> &'static str {
    match (sid >> 56) & 0xFF {
        1 => "public",
        2 => "beta",
        3 => "internal",
        4 => "dev",
        _ => "public",
    }
}

fn parse_vdf_kv_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    let rest = line.strip_prefix('"')?;
    let k_end = rest.find('"')?;
    let key = &rest[..k_end];
    let after_key = rest.get(k_end + 1..)?.trim_start();
    let val_rest = after_key.strip_prefix('"')?;
    let v_end = val_rest.find('"')?;
    let val = &val_rest[..v_end];
    Some((key, val))
}

fn parse_vdf_section_steam_id(line: &str) -> Option<u64> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix('"')?;
    let end = rest.find('"')?;
    let inner = &rest[..end];
    let trailing = rest.get(end + 1..)?.trim();
    // Section header line is either `"7656..."` or `"7656..." {` (not a key-value pair)
    if !trailing.is_empty() && trailing != "{" {
        return None;
    }
    if inner.is_empty() || !inner.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let sid = inner.parse::<u64>().ok()?;
    if sid >= 0x0110_0001_0000_0000 {
        Some(sid)
    } else {
        None
    }
}

fn parse_active_steam_id_from_loginusers(content: &str) -> Option<u64> {
    let mut current_steam_id: Option<u64> = None;
    let mut best_by_timestamp: Option<(u64, u64)> = None;
    let mut first_steam_id: Option<u64> = None;

    for line in content.lines() {
        if let Some(sid) = parse_vdf_section_steam_id(line) {
            current_steam_id = Some(sid);
            if first_steam_id.is_none() {
                first_steam_id = Some(sid);
            }
            continue;
        }

        if let Some((key, val)) = parse_vdf_kv_line(line) {
            if let Some(sid) = current_steam_id {
                if key.eq_ignore_ascii_case("MostRecent") && val.trim() == "1" {
                    return Some(sid);
                }
                if key.eq_ignore_ascii_case("Timestamp") {
                    if let Ok(ts) = val.trim().parse::<u64>() {
                        if best_by_timestamp.is_none_or(|(best_ts, _)| ts > best_ts) {
                            best_by_timestamp = Some((ts, sid));
                        }
                    }
                }
            }
        }
    }

    best_by_timestamp.map(|(_, sid)| sid).or(first_steam_id)
}

/// Read an AppTicket (ownership ticket) for an app.
pub fn get_app_ticket(app_id: u32, ticket: &mut Vec<u8>) -> Status {
    ticket.clear();
    let mut store = lock_store();
    match store.get(app_id) {
        Ok(data) => decode_ticket_hex(data.app_ticket.as_deref(), ticket),
        Err(_) => Status::NotFound,
    }
}

/// Write an AppTicket for an app, preserving existing `steam_id` and `e_ticket`.
pub fn write_app_ticket(app_id: u32, data: &[u8]) -> Status {
    let mut store = lock_store();
    let mut existing = store.get(app_id).unwrap_or_else(|_| AppTicketData {
        app_id,
        ..Default::default()
    });
    existing.app_ticket = if data.is_empty() {
        None
    } else {
        Some(hex::encode(data))
    };

    match store.set(existing) {
        Ok(_) => Status::Ok,
        Err(_) => Status::Failed,
    }
}

/// Read an ETicket (encrypted app ticket) for an app.
pub fn get_e_ticket(app_id: u32, ticket: &mut Vec<u8>) -> Status {
    ticket.clear();
    let mut store = lock_store();
    match store.get(app_id) {
        Ok(data) => decode_ticket_hex(data.e_ticket.as_deref(), ticket),
        Err(_) => Status::NotFound,
    }
}

/// Write an ETicket for an app, preserving existing `steam_id` and `app_ticket`.
pub fn write_e_ticket(app_id: u32, data: &[u8]) -> Status {
    let mut store = lock_store();
    let mut existing = store.get(app_id).unwrap_or_else(|_| AppTicketData {
        app_id,
        ..Default::default()
    });
    existing.e_ticket = if data.is_empty() {
        None
    } else {
        Some(hex::encode(data))
    };

    match store.set(existing) {
        Ok(_) => Status::Ok,
        Err(_) => Status::Failed,
    }
}

/// Read a spoofed SteamID for an app.
pub fn get_steam_id(app_id: u32, steam_id: &mut u64) -> Status {
    let mut store = lock_store();
    match store.get(app_id) {
        Ok(data) => {
            if let Some(sid_str) = &data.steam_id {
                if let Ok(sid) = u64::from_str(sid_str.trim()) {
                    *steam_id = sid;
                    Status::Ok
                } else {
                    Status::Failed
                }
            } else {
                Status::NotFound
            }
        }
        Err(_) => Status::NotFound,
    }
}

/// Write a spoofed SteamID for an app, preserving existing tickets.
pub fn write_steam_id(app_id: u32, steam_id: u64) -> Status {
    let mut store = lock_store();
    let mut existing = store.get(app_id).unwrap_or_else(|_| AppTicketData {
        app_id,
        ..Default::default()
    });
    existing.steam_id = Some(steam_id.to_string());

    match store.set(existing) {
        Ok(_) => Status::Ok,
        Err(_) => Status::Failed,
    }
}

/// Get the active user's account ID and universe from `loginusers.vdf` or cached tickets.
pub fn get_active_user(account_id: &mut u32, universe: &mut String) -> Status {
    let mut store = lock_store();
    let loginusers_path = store.config_dir().join("loginusers.vdf");

    if loginusers_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&loginusers_path) {
            if let Some(sid) = parse_active_steam_id_from_loginusers(&content) {
                *account_id = (sid & 0xFFFF_FFFF) as u32;
                *universe = universe_from_steam_id(sid).to_string();
                return Status::Ok;
            }
        }
    }

    if let Ok(ids) = store.list_app_ids() {
        for id in ids {
            if let Ok(data) = store.get(id) {
                if let Some(sid_str) = &data.steam_id {
                    if let Ok(sid) = u64::from_str(sid_str.trim()) {
                        *account_id = (sid & 0xFFFF_FFFF) as u32;
                        *universe = universe_from_steam_id(sid).to_string();
                        return Status::Ok;
                    }
                }
            }
        }
    }

    Status::NotFound
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::tempdir;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_steam_credential_store_roundtrip() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        initialize(dir.path());

        let app_id = 1245620;
        let test_ticket = vec![0xde, 0xad, 0xbe, 0xef];
        let test_eticket = vec![0xca, 0xfe, 0xba, 0xbe];
        let test_steam_id = 76561198028121353u64;

        assert_eq!(write_app_ticket(app_id, &test_ticket), Status::Ok);
        assert_eq!(write_e_ticket(app_id, &test_eticket), Status::Ok);
        assert_eq!(write_steam_id(app_id, test_steam_id), Status::Ok);

        // Verify all three fields co-exist without clobbering each other
        let mut ticket = Vec::new();
        assert_eq!(get_app_ticket(app_id, &mut ticket), Status::Ok);
        assert_eq!(ticket, test_ticket);

        let mut eticket = Vec::new();
        assert_eq!(get_e_ticket(app_id, &mut eticket), Status::Ok);
        assert_eq!(eticket, test_eticket);

        let mut steam_id = 0u64;
        assert_eq!(get_steam_id(app_id, &mut steam_id), Status::Ok);
        assert_eq!(steam_id, test_steam_id);

        // Reading missing app clears caller's output buffer
        assert_eq!(get_app_ticket(999_999, &mut ticket), Status::NotFound);
        assert!(ticket.is_empty());
    }

    #[test]
    fn test_get_active_user_from_loginusers_and_fallback() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let loginusers = r#"
"users"
{
    "76561198000000111"
    {
        "AccountName"   "older_user"
        "MostRecent"    "0"
        "Timestamp"     "1700000001"
    }
    "76561198000000222" {
        "AccountName"   "active_user"
        "MostRecent"    "1"
        "Timestamp"     "1700000099"
    }
}
"#;
        std::fs::write(dir.path().join("loginusers.vdf"), loginusers).unwrap();
        initialize(dir.path());

        let mut account_id = 0u32;
        let mut universe = String::new();
        assert_eq!(get_active_user(&mut account_id, &mut universe), Status::Ok);
        assert_eq!(account_id, (76561198000000222u64 & 0xFFFF_FFFF) as u32);
        assert_eq!(universe, "public");

        // Fallback to TicketStore when loginusers.vdf is absent and first app lacks steam_id
        let dir2 = tempdir().unwrap();
        initialize(dir2.path());
        assert_eq!(write_app_ticket(100, &[1, 2, 3]), Status::Ok);
        // Beta universe (2 << 56) | individual account (1 << 52) | instance (1 << 32) | account_id 42
        let beta_sid: u64 = (2u64 << 56) | (1u64 << 52) | (1u64 << 32) | 42;
        assert_eq!(write_steam_id(200, beta_sid), Status::Ok);

        assert_eq!(get_active_user(&mut account_id, &mut universe), Status::Ok);
        assert_eq!(account_id, 42);
        assert_eq!(universe, "beta");
    }

    #[test]
    fn test_decode_ticket_hex_malformed_returns_failed() {
        let mut out = vec![1, 2, 3];
        assert_eq!(
            decode_ticket_hex(Some("not-hex!"), &mut out),
            Status::Failed
        );
        assert!(out.is_empty());
        assert_eq!(decode_ticket_hex(None, &mut out), Status::NotFound);
    }
}
