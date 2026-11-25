//! ProtocolService — parses `luatools://` URIs and registers `luatools.desktop` via `xdg-mime`.

use std::collections::HashMap;
use std::fs;
use std::ops::Deref;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use url::Url;

use crate::steam_bridge::token_bridge::{BstUri, TokenBridge};

fn shared_token_bridge() -> &'static TokenBridge {
    static BRIDGE: OnceLock<TokenBridge> = OnceLock::new();
    BRIDGE.get_or_init(|| {
        let mut bridge = TokenBridge::new();
        bridge.register_handler(Box::new(|uri: &BstUri| {
            if uri.is_valid() || !uri.token.is_empty() || uri.app_id > 0 {
                Some(uri.clone())
            } else {
                None
            }
        }));
        bridge
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProtocolActionExt {
    pub token: Option<String>,
    pub redeem_code: Option<String>,
}

type ExtKey = (String, u32, Option<String>, bool);

fn ext_store() -> &'static Mutex<HashMap<ExtKey, &'static ProtocolActionExt>> {
    static STORE: OnceLock<Mutex<HashMap<ExtKey, &'static ProtocolActionExt>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

static EMPTY_EXT: ProtocolActionExt = ProtocolActionExt {
    token: None,
    redeem_code: None,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolAction {
    pub kind: String, // "game" | "install" | "manage" | "fix"
    pub app_id: u32,
    pub source: Option<String>,
    pub silent: bool,
}

impl ProtocolAction {
    fn record_ext(&self, token: Option<String>, redeem_code: Option<String>) {
        let key = (
            self.kind.clone(),
            self.app_id,
            self.source.clone(),
            self.silent,
        );
        let leaked: &'static ProtocolActionExt = Box::leak(Box::new(ProtocolActionExt {
            token,
            redeem_code,
        }));
        if let Ok(mut map) = ext_store().lock() {
            map.insert(key, leaked);
        }
    }
}

impl Deref for ProtocolAction {
    type Target = ProtocolActionExt;

    fn deref(&self) -> &Self::Target {
        let key = (
            self.kind.clone(),
            self.app_id,
            self.source.clone(),
            self.silent,
        );
        if let Ok(map) = ext_store().lock() {
            if let Some(&ext) = map.get(&key) {
                return ext;
            }
        }
        &EMPTY_EXT
    }
}

pub struct ProtocolService;

impl ProtocolService {
    /// Routes a raw `bst://` URI through the shared `TokenBridge` dispatcher.
    pub fn dispatch_bst(raw_url: &str) -> Option<BstUri> {
        shared_token_bridge().handle_uri(raw_url)
    }

    pub fn parse(raw_url: &str) -> Option<ProtocolAction> {
        let trimmed = raw_url.trim();
        let trimmed_lower = trimmed.to_ascii_lowercase();
        let is_luatools = trimmed_lower.starts_with("luatools://");
        let is_bst = trimmed_lower.starts_with("bst://");
        if !is_luatools && !is_bst {
            return None;
        }

        let bst_dispatched = if is_bst {
            Self::dispatch_bst(trimmed)
        } else {
            None
        };

        let parsed = Url::parse(trimmed).ok()?;
        let host_raw = parsed.host_str().unwrap_or("");
        let host_lower = host_raw.to_ascii_lowercase();

        let segments: Vec<&str> = parsed
            .path_segments()
            .map(|s| s.filter(|seg| !seg.is_empty()).collect())
            .unwrap_or_default();

        let mut silent = parsed.query_pairs().any(|(k, v)| {
            k.eq_ignore_ascii_case("silent")
                && (v == "1" || v.eq_ignore_ascii_case("true") || v.is_empty())
        });

        if host_lower == "redeem" {
            let code = segments.first()?.trim();
            if code.is_empty() {
                return None;
            }
            let act = ProtocolAction {
                kind: "game".to_string(),
                app_id: 0,
                source: None,
                silent,
            };
            act.record_ext(None, Some(code.to_string()));
            return Some(act);
        }

        let mut token: Option<String> = None;

        let (kind, app_id) = if matches!(host_lower.as_str(), "game" | "install" | "manage" | "fix") {
            let mut aid = None;
            for seg in &segments {
                if seg.eq_ignore_ascii_case("silent") {
                    silent = true;
                } else if aid.is_none() {
                    if let Ok(id) = seg.parse::<u32>() {
                        aid = Some(id);
                    }
                } else if token.is_none() {
                    token = Some((*seg).to_string());
                }
            }
            (host_lower, aid?)
        } else if let Some(bst) = bst_dispatched.as_ref().filter(|b| b.app_id > 0) {
            if bst.token.eq_ignore_ascii_case("silent")
                || bst.params.get("silent").is_some_and(|v| {
                    v == "1" || v.eq_ignore_ascii_case("true") || v.is_empty()
                })
            {
                silent = true;
            }
            if !bst.token.is_empty() && !bst.token.eq_ignore_ascii_case("silent") {
                token = Some(bst.token.clone());
            } else {
                for seg in &segments {
                    if seg.eq_ignore_ascii_case("silent") {
                        silent = true;
                    } else if token.is_none() {
                        token = Some((*seg).to_string());
                    }
                }
            }
            let q_kind = bst
                .params
                .get("kind")
                .or_else(|| bst.params.get("action"))
                .map(|v| v.to_ascii_lowercase())
                .unwrap_or_else(|| "install".to_string());
            (q_kind, bst.app_id)
        } else if let Ok(aid) = host_lower.parse::<u32>() {
            for seg in &segments {
                if seg.eq_ignore_ascii_case("silent") {
                    silent = true;
                } else if token.is_none() {
                    token = Some((*seg).to_string());
                }
            }
            let q_kind = parsed
                .query_pairs()
                .find(|(k, _)| k == "kind" || k == "action")
                .map(|(_, v)| v.to_ascii_lowercase())
                .unwrap_or_else(|| "install".to_string());
            (q_kind, aid)
        } else {
            return None;
        };

        if token.is_none() {
            token = bst_dispatched
                .as_ref()
                .and_then(|b| b.params.get("token"))
                .filter(|s| !s.is_empty())
                .cloned()
                .or_else(|| {
                    parsed
                        .query_pairs()
                        .find(|(k, _)| k.eq_ignore_ascii_case("token"))
                        .map(|(_, v)| v.to_string())
                        .filter(|s| !s.is_empty())
                });
        }

        let mut source = bst_dispatched
            .as_ref()
            .and_then(|b| b.params.get("source").cloned())
            .or_else(|| {
                parsed
                    .query_pairs()
                    .find(|(k, _)| k == "source")
                    .map(|(_, v)| v.to_string())
            });

        if source.is_none() && is_bst {
            source = Some("bst".to_string());
        }

        let act = ProtocolAction {
            kind,
            app_id,
            source,
            silent,
        };
        act.record_ext(token, None);
        Some(act)
    }

    pub fn register_desktop_handler() -> Result<String, String> {
        let home = dirs::home_dir().ok_or_else(|| "Cannot resolve HOME directory.".to_string())?;
        let apps_dir = home.join(".local/share/applications");
        fs::create_dir_all(&apps_dir).map_err(|e| e.to_string())?;

        let exe = std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "luatools".to_string());

        let desktop_file = apps_dir.join("luatools.desktop");
        let content = format!(
            "[Desktop Entry]\n\
             Version=1.0\n\
             Name=LuaTools Linux\n\
             Comment=Steam manifest and lua configuration manager\n\
             Exec=\"{}\" %u\n\
             Icon=luatools\n\
             Terminal=false\n\
             Type=Application\n\
             Categories=Game;Utility;\n\
             MimeType=x-scheme-handler/luatools;x-scheme-handler/bst;\n",
            exe
        );

        fs::write(&desktop_file, content).map_err(|e| e.to_string())?;
        let _ = Command::new("xdg-mime")
            .args(["default", "luatools.desktop", "x-scheme-handler/luatools"])
            .output();
        let _ = Command::new("xdg-mime")
            .args(["default", "luatools.desktop", "x-scheme-handler/bst"])
            .output();
        let _ = Command::new("update-desktop-database")
            .arg(&apps_dir)
            .status();

        Ok(format!(
            "Registered luatools:// and bst:// handlers at {}",
            desktop_file.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_protocol_urls() {
        let a = ProtocolService::parse("luatools://install/1245620?source=morrenus").unwrap();
        assert_eq!(a.kind, "install");
        assert_eq!(a.app_id, 1245620);
        assert_eq!(a.source.as_deref(), Some("morrenus"));
        assert!(!a.silent);
    }

    #[test]
    fn parses_silent_protocol_urls() {
        let a = ProtocolService::parse("luatools://install/silent/1245620").unwrap();
        assert_eq!(a.kind, "install");
        assert_eq!(a.app_id, 1245620);
        assert!(a.silent);
        assert_eq!(a.source, None);

        let b = ProtocolService::parse("luatools://install/silent/1245620?source=morrenus").unwrap();
        assert_eq!(b.kind, "install");
        assert_eq!(b.app_id, 1245620);
        assert_eq!(b.source.as_deref(), Some("morrenus"));
        assert!(b.silent);

        let c = ProtocolService::parse("luatools://install/1245620?silent=1").unwrap();
        assert_eq!(c.kind, "install");
        assert_eq!(c.app_id, 1245620);
        assert!(c.silent);
    }

    #[test]
    fn parses_bst_protocol_urls() {
        let a = ProtocolService::parse("bst://install/1245620").unwrap();
        assert_eq!(a.kind, "install");
        assert_eq!(a.app_id, 1245620);
        assert_eq!(a.source.as_deref(), Some("bst"));
        assert!(!a.silent);

        let b = ProtocolService::parse("bst://1245620/token?source=hubcap").unwrap();
        assert_eq!(b.kind, "install");
        assert_eq!(b.app_id, 1245620);
        assert_eq!(b.source.as_deref(), Some("hubcap"));
        assert_eq!(b.token.as_deref(), Some("token"));
        assert!(!b.silent);

        let c = ProtocolService::parse("bst://install/silent/730").unwrap();
        assert_eq!(c.kind, "install");
        assert_eq!(c.app_id, 730);
        assert!(c.silent);

        let d = ProtocolService::parse("bst://fix/480").unwrap();
        assert_eq!(d.kind, "fix");
        assert_eq!(d.app_id, 480);

        let e = ProtocolService::parse("bst://redeem/VIP-CODE-2026").unwrap();
        assert_eq!(e.kind, "game");
        assert_eq!(e.app_id, 0);
        assert_eq!(e.source, None);
        assert!(!e.silent);
        assert_eq!(e.token, None);
        assert_eq!(e.redeem_code.as_deref(), Some("VIP-CODE-2026"));

        let dispatched = ProtocolService::dispatch_bst("bst://1245620/my%2Ftok?source=hubcap&v=2")
            .expect("TokenBridge should dispatch valid bst:// URI");
        assert_eq!(dispatched.app_id, 1245620);
        assert_eq!(dispatched.token, "my/tok");
        assert_eq!(dispatched.version, 2);
        assert!(ProtocolService::dispatch_bst("luatools://install/1245620").is_none());
    }
}
