//! CefBridgeService — polls Steam CEF CDP on `127.0.0.1:8080/json` and injects
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

pub struct CefBridgeService {
    pub steam_root: Option<PathBuf>,
    pub http_port: u16,
    stop_flag: Arc<AtomicBool>,
    status: Arc<Mutex<String>>,
    handle: Option<JoinHandle<()>>,
}

impl CefBridgeService {
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
        // LT-22: Create symlink instead of empty file - Millennium/CEF expects a symlink
        // pointing to a marker file or just create the symlink itself
        let target = steam_root.join("cef_debug_enabled");
        if !target.exists() {
            fs::write(&target, b"1").map_err(|e| e.to_string())?;
        }
        // Remove existing file/symlink if present
        let _ = fs::remove_file(&marker);
        std::os::unix::fs::symlink(&target, &marker).map_err(|e| e.to_string())?;
        Ok(format!(
            "Created symlink {} -> {} (restart Steam to open CDP on :8080)",
            marker.display(),
            target.display()
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
        let steam_root = self.steam_root.clone();

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
                            let active_tids: HashSet<String> = tabs
                                .iter()
                                .filter_map(|t| t.get("id").and_then(|v| v.as_str()).map(ToString::to_string))
                                .collect();
                            injected_tabs.retain(|tid| active_tids.contains(tid));

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
                                        injected_tabs.insert(tid.to_string());
                                        let ws_url = ws.to_string();
                                        let stop_tab = Arc::clone(&stop);
                                        let root_opt = steam_root.clone();
                                        thread::spawn(move || {
                                            bridge_ws_tab_with_stop(
                                                &ws_url,
                                                port,
                                                root_opt.as_deref(),
                                                Some(&stop_tab),
                                            );
                                        });
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

impl Drop for CefBridgeService {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn find_luatools_js(steam_root: Option<&Path>) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(root) = steam_root {
        candidates.push(root.join("steamui/LuaTools/luatools.js"));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".local/share/millennium/plugins/luatools/public/luatools.js"));
        candidates.push(home.join(".local/share/Steam/steamui/LuaTools/luatools.js"));
        candidates.push(home.join(".local/share/luatools/plugins/luatools/luatools.js"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

pub fn ensure_luatools_js_installed(steam_root: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = find_luatools_js(steam_root) {
        return Ok(p);
    }
    let installer = crate::services::plugin_installer_service::PluginInstallerService::default();
    installer.ensure_luatools_js_installed(steam_root)
}

fn build_polyfill_js(http_port: u16) -> String {
    format!(
        r#"(function() {{
  if (window.__luatoolsLinuxPolyfill) return;
  window.__luatoolsLinuxPolyfill = true;
  const BASE = "http://127.0.0.1:{http_port}";
  window.__lt_queue = window.__lt_queue || [];
  window.__lt_pending = window.__lt_pending || {{}};
  window.__lt_req_id = window.__lt_req_id || 0;
  window.__lt_drain = function() {{
    const q = window.__lt_queue;
    window.__lt_queue = [];
    return JSON.stringify(q);
  }};
  window.__lt_resolve = function(id, res) {{
    if (window.__lt_pending[id]) {{
      let data = res;
      if (typeof res === "string") {{
        try {{ data = JSON.parse(res); }} catch(_) {{}}
      }}
      window.__lt_pending[id](data);
      delete window.__lt_pending[id];
    }}
  }};
  if (!window.Millennium) {{
    window.Millennium = {{}};
  }}
  window.Millennium.callServerMethod = function(a, b, c) {{
    let method = "";
    let args = {{}};
    if (c !== undefined) {{
      method = String(b || "");
      args = c;
    }} else if (b !== undefined) {{
      if (typeof b === "object" && b !== null) {{
        method = String(a || "");
        args = b;
      }} else {{
        method = String(b || "");
        args = {{}};
      }}
    }} else {{
      method = String(a || "");
      args = {{}};
    }}

    if (typeof args === "string") {{
      try {{ args = JSON.parse(args); }} catch (_) {{}}
    }}

    return new Promise(function(resolve) {{
      const reqId = String(++window.__lt_req_id);
      window.__lt_pending[reqId] = resolve;
      window.__lt_queue.push({{
        id: reqId,
        method: method,
        args: args || {{}}
      }});
    }});
  }};
  if (!window.Millennium.callFrontendMethod) {{
    window.Millennium.callFrontendMethod = function() {{ return Promise.resolve(); }};
  }}
  if (!window.Millennium.findElement) {{
    window.Millennium.findElement = function() {{ return null; }};
  }}

  // LT-23: CEF Protocol Configuration for LuaTools Store Plugin
  window.PluginConfig = {{
    backendPort: {http_port},
    pollInterval: 500
  }};
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

pub fn bridge_ws_tab(ws_url: &str, http_port: u16, steam_root: Option<&Path>) -> bool {
    bridge_ws_tab_with_stop(ws_url, http_port, steam_root, None)
}

pub fn bridge_ws_tab_with_stop(
    ws_url: &str,
    http_port: u16,
    steam_root: Option<&Path>,
    stop: Option<&AtomicBool>,
) -> bool {
    let mut js = build_polyfill_js(http_port);
    if let Ok(p) = ensure_luatools_js_installed(steam_root) {
        if let Ok(extra) = fs::read_to_string(p) {
            js.push('\n');
            js.push_str(&extra);
        }
    }

    let Ok(mut socket) = connect_ws_local(ws_url) else {
        return false;
    };

    // 1. Enable Page domain
    let page_enable = json!({
        "id": 1,
        "method": "Page.enable"
    });
    if socket
        .send(Message::Text(page_enable.to_string().into()))
        .is_err()
    {
        return false;
    }
    let _ = socket.read();

    // 2. Bypass CSP on this tab (LT-23 CSP handling)
    let bypass_csp = json!({
        "id": 2,
        "method": "Page.setBypassCSP",
        "params": { "enabled": true }
    });
    if socket
        .send(Message::Text(bypass_csp.to_string().into()))
        .is_err()
    {
        return false;
    }
    let _ = socket.read();

    // 3. Add script to evaluate on new document so it survives same-tab navigation
    let new_doc_script = json!({
        "id": 3,
        "method": "Page.addScriptToEvaluateOnNewDocument",
        "params": {
            "source": js
        }
    });
    let _ = socket.send(Message::Text(new_doc_script.to_string().into()));
    let _ = socket.read();

    // 4. Evaluate polyfill + luatools.js in current document
    let payload = json!({
        "id": 4,
        "method": "Runtime.evaluate",
        "params": {
            "expression": js,
            "userGesture": true,
            "awaitPromise": false
        }
    });
    if socket
        .send(Message::Text(payload.to_string().into()))
        .is_err()
    {
        return false;
    }
    let _ = socket.read();

    // 5. Keep WebSocket open and poll window.__lt_drain() to dispatch queued RPC calls outside browser CSP
    let mut cmd_id: i64 = 5;
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(4))
        .build();

    loop {
        if let Some(s) = stop {
            if s.load(Ordering::Relaxed) {
                break;
            }
        }

        let drain_eval = json!({
            "id": cmd_id,
            "method": "Runtime.evaluate",
            "params": {
                "expression": "typeof window.__lt_drain === 'function' ? window.__lt_drain() : '[]'",
                "returnByValue": true
            }
        });
        cmd_id += 1;

        if socket.send(Message::Text(drain_eval.to_string().into())).is_err() {
            break;
        }

        let msg = match socket.read() {
            Ok(m) => m,
            Err(e) => {
                match e {
                    tungstenite::Error::Io(ref io_err)
                        if io_err.kind() == std::io::ErrorKind::WouldBlock
                            || io_err.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        continue;
                    }
                    _ => break,
                }
            }
        };

        if let Ok(txt) = msg.into_text() {
            if let Ok(val) = serde_json::from_str::<Value>(&txt) {
                if let Some(res_str) = val
                    .get("result")
                    .and_then(|r| r.get("result"))
                    .and_then(|r| r.get("value"))
                    .and_then(|v| v.as_str())
                {
                    if let Ok(queued) = serde_json::from_str::<Vec<Value>>(res_str) {
                        for item in queued {
                            if let Some(req_id) = item.get("id").and_then(|i| i.as_str()) {
                                let method = item
                                    .get("method")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("");
                                let args = item.get("args").cloned().unwrap_or_else(|| json!({}));
                                let rpc_res = agent
                                    .post(&format!("http://127.0.0.1:{}/rpc", http_port))
                                    .send_json(json!({ "method": method, "args": args }))
                                    .ok()
                                    .and_then(|r| r.into_string().ok())
                                    .unwrap_or_else(|| json!({ "success": true }).to_string());

                                let res_arg = match serde_json::from_str::<Value>(&rpc_res) {
                                    Ok(v) => v.to_string(),
                                    Err(_) => serde_json::to_string(&rpc_res).unwrap_or_else(|_| "{}".into()),
                                };

                                let resolve_js = format!(
                                    "if (typeof window.__lt_resolve === 'function') window.__lt_resolve('{}', {});",
                                    req_id, res_arg
                                );
                                let resolve_cmd = json!({
                                    "id": cmd_id,
                                    "method": "Runtime.evaluate",
                                    "params": {
                                        "expression": resolve_js
                                    }
                                });
                                cmd_id += 1;
                                if socket.send(Message::Text(resolve_cmd.to_string().into())).is_err() {
                                    break;
                                }
                                let _ = socket.read();
                            }
                        }
                    }
                }
            }
        }

        let mut stopped = false;
        for _ in 0..5 {
            if let Some(s) = stop {
                if s.load(Ordering::Relaxed) {
                    stopped = true;
                    break;
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
        if stopped {
            break;
        }
    }

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

        let server = thread::spawn(move || -> Vec<String> {
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
            let mut received = Vec::new();
            for _ in 0..5 {
                if let Ok(msg) = ws.read() {
                    if let Ok(text) = msg.into_text() {
                        let text_str = text.to_string();
                        let id = serde_json::from_str::<Value>(&text_str)
                            .ok()
                            .and_then(|v| v.get("id").and_then(|i| i.as_i64()))
                            .unwrap_or(1);
                        received.push(text_str);
                        let _ = ws.send(Message::Text(format!(r#"{{"id":{},"result":{{"result":{{"value":"[]"}}}}}}"#, id).into()));
                    }
                }
            }
            received
        });

        let ws_url = format!("ws://127.0.0.1:{}/devtools/page/TAB1", port);
        assert!(bridge_ws_tab(&ws_url, 6767, None));
        let received = server.join().unwrap();
        assert_eq!(received.len(), 5);
        assert!(received[0].contains("Page.enable"));
        assert!(received[1].contains("Page.setBypassCSP"));
        assert!(received[1].contains(r#""enabled":true"#));
        assert!(received[2].contains("Page.addScriptToEvaluateOnNewDocument"));
        assert!(received[3].contains("Runtime.evaluate"));
        assert!(received[3].contains("http://127.0.0.1:6767"));
        assert!(received[4].contains("__lt_drain"));
    }

    #[test]
    fn cdp_queue_drain_retrieves_requests_and_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let (rpc_tx, _rpc_rx) = std::sync::mpsc::channel();
        let http_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let http_port = http_listener.local_addr().unwrap().port();
        drop(http_listener);

        let mut srv = crate::services::http_server::HttpServerService::start(
            http_port,
            dir.path().to_path_buf(),
            rpc_tx,
            None,
        )
        .unwrap();

        let ws_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let ws_port = ws_listener.local_addr().unwrap().port();

        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);

        let server = thread::spawn(move || -> Vec<String> {
            let (mut conn, _) = ws_listener.accept().unwrap();
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
            let mut received = Vec::new();

            // 1: Page.enable, 2: Page.setBypassCSP, 3: Page.addScript, 4: Runtime.eval polyfill
            for _ in 0..4 {
                if let Ok(msg) = ws.read() {
                    if let Ok(text) = msg.into_text() {
                        let id = serde_json::from_str::<Value>(&text)
                            .ok()
                            .and_then(|v| v.get("id").and_then(|i| i.as_i64()))
                            .unwrap_or(1);
                        received.push(text.to_string());
                        let _ = ws.send(Message::Text(format!(r#"{{"id":{},"result":{{}}}}"#, id).into()));
                    }
                }
            }

            // 5: First drain eval -> return queued Ping request
            if let Ok(msg) = ws.read() {
                if let Ok(text) = msg.into_text() {
                    let id = serde_json::from_str::<Value>(&text)
                        .ok()
                        .and_then(|v| v.get("id").and_then(|i| i.as_i64()))
                        .unwrap_or(5);
                    received.push(text.to_string());
                    let payload = r#"[{"id":"req-42","method":"Ping","args":{}}]"#;
                    let resp_body = json!({
                        "id": id,
                        "result": {
                            "result": {
                                "value": payload
                            }
                        }
                    });
                    let _ = ws.send(Message::Text(resp_body.to_string().into()));
                }
            }

            // 6: bridge_ws_tab sends Runtime.evaluate with __lt_resolve
            if let Ok(msg) = ws.read() {
                if let Ok(text) = msg.into_text() {
                    let id = serde_json::from_str::<Value>(&text)
                        .ok()
                        .and_then(|v| v.get("id").and_then(|i| i.as_i64()))
                        .unwrap_or(6);
                    received.push(text.to_string());
                    let _ = ws.send(Message::Text(format!(r#"{{"id":{},"result":{{}}}}"#, id).into()));
                }
            }

            // Signal stop and exit
            stop_clone.store(true, Ordering::Relaxed);
            received
        });

        let ws_url = format!("ws://127.0.0.1:{}/devtools/page/TAB1", ws_port);
        assert!(bridge_ws_tab_with_stop(&ws_url, http_port, None, Some(&stop)));
        let received = server.join().unwrap();
        assert!(received.len() >= 6);
        assert!(received[4].contains("__lt_drain"));
        assert!(received[5].contains("__lt_resolve"));
        assert!(received[5].contains("req-42"));
        assert!(received[5].contains("pong"));

        srv.stop();
    }
}
