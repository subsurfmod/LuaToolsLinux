//! PipeManager::Injection conditional runtime library injector (BST-61).
//!
//! Ported directly from usefulsteamtools `Injection.cpp` / `Injection.h`.
//! Evaluates `[[inject]]` rules configured in `opensteamtool.toml` on pipe handshakes,
//! claims deduplicated injections per `(pid, library_path)`, and injects shared libraries
//! into child game processes via `OSTPlatform::RemoteProcess::inject_library`.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use crate::steam_bridge::ost_config::{InjectDll, OstConfig};
use crate::steam_bridge::ost_platform::remote_process::{inject_library, InjectStatus};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct InjectedKey {
    pid: u32,
    path: String,
}

static INJECTED: Mutex<Option<HashSet<InjectedKey>>> = Mutex::new(None);

pub fn claim_injection(pid: u32, path: &str) -> bool {
    let mut guard = INJECTED.lock().unwrap_or_else(|e| e.into_inner());
    let set = guard.get_or_insert_with(HashSet::new);
    set.insert(InjectedKey {
        pid,
        path: path.to_string(),
    })
}

pub fn clear_injection_history() {
    let mut guard = INJECTED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(ref mut set) = *guard {
        set.clear();
    }
}

/// Check if an `[[inject]]` rule matches the current process context.
pub fn matches_rule(
    rule: &InjectDll,
    app_id: u32,
    is_tracked_app: bool,
    cmdline: Option<&str>,
) -> bool {
    // If not all_games, only inject into tracked (Lua-unlocked) apps
    if !rule.all_games && !is_tracked_app {
        return false;
    }
    // Whitelist check
    if !rule.when_appids.is_empty() && !rule.when_appids.contains(&app_id) {
        return false;
    }
    // Substring match on remote command line
    if !rule.when_cmdline.is_empty() {
        match cmdline {
            Some(cmd) if cmd.contains(&rule.when_cmdline) => {}
            _ => return false,
        }
    }
    true
}

/// Apply configured `[[inject]]` DLL/SO rules on process pipe handshake (BST-61).
pub fn apply_injection_rules(app_id: u32, pid: u32, is_tracked_app: bool) {
    if pid == 0 {
        return;
    }

    let config = OstConfig::active();
    if config.inject_dlls.is_empty() {
        return;
    }

    // Lazily read command line only if at least one rule checks when_cmdline
    let mut cmdline: Option<String> = None;
    let mut cmdline_read = false;

    for rule in &config.inject_dlls {
        let cmd_ref = if rule.when_cmdline.is_empty() {
            None
        } else {
            if !cmdline_read {
                let cmd_path = format!("/proc/{}/cmdline", pid);
                cmdline = fs::read(&cmd_path).ok().map(|bytes| {
                    bytes
                        .split(|&b| b == 0)
                        .filter(|p| !p.is_empty())
                        .map(|p| String::from_utf8_lossy(p))
                        .collect::<Vec<_>>()
                        .join(" ")
                });
                cmdline_read = true;
            }
            cmdline.as_deref()
        };

        if !matches_rule(rule, app_id, is_tracked_app, cmd_ref) {
            continue;
        }

        if !claim_injection(pid, &rule.path) {
            log::debug!(
                "[injection] skipped duplicate injection pid={} path=\"{}\"",
                pid,
                rule.path
            );
            continue;
        }

        let lib_path = Path::new(&rule.path);
        let status = inject_library(pid, lib_path);
        if status == InjectStatus::Ok {
            log::info!(
                "[injection] injected pid={} appid={} lib=\"{}\"",
                pid,
                app_id,
                lib_path.display()
            );
        } else {
            log::warn!(
                "[injection] inject failed pid={} appid={} status={:?} lib=\"{}\"",
                pid,
                app_id,
                status,
                lib_path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_rule_logic() {
        let rule_default = InjectDll {
            path: "/tmp/test.so".into(),
            when_cmdline: String::new(),
            when_appids: vec![],
            all_games: false,
        };
        // Untracked app rejected if all_games=false
        assert!(!matches_rule(&rule_default, 1245620, false, None));
        assert!(matches_rule(&rule_default, 1245620, true, None));

        let rule_all = InjectDll {
            all_games: true,
            ..rule_default.clone()
        };
        assert!(matches_rule(&rule_all, 1245620, false, None));

        let rule_appids = InjectDll {
            when_appids: vec![1245620, 730],
            all_games: true,
            ..rule_default.clone()
        };
        assert!(matches_rule(&rule_appids, 1245620, false, None));
        assert!(!matches_rule(&rule_appids, 480, false, None));

        let rule_cmd = InjectDll {
            when_cmdline: "-onlinefix".into(),
            all_games: true,
            ..rule_default.clone()
        };
        assert!(!matches_rule(&rule_cmd, 1245620, true, None));
        assert!(!matches_rule(&rule_cmd, 1245620, true, Some("game.exe -other")));
        assert!(matches_rule(&rule_cmd, 1245620, true, Some("game.exe -onlinefix -debug")));
    }

    #[test]
    fn test_claim_injection_deduplication() {
        clear_injection_history();
        assert!(claim_injection(4242, "/tmp/lib1.so"));
        assert!(!claim_injection(4242, "/tmp/lib1.so"));
        // Same process, different library -> ok
        assert!(claim_injection(4242, "/tmp/lib2.so"));
        // Different process, same library -> ok
        assert!(claim_injection(9999, "/tmp/lib1.so"));
    }
}
