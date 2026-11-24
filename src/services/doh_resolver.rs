//! DohResolver - DNS-over-HTTPS resolver for Steam domain resolution.
//!
//! Resolves Steam domain names via DoH (DNS-over-HTTPS) JSON API
//! (Google Public DNS & Cloudflare compatible) with TTL-aware caching.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DohRecord {
    pub name: String,
    pub rtype: String,
    pub ttl: u32,
    pub value: String,
}

#[derive(Debug, Clone)]
struct CachedRecords {
    records: Vec<DohRecord>,
    expires_at: Instant,
}

#[derive(Debug, Clone)]
pub struct DohResolver {
    cache: Arc<Mutex<HashMap<String, CachedRecords>>>,
    doh_endpoint: String,
}

impl Default for DohResolver {
    fn default() -> Self {
        Self::new(None)
    }
}

impl DohResolver {
    pub fn new(doh_endpoint: Option<&str>) -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
            doh_endpoint: doh_endpoint
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("https://dns.google/resolve")
                .to_string(),
        }
    }

    pub fn resolve(&self, domain: &str) -> Option<Vec<DohRecord>> {
        let normalized = domain.trim().trim_end_matches('.').to_ascii_lowercase();
        if !is_valid_domain(&normalized) {
            return None;
        }

        // Check TTL-aware cache first
        {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = cache.get(&normalized) {
                if Instant::now() < entry.expires_at {
                    return Some(entry.records.clone());
                }
            }
            cache.remove(&normalized);
        }

        if !self.doh_endpoint.starts_with("https://") && !self.doh_endpoint.starts_with("http://") {
            return None;
        }

        let sep = if self.doh_endpoint.contains('?') {
            '&'
        } else {
            '?'
        };
        let url = format!(
            "{}{}name={}&type=A",
            self.doh_endpoint,
            sep,
            urlencoding::encode(&normalized)
        );

        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(2))
            .timeout(Duration::from_secs(4))
            .build();
        let resp = agent
            .get(&url)
            .set("Accept", "application/dns-json")
            .call()
            .ok()?;
        let val = resp.into_json::<Value>().ok()?;
        let records = parse_doh_json(&val)?;

        let min_ttl = records
            .iter()
            .map(|r| r.ttl)
            .min()
            .unwrap_or(60)
            .clamp(5, 86_400);
        let expires_at = Instant::now() + Duration::from_secs(u64::from(min_ttl));
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).insert(
            normalized,
            CachedRecords {
                records: records.clone(),
                expires_at,
            },
        );

        Some(records)
    }

    pub fn set_endpoint(&mut self, endpoint: &str) {
        self.doh_endpoint = endpoint.trim().to_string();
        self.clear_cache();
    }

    pub fn endpoint(&self) -> &str {
        &self.doh_endpoint
    }

    pub fn clear_cache(&mut self) {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

fn is_valid_domain(domain: &str) -> bool {
    if domain.is_empty() || domain.len() > 253 {
        return false;
    }
    domain.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

fn dns_type_name(code: u64) -> String {
    match code {
        1 => "A".into(),
        5 => "CNAME".into(),
        16 => "TXT".into(),
        28 => "AAAA".into(),
        other => other.to_string(),
    }
}

fn parse_doh_json(val: &Value) -> Option<Vec<DohRecord>> {
    // DNS RCODE 0 == NOERROR
    if let Some(status) = val.get("Status").and_then(|v| v.as_u64()) {
        if status != 0 {
            return None;
        }
    }
    let answers = val.get("Answer")?.as_array()?;
    let mut out = Vec::new();
    for ans in answers {
        let name = ans
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim_end_matches('.')
            .to_string();
        let value = ans
            .get("data")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim()
            .to_string();
        if value.is_empty() {
            continue;
        }
        let rtype = ans
            .get("type")
            .map(|v| {
                v.as_u64()
                    .map(dns_type_name)
                    .or_else(|| v.as_str().map(ToString::to_string))
                    .unwrap_or_else(|| "A".into())
            })
            .unwrap_or_else(|| "A".into());
        let ttl = ans
            .get("TTL")
            .and_then(|v| v.as_u64())
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(60);
        out.push(DohRecord {
            name,
            rtype,
            ttl,
            value,
        });
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolver_creation() {
        let resolver = DohResolver::new(None);
        assert_eq!(resolver.endpoint(), "https://dns.google/resolve");
    }

    #[test]
    fn test_custom_endpoint() {
        let resolver = DohResolver::new(Some("https://cloudflare-dns.com/dns-query"));
        assert_eq!(resolver.endpoint(), "https://cloudflare-dns.com/dns-query");
    }

    #[test]
    fn test_resolve_unknown() {
        // Closed loopback endpoint ensures offline determinism and instant failure.
        let resolver = DohResolver::new(Some("http://127.0.0.1:1/resolve"));
        assert!(resolver.resolve("nonexistent.example.com").is_none());
        assert!(resolver.resolve("bad domain!").is_none());
        assert!(resolver.resolve("").is_none());
    }

    #[test]
    fn test_resolve_from_mock_doh_server_and_uses_cache() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let handle = std::thread::spawn(move || {
            let req = server.recv().unwrap();
            let url = req.url().to_string();
            let body = r#"{"Status":0,"Answer":[{"name":"store.steampowered.com.","type":1,"TTL":300,"data":"23.45.67.89"}]}"#;
            let _ = req.respond(tiny_http::Response::from_string(body));
            url
        });

        let resolver = DohResolver::new(Some(&format!("http://{}/resolve", addr)));
        let records = resolver.resolve("Store.SteamPowered.com.").unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].rtype, "A");
        assert_eq!(records[0].value, "23.45.67.89");
        assert_eq!(records[0].ttl, 300);

        let url = handle.join().unwrap();
        assert!(url.contains("name=store.steampowered.com"));
        assert!(url.contains("type=A"));

        // Second resolve hits cache without needing the server thread to respond again
        let cached = resolver.resolve("store.steampowered.com").unwrap();
        assert_eq!(cached, records);
    }

    #[test]
    fn test_parse_doh_json_rejects_nxdomain() {
        let nx: Value = serde_json::json!({"Status": 3, "Answer": []});
        assert!(parse_doh_json(&nx).is_none());
    }
}
