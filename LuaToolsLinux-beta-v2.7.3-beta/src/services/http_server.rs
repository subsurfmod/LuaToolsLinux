//! Local HTTP/RPC server (`127.0.0.1:6767`) for injected Steam Store JS & CEF CDP polyfill.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

#[derive(Debug, Clone)]
pub enum RpcUiEvent {
    AddApp(u32),
    OpenFix(u32),
    OpenSettings,
    RemovedApp(u32),
}

pub struct HttpServerService {
    pub port: u16,
    stop_flag: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl HttpServerService {
    pub fn start(
        port: u16,
        stplug_dir: PathBuf,
        event_tx: Sender<RpcUiEvent>,
        wakeup: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Option<Self> {
        let server = Server::http(("127.0.0.1", port)).ok()?;
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop_flag);

        let handle = thread::spawn(move || {
            while !stop_clone.load(Ordering::Relaxed) {
                if let Ok(Some(req)) = server.recv_timeout(Duration::from_millis(250)) {
                    handle_request(req, &stplug_dir, &event_tx, &wakeup);
                }
            }
        });

        Some(Self {
            port,
            stop_flag,
            handle: Some(handle),
        })
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for HttpServerService {
    fn drop(&mut self) {
        self.stop();
    }
}

fn send_json(req: Request, status: u16, payload: Value) {
    let body = payload.to_string();
    let resp = Response::from_string(body)
        .with_status_code(status)
        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
        .with_header(Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap())
        .with_header(
            Header::from_bytes("Access-Control-Allow-Methods", "GET, POST, OPTIONS").unwrap(),
        )
        .with_header(Header::from_bytes("Access-Control-Allow-Headers", "Content-Type").unwrap());
    let _ = req.respond(resp);
}

fn handle_request(
    mut req: Request,
    stplug_dir: &Path,
    event_tx: &Sender<RpcUiEvent>,
    wakeup: &Option<Arc<dyn Fn() + Send + Sync>>,
) {
    if req.method() == &Method::Options {
        let resp = Response::empty(204)
            .with_header(Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap())
            .with_header(
                Header::from_bytes("Access-Control-Allow-Methods", "GET, POST, OPTIONS").unwrap(),
            )
            .with_header(
                Header::from_bytes("Access-Control-Allow-Headers", "Content-Type").unwrap(),
            );
        let _ = req.respond(resp);
        return;
    }

    let path_only = req.url().split('?').next().unwrap_or("/");
    let parts: Vec<&str> = path_only.split('/').filter(|s| !s.is_empty()).collect();

    let notify = || {
        if let Some(cb) = wakeup {
            cb();
        }
    };

    if parts == ["ping"] {
        send_json(
            req,
            200,
            json!({ "ok": true, "platform": "linux", "version": crate::VERSION }),
        );
        return;
    }

    if parts == ["rpc"] && req.method() == &Method::Post {
        let mut raw = String::new();
        let _ = req.as_reader().take(256 * 1024).read_to_string(&mut raw);
        let Ok(payload) = serde_json::from_str::<Value>(&raw) else {
            send_json(
                req,
                400,
                json!({ "success": false, "error": "Invalid JSON" }),
            );
            return;
        };
        let method = payload.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let args = payload.get("args").cloned().unwrap_or_else(|| json!({}));
        let aid = args
            .get("appid")
            .or_else(|| args.get("appId"))
            .and_then(|v| {
                v.as_u64()
                    .map(|n| n as u32)
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u32>().ok()))
            })
            .unwrap_or(0);

        match method {
            "Ping" | "ping" => send_json(req, 200, json!({ "success": true, "result": "pong" })),
            "HasLuaToolsForApp" if aid > 0 => {
                let exists = stplug_dir.join(format!("{}.lua", aid)).exists();
                send_json(
                    req,
                    200,
                    json!({ "success": true, "exists": exists, "result": exists }),
                );
            }
            "StartAddViaLuaTools" if aid > 0 => {
                let _ = event_tx.send(RpcUiEvent::AddApp(aid));
                notify();
                send_json(req, 200, json!({ "success": true, "queued": true }));
            }
            "DeleteLuaToolsForApp" if aid > 0 => {
                let _ = std::fs::remove_file(stplug_dir.join(format!("{}.lua", aid)));
                let _ = event_tx.send(RpcUiEvent::RemovedApp(aid));
                notify();
                send_json(req, 200, json!({ "success": true }));
            }
            "CheckForFixes" | "OpenFixes" if aid > 0 => {
                let _ = event_tx.send(RpcUiEvent::OpenFix(aid));
                notify();
                send_json(req, 200, json!({ "success": true }));
            }
            "OpenSettings" => {
                let _ = event_tx.send(RpcUiEvent::OpenSettings);
                notify();
                send_json(req, 200, json!({ "success": true }));
            }
            "GetSettingsConfig" => {
                send_json(req, 200, json!({ "success": true, "result": {} }));
            }
            _ => send_json(
                req,
                200,
                json!({ "success": false, "error": format!("Unsupported method: {}", method) }),
            ),
        }
        return;
    }

    if parts.len() == 2 {
        if let Ok(aid) = parts[1].parse::<u32>() {
            match parts[0] {
                "has" => {
                    let exists = stplug_dir.join(format!("{}.lua", aid)).exists();
                    send_json(req, 200, json!({ "has": exists, "appid": aid }));
                    return;
                }
                "add" => {
                    let _ = event_tx.send(RpcUiEvent::AddApp(aid));
                    notify();
                    send_json(req, 200, json!({ "queued": true, "appid": aid }));
                    return;
                }
                "remove" => {
                    let _ = std::fs::remove_file(stplug_dir.join(format!("{}.lua", aid)));
                    let _ = event_tx.send(RpcUiEvent::RemovedApp(aid));
                    notify();
                    send_json(req, 200, json!({ "removed": true, "appid": aid }));
                    return;
                }
                _ => {}
            }
        }
    }

    if parts.len() == 3 && parts[0] == "open" && parts[1] == "fix" {
        if let Ok(aid) = parts[2].parse::<u32>() {
            let _ = event_tx.send(RpcUiEvent::OpenFix(aid));
            notify();
            send_json(req, 200, json!({ "ok": true }));
            return;
        }
    }

    if parts == ["open", "settings"] {
        let _ = event_tx.send(RpcUiEvent::OpenSettings);
        notify();
        send_json(req, 200, json!({ "ok": true }));
        return;
    }

    send_json(req, 404, json!({ "error": "Not found" }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn serves_ping_and_rpc_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::channel();
        let mut srv = HttpServerService::start(16779, dir.path().to_path_buf(), tx, None).unwrap();

        let ping: Value = ureq::get("http://127.0.0.1:16779/ping")
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(ping["ok"], true);

        let rpc: Value = ureq::post("http://127.0.0.1:16779/rpc")
            .send_json(json!({ "method": "HasLuaToolsForApp", "args": { "appid": 1245620 } }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(rpc["success"], true);
        assert_eq!(rpc["exists"], false);

        srv.stop();
    }
}
