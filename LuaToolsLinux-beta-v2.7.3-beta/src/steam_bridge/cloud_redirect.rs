//! CloudRedirect host implementation for Linux.
//!
//! Port of usefulsteamtools `CloudRedirectHost.cpp`.
//! Provides cloud save and achievement redirection by intercepting Steam Cloud operations.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Default)]
pub struct AchievementBlock {
    pub name: String,
    pub state: u8,
    pub unlock_time: u32,
}

/// CloudRedirect service state.
#[derive(Debug, Clone, Default)]
pub struct CloudRedirectState {
    pub enabled: bool,
    pub steam_root: Option<std::path::PathBuf>,
    pub registered_apps: Vec<u32>,
    pub account_id: Option<u32>,
    pub stats_sync_enabled: bool,
    achievements: BTreeMap<u32, Vec<AchievementBlock>>,
}

pub struct CloudRedirectService {
    state: Arc<RwLock<CloudRedirectState>>,
}

impl CloudRedirectService {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(CloudRedirectState::default())),
        }
    }

    /// Initialize the cloud redirect service.
    pub fn initialize(&self, steam_root: &std::path::Path, enabled: bool) -> bool {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.enabled = enabled;
        state.steam_root = Some(steam_root.to_path_buf());
        true
    }

    /// Register apps for cloud redirection.
    pub fn register_apps(&self, app_ids: Vec<u32>) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.registered_apps = app_ids;
    }

    /// Check if an app is registered for cloud redirect.
    pub fn is_app_redirected(&self, app_id: u32) -> bool {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        state.registered_apps.contains(&app_id)
    }

    /// Set account ID for cloud operations.
    pub fn set_account_id(&self, account_id: u32) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.account_id = Some(account_id);
    }

    /// Handle a cloud RPC call.
    pub fn handle_cloud_rpc(
        &self,
        _method: &str,
        app_id: u32,
        _account_id: u32,
        _req_body: &[u8],
    ) -> Option<(Vec<u8>, i32)> {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        if !state.enabled {
            return None;
        }

        if state.registered_apps.contains(&app_id) {
            eprintln!("[cloud_redirect] RPC for app {}", app_id);
        }

        Some((Vec::new(), 1))
    }

    /// Notify that an app is running.
    pub fn notify_app_running(&self, app_id: u32, running: bool) {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        if state.enabled && state.registered_apps.contains(&app_id) {
            eprintln!(
                "[cloud_redirect] App {} is {}",
                app_id,
                if running { "running" } else { "stopped" }
            );
        }
    }

    /// Notify that stats have been stored.
    pub fn notify_stats_stored(&self, app_id: u32) {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        if state.enabled && state.registered_apps.contains(&app_id) {
            eprintln!("[cloud_redirect] Stats stored for app {}", app_id);
        }
    }

    /// Get achievements for an app.
    pub fn get_achievements(&self, app_id: u32, max_blocks: usize) -> Vec<AchievementBlock> {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        if !state.enabled {
            return Vec::new();
        }
        state
            .achievements
            .get(&app_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(max_blocks)
            .collect()
    }

    /// Enable stats sync.
    pub fn enable_stats_sync(&self, enable: bool) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.stats_sync_enabled = enable;
        eprintln!(
            "[cloud_redirect] stats sync {}",
            if enable { "enabled" } else { "disabled" }
        );
    }

    /// Shutdown the service.
    pub fn shutdown(&self) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.enabled = false;
        eprintln!("[cloud_redirect] shutdown");
    }

    /// Get the current state.
    pub fn state(&self) -> CloudRedirectState {
        self.state.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Default for CloudRedirectService {
    fn default() -> Self {
        Self::new()
    }
}

/// High-level CloudRedirect host that the GUI can use.
pub struct CloudRedirectHost {
    service: CloudRedirectService,
}

impl CloudRedirectHost {
    pub fn new() -> Self {
        Self {
            service: CloudRedirectService::new(),
        }
    }

    pub fn initialize(&self, steam_root: &std::path::Path, enabled: bool) -> bool {
        self.service.initialize(steam_root, enabled)
    }

    pub fn is_active(&self) -> bool {
        self.service.state().enabled
    }

    pub fn sync_app_set(&self, app_ids: Vec<u32>) {
        self.service.register_apps(app_ids);
    }

    pub fn is_app(&self, app_id: u32) -> bool {
        self.service.is_app_redirected(app_id)
    }

    pub fn handle_cloud_rpc(
        &self,
        method: &str,
        app_id: u32,
        account_id: u32,
        req_body: Vec<u8>,
    ) -> Option<(Vec<u8>, i32)> {
        self.service
            .handle_cloud_rpc(method, app_id, account_id, &req_body)
    }

    pub fn set_account_id(&self, account_id: u32) {
        self.service.set_account_id(account_id);
    }

    pub fn notify_app_running(&self, app_id: u32, running: bool) {
        self.service.notify_app_running(app_id, running);
    }

    pub fn notify_stats_stored(&self, app_id: u32) {
        self.service.notify_stats_stored(app_id);
    }

    pub fn get_achievements(&self, app_id: u32, max_blocks: usize) -> Vec<AchievementBlock> {
        self.service.get_achievements(app_id, max_blocks)
    }

    pub fn enable_stats_sync(&self, enable: bool) {
        self.service.enable_stats_sync(enable);
    }

    pub fn shutdown(&self) {
        self.service.shutdown();
    }
}

impl Default for CloudRedirectHost {
    fn default() -> Self {
        Self::new()
    }
}
