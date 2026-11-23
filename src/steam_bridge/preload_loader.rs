//! Preload loader for the Steam client integration library.
//!
//! On Linux the cleanest way to load integration code into the Steam client is
//! to set `LD_PRELOAD` (or `/etc/ld.so.preload`) so the dynamic linker loads
//! the shared object into every process, including Steam. The shared object
//! exports a constructor that runs at load time and installs the memory patches.
//!
//! This module:
//! - Discovers the Steam process and its libraries
//! - Writes an `ld.so.preload` entry (system-wide, needs root) or exports
//!   `LD_PRELOAD` for user-session launches
//! - Generates the integration shared library's C entry point
//! - Provides a runtime `LD_PRELOAD` launcher wrapper for Steam

use std::fs;
use std::path::{Path, PathBuf};

/// Error type for preload operations.
#[derive(Debug, thiserror::Error)]
pub enum PreloadError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Steam not running")]
    SteamNotRunning,
    #[error("Permission denied (needs root for /etc/ld.so.preload)")]
    PermissionDenied,
    #[error("Integration library not found: {0}")]
    IntegrationLibNotFound(String),
}

pub const LD_SO_PRELOAD_PATH: &str = "/etc/ld.so.preload";

pub fn find_steam_pid() -> Option<u32> {
    if let Ok(entries) = fs::read_dir("/proc") {
        for e in entries.flatten() {
            let pid_str = e.file_name().to_string_lossy().to_string();
            let Ok(pid): Result<u32, _> = pid_str.parse() else {
                continue;
            };
            let cmdline_path = format!("/proc/{}/cmdline", pid);
            if let Ok(cmdline) = fs::read_to_string(&cmdline_path) {
                if cmdline.to_lowercase().contains("steam") {
                    return Some(pid);
                }
            }
            let exe_path = format!("/proc/{}/exe", pid);
            if let Ok(exe) = fs::read_link(&exe_path) {
                let exe_str = exe.to_string_lossy().to_lowercase();
                if exe_str.contains("steam") {
                    return Some(pid);
                }
            }
        }
    }
    None
}

pub fn is_loaded(pid: u32, lib_name: &str) -> bool {
    let maps_path = format!("/proc/{}/maps", pid);
    if let Ok(maps) = fs::read_to_string(&maps_path) {
        maps.lines().any(|l| l.contains(lib_name))
    } else {
        false
    }
}

pub fn enable_system_preload(lib_path: &Path) -> Result<(), PreloadError> {
    let lib_str = lib_path.to_string_lossy().to_string();
    let current = fs::read_to_string(LD_SO_PRELOAD_PATH).unwrap_or_default();
    if current.lines().any(|l| l.trim() == lib_str) {
        return Ok(());
    }
    let new_content = if current.trim().is_empty() {
        lib_str
    } else {
        format!("{}\n{}", current.trim(), lib_str)
    };
    fs::write(LD_SO_PRELOAD_PATH, new_content)?;
    Ok(())
}

pub fn disable_system_preload(lib_path: &Path) -> Result<(), PreloadError> {
    let lib_str = lib_path.to_string_lossy().to_string();
    let current = fs::read_to_string(LD_SO_PRELOAD_PATH).unwrap_or_default();
    let filtered: String = current
        .lines()
        .filter(|l| l.trim() != lib_str && !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        LD_SO_PRELOAD_PATH,
        if filtered.is_empty() {
            String::new()
        } else {
            filtered + "\n"
        },
    )?;
    Ok(())
}

pub fn generate_ld_preload_launcher(
    steam_exe: &Path,
    lib_path: &Path,
    output_script: &Path,
) -> Result<(), PreloadError> {
    let steam_str = steam_exe.to_string_lossy().to_string();
    let lib_str = lib_path.to_string_lossy().to_string();
    let script = format!(
        "#!/bin/sh\n\
         # Steam integration launcher\n\
         # Preloads the integration shared library into the Steam client process.\n\
         export LD_PRELOAD=\"{lib}\":${{LD_PRELOAD:-}}\n\
         export LD_LIBRARY_PATH=\"{lib_dir}\":${{LD_LIBRARY_PATH:-}}\n\
         exec \"{steam}\" \"$@\"\n",
        lib = lib_str,
        lib_dir = lib_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_string_lossy(),
        steam = steam_str,
    );
    fs::write(output_script, script)?;
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(output_script)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(output_script, perms)?;
    Ok(())
}

pub fn default_integration_lib_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/lib/luatools/libsteam_integration.so")
}

pub fn integration_lib_name() -> &'static str {
    "libsteam_integration.so"
}

pub fn steam_integrated() -> bool {
    let pid = match find_steam_pid() {
        Some(p) => p,
        None => return false,
    };
    is_loaded(pid, integration_lib_name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_steam_pid_returns_option() {
        let _ = find_steam_pid();
    }

    #[test]
    fn integration_lib_name_is_non_empty() {
        assert!(!integration_lib_name().is_empty());
    }
}
