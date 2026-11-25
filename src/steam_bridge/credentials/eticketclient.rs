//! EticketClient - Fetch fresh nonce-bound etickets on Linux.
//! Ported from `usefulsteamtools::EticketClient::FetchFreshEticket`.
//! Replaces Windows registry (`HKCU\Software\Valve\Steam\Apps\<AppId>`)
//! with reading/writing from `localconfig.vdf` via `ticket_store.rs`.

use std::collections::HashMap;
use std::ffi::CString;
use std::os::raw::c_void;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use crate::steam_bridge::ost_platform::http;
use crate::steam_bridge::ticket_store::TicketStore;

const MAX_ETICKET_RESPONSE_BYTES: u32 = 64 * 1024;
const ETICKET_HTTP_TIMEOUT_MS: u32 = 2_000;

fn is_http_url(url: &str) -> bool {
    url.get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("http://"))
        || url
            .get(..8)
            .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
}

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

    /// Fetch a fresh nonce-bound eticket for the given app from a specific Steam config directory.
    pub fn fetch_fresh_eticket_from(
        config_dir: &Path,
        app_id: u32,
        nonce: &[u8],
        fallback_steam_id: u64,
    ) -> Option<Vec<u8>> {
        if app_id == 0 {
            return None;
        }
        let mut store = TicketStore::new(config_dir);
        let data = store.get(app_id).ok()?;

        let existing_eticket = data.e_ticket.as_deref()?.trim();
        if existing_eticket.is_empty() {
            log::debug!(
                "[EticketClient] No existing ETicket in store for AppId={}, cannot mint fresh",
                app_id
            );
            return None;
        }

        // TicketStore persists binary tickets as hex strings; decode hex bytes when valid,
        // falling back to raw bytes only for legacy non-hex values.
        let base_bytes =
            hex::decode(existing_eticket).unwrap_or_else(|_| existing_eticket.as_bytes().to_vec());
        if base_bytes.is_empty() {
            return None;
        }

        let steam_id = data
            .steam_id
            .as_deref()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&sid| sid != 0)
            .unwrap_or(fallback_steam_id);

        let mut fresh_ticket = Vec::with_capacity(base_bytes.len() + nonce.len() + 8);
        fresh_ticket.extend_from_slice(&base_bytes);
        fresh_ticket.extend_from_slice(nonce);
        fresh_ticket.extend_from_slice(&steam_id.to_le_bytes());

        let mut cache = Self::fresh_eticket_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cache.insert(app_id, fresh_ticket.clone());

        Some(fresh_ticket)
    }

    /// Fetch a fresh nonce-bound eticket for the given app from the default Steam config directory.
    pub fn fetch_fresh_eticket(
        app_id: u32,
        nonce: &[u8],
        fallback_steam_id: u64,
    ) -> Option<Vec<u8>> {
        Self::fetch_fresh_eticket_from(
            &TicketStore::default_path(),
            app_id,
            nonce,
            fallback_steam_id,
        )
    }

    /// Fetch a fresh nonce-bound eticket from a remote HTTP/HTTPS endpoint via `ost_platform::http::Execute`.
    pub fn fetch_remote_eticket(
        endpoint_url: &str,
        app_id: u32,
        nonce: &[u8],
        steam_id: u64,
    ) -> Option<Vec<u8>> {
        let base_url = endpoint_url.trim();
        if app_id == 0 || !is_http_url(base_url) {
            return None;
        }

        let target_url = format!("{}/eticket/{}", base_url.trim_end_matches('/'), app_id);
        let payload = serde_json::json!({
            "app_id": app_id,
            "steam_id": steam_id.to_string(),
            "nonce": hex::encode(nonce),
        });
        let body_str = payload.to_string();
        let body_bytes = body_str.as_bytes();
        let body_len = u32::try_from(body_bytes.len()).ok()?;
        let headers = CString::new("Content-Type: application/json\r\nAccept: application/json, text/plain\r\n").ok()?;

        let resp = http::Execute(
            "POST",
            &target_url,
            body_bytes.as_ptr() as *const c_void,
            body_len,
            headers.as_ptr(),
            ETICKET_HTTP_TIMEOUT_MS,
            ETICKET_HTTP_TIMEOUT_MS,
            ETICKET_HTTP_TIMEOUT_MS,
            ETICKET_HTTP_TIMEOUT_MS,
            MAX_ETICKET_RESPONSE_BYTES,
        );

        if !resp.ok {
            log::debug!(
                "[EticketClient] Remote eticket fetch failed for AppId={} (status={})",
                app_id,
                resp.status
            );
            return None;
        }

        let trimmed = resp.body.trim();
        if trimmed.is_empty() {
            return None;
        }

        let base_bytes = if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let raw = json
                .get("e_ticket")
                .or_else(|| json.get("ticket"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())?;
            hex::decode(raw).unwrap_or_else(|_| raw.as_bytes().to_vec())
        } else {
            hex::decode(trimmed).unwrap_or_else(|_| trimmed.as_bytes().to_vec())
        };

        if base_bytes.is_empty() {
            return None;
        }

        let mut fresh_ticket = Vec::with_capacity(base_bytes.len() + nonce.len() + 8);
        fresh_ticket.extend_from_slice(&base_bytes);
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

    /// Remove a cached fresh eticket entry for an app.
    pub fn clear_cached_fresh_eticket(app_id: u32) {
        Self::fresh_eticket_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&app_id);
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
    use crate::steam_bridge::ticket_store::AppTicketData;

    #[test]
    fn test_eticket_client_init() {
        assert!(init().is_ok());
    }

    #[test]
    fn test_fetch_fresh_eticket_without_store_ticket() {
        let tmp = tempfile::tempdir().unwrap();
        let result = EticketClient::fetch_fresh_eticket_from(
            tmp.path(),
            999_999_999,
            &[0xDE, 0xAD, 0xBE, 0xEF],
            0,
        );
        assert!(result.is_none(), "Expected None when no eticket in store");
        assert!(!has_cached_fresh_eticket(999_999_999));
    }

    #[test]
    fn test_fetch_fresh_eticket_decodes_hex_and_caches_result() {
        let tmp = tempfile::tempdir().unwrap();
        let app_id = 888_777_666;
        let steam_id = 76_561_198_028_121_353u64;

        let mut store = TicketStore::new(tmp.path());
        store
            .set(AppTicketData {
                app_id,
                steam_id: Some(steam_id.to_string()),
                app_ticket: None,
                e_ticket: Some("cafebabe".into()),
                last_update: None,
            })
            .unwrap();

        let nonce = [0x01, 0x02, 0x03, 0x04];
        let minted =
            EticketClient::fetch_fresh_eticket_from(tmp.path(), app_id, &nonce, 42).unwrap();

        let mut expected = vec![0xCA, 0xFE, 0xBA, 0xBE];
        expected.extend_from_slice(&nonce);
        expected.extend_from_slice(&steam_id.to_le_bytes());
        assert_eq!(minted, expected);
        assert!(has_cached_fresh_eticket(app_id));
        assert_eq!(
            EticketClient::get_cached_fresh_eticket(app_id),
            Some(expected)
        );

        EticketClient::clear_cached_fresh_eticket(app_id);
        assert!(!has_cached_fresh_eticket(app_id));
    }

    #[test]
    fn test_has_cached_fresh_eticket() {
        let _ = init();
        assert!(!has_cached_fresh_eticket(999_999_998));
    }

    #[test]
    fn test_fetch_remote_eticket_rejects_invalid_scheme_and_closed_port() {
        assert!(EticketClient::fetch_remote_eticket(
            "ftp://invalid.example",
            730,
            &[1, 2],
            76_561_198_028_121_353
        )
        .is_none());

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        assert!(EticketClient::fetch_remote_eticket(
            &format!("http://{}", addr),
            730,
            &[1, 2],
            76_561_198_028_121_353
        )
        .is_none());
    }

    #[test]
    fn test_fetch_remote_eticket_local_http_caches() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut req_buf = [0u8; 1024];
                let _ = stream.read(&mut req_buf);
                let body = r#"{"e_ticket":"cafebabe"}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        let app_id = 888_777_665;
        let steam_id = 76_561_198_028_121_353u64;
        let nonce = [0x09, 0x0A];
        let minted =
            EticketClient::fetch_remote_eticket(&format!("http://{}", addr), app_id, &nonce, steam_id)
                .unwrap();

        let mut expected = vec![0xCA, 0xFE, 0xBA, 0xBE];
        expected.extend_from_slice(&nonce);
        expected.extend_from_slice(&steam_id.to_le_bytes());
        assert_eq!(minted, expected);
        assert_eq!(EticketClient::get_cached_fresh_eticket(app_id), Some(expected));

        EticketClient::clear_cached_fresh_eticket(app_id);
        handle.join().unwrap();
    }
}
