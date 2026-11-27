//! EticketClient - Fetch fresh nonce-bound etickets on Linux.
//! Ported from `usefulsteamtools::EticketClient::FetchFreshEticket`.
//!
//! Persists custom runtime `seteticketurl(...)` endpoints atomically (`0o600`) to
//! `~/.local/share/luatools/eticket_endpoint.json` so `bridge_lib` and `luatools`
//! share endpoint state across processes, decodes JSON (`{"eticket":"<hex|b64>"}`),
//! hex, Base64, and raw binary responses without corrupting cryptographic signatures,
//! and safely returns validated base `ETicket` bytes when remote minting is unavailable.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::steam_bridge::ticket_store::TicketStore;

const MAX_ETICKET_RESPONSE_BYTES: usize = 64 * 1024;
const ETICKET_HTTP_TIMEOUT_MS: u64 = 2_000;
const ETICKET_ENDPOINT_FILENAME: &str = "eticket_endpoint.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedEticketEndpoint {
    url: String,
}

fn is_http_url(url: &str) -> bool {
    url.get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("http://"))
        || url
            .get(..8)
            .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
}

fn decode_encoded_ticket_str(raw: &str) -> Option<Vec<u8>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let hex_cand = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    if !hex_cand.is_empty()
        && hex_cand.len() % 2 == 0
        && hex_cand.bytes().all(|b| b.is_ascii_hexdigit())
    {
        if let Ok(bytes) = hex::decode(hex_cand) {
            if !bytes.is_empty() {
                return Some(bytes);
            }
        }
    }

    // Standard / URL-safe Base64 (require valid Base64 alphabet characters without whitespace/punctuation)
    let looks_like_b64 = trimmed.len() >= 4
        && trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_'));
    if looks_like_b64 {
        for engine in [
            &base64::engine::general_purpose::STANDARD,
            &base64::engine::general_purpose::STANDARD_NO_PAD,
            &base64::engine::general_purpose::URL_SAFE,
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        ] {
            if let Ok(bytes) = engine.decode(trimmed) {
                if !bytes.is_empty() {
                    return Some(bytes);
                }
            }
        }
    }

    None
}

fn decode_remote_eticket_payload(
    raw_bytes: &[u8],
    content_type: Option<&str>,
) -> Option<Vec<u8>> {
    if raw_bytes.is_empty() || raw_bytes.len() > MAX_ETICKET_RESPONSE_BYTES {
        return None;
    }

    if content_type.is_some_and(|ct| {
        ct.trim()
            .to_ascii_lowercase()
            .starts_with("application/octet-stream")
    }) {
        return Some(raw_bytes.to_vec());
    }

    let Ok(utf8_text) = std::str::from_utf8(raw_bytes) else {
        // Non-UTF-8 payload is a raw binary encrypted ticket
        return Some(raw_bytes.to_vec());
    };

    // Binary control bytes (e.g. NUL / binary framing bytes) indicate raw binary ticket data
    if raw_bytes
        .iter()
        .any(|&b| b < 0x20 && !matches!(b, b'\t' | b'\r' | b'\n'))
    {
        return Some(raw_bytes.to_vec());
    }

    let trimmed = utf8_text.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with('{') {
        let json: serde_json::Value = serde_json::from_str(trimmed).ok()?;
        let field = json
            .get("eticket")
            .or_else(|| json.get("e_ticket"))
            .or_else(|| json.get("ticket"))
            .or_else(|| json.get("data"))?;
        if let Some(s) = field.as_str() {
            return decode_encoded_ticket_str(s);
        }
        if let Some(arr) = field.as_array() {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                let n = item.as_u64()?;
                let b = u8::try_from(n).ok()?;
                out.push(b);
            }
            if !out.is_empty() {
                return Some(out);
            }
        }
        return None;
    }

    decode_encoded_ticket_str(trimmed)
}

fn write_endpoint_file_atomic_0600(path: &Path, url: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_string_pretty(&PersistedEticketEndpoint {
        url: url.to_string(),
    })
    .map_err(std::io::Error::other)?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(ETICKET_ENDPOINT_FILENAME);
    static TMP_ETICKET_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let count = TMP_ETICKET_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp_path = path.with_file_name(format!(".{}.tmp.{}.{}", file_name, std::process::id(), count));
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp_path)?;
    f.write_all(payload.as_bytes())?;
    f.flush()?;
    drop(f);
    fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600))?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn read_endpoint_file(path: &Path) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(parsed) = serde_json::from_str::<PersistedEticketEndpoint>(trimmed) {
        let u = parsed.url.trim().to_string();
        if is_http_url(&u) {
            return Some(u);
        }
        return None;
    }
    if let Ok(serde_json::Value::String(s)) = serde_json::from_str::<serde_json::Value>(trimmed) {
        let u = s.trim().to_string();
        if is_http_url(&u) {
            return Some(u);
        }
        return None;
    }
    is_http_url(trimmed).then(|| trimmed.to_string())
}

/// Global eticket client instance.
static ETICKET_CLIENT: OnceLock<Arc<EticketClient>> = OnceLock::new();

/// Fresh eticket cache, keyed by `app_id`, holding ticket bytes.
static FRESH_ETICKET_CACHE: OnceLock<Mutex<HashMap<u32, Vec<u8>>>> = OnceLock::new();

/// Process-wide in-memory cache of the custom eticket endpoint (`BST-07`).
static CUSTOM_ETICKET_URL: OnceLock<Mutex<Option<String>>> = OnceLock::new();

thread_local! {
    /// Scoped `(endpoint_file_path, scoped_memory_cache)` override for deterministic isolated tests.
    static ENDPOINT_FILE_OVERRIDE: RefCell<Option<(PathBuf, Option<String>)>> =
        const { RefCell::new(None) };
}

struct EndpointOverrideGuard {
    prev: Option<(PathBuf, Option<String>)>,
}

impl Drop for EndpointOverrideGuard {
    fn drop(&mut self) {
        let prev = self.prev.take();
        ENDPOINT_FILE_OVERRIDE.with(|slot| {
            *slot.borrow_mut() = prev;
        });
    }
}

/// Online fix app ID (Spacewar).
pub const ONLINE_FIX_APP_ID: u32 = 480;

/// Eticket client for fetching fresh nonce-bound etickets.
#[derive(Debug)]
pub struct EticketClient;

impl EticketClient {
    /// Canonical persistent endpoint state file (`~/.local/share/luatools/eticket_endpoint.json`).
    pub fn default_endpoint_file() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".local/share/luatools")
            .join(ETICKET_ENDPOINT_FILENAME)
    }

    /// Scopes `set_custom_url` / `get_custom_url` / `clear_custom_url` to `path` with a fresh
    /// in-memory cache for isolated unit tests.
    pub fn with_endpoint_file<F, R>(path: &Path, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let prev = ENDPOINT_FILE_OVERRIDE.with(|slot| {
            slot.borrow_mut().replace((path.to_path_buf(), None))
        });
        let _guard = EndpointOverrideGuard { prev };
        f()
    }

    /// Clears only the in-memory endpoint cache without deleting the persisted disk file,
    /// allowing callers/tests to force lazy reload from `eticket_endpoint.json`.
    pub fn reset_custom_url_memory_cache() {
        let handled_scoped = ENDPOINT_FILE_OVERRIDE.with(|slot| {
            let mut borrow = slot.borrow_mut();
            if let Some((_, ref mut cached)) = *borrow {
                *cached = None;
                true
            } else {
                false
            }
        });
        if !handled_scoped {
            let mut lock = CUSTOM_ETICKET_URL
                .get_or_init(|| Mutex::new(None))
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *lock = None;
        }
    }

    /// Configure runtime on-demand eticket endpoint and persist it atomically (`0o600`) to disk (`BST-07`).
    pub fn set_custom_url(url: &str) {
        let trimmed = url.trim();
        if !is_http_url(trimmed) {
            return;
        }

        let scoped_path = ENDPOINT_FILE_OVERRIDE.with(|slot| {
            let mut borrow = slot.borrow_mut();
            if let Some((ref path, ref mut cached)) = *borrow {
                *cached = Some(trimmed.to_string());
                Some(path.clone())
            } else {
                None
            }
        });

        let target_path = if let Some(p) = scoped_path {
            p
        } else {
            let mut lock = CUSTOM_ETICKET_URL
                .get_or_init(|| Mutex::new(None))
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *lock = Some(trimmed.to_string());
            Self::default_endpoint_file()
        };

        if let Err(e) = write_endpoint_file_atomic_0600(&target_path, trimmed) {
            log::warn!(
                "[EticketClient] Failed to persist eticket endpoint to {}: {}",
                target_path.display(),
                e
            );
        }
    }

    /// Returns the currently active runtime on-demand eticket endpoint (`BST-07`), checking:
    /// 1. In-memory cache
    /// 2. `LUATOOLS_ETICKET_URL` environment variable
    /// 3. Lazy load from `~/.local/share/luatools/eticket_endpoint.json`
    pub fn get_custom_url() -> Option<String> {
        let scoped_state = ENDPOINT_FILE_OVERRIDE.with(|slot| slot.borrow().clone());
        if let Some((path, cached)) = scoped_state {
            if cached.is_some() {
                return cached;
            }
            if let Ok(env_val) = std::env::var("LUATOOLS_ETICKET_URL") {
                let env_trimmed = env_val.trim();
                if is_http_url(env_trimmed) {
                    return Some(env_trimmed.to_string());
                }
            }
            if let Some(loaded) = read_endpoint_file(&path) {
                ENDPOINT_FILE_OVERRIDE.with(|slot| {
                    if let Some((_, ref mut mem)) = *slot.borrow_mut() {
                        *mem = Some(loaded.clone());
                    }
                });
                return Some(loaded);
            }
            return None;
        }

        {
            let lock = CUSTOM_ETICKET_URL
                .get_or_init(|| Mutex::new(None))
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(ref u) = *lock {
                return Some(u.clone());
            }
        }

        if let Ok(env_val) = std::env::var("LUATOOLS_ETICKET_URL") {
            let env_trimmed = env_val.trim();
            if is_http_url(env_trimmed) {
                return Some(env_trimmed.to_string());
            }
        }

        let loaded = read_endpoint_file(&Self::default_endpoint_file())?;
        let mut lock = CUSTOM_ETICKET_URL
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *lock = Some(loaded.clone());
        Some(loaded)
    }

    /// Reset any custom eticket endpoint in memory and remove the persisted endpoint file.
    pub fn clear_custom_url() {
        let scoped_path = ENDPOINT_FILE_OVERRIDE.with(|slot| {
            let mut borrow = slot.borrow_mut();
            if let Some((ref path, ref mut cached)) = *borrow {
                *cached = None;
                Some(path.clone())
            } else {
                None
            }
        });

        let target_path = if let Some(p) = scoped_path {
            p
        } else {
            let mut lock = CUSTOM_ETICKET_URL
                .get_or_init(|| Mutex::new(None))
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *lock = None;
            Self::default_endpoint_file()
        };

        if target_path.exists() {
            let _ = fs::remove_file(&target_path);
        }
    }

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
    ///
    /// 1. If a custom remote endpoint is active (`seteticketurl` / `LUATOOLS_ETICKET_URL`),
    ///    requests a signed nonce-bound ticket from that endpoint.
    /// 2. If unset or unreachable, falls back cleanly to the validated decoded base `ETicket`
    ///    in `TicketStore` **without** appending raw `nonce` or `steam_id` bytes (preserving
    ///    Valve's cryptographic RSA ticket structure).
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
        let stored_data = store.get(app_id).ok();

        let effective_steam_id = stored_data
            .as_ref()
            .and_then(|d| d.steam_id.as_deref())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|&sid| sid != 0)
            .unwrap_or(fallback_steam_id);

        // 1. Try remote on-demand eticket endpoint if configured (BST-07 / BST-56)
        if let Some(custom_url) = Self::get_custom_url() {
            if let Some(fresh) =
                Self::fetch_remote_eticket(&custom_url, app_id, nonce, effective_steam_id)
            {
                return Some(fresh);
            }
        }

        // 2. Fallback: return validated decoded base ETicket bytes cleanly (never append nonce/steam_id)
        let data = stored_data?;
        let existing_eticket = data.e_ticket.as_deref()?.trim();
        if existing_eticket.is_empty() {
            log::debug!(
                "[EticketClient] No existing ETicket in store for AppId={}",
                app_id
            );
            return None;
        }

        let base_bytes = decode_encoded_ticket_str(existing_eticket)?;

        let mut cache = Self::fresh_eticket_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cache.insert(app_id, base_bytes.clone());

        Some(base_bytes)
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

    /// Fetch a fresh nonce-bound eticket from a remote HTTP/HTTPS endpoint (`BST-56`).
    ///
    /// Supports JSON (`{"eticket":"<hex|b64>"}`, `{"e_ticket":"..."}`, `{"ticket":"..."}`),
    /// plain hex, Base64, and raw binary (`application/octet-stream` / binary bytes) responses
    /// without corrupting the returned signed ticket bytes.
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

        let timeout = Duration::from_millis(ETICKET_HTTP_TIMEOUT_MS);
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(timeout)
            .timeout_write(timeout)
            .timeout_read(timeout)
            .build();

        let resp = match agent
            .post(&target_url)
            .set("Content-Type", "application/json")
            .set(
                "Accept",
                "application/json, application/octet-stream, text/plain",
            )
            .send_bytes(body_str.as_bytes())
        {
            Ok(r) => r,
            Err(e) => {
                log::debug!(
                    "[EticketClient] Remote eticket fetch failed for AppId={}: {}",
                    app_id,
                    e
                );
                return None;
            }
        };

        let status = resp.status();
        if !(200..300).contains(&status) {
            return None;
        }

        let content_type = resp.header("Content-Type").map(str::to_string);
        let mut raw_bytes = Vec::new();
        let cap = (MAX_ETICKET_RESPONSE_BYTES as u64).saturating_add(1);
        if resp.into_reader().take(cap).read_to_end(&mut raw_bytes).is_err() {
            return None;
        }

        let ticket_bytes = decode_remote_eticket_payload(&raw_bytes, content_type.as_deref())?;

        let mut cache = Self::fresh_eticket_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cache.insert(app_id, ticket_bytes.clone());

        Some(ticket_bytes)
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
    let _ = EticketClient::get_custom_url();

    let mut store = TicketStore::default();
    let _ = store.load();

    log::info!("[EticketClient] Initialized - using localconfig.vdf + sidecar credential store");
    Ok(())
}

/// Check if we have a fresh eticket cached for the given app.
pub fn has_cached_fresh_eticket(app_id: u32) -> bool {
    let cache = EticketClient::fresh_eticket_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    cache.contains_key(&app_id)
}

/// Sets the runtime on-demand eticket endpoint from `seteticketurl` in Lua scripts (BST-07).
pub fn set_eticket_url(url: &str) {
    EticketClient::set_custom_url(url);
}

/// Gets the currently configured runtime on-demand eticket endpoint (BST-07).
pub fn get_eticket_url() -> Option<String> {
    EticketClient::get_custom_url()
}

/// Clears any configured runtime on-demand eticket endpoint (BST-07).
pub fn clear_eticket_url() {
    EticketClient::clear_custom_url();
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
        let ep_file = tmp.path().join("eticket_endpoint.json");
        EticketClient::with_endpoint_file(&ep_file, || {
            let result = EticketClient::fetch_fresh_eticket_from(
                tmp.path(),
                999_999_999,
                &[0xDE, 0xAD, 0xBE, 0xEF],
                0,
            );
            assert!(result.is_none(), "Expected None when no eticket in store");
            assert!(!has_cached_fresh_eticket(999_999_999));
        });
    }

    #[test]
    fn test_fetch_fresh_eticket_preserves_uncorrupted_base_ticket_and_rejects_invalid() {
        let tmp = tempfile::tempdir().unwrap();
        let ep_file = tmp.path().join("eticket_endpoint.json");
        let app_id = 888_777_666;
        let steam_id = 76_561_198_028_121_353u64;

        EticketClient::with_endpoint_file(&ep_file, || {
            let mut store = TicketStore::new(tmp.path());
            store
                .set(AppTicketData {
                    app_id,
                    steam_id: Some(steam_id.to_string()),
                    app_ticket: None,
                    e_ticket: Some("cafebabe0102".into()),
                    last_update: None,
                })
                .unwrap();

            let nonce = [0x01, 0x02, 0x03, 0x04];
            let decoded =
                EticketClient::fetch_fresh_eticket_from(tmp.path(), app_id, &nonce, 42).unwrap();

            // Must NOT append raw nonce + steam_id onto stored ticket!
            let expected = vec![0xCA, 0xFE, 0xBA, 0xBE, 0x01, 0x02];
            assert_eq!(decoded, expected);
            assert!(has_cached_fresh_eticket(app_id));
            assert_eq!(
                EticketClient::get_cached_fresh_eticket(app_id),
                Some(expected)
            );

            EticketClient::clear_cached_fresh_eticket(app_id);
            assert!(!has_cached_fresh_eticket(app_id));

            // Invalid corrupted string must return None rather than raw ASCII bytes
            let bad_app_id = 888_777_667;
            store
                .set(AppTicketData {
                    app_id: bad_app_id,
                    steam_id: Some(steam_id.to_string()),
                    app_ticket: None,
                    e_ticket: Some("not-hex-or-b64!!!".into()),
                    last_update: None,
                })
                .unwrap();
            assert!(
                EticketClient::fetch_fresh_eticket_from(tmp.path(), bad_app_id, &nonce, 42)
                    .is_none()
            );
        });
    }

    #[test]
    fn test_custom_url_persists_0600_and_lazy_loads_across_processes() {
        let tmp = tempfile::tempdir().unwrap();
        let ep_file = tmp.path().join("luatools/eticket_endpoint.json");

        // Process 1 (e.g. lua_installer) writes custom URL
        EticketClient::with_endpoint_file(&ep_file, || {
            assert!(EticketClient::get_custom_url().is_none());
            EticketClient::set_custom_url("ftp://rejected.example");
            assert!(EticketClient::get_custom_url().is_none());
            assert!(!ep_file.exists());

            EticketClient::set_custom_url("https://tickets.example.org/v1");
            assert_eq!(
                EticketClient::get_custom_url().as_deref(),
                Some("https://tickets.example.org/v1")
            );
            assert!(ep_file.is_file());
            let mode = fs::metadata(&ep_file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        });

        // Process 2 (e.g. bridge_lib) starts with cold memory cache (`None`) and lazily loads disk file
        EticketClient::with_endpoint_file(&ep_file, || {
            EticketClient::reset_custom_url_memory_cache();
            assert_eq!(
                EticketClient::get_custom_url().as_deref(),
                Some("https://tickets.example.org/v1")
            );

            EticketClient::clear_custom_url();
            assert!(EticketClient::get_custom_url().is_none());
            assert!(!ep_file.exists());
        });
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
    fn test_fetch_remote_eticket_supports_json_hex_b64_and_raw_binary_without_corruption() {
        use std::net::{Shutdown, TcpListener, TcpStream};

        fn drain_full_http_request(s: &mut TcpStream) {
            let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = Vec::with_capacity(1024);
            let mut chunk = [0u8; 512];
            let mut expected_total: Option<usize> = None;

            loop {
                match s.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if expected_total.is_none() {
                            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                let hdr_end = pos + 4;
                                let hdr_str = String::from_utf8_lossy(&buf[..pos]);
                                let content_len = hdr_str
                                    .lines()
                                    .find_map(|line| {
                                        let (k, v) = line.split_once(':')?;
                                        if k.trim().eq_ignore_ascii_case("content-length") {
                                            v.trim().parse::<usize>().ok()
                                        } else {
                                            None
                                        }
                                    })
                                    .unwrap_or(0);
                                expected_total = Some(hdr_end + content_len);
                            }
                        }
                        if let Some(total) = expected_total {
                            if buf.len() >= total {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        }

        let raw_ticket_bin: Vec<u8> = vec![0xFF, 0x00, 0xCA, 0xFE, 0xBA, 0xBE, 0x80, 0x42];
        let b64_ticket = base64::engine::general_purpose::STANDARD.encode(&raw_ticket_bin);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let bin_for_srv = raw_ticket_bin.clone();
        let b64_for_srv = b64_ticket.clone();

        let handle = std::thread::spawn(move || {
            // 1. JSON {"eticket": "<hex>"}
            if let Ok((mut s, _)) = listener.accept() {
                drain_full_http_request(&mut s);
                let body = r#"{"eticket":"cafebabe"}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s.write_all(resp.as_bytes());
                let _ = s.flush();
                let _ = s.shutdown(Shutdown::Write);
            }
            // 2. JSON {"eticket": "<base64>"}
            if let Ok((mut s, _)) = listener.accept() {
                drain_full_http_request(&mut s);
                let body = format!(r#"{{"eticket":"{}"}}"#, b64_for_srv);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s.write_all(resp.as_bytes());
                let _ = s.flush();
                let _ = s.shutdown(Shutdown::Write);
            }
            // 3. Raw binary application/octet-stream
            if let Ok((mut s, _)) = listener.accept() {
                drain_full_http_request(&mut s);
                let hdr = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bin_for_srv.len()
                );
                let _ = s.write_all(hdr.as_bytes());
                let _ = s.write_all(&bin_for_srv);
                let _ = s.flush();
                let _ = s.shutdown(Shutdown::Write);
            }
        });

        let endpoint = format!("http://{}", addr);
        let sid = 76_561_198_028_121_353u64;
        let nonce = [0x09, 0x0A];

        let t1 = EticketClient::fetch_remote_eticket(&endpoint, 888_777_661, &nonce, sid).unwrap();
        assert_eq!(t1, vec![0xCA, 0xFE, 0xBA, 0xBE]);
        assert_eq!(
            EticketClient::get_cached_fresh_eticket(888_777_661),
            Some(vec![0xCA, 0xFE, 0xBA, 0xBE])
        );
        EticketClient::clear_cached_fresh_eticket(888_777_661);

        let t2 = EticketClient::fetch_remote_eticket(&endpoint, 888_777_662, &nonce, sid).unwrap();
        assert_eq!(t2, raw_ticket_bin);
        EticketClient::clear_cached_fresh_eticket(888_777_662);

        let t3 = EticketClient::fetch_remote_eticket(&endpoint, 888_777_663, &nonce, sid).unwrap();
        assert_eq!(t3, raw_ticket_bin);
        EticketClient::clear_cached_fresh_eticket(888_777_663);

        handle.join().unwrap();
    }
}
