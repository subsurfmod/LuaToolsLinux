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
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::steam_bridge::ost_platform::process::{
    get_environment_variable_value, get_image_path, get_process_command_line,
};

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
    #[error("Unsafe single-architecture preload library for /etc/ld.so.preload (use $LIB/${{LIB}} multilib expansion): {0}")]
    UnsafeMultilibPreload(String),
}

pub const LD_SO_PRELOAD_PATH: &str = "/etc/ld.so.preload";

/// Strip Linux `/proc/<pid>/exe` trailing `" (deleted)"` marker after live binary updates.
fn clean_exe_name(exe_path: Option<&Path>) -> &str {
    let raw = exe_path
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("");
    raw.strip_suffix(" (deleted)").unwrap_or(raw)
}

/// Returns `true` only when `/proc/<pid>/comm` and `/proc/<pid>/exe` identify the actual
/// Steam client binary (`steam`), excluding `steamwebhelper`, launcher wrappers, `steam.sh`,
/// `steam-sls`, `luatools`, shells, and Steam Runtime helper processes.
pub fn is_steam_client_process(comm: &str, exe_path: Option<&Path>) -> bool {
    is_steam_client_process_with_cmdline(comm, exe_path, "")
}

/// Extended Steam client detector that also inspects `/proc/<pid>/cmdline`, which is required
/// under `LD_AUDIT` / `steam-sls`, Steam Runtime, and Flatpak `bwrap` where `/proc/<pid>/exe`
/// can resolve to `ld-linux.so.2`, `"steam (deleted)"`, or be unreadable (`None`).
pub fn is_steam_client_process_with_cmdline(
    comm: &str,
    exe_path: Option<&Path>,
    cmdline: &str,
) -> bool {
    let comm_trim = comm.trim();
    let exe_name = clean_exe_name(exe_path);

    // Explicitly exclude helper / browser / wrapper processes even if they contain "steam"
    let lower_comm = comm_trim.to_ascii_lowercase();
    let lower_exe = exe_name.to_ascii_lowercase();
    let cmd_lower = cmdline.to_ascii_lowercase();

    if lower_comm.contains("webhelper")
        || lower_exe.contains("webhelper")
        || cmd_lower.contains("steamwebhelper")
        || lower_comm.contains("launcher")
        || lower_exe.contains("launcher")
        || cmd_lower.contains("luatools-steam-launcher")
        || lower_comm.contains("luatools")
        || lower_exe.contains("luatools")
        || lower_comm.contains("gameoverlayui")
        || lower_exe.contains("gameoverlayui")
        || cmd_lower.contains("gameoverlayui")
        || lower_comm.contains("fossilize")
        || lower_exe.contains("fossilize")
        || cmd_lower.contains("fossilize")
        || lower_comm == "steam-sls"
        || lower_exe == "steam-sls"
        || lower_exe.ends_with(".sh")
        || lower_comm.ends_with(".sh")
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
                | "pv-adverb"
                | "pressure-vessel-wrap"
        )
        || matches!(
            lower_comm.as_str(),
            "sh" | "bash" | "dash" | "zsh" | "fish" | "reaper" | "srt-bwrap" | "bwrap"
        )
    {
        return false;
    }

    if cmd_lower.contains(" -shutdown") || cmd_lower.ends_with("-shutdown") {
        return false;
    }

    if exe_name == "steam" {
        return true;
    }

    // Under LD_AUDIT / steam-sls / Steam Runtime / Flatpak bwrap, /proc/<pid>/exe can be
    // ld-linux.so.2 or unreadable (None) while cmdline clearly points to the Steam binary.
    let exe_is_linker_or_missing =
        exe_path.is_none() || lower_exe.starts_with("ld-linux") || lower_exe == "ld.so";
    if exe_is_linker_or_missing && !cmd_lower.is_empty() {
        let mut tokens = cmd_lower.split_whitespace();
        let first = tokens.next().unwrap_or("").trim_matches(|c| c == '"' || c == '\'');
        if first.ends_with("steam.sh") || first.ends_with("steam-sls") {
            return false;
        }
        for tok in std::iter::once(first).chain(tokens.take(5)) {
            let t = tok.trim_matches(|c| c == '"' || c == '\'');
            if t.ends_with("/ubuntu12_32/steam")
                || t.ends_with("/ubuntu12_64/steam")
                || t.ends_with("/steamrt64/steam")
                || t.ends_with("/steamrt32/steam")
                || t.ends_with("/steam/steam")
            {
                return true;
            }
        }
        if first == "steam" || first.ends_with("/steam") {
            return true;
        }
    }

    // Fallback if /proc/<pid>/exe cannot be read (e.g. different permissions)
    exe_path.is_none() && comm_trim == "steam"
}

fn parse_proc_stat_state(stat_text: &str) -> Option<char> {
    let rparen = stat_text.rfind(')')?;
    stat_text[rparen + 1..].trim_start().chars().next()
}

fn is_process_zombie(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{}/stat", pid))
        .ok()
        .and_then(|s| parse_proc_stat_state(&s))
        .map(|st| st == 'Z' || st == 'X')
        .unwrap_or(false)
}

/// Parses a Steam `steam.pid` lockfile value into a valid PID (`> 1`).
pub fn parse_steam_pidfile_text(text: &str) -> Option<u32> {
    text.trim().parse::<u32>().ok().filter(|&p| p > 1)
}

fn steam_pidfile_candidates() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    vec![
        home.join(".steam/steam.pid"),
        home.join(".local/share/Steam/steam.pid"),
        home.join(".var/app/com.valvesoftware.Steam/.steam/steam.pid"),
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/steam.pid"),
    ]
}

fn verify_live_steam_pid(pid: u32, my_pid: u32, my_uid: u32) -> bool {
    if pid <= 1 || pid == my_pid {
        return false;
    }
    let Ok(pid_t) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if unsafe { libc::kill(pid_t, 0) } != 0 {
        return false;
    }
    if my_uid != 0 {
        let Ok(meta) = fs::metadata(format!("/proc/{}", pid)) else {
            return false;
        };
        if meta.uid() != my_uid {
            return false;
        }
    }
    if is_process_zombie(pid) {
        return false;
    }
    let comm = fs::read_to_string(format!("/proc/{}/comm", pid)).unwrap_or_default();
    let exe = get_image_path(pid).map(PathBuf::from);
    let cmdline = get_process_command_line(pid).unwrap_or_default();
    is_steam_client_process_with_cmdline(&comm, exe.as_deref(), &cmdline)
}

pub fn find_steam_pid() -> Option<u32> {
    let my_pid = std::process::id();
    let my_uid = unsafe { libc::geteuid() };

    // 1. Check authoritative Steam PID lockfiles first (fast & reliable under LD_AUDIT/Flatpak)
    for pidfile in steam_pidfile_candidates() {
        if let Ok(raw) = fs::read_to_string(&pidfile) {
            if let Some(pid) = parse_steam_pidfile_text(&raw) {
                if verify_live_steam_pid(pid, my_pid, my_uid) {
                    return Some(pid);
                }
            }
        }
    }

    // 2. Scan /proc checking comm, exe, AND cmdline (skipping zombies and other UIDs)
    let mut preloaded_fallback_pid = None;
    let mut fallback_pid = None;

    if let Ok(entries) = fs::read_dir("/proc") {
        for e in entries.flatten() {
            let pid_str = e.file_name().to_string_lossy().to_string();
            let Ok(pid): Result<u32, _> = pid_str.parse() else {
                continue;
            };
            if pid <= 1 || pid == my_pid {
                continue;
            }
            if my_uid != 0 {
                if let Ok(meta) = e.metadata() {
                    if meta.uid() != my_uid {
                        continue;
                    }
                }
            }
            if is_process_zombie(pid) {
                continue;
            }
            let comm = fs::read_to_string(format!("/proc/{}/comm", pid)).unwrap_or_default();
            let exe = get_image_path(pid).map(PathBuf::from);
            let cmdline = get_process_command_line(pid).unwrap_or_default();

            if is_steam_client_process_with_cmdline(&comm, exe.as_deref(), &cmdline) {
                // Prefer the 64-bit steamrt64/steam main process when both 32-bit and 64-bit exist
                let is_rt64 = exe
                    .as_ref()
                    .map(|p| p.to_string_lossy().contains("steamrt64"))
                    .unwrap_or(false)
                    || cmdline.contains("steamrt64");
                if is_rt64 {
                    return Some(pid);
                }
                if preloaded_fallback_pid.is_none() && has_preload_env(pid, integration_lib_name())
                {
                    preloaded_fallback_pid = Some(pid);
                }
                if fallback_pid.is_none() {
                    fallback_pid = Some(pid);
                }
            }
        }
    }
    preloaded_fallback_pid.or(fallback_pid)
}

/// Returns `true` when `/proc/<pid>` belongs to any Steam client, webhelper, overlay,
/// runtime wrapper, or Steam launcher script process, while strictly excluding LuaTools
/// itself, unrelated browsers, IDEs, shells, and non-Steam sandboxes.
pub fn is_any_steam_process(comm: &str, exe_path: Option<&Path>, cmdline: &str) -> bool {
    let comm_lower = comm.trim().to_ascii_lowercase();
    let exe_name_lower = clean_exe_name(exe_path).to_ascii_lowercase();
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
        || exe_full_lower.contains("/.var/app/com.valvesoftware.steam/")
        || exe_full_lower.contains("/steamrt64/")
        || exe_full_lower.contains("/steamrt32/")
        || exe_full_lower.contains("/ubuntu12_32/")
        || exe_full_lower.contains("/ubuntu12_64/")
        || exe_full_lower.contains("/steamlinuxruntime");
    if exe_in_steam_runtime {
        return true;
    }

    // 2b. Dynamic linker invocation under LD_AUDIT running Steam binaries
    if (exe_name_lower.starts_with("ld-linux") || exe_name_lower == "ld.so")
        && (cmd_lower.contains("/ubuntu12_32/steam")
            || cmd_lower.contains("/ubuntu12_64/steam")
            || cmd_lower.contains("/steamrt64/steam")
            || cmd_lower.contains("/steamrt32/steam")
            || cmd_lower.contains("steamwebhelper"))
    {
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

fn collect_ancestor_pids(start_pid: u32) -> HashSet<u32> {
    let mut set = HashSet::new();
    let mut cur = start_pid;
    for _ in 0..64 {
        if cur <= 1 || !set.insert(cur) {
            break;
        }
        let next_ppid = fs::read_to_string(format!("/proc/{}/stat", cur))
            .ok()
            .and_then(|s| {
                let rparen = s.rfind(')')?;
                let mut parts = s[rparen + 1..].split_whitespace();
                let _state = parts.next()?;
                parts.next()?.parse::<u32>().ok()
            })
            .unwrap_or(0);
        cur = next_ppid;
    }
    set
}

/// Finds all running Steam client, webhelper, overlay, and Steam Runtime PIDs.
pub fn find_all_steam_pids() -> Vec<u32> {
    let my_pid = std::process::id();
    let ancestors = collect_ancestor_pids(my_pid);
    let my_uid = unsafe { libc::geteuid() };
    let mut pids = Vec::new();

    if let Ok(entries) = fs::read_dir("/proc") {
        for e in entries.flatten() {
            let pid_str = e.file_name().to_string_lossy().to_string();
            let Ok(pid): Result<u32, _> = pid_str.parse() else {
                continue;
            };
            if pid <= 1 || ancestors.contains(&pid) {
                continue;
            }
            if my_uid != 0 {
                if let Ok(meta) = e.metadata() {
                    if meta.uid() != my_uid {
                        continue;
                    }
                }
            }
            let comm = fs::read_to_string(format!("/proc/{}/comm", pid)).unwrap_or_default();
            let exe = get_image_path(pid).map(PathBuf::from);
            let cmdline = get_process_command_line(pid).unwrap_or_default();

            if is_any_steam_process(&comm, exe.as_deref(), &cmdline) {
                pids.push(pid);
            }
        }
    }
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// Terminates all running instances of Steam (client, webhelpers, overlays, runtime wrappers,
/// and Flatpak Steam sandboxes). Sends `SIGTERM` first, waits up to 3 seconds for clean exit,
/// then sends `SIGKILL` to any remaining Steam processes. Returns the total count of unique
/// Steam processes terminated.
pub fn kill_all_steam_instances() -> Result<usize, String> {
    let mut initial = find_all_steam_pids();
    let mut terminated: HashSet<u32> = HashSet::new();

    // If Flatpak Steam is running, ask Flatpak to terminate the sandbox cleanly as well
    let flatpak_running = dirs::home_dir()
        .map(|h| h.join(".var/app/com.valvesoftware.Steam/.steam/steam.pid"))
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|t| parse_steam_pidfile_text(&t))
        .is_some_and(|pid| {
            libc::pid_t::try_from(pid)
                .ok()
                .is_some_and(|pt| unsafe { libc::kill(pt, 0) } == 0)
        })
        || initial.iter().any(|&pid| {
            let cmd = get_process_command_line(pid)
                .unwrap_or_default()
                .to_ascii_lowercase();
            cmd.contains("com.valvesoftware.steam")
        });

    let mut flatpak_killed = false;
    if flatpak_running {
        if let Ok(st) = std::process::Command::new("flatpak")
            .args(["kill", "com.valvesoftware.Steam"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            if st.success() {
                flatpak_killed = true;
                std::thread::sleep(Duration::from_millis(250));
                initial = find_all_steam_pids();
            }
        }
    }

    if initial.is_empty() {
        return Ok(usize::from(flatpak_killed));
    }

    let mut perm_denied = false;

    let send_sig =
        |pids: &[u32], sig: libc::c_int, terminated: &mut HashSet<u32>, perm_denied: &mut bool| {
            for &pid in pids {
                // Guard against PID <= 1 or overflow into negative pid_t (which signals process groups / all processes).
                let Ok(pid_t) = libc::pid_t::try_from(pid) else {
                    continue;
                };
                if pid_t <= 1 {
                    continue;
                }
                let rc = unsafe { libc::kill(pid_t, sig) };
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

    // Wait up to 3.0 seconds, sending SIGTERM to any newly respawned helper along the way
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(100));
        let alive = find_all_steam_pids();
        if alive.is_empty() {
            return Ok(terminated.len().max(usize::from(flatpak_killed)));
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
        std::thread::sleep(Duration::from_millis(300));
    }

    let still_alive = find_all_steam_pids();
    if !still_alive.is_empty() && perm_denied {
        return Err(format!(
            "Permission denied stopping {} Steam process(es) owned by another user.",
            still_alive.len()
        ));
    }

    Ok(terminated.len().max(usize::from(flatpak_killed)))
}

pub fn is_loaded(pid: u32, lib_name: &str) -> bool {
    if pid == 0 || lib_name.is_empty() {
        return false;
    }
    let maps_path = format!("/proc/{}/maps", pid);
    if let Ok(maps) = fs::read_to_string(&maps_path) {
        maps.lines().any(|l| l.contains(lib_name))
    } else {
        false
    }
}

/// Checks whether a running process has `lib_name` configured in `LD_PRELOAD` or `LD_AUDIT`
/// via `/proc/<pid>/environ`.
pub fn has_preload_env(pid: u32, lib_name: &str) -> bool {
    if pid == 0 || lib_name.is_empty() {
        return false;
    }
    get_environment_variable_value(pid, "LD_PRELOAD")
        .is_some_and(|v| v.contains(lib_name))
        || get_environment_variable_value(pid, "LD_AUDIT")
            .is_some_and(|v| v.contains(lib_name))
}

fn map_preload_io_err(err: std::io::Error) -> PreloadError {
    if err.kind() == std::io::ErrorKind::PermissionDenied {
        PreloadError::PermissionDenied
    } else {
        PreloadError::Io(err)
    }
}

/// Validates that `lib_path` is safe to write into `/etc/ld.so.preload` on a multilib Linux
/// system. Rejects literal single-architecture paths (including 64-bit `ELFCLASS64` `.so` files)
/// unless the path uses dynamic linker `$LIB` / `${LIB}` / `$PLATFORM` / `${PLATFORM}` DST
/// expansion or is a bare multilib SONAME without `/`.
fn validate_system_preload_lib_path(lib_path: &Path) -> Result<String, PreloadError> {
    let lib_str = lib_path.to_string_lossy().trim().to_string();
    if lib_str.is_empty() {
        return Err(PreloadError::IntegrationLibNotFound(
            "empty library path".into(),
        ));
    }
    if lib_str.chars().any(char::is_whitespace) {
        return Err(PreloadError::UnsafeMultilibPreload(format!(
            "preload path contains whitespace delimiter: {}",
            lib_str
        )));
    }

    let uses_multilib_dst = lib_str.contains("$LIB")
        || lib_str.contains("${LIB}")
        || lib_str.contains("$PLATFORM")
        || lib_str.contains("${PLATFORM}");
    let is_bare_soname = !lib_str.contains('/') && lib_str.ends_with(".so");

    if uses_multilib_dst || is_bare_soname {
        return Ok(lib_str);
    }

    // Check if the file on disk is a single-architecture ELF (e.g. ELFCLASS64 = 2)
    if let Ok(mut f) = fs::File::open(lib_path) {
        use std::io::Read;
        let mut hdr = [0u8; 5];
        if f.read_exact(&mut hdr).is_ok() && &hdr[0..4] == b"\x7fELF" && hdr[4] == 2 {
            return Err(PreloadError::UnsafeMultilibPreload(format!(
                "64-bit ELFCLASS64 library '{}' would break 32-bit processes; use $LIB/${{LIB}}",
                lib_str
            )));
        }
    }

    Err(PreloadError::UnsafeMultilibPreload(format!(
        "literal single-arch path '{}' lacks $LIB/${{LIB}} multilib expansion",
        lib_str
    )))
}

fn enable_system_preload_at(preload_file: &Path, lib_path: &Path) -> Result<(), PreloadError> {
    let lib_str = validate_system_preload_lib_path(lib_path)?;
    let current = match fs::read_to_string(preload_file) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(map_preload_io_err(e)),
    };
    if current.lines().any(|l| l.trim() == lib_str) {
        return Ok(());
    }
    let new_content = if current.trim().is_empty() {
        format!("{}\n", lib_str)
    } else {
        format!("{}\n{}\n", current.trim_end(), lib_str)
    };
    fs::write(preload_file, new_content).map_err(map_preload_io_err)?;
    Ok(())
}

fn disable_system_preload_at(preload_file: &Path, lib_path: &Path) -> Result<(), PreloadError> {
    let lib_str = lib_path.to_string_lossy().trim().to_string();
    let current = match fs::read_to_string(preload_file) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(map_preload_io_err(e)),
    };
    let filtered: String = current
        .lines()
        .filter(|l| l.trim() != lib_str && !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        preload_file,
        if filtered.is_empty() {
            String::new()
        } else {
            filtered + "\n"
        },
    )
    .map_err(map_preload_io_err)?;
    Ok(())
}

pub fn enable_system_preload(lib_path: &Path) -> Result<(), PreloadError> {
    enable_system_preload_at(Path::new(LD_SO_PRELOAD_PATH), lib_path)
}

pub fn disable_system_preload(lib_path: &Path) -> Result<(), PreloadError> {
    disable_system_preload_at(Path::new(LD_SO_PRELOAD_PATH), lib_path)
}

/// Escape characters that are special inside POSIX shell double-quoted strings.
fn escape_sh_double_quoted(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '\\' | '"' | '$' | '`' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Generates a rootless `LD_PRELOAD` Steam launcher script supporting `--gdb` and `--lldb`
/// interactive debugging flags without introducing trailing colons in `LD_PRELOAD`/`LD_LIBRARY_PATH`.
pub fn generate_ld_preload_launcher(
    steam_exe: &Path,
    lib_path: &Path,
    output_script: &Path,
) -> Result<(), PreloadError> {
    let steam_str = escape_sh_double_quoted(&steam_exe.to_string_lossy());
    let lib_str = escape_sh_double_quoted(&lib_path.to_string_lossy());
    let lib_dir = escape_sh_double_quoted(
        &lib_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_string_lossy(),
    );

    if let Some(parent) = output_script.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }

    let script = format!(
        "#!/bin/sh\n\
         # Steam integration launcher (generated by LuaTools Linux)\n\
         # Integration library reference: {lib}\n\
         # Fallback Steam executable: {steam}\n\
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
         # LT-18 / BST-65: Strip 64-bit libsteam_integration.so from LD_PRELOAD before launching 32-bit Steam\n\
         if [ -n \"${{LD_PRELOAD:-}}\" ]; then\n\
           CLEAN_LD_PRELOAD=\"\"\n\
           OLD_IFS=\"$IFS\"\n\
           IFS=\":\"\n\
           for entry in $LD_PRELOAD; do\n\
             case \"$entry\" in\n\
               *libsteam_integration.so*|\"{lib}\") ;;\n\
               \"\") ;;\n\
               *) CLEAN_LD_PRELOAD=\"${{CLEAN_LD_PRELOAD:+$CLEAN_LD_PRELOAD:}}$entry\" ;;\n\
             esac\n\
           done\n\
           IFS=\"$OLD_IFS\"\n\
           export LD_PRELOAD=\"$CLEAN_LD_PRELOAD\"\n\
         else\n\
           unset LD_PRELOAD\n\
         fi\n\
         # Prefer multilib $LIB expansion if both lib/ and lib64/ are present\n\
         IS_ELF32=0\n\
         ACTIVE_LIB=\"\"\n\
         if [ -f \"{lib_dir}/lib/libsteam_integration.so\" ] && [ -f \"{lib_dir}/lib64/libsteam_integration.so\" ]; then\n\
           ACTIVE_LIB=\"{lib_dir}/\\$LIB/libsteam_integration.so\"\n\
           IS_ELF32=1\n\
         fi\n\
         if [ -z \"$ACTIVE_LIB\" ]; then\n\
           for cand in \"{lib}\" \"{lib_dir}/lib/libsteam_integration.so\" \"{lib_dir}/32/libsteam_integration.so\" \"{lib_dir}/libsteam_integration32.so\"; do\n\
             if [ -f \"$cand\" ] && command -v file >/dev/null 2>&1; then\n\
               if file -L \"$cand\" | grep -q \"ELF 32-bit\"; then\n\
                 ACTIVE_LIB=\"$cand\"\n\
                 IS_ELF32=1\n\
                 break\n\
               fi\n\
             fi\n\
           done\n\
         fi\n\
         if [ \"$IS_ELF32\" -eq 1 ] && [ -n \"$ACTIVE_LIB\" ]; then\n\
           export LD_PRELOAD=\"$ACTIVE_LIB${{LD_PRELOAD:+:$LD_PRELOAD}}\"\n\
         fi\n\
         # LT-20 / BST-65: Auto-launch LuaTools minimized in background unless user opted out\n\
         # Opt out via LUATOOLS_NO_AUTOLAUNCH=1 or creating ~/.config/luatools/no-autolaunch\n\
         if [ \"${{LUATOOLS_NO_AUTOLAUNCH:-0}}\" != \"1\" ] && [ ! -f \"${{XDG_CONFIG_HOME:-$HOME/.config}}/luatools/no-autolaunch\" ] && [ ! -f \"$HOME/.config/luatools/no-autolaunch\" ]; then\n\
           if command -v luatools >/dev/null 2>&1; then\n\
             if ! pgrep -x luatools >/dev/null 2>&1; then\n\
               luatools --minimized --tray-locked >/dev/null 2>&1 &\n\
             fi\n\
           elif [ -x \"$HOME/.local/bin/luatools\" ]; then\n\
             if ! pgrep -x luatools >/dev/null 2>&1; then\n\
               \"$HOME/.local/bin/luatools\" --minimized --tray-locked >/dev/null 2>&1 &\n\
             fi\n\
           fi\n\
         fi\n\
         # When 32-bit libsteam_integration.so is active via LD_PRELOAD, launch Steam directly without SLSsteam.so (LD_AUDIT)\n\
         STEAM_TARGET=\"{steam}\"\n\
         if [ \"$IS_ELF32\" -ne 1 ]; then\n\
           if [ -x \"$HOME/.local/bin/steam-sls\" ]; then\n\
             STEAM_TARGET=\"$HOME/.local/bin/steam-sls\"\n\
           else\n\
             SLS_DIR=\"$HOME/.local/share/SLSsteam\"\n\
             AUDIT_LIBS=\"\"\n\
             if [ -f \"$SLS_DIR/library-inject.so\" ]; then\n\
               AUDIT_LIBS=\"$SLS_DIR/library-inject.so\"\n\
             fi\n\
             if [ -f \"$SLS_DIR/SLSsteam.so\" ]; then\n\
               AUDIT_LIBS=\"${{AUDIT_LIBS:+$AUDIT_LIBS:}}$SLS_DIR/SLSsteam.so\"\n\
             fi\n\
             if [ -n \"$AUDIT_LIBS\" ]; then\n\
               export LD_AUDIT=\"$AUDIT_LIBS\"\n\
             fi\n\
           fi\n\
         fi\n\
         export LD_LIBRARY_PATH=\"$NEW_LD_LIBRARY_PATH\"\n\
         if [ \"$DEBUGGER\" = \"gdb\" ]; then\n\
           exec gdb \\\n\
             -ex \"set environment LD_PRELOAD=${{LD_PRELOAD:-}}\" \\\n\
             -ex \"set environment LD_LIBRARY_PATH=$NEW_LD_LIBRARY_PATH\" \\\n\
             --args \"$STEAM_TARGET\" \"$@\"\n\
         elif [ \"$DEBUGGER\" = \"lldb\" ]; then\n\
           exec lldb \\\n\
             -o \"settings set target.env-vars LD_PRELOAD=${{LD_PRELOAD:-}}\" \\\n\
             -o \"settings set target.env-vars LD_LIBRARY_PATH=$NEW_LD_LIBRARY_PATH\" \\\n\
             -- \"$STEAM_TARGET\" \"$@\"\n\
         fi\n\
         exec \"$STEAM_TARGET\" \"$@\"\n",
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
        // LD_AUDIT / steam-sls / Flatpak where exe is ld-linux.so.2 or unreadable (None)
        assert!(is_steam_client_process_with_cmdline(
            "steam",
            Some(Path::new("/usr/lib/ld-linux.so.2")),
            "/usr/lib/ld-linux.so.2 /home/u/.local/share/Steam/ubuntu12_32/steam -silent"
        ));
        assert!(is_steam_client_process_with_cmdline(
            "steam",
            None,
            "/home/u/.local/share/Steam/steamrt64/steam"
        ));
        // Helpers & shutdown command must NEVER match as running Steam client
        assert!(!is_steam_client_process_with_cmdline(
            "steam",
            None,
            "/home/u/.local/share/Steam/ubuntu12_32/steam -shutdown"
        ));
        assert!(!is_steam_client_process_with_cmdline(
            "gameoverlayui",
            None,
            "/home/u/.local/share/Steam/ubuntu12_32/gameoverlayui -pid 1234"
        ));
        assert!(!is_steam_client_process_with_cmdline(
            "fossilize_repla",
            None,
            "/home/u/.local/share/Steam/fossilize_replay --cache"
        ));
        assert_eq!(parse_steam_pidfile_text("48120\n"), Some(48120));
        assert_eq!(parse_steam_pidfile_text("0\n"), None);
        assert_eq!(parse_steam_pidfile_text("1\n"), None);
        assert_eq!(parse_steam_pidfile_text("not-a-pid"), None);
        assert_eq!(parse_proc_stat_state("1234 (steam) S 1 1234"), Some('S'));
        assert_eq!(parse_proc_stat_state("1234 (steam (old)) Z 1 1234"), Some('Z'));
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

    #[test]
    fn recognizes_deleted_steam_exe_after_live_update() {
        assert!(is_steam_client_process(
            "steam",
            Some(Path::new(
                "/home/u/.local/share/Steam/steamrt64/steam (deleted)"
            ))
        ));
        assert!(is_any_steam_process(
            "steamwebhelper",
            Some(Path::new(
                "/home/u/.local/share/Steam/ubuntu12_64/steamwebhelper (deleted)"
            )),
            ""
        ));
        assert!(!is_loaded(std::process::id(), ""));
    }

    #[test]
    fn system_preload_file_roundtrip_and_launcher_shell_escaping() {
        let tmp = tempfile::tempdir().unwrap();
        let preload = tmp.path().join("ld.so.preload");
        let lib = Path::new("/opt/luatools/$LIB/libsteam_integration.so");

        enable_system_preload_at(&preload, lib).unwrap();
        // Idempotent second call must not duplicate entry
        enable_system_preload_at(&preload, lib).unwrap();
        assert_eq!(
            fs::read_to_string(&preload).unwrap(),
            "/opt/luatools/$LIB/libsteam_integration.so\n"
        );

        disable_system_preload_at(&preload, lib).unwrap();
        assert_eq!(fs::read_to_string(&preload).unwrap(), "");

        let escaped_script = tmp.path().join("nested/launcher.sh");
        generate_ld_preload_launcher(
            Path::new("/home/u/My $Steam/steam.sh"),
            Path::new("/home/u/lib`dir\"/libsteam_integration.so"),
            &escaped_script,
        )
        .unwrap();
        let txt = fs::read_to_string(&escaped_script).unwrap();
        assert!(txt.contains("My \\$Steam/steam.sh"));
        assert!(txt.contains("lib\\`dir\\\"/libsteam_integration.so"));
        assert!(txt.contains("LUATOOLS_NO_AUTOLAUNCH"));
        assert!(txt.contains("$HOME/.config/luatools/no-autolaunch"));
    }

    #[test]
    fn rejects_64bit_literal_so_in_system_preload_and_accepts_multilib_tokens() {
        let tmp = tempfile::tempdir().unwrap();
        let preload = tmp.path().join("ld.so.preload");

        // 1. Real 64-bit ELFCLASS64 header (.so) must be rejected
        let elf64_so = tmp.path().join("libsteam_integration.so");
        let mut elf64_hdr = vec![0x7F, b'E', b'L', b'F', 2, 1, 1, 0];
        elf64_hdr.resize(64, 0);
        fs::write(&elf64_so, &elf64_hdr).unwrap();

        let err = enable_system_preload_at(&preload, &elf64_so).unwrap_err();
        assert!(matches!(err, PreloadError::UnsafeMultilibPreload(_)));
        assert!(!preload.exists());

        // 2. Literal non-existent /usr/lib64 path without $LIB/${LIB} must also be rejected
        let err_lit = enable_system_preload_at(
            &preload,
            Path::new("/usr/lib64/luatools/libsteam_integration.so"),
        )
        .unwrap_err();
        assert!(matches!(err_lit, PreloadError::UnsafeMultilibPreload(_)));
        assert!(!preload.exists());

        // 3. Multilib $LIB and ${LIB} paths are accepted and round-trip cleanly
        let multilib_dollar = Path::new("/usr/$LIB/luatools/libsteam_integration.so");
        let multilib_braced = Path::new("/opt/luatools/${LIB}/libsteam_integration.so");
        enable_system_preload_at(&preload, multilib_dollar).unwrap();
        enable_system_preload_at(&preload, multilib_braced).unwrap();
        let content = fs::read_to_string(&preload).unwrap();
        assert!(content.contains("/usr/$LIB/luatools/libsteam_integration.so"));
        assert!(content.contains("/opt/luatools/${LIB}/libsteam_integration.so"));

        disable_system_preload_at(&preload, multilib_dollar).unwrap();
        disable_system_preload_at(&preload, multilib_braced).unwrap();
        assert_eq!(fs::read_to_string(&preload).unwrap(), "");
    }

    #[test]
    fn has_preload_env_handles_invalid_and_self_pid_safely() {
        assert!(!has_preload_env(0, "libsteam_integration.so"));
        assert!(!has_preload_env(std::process::id(), ""));
        assert!(!has_preload_env(
            std::process::id(),
            "definitely_not_preloaded_xyz_123.so"
        ));
    }
}
