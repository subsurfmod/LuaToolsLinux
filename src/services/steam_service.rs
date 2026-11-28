//! Steam discovery & multi-library game scanner.

use std::fs;
use std::path::{Path, PathBuf};

use crate::models::steam::{
    default_steam_candidates, discover_libraries, get_game_name_from_acf, get_install_dir_from_acf,
    scan_installed_games, SteamInstallation, SteamLibrary,
};

pub fn detect_steam_install(override_path: Option<&str>) -> Option<SteamInstallation> {
    let mut candidates = Vec::new();
    if let Some(ov) = override_path.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(expanded) = expand_tilde(ov) {
            candidates.push(expanded);
        }
    }
    candidates.extend(default_steam_candidates());

    for cand in candidates {
        if !cand.exists() {
            continue;
        }
        let root = fs::canonicalize(&cand).unwrap_or(cand);
        let has_markers = root.join("steam.sh").exists()
            || root.join("steamapps").exists()
            || root.join("config").exists();
        if has_markers {
            let libraries = discover_libraries(&root);
            return Some(SteamInstallation {
                config_path: root.join("config"),
                stplug_in_path: root.join("config").join("stplug-in"),
                root,
                libraries,
            });
        }
    }
    None
}

pub fn expand_tilde(path_str: &str) -> Option<PathBuf> {
    if let Some(rest) = path_str.strip_prefix("~/") {
        return Some(dirs::home_dir()?.join(rest));
    }
    if path_str == "~" {
        return dirs::home_dir();
    }
    Some(PathBuf::from(path_str))
}

#[derive(Debug, Clone, Default)]
pub struct SteamService {
    pub installation: Option<SteamInstallation>,
}

impl SteamService {
    pub fn new() -> Self {
        Self { installation: None }
    }

    pub fn detect(&mut self, override_path: Option<&str>) -> Option<SteamInstallation> {
        self.installation = detect_steam_install(override_path);
        self.installation.clone()
    }

    pub fn is_available(&self) -> bool {
        self.installation
            .as_ref()
            .map(SteamInstallation::is_valid)
            .unwrap_or(false)
    }

    pub fn libraries(&self) -> &[SteamLibrary] {
        self.installation
            .as_ref()
            .map(|i| i.libraries.as_slice())
            .unwrap_or(&[])
    }

    pub fn scan_all_games(&self) -> Vec<u32> {
        let mut all = Vec::new();
        for lib in self.libraries() {
            all.extend(scan_installed_games(lib));
        }
        all.sort_unstable();
        all.dedup();
        all
    }

    pub fn get_game_name(&self, app_id: u32) -> Option<String> {
        for lib in self.libraries() {
            if let Some(name) = get_game_name_from_acf(lib, app_id) {
                return Some(name);
            }
        }
        None
    }

    pub fn find_install_dir(&self, app_id: u32) -> Option<PathBuf> {
        for lib in self.libraries() {
            if let Some(dir) = get_install_dir_from_acf(lib, app_id) {
                if dir.exists() {
                    return Some(dir);
                }
            }
        }
        None
    }

    pub fn list_lua_app_ids(stplug_dir: &Path) -> Vec<u32> {
        let mut ids = Vec::new();
        if let Ok(entries) = fs::read_dir(stplug_dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("lua") {
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        if let Ok(aid) = stem.parse::<u32>() {
                            ids.push(aid);
                        }
                    }
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Terminates all running Steam client, webhelper, overlay, and Steam Runtime processes cleanly.
    /// Sends `SIGTERM` first, waits up to 2 seconds for clean exit, then sends `SIGKILL` to any
    /// stubborn processes.
    pub fn terminate_steam_instances() -> Result<usize, String> {
        crate::steam_bridge::preload_loader::kill_all_steam_instances()
    }

    /// Checks whether any Steam client or helper process is currently running.
    pub fn is_steam_running() -> bool {
        crate::steam_bridge::preload_loader::find_steam_pid().is_some()
    }

    /// Returns true if `root` points to a Flatpak `com.valvesoftware.Steam` installation.
    pub fn is_flatpak_steam_root(root: &Path) -> bool {
        let s = root.to_string_lossy();
        s.contains(".var/app/com.valvesoftware.Steam")
    }

    /// Builds the command program, arguments, and optional `LD_AUDIT` value for relaunching Steam.
    /// Flatpak Steam installations launch via `flatpak run com.valvesoftware.Steam` (LT-18)
    /// rather than spawning the inner sandbox `steam.sh`.
    pub fn build_relaunch_command_spec(
        override_root: Option<&Path>,
        home: &Path,
    ) -> (PathBuf, Vec<String>, Option<String>) {
        let flatpak_root = home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam");
        let native_steam_sh = home.join(".local/share/Steam/steam.sh");
        let steam_sls_bin = home.join(".local/bin/steam-sls");

        let is_flatpak = override_root
            .map(Self::is_flatpak_steam_root)
            .unwrap_or_else(|| {
                !steam_sls_bin.is_file()
                    && !native_steam_sh.is_file()
                    && crate::services::depot_downloader::which_in_path("steam").is_none()
                    && flatpak_root.exists()
            });

        let resolve_audit = |sls_dirs: &[PathBuf]| -> Option<String> {
            for sls_dir in sls_dirs {
                // Flaw 2 & Fix 2: Clean up deprecated library-inject.so; enforce SINGLE auditor (SLSsteam.so)
                let old_lib_inject = sls_dir.join("library-inject.so");
                if old_lib_inject.exists() {
                    let _ = fs::remove_file(old_lib_inject);
                }
                let sls_so = sls_dir.join("SLSsteam.so");
                if sls_so.is_file() {
                    return Some(sls_so.to_string_lossy().into_owned());
                }
            }
            None
        };

        if is_flatpak {
            let flatpak_sls =
                home.join(".var/app/com.valvesoftware.Steam/.local/share/SLSsteam");
            let native_sls = home.join(".local/share/SLSsteam");
            let mut args = vec!["run".to_string()];
            if let Some(audit) = resolve_audit(&[flatpak_sls, native_sls.clone()]) {
                if audit.contains(&native_sls.to_string_lossy().to_string()) {
                    args.push(format!("--filesystem={}:ro", native_sls.display()));
                }
                args.push(format!("--env=LD_AUDIT={}", audit));
            }
            args.push("com.valvesoftware.Steam".to_string());
            return (PathBuf::from("flatpak"), args, None);
        }

        if steam_sls_bin.is_file() {
            return (steam_sls_bin, Vec::new(), None);
        }

        let steam_bin = override_root
            .map(|r| r.join("steam.sh"))
            .filter(|p| p.is_file())
            .or_else(|| native_steam_sh.is_file().then_some(native_steam_sh))
            .unwrap_or_else(|| PathBuf::from("steam"));
        let audit = resolve_audit(&[home.join(".local/share/SLSsteam")]);
        (steam_bin, Vec::new(), audit)
    }

    /// Runs graceful Steam `-shutdown` (with a hard 5s timeout so `-shutdown` never hangs),
    /// waits up to 6s for `find_steam_pid()` to exit, and falls back to `kill_all_steam_instances()`
    /// plus a brief 400ms settle delay so `steam.pid` and IPC sockets release cleanly.
    pub fn graceful_shutdown_steam<F>(override_root: Option<&Path>, mut on_step: F) -> Result<(), String>
    where
        F: FnMut(&str),
    {
        let running = crate::steam_bridge::preload_loader::find_steam_pid().is_some()
            || !crate::steam_bridge::preload_loader::find_all_steam_pids().is_empty();
        if !running {
            return Ok(());
        }

        let is_flatpak = override_root
            .map(Self::is_flatpak_steam_root)
            .unwrap_or_else(|| {
                dirs::home_dir().is_some_and(|h| {
                    h.join(".var/app/com.valvesoftware.Steam/.steam/steam.pid")
                        .exists()
                        && !h.join(".local/share/Steam/steam.sh").is_file()
                })
            });

        on_step("Requesting graceful Steam shutdown…");
        let mut shutdown_cmd = if is_flatpak {
            let mut c = std::process::Command::new("flatpak");
            c.args(["run", "com.valvesoftware.Steam", "-shutdown"]);
            c
        } else {
            let steam_cmd = override_root
                .map(|r| r.join("steam.sh"))
                .filter(|p| p.is_file())
                .or_else(|| {
                    dirs::home_dir()
                        .map(|h| h.join(".local/share/Steam/steam.sh"))
                        .filter(|p| p.is_file())
                })
                .unwrap_or_else(|| PathBuf::from("steam"));
            let mut c = std::process::Command::new(steam_cmd);
            c.arg("-shutdown");
            c
        };
        shutdown_cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        if let Ok(mut child) = shutdown_cmd.spawn() {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => {
                        if std::time::Instant::now() >= deadline {
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(120));
                    }
                    Err(_) => break,
                }
            }
        }

        // Poll up to 6 seconds for Steam main client to exit cleanly
        on_step("Waiting for Steam processes to exit…");
        for _ in 0..30 {
            if crate::steam_bridge::preload_loader::find_steam_pid().is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }

        if crate::steam_bridge::preload_loader::find_steam_pid().is_some()
            || !crate::steam_bridge::preload_loader::find_all_steam_pids().is_empty()
        {
            on_step("Terminating remaining Steam processes…");
            let _ = Self::terminate_steam_instances();
        }

        // Brief settle wait so steam.pid and IPC sockets release cleanly before relaunch
        std::thread::sleep(std::time::Duration::from_millis(400));
        Ok(())
    }

    /// Spawns a detached Steam launcher process (`setsid` via `pre_exec`), captures immediate
    /// stderr crashes for up to ~3 seconds, and verifies Steam PID startup cleanly.
    pub fn spawn_detached_steam_and_verify<F>(
        program: &Path,
        args: &[String],
        ld_audit: Option<&str>,
        mut on_step: F,
    ) -> Result<Option<u32>, String>
    where
        F: FnMut(&str),
    {
        use std::io::Read;
        use std::os::unix::process::CommandExt;

        let mut cmd = std::process::Command::new(program);
        cmd.args(args);
        if let Some(audit) = ld_audit {
            cmd.env("LD_AUDIT", audit);
        }
        // Ensure multilib $LIB libsteam_integration.so is always preloaded on non-Flatpak Steam launches
        // (even when launched via relaunch_steam_sls / HTTP server) so both 32-bit and 64-bit Steam +
        // steamwebhelper receive null-safe V_strncpy and multi-user Millennium socket isolation.
        if program != Path::new("flatpak") {
            if let Some(home) = dirs::home_dir() {
                let lib_base = home.join(".local/lib/luatools");
                if lib_base.join("lib/libsteam_integration.so").is_file()
                    && lib_base.join("lib64/libsteam_integration.so").is_file()
                {
                    let multilib_entry =
                        format!("{}/$LIB/libsteam_integration.so", lib_base.display());
                    let existing = std::env::var("LD_PRELOAD").unwrap_or_default();
                    let cleaned: Vec<&str> = existing
                        .split(':')
                        .filter(|s| !s.is_empty() && !s.contains("libsteam_integration.so"))
                        .collect();
                    let new_preload = if cleaned.is_empty() {
                        multilib_entry
                    } else {
                        format!("{}:{}", multilib_entry, cleaned.join(":"))
                    };
                    cmd.env("LD_PRELOAD", new_preload);
                }
            }
        }
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());

        unsafe {
            cmd.pre_exec(|| {
                let _ = libc::setsid();
                Ok(())
            });
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn {}: {}", program.display(), e))?;

        let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        if let Some(mut err_pipe) = child.stderr.take() {
            let buf_c = std::sync::Arc::clone(&stderr_buf);
            std::thread::spawn(move || {
                let mut chunk = [0u8; 1024];
                loop {
                    match err_pipe.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            if let Ok(mut g) = buf_c.lock() {
                                if g.len() < 4096 {
                                    g.push_str(&String::from_utf8_lossy(&chunk[..n]));
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        on_step("Verifying Steam startup…");
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(3000);
        let mut child_exited_ok = false;

        loop {
            if !child_exited_ok {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        if !status.success() {
                            std::thread::sleep(std::time::Duration::from_millis(120));
                            let err_txt = stderr_buf
                                .lock()
                                .map(|g| g.trim().to_string())
                                .unwrap_or_default();
                            let compact = err_txt
                                .lines()
                                .rev()
                                .take(4)
                                .collect::<Vec<_>>()
                                .into_iter()
                                .rev()
                                .collect::<Vec<_>>()
                                .join(" | ");
                            if compact.is_empty() {
                                return Err(format!(
                                    "{} exited with {}",
                                    program.display(),
                                    status
                                ));
                            } else {
                                return Err(format!(
                                    "{} failed ({}): {}",
                                    program.display(),
                                    status,
                                    compact
                                ));
                            }
                        }
                        child_exited_ok = true;
                    }
                    Ok(None) => {}
                    Err(_) => {}
                }
            }

            if let Some(pid) = crate::steam_bridge::preload_loader::find_steam_pid() {
                return Ok(Some(pid));
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(150));
        }

        Ok(crate::steam_bridge::preload_loader::find_steam_pid())
    }

    /// Unified launch / restart workflow with live step progress callback (`0.0..=1.0`, message).
    pub fn launch_or_restart_steam_with_progress<F>(
        override_root: Option<&Path>,
        prefer_bridge_launcher: bool,
        mut on_progress: F,
    ) -> Result<(Option<u32>, String), String>
    where
        F: FnMut(f32, String),
    {
        let was_running = crate::steam_bridge::preload_loader::find_steam_pid().is_some()
            || !crate::steam_bridge::preload_loader::find_all_steam_pids().is_empty();
        if was_running {
            on_progress(0.15, "Stopping running Steam processes…".into());
            Self::graceful_shutdown_steam(override_root, |step| {
                on_progress(0.32, step.to_string());
            })?;
        }

        let home = dirs::home_dir().ok_or_else(|| "Cannot determine HOME directory".to_string())?;
        let steam_sls = home.join(".local/bin/steam-sls");
        let bridge_launcher = home.join(".local/bin/luatools-steam-launcher");
        let is_flatpak = override_root
            .map(Self::is_flatpak_steam_root)
            .unwrap_or(false);

        let (program, args, ld_audit, label) = if is_flatpak {
            let (p, a, audit) = Self::build_relaunch_command_spec(override_root, &home);
            (p, a, audit, "Flatpak Steam")
        } else if steam_sls.is_file() {
            (steam_sls, Vec::new(), None, "steam-sls (SLSsteam LD_AUDIT)")
        } else if prefer_bridge_launcher && bridge_launcher.is_file() {
            (
                bridge_launcher,
                Vec::new(),
                None,
                "luatools-steam-launcher (Bridge)",
            )
        } else {
            let (p, a, audit) = Self::build_relaunch_command_spec(override_root, &home);
            (p, a, audit, "Steam")
        };

        on_progress(
            0.60,
            format!("Launching Steam via {} ({})…", label, program.display()),
        );
        let pid_opt = Self::spawn_detached_steam_and_verify(
            &program,
            &args,
            ld_audit.as_deref(),
            |step| on_progress(0.82, step.to_string()),
        )?;

        let done_msg = match pid_opt {
            Some(pid) => format!("Steam is running (PID {}) via {}", pid, label),
            None => format!("Steam launched via {} (initializing…)", label),
        };
        on_progress(1.0, done_msg.clone());
        Ok((pid_opt, done_msg))
    }

    /// Cleanly terminates any running Steam instances and relaunches Steam via `steam-sls`
    /// (with `LD_AUDIT=library-inject.so:SLSsteam.so`) or `flatpak run com.valvesoftware.Steam`
    /// for Flatpak Steam roots so SLSsteam is retained without 32-bit / 64-bit LD_PRELOAD crashes.
    pub fn relaunch_steam_sls(override_root: Option<&Path>) -> Result<(), String> {
        Self::launch_or_restart_steam_with_progress(override_root, false, |_, _| {}).map(|_| ())
    }

    /// Instance method: cleanly terminates running Steam and relaunches via `steam-sls`.
    pub fn relaunch_via_sls(&self, override_root: Option<&Path>) -> Result<(), String> {
        let root = override_root.or_else(|| self.installation.as_ref().map(|i| i.root.as_path()));
        Self::relaunch_steam_sls(root)
    }

    /// Extracts Steam's UI language string from VDF text (`config.vdf` or `registry.vdf`).
    /// Prefers `"Language"` inside the `"Steam"` section (`Software -> Valve -> Steam`) so per-app
    /// `"apps" -> "<appid>" -> "language"` entries in `config.vdf` never shadow the client UI language.
    pub fn parse_language_from_vdf_text(content: &str) -> Option<String> {
        let mut section_stack: Vec<String> = Vec::new();
        let mut pending_section: Option<String> = None;
        let mut fallback_lang: Option<String> = None;

        for raw_line in content.lines() {
            let line = match raw_line.find("//") {
                Some(idx) => &raw_line[..idx],
                None => raw_line,
            }
            .trim();
            if line.is_empty() {
                continue;
            }

            let tokens = Self::extract_vdf_line_tokens(line);
            for tok in tokens {
                match tok {
                    VdfLineToken::OpenBrace => {
                        let sec = pending_section
                            .take()
                            .unwrap_or_default()
                            .to_ascii_lowercase();
                        section_stack.push(sec);
                    }
                    VdfLineToken::CloseBrace => {
                        pending_section = None;
                        let _ = section_stack.pop();
                    }
                    VdfLineToken::KeyOnly(k) => {
                        pending_section = Some(k);
                    }
                    VdfLineToken::KeyValue(k, v) => {
                        pending_section = None;
                        if k.eq_ignore_ascii_case("language") {
                            if let Some(normalized) = Self::normalize_steam_language_token(&v) {
                                let in_apps_or_depots = section_stack
                                    .iter()
                                    .any(|s| s == "apps" || s == "depots");
                                let in_steam_section = section_stack
                                    .last()
                                    .map(|s| s == "steam")
                                    .unwrap_or(false)
                                    && !in_apps_or_depots;
                                if in_steam_section {
                                    return Some(normalized);
                                }
                                if !in_apps_or_depots && fallback_lang.is_none() {
                                    fallback_lang = Some(normalized);
                                }
                            }
                        }
                    }
                }
            }
        }

        if fallback_lang.is_some() {
            return fallback_lang;
        }

        let re = regex::Regex::new(r#"(?i)"language"\s+"([^"]+)""#).ok()?;
        re.captures(content)
            .and_then(|c| c.get(1))
            .and_then(|m| Self::normalize_steam_language_token(m.as_str()))
    }

    fn normalize_steam_language_token(raw: &str) -> Option<String> {
        let trimmed = raw.trim().to_ascii_lowercase();
        if trimmed.is_empty() {
            return None;
        }
        if trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            Some(trimmed)
        } else {
            None
        }
    }

    fn extract_vdf_line_tokens(line: &str) -> Vec<VdfLineToken> {
        let mut out = Vec::new();
        let bytes = line.as_bytes();
        let mut i = 0;
        let mut strings_on_line: Vec<String> = Vec::new();

        let flush_strings = |strings: &mut Vec<String>, out: &mut Vec<VdfLineToken>| {
            let mut idx = 0;
            while idx < strings.len() {
                if idx + 1 < strings.len() {
                    out.push(VdfLineToken::KeyValue(
                        strings[idx].clone(),
                        strings[idx + 1].clone(),
                    ));
                    idx += 2;
                } else {
                    out.push(VdfLineToken::KeyOnly(strings[idx].clone()));
                    idx += 1;
                }
            }
            strings.clear();
        };

        while i < bytes.len() {
            let b = bytes[i];
            if b.is_ascii_whitespace() {
                i += 1;
                continue;
            }
            if b == b'{' {
                flush_strings(&mut strings_on_line, &mut out);
                out.push(VdfLineToken::OpenBrace);
                i += 1;
                continue;
            }
            if b == b'}' {
                flush_strings(&mut strings_on_line, &mut out);
                out.push(VdfLineToken::CloseBrace);
                i += 1;
                continue;
            }
            if b == b'"' {
                i += 1;
                let mut buf = String::new();
                while i < bytes.len() {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        if let Some(ch) = line[i + 1..].chars().next() {
                            buf.push(ch);
                            i += 1 + ch.len_utf8();
                            continue;
                        }
                    }
                    if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    if let Some(ch) = line[i..].chars().next() {
                        buf.push(ch);
                        i += ch.len_utf8();
                    } else {
                        break;
                    }
                }
                strings_on_line.push(buf);
                continue;
            }
            i += 1;
        }
        flush_strings(&mut strings_on_line, &mut out);
        out
    }

    /// Reads Steam UI language from `config/config.vdf` (primary) or `registry.vdf` (fallback).
    pub fn detect_steam_language(override_root: Option<&Path>) -> Option<String> {
        let mut candidate_files = Vec::new();

        if let Some(root) = override_root.filter(|p| !p.as_os_str().is_empty()) {
            candidate_files.push(root.join("config").join("config.vdf"));
            candidate_files.push(root.join("registry.vdf"));
        } else {
            let cfg_override = crate::services::settings_service::SettingsService::default()
                .settings
                .steam_path_override;
            if let Some(ov_root) = Some(cfg_override.trim())
                .filter(|s| !s.is_empty())
                .and_then(expand_tilde)
            {
                candidate_files.push(ov_root.join("config").join("config.vdf"));
                candidate_files.push(ov_root.join("registry.vdf"));
            }

            if let Some(home) = dirs::home_dir() {
                candidate_files.push(home.join(".local/share/Steam/config/config.vdf"));
                candidate_files.push(home.join(".steam/steam/config/config.vdf"));
                candidate_files.push(home.join(".steam/root/config/config.vdf"));
                candidate_files.push(home.join(".steam/debian-installation/config/config.vdf"));
                candidate_files.push(
                    home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/config/config.vdf"),
                );
                candidate_files.push(home.join(".steam/registry.vdf"));
                candidate_files.push(home.join(".local/share/Steam/registry.vdf"));
                candidate_files.push(home.join(".steam/steam/registry.vdf"));
                candidate_files.push(home.join(".steam/debian-installation/registry.vdf"));
                candidate_files.push(
                    home.join(".var/app/com.valvesoftware.Steam/.steam/registry.vdf"),
                );
            }
            // LT-16: Include system-wide /var/lib/steam discovery path in language scan
            candidate_files.push(PathBuf::from("/var/lib/steam/config/config.vdf"));
            candidate_files.push(PathBuf::from("/var/lib/steam/registry.vdf"));
        }

        for p in candidate_files {
            if p.is_file() {
                if let Ok(raw) = fs::read_to_string(&p) {
                    if let Some(lang) = Self::parse_language_from_vdf_text(&raw) {
                        if !lang.is_empty() {
                            return Some(lang);
                        }
                    }
                }
            }
        }
        None
    }

    /// Detects Steam Store API language token (`l=<lang>`) from `config.vdf` / `registry.vdf`,
    /// falling back to `"english"` when no Steam language is configured or readable.
    pub fn detect_steam_store_language(override_root: Option<&Path>) -> String {
        Self::detect_steam_language(override_root).unwrap_or_else(|| "english".to_string())
    }

    /// Instance method returning the Steam Store API language token (`"english"`, `"schinese"`, etc.).
    pub fn store_language(&self) -> String {
        let root = self.installation.as_ref().map(|i| i.root.as_path());
        Self::detect_steam_store_language(root)
    }

    /// Maps Steam's internal language string (`schinese`, `russian`, `english`, etc.)
    /// to 2-letter UI language code (`zh`, `ru`, `en`, etc.).
    pub fn steam_lang_to_ui_code(steam_lang: &str) -> &'static str {
        match steam_lang.trim().to_ascii_lowercase().as_str() {
            "schinese" | "tchinese" | "chinese" | "zh" => "zh",
            "russian" | "ru" => "ru",
            "spanish" | "latam" | "es" => "es",
            "german" | "de" => "de",
            "french" | "fr" => "fr",
            "brazilian" | "portuguese" | "pt" => "pt",
            "japanese" | "ja" => "ja",
            "italian" | "it" => "it",
            "koreana" | "korean" | "ko" => "ko",
            "polish" | "pl" => "pl",
            "dutch" | "nl" => "nl",
            "turkish" | "tr" => "tr",
            "arabic" | "ar" => "ar",
            "thai" | "th" => "th",
            "vietnamese" | "vi" => "vi",
            "ukrainian" | "uk" => "uk",
            "czech" | "cs" => "cs",
            "hungarian" | "hu" => "hu",
            "romanian" | "ro" => "ro",
            "swedish" | "sv" => "sv",
            "danish" | "da" => "da",
            "finnish" | "fi" => "fi",
            "norwegian" | "no" => "no",
            "bulgarian" | "bg" => "bg",
            "greek" | "el" => "el",
            "indonesian" | "id" => "id",
            _ => "en",
        }
    }

    /// Detects Steam UI language and returns the normalized 2-letter code (e.g. "zh", "ru", "en").
    pub fn detect_steam_ui_language_code(override_root: Option<&Path>) -> Option<String> {
        Self::detect_steam_language(override_root)
            .map(|l| Self::steam_lang_to_ui_code(&l).to_string())
    }

    /// Instance method returning detected Steam UI language code.
    pub fn ui_language_code(&self) -> Option<String> {
        let root = self.installation.as_ref().map(|i| i.root.as_path());
        Self::detect_steam_ui_language_code(root)
    }
}

#[derive(Debug, Clone)]
enum VdfLineToken {
    OpenBrace,
    CloseBrace,
    KeyOnly(String),
    KeyValue(String, String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_steam_service_detection_and_query() {
        let mut svc = SteamService::new();
        let _ = svc.detect(None);
        let _ = svc.is_available();
        let _ = svc.scan_all_games();
    }

    #[test]
    fn test_parse_language_from_vdf() {
        let registry_vdf = r#"
"Registry"
{
    "HKCU"
    {
        "Software"
        {
            "Valve"
            {
                "Steam"
                {
                    "Language"		"schinese"
                }
            }
        }
    }
}
"#;
        assert_eq!(
            SteamService::parse_language_from_vdf_text(registry_vdf),
            Some("schinese".to_string())
        );
        assert_eq!(SteamService::steam_lang_to_ui_code("schinese"), "zh");
        assert_eq!(SteamService::steam_lang_to_ui_code("russian"), "ru");
        assert_eq!(SteamService::steam_lang_to_ui_code("english"), "en");
    }

    #[test]
    fn test_config_vdf_language_detection_ignores_per_app_override_and_falls_back_to_english() {
        let dir = tempfile::tempdir().unwrap();
        let steam_root = dir.path().join("Steam");
        fs::create_dir_all(steam_root.join("config")).unwrap();

        // Empty root without config.vdf falls back to "english"
        assert_eq!(SteamService::detect_steam_language(Some(&steam_root)), None);
        assert_eq!(
            SteamService::detect_steam_store_language(Some(&steam_root)),
            "english"
        );

        // config.vdf where per-app "apps" entry appears BEFORE Steam client "Language"
        let config_vdf = r#"
"InstallConfigStore"
{
    "Software"
    {
        "Valve"
        {
            "Steam"
            {
                "apps"
                {
                    "480"
                    {
                        "language"		"english"
                    }
                }
                "Language"		"japanese"
            }
        }
    }
}
"#;
        fs::write(steam_root.join("config/config.vdf"), config_vdf).unwrap();

        assert_eq!(
            SteamService::detect_steam_language(Some(&steam_root)),
            Some("japanese".to_string())
        );
        assert_eq!(
            SteamService::detect_steam_store_language(Some(&steam_root)),
            "japanese"
        );
        assert_eq!(
            SteamService::detect_steam_ui_language_code(Some(&steam_root)),
            Some("ja".to_string())
        );
    }

    #[test]
    fn test_flatpak_relaunch_uses_flatpak_run_instead_of_inner_steam_sh() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let fp_root = home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam");
        fs::create_dir_all(&fp_root).unwrap();
        fs::write(fp_root.join("steam.sh"), b"#!/bin/sh\n").unwrap();

        let (prog, args, audit) = SteamService::build_relaunch_command_spec(Some(&fp_root), home);
        assert_eq!(prog, PathBuf::from("flatpak"));
        assert!(args.iter().any(|a| a == "run"));
        assert!(args.iter().any(|a| a == "com.valvesoftware.Steam"));
        assert!(audit.is_none());
        assert!(SteamService::is_flatpak_steam_root(&fp_root));
    }
}
