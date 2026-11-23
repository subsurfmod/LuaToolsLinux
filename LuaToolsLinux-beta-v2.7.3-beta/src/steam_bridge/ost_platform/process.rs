//! Linux implementation of OSTPlatform::Process
//!
//! Provides process introspection via /proc filesystem.
//! Equivalent to Windows ToolHelp32/PSAPI functions.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Information about a loaded module in a process.
#[derive(Debug, Clone)]
pub struct ModuleInfo {
    pub path: String,
    pub native_path: PathBuf,
    pub size: u32,
    pub executable: bool,
}

/// Get process creation time (start time) from /proc/<pid>/stat.
/// Returns timestamp in seconds since epoch (Unix time).
pub fn get_creation_time(pid: u32) -> Option<u64> {
    let stat_path = format!("/proc/{}/stat", pid);
    let content = fs::read_to_string(&stat_path).ok()?;

    // stat format: pid (comm) state ppid ... starttime(22) ...
    let fields: Vec<&str> = content.split_whitespace().collect();
    if fields.len() < 22 {
        return None;
    }

    // Field 22 (0-indexed 21) is starttime in clock ticks
    let starttime_ticks: u64 = fields[21].parse().ok()?;

    // Get system boot time from /proc/stat
    let boot_time = get_boot_time()?;

    // Convert clock ticks to seconds (typically 100 ticks/sec on Linux)
    let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
    if clk_tck == 0 {
        return None;
    }

    let start_time_secs = boot_time + (starttime_ticks / clk_tck);
    Some(start_time_secs)
}

/// Get system boot time from /proc/stat.
fn get_boot_time() -> Option<u64> {
    let content = fs::read_to_string("/proc/stat").ok()?;
    for line in content.lines() {
        if line.starts_with("btime ") {
            return line[6..].trim().parse().ok();
        }
    }
    None
}

/// Format creation time as a human-readable string.
pub fn format_creation_time(timestamp: u64) -> String {
    use std::time::Duration;
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp as i64, 0);
    dt.map(|d| d.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "Unknown".to_string())
}

/// Get the executable path of a process.
pub fn get_image_path(pid: u32) -> Option<String> {
    let exe_path = format!("/proc/{}/exe", pid);
    fs::read_link(&exe_path)
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

/// Get an environment variable value from another process.
/// Reads /proc/<pid>/environ (null-separated key=value pairs).
pub fn get_environment_variable_value(pid: u32, name: &str) -> Option<String> {
    let environ_path = format!("/proc/{}/environ", pid);
    let content = fs::read(&environ_path).ok()?;

    // Split by null bytes
    let parts = content.split(|&b| b == 0);
    for part in parts {
        if let Ok(s) = std::str::from_utf8(part) {
            if let Some((key, value)) = s.split_once('=') {
                if key == name {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

/// Get the command line of a process.
/// Reads /proc/<pid>/cmdline (null-separated arguments).
pub fn get_process_command_line(pid: u32) -> Option<String> {
    let cmdline_path = format!("/proc/{}/cmdline", pid);
    let content = fs::read(&cmdline_path).ok()?;

    // Split by null bytes and join with spaces
    let args: Vec<String> = content
        .split(|&b| b == 0)
        .filter_map(|part| std::str::from_utf8(part).ok())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    if args.is_empty() {
        None
    } else {
        Some(args.join(" "))
    }
}

/// Enumerate modules (shared libraries) loaded by a process.
/// Parses /proc/<pid>/maps for memory mappings.
pub fn enumerate_modules(pid: u32) -> Vec<ModuleInfo> {
    let maps_path = format!("/proc/{}/maps", pid);
    let content = match fs::read_to_string(&maps_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut modules = Vec::new();
    let mut seen_paths: HashMap<String, ModuleInfo> = HashMap::new();

    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 6 {
            continue;
        }

        // Parse address range
        let range = parts[0];
        let perms = parts[1];
        let offset_str = parts[2];
        let path_str = parts[5..].join(" ");
        let path_clean = path_str.trim_end_matches(" (deleted)").trim();

        if path_clean.is_empty() || path_clean.starts_with('[') {
            // Skip anonymous mappings like [stack], [heap], [vdso], etc.
            continue;
        }

        let path = Path::new(path_clean);

        // Parse size from address range (start-end)
        let size = if let Some((start_str, end_str)) = range.split_once('-') {
            let start = u64::from_str_radix(start_str, 16).unwrap_or(0);
            let end = u64::from_str_radix(end_str, 16).unwrap_or(0);
            (end.saturating_sub(start)) as u32
        } else {
            0
        };

        let executable = perms.contains('x');

        // Deduplicate by path, keep the first (usually the main mapping)
        if let Some(existing) = seen_paths.get(path_clean) {
            // Merge sizes if this is a different mapping of the same file
            if existing.executable != executable {
                // Update if we found an executable mapping
                if executable {
                    seen_paths.insert(path_clean.to_string(), ModuleInfo {
                        path: path_clean.to_string(),
                        native_path: path.to_path_buf(),
                        size: existing.size + size,
                        executable: true,
                    });
                } else {
                    seen_paths.insert(path_clean.to_string(), ModuleInfo {
                        path: path_clean.to_string(),
                        native_path: path.to_path_buf(),
                        size: existing.size + size,
                        executable: existing.executable,
                    });
                }
            }
        } else {
            seen_paths.insert(path_clean.to_string(), ModuleInfo {
                path: path_clean.to_string(),
                native_path: path.to_path_buf(),
                size,
                executable,
            });
        }
    }

    modules.extend(seen_paths.into_values());
    modules.sort_by(|a, b| a.path.cmp(&b.path));
    modules
}

/// Check if a path is a system module path (under /usr/lib, /lib, etc.).
pub fn is_system_module_path(path: &str) -> bool {
    let path = Path::new(path);
    let system_dirs = ["/usr/lib", "/lib", "/lib64", "/usr/lib64", "/usr/lib32", "/lib32"];

    for dir in system_dirs {
        if path.starts_with(dir) {
            return true;
        }
    }
    false
}

/// Launch a detached, hidden process (equivalent to Windows CreateProcess with
/// DETACHED_PROCESS | CREATE_NO_WINDOW).
pub fn launch_detached_hidden(command_line: &str) -> bool {
    // Parse command line into program + args
    let parts: Vec<&str> = command_line.split_whitespace().collect();
    if parts.is_empty() {
        return false;
    }

    let program = parts[0];
    let args = &parts[1..];

    // Use setsid to detach from controlling terminal
    let result = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    match result {
        Ok(child) => {
            // On Unix, just don't wait on the child - it will be reaped by init
            // This is the equivalent of Windows DETACHED_PROCESS
            std::mem::forget(child);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::id;

    #[test]
    fn test_get_image_path_self() {
        let pid = id();
        let path = get_image_path(pid);
        assert!(path.is_some(), "Should get our own executable path");
        println!("Self exe path: {:?}", path);
    }

    #[test]
    fn test_get_command_line_self() {
        let pid = id();
        let cmdline = get_process_command_line(pid);
        assert!(cmdline.is_some(), "Should get our own command line");
        println!("Self cmdline: {:?}", cmdline);
    }

    #[test]
    fn test_enumerate_modules_self() {
        let pid = id();
        let modules = enumerate_modules(pid);
        assert!(!modules.is_empty(), "Should have at least one module");
        println!("Found {} modules", modules.len());
        for m in modules.iter().take(5) {
            println!("  {} (exec={}, size={})", m.path, m.executable, m.size);
        }
    }

    #[test]
    fn test_is_system_module() {
        assert!(is_system_module_path("/usr/lib/libc.so.6"));
        assert!(is_system_module_path("/lib/x86_64-linux-gnu/libm.so.6"));
        assert!(!is_system_module_path("/home/user/.local/share/Steam/linux64/steamclient.so"));
        assert!(!is_system_module_path("./my_binary"));
    }

    #[test]
    fn test_get_creation_time_self() {
        let pid = id();
        let creation = get_creation_time(pid);
        assert!(creation.is_some(), "Should get creation time");
        if let Some(ts) = creation {
            println!("Process start time: {}", format_creation_time(ts));
        }
    }
}