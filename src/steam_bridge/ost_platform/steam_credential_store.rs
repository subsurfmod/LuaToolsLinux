//! Linux implementation of OSTPlatform::SteamCredentialStore
//!
//! Provides Steam credential storage using localconfig.vdf (Linux equivalent of Windows registry).
//! Wraps the existing TicketStore to match the Windows API.

use std::path::Path;
use std::str::FromStr;

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

/// Internal store instance
static mut STORE: Option<TicketStore> = None;

fn get_store() -> &'static mut TicketStore {
    unsafe {
        if STORE.is_none() {
            STORE = Some(TicketStore::new(&TicketStore::default_path()));
            let _ = STORE.as_mut().unwrap().load();
        }
        STORE.as_mut().unwrap()
    }
}

/// Initialize the store with a custom config directory.
pub fn initialize(config_dir: &Path) {
    unsafe {
        STORE = Some(TicketStore::new(config_dir));
        let _ = STORE.as_mut().unwrap().load();
    }
}

/// Read an AppTicket (ownership ticket) for an app.
/// Returns the ticket as a byte vector.
pub fn get_app_ticket(app_id: u32, ticket: &mut Vec<u8>) -> Status {
    let store = get_store();
    match store.get(app_id) {
        Ok(data) => {
            if let Some(ticket_str) = data.app_ticket {
                // Parse hex string to bytes
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

/// Write an AppTicket for an app.
/// Takes ticket as byte vector, stores as hex string.
pub fn write_app_ticket(app_id: u32, data: &[u8]) -> Status {
    let store = get_store();
    let ticket_hex = hex::encode(data);
    let ticket_data = AppTicketData {
        app_id,
        app_ticket: Some(ticket_hex),
        ..Default::default()
    };

    match store.set(ticket_data) {
        Ok(_) => Status::Ok,
        Err(_) => Status::Failed,
    }
}

/// Read an ETicket (encrypted app ticket) for an app.
pub fn get_e_ticket(app_id: u32, ticket: &mut Vec<u8>) -> Status {
    let store = get_store();
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

/// Write an ETicket for an app.
pub fn write_e_ticket(app_id: u32, data: &[u8]) -> Status {
    let store = get_store();
    let ticket_hex = hex::encode(data);
    let ticket_data = AppTicketData {
        app_id,
        e_ticket: Some(ticket_hex),
        ..Default::default()
    };

    match store.set(ticket_data) {
        Ok(_) => Status::Ok,
        Err(_) => Status::Failed,
    }
}

/// Read a spoofed SteamID for an app.
pub fn get_steam_id(app_id: u32, steam_id: &mut u64) -> Status {
    let store = get_store();
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

/// Write a spoofed SteamID for an app.
pub fn write_steam_id(app_id: u32, steam_id: u64) -> Status {
    let store = get_store();
    let ticket_data = AppTicketData {
        app_id,
        steam_id: Some(steam_id.to_string()),
        ..Default::default()
    };

    match store.set(ticket_data) {
        Ok(_) => Status::Ok,
        Err(_) => Status::Failed,
    }
}

/// Get the active user's account ID and universe.
/// On Linux, reads from localconfig.vdf or Steam's loginusers.vdf.
pub fn get_active_user(account_id: &mut u32, universe: &mut String) -> Status {
    let store = get_store();
    // Try to load loginusers.vdf for active user
    let config_dir = store.config_dir.clone();
    let loginusers_path = config_dir.join("loginusers.vdf");

    if loginusers_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&loginusers_path) {
            // Parse loginusers.vdf for most recent account
            // Format: "accounts" { "76561198028121353" { "mostrecent" "1" ... } }
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.contains("mostrecent") && trimmed.contains("1") {
                    // Look for the account ID in previous lines
                    // This is a simplified parser
                }
            }
        }
    }

    // Fallback: try to get from first ticket
    if let Ok(ids) = store.list_app_ids() {
        if let Some(first_id) = ids.first() {
            if let Ok(data) = store.get(*first_id) {
                if let Some(sid_str) = &data.steam_id {
                    if let Ok(sid) = u64::from_str(sid_str) {
                        *account_id = (sid & 0xFFFFFFFF) as u32;
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
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_steam_credential_store_roundtrip() {
        let dir = tempdir().unwrap();
        initialize(dir.path());

        let app_id = 1245620;
        let test_ticket = vec![0xde, 0xad, 0xbe, 0xef];
        let test_eticket = vec![0xca, 0xfe, 0xba, 0xbe];
        let test_steam_id = 76561198028121353u64;

        // Write app ticket
        assert_eq!(write_app_ticket(app_id, &test_ticket), Status::Ok);

        // Read app ticket
        let mut ticket = Vec::new();
        assert_eq!(get_app_ticket(app_id, &mut ticket), Status::Ok);
        assert_eq!(ticket, test_ticket);

        // Write eticket
        assert_eq!(write_e_ticket(app_id, &test_eticket), Status::Ok);

        // Read eticket
        let mut eticket = Vec::new();
        assert_eq!(get_e_ticket(app_id, &mut eticket), Status::Ok);
        assert_eq!(eticket, test_eticket);

        // Write steam ID
        assert_eq!(write_steam_id(app_id, test_steam_id), Status::Ok);

        // Read steam ID
        let mut steam_id = 0u64;
        assert_eq!(get_steam_id(app_id, &mut steam_id), Status::Ok);
        assert_eq!(steam_id, test_steam_id);
    }
}