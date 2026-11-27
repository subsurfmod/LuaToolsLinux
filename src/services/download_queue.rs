//! DownloadQueue — Centralized download worker managing concurrency, cooperative cancellation,
//! live active task tracking, sliding-window speed/ETA, history, and temp file cleanup (LT-38).

use std::collections::VecDeque;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::models::config::default_luatools_cache_dir;

const MAX_HISTORY: usize = 100;
const MAX_CONCURRENT_DOWNLOADS: usize = 3;
const HISTORY_FILE: &str = "download_history.json";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DownloadTask {
    pub id: String,
    pub app_id: u32,
    pub label: String,
    pub source: String,
    pub url: String,
    pub dest_path: PathBuf,
    pub total_bytes: Option<u64>,
    pub priority: i32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DownloadHistoryEntry {
    pub app_id: u32,
    pub label: String,
    pub source: String,
    pub status: String, // "completed" | "failed" | "cancelled"
    pub timestamp: String,
    pub duration_secs: Option<u64>,
    pub bytes_downloaded: Option<u64>,
}

/// Live snapshot of an active download task for UI rendering & cancellation controls.
#[derive(Debug, Clone)]
pub struct ActiveDownloadInfo {
    pub id: String,
    pub app_id: u32,
    pub label: String,
    pub source: String,
    pub progress: f32,
    pub status: String,
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
    pub cancelled: bool,
}

struct ActiveTaskState {
    info: ActiveDownloadInfo,
    cancel_flag: Arc<AtomicBool>,
}

type TaskRunner = Box<dyn FnOnce(DownloadTaskContext) -> Result<Option<u64>, String> + Send + 'static>;

struct QueuedItem {
    task: DownloadTask,
    runner: Option<TaskRunner>,
}

/// Context handed to custom download runners (Acquire manifest / DepotDownloader jobs)
/// so they report progress, bytes, and honor cooperative cancellation through `DownloadQueue`.
#[derive(Clone)]
pub struct DownloadTaskContext {
    pub id: String,
    cancel_flag: Arc<AtomicBool>,
    shutdown: Arc<Mutex<bool>>,
    active_tasks: Arc<Mutex<Vec<ActiveTaskState>>>,
    speed_samples: Arc<Mutex<VecDeque<(Instant, u64)>>>,
}

impl DownloadTaskContext {
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel_flag.clone()
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::SeqCst)
            || *self.shutdown.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn report_progress(&self, pct: f32, status: impl Into<String>, bytes: Option<u64>) {
        let status_str = status.into();
        if let Ok(mut guard) = self.active_tasks.lock() {
            if let Some(st) = guard.iter_mut().find(|t| t.info.id == self.id) {
                st.info.progress = pct.clamp(0.0, 1.0);
                st.info.status = status_str;
                if let Some(b) = bytes {
                    st.info.bytes_downloaded = b;
                }
            }
        }
        if let Some(b) = bytes {
            if let Ok(mut samples) = self.speed_samples.lock() {
                samples.push_back((Instant::now(), b));
                while samples.len() > 60 {
                    samples.pop_front();
                }
            }
        }
    }
}

pub struct DownloadQueue {
    tasks: Arc<Mutex<VecDeque<QueuedItem>>>,
    active_tasks: Arc<Mutex<Vec<ActiveTaskState>>>,
    history: Arc<Mutex<VecDeque<DownloadHistoryEntry>>>,
    active_downloads: Arc<Mutex<usize>>,
    notifier: Arc<Condvar>,
    shutdown: Arc<Mutex<bool>>,
    history_path: PathBuf,
    cache_dir: PathBuf,
    speed_samples: Arc<Mutex<VecDeque<(Instant, u64)>>>,
}

impl fmt::Debug for DownloadQueue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DownloadQueue")
            .field("active_count", &self.active_count())
            .field("pending_count", &self.pending_tasks().len())
            .field("history_path", &self.history_path)
            .finish()
    }
}

impl DownloadQueue {
    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        let cache = cache_dir.unwrap_or_else(default_luatools_cache_dir);
        let history_path = cache.join(HISTORY_FILE);
        let history = Self::load_history(&history_path);

        Self {
            tasks: Arc::new(Mutex::new(VecDeque::new())),
            active_tasks: Arc::new(Mutex::new(Vec::new())),
            history: Arc::new(Mutex::new(history)),
            active_downloads: Arc::new(Mutex::new(0)),
            notifier: Arc::new(Condvar::new()),
            shutdown: Arc::new(Mutex::new(false)),
            history_path,
            cache_dir: cache,
            speed_samples: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    fn load_history(path: &Path) -> VecDeque<DownloadHistoryEntry> {
        if !path.exists() {
            return VecDeque::new();
        }
        let Ok(content) = fs::read_to_string(path) else {
            return VecDeque::new();
        };
        serde_json::from_str(&content).unwrap_or_default()
    }

    fn save_history(&self) {
        if let Some(parent) = self.history_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = self.history_path.with_extension("json.tmp");
        if let Ok(content) = serde_json::to_string_pretty(&*history) {
            let _ = fs::write(&tmp, content);
            let _ = fs::rename(&tmp, &self.history_path);
        }
    }

    /// Adds an HTTP download task to the queue (ordered by descending priority).
    pub fn enqueue(&self, task: DownloadTask) {
        self.enqueue_internal(QueuedItem { task, runner: None });
    }

    /// Adds a managed task with a custom execution runner (e.g., Hubcap/lua.tools manifest fetch
    /// or DepotDownloader execution) so `DownloadQueue` owns concurrency, progress, cancellation,
    /// and history tracking.
    pub fn enqueue_with_runner<F>(&self, task: DownloadTask, runner: F)
    where
        F: FnOnce(DownloadTaskContext) -> Result<Option<u64>, String> + Send + 'static,
    {
        self.enqueue_internal(QueuedItem {
            task,
            runner: Some(Box::new(runner)),
        });
    }

    fn enqueue_internal(&self, item: QueuedItem) {
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        let pos = tasks
            .iter()
            .position(|t| t.task.priority < item.task.priority)
            .unwrap_or(tasks.len());
        tasks.insert(pos, item);
        self.notifier.notify_one();
    }

    /// Gets the next task metadata from the queue (used in tests and worker loop).
    #[cfg(test)]
    fn dequeue(&self) -> Option<DownloadTask> {
        self.dequeue_item().map(|i| i.task)
    }

    fn dequeue_item(&self) -> Option<QueuedItem> {
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        tasks.pop_front()
    }

    /// Cancels a pending or active task by ID. Returns `true` if a matching task was found.
    pub fn cancel_task(&self, id: &str) -> bool {
        // 1. Remove from pending queue if not yet started
        {
            let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(pos) = tasks.iter().position(|t| t.task.id == id) {
                if let Some(removed) = tasks.remove(pos) {
                    drop(tasks);
                    self.record_history(
                        removed.task.app_id,
                        &removed.task.label,
                        &removed.task.source,
                        "cancelled",
                        Duration::from_secs(0),
                        None,
                        None,
                    );
                    return true;
                }
            }
        }
        // 2. Otherwise signal active task cancel flag
        {
            let mut active = self.active_tasks.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(st) = active.iter_mut().find(|t| t.info.id == id) {
                st.cancel_flag.store(true, Ordering::SeqCst);
                st.info.cancelled = true;
                st.info.status = "Cancelling…".into();
                return true;
            }
        }
        false
    }

    /// Cancels all pending and active tasks, returning the number of tasks cancelled.
    pub fn cancel_all(&self) -> usize {
        let mut cancelled = 0usize;
        let pending_removed: Vec<DownloadTask> = {
            let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            let mut out = Vec::with_capacity(tasks.len());
            while let Some(item) = tasks.pop_front() {
                out.push(item.task);
            }
            out
        };
        for task in pending_removed {
            cancelled += 1;
            self.record_history(
                task.app_id,
                &task.label,
                &task.source,
                "cancelled",
                Duration::from_secs(0),
                None,
                None,
            );
        }
        {
            let mut active = self.active_tasks.lock().unwrap_or_else(|e| e.into_inner());
            for st in active.iter_mut() {
                if !st.cancel_flag.swap(true, Ordering::SeqCst) {
                    cancelled += 1;
                }
                st.info.cancelled = true;
                st.info.status = "Cancelling…".into();
            }
        }
        cancelled
    }

    /// Checks whether an active task ID has been flagged for cancellation.
    pub fn is_cancelled(&self, id: &str) -> bool {
        let active = self.active_tasks.lock().unwrap_or_else(|e| e.into_inner());
        active
            .iter()
            .find(|t| t.info.id == id)
            .map(|t| t.cancel_flag.load(Ordering::SeqCst))
            .unwrap_or(false)
    }

    /// Starts the download worker thread.
    pub fn start_worker(&self) {
        let queue = self.clone_inner();
        thread::spawn(move || {
            queue.worker_loop();
        });
    }

    fn clone_inner(&self) -> Self {
        Self {
            tasks: self.tasks.clone(),
            active_tasks: self.active_tasks.clone(),
            history: self.history.clone(),
            active_downloads: self.active_downloads.clone(),
            notifier: self.notifier.clone(),
            shutdown: self.shutdown.clone(),
            history_path: self.history_path.clone(),
            cache_dir: self.cache_dir.clone(),
            speed_samples: self.speed_samples.clone(),
        }
    }

    fn worker_loop(&self) {
        loop {
            if *self.shutdown.lock().unwrap_or_else(|e| e.into_inner()) {
                break;
            }

            let can_start = {
                let active = self
                    .active_downloads
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                *active < MAX_CONCURRENT_DOWNLOADS
            };

            if !can_start {
                let active = self
                    .active_downloads
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let _ = self.notifier.wait_timeout(active, Duration::from_millis(250));
                continue;
            }

            let item = self.dequeue_item();
            let Some(item) = item else {
                let tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
                let _ = self.notifier.wait_timeout(tasks, Duration::from_millis(500));
                continue;
            };

            *self
                .active_downloads
                .lock()
                .unwrap_or_else(|e| e.into_inner()) += 1;

            let cancel_flag = Arc::new(AtomicBool::new(false));
            let task_id = item.task.id.clone();
            {
                let mut active = self.active_tasks.lock().unwrap_or_else(|e| e.into_inner());
                active.push(ActiveTaskState {
                    info: ActiveDownloadInfo {
                        id: task_id.clone(),
                        app_id: item.task.app_id,
                        label: item.task.label.clone(),
                        source: item.task.source.clone(),
                        progress: 0.0,
                        status: "Starting…".into(),
                        bytes_downloaded: 0,
                        total_bytes: item.task.total_bytes,
                        cancelled: false,
                    },
                    cancel_flag: cancel_flag.clone(),
                });
            }

            let ctx = DownloadTaskContext {
                id: task_id.clone(),
                cancel_flag: cancel_flag.clone(),
                shutdown: self.shutdown.clone(),
                active_tasks: self.active_tasks.clone(),
                speed_samples: self.speed_samples.clone(),
            };

            let queue = self.clone_inner();
            thread::spawn(move || {
                let start = Instant::now();
                let QueuedItem { task, runner } = item;
                if let Some(custom_runner) = runner {
                    let outcome = custom_runner(ctx.clone());
                    let was_cancelled = ctx.is_cancelled();
                    match outcome {
                        Ok(bytes) if !was_cancelled => {
                            queue.record_history(
                                task.app_id,
                                &task.label,
                                &task.source,
                                "completed",
                                start.elapsed(),
                                bytes,
                                None,
                            );
                        }
                        Ok(bytes) => {
                            queue.record_history(
                                task.app_id,
                                &task.label,
                                &task.source,
                                "cancelled",
                                start.elapsed(),
                                bytes,
                                None,
                            );
                        }
                        Err(e) if was_cancelled => {
                            let _ = e;
                            queue.record_history(
                                task.app_id,
                                &task.label,
                                &task.source,
                                "cancelled",
                                start.elapsed(),
                                None,
                                None,
                            );
                        }
                        Err(e) => {
                            queue.record_history(
                                task.app_id,
                                &task.label,
                                &task.source,
                                "failed",
                                start.elapsed(),
                                None,
                                Some(e),
                            );
                        }
                    }
                    queue.cleanup_temp_files();
                } else {
                    queue.execute_http_download(task, ctx);
                }

                {
                    let mut active = queue
                        .active_tasks
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    active.retain(|t| t.info.id != task_id);
                }
                {
                    let mut count = queue
                        .active_downloads
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    *count = count.saturating_sub(1);
                }
                queue.notifier.notify_all();
            });
        }
    }

    fn execute_http_download(&self, task: DownloadTask, ctx: DownloadTaskContext) {
        let start = Instant::now();
        let mut bytes_downloaded = 0u64;

        if !task.url.starts_with("http://") && !task.url.starts_with("https://") {
            self.record_history(
                task.app_id,
                &task.label,
                &task.source,
                "failed",
                start.elapsed(),
                None,
                Some(format!(
                    "Unsupported download URL scheme (use enqueue_with_runner for non-HTTP jobs): {}",
                    task.url
                )),
            );
            return;
        }

        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .build();

        match agent.get(&task.url).call() {
            Ok(resp) => {
                let total = resp
                    .header("Content-Length")
                    .and_then(|v| v.parse::<u64>().ok())
                    .or(task.total_bytes);

                if let Some(parent) = task.dest_path.parent() {
                    let _ = fs::create_dir_all(parent);
                }

                let mut file = match fs::File::create(&task.dest_path) {
                    Ok(f) => f,
                    Err(e) => {
                        self.record_history(
                            task.app_id,
                            &task.label,
                            &task.source,
                            "failed",
                            start.elapsed(),
                            None,
                            Some(e.to_string()),
                        );
                        return;
                    }
                };

                let mut reader = resp.into_reader();
                let mut buffer = vec![0u8; 8192];

                loop {
                    if ctx.is_cancelled() {
                        drop(file);
                        let _ = fs::remove_file(&task.dest_path);
                        self.record_history(
                            task.app_id,
                            &task.label,
                            &task.source,
                            "cancelled",
                            start.elapsed(),
                            Some(bytes_downloaded),
                            None,
                        );
                        return;
                    }

                    match std::io::Read::read(&mut reader, &mut buffer) {
                        Ok(0) => break,
                        Ok(n) => {
                            if std::io::Write::write_all(&mut file, &buffer[..n]).is_err() {
                                drop(file);
                                let _ = fs::remove_file(&task.dest_path);
                                self.record_history(
                                    task.app_id,
                                    &task.label,
                                    &task.source,
                                    "failed",
                                    start.elapsed(),
                                    Some(bytes_downloaded),
                                    Some("Write failed".into()),
                                );
                                return;
                            }
                            bytes_downloaded += n as u64;
                            let pct = total
                                .filter(|&t| t > 0)
                                .map(|t| (bytes_downloaded as f32 / t as f32).clamp(0.0, 1.0))
                                .unwrap_or(0.0);
                            ctx.report_progress(pct, "Downloading…", Some(bytes_downloaded));
                        }
                        Err(e) => {
                            drop(file);
                            let _ = fs::remove_file(&task.dest_path);
                            self.record_history(
                                task.app_id,
                                &task.label,
                                &task.source,
                                "failed",
                                start.elapsed(),
                                Some(bytes_downloaded),
                                Some(e.to_string()),
                            );
                            return;
                        }
                    }
                }

                ctx.report_progress(1.0, "Completed", Some(bytes_downloaded));
                self.record_history(
                    task.app_id,
                    &task.label,
                    &task.source,
                    "completed",
                    start.elapsed(),
                    Some(bytes_downloaded),
                    None,
                );
            }
            Err(e) => {
                self.record_history(
                    task.app_id,
                    &task.label,
                    &task.source,
                    "failed",
                    start.elapsed(),
                    Some(bytes_downloaded),
                    Some(e.to_string()),
                );
            }
        }

        self.cleanup_temp_files();
    }

    fn record_history(
        &self,
        app_id: u32,
        label: &str,
        source: &str,
        status: &str,
        duration: Duration,
        bytes: Option<u64>,
        error: Option<String>,
    ) {
        let mut history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        let entry = DownloadHistoryEntry {
            app_id,
            label: label.to_string(),
            source: source.to_string(),
            status: if let Some(err) = error {
                format!("failed: {}", err)
            } else {
                status.to_string()
            },
            timestamp: chrono::Utc::now().to_rfc3339(),
            duration_secs: Some(duration.as_secs()),
            bytes_downloaded: bytes,
        };
        history.push_front(entry);
        if history.len() > MAX_HISTORY {
            history.pop_back();
        }
        drop(history);
        self.save_history();
    }

    fn cleanup_temp_files(&self) {
        let cache = self.cache_dir.join("downloads");
        if !cache.exists() {
            return;
        }
        let now = std::time::SystemTime::now();
        if let Ok(entries) = fs::read_dir(&cache) {
            for entry in entries.flatten() {
                let p = entry.path();
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if let Ok(elapsed) = now.duration_since(modified) {
                            if elapsed > Duration::from_secs(24 * 3600) {
                                let _ = fs::remove_file(&p);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Gets the current download speed in bytes/sec.
    pub fn current_speed(&self) -> Option<u64> {
        let samples = self.speed_samples.lock().unwrap_or_else(|e| e.into_inner());
        if samples.len() < 2 {
            return None;
        }
        let (first_time, first_bytes) = samples.front()?;
        let (last_time, last_bytes) = samples.back()?;
        let elapsed = last_time.duration_since(*first_time).as_secs_f64();
        if elapsed < 1.0 || last_bytes < first_bytes {
            return None;
        }
        Some(((last_bytes - first_bytes) as f64 / elapsed) as u64)
    }

    /// Gets the estimated time remaining for the given download task.
    pub fn eta(&self, task: &DownloadTask) -> Option<Duration> {
        let speed = self.current_speed()?;
        let total = task.total_bytes?;
        let downloaded = self.get_downloaded_bytes(&task.dest_path)?;
        if total <= downloaded || speed == 0 {
            return None;
        }
        Some(Duration::from_secs((total - downloaded) / speed))
    }

    fn get_downloaded_bytes(&self, path: &Path) -> Option<u64> {
        fs::metadata(path).ok().map(|m| m.len())
    }

    /// Gets all pending (queued, not yet started) tasks.
    pub fn pending_tasks(&self) -> Vec<DownloadTask> {
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|i| i.task.clone())
            .collect()
    }

    /// Gets live snapshots of currently executing tasks.
    pub fn active_tasks(&self) -> Vec<ActiveDownloadInfo> {
        self.active_tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|s| s.info.clone())
            .collect()
    }

    /// Gets download history.
    pub fn history(&self) -> Vec<DownloadHistoryEntry> {
        self.history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// Gets number of active downloads.
    pub fn active_count(&self) -> usize {
        *self
            .active_downloads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Shuts down the queue and signals active tasks to stop.
    pub fn shutdown(&self) {
        *self.shutdown.lock().unwrap_or_else(|e| e.into_inner()) = true;
        let _ = self.cancel_all();
        self.notifier.notify_all();
    }
}

impl Clone for DownloadQueue {
    fn clone(&self) -> Self {
        self.clone_inner()
    }
}

impl Default for DownloadQueue {
    fn default() -> Self {
        Self::new(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use tempfile::tempdir;

    #[test]
    fn test_download_queue_enqueue_dequeue() {
        let dir = tempdir().unwrap();
        let queue = DownloadQueue::new(Some(dir.path().to_path_buf()));

        let task = DownloadTask {
            id: "test1".into(),
            app_id: 123,
            label: "Test Game".into(),
            source: "lua.tools".into(),
            url: "https://example.com/test.zip".into(),
            dest_path: dir.path().join("test.zip"),
            total_bytes: Some(1000),
            priority: 0,
        };

        queue.enqueue(task.clone());
        let dequeued = queue.dequeue().unwrap();
        assert_eq!(dequeued.id, "test1");
    }

    #[test]
    fn test_download_queue_runner_execution_and_history() {
        let dir = tempdir().unwrap();
        let queue = DownloadQueue::new(Some(dir.path().to_path_buf()));
        queue.start_worker();

        let (done_tx, done_rx) = mpsc::channel();
        let task = DownloadTask {
            id: "runner-1".into(),
            app_id: 1245620,
            label: "ELDEN RING".into(),
            source: "hubcap".into(),
            url: "https://hubcapmanifest.com/api/v1/manifest/1245620".into(),
            dest_path: dir.path().join("1245620.zip"),
            total_bytes: Some(2048),
            priority: 10,
        };

        queue.enqueue_with_runner(task, move |ctx| {
            ctx.report_progress(0.5, "Downloading…", Some(1024));
            assert!(!ctx.is_cancelled());
            ctx.report_progress(1.0, "Done", Some(2048));
            let _ = done_tx.send(());
            Ok(Some(2048))
        });

        done_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        // Allow worker thread to finish recording history
        thread::sleep(Duration::from_millis(60));

        let hist = queue.history();
        assert!(!hist.is_empty());
        assert_eq!(hist[0].app_id, 1245620);
        assert_eq!(hist[0].status, "completed");
        assert_eq!(hist[0].bytes_downloaded, Some(2048));
        queue.shutdown();
    }

    #[test]
    fn test_download_queue_cancel_pending() {
        let dir = tempdir().unwrap();
        let queue = DownloadQueue::new(Some(dir.path().to_path_buf()));

        let task = DownloadTask {
            id: "pending-1".into(),
            app_id: 480,
            label: "Spacewar".into(),
            source: "morrenus".into(),
            url: "https://lua.tools/api/manifest/download?appid=480".into(),
            dest_path: dir.path().join("480.zip"),
            total_bytes: None,
            priority: 5,
        };

        queue.enqueue(task);
        assert_eq!(queue.pending_tasks().len(), 1);
        assert!(queue.cancel_task("pending-1"));
        assert!(queue.pending_tasks().is_empty());
        assert_eq!(queue.history()[0].status, "cancelled");
    }
}
