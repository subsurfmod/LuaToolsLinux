//! AppTicket management for Steam on Linux.
//! Ported from usefulsteamtools: replaces Windows registry (HKCU\Software\Valve\Steam\Apps\<AppId>)
//! with reading/writing from localconfig.vdf under the Steam config directory.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::steam_bridge::ticket_store::{AppTicketData, TicketError, TicketStore};

/// Source of an AppTicket, matching the usefulsteamtools enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppTicketSource {
    /// Only use the credential store (localconfig.vdf).
    CredentialStoreOnly,
    /// Use credential store, fall back to forging if not found.
    CredentialStoreThenForge,
}

/// Represents an App Ownership Ticket, matching the usefulsteamtools structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppOwnershipTicket {
    pub app_id: u32,
    pub steam_id: Option<String>,
    pub app_ticket: Option<String>,
    pub e_ticket: Option<String>,
}

impl AppOwnershipTicket {
    /// Create a new AppOwnershipTicket from AppTicketData.
    pub fn from_data(data: &AppTicketData) -> Self {
        Self {
            app_id: data.app_id,
            steam_id: data.steam_id.clone(),
            app_ticket: data.app_ticket.clone(),
            e_ticket: data.e_ticket.clone(),
        }
    }
}

/// Reads an AppTicket (ownership ticket) for an app from the localconfig.vdf store.
pub fn read_app_ticket(app_id: u32) -> Result<Option<AppOwnershipTicket>, TicketError> {
    let store = TicketStore::default_path();
    let mut ts = TicketStore::new(&store);
    ts.load()?;
    let data = ts.get(app_id)?;
    Ok(Some(AppOwnershipTicket::from_data(&data)))
}

/// Writes an AppTicket for an app to the localconfig.vdf store.
pub fn write_app_ticket(data: AppTicketData) -> Result<(), TicketError> {
    let store = TicketStore::default_path();
    let mut ts = TicketStore::new(&store);
    ts.set(data)
}

/// Removes an AppTicket entry for an app from the localconfig.vdf store.
pub fn remove_app_ticket(app_id: u32) -> Result<(), TicketError> {
    let store = TicketStore::default_path();
    let mut ts = TicketStore::new(&store);
    ts.remove(app_id)
}

/// Lists all app IDs that have AppTicket entries in the localconfig.vdf store.
pub fn list_app_ids() -> Result<Vec<u32>, TicketError> {
    let store = TicketStore::default_path();
    let mut ts = TicketStore::new(&store);
    ts.load()?;
    Ok(ts.list_app_ids()?)
}

/// Gets an AppTicket by app_id, returning the full AppOwnershipTicket structure.
/// This is an alias for read_app_ticket for API consistency with credentials module.
pub fn get_app_ticket(app_id: u32) -> Result<Option<AppOwnershipTicket>, TicketError> {
    read_app_ticket(app_id)
}

/// Writes an e-ticket for an app to the localconfig.vdf store.
pub fn write_e_ticket(app_id: u32, e_ticket: String) -> Result<(), TicketError> {
    let store = TicketStore::default_path();
    let mut ts = TicketStore::new(&store);
    ts.load()?;
    if let Ok(Some(mut data)) = ts.get(app_id) {
        data.e_ticket = Some(e_ticket);
        data.last_update = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        );
        ts.set(data)
    } else {
        // Create new entry with just e_ticket
        let data = AppTicketData {
            app_id,
            steam_id: None,
            app_ticket: None,
            e_ticket: Some(e_ticket),
            last_update: Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            ),
        };
        ts.set(data)
    }
}

/// Gets an e-ticket for an app from the localconfig.vdf store.
pub fn get_e_ticket(app_id: u32) -> Result<Option<String>, TicketError> {
    let store = TicketStore::default_path();
    let mut ts = TicketStore::new(&store);
    ts.load()?;
    if let Ok(Some(data)) = ts.get(app_id) {
        Ok(data.e_ticket)
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_app_ticket_creates_from_store() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path();

        // Set up a TicketStore with some data
        let mut ts = crate::steam_bridge::ticket_store::TicketStore::new(store_path);
        ts.load().unwrap();

        let data = AppTicketData {
            app_id: 1245620,
            steam_id: Some("76561198028121353".into()),
            app_ticket: Some("deadbeef".into()),
            e_ticket: Some("cafebabe".into()),
            last_update: None,
        };
        ts.set(data).unwrap();

        // Read via the new appticket API
        let result = read_app_ticket(1245620);
        assert!(result.is_ok());
        let ticket = result.unwrap();
        assert_eq!(ticket.app_id, 1245620);
        assert_eq!(ticket.steam_id, Some("76561198028121353".into()));
        assert_eq!(ticket.app_ticket, Some("deadbeef".into()));
        assert_eq!(ticket.e_ticket, Some("cafebabe".into()));
    }

    #[test]
    fn test_write_app_ticket() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path();

        let data = AppTicketData {
            app_id: 1245620,
            steam_id: Some("76561198028121353".into()),
            app_ticket: Some("newticket123".into()),
            e_ticket: Some("eticket456".into()),
            last_update: None,
        };

        let result = write_app_ticket(data.clone());
        assert!(result.is_ok());

        // Verify by reading back
        let ticket = read_app_ticket(1245620);
        assert!(ticket.is_ok());
        let t = ticket.unwrap();
        assert_eq!(t.app_ticket, Some("newticket123".into()));
        assert_eq!(t.e_ticket, Some("eticket456".into()));
    }

    #[test]
    fn test_remove_app_ticket() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path();

        let mut ts = crate::steam_bridge::ticket_store::TicketStore::new(store_path);
        ts.load().unwrap();

        let data = AppTicketData {
            app_id: 1245620,
            steam_id: Some("76561198028121353".into()),
            app_ticket: Some("ticketto_remove".into()),
            e_ticket: None,
            last_update: None,
        };
        ts.set(data).unwrap();

        // Remove via the new appticket API
        let result = remove_app_ticket(1245620);
        assert!(result.is_ok());

        // Verify it's gone
        let ticket = read_app_ticket(1245620);
        assert!(ticket.is_err());
    }

    #[test]
    fn test_list_app_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path();

        let mut ts = crate::steam_bridge::ticket_store::TicketStore::new(store_path);
        ts.load().unwrap();

        // Add two apps
        let data1 = AppTicketData {
            app_id: 1245620,
            steam_id: Some("76561198028121353".into()),
            app_ticket: Some("ticket1".into()),
            e_ticket: None,
            last_update: None,
        };
        ts.set(data1).unwrap();

        let data2 = AppTicketData {
            app_id: 2778580,
            steam_id: Some("76561198028121353".into()),
            app_ticket: Some("ticket2".into()),
            e_ticket: None,
            last_update: None,
        };
        ts.set(data2).unwrap();

        let ids = list_app_ids().unwrap();
        assert!(ids.contains(&1245620));
        assert!(ids.contains(&2778580));
        assert_eq!(ids.len(), 2);
    }
}