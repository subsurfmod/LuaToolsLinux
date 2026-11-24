//! SLSsteam `config.yaml` service — lossless reading & writing of FakeAppIds, AppTokens,
//! DlcData, AdditionalApps, DepotKeys, PlayNotOwnedGames, LimitDownloadsToSteamLibrary, and workshopPath.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_yml::{Mapping, Value};

use crate::models::config::{default_slssteam_config_path, SlssteamConfig};

const KNOWN_KEYS: &[&str] = &[
    "FakeAppIds",
    "AppTokens",
    "DlcData",
    "AdditionalApps",
    "DepotKeys",
    "PlayNotOwnedGames",
    "playNotOwnedGames",
    "LimitDownloadsToSteamLibrary",
    "limitDownloadsToSteamLibrary",
    "workshop",
    "ManifestOverrides",
    "ManifestPins",
    "Manifests",
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
        let config = Self::load_from(&path);
        Self {
            config_path: path,
            config,
        }
    }

    pub fn reload(&mut self) {
        self.config = Self::load_from(&self.config_path);
    }

    fn value_to_u32(v: &Value) -> Option<u32> {
        match v {
            Value::Number(n) => n.as_u64().and_then(|x| u32::try_from(x).ok()),
            Value::String(s) => s.trim().parse::<u32>().ok(),
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

    fn load_from(path: &Path) -> SlssteamConfig {
        let mut cfg = SlssteamConfig {
            path: path.to_path_buf(),
            ..Default::default()
        };
        let Ok(raw) = fs::read_to_string(path) else {
            return cfg;
        };
        let Ok(Value::Mapping(map)) = serde_yml::from_str::<Value>(&raw) else {
            return cfg;
        };

        for (k, v) in &map {
            let Some(key_str) = k.as_str() else {
                continue;
            };
            if !KNOWN_KEYS.contains(&key_str) {
                cfg.extra_fields.insert(key_str.to_string(), v.clone());
                continue;
            }
            match key_str {
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
                "AppTokens" => {
                    if let Value::Mapping(sub) = v {
                        for (ak, tv) in sub {
                            if let (Some(aid), Some(tok)) =
                                (Self::value_to_u32(ak), Self::value_to_string(tv))
                            {
                                cfg.app_tokens.insert(aid, tok);
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
                "AdditionalApps" => match v {
                    Value::Sequence(seq) => {
                        for item in seq {
                            if let Some(aid) = Self::value_to_u32(item) {
                                if !cfg.additional_apps.contains(&aid) {
                                    cfg.additional_apps.push(aid);
                                }
                            }
                        }
                    }
                    Value::String(s) => {
                        for tok in s.split_whitespace() {
                            if let Ok(aid) = tok.parse::<u32>() {
                                if !cfg.additional_apps.contains(&aid) {
                                    cfg.additional_apps.push(aid);
                                }
                            }
                        }
                    }
                    Value::Number(_) => {
                        if let Some(aid) = Self::value_to_u32(v) {
                            if !cfg.additional_apps.contains(&aid) {
                                cfg.additional_apps.push(aid);
                            }
                        }
                    }
                    _ => {}
                },
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
                "ManifestOverrides" | "ManifestPins" | "Manifests" => {
                    if let Value::Mapping(sub) = v {
                        for (dk, mv) in sub {
                            if let (Some(did), Some(gid)) =
                                (Self::value_to_u32(dk), Self::value_to_string(mv))
                            {
                                cfg.manifest_overrides.insert(did, gid);
                            }
                        }
                    }
                }
                "PlayNotOwnedGames" | "playNotOwnedGames" => {
                    if let Some(b) = v.as_bool() {
                        cfg.play_not_owned = b;
                    }
                }
                "LimitDownloadsToSteamLibrary" | "limitDownloadsToSteamLibrary" => {
                    if let Some(b) = v.as_bool() {
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
        cfg
    }

    pub fn serialize_yaml(&self) -> String {
        let cfg = &self.config;
        let mut lines: Vec<String> = Vec::new();

        if !cfg.fake_app_ids.is_empty() {
            lines.push("FakeAppIds:".into());
            for (aid, did) in &cfg.fake_app_ids {
                lines.push(format!("  {}: {}", aid, did));
            }
            lines.push(String::new());
        }

        if !cfg.app_tokens.is_empty() {
            lines.push("AppTokens:".into());
            for (aid, tok) in &cfg.app_tokens {
                let escaped = tok.replace('\\', "\\\\").replace('"', "\\\"");
                lines.push(format!("  {}: \"{}\"", aid, escaped));
            }
            lines.push(String::new());
        }

        if !cfg.dlc_data.is_empty() {
            lines.push("DlcData:".into());
            for (aid, dlcs) in &cfg.dlc_data {
                if dlcs.is_empty() {
                    continue;
                }
                lines.push(format!("  {}:", aid));
                for (did, name) in dlcs {
                    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
                    lines.push(format!("    {}: \"{}\"", did, escaped));
                }
            }
            lines.push(String::new());
        }

        if !cfg.additional_apps.is_empty() {
            let mut sorted = cfg.additional_apps.clone();
            sorted.sort_unstable();
            sorted.dedup();
            lines.push("AdditionalApps:".into());
            for aid in sorted {
                lines.push(format!("  - {}", aid));
            }
            lines.push(String::new());
        }

        if !cfg.depot_keys.is_empty() {
            lines.push("DepotKeys:".into());
            for (did, key) in &cfg.depot_keys {
                let clean_key = key.trim().trim_matches('"');
                lines.push(format!("  {}: \"{}\"", did, clean_key));
            }
            lines.push(String::new());
        }

        if !cfg.manifest_overrides.is_empty() {
            lines.push("ManifestOverrides:".into());
            for (did, gid) in &cfg.manifest_overrides {
                let clean_gid = gid.trim().trim_matches('"');
                lines.push(format!("  {}: \"{}\"", did, clean_gid));
            }
            lines.push(String::new());
        }

        lines.push(format!(
            "PlayNotOwnedGames: {}",
            if cfg.play_not_owned { "true" } else { "false" }
        ));
        lines.push(format!(
            "LimitDownloadsToSteamLibrary: {}",
            if cfg.limit_downloads { "true" } else { "false" }
        ));

        if !cfg.workshop_path.trim().is_empty() {
            let escaped = cfg
                .workshop_path
                .trim()
                .replace('\\', "\\\\")
                .replace('"', "\\\"");
            lines.push("workshop:".into());
            lines.push(format!("  workshopPath: \"{}\"", escaped));
        }

        if !cfg.extra_fields.is_empty() {
            let mut extra_map = Mapping::new();
            for (k, v) in &cfg.extra_fields {
                extra_map.insert(Value::String(k.clone()), v.clone());
            }
            if let Ok(extra_str) = serde_yml::to_string(&Value::Mapping(extra_map)) {
                let trimmed = extra_str.trim();
                if !trimmed.is_empty() {
                    lines.push(String::new());
                    lines.push(trimmed.to_string());
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
        let tmp = self.config_path.with_extension("yaml.tmp");
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
        self.config.app_tokens.insert(app_id, token);
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
        self.config.manifest_overrides.get(&depot_id).map(String::as_str)
    }

    pub fn set_manifest_override(&mut self, depot_id: u32, manifest_id: String) {
        self.config.manifest_overrides.insert(depot_id, manifest_id);
    }

    pub fn remove_manifest_override(&mut self, depot_id: u32) -> bool {
        self.config.manifest_overrides.remove(&depot_id).is_some()
    }

    /// Synchronizes manifest pins into SLSsteam's `ManifestOverrides` so they take effect under SLSsteam.so.
    pub fn sync_manifest_pins(&mut self, pins: &[(u32, String)]) {
        for (depot_id, gid) in pins {
            self.set_manifest_override(*depot_id, gid.clone());
        }
    }

    /// Synchronizes from manifest_pins.json file
    pub fn sync_from_manifest_pins_file(&mut self, pins_path: Option<&Path>) -> bool {
        let default_path = crate::models::config::default_luatools_data_dir().join("manifest_pins.json");
        let path = pins_path.unwrap_or(&default_path);
        if let Ok(raw) = fs::read_to_string(path) {
            if let Ok(list) = serde_json::from_str::<Vec<serde_json::Value>>(&raw) {
                let mut changed = false;
                for item in list {
                    if let (Some(did), Some(gid)) = (
                        item.get("depot_id").and_then(|v| v.as_u64()).map(|v| v as u32),
                        item.get("manifest_id").and_then(|v| v.as_str()),
                    ) {
                        if !gid.is_empty() {
                            self.config.manifest_overrides.insert(did, gid.to_string());
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossless_yaml_roundtrip_including_additional_apps() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.yaml");

        let mut svc = SlssteamService::new(Some(cfg_path.clone()));
        svc.config.play_not_owned = true;
        svc.config.limit_downloads = true;
        svc.config.workshop_path = "/opt/DepotDownloaderMod".into();
        svc.set_fake_app_id(1245620, 480);
        svc.set_token(1245620, "tok_abc".into());
        svc.set_depot_key(1245621, "a".repeat(64));
        svc.set_manifest_override(1245621, "8888777766665555444".into());
        svc.add_additional_app(1245620);
        svc.add_additional_app(1091500);
        svc.config
            .dlc_data
            .entry(1245620)
            .or_default()
            .insert(2778580, "Shadow \"DLC\"".into());
        assert!(svc.save());

        // Check that depot keys and manifest overrides are properly quoted in output YAML
        let raw_yaml = fs::read_to_string(&cfg_path).unwrap();
        assert!(raw_yaml.contains(&format!("  1245621: \"{}\"", "a".repeat(64))));
        assert!(raw_yaml.contains("  1245621: \"8888777766665555444\""));

        let svc2 = SlssteamService::new(Some(cfg_path));
        assert!(svc2.config.play_not_owned);
        assert!(svc2.config.limit_downloads);
        assert_eq!(svc2.config.workshop_path, "/opt/DepotDownloaderMod");
        assert_eq!(svc2.get_fake_app_id(1245620), Some(480));
        assert_eq!(svc2.get_token(1245620), Some("tok_abc"));
        assert_eq!(svc2.get_manifest_override(1245621), Some("8888777766665555444"));
        assert_eq!(svc2.config.additional_apps, vec![1091500, 1245620]);
        assert_eq!(svc2.config.dlc_data[&1245620][&2778580], "Shadow \"DLC\"");
    }
}
