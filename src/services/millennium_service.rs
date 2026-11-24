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

    pub fn config_paths(steam_root: Option<&std::path::Path>) -> Vec<std::path::PathBuf> {
        let mut paths = Vec::new();
        if let Some(sr) = steam_root {
            paths.push(sr.join("ext/config.json"));
            paths.push(sr.join("millennium/config/config.json"));
            paths.push(sr.join("millennium/config.json"));
        }
        if let Some(h) = dirs::home_dir() {
            paths.push(h.join(".local/share/Steam/ext/config.json"));
            paths.push(h.join(".local/share/Steam/millennium/config/config.json"));
            paths.push(h.join(".local/share/millennium/config/config.json"));
        }
        if let Ok(env) = std::env::var("MILLENNIUM__CONFIG_PATH") {
            paths.push(std::path::PathBuf::from(env).join("config.json"));
        }
        paths
    }

    pub fn register_luatools_plugin(steam_root: Option<&std::path::Path>) -> bool {
        let mut registered_any = false;
        for path in Self::config_paths(steam_root) {
            if !path.exists() {
                // If ext directory exists or steam root exists, ensure ext/config.json can be created
                if let Some(parent) = path.parent() {
                    if parent.exists() && path.file_name().and_then(|n| n.to_str()) == Some("config.json") {
                        let default_cfg = json!({
                            "plugins": {
                                "enabledPlugins": ["luatools"]
                            }
                        });
                        if std::fs::write(&path, serde_json::to_string_pretty(&default_cfg).unwrap_or_default()).is_ok() {
                            registered_any = true;
                        }
                    }
                }
                continue;
            }

            if let Ok(raw) = std::fs::read_to_string(&path) {
                if let Ok(mut val) = serde_json::from_str::<Value>(&raw) {
                    let mut modified = false;
                    if let Some(plugins) = val.get_mut("plugins").and_then(|p| p.as_object_mut()) {
                        let arr = plugins.entry("enabledPlugins").or_insert_with(|| json!([]));
                        if let Some(list) = arr.as_array_mut() {
                            let has = list.iter().any(|v| {
                                v.as_str().map(|s| s.split_whitespace().next() == Some("luatools")).unwrap_or(false)
                            });
                            if !has {
                                list.push(json!("luatools"));
                                modified = true;
                            }
                        }
                    } else if let Some(obj) = val.as_object_mut() {
                        obj.insert("plugins".into(), json!({ "enabledPlugins": ["luatools"] }));
                        modified = true;
                    }

                    if modified {
                        if let Ok(js) = serde_json::to_string_pretty(&val) {
                            if std::fs::write(&path, js).is_ok() {
                                registered_any = true;
                            }
                        }
                    } else {
                        registered_any = true;
                    }
                }
            }
        }
        registered_any
    }

}
