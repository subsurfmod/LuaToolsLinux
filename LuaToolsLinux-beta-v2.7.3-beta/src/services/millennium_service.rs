//! MillenniumService — client for Millennium's RPC bridge on `127.0.0.1:38495`.

use std::time::Duration;

use serde_json::{json, Value};

pub const MILLENNIUM_RPC_URL: &str = "http://127.0.0.1:38495/rpc";

#[derive(Debug, Clone)]
pub struct MillenniumService {
    pub base_url: String,
}

impl Default for MillenniumService {
    fn default() -> Self {
        Self {
            base_url: MILLENNIUM_RPC_URL.to_string(),
        }
    }
}

impl MillenniumService {
    pub fn is_available(&self) -> bool {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(2))
            .build();
        agent
            .post(&self.base_url)
            .send_json(json!({ "method": "Ping", "args": {} }))
            .map(|r| r.status() == 200)
            .unwrap_or(false)
    }

    pub fn call(&self, method: &str, args: Value) -> Value {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(8))
            .build();
        match agent
            .post(&self.base_url)
            .send_json(json!({ "method": method, "args": args }))
        {
            Ok(resp) => resp
                .into_json::<Value>()
                .unwrap_or_else(|e| json!({ "success": false, "error": e.to_string() })),
            Err(e) => json!({ "success": false, "error": e.to_string() }),
        }
    }

    pub fn get_version(&self) -> Option<String> {
        let r = self.call("version", json!({}));
        r.get("success")
            .and_then(|v| v.as_bool())
            .filter(|&ok| ok)
            .and_then(|_| r.get("result"))
            .and_then(|v| v.as_str())
            .map(ToString::to_string)
    }

    pub fn get_steam_path(&self) -> Option<String> {
        let r = self.call("steam_path", json!({}));
        r.get("success")
            .and_then(|v| v.as_bool())
            .filter(|&ok| ok)
            .and_then(|_| r.get("result"))
            .and_then(|v| v.as_str())
            .map(ToString::to_string)
    }

    pub fn restart_steam(&self) -> Result<(), String> {
        let r = self.call("restart_steam", json!({}));
        if r.get("success").and_then(|v| v.as_bool()) == Some(true) {
            Ok(())
        } else {
            Err(r
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("Millennium RPC unreachable")
                .to_string())
        }
    }
}
