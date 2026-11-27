//! DonateKeysService — scans Steam `config/config.vdf` for 64-char hex depot keys.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::models::config::default_luatools_cache_dir;

const DONATE_ENDPOINT: &str = "https://167.235.229.108/donatekeys/send";
const DONATE_USER_AGENT: &str = "discord(dot)gg/luatools";

#[derive(Debug, Clone, PartialEq, Eq)]
enum VdfToken {
    Str(String),
    LBrace,
    RBrace,
}

fn tokenize_vdf(raw: &str) -> Vec<VdfToken> {
    let mut tokens = Vec::new();
    let bytes = raw.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        let b = bytes[i];
        // Skip ASCII whitespace
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // Skip // line comments outside quotes
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            i += 2;
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b == b'{' {
            tokens.push(VdfToken::LBrace);
            i += 1;
            continue;
        }
        if b == b'}' {
            tokens.push(VdfToken::RBrace);
            i += 1;
            continue;
        }
        if b == b'"' {
            i += 1;
            let mut s = String::new();
            while i < len {
                let c = bytes[i];
                if c == b'\\' {
                    i += 1;
                    if i < len {
                        let next_ch = match raw[i..].chars().next() {
                            Some(ch) => ch,
                            None => break,
                        };
                        match next_ch {
                            '"' => s.push('"'),
                            '\\' => s.push('\\'),
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            other => {
                                s.push('\\');
                                s.push(other);
                            }
                        }
                        i += next_ch.len_utf8();
                    } else {
                        s.push('\\');
                    }
                    continue;
                }
                if c == b'"' {
                    i += 1;
                    break;
                }
                // Preserve valid UTF-8 characters inside quoted strings
                let ch = match raw[i..].chars().next() {
                    Some(ch) => ch,
                    None => break,
                };
                s.push(ch);
                i += ch.len_utf8();
            }
            tokens.push(VdfToken::Str(s));
            continue;
        }
        // Bare unquoted token
        let start = i;
        while i < len
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'{'
            && bytes[i] != b'}'
            && bytes[i] != b'"'
        {
            if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
                break;
            }
            i += 1;
        }
        if i > start {
            tokens.push(VdfToken::Str(raw[start..i].to_string()));
        } else {
            i += 1;
        }
    }

    tokens
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

    /// Brace-depth-aware VDF `"depots"` scanner (LT-45).
    /// Accurately extracts `"DecryptionKey"` even when nested `{ ... }` sub-blocks appear
    /// before `"DecryptionKey"` inside a depot section.
    pub fn extract_keys_from_vdf_text(raw: &str) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let tokens = tokenize_vdf(raw);
        let mut stack: Vec<String> = Vec::new();
        let mut i = 0;

        while i < tokens.len() {
            match &tokens[i] {
                VdfToken::RBrace => {
                    stack.pop();
                    i += 1;
                }
                VdfToken::LBrace => {
                    // Orphaned opening brace; push anonymous sentinel to keep depth balanced
                    stack.push(String::new());
                    i += 1;
                }
                VdfToken::Str(key) => {
                    if i + 1 < tokens.len() {
                        match &tokens[i + 1] {
                            VdfToken::LBrace => {
                                stack.push(key.clone());
                                i += 2;
                            }
                            VdfToken::Str(val) => {
                                if key.eq_ignore_ascii_case("DecryptionKey")
                                    && val.len() == 64
                                    && val.bytes().all(|b| b.is_ascii_hexdigit())
                                {
                                    // Accept when directly inside "depots" -> "<depot_id>",
                                    // or inside a standalone top-level "<depot_id>" snippet.
                                    let valid_depot_context = match stack.len() {
                                        1 => true,
                                        n if n >= 2 => stack[n - 2].eq_ignore_ascii_case("depots"),
                                        _ => false,
                                    };
                                    if valid_depot_context {
                                        if let Some(depot_str) = stack.last() {
                                            if let Ok(depot_id) = depot_str.parse::<u32>() {
                                                let lowered = val.to_ascii_lowercase();
                                                if depot_id > 0 && !lowered.bytes().all(|b| b == b'0') {
                                                    out.insert(depot_id.to_string(), lowered);
                                                }
                                            }
                                        }
                                    }
                                }
                                i += 2;
                            }
                            VdfToken::RBrace => {
                                // Malformed dangling key before closing brace; skip key only
                                i += 1;
                            }
                        }
                    } else {
                        i += 1;
                    }
                }
            }
        }

        out
    }

    pub fn extract_keys_from_vdf(vdf_path: &Path) -> BTreeMap<String, String> {
        let Ok(raw) = fs::read_to_string(vdf_path) else {
            return BTreeMap::new();
        };
        Self::extract_keys_from_vdf_text(&raw)
    }

    pub fn donate_new_keys(&self) -> usize {
        self.donate_new_keys_to(DONATE_ENDPOINT)
    }

    fn donate_new_keys_to(&self, endpoint: &str) -> usize {
        // Enforce HTTPS or loopback to prevent plaintext leakage of sensitive depot keys
        if !endpoint.starts_with("https://") {
            let is_loopback = endpoint.starts_with("http://127.0.0.1")
                || endpoint.starts_with("http://localhost");
            if !is_loopback {
                return 0;
            }
        }
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

        let body = new_keys
            .iter()
            .map(|(d, k)| format!("{}:{}", d, k))
            .collect::<Vec<_>>()
            .join(",");

        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build();
        if let Ok(resp) = agent
            .post(endpoint)
            .set("User-Agent", DONATE_USER_AGENT)
            .set("Content-Type", "text/plain")
            .send_string(&body)
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
        assert_eq!(body, format!("1245621:{}", key));
        assert!(cache.is_file());

        // Second call finds no new keys and does not hit network
        assert_eq!(svc.donate_new_keys_to("http://127.0.0.1:1/unreachable"), 0);
    }

    #[test]
    fn extracts_keys_across_nested_braces_in_config_vdf() {
        let dir = tempdir().unwrap();
        let vdf = dir.path().join("config.vdf");
        let key1 = "1a".repeat(32);
        let key2 = "2b".repeat(32);
        let key_outside = "3c".repeat(32);
        let content = format!(
            r#""InstallConfigStore"
{{
    "Software"
    {{
        "Valve"
        {{
            "Steam"
            {{
                "apps"
                {{
                    "999999"
                    {{
                        "DecryptionKey" "{key_outside}"
                    }}
                }}
                "depots"
                {{
                    "1245621"
                    {{
                        // Nested config and mounts before DecryptionKey used to break [^}}]*? regex
                        "config"
                        {{
                            "oslist" "windows"
                            "note" "escaped quote \" and brace }} inside string"
                            "nested"
                            {{
                                "flag" "1"
                            }}
                        }}
                        "DecryptionKey" "{key1}"
                    }}
                    "1245622"
                    {{
                        "manifest" {{ "gid" "12345" }}
                        "DecryptionKey" "{key2}"
                    }}
                }}
            }}
        }}
    }}
}}"#
        );
        fs::write(&vdf, content).unwrap();

        let keys = DonateKeysService::extract_keys_from_vdf(&vdf);
        assert_eq!(keys.len(), 2);
        assert_eq!(keys.get("1245621").map(String::as_str), Some(key1.as_str()));
        assert_eq!(keys.get("1245622").map(String::as_str), Some(key2.as_str()));
        assert!(!keys.contains_key("999999"));
    }

    #[test]
    fn test_tokenize_vdf_escapes_with_multibyte_utf8() {
        let input = r#""test" "hello \u00e4 \世界 \"escaped\" \\ \n normal""#;
        let tokens = tokenize_vdf(input);
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0], VdfToken::Str("test".into()));
        assert_eq!(
            tokens[1],
            VdfToken::Str("hello \\u00e4 \\世界 \"escaped\" \\ \n normal".into())
        );
    }
}
