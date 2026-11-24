//! PluginLog - Logging for plugin operations
//!
//! Provides logging functionality for plugin operations including
//! installation, updates, and runtime events.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

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
    entries: Arc<Mutex<VecDeque<LogEntry>>>,
    log_file: Option<PathBuf>,
}

fn sanitize_log_field(s: &str) -> String {
    s.replace('\r', "\\r").replace('\n', "\\n")
}

impl PluginLog {
    pub fn new(max_entries: usize, log_file: Option<&Path>) -> Self {
        Self {
            max_entries,
            entries: Arc::new(Mutex::new(VecDeque::new())),
            log_file: log_file.map(|p| p.to_path_buf()),
        }
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
            if let Some(parent) = log_file.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_file)
                .and_then(|mut f| {
                    use std::io::Write;
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
        Self::new(1000, None)
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
}
