//! CloudRedirect host implementation for Linux.
//!
//! Port of usefulsteamtools `CloudRedirectHost.cpp`.
//! Provides cloud save and achievement redirection by intercepting Steam Cloud operations.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    running_apps: BTreeSet<u32>,
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
        if steam_root.as_os_str().is_empty() {
            return false;
        }
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.enabled = enabled;
        state.steam_root = Some(steam_root.to_path_buf());
        true
    }

    /// Register apps for cloud redirection (deduplicated, ignoring AppID 0).
    pub fn register_apps(&self, app_ids: Vec<u32>) {
        let mut unique: Vec<u32> = app_ids.into_iter().filter(|&id| id != 0).collect();
        unique.sort_unstable();
        unique.dedup();
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.registered_apps = unique;
    }

    /// Add a single app to the redirected app set.
    pub fn add_app(&self, app_id: u32) {
        if app_id == 0 {
            return;
        }
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        if let Err(pos) = state.registered_apps.binary_search(&app_id) {
            state.registered_apps.insert(pos, app_id);
        }
    }

    /// Remove a single app from the redirected app set.
    pub fn remove_app(&self, app_id: u32) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.registered_apps.retain(|&id| id != app_id);
        state.running_apps.remove(&app_id);
    }

    /// Check if an app is actively registered for cloud redirect while the service is enabled.
    pub fn is_app_redirected(&self, app_id: u32) -> bool {
        if app_id == 0 {
            return false;
        }
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        state.enabled && state.registered_apps.contains(&app_id)
    }

    /// Set account ID for cloud operations.
    pub fn set_account_id(&self, account_id: u32) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.account_id = (account_id != 0).then_some(account_id);
    }

    /// Handle a cloud RPC call.
    ///
    /// Returns `None` when disabled or when `app_id` is not registered for redirection so
    /// Steam processes unregistered apps natively without interception.
    pub fn handle_cloud_rpc(
        &self,
        method: &str,
        app_id: u32,
        account_id: u32,
        req_body: &[u8],
    ) -> Option<(Vec<u8>, i32)> {
        if method.trim().is_empty() || app_id == 0 {
            return None;
        }
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        if !state.enabled || !state.registered_apps.contains(&app_id) {
            return None;
        }

        let effective_account = if account_id != 0 {
            account_id
        } else {
            state.account_id.unwrap_or(0)
        };
        log::debug!(
            "[cloud_redirect] RPC method={} app={} account={} req_bytes={}",
            method,
            app_id,
            effective_account,
            req_body.len()
        );

        let mut resp = Vec::new();
        if method == "EMsgClientFSGetFileParams" || method == "147" || method == "EMsgClientFSGetFileParamsResponse" {
            resp.extend_from_slice(&1u32.to_le_bytes());
            resp.extend_from_slice(&0u32.to_le_bytes());
        }
        Some((resp, 1))
    }

    /// Notify that an app is running or stopped.
    pub fn notify_app_running(&self, app_id: u32, running: bool) {
        if app_id == 0 {
            return;
        }
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        if !state.enabled || !state.registered_apps.contains(&app_id) {
            return;
        }
        if running {
            state.running_apps.insert(app_id);
        } else {
            state.running_apps.remove(&app_id);
        }
        log::debug!(
            "[cloud_redirect] App {} is {}",
            app_id,
            if running { "running" } else { "stopped" }
        );
    }

    /// Check whether a redirected app is currently marked running.
    pub fn is_app_running(&self, app_id: u32) -> bool {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        state.enabled && state.running_apps.contains(&app_id)
    }

    /// Notify that stats have been stored.
    pub fn notify_stats_stored(&self, app_id: u32) {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        if state.enabled && state.registered_apps.contains(&app_id) {
            log::debug!("[cloud_redirect] Stats stored for app {}", app_id);
        }
    }

    /// Replace the cached achievement list for an app.
    pub fn set_achievements(&self, app_id: u32, blocks: Vec<AchievementBlock>) {
        if app_id == 0 {
            return;
        }
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.achievements.insert(app_id, blocks);
    }

    /// Upsert a single achievement state for an app.
    pub fn upsert_achievement(&self, app_id: u32, block: AchievementBlock) {
        if app_id == 0 || block.name.is_empty() {
            return;
        }
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        let entry = state.achievements.entry(app_id).or_default();
        if let Some(existing) = entry.iter_mut().find(|a| a.name == block.name) {
            *existing = block;
        } else {
            entry.push(block);
        }
    }

    /// Clear cached achievements for an app.
    pub fn clear_achievements(&self, app_id: u32) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.achievements.remove(&app_id);
    }

    /// Get achievements for an app.
    pub fn get_achievements(&self, app_id: u32, max_blocks: usize) -> Vec<AchievementBlock> {
        if app_id == 0 || max_blocks == 0 {
            return Vec::new();
        }
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
        log::debug!(
            "[cloud_redirect] stats sync {}",
            if enable { "enabled" } else { "disabled" }
        );
    }

    /// Shutdown the service.
    pub fn shutdown(&self) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.enabled = false;
        state.running_apps.clear();
        log::debug!("[cloud_redirect] shutdown");
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

    pub fn set_achievements(&self, app_id: u32, blocks: Vec<AchievementBlock>) {
        self.service.set_achievements(app_id, blocks);
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

// Global instance for inter-module access
static CLOUD_REDIRECT_HOST: std::sync::OnceLock<CloudRedirectHost> = std::sync::OnceLock::new();

/// Get or create the global CloudRedirectHost instance
pub fn get_cloud_redirect_host() -> &'static CloudRedirectHost {
    CLOUD_REDIRECT_HOST.get_or_init(CloudRedirectHost::new)
}

pub fn set_account_id(account_id: u32) {
    get_cloud_redirect_host().set_account_id(account_id);
}

pub fn notify_app_running(app_id: u32, running: bool) {
    get_cloud_redirect_host().notify_app_running(app_id, running);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn handle_cloud_rpc_only_intercepts_enabled_registered_apps() {
        let svc = CloudRedirectService::new();
        assert!(!svc.initialize(Path::new(""), true));
        assert!(svc.initialize(Path::new("/tmp/steam"), true));

        svc.register_apps(vec![1245620, 0, 1245620, 480]);
        assert_eq!(svc.state().registered_apps, vec![480, 1245620]);
        assert!(svc.is_app_redirected(1245620));
        assert!(!svc.is_app_redirected(999999));

        assert_eq!(
            svc.handle_cloud_rpc("Cloud.GetUploadServerInfo", 1245620, 42, b"req"),
            Some((Vec::new(), 1))
        );
        // Unregistered app must pass through to Steam (None)
        assert!(svc
            .handle_cloud_rpc("Cloud.GetUploadServerInfo", 999999, 42, b"req")
            .is_none());
        // Empty method must not be intercepted
        assert!(svc.handle_cloud_rpc("", 1245620, 42, b"req").is_none());

        svc.shutdown();
        assert!(!svc.is_app_redirected(1245620));
        assert!(svc
            .handle_cloud_rpc("Cloud.GetUploadServerInfo", 1245620, 42, b"req")
            .is_none());
    }

    #[test]
    fn achievements_upsert_limit_and_running_state() {
        let host = CloudRedirectHost::new();
        host.initialize(Path::new("/tmp/steam"), true);
        host.sync_app_set(vec![730]);
        host.set_account_id(12345);
        host.enable_stats_sync(true);

        host.set_achievements(
            730,
            vec![
                AchievementBlock {
                    name: "FIRST_BLOOD".into(),
                    state: 1,
                    unlock_time: 1_700_000_000,
                },
                AchievementBlock {
                    name: "SHARPSHOOTER".into(),
                    state: 0,
                    unlock_time: 0,
                },
            ],
        );
        assert_eq!(host.get_achievements(730, 1).len(), 1);
        assert_eq!(host.get_achievements(730, 10).len(), 2);
        assert!(host.get_achievements(730, 0).is_empty());

        host.notify_app_running(730, true);
        assert!(host.service.is_app_running(730));
        host.notify_app_running(730, false);
        assert!(!host.service.is_app_running(730));

        host.service.upsert_achievement(
            730,
            AchievementBlock {
                name: "SHARPSHOOTER".into(),
                state: 1,
                unlock_time: 1_700_000_123,
            },
        );
        let all = host.get_achievements(730, 10);
        assert_eq!(all[1].state, 1);
        assert_eq!(all[1].unlock_time, 1_700_000_123);

        host.shutdown();
        assert!(host.get_achievements(730, 10).is_empty());
    }
}
