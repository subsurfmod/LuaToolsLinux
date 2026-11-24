//! AppTicket management for Steam on Linux.
//! Ported from usefulsteamtools: replaces Windows registry (`HKCU\Software\Valve\Steam\Apps\<AppId>`)
//! with reading/writing from `localconfig.vdf` under the Steam config directory.

use std::path::Path;

use crate::steam_bridge::ticket_store::{AppTicketData, TicketError, TicketStore};

/// Source of an AppTicket, matching the usefulsteamtools enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppTicketSource {
    /// Only use the credential store (`localconfig.vdf`).
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
    /// Create a new `AppOwnershipTicket` from `AppTicketData`.
    pub fn from_data(data: &AppTicketData) -> Self {
        Self {
            app_id: data.app_id,
            steam_id: data.steam_id.clone(),
            app_ticket: data.app_ticket.clone(),
            e_ticket: data.e_ticket.clone(),
        }
    }
}

/// Reads an AppTicket for an app from a specific Steam config directory.
pub fn read_app_ticket_from(
    config_dir: &Path,
    app_id: u32,
) -> Result<Option<AppOwnershipTicket>, TicketError> {
    let mut ts = TicketStore::new(config_dir);
    ts.load()?;
    match ts.get(app_id) {
        Ok(data) => Ok(Some(AppOwnershipTicket::from_data(&data))),
        Err(TicketError::NotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Reads an AppTicket (ownership ticket) for an app from the default `localconfig.vdf` store.
pub fn read_app_ticket(app_id: u32) -> Result<Option<AppOwnershipTicket>, TicketError> {
    read_app_ticket_from(&TicketStore::default_path(), app_id)
}

/// Writes an AppTicket for an app to a specific Steam config directory.
pub fn write_app_ticket_to(config_dir: &Path, data: AppTicketData) -> Result<(), TicketError> {
    let mut ts = TicketStore::new(config_dir);
    ts.set(data)
}

/// Writes an AppTicket for an app to the default `localconfig.vdf` store.
pub fn write_app_ticket(data: AppTicketData) -> Result<(), TicketError> {
    write_app_ticket_to(&TicketStore::default_path(), data)
}

/// Removes an AppTicket entry for an app from a specific Steam config directory.
pub fn remove_app_ticket_from(config_dir: &Path, app_id: u32) -> Result<(), TicketError> {
    let mut ts = TicketStore::new(config_dir);
    ts.remove(app_id)
}

/// Removes an AppTicket entry for an app from the default `localconfig.vdf` store.
pub fn remove_app_ticket(app_id: u32) -> Result<(), TicketError> {
    remove_app_ticket_from(&TicketStore::default_path(), app_id)
}

/// Lists all app IDs that have AppTicket entries in a specific Steam config directory.
pub fn list_app_ids_from(config_dir: &Path) -> Result<Vec<u32>, TicketError> {
    let mut ts = TicketStore::new(config_dir);
    ts.list_app_ids()
}

/// Lists all app IDs that have AppTicket entries in the default `localconfig.vdf` store.
pub fn list_app_ids() -> Result<Vec<u32>, TicketError> {
    list_app_ids_from(&TicketStore::default_path())
}

/// Gets an AppTicket by `app_id`, returning the full `AppOwnershipTicket` structure.
pub fn get_app_ticket(app_id: u32) -> Result<Option<AppOwnershipTicket>, TicketError> {
    read_app_ticket(app_id)
}

/// Writes an e-ticket for an app to a specific Steam config directory while preserving
/// existing `steam_id` and `app_ticket` fields.
pub fn write_e_ticket_to(
    config_dir: &Path,
    app_id: u32,
    e_ticket: String,
) -> Result<(), TicketError> {
    let mut ts = TicketStore::new(config_dir);
    ts.load()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut data = match ts.get(app_id) {
        Ok(existing) => existing,
        Err(TicketError::NotFound(_)) => AppTicketData {
            app_id,
            ..Default::default()
        },
        Err(e) => return Err(e),
    };
    data.e_ticket = Some(e_ticket);
    data.last_update = Some(now);
    ts.set(data)
}

/// Writes an e-ticket for an app to the default `localconfig.vdf` store.
pub fn write_e_ticket(app_id: u32, e_ticket: String) -> Result<(), TicketError> {
    write_e_ticket_to(&TicketStore::default_path(), app_id, e_ticket)
}

/// Gets an e-ticket for an app from a specific Steam config directory.
pub fn get_e_ticket_from(config_dir: &Path, app_id: u32) -> Result<Option<String>, TicketError> {
    let mut ts = TicketStore::new(config_dir);
    ts.load()?;
    match ts.get(app_id) {
        Ok(data) => Ok(data.e_ticket),
        Err(TicketError::NotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Gets an e-ticket for an app from the default `localconfig.vdf` store.
pub fn get_e_ticket(app_id: u32) -> Result<Option<String>, TicketError> {
    get_e_ticket_from(&TicketStore::default_path(), app_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_app_ticket_creates_from_store() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path();

        let data = AppTicketData {
            app_id: 1245620,
            steam_id: Some("76561198028121353".into()),
            app_ticket: Some("deadbeef".into()),
            e_ticket: Some("cafebabe".into()),
            last_update: None,
        };
        write_app_ticket_to(store_path, data).unwrap();

        let ticket = read_app_ticket_from(store_path, 1245620)
            .unwrap()
            .expect("ticket should exist");
        assert_eq!(ticket.app_id, 1245620);
        assert_eq!(ticket.steam_id.as_deref(), Some("76561198028121353"));
        assert_eq!(ticket.app_ticket.as_deref(), Some("deadbeef"));
        assert_eq!(ticket.e_ticket.as_deref(), Some("cafebabe"));
    }

    #[test]
    fn test_write_and_remove_app_ticket() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path();

        let data = AppTicketData {
            app_id: 1245620,
            steam_id: Some("76561198028121353".into()),
            app_ticket: Some("newticket123".into()),
            e_ticket: Some("eticket456".into()),
            last_update: None,
        };

        write_app_ticket_to(store_path, data).unwrap();
        let t = read_app_ticket_from(store_path, 1245620)
            .unwrap()
            .expect("ticket should exist");
        assert_eq!(t.app_ticket.as_deref(), Some("newticket123"));
        assert_eq!(t.e_ticket.as_deref(), Some("eticket456"));

        remove_app_ticket_from(store_path, 1245620).unwrap();
        assert!(read_app_ticket_from(store_path, 1245620).unwrap().is_none());
    }

    #[test]
    fn test_list_app_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path();

        write_app_ticket_to(
            store_path,
            AppTicketData {
                app_id: 1245620,
                steam_id: Some("76561198028121353".into()),
                app_ticket: Some("ticket1".into()),
                e_ticket: None,
                last_update: None,
            },
        )
        .unwrap();
        write_app_ticket_to(
            store_path,
            AppTicketData {
                app_id: 2778580,
                steam_id: Some("76561198028121353".into()),
                app_ticket: Some("ticket2".into()),
                e_ticket: None,
                last_update: None,
            },
        )
        .unwrap();

        let ids = list_app_ids_from(store_path).unwrap();
        assert!(ids.contains(&1245620));
        assert!(ids.contains(&2778580));
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn test_write_and_get_e_ticket_preserves_existing_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path();

        assert_eq!(get_e_ticket_from(store_path, 730).unwrap(), None);

        write_app_ticket_to(
            store_path,
            AppTicketData {
                app_id: 730,
                steam_id: Some("76561198028121353".into()),
                app_ticket: Some("deadbeef".into()),
                e_ticket: None,
                last_update: None,
            },
        )
        .unwrap();

        write_e_ticket_to(store_path, 730, "cafebabe".into()).unwrap();
        assert_eq!(
            get_e_ticket_from(store_path, 730).unwrap().as_deref(),
            Some("cafebabe")
        );

        let full = read_app_ticket_from(store_path, 730).unwrap().unwrap();
        assert_eq!(full.steam_id.as_deref(), Some("76561198028121353"));
        assert_eq!(full.app_ticket.as_deref(), Some("deadbeef"));
        assert_eq!(full.e_ticket.as_deref(), Some("cafebabe"));
    }
}
