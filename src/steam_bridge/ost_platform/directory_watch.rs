//! Linux implementation of OSTPlatform::DirectoryWatch
//!
//! Uses non-blocking `inotify` for filesystem change notifications.
//! Equivalent to Windows `ReadDirectoryChangesW`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeAction {
    Added,
    Removed,
    Modified,
    RenamedOldName,
    RenamedNewName,
}

#[derive(Debug, Clone)]
pub struct Change {
    pub relative_path: String,
    pub action: ChangeAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitStatus {
    Signaled,
    Timeout,
    Failed,
}

#[derive(Debug, Clone, Copy)]
pub struct WaitResult {
    pub status: WaitStatus,
    pub index: usize,
}

/// Internal inotify watch state.
struct WatchInner {
    inotify: Arc<Mutex<Inotify>>,
    watch_descriptor: WatchDescriptor,
    directory: PathBuf,
    pending_events: Arc<Mutex<VecDeque<Change>>>,
    cancelled: Arc<AtomicBool>,
}

/// Directory watcher using non-blocking `inotify`.
pub struct Watch {
    inner: Option<WatchInner>,
}

impl Default for Watch {
    fn default() -> Self {
        Self::new()
    }
}

impl Watch {
    pub fn new() -> Self {
        Self { inner: None }
    }

    /// Open a directory for watching.
    /// `buffer_size` is the size of the inotify event buffer in bytes (minimum 4096).
    pub fn open(&mut self, directory: &str, buffer_size: u32) -> bool {
        self.cancel();

        let path = Path::new(directory);
        if !path.exists() || !path.is_dir() {
            return false;
        }

        let inotify = match Inotify::init() {
            Ok(i) => i,
            Err(_) => return false,
        };

        let mask = WatchMask::CREATE
            | WatchMask::DELETE
            | WatchMask::MODIFY
            | WatchMask::MOVED_FROM
            | WatchMask::MOVED_TO
            | WatchMask::DELETE_SELF
            | WatchMask::MOVE_SELF;

        let watch_descriptor = match inotify.watches().add(path, mask) {
            Ok(wd) => wd,
            Err(_) => return false,
        };

        let pending_events = Arc::new(Mutex::new(VecDeque::new()));
        let cancelled = Arc::new(AtomicBool::new(false));
        let inotify_mutex = Arc::new(Mutex::new(inotify));

        let inotify_clone = Arc::clone(&inotify_mutex);
        let pending_clone = Arc::clone(&pending_events);
        let cancelled_clone = Arc::clone(&cancelled);
        let buf_len = (buffer_size as usize).max(4096);

        thread::spawn(move || {
            let mut buffer = vec![0u8; buf_len];

            while !cancelled_clone.load(Ordering::Acquire) {
                let read_result = {
                    let mut inotify = inotify_clone.lock().unwrap_or_else(|e| e.into_inner());
                    match inotify.read_events(&mut buffer) {
                        Ok(events) => {
                            let mut pending =
                                pending_clone.lock().unwrap_or_else(|e| e.into_inner());
                            for event in events {
                                let name = event
                                    .name
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default();

                                let action = if event.mask.contains(EventMask::CREATE) {
                                    ChangeAction::Added
                                } else if event.mask.contains(EventMask::DELETE) {
                                    ChangeAction::Removed
                                } else if event.mask.contains(EventMask::MODIFY) {
                                    ChangeAction::Modified
                                } else if event.mask.contains(EventMask::MOVED_FROM) {
                                    ChangeAction::RenamedOldName
                                } else if event.mask.contains(EventMask::MOVED_TO) {
                                    ChangeAction::RenamedNewName
                                } else {
                                    ChangeAction::Modified
                                };

                                pending.push_back(Change {
                                    relative_path: name,
                                    action,
                                });
                            }
                            Ok(true)
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Ok(false),
                        Err(e) => Err(e),
                    }
                };

                match read_result {
                    Ok(true) => {}
                    Ok(false) => thread::sleep(Duration::from_millis(15)),
                    Err(_) => {
                        cancelled_clone.store(true, Ordering::Release);
                        break;
                    }
                }
            }
        });

        self.inner = Some(WatchInner {
            inotify: inotify_mutex,
            watch_descriptor,
            directory: path.to_path_buf(),
            pending_events,
            cancelled,
        });

        true
    }

    /// Issue a read request (no-op on Linux, events are read in background thread).
    pub fn issue_read(&mut self) -> bool {
        self.inner.is_some()
    }

    /// Drain all pending changes.
    pub fn drain(&mut self) -> Vec<Change> {
        if let Some(inner) = &self.inner {
            let mut pending = inner
                .pending_events
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            pending.drain(..).collect()
        } else {
            Vec::new()
        }
    }

    /// Cancel watching.
    pub fn cancel(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.cancelled.store(true, Ordering::Release);
            let inotify = inner.inotify.lock().unwrap_or_else(|e| e.into_inner());
            let _ = inotify.watches().remove(inner.watch_descriptor);
        }
    }

    /// Check if watch is open.
    pub fn is_open(&self) -> bool {
        self.inner.is_some()
    }

    /// Get the watched directory path.
    pub fn directory(&self) -> String {
        self.inner
            .as_ref()
            .map(|i| i.directory.to_string_lossy().to_string())
            .unwrap_or_default()
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Wait for any of the watches to have events.
/// Returns the index of the watch that has events, or timeout/failed.
pub fn wait_any(watches: &mut [&mut Watch], timeout_ms: u32) -> WaitResult {
    if watches.is_empty() {
        return WaitResult {
            status: WaitStatus::Failed,
            index: 0,
        };
    }

    let start = std::time::Instant::now();
    let timeout = Duration::from_millis(timeout_ms as u64);

    loop {
        for (index, watch) in watches.iter().enumerate() {
            if let Some(inner) = &watch.inner {
                let pending = inner
                    .pending_events
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if !pending.is_empty() {
                    return WaitResult {
                        status: WaitStatus::Signaled,
                        index,
                    };
                }
            }
        }

        if start.elapsed() >= timeout {
            return WaitResult {
                status: WaitStatus::Timeout,
                index: 0,
            };
        }

        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::thread;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn test_watch_directory() {
        let dir = tempdir().unwrap();
        let mut watch = Watch::new();

        assert!(watch.open(dir.path().to_str().unwrap(), 8192));
        assert!(watch.is_open());
        assert_eq!(watch.directory(), dir.path().to_str().unwrap());

        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "hello").unwrap();

        let mut changes = Vec::new();
        for _ in 0..30 {
            changes = watch.drain();
            if !changes.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }

        assert!(!changes.is_empty());
        assert_eq!(changes[0].action, ChangeAction::Added);
        assert_eq!(changes[0].relative_path, "test.txt");

        watch.cancel();
        assert!(!watch.is_open());
    }

    #[test]
    fn test_wait_any() {
        let dir1 = tempdir().unwrap();
        let dir2 = tempdir().unwrap();

        let mut watch1 = Watch::new();
        let mut watch2 = Watch::new();

        assert!(watch1.open(dir1.path().to_str().unwrap(), 8192));
        assert!(watch2.open(dir2.path().to_str().unwrap(), 8192));

        let file_path = dir2.path().join("test.txt");
        fs::write(&file_path, "hello").unwrap();

        let mut watches = [&mut watch1, &mut watch2];
        let result = wait_any(&mut watches, 1500);

        assert_eq!(result.status, WaitStatus::Signaled);
        assert_eq!(result.index, 1);

        watch1.cancel();
        watch2.cancel();
    }
}
