//! EticketClient - Fetch fresh nonce-bound etickets on Linux.
//! Ported from `usefulsteamtools::EticketClient::FetchFreshEticket`.
//! Replaces Windows registry (`HKCU\Software\Valve\Steam\Apps\<AppId>`)
//! with reading/writing from `localconfig.vdf` via `ticket_store.rs`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::steam_bridge::ticket_store::TicketStore;

/// Global eticket client instance.
static ETICKET_CLIENT: OnceLock<Arc<EticketClient>> = OnceLock::new();

/// Fresh eticket cache, keyed by `app_id`, holding nonce-bound ticket bytes.
static FRESH_ETICKET_CACHE: OnceLock<Mutex<HashMap<u32, Vec<u8>>>> = OnceLock::new();

/// Online fix app ID (Spacewar).
pub const ONLINE_FIX_APP_ID: u32 = 480;

/// Eticket client for fetching fresh nonce-bound etickets.
#[derive(Debug)]
pub struct EticketClient;

impl EticketClient {
    /// Get the global eticket client instance.
    pub fn global() -> &'static Self {
        ETICKET_CLIENT
            .get_or_init(|| Arc::new(EticketClient))
            .as_ref()
    }

    /// Get the fresh eticket cache.
    pub fn fresh_eticket_cache() -> &'static Mutex<HashMap<u32, Vec<u8>>> {
        FRESH_ETICKET_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Fetch a fresh nonce-bound eticket for the given app.
    pub fn fetch_fresh_eticket(
        app_id: u32,
        nonce: &[u8],
        fallback_steam_id: u64,
    ) -> Option<Vec<u8>> {
        let mut store = TicketStore::default();
        let data = store.get(app_id).ok()?;

        let existing_eticket = data.e_ticket.clone()?;
        if existing_eticket.is_empty() {
            log::debug!(
                "[EticketClient] No existing ETicket in store for AppId={}, cannot mint fresh",
                app_id
            );
            return None;
        }

        let steam_id = data
            .steam_id
            .as_deref()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(fallback_steam_id);

        let mut fresh_ticket = Vec::new();
        fresh_ticket.extend_from_slice(existing_eticket.as_bytes());
        fresh_ticket.extend_from_slice(nonce);
        fresh_ticket.extend_from_slice(&steam_id.to_le_bytes());

        let mut cache = Self::fresh_eticket_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cache.insert(app_id, fresh_ticket.clone());

        Some(fresh_ticket)
    }

    /// Retrieve a previously minted fresh eticket from the cache.
    pub fn get_cached_fresh_eticket(app_id: u32) -> Option<Vec<u8>> {
        let cache = Self::fresh_eticket_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cache.get(&app_id).cloned()
    }
}

/// Initialize the eticket client (call once at startup).
pub fn init() -> Result<(), String> {
    let _ = EticketClient::global();
    let _ = EticketClient::fresh_eticket_cache();

    let mut store = TicketStore::default();
    let _ = store.load();

    log::info!("[EticketClient] Initialized - using localconfig.vdf credential store");
    Ok(())
}

/// Check if we have a fresh eticket cached for the given app.
pub fn has_cached_fresh_eticket(app_id: u32) -> bool {
    let cache = EticketClient::fresh_eticket_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    cache.contains_key(&app_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eticket_client_init() {
        assert!(init().is_ok());
    }

    #[test]
    fn test_fetch_fresh_eticket_without_store_ticket() {
        let _ = init();
        let result = EticketClient::fetch_fresh_eticket(999_999_999, &[0xDE, 0xAD, 0xBE, 0xEF], 0);
        assert!(result.is_none(), "Expected None when no eticket in store");
    }

    #[test]
    fn test_has_cached_fresh_eticket() {
        let _ = init();
        assert!(!has_cached_fresh_eticket(999_999_999));
    }
}
