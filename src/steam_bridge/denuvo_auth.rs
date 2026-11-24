//! Denuvo authorization window integration for Linux.
//!
//! Port of usefulsteamtools `Pipe/Features/Auth/DenuvoAuth.cpp`.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_TICKET_TTL_SECS: u32 = 86_400;

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Debug, Clone)]
pub struct DenuvoAuthConfig {
    pub enabled: bool,
    pub auth_url: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub pool_steam_id: Option<u64>,
}

impl Default for DenuvoAuthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auth_url: Some("https://auth.luatools.xyz".to_string()),
            client_id: None,
            client_secret: None,
            pool_steam_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenuvoAuthResult {
    pub success: bool,
    pub ticket: Option<Vec<u8>>,
    pub error: Option<String>,
    pub expires_in: u32,
}

#[derive(Debug, Clone)]
struct StoredTicket {
    bytes: Vec<u8>,
    stored_at: u64,
    ttl_secs: u32,
}

pub struct DenuvoAuthService {
    config: DenuvoAuthConfig,
    pending_auth: Mutex<HashMap<u32, PendingAuth>>,
    /// Authorized PIDs per app_id (0 represents a wildcard/unspecified PID).
    authorized_pipes: RwLock<HashMap<u32, HashSet<u32>>>,
    stored_tickets: RwLock<HashMap<u32, StoredTicket>>,
    protected_apps: RwLock<HashSet<u32>>,
}

struct PendingAuth {
    created_at: u64,
    pid: u32,
}

impl DenuvoAuthService {
    pub fn new() -> Self {
        Self {
            config: DenuvoAuthConfig::default(),
            pending_auth: Mutex::new(HashMap::new()),
            authorized_pipes: RwLock::new(HashMap::new()),
            stored_tickets: RwLock::new(HashMap::new()),
            protected_apps: RwLock::new(HashSet::new()),
        }
    }

    pub fn configure(&mut self, config: DenuvoAuthConfig) {
        self.config = config;
    }

    /// Explicitly register a Denuvo-protected app ID.
    pub fn register_denuvo_app(&self, app_id: u32) {
        if app_id != 0 {
            self.protected_apps
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(app_id);
        }
    }

    /// Unregister an explicitly registered Denuvo app ID.
    pub fn unregister_denuvo_app(&self, app_id: u32) {
        self.protected_apps
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&app_id);
    }

    /// Register a pipe handshake for a Denuvo game.
    pub fn on_handshake(&self, app_id: u32, pid: u32) -> bool {
        if !self.config.enabled || app_id == 0 {
            return false;
        }

        if self.is_denuvo_protected(app_id) {
            let mut pending = self.pending_auth.lock().unwrap_or_else(|e| e.into_inner());
            let now = now_unix_secs();

            pending.insert(
                app_id,
                PendingAuth {
                    created_at: now,
                    pid,
                },
            );

            log::debug!(
                "[denuvo] detected Denuvo game {} (pid={}), scheduling auth",
                app_id,
                pid
            );
            true
        } else {
            false
        }
    }

    pub fn is_denuvo_protected(&self, app_id: u32) -> bool {
        if app_id == 0 || self.config.pool_steam_id.is_none() {
            return false;
        }
        let protected = self
            .protected_apps
            .read()
            .unwrap_or_else(|e| e.into_inner());
        protected.is_empty() || protected.contains(&app_id)
    }

    pub fn is_authorized_pipe(&self, app_id: u32, pid: u32) -> bool {
        if app_id == 0 {
            return false;
        }
        let auth_pipes = self
            .authorized_pipes
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let Some(pids) = auth_pipes.get(&app_id) else {
            return false;
        };
        if pid == 0 {
            !pids.is_empty()
        } else {
            pids.contains(&pid) || pids.contains(&0)
        }
    }

    pub fn apply(&self, app_id: u32, ticket: Option<Vec<u8>>) {
        if app_id == 0 {
            return;
        }

        // Release `pending_auth` lock before acquiring RwLocks to avoid nested lock ordering.
        let pending_auth = {
            let mut pending = self.pending_auth.lock().unwrap_or_else(|e| e.into_inner());
            pending.remove(&app_id)
        };

        let Some(authorized_pid) = pending_auth.map(|p| {
            log::debug!(
                "[denuvo] auth applied for app {} (pid={}, age: {}s)",
                app_id,
                p.pid,
                now_unix_secs().saturating_sub(p.created_at)
            );
            p.pid
        }) else {
            // Allow storing an explicit non-empty ticket even without a prior handshake,
            // authorizing wildcard PID 0.
            if let Some(ticket_data) = ticket.filter(|t| !t.is_empty()) {
                self.store_auth_ticket(app_id, &ticket_data);
                self.authorized_pipes
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .entry(app_id)
                    .or_default()
                    .insert(0);
            }
            return;
        };

        self.authorized_pipes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(app_id)
            .or_default()
            .insert(authorized_pid);

        if let Some(ticket_data) = ticket.filter(|t| !t.is_empty()) {
            self.store_auth_ticket(app_id, &ticket_data);
        }
    }

    /// Revoke authorization and discard any cached Denuvo ticket for an app.
    pub fn revoke(&self, app_id: u32) {
        self.pending_auth
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&app_id);
        self.authorized_pipes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&app_id);
        self.stored_tickets
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&app_id);
    }

    fn store_auth_ticket(&self, app_id: u32, ticket: &[u8]) {
        if ticket.is_empty() {
            return;
        }
        let mut store = self
            .stored_tickets
            .write()
            .unwrap_or_else(|e| e.into_inner());
        store.insert(
            app_id,
            StoredTicket {
                bytes: ticket.to_vec(),
                stored_at: now_unix_secs(),
                ttl_secs: DEFAULT_TICKET_TTL_SECS,
            },
        );
        log::debug!(
            "[denuvo] stored auth ticket for app {} ({} bytes)",
            app_id,
            ticket.len()
        );
    }

    pub fn fetch_auth_ticket(&self, app_id: u32, nonce: &[u8]) -> Option<DenuvoAuthResult> {
        if !self.config.enabled || app_id == 0 {
            return None;
        }

        let _pool_sid = self.config.pool_steam_id?;
        let now = now_unix_secs();

        log::debug!(
            "[denuvo] fetching auth ticket for app {}, nonce len={}",
            app_id,
            nonce.len()
        );

        let stored = {
            let store = self
                .stored_tickets
                .read()
                .unwrap_or_else(|e| e.into_inner());
            store.get(&app_id).cloned()
        };

        if let Some(entry) = stored {
            let elapsed = now.saturating_sub(entry.stored_at);
            if elapsed < u64::from(entry.ttl_secs) {
                let remaining =
                    (u64::from(entry.ttl_secs) - elapsed).min(u64::from(u32::MAX)) as u32;
                let mut payload = entry.bytes;
                if !nonce.is_empty() {
                    payload.extend_from_slice(nonce);
                }
                return Some(DenuvoAuthResult {
                    success: true,
                    ticket: Some(payload),
                    error: None,
                    expires_in: remaining,
                });
            }
        }

        Some(DenuvoAuthResult {
            success: false,
            ticket: None,
            error: Some(format!(
                "No active Denuvo auth ticket cached for app {}",
                app_id
            )),
            expires_in: 0,
        })
    }

    pub fn cleanup_expired(&self, max_age_secs: u64) {
        let now = now_unix_secs();

        {
            let mut pending = self.pending_auth.lock().unwrap_or_else(|e| e.into_inner());
            pending.retain(|_, auth| now.saturating_sub(auth.created_at) < max_age_secs);
        }

        {
            let mut store = self
                .stored_tickets
                .write()
                .unwrap_or_else(|e| e.into_inner());
            store.retain(|_, t| {
                let age = now.saturating_sub(t.stored_at);
                age < max_age_secs && age < u64::from(t.ttl_secs)
            });
        }
    }

    pub fn get_pool_steam_id(&self, app_id: u32) -> Option<u64> {
        if app_id == 0 {
            return None;
        }
        self.config.pool_steam_id
    }

    pub fn should_force_denuvo(&self, app_id: u32) -> bool {
        self.config.enabled && self.is_denuvo_protected(app_id)
    }
}

impl Default for DenuvoAuthService {
    fn default() -> Self {
        Self::new()
    }
}

pub struct DenuvoAuth {
    service: DenuvoAuthService,
}

impl Default for DenuvoAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl DenuvoAuth {
    pub fn new() -> Self {
        Self {
            service: DenuvoAuthService::new(),
        }
    }

    pub fn configure(&mut self, config: DenuvoAuthConfig) {
        self.service.configure(config);
    }

    pub fn on_handshake(&self, app_id: u32, pid: u32) -> bool {
        self.service.on_handshake(app_id, pid)
    }

    pub fn is_authorized(&self, app_id: u32, pid: u32) -> bool {
        self.service.is_authorized_pipe(app_id, pid)
    }

    pub fn apply(&self, app_id: u32, ticket: Option<Vec<u8>>) {
        self.service.apply(app_id, ticket);
    }

    pub fn set_pool_steam_id(&mut self, steam_id: u64) {
        self.service.config.pool_steam_id = Some(steam_id);
    }

    pub fn set_auth_url(&mut self, url: String) {
        self.service.config.auth_url = Some(url);
    }

    pub fn is_denuvo_game(&self, app_id: u32) -> bool {
        self.service.is_denuvo_protected(app_id)
    }

    pub fn fetch_auth_ticket(&self, app_id: u32, nonce: &[u8]) -> Option<DenuvoAuthResult> {
        self.service.fetch_auth_ticket(app_id, nonce)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_apply_pid_isolation_and_ticket_fetch() {
        let mut auth = DenuvoAuthService::new();
        assert!(!auth.is_denuvo_protected(1245620));
        assert!(!auth.on_handshake(1245620, 4242));

        auth.configure(DenuvoAuthConfig {
            enabled: true,
            pool_steam_id: Some(76_561_198_028_121_353),
            ..Default::default()
        });
        auth.register_denuvo_app(1245620);

        assert!(auth.is_denuvo_protected(1245620));
        assert!(!auth.is_denuvo_protected(730));
        assert!(!auth.is_denuvo_protected(0));

        // Uncached fetch returns success=false instead of fabricating success=true with None
        let miss = auth.fetch_auth_ticket(1245620, b"nonce").unwrap();
        assert!(!miss.success);
        assert!(miss.ticket.is_none());

        assert!(auth.on_handshake(1245620, 4242));
        assert!(!auth.is_authorized_pipe(1245620, 4242));

        auth.apply(1245620, Some(vec![0xAA, 0xBB, 0xCC]));
        assert!(auth.is_authorized_pipe(1245620, 4242));
        assert!(!auth.is_authorized_pipe(1245620, 9999));
        assert!(auth.is_authorized_pipe(1245620, 0));

        let hit = auth.fetch_auth_ticket(1245620, &[0x01, 0x02]).unwrap();
        assert!(hit.success);
        assert_eq!(hit.ticket, Some(vec![0xAA, 0xBB, 0xCC, 0x01, 0x02]));
        assert!(hit.expires_in > 0);

        // Expire immediately with max_age_secs = 0
        auth.cleanup_expired(0);
        let expired = auth.fetch_auth_ticket(1245620, &[]).unwrap();
        assert!(!expired.success);

        auth.revoke(1245620);
        assert!(!auth.is_authorized_pipe(1245620, 4242));
    }

    #[test]
    fn disabled_config_blocks_handshake_and_fetch() {
        let mut auth = DenuvoAuth::new();
        auth.configure(DenuvoAuthConfig {
            enabled: false,
            pool_steam_id: Some(76_561_198_028_121_353),
            ..Default::default()
        });
        assert!(!auth.on_handshake(1245620, 100));
        assert!(auth.fetch_auth_ticket(1245620, b"n").is_none());
    }
}
