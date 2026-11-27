//! TokenBridge / `bst://` URI Scheme handler for Linux.
//!
//! Handles `bst://` URI scheme for token-based authentication, local achievement sync,
//! and protocol action dispatch.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::models::steam::{discover_libraries, scan_installed_games};
use crate::services::cloud_redirect_service::CloudRedirectService;
use crate::services::steam_service::{detect_steam_install, SteamService};

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex_str) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(val) = u8::from_str_radix(hex_str, 16) {
                    out.push(val);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Returns `true` if `s` names a built-in `bst://` action verb rather than an `AppToken`.
pub fn is_known_bst_action(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "refreshachievements" | "fix" | "install" | "redeem"
    )
}

/// Structured action recorded and executed by `TokenBridge` handlers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BstDispatchedAction {
    RefreshAchievements {
        target_all: bool,
        app_id: u32,
        refreshed_apps: usize,
        synced_achievements: usize,
    },
    Fix {
        target_all: bool,
        app_id: u32,
    },
    Install {
        target_all: bool,
        app_id: u32,
        silent: bool,
    },
    Redeem {
        target_all: bool,
        code: String,
    },
    AppToken {
        app_id: u32,
        token: String,
    },
}

/// BST URI structure parsed from a `bst://` URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BstUri {
    /// The app ID associated with this URI (`0` for global `/all` or `/redeem/<code>` actions).
    pub app_id: u32,
    /// The token, session ID, or action verb (`refreshachievements`, `fix`, `install`, `redeem`).
    pub token: String,
    /// The URI scheme version.
    pub version: u32,
    /// Optional query or path parameters.
    pub params: HashMap<String, String>,
}

impl BstUri {
    /// Parse a `bst://` URI string (case-insensitive scheme, strips `#fragment`, percent-decodes values).
    ///
    /// Supports:
    /// - App-token URIs: `bst://<appid>/<token>?k1=v1&k2=v2` and `bst://<appid>/<token>/k1=v1`
    /// - Per-app action URIs: `bst://<action>/<appid>` (e.g. `bst://refreshachievements/480`)
    /// - Global action URIs: `bst://<action>/all` (e.g. `bst://refreshachievements/all`)
    /// - Silent/redeem action URIs: `bst://install/silent/<appid>`, `bst://redeem/<code>`
    pub fn parse(uri: &str) -> Option<Self> {
        let trimmed = uri.trim();
        let (scheme, rest_full) = trimmed.split_once("://")?;
        if !scheme.eq_ignore_ascii_case("bst") {
            return None;
        }
        // Strip optional URI fragment (`#...`)
        let rest = rest_full
            .split_once('#')
            .map_or(rest_full, |(before, _)| before);
        if rest.is_empty() {
            return None;
        }

        let (path_part, query_part) = match rest.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (rest, None),
        };

        let parts: Vec<&str> = path_part.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return None;
        }

        let first_decoded = percent_decode(parts[0]);
        let mut app_id = parts[0].parse::<u32>().unwrap_or(0);
        let mut token = String::new();
        let mut params = HashMap::new();

        if is_known_bst_action(&first_decoded) {
            let action_lower = first_decoded.to_ascii_lowercase();
            token = action_lower.clone();

            for raw_seg in parts.iter().skip(1) {
                if let Some((k, v)) = raw_seg.split_once('=') {
                    if !k.is_empty() {
                        params.insert(percent_decode(k), percent_decode(v));
                    }
                    continue;
                }
                let seg = percent_decode(raw_seg);
                if seg.eq_ignore_ascii_case("all") {
                    app_id = 0;
                    params.insert("target".to_string(), "all".to_string());
                } else if seg.eq_ignore_ascii_case("silent") {
                    params.insert("silent".to_string(), "1".to_string());
                } else if let Ok(parsed_id) = seg.parse::<u32>() {
                    if app_id == 0 {
                        app_id = parsed_id;
                    }
                } else if action_lower == "redeem" && !seg.is_empty() {
                    params
                        .entry("code".to_string())
                        .or_insert_with(|| seg.clone());
                    params.entry("target".to_string()).or_insert(seg);
                } else if !seg.is_empty() {
                    // Record unrecognized target so `is_global_action()` rejects invalid targets
                    params.entry("invalid_target".to_string()).or_insert(seg);
                }
            }
        } else if app_id == 0 && parts.len() > 1 {
            // Support generic `<custom_action>/<appid>` when second segment is numeric
            if let Ok(parsed_id) = parts[1].parse::<u32>() {
                token = first_decoded;
                app_id = parsed_id;
                for part in parts.iter().skip(2) {
                    if let Some((k, v)) = part.split_once('=') {
                        if !k.is_empty() {
                            params.insert(percent_decode(k), percent_decode(v));
                        }
                    }
                }
            } else {
                token = percent_decode(parts[1]);
                for part in parts.iter().skip(2) {
                    if let Some((k, v)) = part.split_once('=') {
                        if !k.is_empty() {
                            params.insert(percent_decode(k), percent_decode(v));
                        }
                    }
                }
            }
        } else if parts.len() > 1 {
            token = percent_decode(parts[1]);
            for part in parts.iter().skip(2) {
                if let Some((k, v)) = part.split_once('=') {
                    if !k.is_empty() {
                        params.insert(percent_decode(k), percent_decode(v));
                    }
                }
            }
        }

        if let Some(query) = query_part {
            for pair in query.split('&') {
                if let Some((k, v)) = pair.split_once('=') {
                    if !k.is_empty() {
                        params.insert(percent_decode(k), percent_decode(v));
                    }
                }
            }
        }

        if params
            .get("target")
            .is_some_and(|t| t.eq_ignore_ascii_case("all"))
        {
            params.insert("target".to_string(), "all".to_string());
        }

        let version = params
            .get("v")
            .or_else(|| params.get("version"))
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(1);

        Some(BstUri {
            app_id,
            token,
            version,
            params,
        })
    }

    /// Returns `true` when this URI represents a valid global (`app_id == 0`) action
    /// such as `bst://refreshachievements/all`, `bst://<action>/all`, or `bst://redeem/<code>`.
    pub fn is_global_action(&self) -> bool {
        if self.app_id != 0 || !is_known_bst_action(&self.token) {
            return false;
        }
        if self.params.contains_key("invalid_target") {
            return false;
        }
        if self
            .params
            .get("target")
            .is_some_and(|t| t.eq_ignore_ascii_case("all"))
        {
            return true;
        }
        self.token.eq_ignore_ascii_case("redeem")
            && self.params.get("code").is_some_and(|c| !c.trim().is_empty())
    }

    /// Returns `true` when this URI invokes a built-in action (`refreshachievements`, `fix`, `install`, `redeem`).
    pub fn is_action(&self) -> bool {
        is_known_bst_action(&self.token)
            || self
                .params
                .get("action")
                .is_some_and(|a| is_known_bst_action(a))
    }

    /// Returns `true` only when this URI carries a genuine per-app `AppToken` (never an action verb or `"all"`).
    pub fn is_app_token(&self) -> bool {
        self.app_id > 0
            && !self.token.is_empty()
            && !self.is_action()
            && !self.token.eq_ignore_ascii_case("all")
            && !self.token.eq_ignore_ascii_case("silent")
    }

    /// Check if this URI is a valid per-app URI (`app_id > 0` with non-empty token/action
    /// and no malformed target) or a valid global action (`bst://<action>/all`, `bst://redeem/<code>`).
    pub fn is_valid(&self) -> bool {
        if self.params.contains_key("invalid_target") {
            return false;
        }
        (self.app_id > 0 && !self.token.is_empty()) || self.is_global_action()
    }
}

/// Scans a directory for `UserGameStats_<account_id>_<app_id>.bin` files, recording discovered
/// `(account_id, app_id)` pairs so local achievement reloads work across all accounts/apps.
fn discover_stats_files_in_dir(dir: &Path, accounts: &mut BTreeSet<u32>, apps: &mut BTreeSet<u32>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let Some(name) = fname.to_str() else {
            continue;
        };
        let Some(rest) = name
            .strip_prefix("UserGameStats_")
            .and_then(|s| s.strip_suffix(".bin"))
        else {
            continue;
        };
        let Some((acc_s, app_s)) = rest.split_once('_') else {
            continue;
        };
        if let (Ok(acc), Ok(aid)) = (acc_s.parse::<u32>(), app_s.parse::<u32>()) {
            if acc > 0 && aid > 0 {
                accounts.insert(acc);
                apps.insert(aid);
            }
        }
    }
}

/// Discovers local Steam `account_id`s and stats-bearing `app_id`s under `steam_root`.
fn discover_steam_accounts_and_stats_apps(steam_root: &Path) -> (Vec<u32>, BTreeSet<u32>) {
    let mut accounts = BTreeSet::new();
    let mut stats_apps = BTreeSet::new();

    if let Ok(entries) = fs::read_dir(steam_root.join("userdata")) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                if let Some(acc) = entry
                    .file_name()
                    .to_str()
                    .and_then(|s| s.parse::<u32>().ok())
                    .filter(|&id| id > 0)
                {
                    accounts.insert(acc);
                }
            }
        }
    }

    discover_stats_files_in_dir(
        &steam_root.join("appcache/stats"),
        &mut accounts,
        &mut stats_apps,
    );
    discover_stats_files_in_dir(
        &steam_root.join("depotcache"),
        &mut accounts,
        &mut stats_apps,
    );
    discover_stats_files_in_dir(&steam_root.join("config"), &mut accounts, &mut stats_apps);

    (accounts.into_iter().collect(), stats_apps)
}

/// Reloads local Valve KV1 achievements (`UserGameStats_<account>_<app>.bin` + optional
/// `UserGameStatsSchema_<app>.bin`) into `CloudRedirectService` for either a single `target_app_id`
/// (`Some(480)`) or all cloud/installed/stats apps (`None`).
///
/// Returns `(refreshed_apps_count, synced_achievements_count)`.
pub fn refresh_local_achievements(
    steam_root_override: Option<&Path>,
    cloud_service: Option<&CloudRedirectService>,
    target_app_id: Option<u32>,
    extra_app_ids: &[u32],
) -> (usize, usize) {
    let resolved_root: Option<PathBuf> = steam_root_override
        .map(Path::to_path_buf)
        .or_else(|| detect_steam_install(None).map(|inst| inst.root));

    let Some(steam_root) = resolved_root else {
        return (0, 0);
    };

    let owned_cloud;
    let cloud = match cloud_service {
        Some(svc) => svc,
        None => {
            owned_cloud = CloudRedirectService::with_path(steam_root.join("config/cloudredirect"));
            &owned_cloud
        }
    };

    let config_dir = steam_root.join("config");
    let (accounts, stats_apps) = discover_steam_accounts_and_stats_apps(&steam_root);
    if accounts.is_empty() {
        return (0, 0);
    }

    let target_apps: BTreeSet<u32> = if let Some(aid) = target_app_id.filter(|&id| id > 0) {
        let mut set = BTreeSet::new();
        set.insert(aid);
        set
    } else {
        let mut set = stats_apps;
        for aid in cloud.list_cloud_apps() {
            if aid > 0 {
                set.insert(aid);
            }
        }
        for &aid in extra_app_ids {
            if aid > 0 {
                set.insert(aid);
            }
        }
        for lib in discover_libraries(&steam_root) {
            for aid in scan_installed_games(&lib) {
                if aid > 0 {
                    set.insert(aid);
                }
            }
        }
        for aid in SteamService::list_lua_app_ids(&config_dir.join("stplug-in")) {
            if aid > 0 {
                set.insert(aid);
            }
        }
        set
    };

    let mut refreshed_apps = 0usize;
    let mut synced_achievements = 0usize;

    for aid in target_apps {
        let mut app_best = 0usize;
        let mut app_touched = false;
        for &acc in &accounts {
            let loaded = cloud.load_app_achievements(&config_dir, acc, aid);
            if loaded > 0 {
                app_touched = true;
                app_best = app_best.max(loaded);
            }
        }
        if app_touched || target_app_id == Some(aid) {
            refreshed_apps += 1;
            synced_achievements += app_best;
        }
    }

    (refreshed_apps, synced_achievements)
}

/// Type alias for `bst://` URI handlers.
pub type BstUriHandler = Box<dyn Fn(&BstUri) -> Option<BstUri> + Send + Sync>;

/// TokenBridge service that handles `bst://` URI requests with structured dispatch & KV1 sync.
pub struct TokenBridge {
    /// Registered URI handlers.
    handlers: Arc<RwLock<Vec<BstUriHandler>>>,
    /// Optional Steam root override (used by unit tests or custom installations).
    steam_root_override: Arc<RwLock<Option<PathBuf>>>,
    /// Optional bound `CloudRedirectService` receiving refreshed local KV1 achievements.
    cloud_service: Arc<RwLock<Option<CloudRedirectService>>>,
    /// Recorded log of structured actions dispatched through this `TokenBridge`.
    dispatched_actions: Arc<RwLock<Vec<BstDispatchedAction>>>,
}

impl Default for TokenBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenBridge {
    /// Create a new TokenBridge instance with default handlers.
    pub fn new() -> Self {
        let mut bridge = TokenBridge {
            handlers: Arc::new(RwLock::new(Vec::new())),
            steam_root_override: Arc::new(RwLock::new(None)),
            cloud_service: Arc::new(RwLock::new(None)),
            dispatched_actions: Arc::new(RwLock::new(Vec::new())),
        };
        bridge.register_default_handlers();
        bridge
    }

    /// Create a `TokenBridge` bound to a specific `steam_root` and `CloudRedirectService`.
    pub fn with_services(steam_root: PathBuf, cloud_service: CloudRedirectService) -> Self {
        let mut bridge = TokenBridge {
            handlers: Arc::new(RwLock::new(Vec::new())),
            steam_root_override: Arc::new(RwLock::new(Some(steam_root))),
            cloud_service: Arc::new(RwLock::new(Some(cloud_service))),
            dispatched_actions: Arc::new(RwLock::new(Vec::new())),
        };
        bridge.register_default_handlers();
        bridge
    }

    /// Return all structured actions dispatched so far.
    pub fn dispatched_actions(&self) -> Vec<BstDispatchedAction> {
        self.dispatched_actions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Return the most recently dispatched structured action.
    pub fn last_dispatched_action(&self) -> Option<BstDispatchedAction> {
        self.dispatched_actions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .last()
            .cloned()
    }

    /// Clear recorded dispatch history.
    pub fn clear_dispatched_actions(&self) {
        self.dispatched_actions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// Register default handlers for `bst://` actions with real KV1 sync & structured state recording.
    fn register_default_handlers(&mut self) {
        // 1. bst://refreshachievements/<appid|all> — Reload local KV1 achievements into CloudRedirectHost
        {
            let root_ref = Arc::clone(&self.steam_root_override);
            let cloud_ref = Arc::clone(&self.cloud_service);
            let log_ref = Arc::clone(&self.dispatched_actions);
            self.register_handler(Box::new(move |uri: &BstUri| {
                let is_refresh = uri.token.eq_ignore_ascii_case("refreshachievements")
                    || uri
                        .params
                        .get("action")
                        .is_some_and(|s| s.eq_ignore_ascii_case("refreshachievements"));
                if !is_refresh || !uri.is_valid() {
                    return None;
                }

                let target_all = uri.is_global_action();
                let target_app = (!target_all && uri.app_id > 0).then_some(uri.app_id);
                let root_guard = root_ref.read().unwrap_or_else(|e| e.into_inner());
                let cloud_guard = cloud_ref.read().unwrap_or_else(|e| e.into_inner());
                let (refreshed_apps, synced_achievements) = refresh_local_achievements(
                    root_guard.as_deref(),
                    cloud_guard.as_ref(),
                    target_app,
                    &[],
                );

                log_ref
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(BstDispatchedAction::RefreshAchievements {
                        target_all,
                        app_id: uri.app_id,
                        refreshed_apps,
                        synced_achievements,
                    });

                let mut handled = uri.clone();
                handled
                    .params
                    .insert("dispatched".to_string(), "refreshachievements".to_string());
                handled.params.insert(
                    "refreshed_apps".to_string(),
                    refreshed_apps.to_string(),
                );
                handled.params.insert(
                    "synced_achievements".to_string(),
                    synced_achievements.to_string(),
                );
                Some(handled)
            }));
        }

        // 2. bst://fix/<appid|all> — Structured Fix view dispatch
        {
            let log_ref = Arc::clone(&self.dispatched_actions);
            self.register_handler(Box::new(move |uri: &BstUri| {
                let is_fix = uri.token.eq_ignore_ascii_case("fix")
                    || uri
                        .params
                        .get("action")
                        .is_some_and(|s| s.eq_ignore_ascii_case("fix"));
                if !is_fix || !uri.is_valid() {
                    return None;
                }
                let target_all = uri.is_global_action();
                log_ref
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(BstDispatchedAction::Fix {
                        target_all,
                        app_id: uri.app_id,
                    });
                let mut handled = uri.clone();
                handled
                    .params
                    .insert("dispatched".to_string(), "fix".to_string());
                Some(handled)
            }));
        }

        // 3. bst://install/<appid|all> — Structured Install dispatch
        {
            let log_ref = Arc::clone(&self.dispatched_actions);
            self.register_handler(Box::new(move |uri: &BstUri| {
                let is_install = uri.token.eq_ignore_ascii_case("install")
                    || uri
                        .params
                        .get("action")
                        .is_some_and(|s| s.eq_ignore_ascii_case("install"));
                if !is_install || !uri.is_valid() {
                    return None;
                }
                let target_all = uri.is_global_action();
                let silent = uri.params.get("silent").is_some_and(|v| {
                    v == "1" || v.eq_ignore_ascii_case("true") || v.is_empty()
                });
                log_ref
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(BstDispatchedAction::Install {
                        target_all,
                        app_id: uri.app_id,
                        silent,
                    });
                let mut handled = uri.clone();
                handled
                    .params
                    .insert("dispatched".to_string(), "install".to_string());
                Some(handled)
            }));
        }

        // 4. bst://redeem/<code|all> — Structured Redeem dispatch
        {
            let log_ref = Arc::clone(&self.dispatched_actions);
            self.register_handler(Box::new(move |uri: &BstUri| {
                let is_redeem = uri.token.eq_ignore_ascii_case("redeem")
                    || uri
                        .params
                        .get("action")
                        .is_some_and(|s| s.eq_ignore_ascii_case("redeem"));
                if !is_redeem || !uri.is_valid() {
                    return None;
                }
                let target_all = uri
                    .params
                    .get("target")
                    .is_some_and(|t| t.eq_ignore_ascii_case("all"));
                let code = uri
                    .params
                    .get("code")
                    .or_else(|| uri.params.get("target"))
                    .cloned()
                    .unwrap_or_default();
                log_ref
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(BstDispatchedAction::Redeem { target_all, code });
                let mut handled = uri.clone();
                handled
                    .params
                    .insert("dispatched".to_string(), "redeem".to_string());
                Some(handled)
            }));
        }

        // 5. Genuine per-app token handler (`bst://<appid>/<token>`)
        {
            let log_ref = Arc::clone(&self.dispatched_actions);
            self.register_handler(Box::new(move |uri: &BstUri| {
                if uri.is_app_token() {
                    log_ref
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(BstDispatchedAction::AppToken {
                            app_id: uri.app_id,
                            token: uri.token.clone(),
                        });
                    let mut handled = uri.clone();
                    handled
                        .params
                        .insert("dispatched".to_string(), "apptoken".to_string());
                    Some(handled)
                } else if uri.is_valid() || (uri.app_id > 0 && uri.token.is_empty()) {
                    Some(uri.clone())
                } else {
                    None
                }
            }));
        }
    }

    /// Register a handler for `bst://` URIs.
    pub fn register_handler(&mut self, handler: BstUriHandler) {
        self.handlers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push(handler);
    }

    /// Remove all registered handlers.
    pub fn clear_handlers(&mut self) {
        self.handlers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// Return the number of registered handlers.
    pub fn handler_count(&self) -> usize {
        self.handlers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Handle a `bst://` URI request by dispatching through registered handlers.
    pub fn handle_uri(&self, uri: &str) -> Option<BstUri> {
        let parsed = BstUri::parse(uri)?;
        let handlers = self.handlers.read().unwrap_or_else(|e| e.into_inner());
        for handler in handlers.iter() {
            if let Some(handled) = handler(&parsed) {
                return Some(handled);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steam_bridge::ost_platform::keyvalues1::{write_kv1, Kv1Value};
    use std::collections::BTreeMap;

    #[test]
    fn test_bst_uri_parse() {
        let uri = BstUri::parse("bst://12345/abc123token").unwrap();
        assert_eq!(uri.app_id, 12345);
        assert_eq!(uri.token, "abc123token");
        assert_eq!(uri.version, 1);
        assert!(uri.is_valid());
        assert!(uri.is_app_token());
        assert!(!uri.is_global_action());
    }

    #[test]
    fn test_bst_uri_parse_with_params() {
        let uri = BstUri::parse("bst://5678/token?key1=value1&key2=value2").unwrap();
        assert_eq!(uri.app_id, 5678);
        assert_eq!(uri.token, "token");
        assert_eq!(uri.params.get("key1"), Some(&"value1".to_string()));
        assert_eq!(uri.params.get("key2"), Some(&"value2".to_string()));
    }

    #[test]
    fn test_bst_uri_case_insensitive_fragment_percent_decode_and_version() {
        let uri = BstUri::parse("  BST://730/tok%2F1/mode=online?v=2&note=hello%20world#section  ")
            .unwrap();
        assert_eq!(uri.app_id, 730);
        assert_eq!(uri.token, "tok/1");
        assert_eq!(uri.version, 2);
        assert_eq!(uri.params.get("mode").map(String::as_str), Some("online"));
        assert_eq!(
            uri.params.get("note").map(String::as_str),
            Some("hello world")
        );
        assert!(uri.is_valid());
    }

    #[test]
    fn test_bst_uri_invalid() {
        assert!(BstUri::parse("http://example.com").is_none());
        assert!(BstUri::parse("bst://").is_none());
        assert!(BstUri::parse("bst://#onlyfrag").is_none());
        let bad_app = BstUri::parse("bst://notanumber/tok").unwrap();
        assert!(!bad_app.is_valid());

        let bad_refresh = BstUri::parse("bst://refreshachievements/notanumber").unwrap();
        assert!(!bad_refresh.is_valid());
        assert!(!bad_refresh.is_global_action());
    }

    #[test]
    fn test_bst_uri_valid() {
        let uri = BstUri::parse("bst://12345/token").unwrap();
        assert!(uri.is_valid());
    }

    #[test]
    fn test_bst_uri_parse_refreshachievements_480_and_all_actions() {
        let per_app = BstUri::parse("bst://refreshachievements/480").unwrap();
        assert_eq!(per_app.token, "refreshachievements");
        assert_eq!(per_app.app_id, 480);
        assert!(per_app.is_valid());
        assert!(per_app.is_action());
        assert!(!per_app.is_global_action());
        assert!(!per_app.is_app_token());

        let global = BstUri::parse("bst://refreshachievements/all").unwrap();
        assert_eq!(global.token, "refreshachievements");
        assert_eq!(global.app_id, 0);
        assert_eq!(global.params.get("target").map(String::as_str), Some("all"));
        assert!(global.is_valid());
        assert!(global.is_global_action());
        assert!(global.is_action());
        assert!(!global.is_app_token());

        for action in ["fix", "install", "redeem"] {
            let u = BstUri::parse(&format!("BST://{}/ALL", action.to_uppercase())).unwrap();
            assert_eq!(u.token, action);
            assert_eq!(u.app_id, 0);
            assert_eq!(u.params.get("target").map(String::as_str), Some("all"));
            assert!(u.is_valid());
            assert!(u.is_global_action());
            assert!(!u.is_app_token());
        }
    }

    #[test]
    fn test_token_bridge_refreshachievements_480_and_all_sync_real_kv1() {
        let tmp = tempfile::tempdir().unwrap();
        let steam_root = tmp.path().join("Steam");
        let stats_dir = steam_root.join("appcache/stats");
        fs::create_dir_all(&stats_dir).unwrap();
        fs::create_dir_all(steam_root.join("userdata/77777/config")).unwrap();

        let build_stats_and_schema_kv1 = |ach_names: &[&str]| -> (Vec<u8>, Vec<u8>) {
            let mut times_bits = BTreeMap::new();
            let mut schema_bits = BTreeMap::new();
            let mut mask = 0i32;
            for (idx, &name) in ach_names.iter().enumerate() {
                mask |= 1 << idx;
                times_bits.insert(
                    idx.to_string(),
                    Kv1Value::Int32(1_700_000_000 + idx as i32),
                );
                let mut bit_obj = BTreeMap::new();
                bit_obj.insert("name".to_string(), Kv1Value::String(name.to_string()));
                schema_bits.insert(idx.to_string(), Kv1Value::Object(bit_obj));
            }
            let mut times_root = BTreeMap::new();
            times_root.insert("1".to_string(), Kv1Value::Object(times_bits));
            let mut stats_root = BTreeMap::new();
            stats_root.insert("1".to_string(), Kv1Value::Int32(mask));
            stats_root.insert("AchievementTimes".to_string(), Kv1Value::Object(times_root));
            let stats_bytes =
                write_kv1("UserGameStats", &Kv1Value::Object(stats_root)).unwrap();

            let mut stat_entry = BTreeMap::new();
            stat_entry.insert("bits".to_string(), Kv1Value::Object(schema_bits));
            let mut schema_stats = BTreeMap::new();
            schema_stats.insert("1".to_string(), Kv1Value::Object(stat_entry));
            let mut schema_root = BTreeMap::new();
            schema_root.insert("stats".to_string(), Kv1Value::Object(schema_stats));
            let schema_bytes =
                write_kv1("UserGameStatsSchema", &Kv1Value::Object(schema_root)).unwrap();
            (stats_bytes, schema_bytes)
        };

        let (stats_480, schema_480) =
            build_stats_and_schema_kv1(&["ACH_WIN_ONE_GAME", "ACH_WIN_100_GAMES"]);
        fs::write(stats_dir.join("UserGameStats_77777_480.bin"), stats_480).unwrap();
        fs::write(stats_dir.join("UserGameStatsSchema_480.bin"), schema_480).unwrap();

        let (stats_730, schema_730) =
            build_stats_and_schema_kv1(&["WIN_BOMB_PLANT", "WIN_BOMB_DEFUSE", "ACE_ROUND"]);
        fs::write(stats_dir.join("UserGameStats_77777_730.bin"), stats_730).unwrap();
        fs::write(stats_dir.join("UserGameStatsSchema_730.bin"), schema_730).unwrap();

        let cloud = CloudRedirectService::with_path(steam_root.join("config/cloudredirect"));
        let bridge = TokenBridge::with_services(steam_root, cloud.clone());

        // 1. Single-app refresh (`bst://refreshachievements/480`)
        let res_480 = bridge.handle_uri("bst://refreshachievements/480").unwrap();
        assert_eq!(res_480.app_id, 480);
        assert_eq!(res_480.token, "refreshachievements");
        assert_eq!(cloud.local_achievement_count(480), 2);
        assert_eq!(cloud.local_achievement_count(730), 0);
        assert_eq!(
            bridge.last_dispatched_action(),
            Some(BstDispatchedAction::RefreshAchievements {
                target_all: false,
                app_id: 480,
                refreshed_apps: 1,
                synced_achievements: 2,
            })
        );

        // 2. Global refresh (`bst://refreshachievements/all`)
        let res_all = bridge.handle_uri("bst://refreshachievements/all").unwrap();
        assert_eq!(res_all.app_id, 0);
        assert_eq!(res_all.token, "refreshachievements");
        assert_eq!(res_all.params.get("target").map(String::as_str), Some("all"));
        assert_eq!(cloud.local_achievement_count(480), 2);
        assert_eq!(cloud.local_achievement_count(730), 3);
        assert_eq!(
            bridge.last_dispatched_action(),
            Some(BstDispatchedAction::RefreshAchievements {
                target_all: true,
                app_id: 0,
                refreshed_apps: 2,
                synced_achievements: 5,
            })
        );
    }

    #[test]
    fn test_token_bridge_dispatch_fallback_and_clear() {
        let mut bridge = TokenBridge::new();
        let initial_count = bridge.handler_count();
        assert!(initial_count >= 4);

        bridge.register_handler(Box::new(|_| None));
        bridge.register_handler(Box::new(|u| Some(u.clone())));
        assert_eq!(bridge.handler_count(), initial_count + 2);

        let out = bridge.handle_uri("bst://730/sessiontok").unwrap();
        assert_eq!(out.app_id, 730);
        assert_eq!(out.token, "sessiontok");
        assert_eq!(
            bridge.last_dispatched_action(),
            Some(BstDispatchedAction::AppToken {
                app_id: 730,
                token: "sessiontok".to_string(),
            })
        );

        bridge.clear_handlers();
        assert_eq!(bridge.handler_count(), 0);
        assert!(bridge.handle_uri("bst://730/sessiontok").is_none());
    }
}
