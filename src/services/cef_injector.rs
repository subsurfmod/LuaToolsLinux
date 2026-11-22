//! CefInjectorService — polls Steam CEF CDP on `127.0.0.1:8080/json` and injects
//! `window.Millennium` polyfill + `luatools.js` into store tabs via WebSocket.

use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use rand::RngCore;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use tungstenite::protocol::Role;
use tungstenite::{Message, WebSocket};
use url::Url;

const CEF_DEBUG_URL: &str = "http://127.0.0.1:8080/json";

pub struct CefInjectorService {
    pub steam_root: Option<PathBuf>,
    pub http_port: u16,
    stop_flag: Arc<AtomicBool>,
    status: Arc<Mutex<String>>,
    handle: Option<JoinHandle<()>>,
}

impl CefInjectorService {
    pub fn new(steam_root: Option<PathBuf>, http_port: u16) -> Self {
        Self {
            steam_root,
            http_port,
            stop_flag: Arc::new(AtomicBool::new(false)),
            status: Arc::new(Mutex::new("Stopped".into())),
            handle: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.handle
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }

    pub fn last_status(&self) -> String {
        self.status.lock().unwrap().clone()
    }

    pub fn ensure_cef_debugging_enabled(steam_root: &Path) -> Result<String, String> {
        let marker = steam_root.join(".cef-enable-remote-debugging");
        fs::write(&marker, b"").map_err(|e| e.to_string())?;
        Ok(format!(
            "Created {} (restart Steam to open CDP on :8080)",
            marker.display()
        ))
    }

    pub fn start(&mut self) {
        if self.is_running() {
            return;
        }
        self.stop_flag.store(false, Ordering::Relaxed);
        let stop = Arc::clone(&self.stop_flag);
        let status = Arc::clone(&self.status);
        let port = self.http_port;

        self.handle = Some(thread::spawn(move || {
            *status.lock().unwrap() = "Polling CDP :8080...".into();
            let agent = ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(2))
                .build();
            let mut injected_tabs: HashSet<String> = HashSet::new();

            while !stop.load(Ordering::Relaxed) {
                match agent.get(CEF_DEBUG_URL).call() {
                    Ok(resp) => {
                        if let Ok(tabs) = resp.into_json::<Vec<Value>>() {
                            let mut store_count = 0usize;
                            for tab in tabs {
                                let url = tab.get("url").and_then(|v| v.as_str()).unwrap_or("");
                                let ws = tab
                                    .get("webSocketDebuggerUrl")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let tid = tab.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                if url.contains("store.steampowered.com") && !ws.is_empty() {
                                    store_count += 1;
                                    if !tid.is_empty() && !injected_tabs.contains(tid) {
                                        if inject_ws_tab(ws, port) {
                                            injected_tabs.insert(tid.to_string());
                                        }
                                    }
                                }
                            }
                            *status.lock().unwrap() =
                                format!("CDP connected ({} store tab(s))", store_count);
                        }
                    }
                    Err(_) => {
                        *status.lock().unwrap() = "Waiting for Steam CDP :8080...".into();
                    }
                }
                for _ in 0..8 {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(250));
                }
            }
            *status.lock().unwrap() = "Stopped".into();
        }));
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        *self.status.lock().unwrap() = "Stopped".into();
    }
}

impl Drop for CefInjectorService {
    fn drop(&mut self) {
        self.stop();
    }
}

fn find_luatools_js() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let candidates = [
        home.join(".local/share/millennium/plugins/luatools/public/luatools.js"),
        home.join(".local/share/Steam/steamui/LuaTools/luatools.js"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

fn build_polyfill_js(http_port: u16) -> String {
    format!(
        r#"(function() {{
  if (window.__luatoolsLinuxPolyfill) return;
  window.__luatoolsLinuxPolyfill = true;
  const BASE = "http://127.0.0.1:{http_port}";
  if (!window.Millennium) {{
    window.Millennium = {{
      callServerMethod: async function(method, args) {{
        const r = await fetch(BASE + "/rpc", {{
          method: "POST",
          headers: {{"Content-Type": "application/json"}},
          body: JSON.stringify({{method, args: args || {{}}}})
        }});
        return await r.text();
      }}
    }};
  }}
}})();"#
    )
}

pub fn connect_ws_local(ws_url: &str) -> Result<WebSocket<TcpStream>, String> {
    let parsed = Url::parse(ws_url).map_err(|e| e.to_string())?;
    let host = parsed.host_str().unwrap_or("127.0.0.1");
    let port = parsed.port_or_known_default().unwrap_or(8080);
    let addr: SocketAddr = format!("{}:{}", host, port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or_else(|| "No socket address resolved".to_string())?;

    let mut stream =
        TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|e| e.to_string())?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

    let mut nonce = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ws_key = BASE64.encode(nonce);

    let mut path_and_query = parsed.path().to_string();
    if path_and_query.is_empty() {
        path_and_query.push('/');
    }
    if let Some(q) = parsed.query() {
        path_and_query.push('?');
        path_and_query.push_str(q);
    }

    let req = format!(
        "GET {} HTTP/1.1\r\n\
         Host: {}:{}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {}\r\n\
         Sec-WebSocket-Version: 13\r\n\r\n",
        path_and_query, host, port, ws_key
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;

    let mut resp_buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 256];
    let header_end = loop {
        let n = stream.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("EOF during WebSocket HTTP upgrade".into());
        }
        resp_buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = resp_buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        if resp_buf.len() > 16 * 1024 {
            return Err("WebSocket upgrade header exceeded 16 KiB".into());
        }
    };

    let headers_str = String::from_utf8_lossy(&resp_buf[..header_end]);
    let status_line = headers_str.lines().next().unwrap_or("");
    if !status_line.contains(" 101 ") && !status_line.ends_with(" 101") {
        return Err(format!("Unexpected WS status: {}", status_line));
    }

    let mut sha = Sha1::new();
    sha.update(ws_key.as_bytes());
    sha.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    let expected_accept = BASE64.encode(sha.finalize());
    let accept_ok = headers_str.lines().any(|line| {
        if let Some((k, v)) = line.split_once(':') {
            k.trim().eq_ignore_ascii_case("sec-websocket-accept") && v.trim() == expected_accept
        } else {
            false
        }
    });
    if !accept_ok {
        return Err("Invalid Sec-WebSocket-Accept header".into());
    }

    let leftover = resp_buf[header_end..].to_vec();
    Ok(WebSocket::from_partially_read(
        stream,
        leftover,
        Role::Client,
        None,
    ))
}

fn inject_ws_tab(ws_url: &str, http_port: u16) -> bool {
    let mut js = build_polyfill_js(http_port);
    if let Some(p) = find_luatools_js() {
        if let Ok(extra) = fs::read_to_string(p) {
            js.push('\n');
            js.push_str(&extra);
        }
    }

    let Ok(mut socket) = connect_ws_local(ws_url) else {
        return false;
    };
    let payload = json!({
        "id": 1,
        "method": "Runtime.evaluate",
        "params": { "expression": js }
    });
    if socket
        .send(Message::Text(payload.to_string().into()))
        .is_err()
    {
        return false;
    }
    let _ = socket.read();
    let _ = socket.close(None);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn performs_rfc6455_handshake_and_evaluates_cdp_polyfill() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = thread::spawn(move || -> String {
            let (mut conn, _) = listener.accept().unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 512];
            loop {
                let n = conn.read(&mut buf).unwrap();
                req.extend_from_slice(&buf[..n]);
                if req.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let req_str = String::from_utf8_lossy(&req);
            let key = req_str
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.trim()
                        .eq_ignore_ascii_case("sec-websocket-key")
                        .then(|| v.trim().to_string())
                })
                .unwrap();

            let mut sha = Sha1::new();
            sha.update(key.as_bytes());
            sha.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
            let accept = BASE64.encode(sha.finalize());

            let resp = format!(
                "HTTP/1.1 101 Switching Protocols\r\n\
                 Upgrade: websocket\r\n\
                 Connection: Upgrade\r\n\
                 Sec-WebSocket-Accept: {}\r\n\r\n",
                accept
            );
            conn.write_all(resp.as_bytes()).unwrap();

            let mut ws = WebSocket::from_raw_socket(conn, Role::Server, None);
            let msg = ws.read().unwrap().into_text().unwrap().to_string();
            let _ = ws.send(Message::Text(r#"{"id":1,"result":{}}"#.into()));
            msg
        });

        let ws_url = format!("ws://127.0.0.1:{}/devtools/page/TAB1", port);
        assert!(inject_ws_tab(&ws_url, 6767));
        let received = server.join().unwrap();
        assert!(received.contains("Runtime.evaluate"));
        assert!(received.contains("http://127.0.0.1:6767"));
    }
}
