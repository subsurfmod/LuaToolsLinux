//! Linux implementation of `OSTPlatform::Http`.
//!
//! Provides HTTP client functionality compatible with the Windows `OSTPlatform::Http` API
//! using `ureq` with configurable timeouts and response body size limits.

use std::ffi::CStr;
use std::io::Read;
use std::os::raw::{c_char, c_void};
use std::time::Duration;

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

/// Result structure matching `OSTPlatform::Http::Result`.
#[derive(Debug, Clone)]
pub struct Result {
    pub body: String,
    pub status: u32,
    pub ok: bool,
}

/// Execute an HTTP request, matching `OSTPlatform::Http::Execute`.
#[allow(
    non_snake_case,
    clippy::too_many_arguments,
    clippy::not_unsafe_ptr_arg_deref
)]
pub fn Execute(
    method: &str,
    url: &str,
    req_body: *const c_void,
    req_body_len: u32,
    headers: *const c_char,
    _timeout_resolve: u32,
    timeout_connect: u32,
    timeout_send: u32,
    timeout_recv: u32,
    max_body_bytes: u32,
) -> Result {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Result {
            body: String::new(),
            status: 0,
            ok: false,
        };
    }

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_millis(u64::from(timeout_connect.max(50))))
        .timeout_write(Duration::from_millis(u64::from(timeout_send.max(50))))
        .timeout_read(Duration::from_millis(u64::from(timeout_recv.max(50))))
        .build();

    let method_upper = method.trim().to_ascii_uppercase();
    let verb = if method_upper.is_empty() {
        "GET"
    } else {
        method_upper.as_str()
    };

    let mut req = agent.request(verb, url);

    if !headers.is_null() {
        if let Ok(raw_hdrs) = unsafe { CStr::from_ptr(headers) }.to_str() {
            for line in raw_hdrs.lines() {
                if let Some((k, v)) = line.split_once(':') {
                    let k = k.trim();
                    let v = v.trim();
                    if !k.is_empty() {
                        req = req.set(k, v);
                    }
                }
            }
        }
    }

    let call_result = if !req_body.is_null() && req_body_len > 0 {
        let body_slice =
            unsafe { std::slice::from_raw_parts(req_body as *const u8, req_body_len as usize) };
        req.send_bytes(body_slice)
    } else {
        req.call()
    };

    let read_capped = |resp: ureq::Response| -> String {
        let cap = (max_body_bytes as usize).min(16 * 1024 * 1024);
        let mut buf = Vec::new();
        let _ = resp
            .into_reader()
            .take((cap as u64).saturating_add(1))
            .read_to_end(&mut buf);
        if buf.len() > cap {
            buf.truncate(cap);
            format!(
                "{}...(truncated to {} bytes)",
                String::from_utf8_lossy(&buf),
                max_body_bytes
            )
        } else {
            String::from_utf8_lossy(&buf).into_owned()
        }
    };

    match call_result {
        Ok(resp) => {
            let status = resp.status();
            let body = read_capped(resp);
            Result {
                body,
                status: u32::from(status),
                ok: (200..300).contains(&status),
            }
        }
        Err(ureq::Error::Status(code, resp)) => {
            let body = read_capped(resp);
            Result {
                body,
                status: u32::from(code),
                ok: (200..300).contains(&code),
            }
        }
        Err(e) => Result {
            body: e.to_string(),
            status: 0,
            ok: false,
        },
    }
}

/// Parse a URL into `(host, path, port, tls)`.
#[cfg(test)]
fn parse_url(url: &str) -> (String, String, u16, bool) {
    let (after, default_port, tls) = if let Some(rest) = url.strip_prefix("https://") {
        (rest, 443u16, true)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (rest, 80u16, false)
    } else {
        return (url.to_string(), "/".to_string(), 80, false);
    };

    let (authority, path) = match after.find('/') {
        Some(idx) => (&after[..idx], after[idx..].to_string()),
        None => (after, "/".to_string()),
    };

    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => match p.parse::<u16>() {
            Ok(port_num) => (h.to_string(), port_num),
            Err(_) => (authority.to_string(), default_port),
        },
        None => (authority.to_string(), default_port),
    };

    (host, path, port, tls)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    #[test]
    fn test_result_structure() {
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
    fn test_execute_invalid_url() {
        let result = Execute(
            "get",
            "not-a-url",
            ptr::null(),
            0,
            ptr::null(),
            100,
            100,
            100,
            100,
            4096,
        );
        assert!(!result.ok);
        assert_eq!(result.status, 0);
    }

    #[test]
    fn test_execute_local_http_get_post_and_truncation() {
        use std::io::Write;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                if let Ok((mut stream, _)) = listener.accept() {
                    let mut req_buf = [0u8; 512];
                    let _ = stream.read(&mut req_buf);
                    let body = "0123456789abcdefghijklmnopqrstuvwxyz";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
                }
            }
        });

        let url = format!("http://{}/test", addr);
        let r1 = Execute(
            "GET",
            &url,
            ptr::null(),
            0,
            ptr::null(),
            1000,
            1000,
            1000,
            1000,
            1024,
        );
        assert!(r1.ok);
        assert_eq!(r1.status, 200);
        assert_eq!(r1.body, "0123456789abcdefghijklmnopqrstuvwxyz");

        let payload = b"hello";
        let r2 = Execute(
            "POST",
            &url,
            payload.as_ptr() as *const c_void,
            payload.len() as u32,
            ptr::null(),
            1000,
            1000,
            1000,
            1000,
            10,
        );
        assert!(r2.ok);
        assert!(r2.body.starts_with("0123456789"));
        assert!(r2.body.contains("truncated"));

        let _ = handle.join();
    }

    #[test]
    fn test_parse_url() {
        let (host, path, port, tls) = parse_url("https://example.com:8080/path/to/resource");
        assert!(tls);
        assert_eq!(port, 8080);
        assert_eq!(path, "/path/to/resource");
        assert_eq!(host, "example.com");

        let (host, path, port, tls) = parse_url("http://example.com/");
        assert!(!tls);
        assert_eq!(port, 80);
        assert_eq!(path, "/");
        assert_eq!(host, "example.com");
    }
}
