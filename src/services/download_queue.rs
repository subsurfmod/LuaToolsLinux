//! DownloadQueue — Centralized download worker managing concurrency, sliding-window ETA,
//! history, and temp file cleanup (LT-38).

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Condvar};
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

#[derive(Debug)]
pub struct DownloadQueue {
    tasks: Arc<Mutex<VecDeque<DownloadTask>>>,
    history: Arc<Mutex<VecDeque<DownloadHistoryEntry>>>,
    active_downloads: Arc<Mutex<usize>>,
    notifier: Arc<Condvar>,
    shutdown: Arc<Mutex<bool>>,
    history_path: PathBuf,
    cache_dir: PathBuf,
    speed_samples: Arc<Mutex<VecDeque<(Instant, u64)>>>,
}

impl DownloadQueue {
    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        let cache = cache_dir.unwrap_or_else(default_luatools_cache_dir);
        let history_path = cache.join(HISTORY_FILE);
        let history = Self::load_history(&history_path);

        Self {
            tasks: Arc::new(Mutex::new(VecDeque::new())),
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
        let history = self.history.lock().unwrap();
        let tmp = self.history_path.with_extension("json.tmp");
        if let Ok(content) = serde_json::to_string_pretty(&*history) {
            let _ = fs::write(&tmp, content);
            let _ = fs::rename(&tmp, &self.history_path);
        }
    }

    /// Adds a task to the download queue.
    pub fn enqueue(&self, task: DownloadTask) {
        let mut tasks = self.tasks.lock().unwrap();
        // Insert by priority (higher priority first)
        let pos = tasks.iter().position(|t| t.priority < task.priority).unwrap_or(tasks.len());
        tasks.insert(pos, task);
        self.notifier.notify_one();
    }

    /// Gets the next task from the queue.
    fn dequeue(&self) -> Option<DownloadTask> {
        let mut tasks = self.tasks.lock().unwrap();
        tasks.pop_front()
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
            // Check shutdown
            if *self.shutdown.lock().unwrap() {
                break;
            }

            // Check if we can start a new download
            let can_start = {
                let active = self.active_downloads.lock().unwrap();
                *active < MAX_CONCURRENT_DOWNLOADS
            };

            if !can_start {
                // Wait for a slot to open
                let active = self.active_downloads.lock().unwrap();
                let _ = self.notifier.wait_timeout(active, Duration::from_millis(500));
                continue;
            }

            // Get next task
            let task = self.dequeue();
            let Some(task) = task else {
                // No tasks, wait for notification
                let tasks = self.tasks.lock().unwrap();
                let _ = self.notifier.wait_timeout(tasks, Duration::from_secs(1));
                continue;
            };

            // Start download
            *self.active_downloads.lock().unwrap() += 1;

            let queue = self.clone_inner();
            thread::spawn(move || {
                queue.execute_download(task);
                *queue.active_downloads.lock().unwrap() -= 1;
                queue.notifier.notify_all();
            });
        }
    }

    fn execute_download(&self, task: DownloadTask) {
        let start = Instant::now();
        let mut bytes_downloaded = 0u64;

        // Handle manifest:// custom protocol - these should be downloaded via
        // dedicated functions (HubcapService::download_manifest or
        // LuaToolsApiClient::download_manifest) rather than the generic HTTP queue.
        if task.url.starts_with("manifest://") {
            self.record_history(
                task.app_id,
                &task.label,
                &task.source,
                "failed",
                start.elapsed(),
                None,
                Some("manifest:// URLs must be handled by dedicated manifest download functions, not the generic download queue".into()),
            );
            return;
        }

        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .build();

        let _result = match agent.get(&task.url).call() {
            Ok(resp) => {
                let _total = resp.header("Content-Length")
                    .and_then(|v| v.parse::<u64>().ok());

                fs::create_dir_all(task.dest_path.parent().unwrap()).ok();

                let mut file = match fs::File::create(&task.dest_path) {
                    Ok(f) => f,
                    Err(e) => {
                        self.record_history(task.app_id, &task.label, &task.source, "failed", start.elapsed(), None, Some(e.to_string()));
                        return;
                    }
                };

                let mut reader = resp.into_reader();
                let mut buffer = vec![0u8; 8192];

                loop {
                    if *self.shutdown.lock().unwrap() {
                        break;
                    }

                    match std::io::Read::read(&mut reader, &mut buffer) {
                        Ok(0) => break, // EOF
                        Ok(n) => {
                            if let Err(_) = std::io::Write::write_all(&mut file, &buffer[..n]) {
                                self.record_history(task.app_id, &task.label, &task.source, "failed", start.elapsed(), Some(bytes_downloaded), Some("Write failed".into()));
                                return;
                            }
                            bytes_downloaded += n as u64;

                            // Record speed sample
                            let mut samples = self.speed_samples.lock().unwrap();
                            samples.push_back((Instant::now(), bytes_downloaded));
                            if samples.len() > 60 {
                                samples.pop_front();
                            }
                        }
                        Err(e) => {
                            self.record_history(task.app_id, &task.label, &task.source, "failed", start.elapsed(), Some(bytes_downloaded), Some(e.to_string()));
                            return;
                        }
                    }
                }

                self.record_history(task.app_id, &task.label, &task.source, "completed", start.elapsed(), Some(bytes_downloaded), None);
            }
            Err(e) => {
                self.record_history(task.app_id, &task.label, &task.source, "failed", start.elapsed(), Some(bytes_downloaded), Some(e.to_string()));
            }
        };

        // Cleanup temp files older than 24 hours
        self.cleanup_temp_files();
    }

    fn record_history(&self, app_id: u32, label: &str, source: &str, status: &str, duration: Duration, bytes: Option<u64>, error: Option<String>) {
        let mut history = self.history.lock().unwrap();
        let entry = DownloadHistoryEntry {
            app_id,
            label: label.to_string(),
            source: source.to_string(),
            status: if let Some(err) = error { format!("failed: {}", err) } else { status.to_string() },
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
        let samples = self.speed_samples.lock().unwrap();
        if samples.len() < 2 {
            return None;
        }
        let (first_time, first_bytes) = samples.front()?;
        let (last_time, last_bytes) = samples.back()?;
        let elapsed = last_time.duration_since(*first_time).as_secs_f64();
        if elapsed < 1.0 {
            return None;
        }
        Some(((last_bytes - first_bytes) as f64 / elapsed) as u64)
    }

    /// Gets the estimated time remaining for the current download.
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

    /// Gets all pending tasks.
    pub fn pending_tasks(&self) -> Vec<DownloadTask> {
        self.tasks.lock().unwrap().iter().cloned().collect()
    }

    /// Gets download history.
    pub fn history(&self) -> Vec<DownloadHistoryEntry> {
        self.history.lock().unwrap().iter().cloned().collect()
    }

    /// Gets number of active downloads.
    pub fn active_count(&self) -> usize {
        *self.active_downloads.lock().unwrap()
    }

    /// Shuts down the queue.
    pub fn shutdown(&self) {
        *self.shutdown.lock().unwrap() = true;
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
}