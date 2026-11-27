//! PluginLog - Logging for plugin operations
//!
//! Provides thread-safe in-memory and disk logging for plugin operations including
//! installation, updates, HTTP/RPC traffic, and runtime events, with a shared
//! process-wide `OnceLock` singleton and automatic 2 MiB log rotation (3 backups).

use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

/// Default maximum size of `plugin.log` before automatic rotation (2 MiB).
pub const DEFAULT_MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
/// Default number of rotated backups retained (`.log.1`, `.log.2`, `.log.3`).
pub const DEFAULT_MAX_ROTATED_BACKUPS: usize = 3;

static GLOBAL_PLUGIN_LOG: OnceLock<PluginLog> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: LogLevel,
    pub plugin_name: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogLevel::Debug => write!(f, "DEBUG"),
            LogLevel::Info => write!(f, "INFO"),
            LogLevel::Warn => write!(f, "WARN"),
            LogLevel::Error => write!(f, "ERROR"),
        }
    }
}

#[derive(Clone)]
pub struct PluginLog {
    max_entries: usize,
    max_file_bytes: u64,
    max_backups: usize,
    entries: Arc<Mutex<VecDeque<LogEntry>>>,
    file_lock: Arc<Mutex<()>>,
    log_file: Option<PathBuf>,
}

fn sanitize_log_field(s: &str) -> String {
    s.replace('\r', "\\r").replace('\n', "\\n")
}

fn rotated_backup_path(base: &Path, index: usize) -> PathBuf {
    let mut os = base.as_os_str().to_os_string();
    os.push(format!(".{}", index));
    PathBuf::from(os)
}

fn rotate_file_locked(log_file: &Path, max_file_bytes: u64, max_backups: usize) -> bool {
    if max_file_bytes == 0 {
        return false;
    }
    let current_len = fs::metadata(log_file).map(|m| m.len()).unwrap_or(0);
    if current_len < max_file_bytes {
        return false;
    }

    if max_backups == 0 {
        let _ = fs::remove_file(log_file);
        return true;
    }

    // Remove the oldest backup if present (.log.<max_backups>)
    let oldest = rotated_backup_path(log_file, max_backups);
    if oldest.exists() {
        let _ = fs::remove_file(&oldest);
    }

    // Shift .log.<i-1> -> .log.<i> from highest down to 2
    for idx in (2..=max_backups).rev() {
        let src = rotated_backup_path(log_file, idx - 1);
        let dst = rotated_backup_path(log_file, idx);
        if src.exists() {
            let _ = fs::rename(&src, &dst);
        }
    }

    // Move active log file -> .log.1
    let first_backup = rotated_backup_path(log_file, 1);
    fs::rename(log_file, &first_backup).is_ok()
}

impl PluginLog {
    /// Returns the shared process-wide `PluginLog` singleton (`OnceLock`).
    pub fn global() -> &'static PluginLog {
        GLOBAL_PLUGIN_LOG.get_or_init(|| {
            let log_dir = dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("luatools")
                .join("logs");
            let log_file = log_dir.join("plugin.log");
            let _ = fs::create_dir_all(&log_dir);
            Self::new(1000, Some(&log_file))
        })
    }

    pub fn new(max_entries: usize, log_file: Option<&Path>) -> Self {
        Self {
            max_entries,
            max_file_bytes: DEFAULT_MAX_LOG_BYTES,
            max_backups: DEFAULT_MAX_ROTATED_BACKUPS,
            entries: Arc::new(Mutex::new(VecDeque::new())),
            file_lock: Arc::new(Mutex::new(())),
            log_file: log_file.map(|p| p.to_path_buf()),
        }
    }

    /// Overrides the rotation size threshold (bytes) and retained backup count.
    pub fn with_rotation(mut self, max_file_bytes: u64, max_backups: usize) -> Self {
        self.max_file_bytes = max_file_bytes;
        self.max_backups = max_backups;
        self
    }

    pub fn log_path(&self) -> Option<&Path> {
        self.log_file.as_deref()
    }

    /// Manually triggers log rotation if the active log file has reached the rotation threshold.
    pub fn rotate_if_needed(&self) -> bool {
        let Some(ref log_file) = self.log_file else {
            return false;
        };
        let _guard = self.file_lock.lock().unwrap_or_else(|e| e.into_inner());
        rotate_file_locked(log_file, self.max_file_bytes, self.max_backups)
    }

    pub fn log(&self, level: LogLevel, plugin_name: Option<&str>, message: &str) {
        let entry = LogEntry {
            timestamp: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs().to_string())
                .unwrap_or_default(),
            level,
            plugin_name: plugin_name.map(|s| s.to_string()),
            message: message.to_string(),
        };

        if self.max_entries > 0 {
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            entries.push_back(entry.clone());
            while entries.len() > self.max_entries {
                entries.pop_front();
            }
        }

        if let Some(ref log_file) = self.log_file {
            let _guard = self.file_lock.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(parent) = log_file.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = rotate_file_locked(log_file, self.max_file_bytes, self.max_backups);
            let _ = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_file)
                .and_then(|mut f| {
                    let safe_msg = sanitize_log_field(&entry.message);
                    match entry.plugin_name.as_deref().filter(|s| !s.is_empty()) {
                        Some(name) => writeln!(
                            f,
                            "[{}] [{}] {}: {}",
                            entry.timestamp,
                            entry.level,
                            sanitize_log_field(name),
                            safe_msg
                        ),
                        None => writeln!(f, "[{}] [{}] {}", entry.timestamp, entry.level, safe_msg),
                    }
                });
        }
    }

    pub fn debug(&self, plugin_name: Option<&str>, message: &str) {
        self.log(LogLevel::Debug, plugin_name, message);
    }

    pub fn info(&self, plugin_name: Option<&str>, message: &str) {
        self.log(LogLevel::Info, plugin_name, message);
    }

    pub fn warn(&self, plugin_name: Option<&str>, message: &str) {
        self.log(LogLevel::Warn, plugin_name, message);
    }

    pub fn error(&self, plugin_name: Option<&str>, message: &str) {
        self.log(LogLevel::Error, plugin_name, message);
    }

    pub fn get_entries(&self) -> Vec<LogEntry> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    pub fn clear(&mut self) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

impl Default for PluginLog {
    fn default() -> Self {
        Self::global().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_log_entry() {
        let log = PluginLog::new(10, None);
        log.info(Some("TestPlugin"), "Test message");
        assert_eq!(log.get_entries().len(), 1);
        assert_eq!(log.get_entries()[0].level, LogLevel::Info);
        assert_eq!(log.get_entries()[0].message, "Test message");
    }

    #[test]
    fn test_log_levels() {
        let log = PluginLog::new(10, None);
        log.debug(Some("Plugin"), "debug");
        log.info(Some("Plugin"), "info");
        log.warn(Some("Plugin"), "warn");
        log.error(Some("Plugin"), "error");
        assert_eq!(log.get_entries().len(), 4);
    }

    #[test]
    fn test_max_entries() {
        let log = PluginLog::new(3, None);
        log.info(None, "msg1");
        log.info(None, "msg2");
        log.info(None, "msg3");
        log.info(None, "msg4");
        assert_eq!(log.get_entries().len(), 3);
        assert_eq!(log.get_entries()[0].message, "msg2");

        let zero = PluginLog::new(0, None);
        zero.info(None, "ignored");
        assert!(zero.get_entries().is_empty());
    }

    #[test]
    fn test_log_to_nested_file_sanitizes_newlines() {
        let dir = tempdir().unwrap();
        let log_file = dir.path().join("nested/logs/plugin.log");
        let log = PluginLog::new(10, Some(&log_file));
        log.info(Some("TestPlugin"), "line1\nline2");
        log.warn(None, "core warning");
        assert!(log_file.exists());
        let content = fs::read_to_string(&log_file).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("TestPlugin: line1\\nline2"));
        assert!(lines[1].ends_with("] [WARN] core warning"));
    }

    #[test]
    fn test_global_singleton_shares_state() {
        let g1 = PluginLog::global();
        let g2 = PluginLog::global();
        assert!(std::ptr::eq(g1, g2));
        let marker = format!("singleton-check-{}", std::process::id());
        g1.info(Some("UnitTest"), &marker);
        assert!(g2.get_entries().iter().any(|e| e.message == marker));
    }

    #[test]
    fn test_log_rotation_retains_up_to_three_backups() {
        let dir = tempdir().unwrap();
        let log_file = dir.path().join("plugin.log");
        // Small threshold (64 bytes) so every few lines triggers rotation
        let log = PluginLog::new(50, Some(&log_file)).with_rotation(64, 3);

        for i in 0..16 {
            log.info(
                Some("RotTest"),
                &format!("entry-{:02}-padding-to-exceed-threshold-cleanly", i),
            );
        }

        assert!(log_file.is_file());
        assert!(dir.path().join("plugin.log.1").is_file());
        assert!(dir.path().join("plugin.log.2").is_file());
        assert!(dir.path().join("plugin.log.3").is_file());
        assert!(!dir.path().join("plugin.log.4").exists());
    }
}
