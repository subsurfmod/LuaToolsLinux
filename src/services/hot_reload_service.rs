//! HotReloadService — watches `stplug-in/` via `ost_platform::directory_watch` for `.lua`
//! file changes, logs recent events for GUI inspection, and notifies `bridge_lib` over IPC.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use chrono::Utc;

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
    recent: Arc<Mutex<VecDeque<FileChangeEvent>>>,
    change_callback: Option<Arc<dyn Fn(FileChangeEvent) + Send + Sync>>,
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
            recent: Arc::new(Mutex::new(VecDeque::new())),
            change_callback: None,
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

        thread::spawn(move || {
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

        Ok(())
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
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
}
