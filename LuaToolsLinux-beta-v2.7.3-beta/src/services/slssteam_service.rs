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
                lines.push(format!("  {}: {}", did, key));
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
        svc.add_additional_app(1245620);
        svc.add_additional_app(1091500);
        svc.config
            .dlc_data
            .entry(1245620)
            .or_default()
            .insert(2778580, "Shadow \"DLC\"".into());
        assert!(svc.save());

        let svc2 = SlssteamService::new(Some(cfg_path));
        assert!(svc2.config.play_not_owned);
        assert!(svc2.config.limit_downloads);
        assert_eq!(svc2.config.workshop_path, "/opt/DepotDownloaderMod");
        assert_eq!(svc2.get_fake_app_id(1245620), Some(480));
        assert_eq!(svc2.get_token(1245620), Some("tok_abc"));
        assert_eq!(svc2.config.additional_apps, vec![1091500, 1245620]);
        assert_eq!(svc2.config.dlc_data[&1245620][&2778580], "Shadow \"DLC\"");
    }
}
