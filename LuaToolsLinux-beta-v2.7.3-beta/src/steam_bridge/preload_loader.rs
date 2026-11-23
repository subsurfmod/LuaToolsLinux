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

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

/// Returns `true` when `/proc/<pid>` belongs to any Steam client, webhelper, overlay,
/// runtime wrapper, or Steam launcher script process, while strictly excluding LuaTools
/// itself, unrelated browsers, IDEs, shells, and non-Steam sandboxes.
pub fn is_any_steam_process(comm: &str, exe_path: Option<&Path>, cmdline: &str) -> bool {
    let comm_lower = comm.trim().to_ascii_lowercase();
    let exe_name_lower = exe_path
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let exe_full_lower = exe_path
        .map(|p| p.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let cmd_lower = cmdline.to_ascii_lowercase();

    // Never terminate LuaTools itself, compilers, or interactive debuggers
    if comm_lower.contains("luatools")
        || exe_name_lower.contains("luatools")
        || matches!(
            exe_name_lower.as_str(),
            "cargo" | "rustc" | "gdb" | "lldb" | "firefox" | "chrome" | "chromium" | "brave"
        )
    {
        return false;
    }

    // 1. Direct Steam client / helper binaries
    let is_direct_name = |name: &str| {
        matches!(
            name,
            "steam"
                | "steamwebhelper"
                | "steam.sh"
                | "steamservice"
                | "steam-runtime"
                | "steam-native"
                | "steam-sls"
                | "gameoverlayui"
                | "gameoverlayui64"
                | "fossilize_replay"
                | "steamerrorreporter"
                | "steam-runtime-l"
                | "steam-runtime-launcher-service"
                | "steam-runtime-supervisor"
                | "steam-runtime-launcher-interface-0"
                | "steam-runtime-l-0"
        ) || name.starts_with("steamwebhelper")
            || name.starts_with("steam-runtime-")
    };

    if is_direct_name(&exe_name_lower) || is_direct_name(&comm_lower) {
        return true;
    }

    // 2. Executable resides directly inside Steam's own runtime/client folders
    let exe_in_steam_runtime = exe_full_lower.contains("/.local/share/steam/")
        || exe_full_lower.contains("/.steam/")
        || exe_full_lower.contains("/steamrt64/")
        || exe_full_lower.contains("/steamrt32/")
        || exe_full_lower.contains("/ubuntu12_32/")
        || exe_full_lower.contains("/ubuntu12_64/")
        || exe_full_lower.contains("/steamlinuxruntime");
    if exe_in_steam_runtime {
        return true;
    }

    // 3. Shell wrappers executing steam.sh / /usr/bin/steam / steam-sls
    if matches!(
        exe_name_lower.as_str(),
        "sh" | "bash" | "dash" | "zsh" | "fish"
    ) {
        let runs_steam_script = cmd_lower.contains("/steam.sh")
            || cmd_lower.contains(" steam.sh")
            || cmd_lower.contains("/usr/bin/steam")
            || cmd_lower.contains("/usr/games/steam")
            || cmd_lower.contains("/.local/share/steam/")
            || cmd_lower.contains("/.steam/debian-installation/")
            || cmd_lower.contains("steam-sls")
            || cmd_lower.contains("luatools-steam-launcher");
        if runs_steam_script {
            return true;
        }
    }

    // 4. Steam Runtime container / reaper processes scoped specifically to Steam
    if matches!(
        exe_name_lower.as_str(),
        "reaper" | "srt-bwrap" | "pressure-vessel-wrap" | "pv-adverb" | "bwrap"
    ) || matches!(comm_lower.as_str(), "reaper" | "srt-bwrap" | "pv-adverb")
    {
        let steam_scoped = cmd_lower.contains("steamlaunch")
            || cmd_lower.contains("steamlinuxruntime")
            || cmd_lower.contains("/.local/share/steam/")
            || cmd_lower.contains("/.steam/")
            || cmd_lower.contains("steamapps");
        if steam_scoped {
            return true;
        }
    }

    false
}

/// Finds all running Steam client, webhelper, overlay, and Steam Runtime PIDs.
pub fn find_all_steam_pids() -> Vec<u32> {
    let my_pid = std::process::id();
    let my_ppid = unsafe { libc::getppid() }.max(0) as u32;
    let mut pids = Vec::new();

    if let Ok(entries) = fs::read_dir("/proc") {
        for e in entries.flatten() {
            let pid_str = e.file_name().to_string_lossy().to_string();
            let Ok(pid): Result<u32, _> = pid_str.parse() else {
                continue;
            };
            if pid == my_pid || pid == my_ppid || pid <= 1 {
                continue;
            }
            let comm = fs::read_to_string(format!("/proc/{}/comm", pid)).unwrap_or_default();
            let exe = fs::read_link(format!("/proc/{}/exe", pid)).ok();
            let cmdline = fs::read(format!("/proc/{}/cmdline", pid))
                .map(|b| {
                    b.split(|&c| c == 0)
                        .filter(|s| !s.is_empty())
                        .map(|s| String::from_utf8_lossy(s).into_owned())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();

            if is_any_steam_process(&comm, exe.as_deref(), &cmdline) {
                pids.push(pid);
            }
        }
    }
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// Terminates all running instances of Steam (client, webhelpers, overlays, runtime wrappers).
/// Sends `SIGTERM` first, waits up to 2 seconds for clean exit, then sends `SIGKILL` to any
/// remaining Steam processes. Returns the total count of unique Steam processes terminated.
pub fn kill_all_steam_instances() -> Result<usize, String> {
    let initial = find_all_steam_pids();
    if initial.is_empty() {
        return Ok(0);
    }

    let mut terminated: HashSet<u32> = HashSet::new();
    let mut perm_denied = false;

    let send_sig =
        |pids: &[u32], sig: libc::c_int, terminated: &mut HashSet<u32>, perm_denied: &mut bool| {
            for &pid in pids {
                let rc = unsafe { libc::kill(pid as libc::pid_t, sig) };
                if rc == 0 {
                    terminated.insert(pid);
                } else {
                    let err = std::io::Error::last_os_error();
                    if err.raw_os_error() == Some(libc::EPERM) {
                        *perm_denied = true;
                    }
                }
            }
        };

    // Phase 1: Graceful SIGTERM to all detected Steam instances
    send_sig(&initial, libc::SIGTERM, &mut terminated, &mut perm_denied);

    // Wait up to 2.0 seconds, sending SIGTERM to any newly respawned helper along the way
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(100));
        let alive = find_all_steam_pids();
        if alive.is_empty() {
            return Ok(terminated.len());
        }
        let newcomers: Vec<u32> = alive
            .into_iter()
            .filter(|p| !terminated.contains(p))
            .collect();
        if !newcomers.is_empty() {
            send_sig(&newcomers, libc::SIGTERM, &mut terminated, &mut perm_denied);
        }
    }

    // Phase 2: Force SIGKILL any remaining stubborn Steam processes
    let remaining = find_all_steam_pids();
    if !remaining.is_empty() {
        send_sig(&remaining, libc::SIGKILL, &mut terminated, &mut perm_denied);
        std::thread::sleep(Duration::from_millis(250));
    }

    let still_alive = find_all_steam_pids();
    if !still_alive.is_empty() && perm_denied {
        return Err(format!(
            "Permission denied stopping {} Steam process(es) owned by another user.",
            still_alive.len()
        ));
    }

    Ok(terminated.len())
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
    fn identifies_all_steam_instances_for_kill_without_false_positives() {
        assert!(is_any_steam_process(
            "steam",
            Some(Path::new("/home/u/.local/share/Steam/steamrt64/steam")),
            "/home/u/.local/share/Steam/steamrt64/steam"
        ));
        assert!(is_any_steam_process(
            "steamwebhelper",
            Some(Path::new(
                "/home/u/.local/share/Steam/ubuntu12_64/steamwebhelper"
            )),
            "/home/u/.local/share/Steam/ubuntu12_64/steamwebhelper --type=renderer"
        ));
        assert!(is_any_steam_process(
            "bash",
            Some(Path::new("/usr/bin/bash")),
            "/bin/bash /home/u/.local/share/Steam/steam.sh"
        ));
        assert!(is_any_steam_process(
            "gameoverlayui",
            Some(Path::new(
                "/home/u/.local/share/Steam/ubuntu12_32/gameoverlayui"
            )),
            "gameoverlayui -pid 1234"
        ));
        assert!(is_any_steam_process(
            "reaper",
            Some(Path::new("/home/u/.local/share/Steam/ubuntu12_32/reaper")),
            "reaper SteamLaunch AppId=1245620"
        ));
        assert!(is_any_steam_process(
            "srt-bwrap",
            Some(Path::new("/usr/bin/srt-bwrap")),
            "srt-bwrap /home/u/.local/share/Steam/steamapps/common/SteamLinuxRuntime_sniper"
        ));

        // Strict negative safety checks
        assert!(!is_any_steam_process(
            "luatools",
            Some(Path::new("/home/u/.local/bin/luatools")),
            "/home/u/.local/bin/luatools"
        ));
        assert!(!is_any_steam_process(
            "bash",
            Some(Path::new("/usr/bin/bash")),
            "/usr/bin/bash"
        ));
        assert!(!is_any_steam_process(
            "firefox",
            Some(Path::new("/usr/lib64/firefox/firefox")),
            "/usr/lib64/firefox/firefox https://store.steampowered.com/app/1245620"
        ));
        assert!(!is_any_steam_process(
            "bwrap",
            Some(Path::new("/usr/bin/bwrap")),
            "/usr/bin/bwrap --ro-bind / / /usr/bin/evince"
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
