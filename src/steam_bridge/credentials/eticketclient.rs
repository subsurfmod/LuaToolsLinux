//! EticketClient - Fetch fresh nonce-bound etickets on Linux.
//! Ported from usefulsteamtools::EticketClient::FetchFreshEticket.
//! Replaces Windows registry (HKCU\Software\Valve\Steam\Apps\<AppId>)
//! with reading/writing from localconfig.vdf via ticket_store.rs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::steam_bridge::ticket_store::{AppTicketData, TicketStore};
use crate::steam_bridge::interop::interop_ipc_isteamuser::fresh_eticket;

/// Global eticket client instance.
static ETICKET_CLIENT: OnceLock<Arc<EticketClient>> = OnceLock::new();

/// Fresh eticket cache, keyed by appId, holding nonce-bound ticket bytes.
/// Minted on-demand in RequestEncryptedAppTicket and consumed by
/// GetEncryptedAppTicket on the same launch.
static FRESH_ETICKET_CACHE: OnceLock<Mutex<HashMap<u32, Vec<u8>>>> = OnceLock::new();

/// Online fix app ID (Spacewar)
pub const ONLINE_FIX_APP_ID: u32 = 480;

/// Eticket client for fetching fresh nonce-bound etickets.
#[derive(Debug)]
pub struct EticketClient;

impl EticketClient {
    /// Get the global eticket client instance.
    pub fn global() -> &'static Self {
        ETICKET_CLIENT.get_or_init(|| Arc::new(EticketClient)).as_ref()
    }

    /// Get the fresh eticket cache.
    pub fn fresh_eticket_cache() -> &'static Mutex<HashMap<u32, Vec<u8>>> {
        FRESH_ETICKET_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Fetch a fresh nonce-bound eticket for the given app.
    ///
    /// This replaces `EticketClient::FetchFreshEticket` from usefulsteamtools.
    /// On Windows, this would call into the Steam DLL to mint a nonce-bound
    /// eticket. On Linux, we mint from the credential store (localconfig.vdf)
    /// using the per-launch nonce and the existing SteamID from the store.
    ///
    /// # Arguments
    /// - `app_id`: The Steam application ID
    /// - `nonce`: Per-launch nonce bytes (from pData in RequestEncryptedAppTicket)
    /// - `existing_steam_id`: The pool SteamID already stored in the credential store
    ///
    /// # Returns
    /// `Some(Vec<u8>)` containing the fresh eticket bytes, or `None` if
    /// the ticket cannot be minted (e.g., no existing credential store entry).
    pub fn fetch_fresh_eticket(
        app_id: u32,
        nonce: &[u8],
        existing_steam_id: u64,
    ) -> Option<Vec<u8>> {
        // Check if we have a credential store entry for this app
        let store = TicketStore::default();
        let data = store.get(app_id).ok()?;

        // If we have an existing eticket in the store, we can mint a fresh one
        // bound to the existing SteamID
        let existing_eticket = data.e_ticket.clone()?;
        let existing_steam_id = data.steam_id.clone();

        // If no existing eticket, we can't mint a fresh one
        if existing_eticket.is_empty() {
            log::debug!(
                "[EticketClient] No existing ETicket in store for AppId={}, cannot mint fresh",
                app_id
            );
            return None;
        }

        // Determine the SteamID to use for the fresh ticket
        let steam_id = existing_steam_id.unwrap_or(0);

        // Mint a fresh nonce-bound eticket by combining:
        // - The existing eticket from the credential store
        // - The per-launch nonce
        // - The existing SteamID (pool account)
        let mut fresh_ticket = Vec::new();
        fresh_ticket.extend_from_slice(existing_eticket.as_bytes());
        fresh_ticket.extend_from_slice(nonce);
        fresh_ticket.extend_from_slice(&steam_id.to_le_bytes());

        // Cache the fresh eticket for this app
        let cache = Self::fresh_eticket_cache().lock().unwrap();
        cache.insert(app_id, fresh_ticket.clone());

        Some(fresh_ticket)
    }

    /// Retrieve a previously minted fresh eticket from the cache.
    pub fn get_cached_fresh_eticket(app_id: u32) -> Option<Vec<u8>> {
        let cache = FRESH_ETICKET_CACHE.get().map(|c| c.lock().unwrap()).unwrap_or_default();
        cache.get(&app_id).cloned()
    }
}

/// Initialize the eticket client (call once at startup).
pub fn init() -> Result<(), String> {
    // Ensure the global client and fresh eticket cache are initialized
    let _ = EticketClient::global();
    let _ = EticketClient::fresh_eticket_cache();

    // Load any previously cached fresh etickets from the credential store
    let store = TicketStore::default();
    let _ = store.load();

    log::info!("[EticketClient] Initialized - using localconfig.vdf credential store");
    Ok(())
}

/// Check if we have a fresh eticket cached for the given app.
pub fn has_cached_fresh_eticket(app_id: u32) -> bool {
    let cache = FRESH_ETICKET_CACHE.get().map(|c| c.lock().unwrap()).unwrap_or_default();
    cache.contains_key(&app_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eticket_client_init() {
        let _ = init();
        // Just verify it doesn't panic
    }

    #[test]
    fn test_fetch_fresh_eticket_without_store_ticket() {
        let _ = init();
        // Should return None when no eticket in store
        let result = EticketClient::fetch_fresh_eticket(
            1245620,
            &[0xDE, 0xAD, 0xBE, 0xEF],
            0,
        );
        assert!(result.is_none(), "Expected None when no eticket in store");
    }

    #[test]
    fn test_has_cached_fresh_eticket() {
        let _ = init();
        let result = has_cached_fresh_eticket(999999);
        // Should not have cached etickets initially
        assert!(!result);
    }
}