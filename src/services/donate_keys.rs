//! DonateKeysService — scans Steam `config/config.vdf` for 64-char hex depot decryption keys.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use regex::Regex;
use serde_json::json;

use crate::models::config::default_luatools_cache_dir;

const DONATE_ENDPOINT: &str = "http://167.235.229.108/donatekeys/send";
const DONATE_USER_AGENT: &str = "discord(dot)gg/luatools";

#[derive(Debug, Clone)]
pub struct DonateKeysService {
    pub steam_root: PathBuf,
    pub cache_path: PathBuf,
}

impl DonateKeysService {
    pub fn new(steam_root: PathBuf, cache_path: Option<PathBuf>) -> Self {
        let cp =
            cache_path.unwrap_or_else(|| default_luatools_cache_dir().join("donated_keys.json"));
        if let Some(p) = cp.parent() {
            let _ = fs::create_dir_all(p);
        }
        Self {
            steam_root,
            cache_path: cp,
        }
    }

    pub fn extract_keys_from_vdf(vdf_path: &Path) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let Ok(raw) = fs::read_to_string(vdf_path) else {
            return out;
        };
        let block_re =
            Regex::new(r#"(?is)"(\d+)"\s*\{\s*[^}]*?"DecryptionKey"\s+"([0-9a-fA-F]{64})""#)
                .unwrap();
        for cap in block_re.captures_iter(&raw) {
            if let (Some(did), Some(key)) = (cap.get(1), cap.get(2)) {
                out.insert(did.as_str().to_string(), key.as_str().to_ascii_lowercase());
            }
        }
        out
    }

    pub fn donate_new_keys(&self) -> usize {
        let vdf = self.steam_root.join("config").join("config.vdf");
        let found = Self::extract_keys_from_vdf(&vdf);
        if found.is_empty() {
            return 0;
        }

        let mut sent: BTreeMap<String, String> = fs::read_to_string(&self.cache_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        let mut new_keys = BTreeMap::new();
        for (d, k) in found {
            if sent.get(&d) != Some(&k) {
                new_keys.insert(d, k);
            }
        }
        if new_keys.is_empty() {
            return 0;
        }

        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(15))
            .build();
        if let Ok(resp) = agent
            .post(DONATE_ENDPOINT)
            .set("User-Agent", DONATE_USER_AGENT)
            .send_json(json!({ "keys": new_keys }))
        {
            if matches!(resp.status(), 200 | 201 | 204) {
                let count = new_keys.len();
                sent.extend(new_keys);
                if let Ok(js) = serde_json::to_string(&sent) {
                    let _ = fs::write(&self.cache_path, js);
                }
                return count;
            }
        }
        0
    }
}
