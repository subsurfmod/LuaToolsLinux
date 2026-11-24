//! Linux implementation of OSTPlatform::Process
//!
//! Provides process introspection via /proc filesystem.
//! Equivalent to Windows ToolHelp32/PSAPI functions.
//!
//! API compatibility: Matches the C++ header at
//! usefulsteamtools/src/OSTPlatform/include/Process.h

use std::collections::HashMap;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use thiserror::Error;

/// Error type for process operations.
#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("Process not found: PID {0}")]
    ProcessNotFound(u32),

    #[error("Permission denied accessing process {0}")]
    PermissionDenied(u32),

    #[error("Failed to read /proc/{pid}/{file}: {source}")]
    ProcReadError {
        pid: u32,
        file: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to parse process data: {0}")]
    ParseError(String),

    #[error("Invalid PID: {0}")]
    InvalidPid(u32),

    #[error("System time error: {0}")]
    SystemTimeError(#[from] std::time::SystemTimeError),

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),
}

/// Result type for process operations.
pub type Result<T> = std::result::Result<T, ProcessError>;

/// Information about a loaded module in a process.
/// Matches OSTPlatform::Process::ModuleInfo from the C++ header.
#[derive(Debug, Clone)]
pub struct ModuleInfo {
    pub path: String,
    pub native_path: PathBuf,
    pub size: u32,
    pub executable: bool,
}

impl ModuleInfo {
    /// Create a new ModuleInfo.
    pub fn new(path: String, size: u32, executable: bool) -> Self {
        let native_path = PathBuf::from(&path);
        Self {
            path,
            native_path,
            size,
            executable,
        }
    }
}

/// Convert Unix time (seconds since 1970) to FILETIME (100ns intervals since Jan 1, 1601).
/// This matches Windows FILETIME format.
const UNIX_EPOCH_TO_FILETIME_OFFSET: u64 = 116444736000000000; // 100ns intervals between 1601 and 1970

#[cfg(test)]
fn unix_time_to_filetime(unix_secs: u64) -> u64 {
    unix_secs
        .saturating_mul(10_000_000)
        .saturating_add(UNIX_EPOCH_TO_FILETIME_OFFSET)
}

/// Convert FILETIME (100ns intervals since Jan 1, 1601) to Unix time (seconds since 1970).
#[cfg(test)]
fn filetime_to_unix_time(filetime: u64) -> u64 {
    if filetime < UNIX_EPOCH_TO_FILETIME_OFFSET {
        return 0;
    }
    (filetime - UNIX_EPOCH_TO_FILETIME_OFFSET) / 10_000_000
}

fn ticks_and_boot_to_filetime(boot_time: u64, starttime_ticks: u64, clk_tck: u64) -> Option<u64> {
    if clk_tck == 0 {
        return None;
    }
    let ticks_100ns = (u128::from(starttime_ticks) * 10_000_000) / u128::from(clk_tck);
    let total = u128::from(boot_time)
        .checked_mul(10_000_000)?
        .checked_add(u128::from(UNIX_EPOCH_TO_FILETIME_OFFSET))?
        .checked_add(ticks_100ns)?;
    u64::try_from(total).ok()
}

fn map_proc_io_err(pid: u32, file: &str, e: std::io::Error) -> ProcessError {
    match e.kind() {
        std::io::ErrorKind::NotFound => ProcessError::ProcessNotFound(pid),
        std::io::ErrorKind::PermissionDenied => ProcessError::PermissionDenied(pid),
        _ => ProcessError::ProcReadError {
            pid,
            file: file.to_string(),
            source: e,
        },
    }
}

fn parse_maps_modules(content: &str) -> Vec<ModuleInfo> {
    let mut seen_paths: HashMap<String, ModuleInfo> = HashMap::new();

    for line in content.lines() {
        let mut rest = line.trim_start();
        let Some(ws1) = rest.find(char::is_whitespace) else {
            continue;
        };
        let range = &rest[..ws1];
        rest = rest[ws1..].trim_start();

        let Some(ws2) = rest.find(char::is_whitespace) else {
            continue;
        };
        let perms = &rest[..ws2];
        rest = rest[ws2..].trim_start();

        // Skip offset, dev, inode (3 fields)
        let mut ok = true;
        for _ in 0..3 {
            let Some(ws) = rest.find(char::is_whitespace) else {
                ok = false;
                break;
            };
            rest = rest[ws..].trim_start();
        }
        if !ok || rest.is_empty() {
            continue;
        }

        let path_clean = rest.strip_suffix(" (deleted)").unwrap_or(rest).trim();
        if path_clean.is_empty() || path_clean.starts_with('[') {
            // Skip anonymous mappings like [stack], [heap], [vdso], etc.
            continue;
        }

        let size = if let Some((start_str, end_str)) = range.split_once('-') {
            let start = u64::from_str_radix(start_str, 16).unwrap_or(0);
            let end = u64::from_str_radix(end_str, 16).unwrap_or(0);
            u32::try_from(end.saturating_sub(start)).unwrap_or(u32::MAX)
        } else {
            0
        };

        let executable = perms.contains('x');

        if let Some(existing) = seen_paths.get_mut(path_clean) {
            existing.size = existing.size.saturating_add(size);
            existing.executable |= executable;
        } else {
            seen_paths.insert(
                path_clean.to_string(),
                ModuleInfo {
                    path: path_clean.to_string(),
                    native_path: PathBuf::from(path_clean),
                    size,
                    executable,
                },
            );
        }
    }

    let mut modules: Vec<ModuleInfo> = seen_paths.into_values().collect();
    modules.sort_by(|a, b| a.path.cmp(&b.path));
    modules
}

fn split_command_line(command_line: &str) -> Option<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut token_started = false;

    for ch in command_line.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            token_started = true;
            continue;
        }

        if ch == '\\' && !in_single {
            escaped = true;
            token_started = true;
            continue;
        }

        if ch == '\'' && !in_double {
            in_single = !in_single;
            token_started = true;
            continue;
        }

        if ch == '"' && !in_single {
            in_double = !in_double;
            token_started = true;
            continue;
        }

        if ch.is_whitespace() && !in_single && !in_double {
            if token_started {
                args.push(std::mem::take(&mut current));
                token_started = false;
            }
            continue;
        }

        current.push(ch);
        token_started = true;
    }

    if in_single || in_double || escaped {
        return None;
    }
    if token_started {
        args.push(current);
    }
    if args.is_empty() {
        None
    } else {
        Some(args)
    }
}

/// Get process creation time (start time) from /proc/<pid>/stat.
/// Returns timestamp in FILETIME format (100-nanosecond intervals since January 1, 1601 UTC).
/// This matches the Windows API exactly.
///
/// # Arguments
/// * `pid` - Process ID
///
/// # Returns
/// FILETIME timestamp on success, or None if the process doesn't exist or can't be read.
fn parse_stat_starttime(content: &str) -> Option<u64> {
    // /proc/<pid>/stat format: `pid (comm may contain spaces) state ppid ... starttime(22)`
    // After the last ')', field 3 (state) is index 0, so field 22 (starttime) is index 19.
    let rparen = content.rfind(')')?;
    let after = content.get(rparen + 1..)?;
    after.split_whitespace().nth(19)?.parse().ok()
}

pub fn get_creation_time(pid: u32) -> Option<u64> {
    if pid == 0 {
        return None;
    }

    let stat_path = format!("/proc/{}/stat", pid);
    let content = fs::read_to_string(&stat_path).ok()?;
    let starttime_ticks = parse_stat_starttime(&content)?;

    let boot_time = get_boot_time()?;
    let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if clk_tck <= 0 {
        return None;
    }

    ticks_and_boot_to_filetime(boot_time, starttime_ticks, clk_tck as u64)
}

/// Get system boot time from /proc/stat (Unix timestamp in seconds).
fn get_boot_time() -> Option<u64> {
    let content = fs::read_to_string("/proc/stat").ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("btime ") {
            return rest.trim().parse().ok();
        }
    }
    None
}

/// Format a FILETIME timestamp as a human-readable string.
/// Matches OSTPlatform::Process::FormatCreationTime from the C++ header.
///
/// # Arguments
/// * `file_time` - FILETIME timestamp (100ns intervals since Jan 1, 1601)
///
/// # Returns
/// Formatted string like "2024-01-15 14:30:45.123" or "-" if file_time is 0.
pub fn format_creation_time(file_time: u64) -> String {
    if file_time == 0 {
        return "-".to_string();
    }

    if file_time < UNIX_EPOCH_TO_FILETIME_OFFSET {
        return format!("filetime={}", file_time);
    }

    let since_unix_100ns = file_time - UNIX_EPOCH_TO_FILETIME_OFFSET;
    let Ok(unix_secs) = i64::try_from(since_unix_100ns / 10_000_000) else {
        return format!("filetime={}", file_time);
    };
    let nanos = ((since_unix_100ns % 10_000_000) * 100) as u32;

    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(unix_secs, nanos);
    dt.map(|d| d.format("%Y-%m-%d %H:%M:%S%.3f").to_string())
        .unwrap_or_else(|| format!("filetime={}", file_time))
}

/// Get the executable path of a process.
/// Reads /proc/<pid>/exe symlink.
///
/// # Arguments
/// * `pid` - Process ID
///
/// # Returns
/// Executable path as UTF-8 string, or None if not found.
pub fn get_image_path(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }

    let exe_path = format!("/proc/{}/exe", pid);
    fs::read_link(&exe_path)
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

/// Get an environment variable value from another process.
/// Reads /proc/<pid>/environ (null-separated key=value pairs).
///
/// # Arguments
/// * `pid` - Process ID
/// * `name` - Environment variable name (UTF-8)
///
/// # Returns
/// Variable value as UTF-8 string, or None if not found or inaccessible.
pub fn get_environment_variable_value(pid: u32, name: &str) -> Option<String> {
    if pid == 0 || name.is_empty() {
        return None;
    }

    let environ_path = format!("/proc/{}/environ", pid);
    let content = fs::read(&environ_path).ok()?;

    // Split by null bytes
    for part in content.split(|&b| b == 0) {
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
///
/// # Arguments
/// * `pid` - Process ID
///
/// # Returns
/// Command line as a single UTF-8 string (arguments joined by spaces),
/// or None if not found or inaccessible.
pub fn get_process_command_line(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }

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
///
/// # Arguments
/// * `pid` - Process ID
///
/// # Returns
/// Vector of ModuleInfo, sorted by path. Empty on error.
pub fn enumerate_modules(pid: u32) -> Vec<ModuleInfo> {
    if pid == 0 {
        return Vec::new();
    }

    let maps_path = format!("/proc/{}/maps", pid);
    let content = match fs::read_to_string(&maps_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    parse_maps_modules(&content)
}

/// Check if a path is a system module path (under /usr/lib, /lib, etc.).
/// Matches OSTPlatform::Process::IsSystemModulePath from the C++ header.
///
/// # Arguments
/// * `path` - Module path to check
///
/// # Returns
/// True if the path is under a system library directory.
pub fn is_system_module_path(path: &str) -> bool {
    let path = Path::new(path);
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }

    let system_dirs = [
        "/usr/lib",
        "/lib",
        "/lib64",
        "/usr/lib64",
        "/usr/lib32",
        "/lib32",
        "/usr/lib/x86_64-linux-gnu",
        "/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/lib/aarch64-linux-gnu",
    ];

    for dir in system_dirs {
        if path.starts_with(dir) {
            return true;
        }
    }
    false
}

/// Launch a detached, hidden process (equivalent to Windows CreateProcess with
/// DETACHED_PROCESS | CREATE_NO_WINDOW).
/// Creates a new session via `setsid()` and reaps the child in a detached reaper
/// thread so no zombie `<defunct>` processes accumulate.
///
/// # Arguments
/// * `command_line` - Full command line to execute (program + args, supports quotes)
///
/// # Returns
/// True if the process was created successfully.
pub fn launch_detached_hidden(command_line: &str) -> bool {
    let Some(parts) = split_command_line(command_line.trim()) else {
        return false;
    };

    let program = &parts[0];
    let args = &parts[1..];

    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    // Safety: setsid() is async-signal-safe per POSIX.1-2008.
    unsafe {
        cmd.pre_exec(|| {
            let _ = libc::setsid();
            Ok(())
        });
    }

    match cmd.spawn() {
        Ok(mut child) => {
            // Reap the detached child asynchronously so it never lingers as a zombie.
            let _ = std::thread::Builder::new()
                .name("ost-child-reaper".to_string())
                .spawn(move || {
                    let _ = child.wait();
                });
            true
        }
        Err(_) => false,
    }
}

/// Get creation time with full error information.
/// This is a helper that returns the ProcessError for better diagnostics.
pub fn get_creation_time_checked(pid: u32) -> Result<u64> {
    if pid == 0 {
        return Err(ProcessError::InvalidPid(pid));
    }

    let stat_path = format!("/proc/{}/stat", pid);
    let content = fs::read_to_string(&stat_path).map_err(|e| map_proc_io_err(pid, "stat", e))?;

    let starttime_ticks = parse_stat_starttime(&content).ok_or_else(|| {
        ProcessError::ParseError("failed to parse starttime from /proc/stat".into())
    })?;

    let boot_time = get_boot_time().ok_or(ProcessError::ParseError(
        "boot time not found in /proc/stat".into(),
    ))?;

    let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if clk_tck <= 0 {
        return Err(ProcessError::ParseError(
            "sysconf(_SC_CLK_TCK) returned <= 0".into(),
        ));
    }

    ticks_and_boot_to_filetime(boot_time, starttime_ticks, clk_tck as u64)
        .ok_or_else(|| ProcessError::ParseError("overflow computing process FILETIME".into()))
}

/// Get image path with full error information.
pub fn get_image_path_checked(pid: u32) -> Result<String> {
    if pid == 0 {
        return Err(ProcessError::InvalidPid(pid));
    }

    let exe_path = format!("/proc/{}/exe", pid);
    fs::read_link(&exe_path)
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| map_proc_io_err(pid, "exe", e))
}

/// Get environment variable with full error information.
pub fn get_environment_variable_value_checked(pid: u32, name: &str) -> Result<Option<String>> {
    if pid == 0 {
        return Err(ProcessError::InvalidPid(pid));
    }
    if name.is_empty() {
        return Ok(None);
    }

    let environ_path = format!("/proc/{}/environ", pid);
    let content = fs::read(&environ_path).map_err(|e| map_proc_io_err(pid, "environ", e))?;

    for part in content.split(|&b| b == 0) {
        if let Ok(s) = std::str::from_utf8(part) {
            if let Some((key, value)) = s.split_once('=') {
                if key == name {
                    return Ok(Some(value.to_string()));
                }
            }
        }
    }
    Ok(None)
}

/// Get command line with full error information.
pub fn get_process_command_line_checked(pid: u32) -> Result<Option<String>> {
    if pid == 0 {
        return Err(ProcessError::InvalidPid(pid));
    }

    let cmdline_path = format!("/proc/{}/cmdline", pid);
    let content = fs::read(&cmdline_path).map_err(|e| map_proc_io_err(pid, "cmdline", e))?;

    let args: Vec<String> = content
        .split(|&b| b == 0)
        .filter_map(|part| std::str::from_utf8(part).ok())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    Ok(if args.is_empty() {
        None
    } else {
        Some(args.join(" "))
    })
}

/// Enumerate modules with full error information.
pub fn enumerate_modules_checked(pid: u32) -> Result<Vec<ModuleInfo>> {
    if pid == 0 {
        return Err(ProcessError::InvalidPid(pid));
    }

    let maps_path = format!("/proc/{}/maps", pid);
    let content = fs::read_to_string(&maps_path).map_err(|e| map_proc_io_err(pid, "maps", e))?;
    Ok(parse_maps_modules(&content))
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
    fn test_get_image_path_checked_self() {
        let pid = id();
        let result = get_image_path_checked(pid);
        assert!(
            result.is_ok(),
            "Should get our own executable path: {:?}",
            result
        );
        println!("Self exe path (checked): {:?}", result.unwrap());
    }

    #[test]
    fn test_get_command_line_self() {
        let pid = id();
        let cmdline = get_process_command_line(pid);
        assert!(cmdline.is_some(), "Should get our own command line");
        println!("Self cmdline: {:?}", cmdline);
    }

    #[test]
    fn test_get_command_line_checked_self() {
        let pid = id();
        let result = get_process_command_line_checked(pid);
        assert!(
            result.is_ok(),
            "Should get our own command line: {:?}",
            result
        );
        println!("Self cmdline (checked): {:?}", result.unwrap());
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
    fn test_enumerate_modules_checked_self() {
        let pid = id();
        let result = enumerate_modules_checked(pid);
        assert!(result.is_ok(), "Should enumerate modules: {:?}", result);
        let modules = result.unwrap();
        assert!(!modules.is_empty(), "Should have at least one module");
        println!("Found {} modules (checked)", modules.len());
    }

    #[test]
    fn test_is_system_module() {
        assert!(is_system_module_path("/usr/lib/libc.so.6"));
        assert!(is_system_module_path("/lib/x86_64-linux-gnu/libm.so.6"));
        assert!(is_system_module_path("/usr/lib64/libstdc++.so.6"));
        assert!(is_system_module_path("/lib/aarch64-linux-gnu/libc.so.6"));
        assert!(!is_system_module_path(
            "/home/user/.local/share/Steam/linux64/steamclient.so"
        ));
        assert!(!is_system_module_path("./my_binary"));
        assert!(!is_system_module_path("/opt/myapp/libfoo.so"));
    }

    #[test]
    fn test_get_creation_time_self() {
        let pid = id();
        let creation = get_creation_time(pid);
        assert!(creation.is_some(), "Should get creation time");
        if let Some(ts) = creation {
            println!("Process start time (FILETIME): {}", ts);
            println!("Formatted: {}", format_creation_time(ts));
            // Verify it's a reasonable FILETIME (after year 2000)
            assert!(
                ts > unix_time_to_filetime(946684800),
                "FILETIME should be after year 2000"
            );
        }
    }

    #[test]
    fn test_get_creation_time_checked_self() {
        let pid = id();
        let result = get_creation_time_checked(pid);
        assert!(result.is_ok(), "Should get creation time: {:?}", result);
        let ts = result.unwrap();
        println!("Process start time (FILETIME, checked): {}", ts);
        println!("Formatted: {}", format_creation_time(ts));
    }

    #[test]
    fn test_format_creation_time() {
        // Test zero
        assert_eq!(format_creation_time(0), "-");

        // Test a known time: 2024-01-15 14:30:45 UTC
        // Unix timestamp: 1705329045
        let filetime = unix_time_to_filetime(1705329045);
        let formatted = format_creation_time(filetime);
        println!("Formatted test time: {}", formatted);
        // Should contain the date
        assert!(formatted.contains("2024-01-15"));
    }

    #[test]
    fn test_unix_to_filetime_conversion() {
        // Unix epoch (1970-01-01 00:00:00) should convert to Windows epoch offset
        assert_eq!(unix_time_to_filetime(0), UNIX_EPOCH_TO_FILETIME_OFFSET);

        // Round-trip conversion
        let unix_time = 1705329045u64; // 2024-01-15 14:30:45
        let filetime = unix_time_to_filetime(unix_time);
        let back = filetime_to_unix_time(filetime);
        // Allow 1 second difference due to 100ns precision
        assert!((unix_time as i64 - back as i64).abs() <= 1);
    }

    #[test]
    fn test_get_environment_variable_self() {
        let pid = id();
        // Test with a known environment variable
        let home = get_environment_variable_value(pid, "HOME");
        // HOME might not be set in all test environments, so just check it doesn't panic
        println!("HOME: {:?}", home);

        // Test with non-existent variable
        let none = get_environment_variable_value(pid, "THIS_VAR_DEFINITELY_DOES_NOT_EXIST_12345");
        assert_eq!(none, None);
    }

    #[test]
    fn test_get_environment_variable_checked_self() {
        let pid = id();
        let result = get_environment_variable_value_checked(pid, "HOME");
        assert!(result.is_ok(), "Should get env var: {:?}", result);
        println!("HOME (checked): {:?}", result.unwrap());

        let result2 = get_environment_variable_value_checked(pid, "");
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap(), None);
    }

    #[test]
    fn test_launch_detached_hidden() {
        // Test launching a simple command that exits quickly
        // Using `true` which is a no-op command
        let result = launch_detached_hidden("true");
        // This should succeed on any Unix system
        assert!(result, "Should be able to launch 'true'");

        // Test with empty command line
        let result = launch_detached_hidden("");
        assert!(!result, "Empty command line should fail");

        // Test with invalid command
        let result = launch_detached_hidden("/this/path/does/not/exist/12345");
        assert!(!result, "Invalid command should fail");
    }

    #[test]
    fn test_module_info_creation() {
        let mi = ModuleInfo::new("/test/path.so".to_string(), 4096, true);
        assert_eq!(mi.path, "/test/path.so");
        assert_eq!(mi.size, 4096);
        assert!(mi.executable);
        assert_eq!(mi.native_path, PathBuf::from("/test/path.so"));
    }

    #[test]
    fn test_process_error_display() {
        let err = ProcessError::ProcessNotFound(1234);
        assert_eq!(err.to_string(), "Process not found: PID 1234");

        let err = ProcessError::PermissionDenied(5678);
        assert_eq!(err.to_string(), "Permission denied accessing process 5678");

        let err = ProcessError::InvalidPid(0);
        assert_eq!(err.to_string(), "Invalid PID: 0");
    }

    #[test]
    fn test_get_invalid_pid() {
        let result = get_creation_time_checked(0);
        assert!(matches!(result, Err(ProcessError::InvalidPid(0))));

        let result = get_image_path_checked(0);
        assert!(matches!(result, Err(ProcessError::InvalidPid(0))));

        let result = get_environment_variable_value_checked(0, "TEST");
        assert!(matches!(result, Err(ProcessError::InvalidPid(0))));

        let result = get_process_command_line_checked(0);
        assert!(matches!(result, Err(ProcessError::InvalidPid(0))));

        let result = enumerate_modules_checked(0);
        assert!(matches!(result, Err(ProcessError::InvalidPid(0))));
    }

    #[test]
    fn test_enumerate_modules_empty_pid() {
        let modules = enumerate_modules(0);
        assert!(modules.is_empty(), "PID 0 should return empty module list");
    }

    #[test]
    fn test_get_creation_time_invalid_pid() {
        let creation = get_creation_time(0);
        assert!(creation.is_none(), "PID 0 should return None");
    }

    #[test]
    fn test_get_image_path_invalid_pid() {
        let path = get_image_path(0);
        assert!(path.is_none(), "PID 0 should return None");
    }

    #[test]
    fn test_get_command_line_invalid_pid() {
        let cmdline = get_process_command_line(0);
        assert!(cmdline.is_none(), "PID 0 should return None");
    }

    #[test]
    fn test_get_env_var_invalid_pid() {
        let value = get_environment_variable_value(0, "TEST");
        assert!(value.is_none(), "PID 0 should return None");
    }

    #[test]
    fn test_format_creation_time_subsecond_and_epoch() {
        // Unix epoch itself should format as 1970-01-01 00:00:00.000
        assert_eq!(
            format_creation_time(UNIX_EPOCH_TO_FILETIME_OFFSET),
            "1970-01-01 00:00:00.000"
        );
        // 123 milliseconds past 2024-01-15 14:30:45 UTC
        let ft = unix_time_to_filetime(1_705_329_045) + 1_230_000;
        assert_eq!(format_creation_time(ft), "2024-01-15 14:30:45.123");

        // Sub-second tick precision preserved
        let ft_ticks = ticks_and_boot_to_filetime(1_705_329_045, 50, 100).unwrap();
        assert_eq!(format_creation_time(ft_ticks), "2024-01-15 14:30:45.500");
    }

    #[test]
    fn test_parse_maps_modules_saturating_size_and_spaces() {
        let maps = "\
0000000000001000-0000000100001000 r-xp 00000000 08:01 1 /opt/my  game/libhuge.so\n\
0000000100001000-0000000100002000 rw-p 00000000 08:01 1 /opt/my  game/libhuge.so\n\
00007fff00000000-00007fff00021000 rw-p 00000000 00:00 0 [stack]\n";
        let mods = parse_maps_modules(maps);
        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].path, "/opt/my  game/libhuge.so");
        assert_eq!(mods[0].size, u32::MAX);
        assert!(mods[0].executable);
    }

    #[test]
    fn test_is_system_module_rejects_parent_traversal() {
        assert!(!is_system_module_path("/usr/lib/../../tmp/evil.so"));
    }

    #[test]
    fn test_split_command_line_quotes() {
        assert_eq!(
            split_command_line("\"/usr/bin/my app\" --flag 'hello world'").unwrap(),
            vec!["/usr/bin/my app", "--flag", "hello world"]
        );
        assert!(split_command_line("\"unterminated").is_none());
    }
}
