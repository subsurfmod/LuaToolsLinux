//! Linux implementation of OSTPlatform::DirectoryWatch
//!
//! Uses inotify for filesystem change notifications.
//! Equivalent to Windows ReadDirectoryChangesW.

use std::collections::VecDeque;
use std::fs;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd};
use std::path::{Path, PathBuf};
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

/// Internal inotify watch state
struct WatchInner {
    inotify: Mutex<Inotify>,
    watch_descriptor: WatchDescriptor,
    directory: PathBuf,
    buffer_size: u32,
    pending_events: Arc<Mutex<VecDeque<Change>>>,
    cancelled: Arc<Mutex<bool>>,
}

/// Directory watcher using inotify.
pub struct Watch {
    inner: Option<WatchInner>,
}

impl Watch {
    pub fn new() -> Self {
        Self { inner: None }
    }

    /// Open a directory for watching.
    /// buffer_size is the size of the inotify event buffer in bytes.
    pub fn open(&mut self, directory: &str, buffer_size: u32) -> bool {
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

        let watch_descriptor = match inotify.add_watch(path, mask) {
            Ok(wd) => wd,
            Err(_) => return false,
        };

        let pending_events = Arc::new(Mutex::new(VecDeque::new()));
        let cancelled = Arc::new(Mutex::new(false));
        let inotify_mutex = Arc::new(Mutex::new(inotify));

        // Start background thread to read events
        let inotify_clone = inotify_mutex.clone();
        let pending_clone = pending_events.clone();
        let cancelled_clone = cancelled.clone();
        let buffer_size = buffer_size as usize;

        thread::spawn(move || {
            let mut buffer = vec![0u8; buffer_size];

            loop {
                if *cancelled_clone.lock().unwrap() {
                    break;
                }

                let mut inotify = inotify_clone.lock().unwrap();
                match inotify.read_events(&mut buffer) {
                    Ok(events) => {
                        let mut pending = pending_clone.lock().unwrap();
                        for event in events {
                            let name = event.name.map(|n| n.to_string_lossy().to_string())
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
                    }
                    Err(e) => {
                        if e.kind() == std::io::ErrorKind::Interrupted {
                            continue;
                        }
                        // On error, mark cancelled and exit
                        *cancelled_clone.lock().unwrap() = true;
                        break;
                    }
                }
            }
        });

        self.inner = Some(WatchInner {
            inotify: inotify_mutex,
            watch_descriptor,
            directory: path.to_path_buf(),
            buffer_size: buffer_size as u32,
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
            let mut pending = inner.pending_events.lock().unwrap();
            pending.drain(..).collect()
        } else {
            Vec::new()
        }
    }

    /// Cancel watching.
    pub fn cancel(&mut self) {
        if let Some(mut inner) = self.inner.take() {
            *inner.cancelled.lock().unwrap() = true;
            // Remove watch
            let mut inotify = inner.inotify.lock().unwrap();
            let _ = inotify.rm_watch(inner.watch_descriptor);
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
        // Check each watch for pending events
        for (index, watch) in watches.iter().enumerate() {
            if let Some(inner) = &watch.inner {
                let pending = inner.pending_events.lock().unwrap();
                if !pending.is_empty() {
                    return WaitResult {
                        status: WaitStatus::Signaled,
                        index,
                    };
                }
            }
        }

        // Check timeout
        if start.elapsed() >= timeout {
            return WaitResult {
                status: WaitStatus::Timeout,
                index: 0,
            };
        }

        // Short sleep to avoid busy waiting
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
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

        // Create a file
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "hello").unwrap();

        // Wait for event
        thread::sleep(Duration::from_millis(100));

        let changes = watch.drain();
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

        // Create file in dir2
        let file_path = dir2.path().join("test.txt");
        fs::write(&file_path, "hello").unwrap();

        thread::sleep(Duration::from_millis(100));

        let mut watches = [&mut watch1, &mut watch2];
        let result = wait_any(&mut watches, 1000);

        assert_eq!(result.status, WaitStatus::Signaled);
        assert_eq!(result.index, 1); // dir2 is index 1

        watch1.cancel();
        watch2.cancel();
    }
}