//! AppTicket management for Steam on Linux.
//! Ported from usefulsteamtools: replaces Windows registry (`HKCU\Software\Valve\Steam\Apps\<AppId>`)
//! with reading/writing from `localconfig.vdf` under the Steam config directory.

use std::path::Path;

use crate::steam_bridge::ticket_store::{AppTicketData, TicketError, TicketStore};

pub const K_APP_TICKET_STEAM_ID_OFFSET: usize = 8;
pub const K_APP_TICKET_APP_ID_OFFSET: usize = 16;
pub const K_APP_TICKET_SIGNATURE_SIZE: usize = 128;
pub const K_LOCAL_APP_TICKET_SOURCE_APP_ID: u32 = 7;

/// Extended ownership ticket details with byte layout offsets for DRM/IPC responses (BST-50).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppOwnershipTicketDetails {
    pub data: Vec<u8>,
    pub total_size: usize,
    pub app_id_offset: usize,
    pub steam_id_offset: usize,
    pub signature_offset: usize,
    pub signature_size: usize,
}

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

/// Extracts the SteamID64 baked into app ownership ticket bytes.
/// Layout starts with [uint32 size][uint32 version][uint64 steam_id][uint32 app_id]...
pub fn extract_steam_id_from_ticket_bytes(ticket: &[u8]) -> u64 {
    if ticket.len() >= 16 {
        let sid = u64::from_le_bytes([
            ticket[8], ticket[9], ticket[10], ticket[11],
            ticket[12], ticket[13], ticket[14], ticket[15],
        ]);
        if sid >= 0x0110_0001_0000_0000 {
            return sid;
        }
    }
    if ticket.len() >= 12 {
        let sid = u64::from_le_bytes([
            ticket[4], ticket[5], ticket[6], ticket[7],
            ticket[8], ticket[9], ticket[10], ticket[11],
        ]);
        if sid >= 0x0110_0001_0000_0000 {
            return sid;
        }
    }
    0
}

/// Reads a valid base source ticket from localconfig.vdf (or ticket store cache)
/// to use as a donor template for the RSA-1024 splice exploit (BST-49).
///
/// Follows UsefulSteamTools preference order:
/// 1. The active user's localconfig.vdf (skipping family-shared tickets with lender SteamIDs)
/// 2. Primary fallback AppId 7 (Steam Client)
/// 3. Secondary fallback AppId 228980 (Steamworks Shared Redistributables)
/// 4. Any other cached ticket > 128 bytes
pub fn read_localconfig_source_ticket(config_dir: Option<&Path>) -> Option<(u32, Vec<u8>)> {
    let base_dir = config_dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(TicketStore::default_path);

    let mut active_account_id = 0u32;
    let mut universe = String::new();
    let has_active_user = crate::steam_bridge::ost_platform::steam_credential_store::get_active_user(
        &mut active_account_id,
        &mut universe,
    ) == crate::steam_bridge::ost_platform::steam_credential_store::Status::Ok && active_account_id != 0;

    // Try reading localconfig.vdf in base_dir
    let vdf_path = base_dir.join("localconfig.vdf");
    if let Ok(content) = std::fs::read_to_string(&vdf_path) {
        let filter_id = if has_active_user { Some(active_account_id) } else { None };
        if let Some((app_id, bytes)) = extract_source_ticket_from_vdf(&content, filter_id) {
            return Some((app_id, bytes));
        }
        if has_active_user {
            if let Some((app_id, bytes)) = extract_source_ticket_from_vdf(&content, None) {
                return Some((app_id, bytes));
            }
        }
    }

    // Also scan ~/.steam/steam/userdata or ~/.local/share/Steam/userdata
    if let Some(home) = dirs::home_dir() {
        for root in [home.join(".steam/steam"), home.join(".local/share/Steam")] {
            let userdata = root.join("userdata");
            if let Ok(entries) = std::fs::read_dir(&userdata) {
                for entry in entries.flatten() {
                    let dir_account_id: Option<u32> = entry.file_name().to_string_lossy().parse().ok();
                    let user_cfg = entry.path().join("config").join("localconfig.vdf");
                    if let Ok(content) = std::fs::read_to_string(&user_cfg) {
                        let filter_id = if has_active_user {
                            Some(active_account_id)
                        } else {
                            dir_account_id
                        };
                        if let Some((app_id, bytes)) = extract_source_ticket_from_vdf(&content, filter_id) {
                            return Some((app_id, bytes));
                        }
                    }
                }
            }
        }
    }

    // Fall back to checking global TicketStore for any ticket > 128 bytes (AppId 7 preferred, then 228980, 480)
    let mut ts = TicketStore::global().lock().unwrap_or_else(|e| e.into_inner());
    let _ = ts.load();
    for candidate_app in [7u32, 228980u32, 480u32] {
        if let Ok(data) = ts.get(candidate_app) {
            if let Some(raw) = data.app_ticket.as_deref().and_then(|h| hex::decode(h).ok()) {
                if raw.len() > K_APP_TICKET_SIGNATURE_SIZE {
                    return Some((candidate_app, raw));
                }
            }
        }
    }
    for app_id in ts.list_app_ids().unwrap_or_default() {
        if let Ok(data) = ts.get(app_id) {
            if let Some(raw) = data.app_ticket.as_deref().and_then(|h| hex::decode(h).ok()) {
                if raw.len() > K_APP_TICKET_SIGNATURE_SIZE {
                    return Some((app_id, raw));
                }
            }
        }
    }
    None
}

fn extract_source_ticket_from_vdf(content: &str, active_account_id: Option<u32>) -> Option<(u32, Vec<u8>)> {
    if let Some(sec_idx) = content.find("\"apptickets\"") {
        if let Some(open_rel) = content[sec_idx..].find('{') {
            let open = sec_idx + open_rel + 1;
            let close = content[open..].find('}').map(|c| open + c).unwrap_or(content.len());
            let slice = &content[open..close];
            let mut fallback: Option<(u32, Vec<u8>)> = None;
            for line in slice.lines() {
                let trimmed = line.trim();
                let mut parts = trimmed.split('"').filter(|p| !p.trim().is_empty());
                if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
                    if let Ok(app_id) = k.parse::<u32>() {
                        if let Ok(bytes) = hex::decode(v) {
                            if bytes.len() > K_APP_TICKET_SIGNATURE_SIZE {
                                // Match UsefulSteamTools AppTicket.cpp:
                                // Skip family-shared tickets where ticket SteamID account doesn't match active user
                                if let Some(aid) = active_account_id {
                                    let ticket_sid = extract_steam_id_from_ticket_bytes(&bytes);
                                    if aid != 0 && ticket_sid != 0 && ((ticket_sid & 0xFFFF_FFFF) as u32) != aid {
                                        continue;
                                    }
                                }
                                // Prefer kLocalAppTicketSourceAppId (7)
                                if app_id == K_LOCAL_APP_TICKET_SOURCE_APP_ID {
                                    return Some((app_id, bytes));
                                }
                                if fallback.is_none() {
                                    fallback = Some((app_id, bytes));
                                }
                            }
                        }
                    }
                }
            }
            return fallback;
        }
    }
    None
}

/// Performs the off-by-four RSA-1024 splice on a source ticket buffer (BST-49).
///
/// In SteamDRMP's ticket parsing, the signature check verifies `signed_size` bytes,
/// but an off-by-four overhang flaw reads the app_id spliced directly prior to the 128-byte
/// RSA signature block.
pub fn splice_ticket_off_by_four(
    source: &[u8],
    app_id: u32,
    steam_id: Option<u64>,
) -> Option<Vec<u8>> {
    if source.len() <= K_APP_TICKET_SIGNATURE_SIZE {
        return None;
    }
    let signed_size = source.len() - K_APP_TICKET_SIGNATURE_SIZE;
    let mut ticket = Vec::with_capacity(source.len() + 4);
    ticket.extend_from_slice(&source[..signed_size]);
    ticket.extend_from_slice(&app_id.to_le_bytes());
    ticket.extend_from_slice(&source[signed_size..]);
    if let Some(sid) = steam_id {
        if sid >= 0x0110_0001_0000_0000 && ticket.len() >= 16 {
            ticket[K_APP_TICKET_STEAM_ID_OFFSET..K_APP_TICKET_STEAM_ID_OFFSET + 8]
                .copy_from_slice(&sid.to_le_bytes());
        }
    }
    Some(ticket)
}

/// Forges an App Ownership Ticket using the SteamDRMP off-by-four RSA-1024 splice exploit (BST-49).
///
/// Splices `app_id` (4 bytes) directly before the 128-byte signature block of a valid donor ticket,
/// taking advantage of SteamDRMP's `signed_size + 4` buffer overhang parser quirk.
pub fn forge_local_app_ownership_ticket(
    app_id: u32,
    steam_root: Option<&Path>,
) -> Option<Vec<u8>> {
    forge_local_app_ownership_ticket_with_steam_id(app_id, None, steam_root)
}

/// Forges an App Ownership Ticket with optional donor SteamID override (BST-49).
pub fn forge_local_app_ownership_ticket_with_steam_id(
    app_id: u32,
    steam_id: Option<u64>,
    steam_root: Option<&Path>,
) -> Option<Vec<u8>> {
    if let Some((_src_app, source)) = read_localconfig_source_ticket(steam_root) {
        if let Some(spliced) = splice_ticket_off_by_four(&source, app_id, steam_id) {
            return Some(spliced);
        }
    }

    // Fallback: construct structurally valid forged ticket with 128-byte signature.
    // The fallback donor has signed_size = 24 and total size S = 24 + 128 = 152 bytes.
    // With the off-by-four splice exploit, the physical buffer has S + 4 = 156 bytes:
    // [0..24]: signed payload (size=24, version=4, steam_id, donor_app_id=7, now)
    // [24..28]: spliced target app_id at signed_size (points at piAppId = totalSize - 128 = 24)
    // [28..156]: 128-byte RSA-1024 signature block shifted by 4 bytes (piSignature = piAppId + 4 = 28)
    let sid = steam_id.unwrap_or(76_561_197_960_287_930u64);
    let signed_size = 24usize;
    let total_physical = signed_size + 4 + K_APP_TICKET_SIGNATURE_SIZE; // 156 bytes
    let mut buf = vec![0u8; total_physical];
    buf[0..4].copy_from_slice(&(signed_size as u32).to_le_bytes()); // size of signed region
    buf[4..8].copy_from_slice(&4u32.to_le_bytes()); // version 4
    buf[8..16].copy_from_slice(&sid.to_le_bytes());
    buf[16..20].copy_from_slice(&K_LOCAL_APP_TICKET_SOURCE_APP_ID.to_le_bytes()); // donor AppId 7
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32;
    buf[20..24].copy_from_slice(&now.to_le_bytes());
    buf[24..28].copy_from_slice(&app_id.to_le_bytes()); // spliced target AppId
    for i in 0..K_APP_TICKET_SIGNATURE_SIZE {
        buf[28 + i] = ((i * 37 + 13) & 0xFF) as u8;
    }
    Some(buf)
}

/// Get ownership ticket with offsets matching UsefulSteamTools GetAppOwnershipTicket (BST-50).
pub fn get_app_ownership_ticket_details(
    app_id: u32,
    source: AppTicketSource,
) -> Option<AppOwnershipTicketDetails> {
    if app_id == 0 {
        return None;
    }

    if source == AppTicketSource::CredentialStoreOnly || source == AppTicketSource::CredentialStoreThenForge {
        if let Ok(Some(ticket)) = read_app_ticket(app_id) {
            if let Some(raw) = ticket.app_ticket.as_deref().and_then(|s| hex::decode(s).ok()).filter(|b| b.len() >= 16) {
                let total_size = raw.len();
                let signature_offset = if total_size >= 4 {
                    let signed_len = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
                    if signed_len > 0 && signed_len + K_APP_TICKET_SIGNATURE_SIZE <= total_size {
                        signed_len
                    } else if total_size >= K_APP_TICKET_SIGNATURE_SIZE {
                        total_size - K_APP_TICKET_SIGNATURE_SIZE
                    } else {
                        0
                    }
                } else if total_size >= K_APP_TICKET_SIGNATURE_SIZE {
                    total_size - K_APP_TICKET_SIGNATURE_SIZE
                } else {
                    0
                };
                return Some(AppOwnershipTicketDetails {
                    data: raw,
                    total_size,
                    app_id_offset: K_APP_TICKET_APP_ID_OFFSET,
                    steam_id_offset: K_APP_TICKET_STEAM_ID_OFFSET,
                    signature_offset,
                    signature_size: K_APP_TICKET_SIGNATURE_SIZE,
                });
            }
        }
    }

    if source == AppTicketSource::CredentialStoreOnly {
        return None;
    }

    let forged = forge_local_app_ownership_ticket(app_id, None)?;
    let total_size = forged.len().saturating_sub(4);
    let app_id_offset = total_size.saturating_sub(K_APP_TICKET_SIGNATURE_SIZE);
    let signature_offset = app_id_offset + 4;
    Some(AppOwnershipTicketDetails {
        data: forged,
        total_size,
        app_id_offset,
        steam_id_offset: K_APP_TICKET_STEAM_ID_OFFSET,
        signature_offset,
        signature_size: K_APP_TICKET_SIGNATURE_SIZE,
    })
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

    #[test]
    fn test_extract_steam_id_from_ticket_bytes() {
        let sid = 76_561_198_028_121_353u64;
        let mut buf = vec![0u8; 32];
        buf[8..16].copy_from_slice(&sid.to_le_bytes());
        assert_eq!(extract_steam_id_from_ticket_bytes(&buf), sid);

        let mut buf_short = vec![0u8; 20];
        buf_short[4..12].copy_from_slice(&sid.to_le_bytes());
        assert_eq!(extract_steam_id_from_ticket_bytes(&buf_short), sid);

        assert_eq!(extract_steam_id_from_ticket_bytes(&[0u8; 8]), 0);
    }

    #[test]
    fn test_forge_local_app_ownership_ticket_structure() {
        let forged = forge_local_app_ownership_ticket(1245620, None).expect("forge should succeed");
        assert!(forged.len() >= 24 + 4 + K_APP_TICKET_SIGNATURE_SIZE);
        assert_eq!(forged.len(), 156);

        let details = get_app_ownership_ticket_details(1245620, AppTicketSource::CredentialStoreThenForge)
            .expect("details should exist");
        assert_eq!(details.signature_size, K_APP_TICKET_SIGNATURE_SIZE);
        assert_eq!(details.steam_id_offset, K_APP_TICKET_STEAM_ID_OFFSET);
        assert_eq!(details.total_size, 152);
        assert_eq!(details.app_id_offset, 24);
        assert_eq!(details.signature_offset, 28);
        assert_eq!(
            &details.data[details.app_id_offset..details.app_id_offset + 4],
            &1245620u32.to_le_bytes()
        );
        assert_eq!(details.data.len(), details.total_size + 4);
    }
}
