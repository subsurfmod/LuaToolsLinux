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
            Header::from_bytes(
                "Access-Control-Allow-Methods",
                "GET, POST, PUT, DELETE, OPTIONS, HEAD",
            )
            .unwrap(),
        )
        .with_header(
            Header::from_bytes(
                "Access-Control-Allow-Headers",
                "Content-Type, Authorization, X-Requested-With, Accept, Origin",
            )
            .unwrap(),
        )
        .with_header(Header::from_bytes("Access-Control-Max-Age", "86400").unwrap());
    let _ = req.respond(resp);
}

fn send_bytes(req: Request, status: u16, content_type: &str, bytes: Vec<u8>) {
    let resp = Response::from_data(bytes)
        .with_status_code(status)
        .with_header(Header::from_bytes("Content-Type", content_type.to_string()).unwrap())
        .with_header(Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap())
        .with_header(
            Header::from_bytes(
                "Access-Control-Allow-Methods",
                "GET, POST, PUT, DELETE, OPTIONS, HEAD",
            )
            .unwrap(),
        )
        .with_header(
            Header::from_bytes(
                "Access-Control-Allow-Headers",
                "Content-Type, Authorization, X-Requested-With, Accept, Origin",
            )
            .unwrap(),
        )
        .with_header(Header::from_bytes("Access-Control-Max-Age", "86400").unwrap());
    let _ = req.respond(resp);
}

fn extract_query_param(query_str: &str, param: &str) -> Option<String> {
    for pair in query_str.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k.eq_ignore_ascii_case(param) {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn extract_query_appid(query_str: &str) -> Option<u32> {
    for pair in query_str.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k.eq_ignore_ascii_case("appid") {
                if let Ok(id) = v.parse::<u32>() {
                    return Some(id);
                }
            }
        }
    }
    None
}

fn list_installed_apps(stplug_dir: &Path) -> Vec<u32> {
    let mut apps = Vec::new();
    if let Ok(entries) = std::fs::read_dir(stplug_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().map_or(false, |ext| ext == "lua") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    if let Ok(id) = stem.parse::<u32>() {
                        apps.push(id);
                    }
                }
            }
        }
    }
    apps.sort();
    apps
}

fn handle_request(
    mut req: Request,
    stplug_dir: &Path,
    event_tx: &Sender<RpcUiEvent>,
    wakeup: &Option<Arc<dyn Fn() + Send + Sync>>,
) {
    // 1. CORS preflight handling (LT-26)
    if req.method() == &Method::Options {
        let resp = Response::empty(204)
            .with_header(Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap())
            .with_header(
                Header::from_bytes(
                    "Access-Control-Allow-Methods",
                    "GET, POST, PUT, DELETE, OPTIONS, HEAD",
                )
                .unwrap(),
            )
            .with_header(
                Header::from_bytes(
                    "Access-Control-Allow-Headers",
                    "Content-Type, Authorization, X-Requested-With, Accept, Origin",
                )
                .unwrap(),
            )
            .with_header(Header::from_bytes("Access-Control-Max-Age", "86400").unwrap());
        let _ = req.respond(resp);
        return;
    }

    let full_url = req.url().to_string();
    let mut parts_split = full_url.splitn(2, '?');
    let path_only = parts_split.next().unwrap_or("/");
    let query_str = parts_split.next().unwrap_or("");
    let parts: Vec<&str> = path_only.split('/').filter(|s| !s.is_empty()).collect();

    let notify = || {
        if let Some(cb) = wakeup {
            cb();
        }
    };

    // Helper to extract appid from path segments, query param, or JSON body
    let mut aid = parts
        .iter()
        .find_map(|p| p.parse::<u32>().ok())
        .or_else(|| extract_query_appid(query_str))
        .unwrap_or(0);

    // Read JSON body for POST/PUT if available
    let body_json = if req.method() == &Method::Post || req.method() == &Method::Put {
        let mut raw = String::new();
        let _ = req.as_reader().take(256 * 1024).read_to_string(&mut raw);
        serde_json::from_str::<Value>(&raw).ok()
    } else {
        None
    };

    if aid == 0 {
        if let Some(ref val) = body_json {
            if let Some(id) = val
                .get("appid")
                .or_else(|| val.get("appId"))
                .and_then(|v| {
                    v.as_u64()
                        .map(|n| n as u32)
                        .or_else(|| v.as_str().and_then(|s| s.parse::<u32>().ok()))
                })
            {
                aid = id;
            }
        }
    }

    // Route: /ping
    if parts == ["ping"] {
        send_json(
            req,
            200,
            json!({ "ok": true, "platform": "linux", "version": crate::VERSION }),
        );
        return;
    }

    // Route: /version
    if parts == ["version"] || parts == ["api", "version"] {
        send_json(
            req,
            200,
            json!({ "ok": true, "version": crate::VERSION, "platform": "linux" }),
        );
        return;
    }

    // Route: /status or /health
    if parts == ["status"] || parts == ["health"] {
        send_json(
            req,
            200,
            json!({ "ok": true, "status": "running", "platform": "linux" }),
        );
        return;
    }

    // Route: /apps or /installed or /list
    if parts == ["apps"] || parts == ["installed"] || parts == ["list"] {
        let apps = list_installed_apps(stplug_dir);
        send_json(
            req,
            200,
            json!({ "ok": true, "apps": apps, "total": apps.len() }),
        );
        return;
    }

    // Route: /rpc (JSON-RPC)
    if parts == ["rpc"] && req.method() == &Method::Post {
        let Some(payload) = body_json else {
            send_json(
                req,
                400,
                json!({ "success": false, "error": "Invalid JSON" }),
            );
            return;
        };
        let method = payload.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let args = payload.get("args").cloned().unwrap_or_else(|| json!({}));
        let rpc_aid = args
            .get("appid")
            .or_else(|| args.get("appId"))
            .and_then(|v| {
                v.as_u64()
                    .map(|n| n as u32)
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u32>().ok()))
            })
            .unwrap_or(aid);

        match method {
            "Ping" | "ping" => send_json(req, 200, json!({ "success": true, "result": "pong" })),
            "HasLuaToolsForApp" if rpc_aid > 0 => {
                let exists = stplug_dir.join(format!("{}.lua", rpc_aid)).exists();
                send_json(
                    req,
                    200,
                    json!({ "success": true, "exists": exists, "result": exists }),
                );
            }
            "StartAddViaLuaTools" if rpc_aid > 0 => {
                let _ = event_tx.send(RpcUiEvent::AddApp(rpc_aid));
                notify();
                send_json(req, 200, json!({ "success": true, "queued": true }));
            }
            "DeleteLuaToolsForApp" if rpc_aid > 0 => {
                let _ = std::fs::remove_file(stplug_dir.join(format!("{}.lua", rpc_aid)));
                let _ = event_tx.send(RpcUiEvent::RemovedApp(rpc_aid));
                notify();
                send_json(req, 200, json!({ "success": true }));
            }
            "CheckForFixes" | "OpenFixes" if rpc_aid > 0 => {
                let _ = event_tx.send(RpcUiEvent::OpenFix(rpc_aid));
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
            "GetInstalledLuaScripts" | "ReadLoadedApps" | "GetInstalledApps" => {
                let apps = list_installed_apps(stplug_dir);
                send_json(
                    req,
                    200,
                    json!({ "success": true, "result": apps, "apps": apps }),
                );
            }
            "GetAddViaLuaToolsStatus" => {
                send_json(
                    req,
                    200,
                    json!({ "success": true, "result": { "status": "completed" } }),
                );
            }
            "GetApplyFixStatus" => {
                send_json(
                    req,
                    200,
                    json!({ "success": true, "result": { "status": "idle" } }),
                );
            }
            "GetInstalledFixes" => {
                send_json(req, 200, json!({ "success": true, "result": [] }));
            }
            "GetLauncherPath" => {
                send_json(req, 200, json!({ "success": true, "result": "" }));
            }
            "GetWorkshopToolPath" => {
                send_json(req, 200, json!({ "success": true, "result": "" }));
            }
            "RestartSteam" => {
                let _ = crate::services::steam_service::SteamService::relaunch_steam_sls(None);
                send_json(req, 200, json!({ "success": true, "result": "restarted" }));
            }
            "GetThemes" => {
                send_json(req, 200, json!({ "success": true, "result": [] }));
            }
            "GetAvailableLocales" => {
                send_json(
                    req,
                    200,
                    json!({ "success": true, "result": ["en", "zh"] }),
                );
            }
            "GetTranslations" => {
                send_json(req, 200, json!({ "success": true, "result": {} }));
            }
            "GetGamesDatabase" => {
                send_json(req, 200, json!({ "success": true, "result": {} }));
            }
            "GetProtonDBStatus" => {
                send_json(req, 200, json!({ "success": true, "result": {} }));
            }
            "version" | "GetVersion" => {
                send_json(
                    req,
                    200,
                    json!({ "success": true, "result": crate::VERSION }),
                );
            }
            _ => send_json(
                req,
                200,
                json!({ "success": true, "result": {} }),
            ),
        }
        return;
    }

    // Route: /has or /has/:appid
    if parts.first() == Some(&"has") || (parts.len() == 2 && parts[0] == "api" && parts[1] == "has") {
        let exists = aid > 0 && stplug_dir.join(format!("{}.lua", aid)).exists();
        send_json(
            req,
            200,
            json!({ "has": exists, "exists": exists, "appid": aid, "ok": true }),
        );
        return;
    }

    // Route: /add or /add/:appid
    if parts.first() == Some(&"add") || (parts.len() == 2 && parts[0] == "api" && parts[1] == "add") {
        if aid > 0 {
            let _ = event_tx.send(RpcUiEvent::AddApp(aid));
            notify();
        }
        send_json(
            req,
            200,
            json!({ "queued": true, "appid": aid, "success": true, "ok": true }),
        );
        return;
    }

    // Route: /remove or /remove/:appid or /delete or /delete/:appid
    if parts.first() == Some(&"remove")
        || parts.first() == Some(&"delete")
        || (parts.len() == 2 && parts[0] == "api" && (parts[1] == "remove" || parts[1] == "delete"))
    {
        if aid > 0 {
            let _ = std::fs::remove_file(stplug_dir.join(format!("{}.lua", aid)));
            let _ = event_tx.send(RpcUiEvent::RemovedApp(aid));
            notify();
        }
        send_json(
            req,
            200,
            json!({ "removed": true, "appid": aid, "success": true, "ok": true }),
        );
        return;
    }

    // Route: /open/fix or /open/fix/:appid or /fix or /fix/:appid
    if (parts.len() >= 2 && parts[0] == "open" && parts[1] == "fix")
        || parts.first() == Some(&"fix")
    {
        let _ = event_tx.send(RpcUiEvent::OpenFix(aid));
        notify();
        send_json(req, 200, json!({ "ok": true, "appid": aid }));
        return;
    }

    // Route: /open/settings or /settings
    if (parts.len() == 2 && parts[0] == "open" && parts[1] == "settings")
        || parts == ["settings"]
    {
        let _ = event_tx.send(RpcUiEvent::OpenSettings);
        notify();
        send_json(req, 200, json!({ "ok": true }));
        return;
    }

    // Route: /open/app/:appid or /open/:appid
    if (parts.len() >= 2 && parts[0] == "open" && parts[1] == "app")
        || (parts.len() == 2 && parts[0] == "open" && aid > 0)
    {
        if aid > 0 {
            let _ = event_tx.send(RpcUiEvent::AddApp(aid));
            notify();
        }
        send_json(req, 200, json!({ "ok": true, "appid": aid }));
        return;
    }

    // Route: /open/acquire or /open/acquire/:appid
    if parts.len() >= 2 && parts[0] == "open" && parts[1] == "acquire" {
        if aid > 0 {
            let _ = event_tx.send(RpcUiEvent::AddApp(aid));
            notify();
        }
        send_json(req, 200, json!({ "ok": true, "appid": aid, "queued": true }));
        return;
    }

    // Route: /open/library
    if parts == ["open", "library"] || parts == ["library"] {
        notify();
        send_json(req, 200, json!({ "ok": true }));
        return;
    }

    // Route: /open/depots or /open/depots/:appid
    if parts.len() >= 2 && parts[0] == "open" && parts[1] == "depots" {
        notify();
        send_json(req, 200, json!({ "ok": true, "appid": aid }));
        return;
    }

    // Route: /steam/restart or /restart
    if parts == ["steam", "restart"] || parts == ["restart"] {
        let _ = crate::services::steam_service::SteamService::relaunch_steam_sls(None);
        send_json(req, 200, json!({ "ok": true, "restarting": true }));
        return;
    }

    // Route: /steam/status
    if parts == ["steam", "status"] {
        let running = crate::services::steam_service::SteamService::is_steam_running();
        send_json(req, 200, json!({ "ok": true, "running": running }));
        return;
    }

    // Route: /config or /settings/config
    if parts == ["config"] || parts == ["settings", "config"] {
        send_json(
            req,
            200,
            json!({ "ok": true, "config": { "platform": "linux", "version": crate::VERSION } }),
        );
        return;
    }

    // Route: /check/:appid or /check/fix/:appid
    if parts.first() == Some(&"check") {
        send_json(
            req,
            200,
            json!({ "ok": true, "appid": aid, "has_fix": true }),
        );
        return;
    }

    // Route: /info/:appid
    if parts.first() == Some(&"info") {
        let exists = aid > 0 && stplug_dir.join(format!("{}.lua", aid)).exists();
        send_json(
            req,
            200,
            json!({ "ok": true, "appid": aid, "installed": exists }),
        );
        return;
    }

    // Route: /api/givemethemanifestpunk (LT-44)
    if parts == ["api", "givemethemanifestpunk"] || parts == ["givemethemanifestpunk"] {
        let depot_id = parts
            .iter()
            .find_map(|p| p.parse::<u32>().ok())
            .or_else(|| extract_query_appid(query_str))
            .or_else(|| extract_query_param(query_str, "depotid").and_then(|v| v.parse::<u32>().ok()))
            .or_else(|| extract_query_param(query_str, "depot_id").and_then(|v| v.parse::<u32>().ok()))
            .unwrap_or(0);

        let gid = extract_query_param(query_str, "gid")
            .or_else(|| extract_query_param(query_str, "manifestid"))
            .or_else(|| extract_query_param(query_str, "manifest_id"))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        if depot_id > 0 && gid > 0 {
            if let Some(home) = dirs::home_dir() {
                let cache_candidates = [
                    home.join(format!(".local/share/luatools/depotcache/{}_{}.manifest", depot_id, gid)),
                    home.join(format!(".local/share/Steam/depotcache/{}_{}.manifest", depot_id, gid)),
                    stplug_dir.join(format!("{}_{}.manifest", depot_id, gid)),
                ];
                for p in cache_candidates {
                    if let Ok(b) = std::fs::read(&p) {
                        send_bytes(req, 200, "application/octet-stream", b);
                        return;
                    }
                }
            }

            if let Ok(b) = crate::services::api_client::HubcapService::fetch_raw_manifest(depot_id, gid, None) {
                send_bytes(req, 200, "application/octet-stream", b);
                return;
            }
        }

        send_json(
            req,
            200,
            json!({ "ok": true, "depot_id": depot_id, "gid": gid, "cached": false }),
        );
        return;
    }

    // Route: /manifest/:depotid/:gid or /api/manifest/:depotid/:gid (LT-44)
    if (parts.len() >= 3 && parts[0] == "manifest")
        || (parts.len() >= 4 && parts[0] == "api" && parts[1] == "manifest")
    {
        let offset = if parts[0] == "api" { 2 } else { 1 };
        let depot_id = parts[offset].parse::<u32>().unwrap_or(0);
        let gid = parts.get(offset + 1).and_then(|g| g.parse::<u64>().ok()).unwrap_or(0);

        if depot_id > 0 && gid > 0 {
            if let Some(home) = dirs::home_dir() {
                let cache_candidates = [
                    home.join(format!(".local/share/luatools/depotcache/{}_{}.manifest", depot_id, gid)),
                    home.join(format!(".local/share/Steam/depotcache/{}_{}.manifest", depot_id, gid)),
                    stplug_dir.join(format!("{}_{}.manifest", depot_id, gid)),
                ];
                for p in cache_candidates {
                    if let Ok(b) = std::fs::read(&p) {
                        send_bytes(req, 200, "application/octet-stream", b);
                        return;
                    }
                }
            }

            if let Ok(b) = crate::services::api_client::HubcapService::fetch_raw_manifest(depot_id, gid, None) {
                send_bytes(req, 200, "application/octet-stream", b);
                return;
            }
        }

        send_json(
            req,
            200,
            json!({ "ok": true, "depot_id": depot_id, "gid": gid, "cached": false }),
        );
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

    #[test]
    fn handles_cors_preflight_and_headers() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::channel();
        let mut srv = HttpServerService::start(16780, dir.path().to_path_buf(), tx, None).unwrap();

        let resp = ureq::request("OPTIONS", "http://127.0.0.1:16780/rpc")
            .set("Origin", "https://store.steampowered.com")
            .set("Access-Control-Request-Method", "POST")
            .set("Access-Control-Request-Headers", "Content-Type, X-Requested-With")
            .call()
            .unwrap();
        assert_eq!(resp.status(), 204);
        assert_eq!(resp.header("Access-Control-Allow-Origin"), Some("*"));
        assert!(resp.header("Access-Control-Allow-Methods").unwrap().contains("OPTIONS"));
        assert!(resp.header("Access-Control-Allow-Headers").unwrap().contains("Content-Type"));

        srv.stop();
    }

    #[test]
    fn handles_store_plugin_and_extended_routes() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let mut srv = HttpServerService::start(16781, dir.path().to_path_buf(), tx, None).unwrap();

        // 1. /has
        let has1: Value = ureq::get("http://127.0.0.1:16781/has/1245620").call().unwrap().into_json().unwrap();
        assert_eq!(has1["has"], false);

        let has2: Value = ureq::get("http://127.0.0.1:16781/has?appid=1245620").call().unwrap().into_json().unwrap();
        assert_eq!(has2["has"], false);

        // 2. /add
        let add1: Value = ureq::get("http://127.0.0.1:16781/add/1245620").call().unwrap().into_json().unwrap();
        assert_eq!(add1["queued"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::AddApp(1245620)));

        // Create a dummy .lua
        std::fs::write(dir.path().join("1245620.lua"), "addappid(1245620)").unwrap();
        let has3: Value = ureq::get("http://127.0.0.1:16781/has/1245620").call().unwrap().into_json().unwrap();
        assert_eq!(has3["has"], true);

        // 3. /apps / /installed
        let apps: Value = ureq::get("http://127.0.0.1:16781/apps").call().unwrap().into_json().unwrap();
        assert_eq!(apps["apps"], json!([1245620]));

        // 4. /remove
        let rem: Value = ureq::get("http://127.0.0.1:16781/remove/1245620").call().unwrap().into_json().unwrap();
        assert_eq!(rem["removed"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::RemovedApp(1245620)));

        // 5. /open/fix and /open/settings
        let fix: Value = ureq::get("http://127.0.0.1:16781/open/fix/1245620").call().unwrap().into_json().unwrap();
        assert_eq!(fix["ok"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::OpenFix(1245620)));

        let set: Value = ureq::get("http://127.0.0.1:16781/open/settings").call().unwrap().into_json().unwrap();
        assert_eq!(set["ok"], true);
        assert!(matches!(rx.try_recv().unwrap(), RpcUiEvent::OpenSettings));

        // 6. Extended routes: /version, /status, /config, /info/1245620
        let ver: Value = ureq::get("http://127.0.0.1:16781/version").call().unwrap().into_json().unwrap();
        assert_eq!(ver["ok"], true);

        let st: Value = ureq::get("http://127.0.0.1:16781/status").call().unwrap().into_json().unwrap();
        assert_eq!(st["status"], "running");

        let cfg: Value = ureq::get("http://127.0.0.1:16781/config").call().unwrap().into_json().unwrap();
        assert_eq!(cfg["ok"], true);

        let inf: Value = ureq::get("http://127.0.0.1:16781/info/1245620").call().unwrap().into_json().unwrap();
        assert_eq!(inf["ok"], true);

        // 7. Raw manifest routes: /manifest/:depotid/:gid & /api/givemethemanifestpunk
        std::fs::write(dir.path().join("1245621_987654321.manifest"), b"MANIFEST_BYTES_SAMPLE").unwrap();
        let mf_resp = ureq::get("http://127.0.0.1:16781/manifest/1245621/987654321").call().unwrap();
        assert_eq!(mf_resp.status(), 200);
        let mut mf_bytes = Vec::new();
        mf_resp.into_reader().read_to_end(&mut mf_bytes).unwrap();
        assert_eq!(mf_bytes, b"MANIFEST_BYTES_SAMPLE");

        let punk_resp = ureq::get("http://127.0.0.1:16781/api/givemethemanifestpunk?depotid=1245621&gid=987654321").call().unwrap();
        assert_eq!(punk_resp.status(), 200);
        let mut punk_bytes = Vec::new();
        punk_resp.into_reader().read_to_end(&mut punk_bytes).unwrap();
        assert_eq!(punk_bytes, b"MANIFEST_BYTES_SAMPLE");

        srv.stop();
    }
}
