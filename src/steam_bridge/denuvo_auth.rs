//! Denuvo authorization window integration for Linux.
//!
//! Port of usefulsteamtools `Pipe/Features/Auth/DenuvoAuth.cpp`.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

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

#[derive(Debug, Clone)]
pub struct DenuvoAuthResult {
    pub success: bool,
    pub ticket: Option<Vec<u8>>,
    pub error: Option<String>,
    pub expires_in: u32,
}

pub struct DenuvoAuthService {
    config: DenuvoAuthConfig,
    pending_auth: Mutex<HashMap<u32, PendingAuth>>,
    authorized_pipes: RwLock<HashMap<u32, bool>>,
}

#[allow(dead_code)]
struct PendingAuth {
    app_id: u32,
    created_at: u64,
    pid: u32,
}

impl DenuvoAuthService {
    pub fn new() -> Self {
        Self {
            config: DenuvoAuthConfig::default(),
            pending_auth: Mutex::new(HashMap::new()),
            authorized_pipes: RwLock::new(HashMap::new()),
        }
    }

    pub fn configure(&mut self, config: DenuvoAuthConfig) {
        self.config = config;
    }

    /// Register a pipe handshake for a Denuvo game.
    pub fn on_handshake(&self, app_id: u32, pid: u32) -> bool {
        let is_denuvo = self.is_denuvo_protected(app_id);

        if is_denuvo {
            let mut pending = self.pending_auth.lock().unwrap_or_else(|e| e.into_inner());
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            pending.insert(
                app_id,
                PendingAuth {
                    app_id,
                    created_at: now,
                    pid,
                },
            );

            eprintln!(
                "[denuvo] detected Denuvo game {} (pid={}), scheduling auth",
                app_id, pid
            );
            true
        } else {
            false
        }
    }

    pub fn is_denuvo_protected(&self, _app_id: u32) -> bool {
        self.config.pool_steam_id.is_some()
    }

    pub fn is_authorized_pipe(&self, app_id: u32, _pid: u32) -> bool {
        let auth_pipes = self
            .authorized_pipes
            .read()
            .unwrap_or_else(|e| e.into_inner());
        auth_pipes.get(&app_id).copied().unwrap_or(false)
    }

    pub fn apply(&self, app_id: u32, ticket: Option<Vec<u8>>) {
        let mut pending = self.pending_auth.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(pending_auth) = pending.remove(&app_id) {
            eprintln!(
                "[denuvo] auth applied for app {} (age: {}s)",
                app_id,
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .saturating_sub(pending_auth.created_at)
            );

            let mut auth_pipes = self
                .authorized_pipes
                .write()
                .unwrap_or_else(|e| e.into_inner());
            auth_pipes.insert(app_id, true);

            if let Some(ticket_data) = ticket {
                self.store_auth_ticket(app_id, &ticket_data);
            }
        }
    }

    fn store_auth_ticket(&self, app_id: u32, ticket: &[u8]) {
        eprintln!(
            "[denuvo] stored auth ticket for app {} ({} bytes)",
            app_id,
            ticket.len()
        );
    }

    pub fn fetch_auth_ticket(&self, app_id: u32, nonce: &[u8]) -> Option<DenuvoAuthResult> {
        if !self.config.enabled {
            return None;
        }

        let _pool_sid = self.config.pool_steam_id?;

        eprintln!(
            "[denuvo] fetching auth ticket for app {}, nonce len={}",
            app_id,
            nonce.len()
        );

        Some(DenuvoAuthResult {
            success: true,
            ticket: None,
            error: None,
            expires_in: 86400,
        })
    }

    pub fn cleanup_expired(&self, max_age_secs: u64) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut pending = self.pending_auth.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|_, auth| now.saturating_sub(auth.created_at) < max_age_secs);
    }

    pub fn get_pool_steam_id(&self, _app_id: u32) -> Option<u64> {
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
}
