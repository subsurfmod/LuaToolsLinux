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

/// Read an AppTicket (ownership ticket) for an app.
pub fn get_app_ticket(app_id: u32, ticket: &mut Vec<u8>) -> Status {
    let mut store = lock_store();
    match store.get(app_id) {
        Ok(data) => {
            if let Some(ticket_str) = data.app_ticket {
                *ticket = hex::decode(&ticket_str).unwrap_or_default();
                if !ticket.is_empty() {
                    Status::Ok
                } else {
                    Status::NotFound
                }
            } else {
                Status::NotFound
            }
        }
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
    existing.app_ticket = Some(hex::encode(data));

    match store.set(existing) {
        Ok(_) => Status::Ok,
        Err(_) => Status::Failed,
    }
}

/// Read an ETicket (encrypted app ticket) for an app.
pub fn get_e_ticket(app_id: u32, ticket: &mut Vec<u8>) -> Status {
    let mut store = lock_store();
    match store.get(app_id) {
        Ok(data) => {
            if let Some(ticket_str) = data.e_ticket {
                *ticket = hex::decode(&ticket_str).unwrap_or_default();
                if !ticket.is_empty() {
                    Status::Ok
                } else {
                    Status::NotFound
                }
            } else {
                Status::NotFound
            }
        }
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
    existing.e_ticket = Some(hex::encode(data));

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
                if let Ok(sid) = u64::from_str(sid_str) {
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
            let mut current_steam_id: Option<u64> = None;
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() > 2 {
                    let inner = &trimmed[1..trimmed.len() - 1];
                    if inner.chars().all(|c| c.is_ascii_digit()) {
                        if let Ok(sid) = inner.parse::<u64>() {
                            if sid >= 0x0110_0001_0000_0000 {
                                current_steam_id = Some(sid);
                            }
                        }
                    }
                }
                let lower = trimmed.to_ascii_lowercase();
                if lower.contains("\"mostrecent\"") && lower.contains("\"1\"") {
                    if let Some(sid) = current_steam_id {
                        *account_id = (sid & 0xFFFF_FFFF) as u32;
                        *universe = "public".to_string();
                        return Status::Ok;
                    }
                }
            }
        }
    }

    if let Ok(ids) = store.list_app_ids() {
        if let Some(first_id) = ids.first() {
            if let Ok(data) = store.get(*first_id) {
                if let Some(sid_str) = &data.steam_id {
                    if let Ok(sid) = u64::from_str(sid_str) {
                        *account_id = (sid & 0xFFFF_FFFF) as u32;
                        *universe = "public".to_string();
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
    use tempfile::tempdir;

    #[test]
    fn test_steam_credential_store_roundtrip() {
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
    }
}
