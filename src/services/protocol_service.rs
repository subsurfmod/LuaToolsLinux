//! ProtocolService — parses `luatools://` URIs and registers `luatools.desktop` via `xdg-mime`.

use std::fs;
use std::process::Command;

use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolAction {
    pub kind: String, // "game" | "install" | "manage" | "fix"
    pub app_id: u32,
    pub source: Option<String>,
    pub silent: bool,
}

pub struct ProtocolService;

impl ProtocolService {
    pub fn parse(raw_url: &str) -> Option<ProtocolAction> {
        let trimmed = raw_url.trim();
        let trimmed_lower = trimmed.to_ascii_lowercase();
        let is_luatools = trimmed_lower.starts_with("luatools://");
        let is_bst = trimmed_lower.starts_with("bst://");
        if !is_luatools && !is_bst {
            return None;
        }

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

        let (kind, app_id) = if matches!(host_lower.as_str(), "game" | "install" | "manage" | "fix") {
            let mut aid = None;
            for seg in &segments {
                if seg.eq_ignore_ascii_case("silent") {
                    silent = true;
                } else if aid.is_none() {
                    if let Ok(id) = seg.parse::<u32>() {
                        aid = Some(id);
                    }
                }
            }
            (host_lower, aid?)
        } else if let Ok(aid) = host_lower.parse::<u32>() {
            for seg in &segments {
                if seg.eq_ignore_ascii_case("silent") {
                    silent = true;
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

        let mut source = parsed
            .query_pairs()
            .find(|(k, _)| k == "source")
            .map(|(_, v)| v.to_string());

        if source.is_none() && is_bst {
            source = Some("bst".to_string());
        }

        Some(ProtocolAction {
            kind,
            app_id,
            source,
            silent,
        })
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
        assert!(!b.silent);

        let c = ProtocolService::parse("bst://install/silent/730").unwrap();
        assert_eq!(c.kind, "install");
        assert_eq!(c.app_id, 730);
        assert!(c.silent);

        let d = ProtocolService::parse("bst://fix/480").unwrap();
        assert_eq!(d.kind, "fix");
        assert_eq!(d.app_id, 480);
    }
}
