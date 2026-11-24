//! Linux implementation of OSTPlatform::DirectoryWatch
//!
//! Uses non-blocking `inotify` for filesystem change notifications.
//! Equivalent to Windows `ReadDirectoryChangesW`.

use std::collections::VecDeque;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use inotify::{EventMask, Inotify, WatchMask};

const MIN_EVENT_BUFFER_SIZE: usize = 4096;
const MAX_EVENT_BUFFER_SIZE: usize = 16 * 1024 * 1024;
const MAX_PENDING_EVENTS: usize = 65_536;

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

fn map_event_mask(mask: EventMask) -> Option<ChangeAction> {
    if mask.contains(EventMask::IGNORED) || mask.contains(EventMask::Q_OVERFLOW) {
        return None;
    }
    if mask.contains(EventMask::CREATE) {
        Some(ChangeAction::Added)
    } else if mask.contains(EventMask::DELETE) || mask.contains(EventMask::DELETE_SELF) {
        Some(ChangeAction::Removed)
    } else if mask.contains(EventMask::MODIFY) {
        Some(ChangeAction::Modified)
    } else if mask.contains(EventMask::MOVED_FROM) || mask.contains(EventMask::MOVE_SELF) {
        Some(ChangeAction::RenamedOldName)
    } else if mask.contains(EventMask::MOVED_TO) {
        Some(ChangeAction::RenamedNewName)
    } else {
        Some(ChangeAction::Modified)
    }
}

/// Internal inotify watch state.
struct WatchInner {
    directory: PathBuf,
    pending_events: Arc<Mutex<VecDeque<Change>>>,
    cancelled: Arc<AtomicBool>,
    wake_fd: Arc<OwnedFd>,
    worker: Option<JoinHandle<()>>,
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
    /// `buffer_size` is the size of the inotify event buffer in bytes (clamped to `[4096, 16 MiB]`).
    pub fn open(&mut self, directory: &str, buffer_size: u32) -> bool {
        self.cancel();

        let path = Path::new(directory);
        if !path.exists() || !path.is_dir() {
            return false;
        }

        let mut inotify = match Inotify::init() {
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

        if inotify.watches().add(path, mask).is_err() {
            return false;
        }

        let raw_wake_fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if raw_wake_fd < 0 {
            return false;
        }
        // Safety: raw_wake_fd is a freshly created valid file descriptor.
        let wake_fd = Arc::new(unsafe { OwnedFd::from_raw_fd(raw_wake_fd) });

        let pending_events = Arc::new(Mutex::new(VecDeque::new()));
        let cancelled = Arc::new(AtomicBool::new(false));

        let pending_clone = Arc::clone(&pending_events);
        let cancelled_clone = Arc::clone(&cancelled);
        let wake_clone = Arc::clone(&wake_fd);
        let buf_len = (buffer_size as usize).clamp(MIN_EVENT_BUFFER_SIZE, MAX_EVENT_BUFFER_SIZE);

        let worker = match thread::Builder::new()
            .name("ost-dir-watch".to_string())
            .spawn(move || {
                let mut buffer = vec![0u8; buf_len];

                while !cancelled_clone.load(Ordering::Acquire) {
                    let mut fds = [
                        libc::pollfd {
                            fd: inotify.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                        libc::pollfd {
                            fd: wake_clone.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                    ];

                    let poll_rc =
                        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 250) };
                    if poll_rc < 0 {
                        let err = std::io::Error::last_os_error();
                        if err.kind() == std::io::ErrorKind::Interrupted {
                            continue;
                        }
                        cancelled_clone.store(true, Ordering::Release);
                        break;
                    }

                    if cancelled_clone.load(Ordering::Acquire)
                        || (fds[1].revents & libc::POLLIN) != 0
                    {
                        break;
                    }

                    if poll_rc == 0 || (fds[0].revents & libc::POLLIN) == 0 {
                        continue;
                    }

                    match inotify.read_events(&mut buffer) {
                        Ok(events) => {
                            let mut pending =
                                pending_clone.lock().unwrap_or_else(|e| e.into_inner());
                            for event in events {
                                let Some(action) = map_event_mask(event.mask) else {
                                    continue;
                                };
                                let name = event
                                    .name
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default();

                                if pending.len() >= MAX_PENDING_EVENTS {
                                    pending.pop_front();
                                }
                                pending.push_back(Change {
                                    relative_path: name,
                                    action,
                                });
                            }
                        }
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                || e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => {
                            cancelled_clone.store(true, Ordering::Release);
                            break;
                        }
                    }
                }
            }) {
            Ok(h) => h,
            Err(_) => return false,
        };

        self.inner = Some(WatchInner {
            directory: path.to_path_buf(),
            pending_events,
            cancelled,
            wake_fd,
            worker: Some(worker),
        });

        true
    }

    /// Issue a read request (no-op on Linux, events are read in background thread).
    pub fn issue_read(&mut self) -> bool {
        self.is_open()
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

    /// Cancel watching and join the background worker thread cleanly.
    pub fn cancel(&mut self) {
        if let Some(mut inner) = self.inner.take() {
            inner.cancelled.store(true, Ordering::Release);
            let val: u64 = 1;
            let _ = unsafe {
                libc::write(
                    inner.wake_fd.as_raw_fd(),
                    (&val as *const u64).cast::<libc::c_void>(),
                    std::mem::size_of::<u64>(),
                )
            };
            if let Some(worker) = inner.worker.take() {
                let _ = worker.join();
            }
        }
    }

    /// Check if watch is open and its background worker is active.
    pub fn is_open(&self) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|i| !i.cancelled.load(Ordering::Acquire))
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
    if watches.is_empty() || !watches.iter().any(|w| w.is_open()) {
        return WaitResult {
            status: WaitStatus::Failed,
            index: 0,
        };
    }

    let start = std::time::Instant::now();
    let timeout = Duration::from_millis(timeout_ms as u64);

    loop {
        let mut any_open = false;
        for (index, watch) in watches.iter().enumerate() {
            if let Some(inner) = &watch.inner {
                if !inner.cancelled.load(Ordering::Acquire) {
                    any_open = true;
                }
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

        if !any_open {
            return WaitResult {
                status: WaitStatus::Failed,
                index: 0,
            };
        }

        let elapsed = start.elapsed();
        if elapsed >= timeout {
            return WaitResult {
                status: WaitStatus::Timeout,
                index: 0,
            };
        }

        let remaining = timeout.saturating_sub(elapsed);
        thread::sleep(Duration::from_millis(5).min(remaining));
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

    #[test]
    fn test_map_event_mask_ignores_control_events() {
        assert_eq!(map_event_mask(EventMask::IGNORED), None);
        assert_eq!(map_event_mask(EventMask::Q_OVERFLOW), None);
        assert_eq!(
            map_event_mask(EventMask::DELETE_SELF),
            Some(ChangeAction::Removed)
        );
        assert_eq!(
            map_event_mask(EventMask::MOVE_SELF),
            Some(ChangeAction::RenamedOldName)
        );
    }

    #[test]
    fn test_wait_any_closed_and_timeout() {
        let mut closed = Watch::new();
        assert!(!closed.is_open());
        let mut watches = [&mut closed];
        assert_eq!(wait_any(&mut watches, 100).status, WaitStatus::Failed);

        let dir = tempdir().unwrap();
        let mut watch = Watch::new();
        assert!(watch.open(dir.path().to_str().unwrap(), 4096));
        let mut active = [&mut watch];
        assert_eq!(wait_any(&mut active, 30).status, WaitStatus::Timeout);

        // Open on nonexistent directory fails cleanly
        assert!(!watch.open("/nonexistent/ost_watch_path_12345", 4096));
        assert!(!watch.is_open());
    }
}
