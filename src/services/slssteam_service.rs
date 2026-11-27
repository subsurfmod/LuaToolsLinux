//! SLSsteam `config.yaml` service — complete schema-compliant reading, healing, and writing
//! for upstream `AceSLS/SLSsteam` (`yaml-cpp`) plus companion ACCELA / LuaTools keys.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_yml::{Mapping, Value};

static SLS_TMP_COUNTER: AtomicU64 = AtomicU64::new(1);

use crate::models::config::{default_slssteam_config_path, SlssteamConfig};

/// Every top-level key required by `AceSLS/SLSsteam` (`src/config.cpp` `CConfig::loadSettings`)
/// so `yaml-cpp` never emits `Missing <Key>` or `Failed to parse <Key>` warnings.
pub const REQUIRED_SLSSTEAM_KEYS: &[&str] = &[
    "LogLevels",
    "API",
    "DisableFamilyShareLock",
    "UseWhitelist",
    "MaxSchemaTries",
    "SmartTickets",
    "SafeMode",
    "WarnHashMissmatch",
    "NotifyInit",
    "FakeName",
    "FakeEmail",
    "FakeWalletBalance",
    "Plugins",
    "DisableCloud",
    "DisableUpdates",
    "DumpClientInterfaces",
    "ExtendedLogging",
    "AppIds",
    "FakeOffline",
    "DepotBlacklist",
    "FakeAppIds",
    "ManifestIds",
    "AppTokens",
    "CDKeys",
    "GameTitles",
    "SubscriptionTimestamps",
    "SteamIdOverride",
    "LaunchOptions",
    "AdditionalApps",
    "IdleStatus",
    "DlcData",
    "DenuvoGames",
];

const KNOWN_KEYS: &[&str] = &[
    // Upstream AceSLS/SLSsteam keys
    "LogLevels",
    "API",
    "DisableFamilyShareLock",
    "UseWhitelist",
    "MaxSchemaTries",
    "SmartTickets",
    "SafeMode",
    "WarnHashMissmatch",
    "NotifyInit",
    "FakeName",
    "FakeEmail",
    "FakeWalletBalance",
    "Plugins",
    "DisableCloud",
    "DisableUpdates",
    "DumpClientInterfaces",
    "ExtendedLogging",
    "AppIds",
    "FakeOffline",
    "DepotBlacklist",
    "FakeAppIds",
    "ManifestIds",
    "AppTokens",
    "CDKeys",
    "GameTitles",
    "SubscriptionTimestamps",
    "SteamIdOverride",
    "LaunchOptions",
    "AdditionalApps",
    "IdleStatus",
    "DlcData",
    "DenuvoGames",
    // Legacy aliases & companion ACCELA / LuaTools keys
    "AppTokenStrings",
    "DepotKeys",
    "PlayNotOwnedGames",
    "playNotOwnedGames",
    "LimitDownloadsToSteamLibrary",
    "limitDownloadsToSteamLibrary",
    "workshop",
    "ManifestOverrides",
    "ManifestPins",
    "Manifests",
    "ProcessMap",
    "ForceDenuvo",
    "PinnedApps",
];

const LEGACY_ALIAS_KEYS: &[&str] = &[
    "ManifestOverrides",
    "ManifestPins",
    "Manifests",
    "playNotOwnedGames",
    "limitDownloadsToSteamLibrary",
];

#[derive(Debug, Clone)]
pub struct SlssteamService {
    pub config_path: PathBuf,
    pub config: SlssteamConfig,
}

impl Default for SlssteamService {
    fn default() -> Self {
        Self::new(None)
    }
}

impl SlssteamService {
    pub fn new(config_path: Option<PathBuf>) -> Self {
        let path = config_path.unwrap_or_else(default_slssteam_config_path);
        let (config, should_heal) = Self::load_from_with_status(&path);
        let svc = Self {
            config_path: path,
            config,
        };
        if should_heal {
            let _ = svc.save();
        }
        svc
    }

    pub fn reload(&mut self) {
        let (config, should_heal) = Self::load_from_with_status(&self.config_path);
        self.config = config;
        if should_heal {
            let _ = self.save();
        }
    }

    pub fn load(&mut self) {
        self.reload();
    }

    fn parse_u64_str(s: &str) -> Option<u64> {
        let clean = s.trim().trim_matches('"').trim_matches('\'').trim();
        if clean.is_empty() {
            return None;
        }
        if let Some(hex) = clean
            .strip_prefix("0x")
            .or_else(|| clean.strip_prefix("0X"))
        {
            u64::from_str_radix(hex, 16).ok()
        } else {
            clean.parse::<u64>().ok()
        }
    }

    fn value_to_u64(v: &Value) -> Option<u64> {
        match v {
            Value::Number(n) => n.as_u64(),
            Value::String(s) => Self::parse_u64_str(s),
            _ => None,
        }
    }

    fn value_to_u32(v: &Value) -> Option<u32> {
        Self::value_to_u64(v).and_then(|x| u32::try_from(x).ok())
    }

    fn value_to_i32(v: &Value) -> Option<i32> {
        match v {
            Value::Number(n) => n.as_i64().and_then(|x| i32::try_from(x).ok()),
            Value::String(s) => s
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .trim()
                .parse::<i32>()
                .ok(),
            _ => None,
        }
    }

    fn value_to_bool(v: &Value) -> Option<bool> {
        match v {
            Value::Bool(b) => Some(*b),
            Value::Number(n) => n.as_i64().map(|x| x != 0),
            Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "y" | "on" | "1" => Some(true),
                "false" | "no" | "n" | "off" | "0" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }

    fn value_to_string(v: &Value) -> Option<String> {
        match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }

    fn yaml_quote(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for ch in s.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    fn parse_u32_list(v: &Value, target: &mut Vec<u32>) {
        match v {
            Value::Sequence(seq) => {
                for item in seq {
                    if let Some(aid) = Self::value_to_u32(item) {
                        if !target.contains(&aid) {
                            target.push(aid);
                        }
                    }
                }
            }
            Value::String(s) => {
                for tok in s.split_whitespace() {
                    if let Some(aid) = Self::parse_u64_str(tok).and_then(|x| u32::try_from(x).ok())
                    {
                        if !target.contains(&aid) {
                            target.push(aid);
                        }
                    }
                }
            }
            Value::Number(_) => {
                if let Some(aid) = Self::value_to_u32(v) {
                    if !target.contains(&aid) {
                        target.push(aid);
                    }
                }
            }
            _ => {}
        }
    }

    /// Checks whether a parsed YAML mapping is missing any required SLSsteam schema keys or
    /// contains malformed `ManifestIds` / `AppTokens` / `SteamIdOverride` types that would cause
    /// `yaml-cpp` warnings on Steam startup.
    fn mapping_needs_heal(map: &Mapping) -> bool {
        for &req in REQUIRED_SLSSTEAM_KEYS {
            let k = Value::String(req.to_string());
            match map.get(&k) {
                None | Some(Value::Null) => return true,
                _ => {}
            }
        }
        for &alias in LEGACY_ALIAS_KEYS {
            if map.contains_key(&Value::String(alias.to_string())) {
                return true;
            }
        }
        if let Some(m_val) = map.get(&Value::String("ManifestIds".into())) {
            match m_val {
                Value::Mapping(sub) => {
                    for (dk, mv) in sub {
                        let key_ok = matches!(dk, Value::Number(n) if n.as_u64().and_then(|x| u32::try_from(x).ok()).is_some())
                            || matches!(dk, Value::String(s) if s.parse::<u32>().is_ok());
                        let val_ok = matches!(mv, Value::Number(n) if n.as_u64().is_some());
                        if !key_ok || !val_ok {
                            return true;
                        }
                    }
                }
                _ => return true,
            }
        }
        if let Some(t_val) = map.get(&Value::String("AppTokens".into())) {
            match t_val {
                Value::Mapping(sub) => {
                    for (ak, tv) in sub {
                        let key_ok = matches!(ak, Value::Number(n) if n.as_u64().and_then(|x| u32::try_from(x).ok()).is_some())
                            || matches!(ak, Value::String(s) if s.parse::<u32>().is_ok());
                        let val_ok = matches!(tv, Value::Number(n) if n.as_u64().is_some());
                        if !key_ok || !val_ok {
                            return true;
                        }
                    }
                }
                _ => return true,
            }
        }
        if let Some(s_val) = map.get(&Value::String("SteamIdOverride".into())) {
            if !matches!(s_val, Value::Mapping(_)) {
                return true;
            }
        }
        false
    }

    pub fn parse_yaml(raw: &str, path: &Path) -> SlssteamConfig {
        let (cfg, _) = Self::parse_yaml_with_status(raw, path);
        cfg
    }

    fn parse_yaml_with_status(raw: &str, path: &Path) -> (SlssteamConfig, bool) {
        let mut cfg = SlssteamConfig {
            path: path.to_path_buf(),
            ..Default::default()
        };
        let Ok(Value::Mapping(map)) = serde_yml::from_str::<Value>(raw) else {
            return (cfg, !raw.trim().is_empty());
        };

        let needs_heal = Self::mapping_needs_heal(&map);

        for (k, v) in &map {
            let Some(key_str) = k.as_str() else {
                continue;
            };
            if !KNOWN_KEYS.contains(&key_str) {
                cfg.extra_fields.insert(key_str.to_string(), v.clone());
                continue;
            }
            match key_str {
                "LogLevels" => {
                    if let Some(lvl) = Self::value_to_u32(v) {
                        cfg.log_levels = lvl;
                    }
                }
                "API" => {
                    if let Some(b) = Self::value_to_bool(v) {
                        cfg.api = b;
                    }
                }
                "DisableFamilyShareLock" => {
                    if let Some(b) = Self::value_to_bool(v) {
                        cfg.disable_family_share_lock = b;
                    }
                }
                "UseWhitelist" => {
                    if let Some(b) = Self::value_to_bool(v) {
                        cfg.use_whitelist = b;
                    }
                }
                "MaxSchemaTries" => {
                    if let Some(n) = Self::value_to_u32(v) {
                        cfg.max_schema_tries = n;
                    }
                }
                "SmartTickets" => {
                    if let Some(n) = Self::value_to_u32(v) {
                        cfg.smart_tickets = n;
                    }
                }
                "SafeMode" => {
                    if let Some(b) = Self::value_to_bool(v) {
                        cfg.safe_mode = b;
                    }
                }
                "WarnHashMissmatch" => {
                    if let Some(b) = Self::value_to_bool(v) {
                        cfg.warn_hash_missmatch = b;
                    }
                }
                "NotifyInit" => {
                    if let Some(b) = Self::value_to_bool(v) {
                        cfg.notify_init = b;
                    }
                }
                "FakeName" => {
                    if let Some(s) = Self::value_to_string(v) {
                        cfg.fake_name = s;
                    }
                }
                "FakeEmail" => {
                    if let Some(s) = Self::value_to_string(v) {
                        cfg.fake_email = s;
                    }
                }
                "FakeWalletBalance" => {
                    if let Some(bal) = Self::value_to_i32(v) {
                        cfg.fake_wallet_balance = bal;
                    }
                }
                "Plugins" => {
                    if let Some(b) = Self::value_to_bool(v) {
                        cfg.plugins = b;
                    }
                }
                "DisableCloud" => {
                    if let Some(b) = Self::value_to_bool(v) {
                        cfg.disable_cloud = b;
                    }
                }
                "DisableUpdates" => {
                    if let Some(b) = Self::value_to_bool(v) {
                        cfg.disable_updates = b;
                    }
                }
                "DumpClientInterfaces" => {
                    if let Some(b) = Self::value_to_bool(v) {
                        cfg.dump_client_interfaces = b;
                    }
                }
                "ExtendedLogging" => {
                    if let Some(b) = Self::value_to_bool(v) {
                        cfg.extended_logging = b;
                    }
                }
                "AppIds" => Self::parse_u32_list(v, &mut cfg.app_ids),
                "FakeOffline" => Self::parse_u32_list(v, &mut cfg.fake_offline),
                "DepotBlacklist" => Self::parse_u32_list(v, &mut cfg.depot_blacklist),
                "AdditionalApps" => Self::parse_u32_list(v, &mut cfg.additional_apps),
                "FakeAppIds" => {
                    if let Value::Mapping(sub) = v {
                        for (ak, dv) in sub {
                            if let (Some(aid), Some(did)) =
                                (Self::value_to_u32(ak), Self::value_to_u32(dv))
                            {
                                cfg.fake_app_ids.insert(aid, did);
                            }
                        }
                    }
                }
                "AppTokens" | "AppTokenStrings" => {
                    if let Value::Mapping(sub) = v {
                        for (ak, tv) in sub {
                            if let (Some(aid), Some(tok)) =
                                (Self::value_to_u32(ak), Self::value_to_string(tv))
                            {
                                let trimmed = tok.trim();
                                if !trimmed.is_empty() {
                                    if let Some(num) = Self::parse_u64_str(trimmed) {
                                        cfg.app_tokens.insert(aid, num.to_string());
                                    } else {
                                        cfg.app_tokens.insert(aid, trimmed.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
                "DlcData" => {
                    if let Value::Mapping(sub) = v {
                        for (ak, dlcs_v) in sub {
                            if let (Some(aid), Value::Mapping(dlcs_map)) =
                                (Self::value_to_u32(ak), dlcs_v)
                            {
                                let mut inner = BTreeMap::new();
                                for (dk, dname) in dlcs_map {
                                    if let (Some(did), Some(nm)) =
                                        (Self::value_to_u32(dk), Self::value_to_string(dname))
                                    {
                                        inner.insert(did, nm);
                                    }
                                }
                                if !inner.is_empty() {
                                    cfg.dlc_data.insert(aid, inner);
                                }
                            }
                        }
                    }
                }
                "DepotKeys" => {
                    if let Value::Mapping(sub) = v {
                        for (dk, kv) in sub {
                            if let (Some(did), Some(kstr)) =
                                (Self::value_to_u32(dk), Self::value_to_string(kv))
                            {
                                cfg.depot_keys.insert(did, kstr);
                            }
                        }
                    }
                }
                "ManifestOverrides" | "ManifestPins" | "Manifests" | "ManifestIds" => {
                    if let Value::Mapping(sub) = v {
                        for (dk, mv) in sub {
                            if let Value::Mapping(nested) = mv {
                                // Heal nested appId -> { depotId: manifestId } entries into flat depotId -> uint64
                                for (inner_dk, inner_mv) in nested {
                                    if let (Some(did), Some(gid)) = (
                                        Self::value_to_u32(inner_dk),
                                        Self::value_to_u64(inner_mv),
                                    ) {
                                        cfg.manifest_overrides.insert(did, gid.to_string());
                                    }
                                }
                            } else if let (Some(did), Some(gid)) =
                                (Self::value_to_u32(dk), Self::value_to_u64(mv))
                            {
                                cfg.manifest_overrides.insert(did, gid.to_string());
                            }
                        }
                    }
                }
                "CDKeys" => {
                    if let Value::Mapping(sub) = v {
                        for (ak, kv) in sub {
                            if let (Some(aid), Some(kstr)) =
                                (Self::value_to_u32(ak), Self::value_to_string(kv))
                            {
                                cfg.cd_keys.insert(aid, kstr);
                            }
                        }
                    }
                }
                "GameTitles" => {
                    if let Value::Mapping(sub) = v {
                        for (ak, tv) in sub {
                            if let (Some(aid), Some(title)) =
                                (Self::value_to_u32(ak), Self::value_to_string(tv))
                            {
                                cfg.game_titles.insert(aid, title);
                            }
                        }
                    }
                }
                "SubscriptionTimestamps" => {
                    if let Value::Mapping(sub) = v {
                        for (ak, tv) in sub {
                            if let (Some(aid), Some(ts)) =
                                (Self::value_to_u32(ak), Self::value_to_u32(tv))
                            {
                                cfg.subscription_timestamps.insert(aid, ts);
                            }
                        }
                    }
                }
                "SteamIdOverride" => match v {
                    Value::Mapping(sub) => {
                        for (ak, sv) in sub {
                            if let (Some(aid), Some(sid)) =
                                (Self::value_to_u32(ak), Self::value_to_u64(sv))
                            {
                                cfg.steam_id_overrides.insert(aid, sid);
                                if aid == 0 {
                                    cfg.steam_id_override = Some(sid);
                                }
                            }
                        }
                    }
                    _ => {
                        if let Some(sid) = Self::value_to_u64(v) {
                            cfg.steam_id_override = Some(sid);
                            cfg.steam_id_overrides.insert(0, sid);
                        }
                    }
                },
                "LaunchOptions" => {
                    if let Value::Mapping(sub) = v {
                        for (ak, lv) in sub {
                            if let (Some(aid), Some(opt)) =
                                (Self::value_to_u32(ak), Self::value_to_string(lv))
                            {
                                cfg.launch_options.insert(aid, opt);
                            }
                        }
                    }
                }
                "IdleStatus" => {
                    if let Value::Mapping(sub) = v {
                        if let Some(aid) = sub
                            .get(&Value::String("AppId".into()))
                            .and_then(Self::value_to_u32)
                        {
                            cfg.idle_status_app_id = aid;
                        }
                        if let Some(title) = sub
                            .get(&Value::String("Title".into()))
                            .and_then(Self::value_to_string)
                        {
                            cfg.idle_status_title = title;
                        }
                    }
                }
                "DenuvoGames" => {
                    if let Value::Mapping(sub) = v {
                        for (sid_k, app_seq) in sub {
                            if let (Some(sid), Value::Sequence(seq)) =
                                (Self::value_to_u64(sid_k), app_seq)
                            {
                                let mut aids = Vec::new();
                                for it in seq {
                                    if let Some(aid) = Self::value_to_u32(it) {
                                        if !aids.contains(&aid) {
                                            aids.push(aid);
                                        }
                                    }
                                }
                                cfg.denuvo_games.insert(sid, aids);
                            }
                        }
                    }
                }
                "ProcessMap" => {
                    if let Value::Mapping(sub) = v {
                        for (ak, ev) in sub {
                            if let (Some(aid), Some(exe)) =
                                (Self::value_to_u32(ak), Self::value_to_string(ev))
                            {
                                cfg.process_map.insert(aid, exe);
                            }
                        }
                    }
                }
                "ForceDenuvo" => Self::parse_u32_list(v, &mut cfg.force_denuvo),
                "PinnedApps" => Self::parse_u32_list(v, &mut cfg.pinned_apps),
                "PlayNotOwnedGames" | "playNotOwnedGames" => {
                    if let Some(b) = Self::value_to_bool(v) {
                        cfg.play_not_owned = b;
                    }
                }
                "LimitDownloadsToSteamLibrary" | "limitDownloadsToSteamLibrary" => {
                    if let Some(b) = Self::value_to_bool(v) {
                        cfg.limit_downloads = b;
                    }
                }
                "workshop" => {
                    if let Value::Mapping(wmap) = v {
                        let key = Value::String("workshopPath".into());
                        if let Some(wp) = wmap.get(&key).and_then(Self::value_to_string) {
                            cfg.workshop_path = wp;
                        }
                    }
                }
                _ => {}
            }
        }
        (cfg, needs_heal)
    }

    fn load_from_with_status(path: &Path) -> (SlssteamConfig, bool) {
        let Ok(raw) = fs::read_to_string(path) else {
            return (
                SlssteamConfig {
                    path: path.to_path_buf(),
                    ..Default::default()
                },
                false,
            );
        };
        Self::parse_yaml_with_status(&raw, path)
    }

    fn push_u32_seq(lines: &mut Vec<String>, key: &str, items: &[u32]) {
        let mut sorted = items.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        if sorted.is_empty() {
            lines.push(format!("{}: []", key));
        } else {
            lines.push(format!("{}:", key));
            for id in sorted {
                lines.push(format!("  - {}", id));
            }
        }
    }

    pub fn serialize_yaml(&self) -> String {
        let cfg = &self.config;
        let mut lines: Vec<String> = Vec::new();

        // 1. Scalar settings required by AceSLS/SLSsteam
        lines.push(format!("LogLevels: 0x{:x}", cfg.log_levels));
        lines.push(format!("API: {}", cfg.api));
        lines.push(format!(
            "DisableFamilyShareLock: {}",
            cfg.disable_family_share_lock
        ));
        lines.push(format!("UseWhitelist: {}", cfg.use_whitelist));
        lines.push(format!("MaxSchemaTries: {}", cfg.max_schema_tries));
        lines.push(format!("SmartTickets: 0x{:x}", cfg.smart_tickets));
        lines.push(format!("SafeMode: {}", cfg.safe_mode));
        lines.push(format!("WarnHashMissmatch: {}", cfg.warn_hash_missmatch));
        lines.push(format!("NotifyInit: {}", cfg.notify_init));
        lines.push(format!("FakeName: {}", Self::yaml_quote(&cfg.fake_name)));
        lines.push(format!("FakeEmail: {}", Self::yaml_quote(&cfg.fake_email)));
        lines.push(format!("FakeWalletBalance: {}", cfg.fake_wallet_balance));
        lines.push(format!("Plugins: {}", cfg.plugins));
        lines.push(format!("DisableCloud: {}", cfg.disable_cloud));
        lines.push(format!("DisableUpdates: {}", cfg.disable_updates));
        lines.push(format!(
            "DumpClientInterfaces: {}",
            cfg.dump_client_interfaces
        ));
        lines.push(format!("ExtendedLogging: {}", cfg.extended_logging));
        lines.push(String::new());

        // 2. Sequence settings required by AceSLS/SLSsteam
        Self::push_u32_seq(&mut lines, "AppIds", &cfg.app_ids);
        Self::push_u32_seq(&mut lines, "FakeOffline", &cfg.fake_offline);
        Self::push_u32_seq(&mut lines, "DepotBlacklist", &cfg.depot_blacklist);
        Self::push_u32_seq(&mut lines, "AdditionalApps", &cfg.additional_apps);
        lines.push(String::new());

        // 3. Map & structured settings required by AceSLS/SLSsteam
        if cfg.fake_app_ids.is_empty() {
            lines.push("FakeAppIds: {}".into());
        } else {
            lines.push("FakeAppIds:".into());
            for (aid, did) in &cfg.fake_app_ids {
                lines.push(format!("  {}: {}", aid, did));
            }
        }

        // ManifestIds: SLSsteam parses `getMap<AppId_t, uint64_t>(rootNode, "ManifestIds")`.
        // Emit only valid unquoted base-10 uint64 values (or `{}` when empty).
        let valid_manifests: Vec<(u32, u64)> = cfg
            .manifest_overrides
            .iter()
            .filter_map(|(&did, gid)| Self::parse_u64_str(gid).map(|g| (did, g)))
            .collect();
        if valid_manifests.is_empty() {
            lines.push("ManifestIds: {}".into());
        } else {
            lines.push("ManifestIds:".into());
            for (did, gid) in valid_manifests {
                lines.push(format!("  {}: {}", did, gid));
            }
        }

        // AppTokens: SLSsteam parses `getMap<AppId_t, uint64_t>(rootNode, "AppTokens")`.
        // Numeric u64 PICS access tokens go into `AppTokens`; non-numeric string tokens
        // are preserved in `AppTokenStrings` so SLSsteam never throws a parse error.
        let mut numeric_tokens: Vec<(u32, u64)> = Vec::new();
        let mut string_tokens: Vec<(u32, &str)> = Vec::new();
        for (&aid, tok) in &cfg.app_tokens {
            let clean = tok.trim();
            if clean.is_empty() {
                continue;
            }
            if let Some(num) = Self::parse_u64_str(clean) {
                numeric_tokens.push((aid, num));
            } else {
                string_tokens.push((aid, clean));
            }
        }
        if numeric_tokens.is_empty() {
            lines.push("AppTokens: {}".into());
        } else {
            lines.push("AppTokens:".into());
            for (aid, num) in numeric_tokens {
                lines.push(format!("  {}: {}", aid, num));
            }
        }

        if cfg.cd_keys.is_empty() {
            lines.push("CDKeys: {}".into());
        } else {
            lines.push("CDKeys:".into());
            for (aid, key) in &cfg.cd_keys {
                let clean_key = key.trim().trim_matches('"');
                lines.push(format!("  {}: {}", aid, Self::yaml_quote(clean_key)));
            }
        }

        if cfg.game_titles.is_empty() {
            lines.push("GameTitles: {}".into());
        } else {
            lines.push("GameTitles:".into());
            for (aid, title) in &cfg.game_titles {
                lines.push(format!("  {}: {}", aid, Self::yaml_quote(title.trim())));
            }
        }

        if cfg.subscription_timestamps.is_empty() {
            lines.push("SubscriptionTimestamps: {}".into());
        } else {
            lines.push("SubscriptionTimestamps:".into());
            for (aid, ts) in &cfg.subscription_timestamps {
                lines.push(format!("  {}: {}", aid, ts));
            }
        }

        let mut effective_steam_ids = cfg.steam_id_overrides.clone();
        if let Some(sid) = cfg.steam_id_override {
            effective_steam_ids.entry(0).or_insert(sid);
        }
        if effective_steam_ids.is_empty() {
            lines.push("SteamIdOverride: {}".into());
        } else {
            lines.push("SteamIdOverride:".into());
            for (aid, sid) in effective_steam_ids {
                lines.push(format!("  {}: {}", aid, sid));
            }
        }

        if cfg.launch_options.is_empty() {
            lines.push("LaunchOptions: {}".into());
        } else {
            lines.push("LaunchOptions:".into());
            for (aid, opt) in &cfg.launch_options {
                lines.push(format!("  {}: {}", aid, Self::yaml_quote(opt)));
            }
        }

        lines.push("IdleStatus:".into());
        lines.push(format!("  AppId: {}", cfg.idle_status_app_id));
        lines.push(format!(
            "  Title: {}",
            Self::yaml_quote(&cfg.idle_status_title)
        ));

        let non_empty_dlc: Vec<(&u32, &BTreeMap<u32, String>)> = cfg
            .dlc_data
            .iter()
            .filter(|(_, dlcs)| !dlcs.is_empty())
            .collect();
        if non_empty_dlc.is_empty() {
            lines.push("DlcData: {}".into());
        } else {
            lines.push("DlcData:".into());
            for (aid, dlcs) in non_empty_dlc {
                lines.push(format!("  {}:", aid));
                for (did, name) in dlcs {
                    lines.push(format!("    {}: {}", did, Self::yaml_quote(name)));
                }
            }
        }

        if cfg.denuvo_games.is_empty() {
            lines.push("DenuvoGames: {}".into());
        } else {
            lines.push("DenuvoGames:".into());
            for (sid, aids) in &cfg.denuvo_games {
                let mut sorted = aids.clone();
                sorted.sort_unstable();
                sorted.dedup();
                if sorted.is_empty() {
                    lines.push(format!("  {}: []", sid));
                } else {
                    lines.push(format!("  {}:", sid));
                    for aid in sorted {
                        lines.push(format!("    - {}", aid));
                    }
                }
            }
        }
        lines.push(String::new());

        // 4. Companion ACCELA / LuaTools extensions (ignored by SLSsteam)
        if !string_tokens.is_empty() {
            lines.push("AppTokenStrings:".into());
            for (aid, tok) in string_tokens {
                let clean_tok = tok.trim_matches('"');
                lines.push(format!("  {}: {}", aid, Self::yaml_quote(clean_tok)));
            }
        }

        if !cfg.depot_keys.is_empty() {
            lines.push("DepotKeys:".into());
            for (did, key) in &cfg.depot_keys {
                let clean_key = key.trim().trim_matches('"');
                lines.push(format!("  {}: {}", did, Self::yaml_quote(clean_key)));
            }
        }

        if !cfg.process_map.is_empty() {
            lines.push("ProcessMap:".into());
            for (aid, exe) in &cfg.process_map {
                let clean_exe = exe.trim().trim_matches('"');
                lines.push(format!("  {}: {}", aid, Self::yaml_quote(clean_exe)));
            }
        }

        if !cfg.force_denuvo.is_empty() {
            Self::push_u32_seq(&mut lines, "ForceDenuvo", &cfg.force_denuvo);
        }

        if !cfg.pinned_apps.is_empty() {
            Self::push_u32_seq(&mut lines, "PinnedApps", &cfg.pinned_apps);
        }

        lines.push(format!("PlayNotOwnedGames: {}", cfg.play_not_owned));
        lines.push(format!(
            "LimitDownloadsToSteamLibrary: {}",
            cfg.limit_downloads
        ));

        if !cfg.workshop_path.trim().is_empty() {
            lines.push("workshop:".into());
            lines.push(format!(
                "  workshopPath: {}",
                Self::yaml_quote(cfg.workshop_path.trim())
            ));
        }

        if !cfg.extra_fields.is_empty() {
            let mut extra_map = Mapping::new();
            for (k, v) in &cfg.extra_fields {
                if !KNOWN_KEYS.contains(&k.as_str()) {
                    extra_map.insert(Value::String(k.clone()), v.clone());
                }
            }
            if !extra_map.is_empty() {
                if let Ok(extra_str) = serde_yml::to_string(&Value::Mapping(extra_map)) {
                    let trimmed = extra_str.trim();
                    let without_doc_sep = trimmed
                        .strip_prefix("---\n")
                        .or_else(|| trimmed.strip_prefix("---\r\n"))
                        .unwrap_or(trimmed)
                        .trim();
                    if !without_doc_sep.is_empty() && without_doc_sep != "{}" {
                        lines.push(String::new());
                        lines.push(without_doc_sep.to_string());
                    }
                }
            }
        }

        let mut out = lines.join("\n");
        out.push('\n');
        out
    }

    pub fn save(&self) -> bool {
        if let Some(parent) = self.config_path.parent() {
            if fs::create_dir_all(parent).is_err() {
                return false;
            }
        }
        let content = self.serialize_yaml();
        let cnt = SLS_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = self.config_path.with_extension(format!("yaml.tmp.{}.{}", std::process::id(), cnt));
        if fs::write(&tmp, content).is_err() {
            return false;
        }
        fs::rename(&tmp, &self.config_path).is_ok()
    }

    pub fn get_fake_app_id(&self, app_id: u32) -> Option<u32> {
        self.config.fake_app_ids.get(&app_id).copied()
    }

    pub fn set_fake_app_id(&mut self, app_id: u32, depot_id: u32) {
        self.config.fake_app_ids.insert(app_id, depot_id);
    }

    pub fn remove_fake_app_id(&mut self, app_id: u32) -> bool {
        self.config.fake_app_ids.remove(&app_id).is_some()
    }

    pub fn get_token(&self, app_id: u32) -> Option<&str> {
        self.config.app_tokens.get(&app_id).map(String::as_str)
    }

    pub fn set_token(&mut self, app_id: u32, token: String) {
        let clean = token.trim();
        if clean.is_empty() {
            self.config.app_tokens.remove(&app_id);
        } else if let Some(num) = Self::parse_u64_str(clean) {
            self.config.app_tokens.insert(app_id, num.to_string());
        } else {
            self.config.app_tokens.insert(app_id, clean.to_string());
        }
    }

    pub fn remove_token(&mut self, app_id: u32) -> bool {
        self.config.app_tokens.remove(&app_id).is_some()
    }

    pub fn add_additional_app(&mut self, app_id: u32) {
        if !self.config.additional_apps.contains(&app_id) {
            self.config.additional_apps.push(app_id);
        }
    }

    pub fn remove_additional_app(&mut self, app_id: u32) {
        self.config.additional_apps.retain(|&x| x != app_id);
    }

    pub fn set_depot_key(&mut self, depot_id: u32, key: String) {
        self.config.depot_keys.insert(depot_id, key);
    }

    pub fn get_manifest_override(&self, depot_id: u32) -> Option<&str> {
        self.config
            .manifest_overrides
            .get(&depot_id)
            .map(String::as_str)
    }

    /// Sets a depot manifest override in `ManifestIds`. Only valid `uint64` manifest GIDs are
    /// accepted so `SLSsteam`'s `getMap<AppId_t, uint64_t>` never fails to parse `ManifestIds`.
    pub fn set_manifest_override(&mut self, depot_id: u32, manifest_id: String) {
        if let Some(gid_u64) = Self::parse_u64_str(&manifest_id) {
            self.config
                .manifest_overrides
                .insert(depot_id, gid_u64.to_string());
        }
    }

    pub fn remove_manifest_override(&mut self, depot_id: u32) -> bool {
        self.config.manifest_overrides.remove(&depot_id).is_some()
    }

    pub fn set_process_mapping(&mut self, app_id: u32, exe: String) {
        self.config.process_map.insert(app_id, exe);
    }

    pub fn get_process_mapping(&self, app_id: u32) -> Option<&str> {
        self.config.process_map.get(&app_id).map(String::as_str)
    }

    pub fn remove_process_mapping(&mut self, app_id: u32) -> bool {
        self.config.process_map.remove(&app_id).is_some()
    }

    pub fn add_force_denuvo(&mut self, app_id: u32) {
        if !self.config.force_denuvo.contains(&app_id) {
            self.config.force_denuvo.push(app_id);
        }
    }

    pub fn remove_force_denuvo(&mut self, app_id: u32) {
        self.config.force_denuvo.retain(|&x| x != app_id);
    }

    pub fn is_force_denuvo(&self, app_id: u32) -> bool {
        self.config.force_denuvo.contains(&app_id)
    }

    pub fn add_pinned_app(&mut self, app_id: u32) {
        if !self.config.pinned_apps.contains(&app_id) {
            self.config.pinned_apps.push(app_id);
        }
    }

    pub fn remove_pinned_app(&mut self, app_id: u32) {
        self.config.pinned_apps.retain(|&x| x != app_id);
    }

    pub fn is_pinned_app(&self, app_id: u32) -> bool {
        self.config.pinned_apps.contains(&app_id)
    }

    pub fn set_cd_key(&mut self, app_id: u32, key: String) {
        self.config.cd_keys.insert(app_id, key);
    }

    pub fn get_cd_key(&self, app_id: u32) -> Option<&str> {
        self.config.cd_keys.get(&app_id).map(String::as_str)
    }

    pub fn remove_cd_key(&mut self, app_id: u32) -> bool {
        self.config.cd_keys.remove(&app_id).is_some()
    }

    pub fn set_game_title(&mut self, app_id: u32, title: String) {
        self.config.game_titles.insert(app_id, title);
    }

    pub fn get_game_title(&self, app_id: u32) -> Option<&str> {
        self.config.game_titles.get(&app_id).map(String::as_str)
    }

    pub fn remove_game_title(&mut self, app_id: u32) -> bool {
        self.config.game_titles.remove(&app_id).is_some()
    }

    pub fn set_denuvo_games(&mut self, steam_id: u64, app_ids: Vec<u32>) {
        self.config.denuvo_games.insert(steam_id, app_ids);
    }

    pub fn set_steam_id_override(&mut self, steam_id: Option<u64>) {
        self.config.steam_id_override = steam_id;
        match steam_id {
            Some(sid) => {
                self.config.steam_id_overrides.insert(0, sid);
            }
            None => {
                self.config.steam_id_overrides.remove(&0);
            }
        }
    }

    pub fn set_app_steam_id_override(&mut self, app_id: u32, steam_id: u64) {
        self.config.steam_id_overrides.insert(app_id, steam_id);
        if app_id == 0 {
            self.config.steam_id_override = Some(steam_id);
        }
    }

    pub fn remove_app_steam_id_override(&mut self, app_id: u32) -> bool {
        if app_id == 0 {
            self.config.steam_id_override = None;
        }
        self.config.steam_id_overrides.remove(&app_id).is_some()
    }

    /// Synchronizes manifest pins into SLSsteam's `ManifestIds` so they take effect under SLSsteam.so.
    pub fn sync_manifest_pins(&mut self, pins: &[(u32, String)]) {
        for (depot_id, gid) in pins {
            self.set_manifest_override(*depot_id, gid.clone());
        }
    }

    /// Synchronizes from manifest_pins.json file
    pub fn sync_from_manifest_pins_file(&mut self, pins_path: Option<&Path>) -> bool {
        let default_path =
            crate::models::config::default_luatools_data_dir().join("manifest_pins.json");
        let path = pins_path.unwrap_or(&default_path);
        if let Ok(raw) = fs::read_to_string(path) {
            if let Ok(list) = serde_json::from_str::<Vec<serde_json::Value>>(&raw) {
                let mut changed = false;
                for item in list {
                    if let (Some(did), Some(gid)) = (
                        item.get("depot_id")
                            .and_then(|v| v.as_u64())
                            .and_then(|v| u32::try_from(v).ok()),
                        item.get("manifest_id").and_then(|v| v.as_str()),
                    ) {
                        if let Some(gid_u64) = Self::parse_u64_str(gid) {
                            self.config
                                .manifest_overrides
                                .insert(did, gid_u64.to_string());
                            changed = true;
                        }
                    }
                }
                if changed {
                    return self.save();
                }
            }
        }
        false
    }

    /// Synchronizes all bridge states (spoofed apps, fake app IDs, depot keys, tokens, and manifest pins)
    /// into SLSsteam config.yaml.
    pub fn sync_bridge_states(
        &mut self,
        additional_apps: &[u32],
        fake_app_ids: &[(u32, u32)],
        depot_keys: &[(u32, String)],
        app_tokens: &[(u32, String)],
        manifest_pins: &[(u32, String)],
    ) -> bool {
        self.config.play_not_owned = true;
        for &app_id in additional_apps {
            self.add_additional_app(app_id);
        }
        for &(app_id, target_id) in fake_app_ids {
            self.set_fake_app_id(app_id, target_id);
        }
        for (depot_id, key) in depot_keys {
            self.set_depot_key(*depot_id, key.clone());
        }
        for (app_id, token) in app_tokens {
            self.set_token(*app_id, token.clone());
        }
        for (depot_id, gid) in manifest_pins {
            self.set_manifest_override(*depot_id, gid.clone());
        }
        self.save()
    }

    /// Synchronizes extended directives (`process_map`, `force_denuvo`, `pinned_apps`) from parsed Lua.
    pub fn sync_parsed_lua_extended(
        &mut self,
        process_map: &[(u32, String)],
        force_denuvo: &[u32],
        pinned_apps: &[u32],
    ) -> bool {
        for (aid, exe) in process_map {
            self.set_process_mapping(*aid, exe.clone());
        }
        for &aid in force_denuvo {
            self.add_force_denuvo(aid);
        }
        for &aid in pinned_apps {
            self.add_pinned_app(aid);
        }
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact 30 keys reported by `AceSLS/SLSsteam` in `CConfig::loadSettings`.
    const REPORTED_30_KEYS: &[&str] = &[
        "LogLevels",
        "API",
        "DisableFamilyShareLock",
        "UseWhitelist",
        "MaxSchemaTries",
        "SafeMode",
        "WarnHashMissmatch",
        "NotifyInit",
        "FakeName",
        "FakeEmail",
        "FakeWalletBalance",
        "Plugins",
        "DisableCloud",
        "DisableUpdates",
        "DumpClientInterfaces",
        "ExtendedLogging",
        "AppIds",
        "FakeOffline",
        "DepotBlacklist",
        "FakeAppIds",
        "ManifestIds",
        "AppTokens",
        "CDKeys",
        "GameTitles",
        "SubscriptionTimestamps",
        "SteamIdOverride",
        "LaunchOptions",
        "AdditionalApps",
        "DlcData",
        "DenuvoGames",
    ];

    #[test]
    fn default_empty_config_emits_all_30_plus_slssteam_keys_with_valid_yaml_types() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.yaml");

        let svc = SlssteamService::new(Some(cfg_path.clone()));
        assert!(!svc.config.exists());
        assert!(svc.save());

        let raw_yaml = fs::read_to_string(&cfg_path).unwrap();
        let parsed: Value = serde_yml::from_str(&raw_yaml).expect("valid YAML document");
        let Value::Mapping(map) = parsed else {
            panic!("root YAML node must be a mapping");
        };

        assert_eq!(REPORTED_30_KEYS.len(), 30);
        for &key in REPORTED_30_KEYS {
            let val = map
                .get(&Value::String(key.to_string()))
                .unwrap_or_else(|| panic!("missing required SLSsteam key: {}", key));
            assert!(
                !matches!(val, Value::Null),
                "key {} must not serialize as null",
                key
            );
        }

        // Verify exact YAML node types expected by yaml-cpp in CConfig::loadSettings
        for &seq_key in &["AppIds", "FakeOffline", "DepotBlacklist", "AdditionalApps"] {
            assert!(
                matches!(map.get(&Value::String(seq_key.into())), Some(Value::Sequence(s)) if s.is_empty()),
                "{} must be an empty YAML sequence []",
                seq_key
            );
        }
        for &map_key in &[
            "FakeAppIds",
            "ManifestIds",
            "AppTokens",
            "CDKeys",
            "GameTitles",
            "SubscriptionTimestamps",
            "SteamIdOverride",
            "LaunchOptions",
            "DlcData",
            "DenuvoGames",
        ] {
            assert!(
                matches!(map.get(&Value::String(map_key.into())), Some(Value::Mapping(m)) if m.is_empty()),
                "{} must be an empty YAML mapping {{}}",
                map_key
            );
        }
        // FakeName and FakeEmail must be empty strings (""), never null (which yaml-cpp turns into "null")
        assert_eq!(
            map.get(&Value::String("FakeName".into())),
            Some(&Value::String(String::new()))
        );
        assert_eq!(
            map.get(&Value::String("FakeEmail".into())),
            Some(&Value::String(String::new()))
        );
        assert!(raw_yaml.contains("FakeName: \"\""));
        assert!(raw_yaml.contains("FakeEmail: \"\""));
        assert!(raw_yaml.contains("LogLevels: 0xff"));
        assert!(raw_yaml.contains("SmartTickets: 0x1"));

        // Reloading an already schema-compliant file must not require healing
        assert!(!SlssteamService::mapping_needs_heal(&map));
    }

    #[test]
    fn lossless_yaml_roundtrip_all_fields_and_unquoted_u64_manifests() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.yaml");

        let mut svc = SlssteamService::new(Some(cfg_path.clone()));
        svc.config.log_levels = 0x90;
        svc.config.api = true;
        svc.config.disable_family_share_lock = true;
        svc.config.use_whitelist = true;
        svc.config.max_schema_tries = 5;
        svc.config.smart_tickets = 0x3;
        svc.config.safe_mode = true;
        svc.config.warn_hash_missmatch = true;
        svc.config.notify_init = false;
        svc.config.fake_name = "Player \"One\"".into();
        svc.config.fake_email = "user@example.com".into();
        svc.config.fake_wallet_balance = 4200;
        svc.config.plugins = true;
        svc.config.disable_cloud = false;
        svc.config.disable_updates = false;
        svc.config.dump_client_interfaces = true;
        svc.config.extended_logging = true;
        svc.config.app_ids = vec![730, 440];
        svc.config.fake_offline = vec![1245620];
        svc.config.depot_blacklist = vec![228980];
        svc.config.subscription_timestamps.insert(1245620, 1_700_000_000);
        svc.config.launch_options.insert(1245620, "GAMEMODERUN=1 %command%".into());
        svc.config.idle_status_app_id = 480;
        svc.config.idle_status_title = "Spacewar Idle".into();
        svc.set_app_steam_id_override(1245620, 76561198012345678);
        svc.set_denuvo_games(76561198012345678, vec![1245620, 1091500]);
        svc.set_cd_key(1245620, "AAAA-BBBB-CCCC-DDDD".into());
        svc.set_game_title(1245620, "ELDEN RING".into());
        svc.config.play_not_owned = true;
        svc.config.limit_downloads = true;
        svc.config.workshop_path = "/opt/DepotDownloaderMod".into();
        svc.set_fake_app_id(1245620, 480);
        svc.set_token(1245620, "tok_abc".into());
        svc.set_token(1091500, "8392019384729102938".into());
        svc.set_depot_key(1245621, "a".repeat(64));
        svc.set_manifest_override(1245621, "8888777766665555444".into());
        // Invalid non-u64 manifest strings must be rejected so SLSsteam never fails to parse ManifestIds
        svc.set_manifest_override(1245622, "pin2".into());
        svc.set_process_mapping(1245620, "eldenring.exe".into());
        svc.add_force_denuvo(1245620);
        svc.add_pinned_app(1245620);
        svc.add_additional_app(1245620);
        svc.add_additional_app(1091500);
        svc.config
            .dlc_data
            .entry(1245620)
            .or_default()
            .insert(2778580, "Shadow \"DLC\"".into());
        svc.config
            .extra_fields
            .insert("CustomUserKey".into(), Value::String("preserved_val".into()));
        assert!(svc.save());

        let raw_yaml = fs::read_to_string(&cfg_path).unwrap();
        assert!(raw_yaml.contains(&format!("  1245621: \"{}\"", "a".repeat(64))));
        // ManifestIds and numeric AppTokens must be unquoted uint64 integers
        assert!(raw_yaml.contains("ManifestIds:\n  1245621: 8888777766665555444\n"));
        assert!(!raw_yaml.contains("pin2"));
        assert!(raw_yaml.contains("AppTokens:\n  1091500: 8392019384729102938\n"));
        assert!(raw_yaml.contains("  1245620: \"eldenring.exe\""));
        assert!(raw_yaml.contains("ForceDenuvo:\n  - 1245620"));
        assert!(raw_yaml.contains("PinnedApps:\n  - 1245620"));
        assert!(!raw_yaml.contains("\n---"));

        let svc2 = SlssteamService::new(Some(cfg_path));
        assert_eq!(svc2.config.log_levels, 0x90);
        assert!(svc2.config.api);
        assert!(svc2.config.disable_family_share_lock);
        assert!(svc2.config.use_whitelist);
        assert_eq!(svc2.config.max_schema_tries, 5);
        assert_eq!(svc2.config.smart_tickets, 0x3);
        assert!(svc2.config.safe_mode);
        assert!(svc2.config.warn_hash_missmatch);
        assert!(!svc2.config.notify_init);
        assert_eq!(svc2.config.fake_name, "Player \"One\"");
        assert_eq!(svc2.config.fake_email, "user@example.com");
        assert_eq!(svc2.config.fake_wallet_balance, 4200);
        assert!(svc2.config.plugins);
        assert!(!svc2.config.disable_cloud);
        assert!(!svc2.config.disable_updates);
        assert!(svc2.config.dump_client_interfaces);
        assert!(svc2.config.extended_logging);
        assert_eq!(svc2.config.app_ids, vec![440, 730]);
        assert_eq!(svc2.config.fake_offline, vec![1245620]);
        assert_eq!(svc2.config.depot_blacklist, vec![228980]);
        assert_eq!(svc2.config.subscription_timestamps.get(&1245620), Some(&1_700_000_000));
        assert_eq!(
            svc2.config.launch_options.get(&1245620).map(String::as_str),
            Some("GAMEMODERUN=1 %command%")
        );
        assert_eq!(svc2.config.idle_status_app_id, 480);
        assert_eq!(svc2.config.idle_status_title, "Spacewar Idle");
        assert_eq!(
            svc2.config.steam_id_overrides.get(&1245620),
            Some(&76561198012345678)
        );
        assert_eq!(
            svc2.config.denuvo_games.get(&76561198012345678),
            Some(&vec![1091500, 1245620])
        );
        assert_eq!(svc2.get_cd_key(1245620), Some("AAAA-BBBB-CCCC-DDDD"));
        assert_eq!(svc2.get_game_title(1245620), Some("ELDEN RING"));
        assert!(svc2.config.play_not_owned);
        assert!(svc2.config.limit_downloads);
        assert_eq!(svc2.config.workshop_path, "/opt/DepotDownloaderMod");
        assert_eq!(svc2.get_fake_app_id(1245620), Some(480));
        assert_eq!(svc2.get_token(1245620), Some("tok_abc"));
        assert_eq!(svc2.get_token(1091500), Some("8392019384729102938"));
        assert_eq!(svc2.get_manifest_override(1245621), Some("8888777766665555444"));
        assert_eq!(svc2.get_manifest_override(1245622), None);
        assert_eq!(svc2.get_process_mapping(1245620), Some("eldenring.exe"));
        assert!(svc2.is_force_denuvo(1245620));
        assert!(svc2.is_pinned_app(1245620));
        assert_eq!(svc2.config.additional_apps, vec![1091500, 1245620]);
        assert_eq!(svc2.config.dlc_data[&1245620][&2778580], "Shadow \"DLC\"");
        assert_eq!(
            svc2.config.extra_fields.get("CustomUserKey"),
            Some(&Value::String("preserved_val".into()))
        );
    }

    #[test]
    fn heals_legacy_v301_incomplete_and_malformed_manifest_ids_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.yaml");

        // Simulate a v3.0.1 / legacy config with missing keys, yes/no booleans, scalar SteamIdOverride,
        // nested + quoted + leading-zero + garbage ManifestIds, and custom extra key
        let broken_yaml = r#"
DisableFamilyShareLock: yes
NotifyInit: no
AppIds:
ManifestIds:
  1245621: "8492018394810293847"
  1245622: "pin2"
  1245623: "0099887766554433221"
  1091500:
    1091501: "7766554433221100998"
SteamIdOverride: 76561198000000001
PlayNotOwnedGames: true
UserCustomSetting: 42
"#;
        fs::write(&cfg_path, broken_yaml).unwrap();

        // Loading automatically heals the file on disk
        let svc = SlssteamService::new(Some(cfg_path.clone()));
        assert!(svc.config.disable_family_share_lock);
        assert!(!svc.config.notify_init);
        assert_eq!(svc.get_manifest_override(1245621), Some("8492018394810293847"));
        assert_eq!(svc.get_manifest_override(1245622), None);
        assert_eq!(svc.get_manifest_override(1245623), Some("99887766554433221"));
        assert_eq!(svc.get_manifest_override(1091501), Some("7766554433221100998"));
        assert_eq!(svc.config.steam_id_overrides.get(&0), Some(&76561198000000001));
        assert_eq!(svc.config.steam_id_override, Some(76561198000000001));
        assert_eq!(
            svc.config.extra_fields.get("UserCustomSetting"),
            Some(&Value::Number(42.into()))
        );

        // Verify healed file on disk now has all 30 required keys and valid types without duplicates
        let healed_raw = fs::read_to_string(&cfg_path).unwrap();
        let Value::Mapping(healed_map) = serde_yml::from_str::<Value>(&healed_raw).unwrap() else {
            panic!("expected YAML mapping");
        };
        assert!(!SlssteamService::mapping_needs_heal(&healed_map));
        for &key in REPORTED_30_KEYS {
            assert!(healed_map.contains_key(&Value::String(key.into())));
        }
        assert!(healed_raw.contains("  1245621: 8492018394810293847\n"));
        assert!(healed_raw.contains("  1245623: 99887766554433221\n"));
        assert!(healed_raw.contains("  1091501: 7766554433221100998\n"));
        assert!(!healed_raw.contains("pin2"));
        assert!(healed_raw.contains("SteamIdOverride:\n  0: 76561198000000001\n"));
        assert_eq!(healed_raw.matches("UserCustomSetting:").count(), 1);
    }
}
