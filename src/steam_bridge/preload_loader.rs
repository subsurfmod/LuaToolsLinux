//! Preload loader for the Steam client integration library.
//!
//! On Linux the cleanest way to load integration code into the Steam client is
//! to set `LD_PRELOAD` (or `/etc/ld.so.preload`) so the dynamic linker loads
//! the shared object into the Steam client process. The shared object exports
//! a constructor that verifies it is running inside the main Steam client process
//! before initializing IPC and memory integration.
//!
//! This module:
//! - Discovers the main Steam client process (excluding `steamwebhelper`, launchers, etc.)
//! - Writes an `ld.so.preload` entry (system-wide, needs root) or exports
//!   `LD_PRELOAD` for user-session launches
//! - Generates a runtime `LD_PRELOAD` launcher wrapper for Steam with `--gdb` / `--lldb` support

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

/// Returns `true` only when `/proc/<pid>/comm` and `/proc/<pid>/exe` identify the actual
/// Steam client binary (`steam`), excluding `steamwebhelper`, launcher wrappers, `steam.sh`,
/// `steam-sls`, `luatools`, shells, and Steam Runtime helper processes.
pub fn is_steam_client_process(comm: &str, exe_path: Option<&Path>) -> bool {
    let comm_trim = comm.trim();
    let exe_name = exe_path
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("");

    // Explicitly exclude helper / browser / wrapper processes even if they contain "steam"
    let lower_comm = comm_trim.to_ascii_lowercase();
    let lower_exe = exe_name.to_ascii_lowercase();
    if lower_comm.contains("webhelper")
        || lower_exe.contains("webhelper")
        || lower_comm.contains("launcher")
        || lower_exe.contains("launcher")
        || lower_comm.contains("luatools")
        || lower_exe.contains("luatools")
        || lower_exe.ends_with(".sh")
        || matches!(
            lower_exe.as_str(),
            "sh" | "bash"
                | "dash"
                | "zsh"
                | "fish"
                | "gdb"
                | "lldb"
                | "bwrap"
                | "srt-bwrap"
                | "reaper"
        )
    {
        return false;
    }

    if exe_name == "steam" {
        return true;
    }

    // Fallback if /proc/<pid>/exe cannot be read (e.g. different permissions)
    exe_path.is_none() && comm_trim == "steam"
}

pub fn find_steam_pid() -> Option<u32> {
    let my_pid = std::process::id();
    let mut fallback_pid = None;

    if let Ok(entries) = fs::read_dir("/proc") {
        for e in entries.flatten() {
            let pid_str = e.file_name().to_string_lossy().to_string();
            let Ok(pid): Result<u32, _> = pid_str.parse() else {
                continue;
            };
            if pid == my_pid {
                continue;
            }
            let comm = fs::read_to_string(format!("/proc/{}/comm", pid)).unwrap_or_default();
            let exe = fs::read_link(format!("/proc/{}/exe", pid)).ok();

            if is_steam_client_process(&comm, exe.as_deref()) {
                // Prefer the 64-bit steamrt64/steam main process when both 32-bit and 64-bit exist
                if exe
                    .as_ref()
                    .map(|p| p.to_string_lossy().contains("steamrt64"))
                    .unwrap_or(false)
                {
                    return Some(pid);
                }
                if fallback_pid.is_none() {
                    fallback_pid = Some(pid);
                }
            }
        }
    }
    fallback_pid
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
        format!("{}\n", lib_str)
    } else {
        format!("{}\n{}\n", current.trim(), lib_str)
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

/// Generates a rootless `LD_PRELOAD` Steam launcher script supporting `--gdb` and `--lldb`
/// interactive debugging flags without introducing trailing colons in `LD_PRELOAD`/`LD_LIBRARY_PATH`.
pub fn generate_ld_preload_launcher(
    steam_exe: &Path,
    lib_path: &Path,
    output_script: &Path,
) -> Result<(), PreloadError> {
    let steam_str = steam_exe.to_string_lossy().to_string();
    let lib_str = lib_path.to_string_lossy().to_string();
    let lib_dir = lib_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_string_lossy()
        .to_string();

    let script = format!(
        "#!/bin/sh\n\
         # Steam integration launcher (generated by LuaTools Linux)\n\
         # Preloads libsteam_integration.so into the Steam client process.\n\
         # Interactive debugging: pass --gdb or --lldb as the first argument.\n\
         DEBUGGER=\"\"\n\
         case \"${{1:-}}\" in\n\
           --gdb)\n\
             DEBUGGER=\"gdb\"\n\
             shift\n\
             ;;\n\
           --lldb)\n\
             DEBUGGER=\"lldb\"\n\
             shift\n\
             ;;\n\
         esac\n\
         NEW_LD_PRELOAD=\"{lib}${{LD_PRELOAD:+:$LD_PRELOAD}}\"\n\
         NEW_LD_LIBRARY_PATH=\"{lib_dir}${{LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}}\"\n\
         if [ \"$DEBUGGER\" = \"gdb\" ]; then\n\
           exec gdb \\\n\
             -ex \"set environment LD_PRELOAD=$NEW_LD_PRELOAD\" \\\n\
             -ex \"set environment LD_LIBRARY_PATH=$NEW_LD_LIBRARY_PATH\" \\\n\
             --args \"{steam}\" \"$@\"\n\
         elif [ \"$DEBUGGER\" = \"lldb\" ]; then\n\
           exec lldb \\\n\
             -o \"settings set target.env-vars LD_PRELOAD=$NEW_LD_PRELOAD\" \\\n\
             -o \"settings set target.env-vars LD_LIBRARY_PATH=$NEW_LD_LIBRARY_PATH\" \\\n\
             -- \"{steam}\" \"$@\"\n\
         fi\n\
         export LD_PRELOAD=\"$NEW_LD_PRELOAD\"\n\
         export LD_LIBRARY_PATH=\"$NEW_LD_LIBRARY_PATH\"\n\
         exec \"{steam}\" \"$@\"\n",
        lib = lib_str,
        lib_dir = lib_dir,
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

    #[test]
    fn identifies_only_main_steam_client_process() {
        assert!(is_steam_client_process(
            "steam",
            Some(Path::new("/home/u/.local/share/Steam/steamrt64/steam"))
        ));
        assert!(is_steam_client_process(
            "steam",
            Some(Path::new("/home/u/.local/share/Steam/ubuntu12_32/steam"))
        ));
        assert!(!is_steam_client_process(
            "steamwebhelper",
            Some(Path::new(
                "/home/u/.local/share/Steam/ubuntu12_64/steamwebhelper"
            ))
        ));
        assert!(!is_steam_client_process(
            "luatools-steam-",
            Some(Path::new("/home/u/.local/bin/luatools-steam-launcher"))
        ));
        assert!(!is_steam_client_process(
            "sh",
            Some(Path::new("/usr/bin/bash"))
        ));
        assert!(!is_steam_client_process(
            "steam.sh",
            Some(Path::new("/home/u/.local/share/Steam/steam.sh"))
        ));
        assert!(!is_steam_client_process(
            "steam-sls",
            Some(Path::new("/home/u/.local/bin/steam-sls"))
        ));
    }

    #[test]
    fn launcher_script_avoids_trailing_colons_and_supports_gdb_lldb() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("luatools-steam-launcher");
        generate_ld_preload_launcher(
            Path::new("/home/u/.local/share/Steam/steam.sh"),
            Path::new("/home/u/.local/lib/luatools/libsteam_integration.so"),
            &out,
        )
        .unwrap();
        let txt = fs::read_to_string(&out).unwrap();
        assert!(!txt.contains(":${LD_PRELOAD:-}"));
        assert!(!txt.contains(":${LD_LIBRARY_PATH:-}"));
        assert!(txt.contains("${LD_PRELOAD:+:$LD_PRELOAD}"));
        assert!(txt.contains("--gdb)"));
        assert!(txt.contains("--lldb)"));
    }
}
