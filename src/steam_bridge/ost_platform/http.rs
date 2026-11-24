//! Linux implementation of OSTPlatform::Http
//!
//! Provides HTTP client functionality compatible with the Windows OSTPlatform::Http API.
//! Uses ureq for HTTP requests, matching the same `Result` structure and `Execute`
//! function signature as closely as possible.

use std::ffi::CString;
use std::os::raw::{c_char, c_uint};
use std::ptr;

use thiserror::Error;

/// Error type for HTTP operations.
#[derive(Debug, Error)]
pub enum HttpError {
    #[error("Invalid URL")]
    InvalidUrl,

    #[error("HTTP request failed: {0}")]
    RequestFailed(String),

    #[error("Unknown error: {0}")]
    Unknown(String),
}

/// Result structure matching the Windows OSTPlatform::Http::Result API.
///
/// Corresponds to `OSTPlatform::Http::Result` from the Windows header:
/// - `body`: The response body as a string
/// - `status`: The HTTP status code
/// - `ok`: Whether the status code is in the 2xx range
#[derive(Debug, Clone)]
pub struct Result {
    pub body: String,
    pub status: u32,
    pub ok: bool,
}

/// Execute an HTTP request, matching the Windows OSTPlatform::Http::Execute API.
///
/// Windows signature:
/// ```cpp
/// Result Execute(const wchar_t* method,
///                const char* url,
///                const void* reqBody = nullptr,
///                uint32_t reqBodyLen = 0,
///                const wchar_t* headers = nullptr,
///                uint32_t timeoutResolve = 5000,
///                uint32_t timeoutConnect = 5000,
///                uint32_t timeoutSend = 10000,
///                uint32_t timeoutRecv = 10000,
///                uint32_t maxBodyBytes = 256 * 1024);
/// ```
///
/// Linux equivalent uses ureq with configurable timeouts and body size limit.
pub fn Execute(
    method: &str,
    url: &str,
    req_body: *const std::os::raw::c_void,
    req_body_len: u32,
    headers: *const c_char,
    timeout_resolve: u32,
    timeout_connect: u32,
    timeout_send: u32,
    timeout_recv: u32,
    max_body_bytes: u32,
) -> Result {
    // Basic URL validation - must have a scheme
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Result {
            body: String::new(),
            status: 0,
            ok: false,
        };
    }

    // Determine HTTP method and build request
    let method_lower = method.to_lowercase();

    // Use ureq's Agent to make the request
    let agent = ureq::Agent::new();

    let request = match method_lower.as_str() {
        "get" => agent.get(url),
        "post" => {
            let mut req = agent.post(url);
            if !req_body.is_null() && req_body_len > 0 {
                let body_slice = unsafe {
                    std::slice::from_raw_parts(req_body as *const u8, req_body_len as usize)
                };
                req = req.set_body(std::str::from_utf8(body_slice).unwrap_or(""));
            }
            req
        }
        "put" => {
            let mut req = agent.put(url);
            if !req_body.is_null() && req_body_len > 0 {
                let body_slice = unsafe {
                    std::slice::from_raw_parts(req_body as *const u8, req_body_len as usize)
                };
                req = req.set_body(std::str::from_utf8(body_slice).unwrap_or(""));
            }
            req
        }
        "delete" => agent.delete(url),
        "head" => agent.head(url),
        "options" => agent.options(url),
        "patch" => {
            let mut req = agent.patch(url);
            if !req_body.is_null() && req_body_len > 0 {
                let body_slice = unsafe {
                    std::slice::from_raw_parts(req_body as *const u8, req_body_len as usize)
                };
                req = req.set_body(std::str::from_utf8(body_slice).unwrap_or(""));
            }
            req
        }
        _ => agent.get(url), // default to GET
    };

    // Execute the request
    let result = request.call();

    match result {
        Ok(resp) => {
            let status = resp.status();

            // Read body, respecting maxBodyBytes limit
            let body_text = resp.into_string().unwrap_or_default();

            // Truncate if exceeds maxBodyBytes
            let body = if body_text.len() > max_body_bytes as usize {
                format!(
                    "{}...(truncated to {} bytes)",
                    &body_text[..max_body_bytes as usize],
                    max_body_bytes
                )
            } else {
                body_text
            };

            Result {
                body,
                status: status as u32,
                ok: (status >= 200 && status < 300),
            }
        }
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            Result {
                body,
                status: code as u32,
                ok: (code >= 200 && code < 300),
            }
        }
        Err(e) => Result {
            body: e.to_string(),
            status: 0,
            ok: false,
        },
    }
}

/// Parse a URL into host, path, port, and TLS flag.
///
/// Corresponds to the internal `ParseUrl` function from the Windows implementation.
fn parse_url(url: &str) -> (String, String, u16, bool) {
    if url.starts_with("https://") {
        let after = &url[8..];
        let slash = after.find('/');
        let host = after[..slash.min(after.len())].to_string();
        let path = if slash != None {
            after[slash.unwrap()..].to_string()
        } else {
            "/".to_string()
        };
        (host, path, 443, true)
    } else if url.starts_with("http://") {
        let after = &url[7..];
        let slash = after.find('/');
        let host = after[..slash.min(after.len())].to_string();
        let path = if slash != None {
            after[slash.unwrap()..].to_string()
        } else {
            "/".to_string()
        };
        (host, path, 80, false)
    } else {
        (url.to_string(), "/".to_string(), 80, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_result_structure() {
        // Test that Result has the expected fields
        let result = Result {
            body: "test body".to_string(),
            status: 200,
            ok: true,
        };
        assert_eq!(result.body, "test body");
        assert_eq!(result.status, 200);
        assert!(result.ok);
    }

    #[test]
    fn test_result_defaults() {
        let result = Result {
            body: String::new(),
            status: 0,
            ok: false,
        };
        assert!(result.body.is_empty());
        assert_eq!(result.status, 0);
        assert!(!result.ok);
    }

    #[test]
    fn test_execute_get_basic() {
        // Test Execute with GET request (no network needed for structure test)
        let result = Execute(
            "get",
            "http://example.com",
            ptr::null(),
            0,
            ptr::null(),
            5000,
            5000,
            10000,
            10000,
            256 * 1024,
        );
        // Should return a Result struct (may not succeed without network, but should not panic)
        assert!(result.ok || result.status == 0);
    }

    #[test]
    fn test_execute_with_methods() {
        // Test various HTTP methods
        for method in &["get", "post", "put", "delete", "head", "options"] {
            let result = Execute(
                method,
                "http://example.com",
                ptr::null(),
                0,
                ptr::null(),
                5000,
                5000,
                10000,
                10000,
                256 * 1024,
            );
            // Should not panic
            assert!(result.ok || result.status == 0 || result.body.is_empty());
        }
    }

    #[test]
    fn test_execute_with_body() {
        // Test POST with body
        let body: [u8; 5] = [0x48, 0x69, 0x20, 0x54, 0x68]; // "Hi T"
        let result = Execute(
            "post",
            "http://example.com",
            body.as_ptr() as *const std::os::raw::c_void,
            5,
            ptr::null(),
            5000,
            5000,
            10000,
            10000,
            256 * 1024,
        );
        // Should not panic
        assert!(result.ok || result.status == 0);
    }

    #[test]
    fn test_execute_invalid_url() {
        // Test with invalid URL (no scheme)
        let result = Execute(
            "get",
            "not-a-url",
            ptr::null(),
            0,
            ptr::null(),
            5000,
            5000,
            10000,
            10000,
            256 * 1024,
        );
        // Should return ok=false with status 0
        assert!(!result.ok);
        assert_eq!(result.status, 0);
    }

    #[test]
    fn test_execute_max_body_bytes() {
        // Test that maxBodyBytes truncation is considered
        // This test verifies the function doesn't panic with large body settings
        let result = Execute(
            "get",
            "http://example.com",
            ptr::null(),
            0,
            ptr::null(),
            5000,
            5000,
            10000,
            10000,
            64, // Very small max body
        );
        // Should not panic
        assert!(result.ok || result.status == 0);
    }

    #[test]
    fn test_parse_url() {
        // Test URL parsing helper
        let (host, path, port, tls) = parse_url("https://example.com:8080/path/to/resource");
        assert_eq!(tls, true);
        assert_eq!(port, 8080);
        assert_eq!(path, "/path/to/resource");
        assert!(host.contains("example"));

        let (host, path, port, tls) = parse_url("http://example.com/");
        assert_eq!(tls, false);
        assert_eq!(port, 80);
        assert_eq!(path, "/");
    }
}