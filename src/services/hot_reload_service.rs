//! HotReloadService — watches `stplug-in/` via `ost_platform::directory_watch` for `.lua`
//! file changes, logs recent events for GUI inspection, and notifies `bridge_lib` over IPC.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use chrono::Utc;

use crate::parsers::lua_parser::LuaFileParser;
use crate::services::lua_installer::LuaInstaller;
use crate::services::slssteam_service::SlssteamService;
use crate::steam_bridge::ipc::{IpcClient, IpcMessage};
use crate::steam_bridge::ost_platform::directory_watch::{
    wait_any, ChangeAction, WaitStatus, Watch,
};

const MAX_RECENT_EVENTS: usize = 30;

/// Change event for a watched `.lua` file.
#[derive(Debug, Clone)]
pub struct FileChangeEvent {
    pub timestamp: String,
    pub path: PathBuf,
    pub event_type: FileChangeType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChangeType {
    Created,
    Modified,
    Deleted,
    Moved,
}

impl FileChangeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileChangeType::Created => "CREATED",
            FileChangeType::Modified => "MODIFIED",
            FileChangeType::Deleted => "DELETED",
            FileChangeType::Moved => "MOVED",
        }
    }
}

/// Service that watches `stplug-in/` for `.lua` changes in a non-blocking poll loop.
#[derive(Clone)]
pub struct HotReloadService {
    watch_dir: Arc<Mutex<PathBuf>>,
    running: Arc<AtomicBool>,
    thread_handle: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
    recent: Arc<Mutex<VecDeque<FileChangeEvent>>>,
    change_callback: Option<Arc<dyn Fn(FileChangeEvent) + Send + Sync>>,
    auto_sync_slssteam: Arc<AtomicBool>,
    sls_config_path: Arc<Mutex<Option<PathBuf>>>,
}

impl Default for HotReloadService {
    fn default() -> Self {
        Self::new()
    }
}

impl HotReloadService {
    pub fn new() -> Self {
        let default_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".local/share/Steam/config/stplug-in");
        Self {
            watch_dir: Arc::new(Mutex::new(default_dir)),
            running: Arc::new(AtomicBool::new(false)),
            thread_handle: Arc::new(Mutex::new(None)),
            recent: Arc::new(Mutex::new(VecDeque::new())),
            change_callback: None,
            auto_sync_slssteam: Arc::new(AtomicBool::new(!cfg!(test))),
            sls_config_path: Arc::new(Mutex::new(None)),
        }
    }

    /// Enables or disables automatic `SLSsteam` `config.yaml` re-sync on external `.lua` edits (LT-35).
    pub fn with_auto_slssteam_sync(self, enabled: bool) -> Self {
        self.auto_sync_slssteam.store(enabled, Ordering::SeqCst);
        self
    }

    /// Sets an explicit `SLSsteam` `config.yaml` path for automatic hot-reload sync (useful in tests).
    pub fn with_slssteam_config_path<P: AsRef<Path>>(self, path: P) -> Self {
        *self.sls_config_path.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(path.as_ref().to_path_buf());
        self
    }

    /// Re-syncs `SLSsteam` `config.yaml` in response to an external `.lua` create/modify/delete event (LT-35).
    /// Returns `true` if `SLSsteam` config was updated.
    pub fn sync_lua_event_to_slssteam(
        event: &FileChangeEvent,
        sls_config_path: Option<&Path>,
    ) -> bool {
        let Some(stem) = event.path.file_stem().and_then(|s| s.to_str()) else {
            return false;
        };
        let Ok(app_id) = stem.parse::<u32>() else {
            return false;
        };
        let mut sls = SlssteamService::new(sls_config_path.map(Path::to_path_buf));
        let config_dir = event
            .path
            .parent()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(crate::steam_bridge::ticket_store::TicketStore::default_path);

        match event.event_type {
            FileChangeType::Created | FileChangeType::Modified | FileChangeType::Moved => {
                if let Ok(text) = fs::read_to_string(&event.path) {
                    let parsed = LuaFileParser::parse_text(&text, app_id);
                    LuaInstaller::sync_with_slssteam_to(&mut sls, app_id, &parsed, &config_dir);
                    return true;
                }
                false
            }
            FileChangeType::Deleted => {
                if event.path.exists() {
                    return false;
                }
                sls.remove_additional_app(app_id);
                sls.remove_fake_app_id(app_id);
                sls.remove_token(app_id);
                sls.remove_cd_key(app_id);
                sls.remove_game_title(app_id);
                sls.remove_process_mapping(app_id);
                sls.remove_force_denuvo(app_id);
                sls.remove_pinned_app(app_id);
                sls.config.dlc_data.remove(&app_id);
                sls.save()
            }
        }
    }

    pub fn set_watch_dir<P: AsRef<Path>>(self, path: P) -> Self {
        *self.watch_dir.lock().unwrap() = path.as_ref().to_path_buf();
        self
    }

    pub fn update_watch_dir<P: AsRef<Path>>(&self, path: P) {
        let new_path = path.as_ref().to_path_buf();
        let changed = {
            let mut guard = self.watch_dir.lock().unwrap_or_else(|e| e.into_inner());
            if *guard != new_path {
                *guard = new_path;
                true
            } else {
                false
            }
        };
        if changed && self.is_running() {
            self.stop();
            let _ = self.start();
        }
    }

    pub fn watch_dir(&self) -> PathBuf {
        self.watch_dir.lock().unwrap().clone()
    }

    pub fn on_change<F>(mut self, callback: F) -> Self
    where
        F: Fn(FileChangeEvent) + Send + Sync + 'static,
    {
        self.change_callback = Some(Arc::new(callback));
        self
    }

    /// Starts watching `watch_dir` in a background thread.
    pub fn start(&self) -> Result<(), String> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Err("Hot-reload watcher is already running.".into());
        }

        let watch_dir = self.watch_dir();
        if !watch_dir.exists() {
            if let Err(e) = fs::create_dir_all(&watch_dir) {
                self.running.store(false, Ordering::SeqCst);
                return Err(format!("Failed to create {}: {}", watch_dir.display(), e));
            }
        }

        let mut watch = Watch::new();
        let watch_dir_str = watch_dir.to_string_lossy().to_string();
        if !watch.open(&watch_dir_str, 8192) {
            self.running.store(false, Ordering::SeqCst);
            return Err(format!("Failed to watch {}", watch_dir.display()));
        }

        let running = Arc::clone(&self.running);
        let recent = Arc::clone(&self.recent);
        let callback = self.change_callback.clone();
        let auto_sync = Arc::clone(&self.auto_sync_slssteam);
        let sls_cfg_path = Arc::clone(&self.sls_config_path);

        let handle = thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                let wait = wait_any(&mut [&mut watch], 50);
                if wait.status == WaitStatus::Failed {
                    break;
                }
                if !running.load(Ordering::Relaxed) {
                    break;
                }
                for change in watch.drain() {
                    if change.relative_path.is_empty() {
                        continue;
                    }
                    let path = watch_dir.join(&change.relative_path);
                    if path.extension().and_then(|e| e.to_str()) == Some("lua") {
                        let event_type = match change.action {
                            ChangeAction::Added => FileChangeType::Created,
                            ChangeAction::Removed => FileChangeType::Deleted,
                            ChangeAction::RenamedOldName | ChangeAction::RenamedNewName => {
                                FileChangeType::Moved
                            }
                            ChangeAction::Modified => FileChangeType::Modified,
                        };

                        let file_event = FileChangeEvent {
                            timestamp: Utc::now().format("%H:%M:%S").to_string(),
                            path,
                            event_type,
                        };

                        {
                            let mut log = recent.lock().unwrap();
                            log.push_front(file_event.clone());
                            while log.len() > MAX_RECENT_EVENTS {
                                log.pop_back();
                            }
                        }

                        if auto_sync.load(Ordering::Relaxed) {
                            let cfg_opt = sls_cfg_path
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .clone();
                            let _ = Self::sync_lua_event_to_slssteam(
                                &file_event,
                                cfg_opt.as_deref(),
                            );
                        }

                        let _ = send_hot_reload_ipc(&file_event);
                        if let Some(cb) = &callback {
                            cb(file_event);
                        }
                    }
                }
            }
            watch.cancel();
            running.store(false, Ordering::SeqCst);
        });

        *self.thread_handle.lock().unwrap() = Some(handle);

        Ok(())
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.thread_handle.lock().unwrap().take() {
            let _ = handle.join();
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn recent_events(&self) -> Vec<FileChangeEvent> {
        self.recent.lock().unwrap().iter().cloned().collect()
    }

    pub fn push_synthetic_event(&self, path: PathBuf, event_type: FileChangeType) {
        let mut log = self.recent.lock().unwrap();
        log.push_front(FileChangeEvent {
            timestamp: Utc::now().format("%H:%M:%S").to_string(),
            path,
            event_type,
        });
        while log.len() > MAX_RECENT_EVENTS {
            log.pop_back();
        }
    }

    pub fn notify_bridge(&self, event: &FileChangeEvent) -> Result<(), String> {
        send_hot_reload_ipc(event)
    }
}

fn send_hot_reload_ipc(event: &FileChangeEvent) -> Result<(), String> {
    let mut payload = HashMap::new();
    payload.insert("path".into(), event.path.to_string_lossy().to_string());
    payload.insert("action".into(), event.event_type.as_str().to_string());
    let client = IpcClient::new();
    let _ = client.send(&IpcMessage::Event {
        kind: "lua_hot_reload".into(),
        payload,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn watches_lua_creations_and_stops_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let stplug = tmp.path().join("stplug-in");
        let svc = HotReloadService::new().set_watch_dir(&stplug);
        assert!(!svc.is_running());

        svc.start().unwrap();
        assert!(svc.is_running());

        thread::sleep(Duration::from_millis(60));
        fs::write(stplug.join("1245620.lua"), "addappid(1245620)\n").unwrap();
        thread::sleep(Duration::from_millis(220));

        let events = svc.recent_events();
        assert!(!events.is_empty());
        assert!(events[0].path.ends_with("1245620.lua"));

        svc.stop();
        thread::sleep(Duration::from_millis(200));
        assert!(!svc.is_running());
    }

    #[test]
    fn external_lua_edit_auto_syncs_slssteam_and_deletion_cleans_up() {
        let tmp = tempfile::tempdir().unwrap();
        let stplug = tmp.path().join("Steam/config/stplug-in");
        let sls_path = tmp.path().join("SLSsteam/config.yaml");
        let svc = HotReloadService::new()
            .set_watch_dir(&stplug)
            .with_slssteam_config_path(&sls_path)
            .with_auto_slssteam_sync(true);

        svc.start().unwrap();
        thread::sleep(Duration::from_millis(60));

        let lua_file = stplug.join("1245620.lua");
        fs::write(
            &lua_file,
            format!("addappid(1245620)\naddappid(1245621, 1, \"{}\")\n", "a".repeat(64)),
        )
        .unwrap();
        thread::sleep(Duration::from_millis(250));

        let sls_after_create = SlssteamService::new(Some(sls_path.clone()));
        assert!(sls_after_create.config.additional_apps.contains(&1245620));
        assert_eq!(
            sls_after_create.config.depot_keys.get(&1245621).map(String::len),
            Some(64)
        );

        fs::remove_file(&lua_file).unwrap();
        thread::sleep(Duration::from_millis(250));
        let sls_after_delete = SlssteamService::new(Some(sls_path));
        assert!(!sls_after_delete.config.additional_apps.contains(&1245620));

        svc.stop();
    }
}
