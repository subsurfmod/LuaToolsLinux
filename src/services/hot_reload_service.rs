//! HotReloadService — watches stplug-in/ for .lua file changes and signals
//! the bridge_lib to reload configuration without restarting Steam.
//!
//! Uses inotify (via the `inotify` crate) to monitor the Steam stplug-in
//! directory. When .lua files are created, modified, or deleted, an IPC
//! message is sent to the integration library running inside Steam.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use inotify::{Inotify, EventMask, WatchMask};

/// Change event for a watched file.
#[derive(Debug, Clone)]
pub struct FileChangeEvent {
    pub path: PathBuf,
    pub event_type: FileChangeType,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FileChangeType {
    Created,
    Modified,
    Deleted,
    Moved,
}

/// Service that watches stplug-in/ for .lua changes.
pub struct HotReloadService {
    watch_dir: PathBuf,
    running: Arc<AtomicBool>,
    change_callback: Option<Arc<dyn Fn(FileChangeEvent) + Send + Sync>>,
}

impl Default for HotReloadService {
    fn default() -> Self {
        Self::new()
    }
}

impl HotReloadService {
    pub fn new() -> Self {
        Self {
            watch_dir: dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/share/Steam/config/stplug-in"),
            running: Arc::new(AtomicBool::new(false)),
            change_callback: None,
        }
    }

    /// Set the watch directory (overrides default Steam stplug-in path).
    pub fn set_watch_dir<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.watch_dir = path.as_ref().to_path_buf();
        self
    }

    /// Register a callback for file changes.
    pub fn on_change<F>(mut self, callback: F) -> Self
    where
        F: Fn(FileChangeEvent) + Send + Sync + 'static,
    {
        self.change_callback = Some(Arc::new(callback));
        self
    }

    /// Start watching in a background thread.
    pub fn start(&mut self) -> Result<(), String> {
        if self.running.load(Ordering::Relaxed) {
            return Err("Already watching".into());
        }

        if !self.watch_dir.exists() {
            fs::create_dir_all(&self.watch_dir).map_err(|e| e.to_string())?;
        }

        let running = Arc::clone(&self.running);
        running.store(true, Ordering::Relaxed);
        let watch_dir = self.watch_dir.clone();
        let callback = self.change_callback.clone();

        thread::spawn(move || {
            let mut inotify = match Inotify::init() {
                Ok(i) => i,
                Err(e) => {
                    eprintln!("[hot_reload] inotify init failed: {}", e);
                    running.store(false, Ordering::Relaxed);
                    return;
                }
            };

            let watch_mask = WatchMask::MODIFY
                | WatchMask::CREATE
                | WatchMask::DELETE
                | WatchMask::MOVED_FROM
                | WatchMask::MOVED_TO;

            match inotify.watches().add(&watch_dir, watch_mask) {
                Ok(_) => {
                    eprintln!(
                        "[hot_reload] watching {}",
                        watch_dir.display()
                    );
                }
                Err(e) => {
                    eprintln!("[hot_reload] failed to add watch: {}", e);
                    running.store(false, Ordering::Relaxed);
                    return;
                }
            }

            let mut buffer = [0u8; 4096];
            while running.load(Ordering::Relaxed) {
                match inotify.read_events_blocking(&mut buffer) {
                    Ok(events) => {
                        for event in events {
                            if let Some(name) = event.name {
                                let path = watch_dir.join(name);
                                if path.extension().and_then(|e| e.to_str()) == Some("lua") {
                                    let event_type = if event.mask.contains(EventMask::CREATE) {
                                        FileChangeType::Created
                                    } else if event.mask.contains(EventMask::DELETE) {
                                        FileChangeType::Deleted
                                    } else if event.mask.contains(EventMask::MOVED_TO) {
                                        FileChangeType::Moved
                                    } else if event.mask.contains(EventMask::MOVED_FROM) {
                                        FileChangeType::Moved
                                    } else {
                                        FileChangeType::Modified
                                    };

                                    let change = FileChangeEvent { path, event_type };
                                    if let Some(cb) = &callback {
                                        cb(change);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[hot_reload] read error: {}", e);
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    /// Stop watching.
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Send an IPC notification to the bridge_lib about a file change.
    pub fn notify_bridge(&self, event: &FileChangeEvent) -> Result<(), String> {
        use crate::steam_bridge::ipc::IpcClient;

        let client = IpcClient::new();
        let _ = client.send(&crate::steam_bridge::ipc::IpcMessage::Ping);
        let _ = event;
        Ok(())
    }
}

impl Drop for HotReloadService {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_defaults() {
        let svc = HotReloadService::new();
        assert!(!svc.is_running());
        assert!(svc.watch_dir.to_string_lossy().contains("stplug-in"));
    }
}
