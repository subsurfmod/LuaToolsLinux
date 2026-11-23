//! TokenBridge / bst:// URI Scheme handler for Linux.
//!
//! Handles bst:// URI scheme for token-based authentication and configuration.
//! Provides URI parsing, scheme registration, and integration with the Steam client.

use std::net::SocketAddr;
use std::sync::Arc;

/// BST URI structure parsed from bst:// URI
#[derive(Debug, Clone)]
pub struct BstUri {
    /// The app ID associated with this URI
    pub app_id: u32,
    /// The token or session ID
    pub token: String,
    /// The URI scheme version
    pub version: u32,
    /// Optional parameters
    pub params: HashMap<String, String>,
}

impl BstUri {
    /// Parse a bst:// URI string
    pub fn parse(uri: &str) -> Option<Self> {
        if !uri.starts_with("bst://") {
            return None;
        }

        let uri = &uri[6..]; // Strip "bst://" prefix
        let parts: Vec<&str> = uri.split('/').collect();

        if parts.is_empty() {
            return None;
        }

        // First part is app_id
        let app_id = parts.first()
            .and_then(|p| p.parse::<u32>().ok())
            .unwrap_or(0);

        // Remaining parts contain token and optional parameters
        let mut token = String::new();
        let mut params = HashMap::new();

        if parts.len() > 1 {
            // Second part is the token
            token = parts[1].to_string();

            // Additional parts are key=value parameters
            for part in parts.iter().skip(2) {
                if let Some(idx) = part.find('=') {
                    let key = part[..idx].to_string();
                    let value = part[idx + 1..].to_string();
                    params.insert(key, value);
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

    /// Check if this URI is valid
    pub fn is_valid(&self) -> bool {
        self.app_id > 0 && !self.token.is_empty()
    }
}

/// TokenBridge service that handles bst:// URI requests
pub struct TokenBridge {
    /// Registered URI handlers
    handlers: Arc<Mutex<Vec<BstUriHandler>>>,
}

impl TokenBridge {
    /// Create a new TokenBridge instance
    pub fn new() -> Self {
        TokenBridge {
            handlers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Register a handler for bst:// URIs
    pub fn register_handler(&mut self, handler: BstUriHandler) {
        self.handlers.lock().unwrap().push(handler);
    }

    /// Handle a bst:// URI request
    pub fn handle_uri(&self, uri: &str) -> Option<BstUri> {
        let parsed = BstUri::parse(uri)?;

        let handlers = self.handlers.lock().unwrap();
        for handler in handlers.iter() {
            if handler.matches(&parsed) {
                return handler.handle(&parsed);
            }
        }

        None
    }
}

/// Type alias for bst URI handlers
type BstUriHandler = Box<dyn Fn(&BstUri) -> Option<BstUri> + Send + Sync>;

/// Default handler that returns the parsed URI
fn default_handler(uri: &BstUri) -> Option<BstUri> {
    Some(uri.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bst_uri_parse() {
        let uri = BstUri::parse("bst://12345/abc123token");
        assert!(uri.is_some());
        let uri = uri.unwrap();
        assert_eq!(uri.app_id, 12345);
        assert_eq!(uri.token, "abc123token");
    }

    #[test]
    fn test_bst_uri_parse_with_params() {
        let uri = BstUri::parse("bst://5678/token?key1=value1&key2=value2");
        assert!(uri.is_some());
        let uri = uri.unwrap();
        assert_eq!(uri.app_id, 5678);
        assert_eq!(uri.token, "token");
        assert_eq!(uri.params.get("key1"), Some(&"value1".to_string()));
        assert_eq!(uri.params.get("key2"), Some(&"value2".to_string()));
    }

    #[test]
    fn test_bst_uri_invalid() {
        let uri = BstUri::parse("http://example.com");
        assert!(uri.is_none());
    }

    #[test]
    fn test_bst_uri_valid() {
        let uri = BstUri::parse("bst://12345/token").is_valid();
        assert!(uri);
    }
}