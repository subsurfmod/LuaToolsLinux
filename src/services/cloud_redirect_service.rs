//! CloudRedirectService — Steam Cloud & local achievement sync integration (`CloudRedirect`, LT-71).
//!
//! Persists structured local redirect state in `cloud_state.json` (eliminating unread placebo
//! `cloudredirect.conf`), resolves `UserGameStatsSchema_<app>.bin` when available for canonical
//! achievement API name extraction, and notifies `bridge_lib` via Unix socket IPC.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::steam_bridge::cloud_redirect::{AchievementBlock, CloudRedirectHost};
use crate::steam_bridge::ipc::{IpcClient, IpcMessage};
use crate::steam_bridge::ost_platform::keyvalues1::{
    extract_achievements_with_schema, find_user_game_stats_file,
    find_user_game_stats_schema_file as find_kv1_schema_file, parse_kv1_file,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CloudRedirectState {
    pub enabled: bool,
    #[serde(default)]
    pub steam_path: Option<PathBuf>,
    #[serde(default)]
    pub account_id: Option<u32>,
    #[serde(default)]
    pub apps: Vec<u32>,
    #[serde(default)]
    pub app_save_dirs: BTreeMap<u32, PathBuf>,
}

/// Status of CloudRedirect integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudRedirectStatus {
    NotInstalled,
    Installed,
    Active,
    Error(String),
}

/// Represents a cloud-synced app.
#[derive(Debug, Clone)]
pub struct CloudApp {
    pub app_id: u32,
    pub cloud_enabled: bool,
    pub last_sync: Option<u64>,
}

/// Service for CloudRedirect integration.
#[derive(Clone)]
pub struct CloudRedirectService {
    status: Arc<Mutex<CloudRedirectStatus>>,
    cloud_apps: Arc<Mutex<HashSet<u32>>>,
    redirect_path: PathBuf,
    host: Arc<CloudRedirectHost>,
    achievement_counts: Arc<Mutex<BTreeMap<u32, usize>>>,
}

impl fmt::Debug for CloudRedirectService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CloudRedirectService")
            .field("status", &self.status)
            .field("cloud_apps", &self.cloud_apps)
            .field("redirect_path", &self.redirect_path)
            .field("host_active", &self.host.is_active())
            .field("achievement_counts", &self.achievement_counts)
            .finish()
    }
}

impl Default for CloudRedirectService {
    fn default() -> Self {
        Self::new()
    }
}

impl CloudRedirectService {
    pub fn new() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let candidates = [
            home.join(".local/share/Steam/config/cloudredirect"),
            home.join(".local/share/CloudRedirect"),
            home.join(".var/app/com.valvesoftware.Steam/.local/share/CloudRedirect"),
        ];
        let chosen = candidates
            .iter()
            .find(|p| p.exists())
            .cloned()
            .unwrap_or_else(|| candidates[0].clone());
        Self::with_path(chosen)
    }

    pub fn with_path(redirect_path: PathBuf) -> Self {
        let mut apps = HashSet::new();
        let mut parsed_steam_path: Option<PathBuf> = None;
        let mut parsed_account_id: Option<u32> = None;

        let state_path = Self::state_path_for(&redirect_path);
        let legacy_conf_path = Self::legacy_conf_path_for(&redirect_path);
        let mut has_persisted_state = false;

        if let Ok(raw) = fs::read_to_string(&state_path) {
            if let Ok(st) = serde_json::from_str::<CloudRedirectState>(&raw) {
                has_persisted_state = true;
                for aid in st.apps {
                    if aid != 0 {
                        apps.insert(aid);
                    }
                }
                parsed_steam_path = st.steam_path;
                parsed_account_id = st.account_id.filter(|&a| a != 0);
            }
        } else if let Ok(raw) = fs::read_to_string(&legacy_conf_path) {
            // One-time read of legacy cloudredirect.conf for seamless migration
            has_persisted_state = true;
            for line in raw.lines() {
                let trimmed = line.trim();
                if let Some(rhs) = trimmed.strip_prefix("appid") {
                    let val = rhs.trim().trim_start_matches('=').trim();
                    if let Ok(aid) = val.parse::<u32>() {
                        apps.insert(aid);
                    }
                } else if let Some(rhs) = trimmed.strip_prefix("steam_path") {
                    let val = rhs.trim().trim_start_matches('=').trim();
                    if !val.is_empty() {
                        parsed_steam_path = Some(PathBuf::from(val));
                    }
                } else if let Some(rhs) = trimmed.strip_prefix("account_id") {
                    let val = rhs.trim().trim_start_matches('=').trim();
                    if let Ok(acc) = val.parse::<u32>() {
                        if acc != 0 {
                            parsed_account_id = Some(acc);
                        }
                    }
                }
            }
        }

        let status = if has_persisted_state && !apps.is_empty() {
            CloudRedirectStatus::Active
        } else if redirect_path.exists() {
            CloudRedirectStatus::Installed
        } else {
            CloudRedirectStatus::NotInstalled
        };

        let host = Arc::new(CloudRedirectHost::new());
        let host_enabled = status != CloudRedirectStatus::NotInstalled;
        if host_enabled {
            let root = parsed_steam_path
                .clone()
                .or_else(|| Self::infer_steam_root(&redirect_path))
                .unwrap_or_else(|| redirect_path.clone());
            host.initialize(&root, !apps.is_empty());
            host.sync_app_set(apps.iter().copied().collect());
            if let Some(acc) = parsed_account_id {
                host.set_account_id(acc);
            }
            host.enable_stats_sync(true);
        }

        let svc = Self {
            status: Arc::new(Mutex::new(status)),
            cloud_apps: Arc::new(Mutex::new(apps)),
            redirect_path,
            host,
            achievement_counts: Arc::new(Mutex::new(BTreeMap::new())),
        };

        if let Some(acc) = parsed_account_id {
            if let Some(config_dir) = parsed_steam_path
                .as_deref()
                .map(|r| r.join("config"))
                .or_else(|| svc.redirect_path.parent().map(Path::to_path_buf))
            {
                svc.sync_local_achievements(&config_dir, acc);
            }
        }

        svc
    }

    fn infer_steam_root(redirect_path: &Path) -> Option<PathBuf> {
        let parent = redirect_path.parent()?;
        if parent.file_name().and_then(|n| n.to_str()) == Some("config") {
            return parent.parent().map(Path::to_path_buf);
        }
        Some(parent.to_path_buf())
    }

    fn state_path_for(redirect_path: &Path) -> PathBuf {
        redirect_path.join("cloud_state.json")
    }

    fn legacy_conf_path_for(redirect_path: &Path) -> PathBuf {
        redirect_path
            .parent()
            .unwrap_or(redirect_path)
            .join("cloudredirect.conf")
    }

    /// Locates `UserGameStatsSchema_<app_id>.bin` alongside the stats file or under Steam's
    /// `appcache/stats` directory so KV1 achievement bitmasks resolve to real API names (LT-71 / BST-46).
    pub fn find_user_game_stats_schema_file(
        steam_config_dir: &Path,
        stats_file: &Path,
        app_id: u32,
    ) -> Option<PathBuf> {
        stats_file
            .parent()
            .and_then(|dir| find_kv1_schema_file(dir, app_id))
            .or_else(|| find_kv1_schema_file(steam_config_dir, app_id))
    }

    pub fn is_installed(&self) -> bool {
        self.redirect_path.exists()
    }

    pub fn status(&self) -> CloudRedirectStatus {
        self.status.lock().unwrap().clone()
    }

    pub fn enable_cloud_for_app(&self, app_id: u32) {
        self.cloud_apps.lock().unwrap().insert(app_id);
        self.host.sync_app_set(self.list_cloud_apps());
        let _ = self.notify_bridge(app_id, true);
    }

    pub fn disable_cloud_for_app(&self, app_id: u32) {
        self.cloud_apps.lock().unwrap().remove(&app_id);
        self.host.sync_app_set(self.list_cloud_apps());
        let _ = self.notify_bridge(app_id, false);
    }

    pub fn is_cloud_enabled(&self, app_id: u32) -> bool {
        self.cloud_apps.lock().unwrap().contains(&app_id)
    }

    pub fn list_cloud_apps(&self) -> Vec<u32> {
        let mut apps: Vec<u32> = self.cloud_apps.lock().unwrap().iter().copied().collect();
        apps.sort_unstable();
        apps
    }

    pub fn redirect_path(&self) -> &Path {
        &self.redirect_path
    }

    pub fn host(&self) -> &CloudRedirectHost {
        &self.host
    }

    /// Loads local Valve KV1 `UserGameStats_<account_id>_<app_id>.bin` achievements for `app_id`
    /// (resolved against `UserGameStatsSchema_<app_id>.bin`) into `CloudRedirectHost`.
    pub fn load_app_achievements(
        &self,
        steam_config_dir: &Path,
        account_id: u32,
        app_id: u32,
    ) -> usize {
        if account_id == 0 || app_id == 0 {
            return 0;
        }
        let Some(stats_file) = find_user_game_stats_file(steam_config_dir, account_id, app_id)
        else {
            return 0;
        };
        let Ok(kv1) = parse_kv1_file(&stats_file) else {
            return 0;
        };
        let schema_kv1 =
            Self::find_user_game_stats_schema_file(steam_config_dir, &stats_file, app_id)
                .and_then(|sp| parse_kv1_file(sp).ok());
        let blocks: Vec<AchievementBlock> =
            extract_achievements_with_schema(&kv1, schema_kv1.as_ref())
                .into_values()
                .map(|ach| AchievementBlock {
                    name: ach.name,
                    state: u8::from(ach.unlocked),
                    unlock_time: ach.unlock_time.unwrap_or(0),
                })
                .collect();
        let count = blocks.len();
        self.host.set_achievements(app_id, blocks);
        self.achievement_counts.lock().unwrap().insert(app_id, count);
        count
    }

    /// Syncs local BKV achievement records for all registered apps into `CloudRedirectHost`.
    pub fn sync_local_achievements(&self, steam_config_dir: &Path, account_id: u32) -> usize {
        if account_id == 0 {
            return 0;
        }
        let mut total = 0usize;
        for app_id in self.list_cloud_apps() {
            total += self.load_app_achievements(steam_config_dir, account_id, app_id);
        }
        total
    }

    /// Returns the number of local BKV achievements cached for `app_id`.
    pub fn local_achievement_count(&self, app_id: u32) -> usize {
        if let Some(&count) = self.achievement_counts.lock().unwrap().get(&app_id) {
            return count;
        }
        self.host.get_achievements(app_id, usize::MAX).len()
    }

    /// Returns a map of `app_id -> achievement_count` across synced apps.
    pub fn local_achievement_counts(&self) -> BTreeMap<u32, usize> {
        self.achievement_counts.lock().unwrap().clone()
    }

    /// Persists structured local redirect state (`cloud_state.json`) and removes any stale
    /// legacy `cloudredirect.conf` placebo file (LT-71).
    pub fn sync_to_cloudredirect(&self) -> Result<PathBuf, String> {
        self.generate_cloudredirect_conf(None, None)
    }

    /// Persists local redirect state (`cloud_state.json`) with optional `steam_root` and `account_id`,
    /// creates per-app save directories, syncs `CloudRedirectHost` + schema-backed achievements,
    /// and cleans up any legacy `cloudredirect.conf` file (LT-71).
    pub fn generate_cloudredirect_conf(
        &self,
        steam_root: Option<&Path>,
        account_id: Option<u32>,
    ) -> Result<PathBuf, String> {
        fs::create_dir_all(&self.redirect_path).map_err(|e| e.to_string())?;
        let apps = self.list_cloud_apps();
        let state_path = Self::state_path_for(&self.redirect_path);

        let mut app_save_dirs = BTreeMap::new();
        for &app_id in &apps {
            let app_save_dir = self.redirect_path.join(app_id.to_string());
            let _ = fs::create_dir_all(&app_save_dir);
            app_save_dirs.insert(app_id, app_save_dir);
        }

        let state = CloudRedirectState {
            enabled: !apps.is_empty(),
            steam_path: steam_root.map(Path::to_path_buf),
            account_id: account_id.filter(|&a| a != 0),
            apps: apps.clone(),
            app_save_dirs,
        };
        let json = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
        let tmp = state_path.with_extension("json.tmp");
        fs::write(&tmp, json).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &state_path).map_err(|e| e.to_string())?;

        // Clean up any stale legacy unread placebo cloudredirect.conf
        let legacy_conf = Self::legacy_conf_path_for(&self.redirect_path);
        if legacy_conf.is_file() {
            let _ = fs::remove_file(&legacy_conf);
        }

        let effective_root = steam_root
            .map(Path::to_path_buf)
            .or_else(|| Self::infer_steam_root(&self.redirect_path))
            .unwrap_or_else(|| self.redirect_path.clone());
        self.host.initialize(&effective_root, !apps.is_empty());
        self.host.sync_app_set(apps.clone());
        if let Some(acc) = account_id.filter(|&a| a != 0) {
            self.host.set_account_id(acc);
            let config_dir = steam_root
                .map(|r| r.join("config"))
                .or_else(|| self.redirect_path.parent().map(Path::to_path_buf))
                .unwrap_or_else(|| effective_root.join("config"));
            self.sync_local_achievements(&config_dir, acc);
        }
        self.host.enable_stats_sync(true);

        let mut status = self.status.lock().unwrap();
        *status = if apps.is_empty() {
            CloudRedirectStatus::Installed
        } else {
            CloudRedirectStatus::Active
        };
        Ok(state_path)
    }

    pub fn notify_bridge(&self, app_id: u32, cloud_enabled: bool) -> Result<(), String> {
        let mut payload = HashMap::new();
        payload.insert("app_id".into(), app_id.to_string());
        payload.insert("cloud_enabled".into(), cloud_enabled.to_string());
        let client = IpcClient::new();
        let _ = client.send(&IpcMessage::Event {
            kind: "cloud_redirect".into(),
            payload,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_defaults_and_syncs_in_isolated_tempdir() {
        let tmp = tempfile::tempdir().unwrap();
        let cr_dir = tmp.path().join("config/cloudredirect");
        let svc = CloudRedirectService::with_path(cr_dir.clone());
        assert_eq!(svc.status(), CloudRedirectStatus::NotInstalled);
        assert!(svc.list_cloud_apps().is_empty());

        svc.enable_cloud_for_app(1245620);
        assert!(svc.is_cloud_enabled(1245620));
        let conf = svc.sync_to_cloudredirect().unwrap();
        assert!(conf.is_file());
        assert_eq!(svc.status(), CloudRedirectStatus::Active);

        let reloaded = CloudRedirectService::with_path(cr_dir);
        assert!(reloaded.is_cloud_enabled(1245620));

        reloaded.disable_cloud_for_app(1245620);
        assert!(!reloaded.is_cloud_enabled(1245620));
    }

    #[test]
    fn syncs_local_bkv_achievements_into_cloud_redirect_host() {
        use crate::steam_bridge::ost_platform::keyvalues1::{write_kv1, Kv1Value};

        let tmp = tempfile::tempdir().unwrap();
        let steam_root = tmp.path().join("Steam");
        let cr_dir = steam_root.join("config/cloudredirect");
        let stats_dir = steam_root.join("appcache/stats");
        fs::create_dir_all(&stats_dir).unwrap();

        // Build real Valve KV1 UserGameStats_123456_1245620.bin with 1 unlocked achievement
        let mut ach0 = BTreeMap::new();
        ach0.insert("name".into(), Kv1Value::String("FIRST_KILL".into()));
        ach0.insert("unlocked".into(), Kv1Value::Int32(1));
        ach0.insert("unlockTime".into(), Kv1Value::Int32(42));
        let mut root = BTreeMap::new();
        root.insert("ach0".into(), Kv1Value::Object(ach0));
        let kv1_bytes = write_kv1("UserGameStats", &Kv1Value::Object(root)).unwrap();

        fs::write(stats_dir.join("UserGameStats_123456_1245620.bin"), &kv1_bytes).unwrap();

        let svc = CloudRedirectService::with_path(cr_dir.clone());
        svc.enable_cloud_for_app(1245620);
        svc.generate_cloudredirect_conf(Some(&steam_root), Some(123456))
            .unwrap();

        assert_eq!(svc.local_achievement_count(1245620), 1);
        assert_eq!(svc.local_achievement_counts().get(&1245620), Some(&1));
        assert!(svc.host().is_app(1245620));
        let blocks = svc.host().get_achievements(1245620, 10);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].name, "FIRST_KILL");
        assert_eq!(blocks[0].state, 1);
        assert_eq!(blocks[0].unlock_time, 42);

        // Reloading from cloud_state.json preserves synced app & BKV achievements
        let reloaded = CloudRedirectService::with_path(cr_dir);
        assert_eq!(reloaded.local_achievement_count(1245620), 1);
        assert!(reloaded.host().is_app(1245620));
        assert!(!steam_root.join("config/cloudredirect.conf").exists());
    }

    #[test]
    fn resolves_kv1_bitmask_achievements_using_user_game_stats_schema() {
        use crate::steam_bridge::ost_platform::keyvalues1::{write_kv1, Kv1Value};

        let tmp = tempfile::tempdir().unwrap();
        let steam_root = tmp.path().join("Steam");
        let cr_dir = steam_root.join("config/cloudredirect");
        let stats_dir = steam_root.join("appcache/stats");
        fs::create_dir_all(&stats_dir).unwrap();

        // Build KV1 UserGameStatsSchema_1245620.bin mapping (stat_id=1, bit=0) -> "ACH_ELDEN_LORD"
        let mut bit0 = BTreeMap::new();
        bit0.insert("name".into(), Kv1Value::String("ACH_ELDEN_LORD".into()));
        let mut bits = BTreeMap::new();
        bits.insert("0".into(), Kv1Value::Object(bit0));
        let mut stat1 = BTreeMap::new();
        stat1.insert("bits".into(), Kv1Value::Object(bits));
        let mut stats = BTreeMap::new();
        stats.insert("1".into(), Kv1Value::Object(stat1));
        let mut schema_root = BTreeMap::new();
        schema_root.insert("stats".into(), Kv1Value::Object(stats));
        let schema_bytes = write_kv1("1245620", &Kv1Value::Object(schema_root)).unwrap();
        fs::write(stats_dir.join("UserGameStatsSchema_1245620.bin"), &schema_bytes).unwrap();

        // Build KV1 UserGameStats_123456_1245620.bin with stat 1 bitmask = 1 (unlocked)
        let mut user_root = BTreeMap::new();
        user_root.insert("1".into(), Kv1Value::Int32(1));
        let user_bytes = write_kv1("UserGameStats", &Kv1Value::Object(user_root)).unwrap();
        fs::write(stats_dir.join("UserGameStats_123456_1245620.bin"), &user_bytes).unwrap();

        let svc = CloudRedirectService::with_path(cr_dir);
        svc.enable_cloud_for_app(1245620);
        svc.generate_cloudredirect_conf(Some(&steam_root), Some(123456))
            .unwrap();
        assert_eq!(svc.local_achievement_count(1245620), 1);
        let blocks = svc.host().get_achievements(1245620, 10);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].name, "ACH_ELDEN_LORD");
        assert_eq!(blocks[0].state, 1);
    }
}
