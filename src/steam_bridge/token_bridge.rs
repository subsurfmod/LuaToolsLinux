//! TokenBridge / `bst://` URI Scheme handler for Linux.
//!
//! Handles `bst://` URI scheme for token-based authentication and configuration.
//! Provides URI parsing, scheme registration, and integration with the Steam client.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// BST URI structure parsed from a `bst://` URI.
#[derive(Debug, Clone)]
pub struct BstUri {
    /// The app ID associated with this URI.
    pub app_id: u32,
    /// The token or session ID.
    pub token: String,
    /// The URI scheme version.
    pub version: u32,
    /// Optional query or path parameters.
    pub params: HashMap<String, String>,
}

impl BstUri {
    /// Parse a `bst://` URI string.
    ///
    /// Supports both query strings (`bst://<appid>/<token>?k1=v1&k2=v2`) and
    /// slash-delimited `key=value` segments (`bst://<appid>/<token>/k1=v1`).
    pub fn parse(uri: &str) -> Option<Self> {
        let rest = uri.strip_prefix("bst://")?;
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

        let app_id = parts
            .first()
            .and_then(|p| p.parse::<u32>().ok())
            .unwrap_or(0);
        let mut token = String::new();
        let mut params = HashMap::new();

        if parts.len() > 1 {
            token = parts[1].to_string();
            for part in parts.iter().skip(2) {
                if let Some((k, v)) = part.split_once('=') {
                    params.insert(k.to_string(), v.to_string());
                }
            }
        }

        if let Some(query) = query_part {
            for pair in query.split('&') {
                if let Some((k, v)) = pair.split_once('=') {
                    if !k.is_empty() {
                        params.insert(k.to_string(), v.to_string());
                    }
                }
            }
        }

        Some(BstUri {
            app_id,
            token,
            version: 1,
            params,
        })
    }

    /// Check if this URI has a valid non-zero app ID and non-empty token.
    pub fn is_valid(&self) -> bool {
        self.app_id > 0 && !self.token.is_empty()
    }
}

/// Type alias for `bst://` URI handlers.
pub type BstUriHandler = Box<dyn Fn(&BstUri) -> Option<BstUri> + Send + Sync>;

/// TokenBridge service that handles `bst://` URI requests.
pub struct TokenBridge {
    /// Registered URI handlers.
    handlers: Arc<Mutex<Vec<BstUriHandler>>>,
}

impl Default for TokenBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenBridge {
    /// Create a new TokenBridge instance.
    pub fn new() -> Self {
        TokenBridge {
            handlers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Register a handler for `bst://` URIs.
    pub fn register_handler(&mut self, handler: BstUriHandler) {
        self.handlers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(handler);
    }

    /// Handle a `bst://` URI request by dispatching through registered handlers.
    pub fn handle_uri(&self, uri: &str) -> Option<BstUri> {
        let parsed = BstUri::parse(uri)?;
        let handlers = self.handlers.lock().unwrap_or_else(|e| e.into_inner());
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

    #[test]
    fn test_bst_uri_parse() {
        let uri = BstUri::parse("bst://12345/abc123token").unwrap();
        assert_eq!(uri.app_id, 12345);
        assert_eq!(uri.token, "abc123token");
        assert!(uri.is_valid());
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
    fn test_bst_uri_invalid() {
        assert!(BstUri::parse("http://example.com").is_none());
    }

    #[test]
    fn test_bst_uri_valid() {
        let uri = BstUri::parse("bst://12345/token").unwrap();
        assert!(uri.is_valid());
    }

    #[test]
    fn test_token_bridge_dispatch() {
        let mut bridge = TokenBridge::new();
        bridge.register_handler(Box::new(|u| Some(u.clone())));
        let out = bridge.handle_uri("bst://730/sessiontok").unwrap();
        assert_eq!(out.app_id, 730);
        assert_eq!(out.token, "sessiontok");
    }
}
