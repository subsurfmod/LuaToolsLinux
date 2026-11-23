//! DohResolver - DNS-over-HTTPS resolver for Steam domain resolution.
//!
//! Resolves Steam domain names via DoH (DNS-over-HTTPS).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DohRecord {
    pub name: String,
    pub rtype: String,
    pub ttl: u32,
    pub value: String,
}

#[derive(Debug, Clone, Default)]
pub struct DohResolver {
    cache: Arc<Mutex<HashMap<String, Vec<DohRecord>>>>,
    doh_endpoint: String,
}

impl DohResolver {
    pub fn new(doh_endpoint: Option<&str>) -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
            doh_endpoint: doh_endpoint
                .unwrap_or("https://dns.google/resolve")
                .to_string(),
        }
    }

    pub fn resolve(&self, domain: &str) -> Option<Vec<DohRecord>> {
        // Check cache first
        if let Some(cached) = self.cache.lock().unwrap().get(domain) {
            return Some(cached.clone());
        }

        // In a full implementation, this would make an HTTPS request to the DoH endpoint
        // For now, return empty - DNS resolution is handled by the OS
        None
    }

    pub fn set_endpoint(&mut self, endpoint: &str) {
        self.doh_endpoint = endpoint.to_string();
    }

    pub fn endpoint(&self) -> &str {
        &self.doh_endpoint
    }

    pub fn clear_cache(&mut self) {
        self.cache.lock().unwrap().clear();
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
        let resolver = DohResolver::new(None);
        assert!(resolver.resolve("nonexistent.example.com").is_none());
    }
}