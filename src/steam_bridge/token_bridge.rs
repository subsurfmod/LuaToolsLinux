//! TokenBridge / `bst://` URI Scheme handler for Linux.
//!
//! Handles `bst://` URI scheme for token-based authentication and configuration.
//! Provides URI parsing, scheme registration, and integration with the Steam client.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

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

/// BST URI structure parsed from a `bst://` URI.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Parse a `bst://` URI string (case-insensitive scheme, strips `#fragment`, percent-decodes values).
    ///
    /// Supports both query strings (`bst://<appid>/<token>?k1=v1&k2=v2`) and
    /// slash-delimited `key=value` segments (`bst://<appid>/<token>/k1=v1`).
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

        let mut app_id = parts
            .first()
            .and_then(|p| p.parse::<u32>().ok())
            .unwrap_or(0);
        let mut token = String::new();
        let mut params = HashMap::new();

        if app_id == 0 && parts.len() > 1 {
            // Support action-first URI grammar such as `bst://refreshachievements/480`
            if let Ok(parsed_id) = parts[1].parse::<u32>() {
                token = percent_decode(parts[0]);
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
    handlers: Arc<RwLock<Vec<BstUriHandler>>>,
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
        };
        bridge.register_default_handlers();
        bridge
    }

    /// Register default handlers for common bst:// actions.
    fn register_default_handlers(&mut self) {
        // bst://refreshachievements/<appid> - Trigger Steam achievement refresh
        self.register_handler(Box::new(|uri: &BstUri| {
            if uri.app_id > 0 && (uri.token.eq_ignore_ascii_case("refreshachievements")
                || uri.params.get("action").map(|s| s.as_str()) == Some("refreshachievements")) {
                Some(uri.clone())
            } else {
                None
            }
        }));

        // bst://fix/<appid> - Open fix for app
        self.register_handler(Box::new(|uri: &BstUri| {
            if uri.app_id > 0 && (uri.token.eq_ignore_ascii_case("fix")
                || uri.params.get("action").map(|s| s.as_str()) == Some("fix")) {
                Some(uri.clone())
            } else {
                None
            }
        }));

        // bst://install/<appid> - Install app via LuaTools
        self.register_handler(Box::new(|uri: &BstUri| {
            if uri.app_id > 0 && (uri.token.eq_ignore_ascii_case("install")
                || uri.params.get("action").map(|s| s.as_str()) == Some("install")) {
                Some(uri.clone())
            } else {
                None
            }
        }));

        // Generic fallback handler
        self.register_handler(Box::new(|uri: &BstUri| {
            if uri.is_valid() || !uri.token.is_empty() || uri.app_id > 0 {
                Some(uri.clone())
            } else {
                None
            }
        }));
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

    #[test]
    fn test_bst_uri_parse() {
        let uri = BstUri::parse("bst://12345/abc123token").unwrap();
        assert_eq!(uri.app_id, 12345);
        assert_eq!(uri.token, "abc123token");
        assert_eq!(uri.version, 1);
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
    }

    #[test]
    fn test_bst_uri_valid() {
        let uri = BstUri::parse("bst://12345/token").unwrap();
        assert!(uri.is_valid());
    }

    #[test]
    fn test_token_bridge_dispatch_fallback_and_clear() {
        let mut bridge = TokenBridge::new();
        // TokenBridge::new() now registers 4 default handlers
        let initial_count = bridge.handler_count();
        assert!(initial_count >= 4);

        bridge.register_handler(Box::new(|_| None));
        bridge.register_handler(Box::new(|u| Some(u.clone())));
        assert_eq!(bridge.handler_count(), initial_count + 2);

        let out = bridge.handle_uri("bst://730/sessiontok").unwrap();
        assert_eq!(out.app_id, 730);
        assert_eq!(out.token, "sessiontok");

        bridge.clear_handlers();
        assert_eq!(bridge.handler_count(), 0);
        assert!(bridge.handle_uri("bst://730/sessiontok").is_none());
    }
}
