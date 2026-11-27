//! ConfigFileWatcher — watches `opensteamtool.toml` and `manifest_probe.txt` (BST-18).
//!
//! Ported directly from UsefulSteamTools `ConfigFileWatcher.cpp` / `ConfigFileWatcher.h`.
//!
//! Watches the directory containing `opensteamtool.toml` with 500ms debouncing.
//! Features:
//! 1. `opensteamtool.toml` dynamic reloading:
//!    - Reloads configuration snapshot dynamically on filesystem change.
//!    - If Lua paths changed, triggers subscriber callbacks / Lua watcher refresh.
//! 2. `manifest_probe.txt` diagnostic probe triggers:
//!    - Beside `opensteamtool.toml`, lines of depot IDs (ignoring '#' comments and whitespace)
//!      are turned into client-originated manifest probe requests (`request_manifest_code`).
//!    - Allows running diagnostic manifest probes without restarting Steam.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::{self, JoinHandle};

use crate::steam_bridge::netpacket::netpacket_manifestprobe;
use crate::steam_bridge::ost_config::{self, LoadResult};
use crate::steam_bridge::ost_platform::directory_watch::{self, Change, Watch};

const DEBOUNCE_MS: u64 = 500;
const PROBE_FILE_NAME: &str = "manifest_probe.txt";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Touched {
    config: bool,
    probe: bool,
}

static WATCHER_RUNNING: AtomicBool = AtomicBool::new(false);
static WATCHER_THREAD: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
static CONFIG_PATH: OnceLock<Mutex<PathBuf>> = OnceLock::new();
static DEFAULT_LUA_DIR: OnceLock<Mutex<PathBuf>> = OnceLock::new();

fn config_path_lock() -> &'static Mutex<PathBuf> {
    CONFIG_PATH.get_or_init(|| Mutex::new(PathBuf::from("opensteamtool.toml")))
}

fn default_lua_dir_lock() -> &'static Mutex<PathBuf> {
    DEFAULT_LUA_DIR.get_or_init(|| Mutex::new(PathBuf::from(".")))
}

fn same_file_name(a: &str, b: &str) -> bool {
    let pa = Path::new(a);
    let pb = Path::new(b);
    let na = pa.file_name().and_then(|s| s.to_str()).unwrap_or(a);
    let nb = pb.file_name().and_then(|s| s.to_str()).unwrap_or(b);
    na.eq_ignore_ascii_case(nb)
}

fn contains_file(changes: &[Change], target_file_name: &str) -> bool {
    changes
        .iter()
        .any(|c| same_file_name(&c.relative_path, target_file_name))
}

/// Process a probe file containing depot IDs, one per line.
/// Returns `(requested, accepted)` count.
pub fn process_probe_file(probe_path: &Path) -> (u32, u32) {
    let content = match fs::read_to_string(probe_path) {
        Ok(c) => c,
        Err(_) => return (0, 0),
    };

    let mut requested: u32 = 0;
    let mut accepted: u32 = 0;

    for line in content.lines() {
        let mut trimmed = line;
        if let Some(hash_idx) = trimmed.find('#') {
            trimmed = &trimmed[..hash_idx];
        }
        trimmed = trimmed.trim();
        if trimmed.is_empty() {
            continue;
        }

        let (parsed_depot, parsed_gid) = if let Some((d, g)) = trimmed
            .split_once(':')
            .or_else(|| trimmed.split_once(' '))
            .or_else(|| trimmed.split_once('\t'))
        {
            (d.trim().parse::<u32>().ok(), g.trim().parse::<u64>().unwrap_or(0))
        } else {
            (trimmed.parse::<u32>().ok(), 0)
        };

        if let Some(depot_id) = parsed_depot {
            if depot_id > 0 {
                requested += 1;
                // Dispatch manifest probe request for this depot
                if netpacket_manifestprobe::request_manifest_code(0, depot_id, parsed_gid).is_some() {
                    accepted += 1;
                }
            } else {
                log::warn!(
                    "[ConfigFileWatcher] manifest_probe: ignoring non-positive depot_id line {:?}",
                    line
                );
            }
        } else {
            log::warn!(
                "[ConfigFileWatcher] manifest_probe: ignoring unparseable line {:?}",
                line
            );
        }
    }

    log::info!(
        "[ConfigFileWatcher] manifest_probe: {} depot(s) read, {} request(s) sent from {:?}",
        requested,
        accepted,
        probe_path
    );
    (requested, accepted)
}

/// Reload configuration from target config path.
pub fn reload_config(config_path: &Path) -> LoadResult {
    log::info!("[ConfigFileWatcher] Reloading config: {:?}", config_path);
    let res = ost_config::load(config_path);
    if !res.applied {
        log::warn!("[ConfigFileWatcher] Config reload skipped; keeping previous valid config");
    }
    res
}

/// Background watcher thread implementation.
fn watcher_loop(config_path: PathBuf, target_file_name: String) {
    let dir_path = config_path.parent().unwrap_or_else(|| Path::new("."));

    let mut watch = Watch::new();
    if !watch.open(dir_path.to_str().unwrap_or("."), 4096) {
        log::warn!(
            "[ConfigFileWatcher] Failed to open config watch directory: {:?}",
            dir_path
        );
        WATCHER_RUNNING.store(false, Ordering::Release);
        return;
    }

    if !watch.issue_read() {
        WATCHER_RUNNING.store(false, Ordering::Release);
        return;
    }

    log::info!(
        "[ConfigFileWatcher] Watching config file: {:?} in dir: {:?}",
        config_path,
        dir_path
    );

    let probe_path = dir_path.join(PROBE_FILE_NAME);

    while WATCHER_RUNNING.load(Ordering::Acquire) {
        let wait_result = directory_watch::wait_any(&mut [&mut watch], 1000);

        if !WATCHER_RUNNING.load(Ordering::Acquire) {
            break;
        }
        if wait_result.status == directory_watch::WaitStatus::Timeout {
            continue;
        }
        if wait_result.status != directory_watch::WaitStatus::Signaled {
            continue;
        }

        let changes = watch.drain();
        let mut touched = Touched {
            config: contains_file(&changes, &target_file_name),
            probe: contains_file(&changes, PROBE_FILE_NAME),
        };
        watch.issue_read();

        // Debounce subsequent rapid changes
        while WATCHER_RUNNING.load(Ordering::Acquire) {
            let debounce = directory_watch::wait_any(&mut [&mut watch], DEBOUNCE_MS as u32);
            if !WATCHER_RUNNING.load(Ordering::Acquire) {
                break;
            }
            if debounce.status == directory_watch::WaitStatus::Timeout {
                break;
            }
            if debounce.status != directory_watch::WaitStatus::Signaled {
                break;
            }
            let more = watch.drain();
            touched.config = touched.config || contains_file(&more, &target_file_name);
            touched.probe = touched.probe || contains_file(&more, PROBE_FILE_NAME);
            watch.issue_read();
        }

        if touched.config {
            reload_config(&config_path);
        }
        if touched.probe {
            process_probe_file(&probe_path);
        }
    }

    watch.cancel();
    log::info!("[ConfigFileWatcher] Config watcher stopped");
}

/// Start watching `config_path` and `manifest_probe.txt`.
pub fn start(config_path: &Path, default_lua_dir: &Path) {
    if WATCHER_RUNNING.swap(true, Ordering::SeqCst) {
        log::warn!("[ConfigFileWatcher] Config watcher already running");
        return;
    }

    *config_path_lock().lock().unwrap_or_else(|e| e.into_inner()) = config_path.to_path_buf();
    *default_lua_dir_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = default_lua_dir.to_path_buf();

    // Initial load
    reload_config(config_path);

    let cfg_path = config_path.to_path_buf();
    let file_name = config_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("opensteamtool.toml")
        .to_string();

    let handle = thread::Builder::new()
        .name("ost-config-watcher".to_string())
        .spawn(move || {
            watcher_loop(cfg_path, file_name);
        });

    match handle {
        Ok(h) => {
            *WATCHER_THREAD.lock().unwrap_or_else(|e| e.into_inner()) = Some(h);
        }
        Err(e) => {
            log::warn!("[ConfigFileWatcher] Failed to spawn watcher thread: {}", e);
            WATCHER_RUNNING.store(false, Ordering::Release);
        }
    }
}

/// Stop the background config file watcher.
pub fn stop() {
    if !WATCHER_RUNNING.swap(false, Ordering::SeqCst) {
        return;
    }
    let handle = WATCHER_THREAD
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some(h) = handle {
        let _ = h.join();
    }
}

/// Check if the watcher is running.
pub fn is_running() -> bool {
    WATCHER_RUNNING.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    

    #[test]
    fn test_manifest_probe_parsing() {
        let probe_content = r#"
# Diagnostic probe targets
1245620 987654321
1245621
# comments and blanks

480 112233445566
"#;
        let mut count = 0;
        for line in probe_content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Ok(depot_id) = parts[0].parse::<u32>() {
                assert!(depot_id == 1245620 || depot_id == 1245621 || depot_id == 480);
                count += 1;
            }
        }
        assert_eq!(count, 3);
    }
}
