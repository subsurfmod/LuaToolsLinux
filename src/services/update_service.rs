//! UpdateService — one-shot background Git update check, `git pull` + native rebuild,
//! atomic executable replacement (immune to Linux `ETXTBSY`), and one-click relaunch.

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::config::default_luatools_data_dir;
use crate::services::file_hash::FileHashService;
use crate::services::proxied_file_downloader::ProxiedFileDownloader;
use crate::services::steam_service::expand_tilde;
use crate::{BUILD_GIT_SHA, VERSION};

static PREBUILT_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrebuiltAssetKind {
    RawBinary,
    TarGz,
    TarXz,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrebuiltReleaseAsset {
    pub name: String,
    pub download_url: String,
    pub digest: Option<String>,
    pub size: Option<u64>,
    pub kind: PrebuiltAssetKind,
}

pub const DEFAULT_GIT_REMOTE_URL: &str = "https://github.com/subsurfmod/LuaToolsLinux.git";
pub const BACKUP_GIT_REMOTE_URLS: &[&str] = &[
    "https://ghfast.top/https://github.com/subsurfmod/LuaToolsLinux.git",
    "https://gh-proxy.com/https://github.com/subsurfmod/LuaToolsLinux.git",
];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SourceRepoMeta {
    #[serde(default)]
    pub repo_dir: String,
    #[serde(default)]
    pub remote_url: String,
}

#[derive(Debug, Clone)]
pub struct UpdateCheckResult {
    pub available: bool,
    pub current_version: String,
    pub remote_version: Option<String>,
    pub commits_behind: usize,
    pub latest_commit: Option<String>,
    pub repo_dir: Option<PathBuf>,
    pub remote_url: Option<String>,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct UpdateApplyResult {
    pub new_version: String,
    pub commit_info: String,
    pub updated_exe: PathBuf,
}

#[derive(Debug, Clone)]
pub struct UpdateProgressEvent {
    pub fraction: f32,
    pub stage_label: String,
    pub current_crate: String,
    pub compiled_crates: usize,
    pub total_crates: usize,
    pub log_line: Option<String>,
}

/// Parses live `cargo build` stderr/stdout output lines into smooth, monotonic fractional
/// progress (`base_fraction..=end_fraction`) with real crate counts and active crate name.
#[derive(Debug, Clone)]
pub struct CargoProgressTracker {
    pub compiled_crates: usize,
    pub total_crates: usize,
    pub current_crate: String,
    pub stage_label: String,
    pub fraction: f32,
    pub in_linking: bool,
    pub finished: bool,
    pub base_fraction: f32,
    pub end_fraction: f32,
}

impl CargoProgressTracker {
    pub fn new(total_crates_est: usize, base_fraction: f32, end_fraction: f32) -> Self {
        let base = base_fraction.clamp(0.0, 1.0);
        let end = end_fraction.clamp(base, 1.0);
        Self {
            compiled_crates: 0,
            total_crates: total_crates_est.max(1),
            current_crate: String::new(),
            stage_label: "Compiling Rust".into(),
            fraction: base,
            in_linking: false,
            finished: false,
            base_fraction: base,
            end_fraction: end,
        }
    }

    pub fn feed_line(&mut self, raw_line: &str) -> UpdateProgressEvent {
        let cleaned = strip_ansi_escapes(raw_line);
        let trimmed = cleaned.trim();
        let span = (self.end_fraction - self.base_fraction).max(0.0);

        if let Some((done, total, active)) = parse_cargo_building_progress(trimmed) {
            if total > 0 {
                self.total_crates = total;
                self.compiled_crates = done.min(total);
                if let Some(crate_hint) = active {
                    self.current_crate = crate_hint;
                }
                self.stage_label = "Compiling Rust".into();
                let ratio = (self.compiled_crates as f32 / self.total_crates as f32).clamp(0.0, 1.0);
                let target = self.base_fraction + ratio * span;
                self.fraction = self.fraction.max(target).min(self.end_fraction);
            }
        } else if let Some(crate_desc) = parse_cargo_crate_action(trimmed) {
            self.compiled_crates = self.compiled_crates.saturating_add(1);
            if self.compiled_crates > self.total_crates {
                let bump = (self.compiled_crates / 6).max(8);
                self.total_crates = self.compiled_crates.saturating_add(bump);
            }
            self.current_crate = crate_desc;
            self.stage_label = "Compiling Rust".into();
            let ratio = (self.compiled_crates as f32 / self.total_crates.max(1) as f32).clamp(0.0, 0.96);
            let target = self.base_fraction + ratio * span;
            let cap = (self.end_fraction - 0.015).max(self.base_fraction);
            self.fraction = self.fraction.max(target).min(cap);
        } else if trimmed.starts_with("Updating ") {
            self.stage_label = "Updating crate index".into();
            let target = (self.base_fraction + span * 0.02).min(self.end_fraction);
            self.fraction = self.fraction.max(target);
        } else if let Some(rest) = trimmed
            .strip_prefix("Downloaded ")
            .or_else(|| trimmed.strip_prefix("Downloading "))
        {
            self.stage_label = "Downloading crates".into();
            let desc = compact_crate_tokens(rest);
            if !desc.is_empty() && !desc.starts_with("crates") {
                self.current_crate = desc;
            }
            let target = (self.base_fraction + span * 0.05).min(self.end_fraction);
            self.fraction = self.fraction.max(target);
        } else if trimmed.starts_with("Linking ")
            || trimmed.to_ascii_lowercase().contains("linking with")
        {
            self.in_linking = true;
            self.stage_label = "Linking release binary".into();
            let target = (self.end_fraction - 0.015).max(self.base_fraction);
            self.fraction = self.fraction.max(target).min(self.end_fraction);
        } else if trimmed.starts_with("Finished ") {
            self.finished = true;
            self.total_crates = self.total_crates.max(self.compiled_crates);
            self.compiled_crates = self.total_crates;
            self.stage_label = "Compilation finished".into();
            self.fraction = self.fraction.max(self.end_fraction);
        }

        UpdateProgressEvent {
            fraction: self.fraction,
            stage_label: self.stage_label.clone(),
            current_crate: self.current_crate.clone(),
            compiled_crates: self.compiled_crates,
            total_crates: self.total_crates,
            log_line: (!trimmed.is_empty()).then(|| trimmed.to_string()),
        }
    }
}

fn strip_ansi_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for ch in chars.by_ref() {
                    if ch.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn compact_crate_tokens(rest: &str) -> String {
    let mut parts = rest.split_whitespace();
    let Some(first) = parts.next() else {
        return String::new();
    };
    let clean_first = first.trim_end_matches(':');
    if let Some(second) = parts.next() {
        if second.starts_with('v') && second.len() > 1 && second[1..].starts_with(|c: char| c.is_ascii_digit()) {
            return format!("{} {}", clean_first, second.trim_end_matches(|c| c == '(' || c == ':'));
        }
    }
    clean_first.to_string()
}

fn parse_cargo_crate_action(trimmed: &str) -> Option<String> {
    let rest = trimmed
        .strip_prefix("Compiling ")
        .or_else(|| trimmed.strip_prefix("Checking "))
        .or_else(|| trimmed.strip_prefix("Fresh "))?;
    let desc = compact_crate_tokens(rest);
    (!desc.is_empty()).then_some(desc)
}

fn parse_cargo_building_progress(trimmed: &str) -> Option<(usize, usize, Option<String>)> {
    let after_build = trimmed.strip_prefix("Building [")?;
    let close_idx = after_build.find(']')?;
    let tail = after_build[close_idx + 1..].trim();
    let (counts_part, active_part) = match tail.split_once(':') {
        Some((c, a)) => (c.trim(), Some(a.trim())),
        None => (tail, None),
    };
    let (done_s, total_s) = counts_part.split_once('/')?;
    let done = done_s.trim().parse::<usize>().ok()?;
    let total_token = total_s.split_whitespace().next().unwrap_or("");
    let total = total_token.parse::<usize>().ok()?;
    let active = active_part
        .and_then(|a| a.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    Some((done, total, active))
}

fn stream_reader_lines<R: Read>(mut reader: R, tx: mpsc::Sender<String>) {
    let mut buf = [0u8; 4096];
    let mut acc: Vec<u8> = Vec::with_capacity(256);
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                if !acc.is_empty() {
                    let s = String::from_utf8_lossy(&acc).to_string();
                    let _ = tx.send(s);
                }
                break;
            }
            Ok(n) => {
                for &b in &buf[..n] {
                    if b == b'\n' || b == b'\r' {
                        if !acc.is_empty() {
                            let s = String::from_utf8_lossy(&acc).to_string();
                            acc.clear();
                            if tx.send(s).is_err() {
                                return;
                            }
                        }
                    } else {
                        acc.push(b);
                        if acc.len() >= 4096 {
                            let s = String::from_utf8_lossy(&acc).to_string();
                            acc.clear();
                            if tx.send(s).is_err() {
                                return;
                            }
                        }
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

fn run_command_streaming<F>(mut cmd: Command, mut on_line: F) -> Result<Vec<String>, (String, Vec<String>)>
where
    F: FnMut(&str),
{
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| (format!("Failed to spawn process: {}", e), Vec::new()))?;

    let (tx, rx) = mpsc::channel::<String>();
    let h_out = child.stdout.take().map(|out| {
        let tx_c = tx.clone();
        thread::spawn(move || stream_reader_lines(out, tx_c))
    });
    let h_err = child.stderr.take().map(|err| {
        let tx_c = tx.clone();
        thread::spawn(move || stream_reader_lines(err, tx_c))
    });
    drop(tx);

    let mut captured: Vec<String> = Vec::new();
    for raw in rx {
        let clean = strip_ansi_escapes(&raw);
        let trimmed = clean.trim();
        if !trimmed.is_empty() {
            on_line(trimmed);
            captured.push(trimmed.to_string());
            if captured.len() > 250 {
                captured.remove(0);
            }
        }
    }

    if let Some(h) = h_out {
        let _ = h.join();
    }
    if let Some(h) = h_err {
        let _ = h.join();
    }

    match child.wait() {
        Ok(status) if status.success() => Ok(captured),
        Ok(status) => {
            let tail: Vec<String> = captured.iter().rev().take(6).cloned().collect::<Vec<_>>().into_iter().rev().collect();
            let msg = if tail.is_empty() {
                format!("Process exited with status {}", status)
            } else {
                tail.join(" | ")
            };
            Err((msg, captured))
        }
        Err(e) => Err((format!("Failed waiting on process: {}", e), captured)),
    }
}

pub struct UpdateService;

impl UpdateService {
    pub fn meta_path() -> PathBuf {
        default_luatools_data_dir().join("source_repo.json")
    }

    pub fn load_meta() -> SourceRepoMeta {
        fs::read_to_string(Self::meta_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_meta(repo_dir: Option<&Path>, remote_url: Option<&str>) {
        let mut meta = Self::load_meta();
        if let Some(d) = repo_dir {
            meta.repo_dir = d.to_string_lossy().to_string();
        }
        if let Some(u) = remote_url.map(str::trim).filter(|s| !s.is_empty()) {
            meta.remote_url = u.to_string();
        }
        if let Some(parent) = Self::meta_path().parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(js) = serde_json::to_string_pretty(&meta) {
            let _ = fs::write(Self::meta_path(), js);
        }
    }

    /// Resolves the local source / git repository directory if one exists on disk.
    pub fn discover_repo_dir(override_dir: &str) -> Option<PathBuf> {
        let mut candidates: Vec<PathBuf> = Vec::new();

        if let Some(ov) = Some(override_dir.trim()).filter(|s| !s.is_empty()) {
            if let Some(p) = expand_tilde(ov) {
                candidates.push(p);
            }
        }

        let meta = Self::load_meta();
        if !meta.repo_dir.trim().is_empty() {
            if let Some(p) = expand_tilde(meta.repo_dir.trim()) {
                candidates.push(p);
            }
        }

        if let Ok(exe) = std::env::current_exe() {
            for anc in exe.ancestors().skip(1).take(5) {
                candidates.push(anc.to_path_buf());
            }
        }
        if let Ok(cwd) = std::env::current_dir() {
            for anc in cwd.ancestors().take(4) {
                candidates.push(anc.to_path_buf());
            }
        }

        candidates.push(default_luatools_data_dir().join("repo"));
        if let Some(home) = dirs::home_dir() {
            candidates.push(home.join("Documents/luatools-linux-v2.1"));
            candidates.push(home.join("Documents/LuaToolsLinux"));
            candidates.push(home.join("LuaToolsLinux"));
            candidates.push(home.join("Documents/luatools-linux-v2"));
        }

        // First prefer directories containing `.git`
        for cand in &candidates {
            if cand.join(".git").exists() {
                return Some(cand.clone());
            }
            if let Some(parent) = cand.parent() {
                if parent.join(".git").exists() && cand.join("Cargo.toml").exists() {
                    return Some(parent.to_path_buf());
                }
            }
        }

        // Otherwise return a directory with Cargo.toml if present
        candidates.into_iter().find(|c| {
            c.join("Cargo.toml").exists()
                || c.join("luatools-linux-v2.1/Cargo.toml").exists()
                || c.join("luatools-linux-v2/Cargo.toml").exists()
        })
    }

    pub fn resolve_crate_dir(repo_dir: &Path) -> Option<PathBuf> {
        if repo_dir.join("Cargo.toml").is_file() {
            return Some(repo_dir.to_path_buf());
        }
        for sub_name in ["luatools-linux-v2.1", "luatools-linux-v2"] {
            let sub = repo_dir.join(sub_name);
            if sub.join("Cargo.toml").is_file() {
                return Some(sub);
            }
        }
        None
    }

    fn git_cmd(repo_dir: &Path, args: &[&str]) -> Result<String, String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo_dir)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_HTTP_LOW_SPEED_LIMIT", "1000")
            .env("GIT_HTTP_LOW_SPEED_TIME", "8")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("Failed to execute git: {}", e))?;

        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let out_s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            Err(if !err.is_empty() { err } else { out_s })
        }
    }

    pub fn parse_cargo_version(cargo_toml_text: &str) -> Option<String> {
        let mut in_pkg = false;
        for raw_line in cargo_toml_text.lines() {
            let line = raw_line.trim();
            if line.starts_with('[') {
                in_pkg = line == "[package]";
                continue;
            }
            if in_pkg && line.starts_with("version") {
                let (_, rhs) = line.split_once('=')?;
                let cleaned = rhs.trim().trim_matches('"').trim_matches('\'').trim();
                if !cleaned.is_empty() {
                    return Some(cleaned.to_string());
                }
            }
        }
        None
    }

    pub fn is_newer_semver(remote: &str, current: &str) -> bool {
        let split_ver = |s: &str| -> (Vec<u32>, Option<String>) {
            let trimmed = s.trim().trim_start_matches('v');
            let core_and_pre = trimmed.split('+').next().unwrap_or(trimmed);
            let (nums_part, pre_opt) = match core_and_pre.split_once('-') {
                Some((n, p)) if !p.trim().is_empty() => (n, Some(p.trim().to_ascii_lowercase())),
                _ => (core_and_pre, None),
            };
            let mut nums: Vec<u32> = nums_part
                .split('.')
                .filter(|p| !p.is_empty())
                .filter_map(|p| p.parse::<u32>().ok())
                .collect();
            while nums.len() < 3 && !nums.is_empty() {
                nums.push(0);
            }
            (nums, pre_opt)
        };

        let (r_nums, r_pre) = split_ver(remote);
        let (c_nums, c_pre) = split_ver(current);
        if r_nums.is_empty() || c_nums.is_empty() {
            return false;
        }
        for i in 0..r_nums.len().max(c_nums.len()) {
            let rv = r_nums.get(i).copied().unwrap_or(0);
            let cv = c_nums.get(i).copied().unwrap_or(0);
            if rv != cv {
                return rv > cv;
            }
        }
        match (r_pre, c_pre) {
            (None, Some(_)) => true, // Stable 2.0.0 is newer than 2.0-beta
            (Some(_), None) => false,
            (Some(rp), Some(cp)) => compare_prerelease(&rp, &cp) == std::cmp::Ordering::Greater,
            (None, None) => false,
        }
    }

    /// Builds ordered failover git remote URLs: primary URL first, followed by GitHub mirror
    /// proxies and backup repository mirrors (LT-50).
    pub fn remote_failover_urls(primary_url: &str) -> Vec<String> {
        let mut urls = Vec::new();
        let trimmed = primary_url.trim();
        if !trimmed.is_empty() {
            urls.push(trimmed.to_string());
            if crate::services::github_proxy::GithubProxy::is_github(trimmed) {
                for cand in crate::services::github_proxy::GithubProxy::new().candidates(trimmed) {
                    if !urls.contains(&cand) {
                        urls.push(cand);
                    }
                }
            }
        }
        if trimmed == DEFAULT_GIT_REMOTE_URL || trimmed.is_empty() {
            for &b in BACKUP_GIT_REMOTE_URLS {
                let s = b.to_string();
                if crate::services::github_proxy::GithubProxy::is_trusted_mirror(&s) && !urls.contains(&s) {
                    urls.push(s);
                }
            }
        }
        urls
    }

    /// Checks whether the target executable path (or its parent directory) is writable by the
    /// current user before attempting an in-place self-update (LT-50).
    pub fn is_path_writable(target_bin: &Path) -> bool {
        let Some(parent) = target_bin.parent() else {
            return false;
        };
        if !parent.exists() {
            return false;
        }
        let probe = parent.join(format!(
            ".luatools.write_probe.{}.tmp",
            std::process::id()
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
        {
            Ok(_) => {
                let _ = fs::remove_file(&probe);
                true
            }
            Err(_) => false,
        }
    }

    /// Performs a single lightweight background check against the Git remote.
    pub fn check_once(override_repo_dir: &str, override_remote_url: &str) -> UpdateCheckResult {
        Self::check_once_with_build_info(
            override_repo_dir,
            override_remote_url,
            VERSION,
            BUILD_GIT_SHA,
        )
    }

    pub fn check_once_with_build_info(
        override_repo_dir: &str,
        override_remote_url: &str,
        running_version: &str,
        build_git_sha: &str,
    ) -> UpdateCheckResult {
        let current_version = running_version.to_string();
        let meta = if override_repo_dir.trim().is_empty() {
            Self::load_meta()
        } else {
            SourceRepoMeta::default()
        };
        let explicit_remote = (!override_remote_url.trim().is_empty())
            .then(|| override_remote_url.trim().to_string());

        let repo_dir = Self::discover_repo_dir(override_repo_dir);

        if let Some(ref dir) = repo_dir {
            if dir.join(".git").exists() {
                let origin_url = Self::git_cmd(dir, &["remote", "get-url", "origin"]).ok();
                let meta_remote = (!meta.remote_url.trim().is_empty())
                    .then(|| meta.remote_url.trim().to_string());
                let effective_remote = explicit_remote
                    .clone()
                    .or_else(|| origin_url.clone())
                    .or(meta_remote);

                if let Some(ref rem_url) = effective_remote {
                    let mut fetch_res = if origin_url.is_some() && explicit_remote.is_none() {
                        Self::git_cmd(dir, &["fetch", "--quiet", "origin"])
                    } else {
                        Self::git_cmd(dir, &["fetch", "--quiet", rem_url])
                    };
                    let mut used_mirror = false;
                    if fetch_res.is_err() {
                        for mirror_url in Self::remote_failover_urls(rem_url).into_iter().skip(1) {
                            if Self::git_cmd(dir, &["fetch", "--quiet", &mirror_url]).is_ok() {
                                fetch_res = Ok(String::new());
                                used_mirror = true;
                                break;
                            }
                        }
                    }

                    if let Err(e) = fetch_res {
                        return UpdateCheckResult {
                            available: false,
                            current_version,
                            remote_version: None,
                            commits_behind: 0,
                            latest_commit: None,
                            repo_dir: Some(dir.clone()),
                            remote_url: Some(rem_url.clone()),
                            summary: format!("Git fetch skipped: {}", e),
                        };
                    }

                    let upstream_candidates = if origin_url.is_some()
                        && explicit_remote.is_none()
                        && !used_mirror
                    {
                        vec![
                            "@{u}",
                            "origin/HEAD",
                            "origin/main",
                            "origin/master",
                            "FETCH_HEAD",
                        ]
                    } else {
                        vec!["FETCH_HEAD", "@{u}", "origin/main", "origin/master"]
                    };

                    let mut target_ref = None;
                    for cand in upstream_candidates {
                        if Self::git_cmd(dir, &["rev-parse", "--verify", "--quiet", cand]).is_ok() {
                            target_ref = Some(cand);
                            break;
                        }
                    }

                    if let Some(up_ref) = target_ref {
                        let behind = Self::git_cmd(
                            dir,
                            &["rev-list", "--count", &format!("HEAD..{}", up_ref)],
                        )
                        .ok()
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(0);

                        let latest_commit =
                            Self::git_cmd(dir, &["log", "-1", "--format=%h — %s", up_ref])
                                .ok()
                                .filter(|s| !s.is_empty());

                        let remote_cargo =
                            Self::git_cmd(dir, &["show", &format!("{}:Cargo.toml", up_ref)])
                                .or_else(|_| {
                                    Self::git_cmd(
                                        dir,
                                        &[
                                            "show",
                                            &format!("{}:luatools-linux-v2/Cargo.toml", up_ref),
                                        ],
                                    )
                                })
                                .ok();
                        let remote_version =
                            remote_cargo.as_deref().and_then(Self::parse_cargo_version);

                        let binary_behind = if !build_git_sha.is_empty()
                            && Self::git_cmd(
                                dir,
                                &["cat-file", "-e", &format!("{}^{{commit}}", build_git_sha)],
                            )
                            .is_ok()
                        {
                            Self::git_cmd(
                                dir,
                                &[
                                    "rev-list",
                                    "--count",
                                    &format!("{}..{}", build_git_sha, up_ref),
                                ],
                            )
                            .ok()
                            .and_then(|s| s.parse::<usize>().ok())
                            .unwrap_or(0)
                        } else {
                            0
                        };
                        let version_behind = remote_version
                            .as_deref()
                            .map(|rv| Self::is_newer_semver(rv, &current_version))
                            .unwrap_or(false);
                        let effective_behind =
                            behind.max(binary_behind).max(usize::from(version_behind));
                        let available = effective_behind > 0;

                        let summary = if behind > 0 {
                            match (&remote_version, behind) {
                                (Some(v), b) if b > 0 => {
                                    format!("Update available: v{} ({} new commit(s))", v, b)
                                }
                                (Some(v), _) => format!("Update available: v{}", v),
                                (None, b) => format!("Update available: {} new commit(s)", b),
                            }
                        } else if version_behind {
                            format!(
                                "Update ready to build: running v{}, repo is v{} (click Pull & Update Now)",
                                current_version,
                                remote_version.as_deref().unwrap_or("latest")
                            )
                        } else if binary_behind > 0 {
                            format!(
                                "Update ready to build: running binary is {} commit(s) behind repo (click Pull & Update Now)",
                                binary_behind
                            )
                        } else {
                            format!("Up to date (v{})", current_version)
                        };

                        return UpdateCheckResult {
                            available,
                            current_version,
                            remote_version,
                            commits_behind: effective_behind,
                            latest_commit,
                            repo_dir: Some(dir.clone()),
                            remote_url: Some(rem_url.clone()),
                            summary,
                        };
                    }
                } else {
                    return UpdateCheckResult {
                        available: false,
                        current_version,
                        remote_version: None,
                        commits_behind: 0,
                        latest_commit: None,
                        repo_dir: Some(dir.clone()),
                        remote_url: None,
                        summary: "Git repo has no 'origin' remote configured yet (set Remote URL in Settings › Updates & Toolchain)."
                            .into(),
                    };
                }
            }
        }

        // Fallback: no local .git checkout yet, check GitHub API / raw Cargo.toml against configured or default remote
        let fallback_remote = explicit_remote
            .or_else(|| {
                (!meta.remote_url.trim().is_empty()).then(|| meta.remote_url.trim().to_string())
            })
            .unwrap_or_else(|| DEFAULT_GIT_REMOTE_URL.to_string());
        if let Some((remote_ver, note)) = Self::check_github_remote_version(&fallback_remote) {
            let available = Self::is_newer_semver(&remote_ver, &current_version);
            return UpdateCheckResult {
                available,
                summary: if available {
                    format!("Update available from remote: v{}", remote_ver)
                } else {
                    format!("Up to date (v{})", current_version)
                },
                current_version,
                remote_version: Some(remote_ver),
                commits_behind: usize::from(available),
                latest_commit: note,
                repo_dir,
                remote_url: Some(fallback_remote),
            };
        }

        UpdateCheckResult {
            available: false,
            current_version,
            remote_version: None,
            commits_behind: 0,
            latest_commit: None,
            repo_dir,
            remote_url: Some(fallback_remote),
            summary: "No reachable Git remote (configure in Settings › Updates & Toolchain to enable Git updates)."
                .into(),
        }
    }

    fn parse_github_owner_repo(remote_url: &str) -> Option<(String, String)> {
        let trimmed = remote_url
            .trim()
            .trim_end_matches(".git")
            .trim_end_matches('/');

        let slug = if let Some(idx) = trimmed.find("https://github.com/") {
            &trimmed[idx + "https://github.com/".len()..]
        } else if let Some(idx) = trimmed.find("http://github.com/") {
            &trimmed[idx + "http://github.com/".len()..]
        } else if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
            rest
        } else {
            return None;
        };

        let mut parts = slug.split('/');
        let owner = parts.next()?.trim();
        let repo = parts.next()?.trim();
        if owner.is_empty() || repo.is_empty() {
            return None;
        }
        Some((owner.to_string(), repo.to_string()))
    }

    fn resolve_effective_remote_url(override_repo_dir: &str, override_remote_url: &str) -> String {
        if !override_remote_url.trim().is_empty() {
            return override_remote_url.trim().to_string();
        }
        if let Some(dir) = Self::discover_repo_dir(override_repo_dir) {
            if dir.join(".git").exists() {
                if let Ok(origin) = Self::git_cmd(&dir, &["remote", "get-url", "origin"]) {
                    if !origin.trim().is_empty() {
                        return origin.trim().to_string();
                    }
                }
            }
        }
        if override_repo_dir.trim().is_empty() {
            let meta = Self::load_meta();
            if !meta.remote_url.trim().is_empty() {
                return meta.remote_url.trim().to_string();
            }
        }
        DEFAULT_GIT_REMOTE_URL.to_string()
    }

    fn check_github_remote_version(remote_url: &str) -> Option<(String, Option<String>)> {
        let (owner, repo) = Self::parse_github_owner_repo(remote_url)?;

        let downloader = ProxiedFileDownloader::new();

        // Check latest release first (with GitHub proxy mirror fallback)
        let rel_url = format!(
            "https://api.github.com/repos/{}/{}/releases/latest",
            owner, repo
        );
        if let Ok(txt) = downloader.download_string_sync(&rel_url) {
            if let Ok(val) = serde_json::from_str::<Value>(&txt) {
                if let Some(tag) = val.get("tag_name").and_then(|v| v.as_str()) {
                    let clean = tag.trim_start_matches('v').to_string();
                    let name = val
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string);
                    return Some((clean, name));
                }
            }
        }

        // Check raw Cargo.toml on main/master
        for branch in ["main", "master"] {
            for rel_path in ["Cargo.toml", "luatools-linux-v2/Cargo.toml"] {
                let raw_url = format!(
                    "https://raw.githubusercontent.com/{}/{}/{}/{}",
                    owner, repo, branch, rel_path
                );
                if let Ok(txt) = downloader.download_string_sync(&raw_url) {
                    if let Some(ver) = Self::parse_cargo_version(&txt) {
                        return Some((ver, Some(format!("{}/{} ({})", owner, repo, branch))));
                    }
                }
            }
        }
        None
    }

    /// Pulls latest source files from Git into `repo_dir` (or clones if needed).
    pub fn pull_git_changes(
        override_repo_dir: &str,
        override_remote_url: &str,
    ) -> Result<(PathBuf, String), String> {
        Self::pull_git_changes_streaming(override_repo_dir, override_remote_url, |_| {})
    }

    fn git_cmd_streaming<F>(repo_dir: &Path, args: &[&str], mut on_line: F) -> Result<(), String>
    where
        F: FnMut(&str),
    {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(repo_dir)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_HTTP_LOW_SPEED_LIMIT", "1000")
            .env("GIT_HTTP_LOW_SPEED_TIME", "8");
        run_command_streaming(cmd, |line| on_line(line))
            .map(|_| ())
            .map_err(|(err, _)| err)
    }

    /// Streaming variant of `pull_git_changes` emitting every git status/progress line live.
    pub fn pull_git_changes_streaming<F>(
        override_repo_dir: &str,
        override_remote_url: &str,
        mut on_line: F,
    ) -> Result<(PathBuf, String), String>
    where
        F: FnMut(&str),
    {
        let meta = if override_repo_dir.trim().is_empty() {
            Self::load_meta()
        } else {
            SourceRepoMeta::default()
        };
        let explicit_remote = (!override_remote_url.trim().is_empty())
            .then(|| override_remote_url.trim().to_string());

        if let Some(dir) = Self::discover_repo_dir(override_repo_dir) {
            if dir.join(".git").exists() {
                on_line(&format!("Repository: {}", dir.display()));
                if let Some(ref rem) = explicit_remote {
                    if Self::git_cmd(&dir, &["remote", "get-url", "origin"]).is_ok() {
                        let _ = Self::git_cmd(&dir, &["remote", "set-url", "origin", rem]);
                    } else {
                        let _ = Self::git_cmd(&dir, &["remote", "add", "origin", rem]);
                    }
                }

                on_line("$ git pull --ff-only --progress");
                if Self::git_cmd_streaming(&dir, &["pull", "--ff-only", "--progress"], &mut on_line)
                    .is_err()
                {
                    on_line("Fast-forward unavailable; trying git pull --rebase --autostash…");
                    if let Err(primary_err) = Self::git_cmd_streaming(
                        &dir,
                        &["pull", "--rebase", "--autostash", "--progress"],
                        &mut on_line,
                    ) {
                        let base_remote = explicit_remote
                            .clone()
                            .or_else(|| Self::git_cmd(&dir, &["remote", "get-url", "origin"]).ok())
                            .unwrap_or_else(|| DEFAULT_GIT_REMOTE_URL.to_string());
                        let mut pulled_via_mirror = false;
                        for mirror in Self::remote_failover_urls(&base_remote).into_iter().skip(1) {
                            on_line(&format!("Retrying via mirror: {}", mirror));
                            if Self::git_cmd_streaming(
                                &dir,
                                &["pull", "--ff-only", "--progress", &mirror],
                                &mut on_line,
                            )
                            .is_ok()
                                || Self::git_cmd_streaming(
                                    &dir,
                                    &["pull", "--rebase", "--autostash", "--progress", &mirror],
                                    &mut on_line,
                                )
                                .is_ok()
                            {
                                pulled_via_mirror = true;
                                break;
                            }
                        }
                        if !pulled_via_mirror {
                            return Err(format!("git pull failed: {}", primary_err));
                        }

                        // Verify integrity of code pulled from third-party mirror before building
                        let allow_unverified = std::env::var("LUATOOLS_ALLOW_UNVERIFIED_MIRROR").is_ok();
                        let verified = Self::git_cmd(&dir, &["verify-commit", "HEAD"])
                            .or_else(|_| Self::git_cmd(&dir, &["verify-tag", "HEAD"]))
                            .is_ok();
                        if !allow_unverified && !verified {
                            return Err(
                                "Security: Refusing to compile source code pulled from third-party mirror without commit/tag signature verification. Set LUATOOLS_ALLOW_UNVERIFIED_MIRROR=1 or use direct GitHub.".into()
                            );
                        }
                        if allow_unverified && !verified {
                            on_line("Warning: Using unverified third-party mirror source per LUATOOLS_ALLOW_UNVERIFIED_MIRROR opt-in.");
                        }
                    }
                }

                let head_info = Self::git_cmd(&dir, &["log", "-1", "--format=%h — %s"])
                    .unwrap_or_else(|_| "updated".into());
                on_line(&format!("HEAD now at {}", head_info));
                if override_repo_dir.trim().is_empty() {
                    let saved_rem = Self::git_cmd(&dir, &["remote", "get-url", "origin"]).ok();
                    Self::save_meta(
                        Some(&dir),
                        saved_rem.as_deref().or(explicit_remote.as_deref()),
                    );
                }
                return Ok((dir, head_info));
            }
        }

        // No local .git checkout yet — clone into ~/.local/share/luatools/repo (or override_repo_dir)
        let rem = explicit_remote
            .or_else(|| {
                (!meta.remote_url.trim().is_empty()).then(|| meta.remote_url.trim().to_string())
            })
            .unwrap_or_else(|| DEFAULT_GIT_REMOTE_URL.to_string());

        let target_dir = if !override_repo_dir.trim().is_empty() {
            expand_tilde(override_repo_dir.trim())
                .unwrap_or_else(|| PathBuf::from(override_repo_dir.trim()))
        } else {
            default_luatools_data_dir().join("repo")
        };

        if target_dir.exists() && !target_dir.join(".git").exists() {
            let _ = fs::remove_dir_all(&target_dir);
        }
        if let Some(parent) = target_dir.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }

        let mut last_clone_err = String::new();
        let mut cloned_url = None;
        for cand_url in Self::remote_failover_urls(&rem) {
            let _ = fs::remove_dir_all(&target_dir);
            on_line(&format!("$ git clone --depth 1 --progress {}", cand_url));
            let mut cmd = Command::new("git");
            cmd.args(["clone", "--depth", "1", "--progress", &cand_url])
                .arg(&target_dir)
                .env("GIT_TERMINAL_PROMPT", "0");
            match run_command_streaming(cmd, |line| on_line(line)) {
                Ok(_) => {
                    cloned_url = Some(cand_url);
                    break;
                }
                Err((err, _)) => {
                    last_clone_err = err;
                }
            }
        }
        let Some(used_url) = cloned_url else {
            return Err(format!(
                "git clone failed (all mirrors exhausted): {}",
                last_clone_err
            ));
        };

        let is_mirror = used_url != DEFAULT_GIT_REMOTE_URL
            && (used_url.contains("ghfast") || used_url.contains("proxy") || !used_url.starts_with("https://github.com/"));
        if is_mirror {
            let allow_unverified = std::env::var("LUATOOLS_ALLOW_UNVERIFIED_MIRROR").is_ok();
            let verified = Self::git_cmd(&target_dir, &["verify-commit", "HEAD"])
                .or_else(|_| Self::git_cmd(&target_dir, &["verify-tag", "HEAD"]))
                .is_ok();
            if !allow_unverified && !verified {
                let _ = fs::remove_dir_all(&target_dir);
                return Err(
                    "Security: Refusing to compile source code cloned from third-party mirror without commit/tag signature verification. Set LUATOOLS_ALLOW_UNVERIFIED_MIRROR=1 or use direct GitHub.".into()
                );
            }
            if allow_unverified && !verified {
                on_line("Warning: Using unverified third-party mirror source per LUATOOLS_ALLOW_UNVERIFIED_MIRROR opt-in.");
            }
        }

        let head_info = Self::git_cmd(&target_dir, &["log", "-1", "--format=%h — %s"])
            .unwrap_or_else(|_| "cloned".into());
        on_line(&format!("Cloned at {}", head_info));
        Self::save_meta(Some(&target_dir), Some(&used_url));
        Ok((target_dir, head_info))
    }

    /// Atomically replaces an executable on Linux via temp-file + `rename()` so a running
    /// binary never triggers `ETXTBSY` ("Text file busy").
    pub fn atomic_replace_executable(src_bin: &Path, dst_bin: &Path) -> Result<(), String> {
        let Some(parent) = dst_bin.parent() else {
            return Err("Invalid target executable path".into());
        };
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;

        // If src and dst resolve to the exact same file, nothing to copy
        if let (Ok(cs), Ok(cd)) = (fs::canonicalize(src_bin), fs::canonicalize(dst_bin)) {
            if cs == cd {
                return Ok(());
            }
        }

        let tmp = parent.join(format!(".luatools.update.{}.tmp", std::process::id()));
        fs::copy(src_bin, &tmp).map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                format!(
                    "Permission denied writing to {} (system install — run `sudo install -m 755 {} {}`)",
                    parent.display(),
                    src_bin.display(),
                    dst_bin.display()
                )
            } else {
                format!("Copy failed: {}", e)
            }
        })?;
        let mut perms = fs::metadata(&tmp).map_err(|e| e.to_string())?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&tmp, perms).map_err(|e| e.to_string())?;
        fs::rename(&tmp, dst_bin).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("Atomic rename onto {} failed: {}", dst_bin.display(), e)
        })?;
        Ok(())
    }

    /// Selects the highest-priority prebuilt Linux `x86_64` asset from a GitHub Release JSON
    /// payload (`assets[]`), preferring standalone `luatools` binaries over `.tar.gz`/`.tgz`/`.tar.xz`
    /// archives and skipping packages (`.deb`, `.rpm`, `.AppImage`), checksums/signatures, and
    /// non-Linux or non-`x86_64` assets (LT-50).
    pub fn select_prebuilt_linux_asset(release_json: &Value) -> Option<PrebuiltReleaseAsset> {
        const DISALLOWED_SUFFIXES: &[&str] = &[
            ".deb",
            ".rpm",
            ".appimage",
            ".flatpak",
            ".snap",
            ".exe",
            ".msi",
            ".dmg",
            ".pkg",
            ".zip",
            ".7z",
            ".rar",
            ".sha256",
            ".sha256sum",
            ".sha512",
            ".sha1",
            ".md5",
            ".sig",
            ".asc",
            ".minisig",
            ".sbom",
            ".json",
            ".txt",
            ".md",
            ".pem",
            ".pub",
            ".cert",
            ".so",
            ".dll",
            ".dylib",
            ".dwp",
            ".debug",
            ".sh",
            ".py",
            ".desktop",
            ".png",
            ".svg",
            ".ico",
            ".toml",
            ".yml",
            ".yaml",
            ".zst",
            ".bz2",
        ];

        const DISALLOWED_SUBSTRINGS: &[&str] = &[
            "checksum",
            "sha256sum",
            "shasum",
            "windows",
            "win32",
            "win64",
            "msvc",
            "mingw",
            "macos",
            "darwin",
            "apple",
            "osx",
            "freebsd",
            "openbsd",
            "netbsd",
            "android",
            "ios",
            "aarch64",
            "arm64",
            "armv7",
            "armv6",
            "armhf",
            "i686",
            "i386",
            "i586",
            "ia32",
            "32bit",
            "riscv64",
            "ppc64",
            "s390x",
            "mips",
            "libsteam_integration",
            "libsteam_auto_patch",
            "bridge_lib",
        ];

        let assets = release_json.get("assets")?.as_array()?;
        let mut best: Option<(u32, PrebuiltReleaseAsset)> = None;

        for asset in assets {
            let Some(name) = asset
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let Some(download_url) = asset
                .get("browser_download_url")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };

            let low = name.to_ascii_lowercase();
            if DISALLOWED_SUFFIXES.iter().any(|s| low.ends_with(s)) {
                continue;
            }
            if DISALLOWED_SUBSTRINGS.iter().any(|s| low.contains(s)) {
                continue;
            }

            let kind = if low.ends_with(".tar.gz") || low.ends_with(".tgz") {
                PrebuiltAssetKind::TarGz
            } else if low.ends_with(".tar.xz") || low.ends_with(".txz") {
                PrebuiltAssetKind::TarXz
            } else if low.ends_with(".tar") || low.ends_with(".gz") || low.ends_with(".xz") {
                continue;
            } else {
                PrebuiltAssetKind::RawBinary
            };

            let has_linux = low.contains("linux");
            let has_x86_64 =
                low.contains("x86_64") || low.contains("amd64") || low.contains("x64");
            let has_luatools = low.contains("luatools");

            let score = match kind {
                PrebuiltAssetKind::RawBinary => {
                    if low == "luatools" {
                        Some(100)
                    } else if has_luatools && has_linux && has_x86_64 {
                        Some(95)
                    } else if has_luatools && has_linux {
                        Some(90)
                    } else if has_luatools && has_x86_64 {
                        Some(88)
                    } else if has_linux && has_x86_64 {
                        Some(85)
                    } else {
                        None
                    }
                }
                PrebuiltAssetKind::TarGz => {
                    if has_linux && has_x86_64 {
                        Some(if has_luatools { 60 } else { 55 })
                    } else {
                        None
                    }
                }
                PrebuiltAssetKind::TarXz => {
                    if has_linux && has_x86_64 {
                        Some(if has_luatools { 50 } else { 45 })
                    } else {
                        None
                    }
                }
            };

            let Some(score) = score else {
                continue;
            };

            let digest = asset
                .get("digest")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string);
            let size = asset.get("size").and_then(|v| v.as_u64());

            let candidate = PrebuiltReleaseAsset {
                name: name.to_string(),
                download_url: download_url.to_string(),
                digest,
                size,
                kind,
            };

            if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
                best = Some((score, candidate));
            }
        }

        best.map(|(_, asset)| asset)
    }

    /// Validates that `bytes` begin with the Linux ELF magic (`\x7fELF`) and match
    /// `ELFCLASS64` (and `EM_X86_64` when `e_machine` is populated on `x86_64`).
    pub fn verify_elf_executable_bytes(bytes: &[u8]) -> Result<(), String> {
        if bytes.len() < 16 {
            return Err(format!(
                "Downloaded payload is too small to be an ELF binary ({} bytes)",
                bytes.len()
            ));
        }
        if &bytes[0..4] != b"\x7fELF" {
            return Err(
                "Downloaded payload is not a valid ELF executable (missing \\x7fELF header)"
                    .into(),
            );
        }
        #[cfg(target_arch = "x86_64")]
        {
            const ELFCLASS64: u8 = 2;
            const ELFDATA2LSB: u8 = 1;
            const EM_X86_64: u16 = 62;
            if bytes[4] != ELFCLASS64 {
                return Err(format!(
                    "Downloaded ELF binary is not 64-bit ELFCLASS64 (got class {})",
                    bytes[4]
                ));
            }
            if bytes.len() >= 20 && bytes[5] == ELFDATA2LSB {
                let e_machine = u16::from_le_bytes([bytes[18], bytes[19]]);
                if e_machine != 0 && e_machine != EM_X86_64 {
                    return Err(format!(
                        "Downloaded ELF binary architecture mismatch (expected x86_64 EM_X86_64=62, got {})",
                        e_machine
                    ));
                }
            }
        }
        Ok(())
    }

    fn verify_elf_executable_file(path: &Path) -> Result<(), String> {
        let mut f = fs::File::open(path)
            .map_err(|e| format!("Failed to open staged binary {}: {}", path.display(), e))?;
        let mut hdr = [0u8; 64];
        let n = f
            .read(&mut hdr)
            .map_err(|e| format!("Failed to read ELF header from {}: {}", path.display(), e))?;
        Self::verify_elf_executable_bytes(&hdr[..n])
    }

    /// Pre-screens `tar -tf` entry listings against path traversal (`..`) and leading `/` or `\`.
    pub fn validate_tar_entries(listing: &str) -> Result<(), String> {
        let mut count = 0usize;
        for raw_line in listing.lines() {
            let entry = raw_line.trim_end_matches('\r');
            if entry.is_empty() {
                continue;
            }
            count += 1;
            if entry.starts_with('/') || entry.starts_with('\\') {
                return Err(format!(
                    "Unsafe tarball entry with leading path separator: '{}'",
                    entry
                ));
            }
            if entry.contains('\\') {
                return Err(format!(
                    "Unsafe tarball entry containing backslash: '{}'",
                    entry
                ));
            }
            for comp in entry.split('/') {
                if comp == ".." {
                    return Err(format!(
                        "Unsafe tarball entry with path traversal ('..'): '{}'",
                        entry
                    ));
                }
            }
        }
        if count == 0 {
            return Err("Release archive contains no entries".into());
        }
        Ok(())
    }

    fn find_extracted_luatools_binary(extract_dir: &Path) -> Option<PathBuf> {
        let mut stack: Vec<(PathBuf, usize)> = vec![(extract_dir.to_path_buf(), 0)];
        let mut best: Option<(u32, PathBuf)> = None;

        while let Some((dir, depth)) = stack.pop() {
            if depth > 4 {
                continue;
            }
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(meta) = fs::symlink_metadata(&path) else {
                    continue;
                };
                if meta.file_type().is_symlink() {
                    continue;
                }
                if meta.is_dir() {
                    stack.push((path, depth + 1));
                    continue;
                }
                if !meta.is_file() {
                    continue;
                }
                let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                let low = fname.to_ascii_lowercase();
                if low.ends_with(".so") || low.starts_with("lib") {
                    continue;
                }
                if Self::verify_elf_executable_file(&path).is_err() {
                    continue;
                }
                let score = if fname == "luatools" {
                    100
                } else if low.starts_with("luatools") {
                    80
                } else {
                    40
                };
                if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
                    best = Some((score, path));
                }
            }
        }

        best.map(|(_, p)| p)
    }

    /// Pre-screens a `.tar.gz`/`.tar.xz` archive with `tar -tf` against path traversal and
    /// extracts the verified `luatools` ELF binary into `extract_dir`.
    fn extract_prebuilt_tarball(
        archive_path: &Path,
        extract_dir: &Path,
    ) -> Result<PathBuf, (bool, String)> {
        let list_out = Command::new("tar")
            .arg("-tf")
            .arg(archive_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| (false, format!("Failed to invoke tar -tf: {}", e)))?;

        if !list_out.status.success() {
            let err = String::from_utf8_lossy(&list_out.stderr).trim().to_string();
            return Err((
                false,
                format!("tar -tf failed on {}: {}", archive_path.display(), err),
            ));
        }

        let listing = String::from_utf8_lossy(&list_out.stdout);
        if let Err(unsafe_msg) = Self::validate_tar_entries(&listing) {
            return Err((
                true,
                format!("Refusing to extract unsafe release archive: {}", unsafe_msg),
            ));
        }

        fs::create_dir_all(extract_dir)
            .map_err(|e| (false, format!("Failed to create extract dir: {}", e)))?;

        let extract_out = Command::new("tar")
            .arg("-xf")
            .arg(archive_path)
            .arg("-C")
            .arg(extract_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| (false, format!("Failed to invoke tar -xf: {}", e)))?;

        if !extract_out.status.success() {
            let err = String::from_utf8_lossy(&extract_out.stderr)
                .trim()
                .to_string();
            return Err((
                false,
                format!("tar -xf failed on {}: {}", archive_path.display(), err),
            ));
        }

        Self::find_extracted_luatools_binary(extract_dir).ok_or_else(|| {
            (
                false,
                format!(
                    "Archive {} did not contain a valid Linux x86_64 ELF binary",
                    archive_path.display()
                ),
            )
        })
    }

    /// Verifies optional SHA-256 digest and ELF64 header on raw binary `bytes`, sets `0o755`,
    /// and atomically installs onto `target_bin` (immune to Linux `ETXTBSY`).
    pub fn install_prebuilt_binary_bytes(
        bytes: &[u8],
        asset_name: &str,
        expected_sha256_digest: Option<&str>,
        target_bin: &Path,
    ) -> Result<(), String> {
        let Some(parent) = target_bin.parent() else {
            return Err("Invalid target executable path".into());
        };
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;

        let seq = PREBUILT_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = parent.join(format!(
            ".luatools.prebuilt.{}.{}.tmp",
            std::process::id(),
            seq
        ));

        if let Err(e) = fs::write(&tmp, bytes) {
            let _ = fs::remove_file(&tmp);
            return Err(if e.kind() == std::io::ErrorKind::PermissionDenied {
                format!(
                    "Permission denied writing to {} (system install — install to ~/.local/bin/luatools or run with sudo)",
                    parent.display()
                )
            } else {
                format!("Failed to stage prebuilt binary at {}: {}", tmp.display(), e)
            });
        }

        if let Some(digest) = expected_sha256_digest
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let hash_svc = FileHashService::new();
            if let Err(err) = hash_svc.verify_sha256_digest(&tmp, asset_name, digest) {
                let _ = fs::remove_file(&tmp);
                return Err(err);
            }
        }

        if let Err(err) = Self::verify_elf_executable_bytes(bytes) {
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }

        let mut perms = fs::metadata(&tmp)
            .map_err(|e| {
                let _ = fs::remove_file(&tmp);
                e.to_string()
            })?
            .permissions();
        perms.set_mode(0o755);
        if let Err(e) = fs::set_permissions(&tmp, perms) {
            let _ = fs::remove_file(&tmp);
            return Err(format!("Failed to chmod 0o755 staged binary: {}", e));
        }

        fs::rename(&tmp, target_bin).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("Atomic rename onto {} failed: {}", target_bin.display(), e)
        })?;

        Ok(())
    }

    #[cfg(unix)]
    fn atomic_symlink(target: &str, link_path: &Path) -> Result<(), String> {
        if let Some(parent) = link_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
        }
        if let Ok(meta) = fs::symlink_metadata(link_path) {
            if meta.file_type().is_dir() {
                fs::remove_dir_all(link_path).map_err(|e| {
                    format!(
                        "Failed to remove existing directory {}: {}",
                        link_path.display(),
                        e
                    )
                })?;
            }
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp_name = format!(
            ".{}.tmp.{}.{}",
            link_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("link"),
            std::process::id(),
            stamp
        );
        let tmp_link = link_path.with_file_name(tmp_name);
        let _ = fs::remove_file(&tmp_link);
        std::os::unix::fs::symlink(target, &tmp_link)
            .map_err(|e| format!("Failed to create symlink {}: {}", tmp_link.display(), e))?;
        fs::rename(&tmp_link, link_path).map_err(|e| {
            let _ = fs::remove_file(&tmp_link);
            format!(
                "Failed to atomically install symlink {} -> {}: {}",
                link_path.display(),
                target,
                e
            )
        })?;
        Ok(())
    }

    fn sync_bridge_lib_layout(
        base_dir: &Path,
        bridge_32: Option<&Path>,
        bridge_64: Option<&Path>,
    ) -> Vec<PathBuf> {
        let mut installed = Vec::new();
        if let Some(b32) = bridge_32 {
            for rel in [
                "libsteam_integration.so",
                "libsteam_integration32.so",
                "32/libsteam_integration.so",
                "lib/libsteam_integration.so",
            ] {
                let dst = base_dir.join(rel);
                if Self::atomic_replace_executable(b32, &dst).is_ok() {
                    installed.push(dst);
                }
            }
            #[cfg(unix)]
            {
                let _ = Self::atomic_symlink("lib", &base_dir.join("lib32"));
                let _ = Self::atomic_symlink("lib", &base_dir.join("i386-linux-gnu"));
                let _ = Self::atomic_symlink("../32", &base_dir.join("lib/i386-linux-gnu"));
            }
        } else if let Some(b64) = bridge_64 {
            let dst = base_dir.join("libsteam_integration.so");
            if Self::atomic_replace_executable(b64, &dst).is_ok() {
                installed.push(dst);
            }
        }

        if let Some(b64) = bridge_64 {
            for rel in [
                "libsteam_integration64.so",
                "64/libsteam_integration.so",
                "lib64/libsteam_integration.so",
            ] {
                let dst = base_dir.join(rel);
                if Self::atomic_replace_executable(b64, &dst).is_ok() {
                    installed.push(dst);
                }
            }
            #[cfg(unix)]
            {
                let _ = Self::atomic_symlink("lib64", &base_dir.join("x86_64-linux-gnu"));
                let _ = Self::atomic_symlink("../lib64", &base_dir.join("lib/x86_64-linux-gnu"));
            }
        }
        installed
    }

    fn find_git_bin() -> Option<PathBuf> {
        let path_var = std::env::var_os("PATH")?;
        std::env::split_paths(&path_var)
            .map(|d| d.join("git"))
            .find(|p| p.is_file())
    }

    fn check_source_build_toolchain_with(has_git: bool, has_cargo: bool) -> Result<(), String> {
        match (has_git, has_cargo) {
            (true, true) => Ok(()),
            (false, false) => Err(
                "Source-build fallback unavailable: neither `git` nor `cargo` was found in PATH (or ~/.cargo/bin). Install Git and the Rust toolchain (https://rustup.rs), or download a prebuilt LuaTools release binary from GitHub Releases."
                    .into(),
            ),
            (false, true) => Err(
                "Source-build fallback unavailable: `git` was not found in PATH. Install Git (`sudo apt install git` / `sudo dnf install git` / `sudo pacman -S git`), or download a prebuilt LuaTools release binary from GitHub Releases."
                    .into(),
            ),
            (true, false) => Err(
                "Source-build fallback unavailable: `cargo` (Rust toolchain) was not found in PATH or ~/.cargo/bin. Install Rust via https://rustup.rs, or download a prebuilt LuaTools release binary from GitHub Releases."
                    .into(),
            ),
        }
    }

    fn ensure_source_build_toolchain() -> Result<(), String> {
        Self::check_source_build_toolchain_with(
            Self::find_git_bin().is_some(),
            Self::find_cargo_bin().is_some(),
        )
    }

    /// Attempts to update LuaTools directly from the latest GitHub Release prebuilt Linux
    /// `x86_64` binary asset without requiring `git` or `cargo`.
    ///
    /// Returns:
    /// - `Ok(Some(UpdateApplyResult))` when a prebuilt release binary installed cleanly.
    /// - `Ok(None)` when no matching prebuilt asset is available or downloadable (caller falls
    ///   back to Git + Cargo source build).
    /// - `Err(String)` on a hard integrity/security error (SHA-256 mismatch, unsafe tarball path
    ///   traversal) or when no target install location is writable.
    pub fn try_install_prebuilt_release_stream<F>(
        override_repo_dir: &str,
        override_remote_url: &str,
        mut progress_cb: F,
    ) -> Result<Option<UpdateApplyResult>, String>
    where
        F: FnMut(UpdateProgressEvent),
    {
        let emit = |cb: &mut F, frac: f32, stage: &str, asset: &str, line: String| {
            cb(UpdateProgressEvent {
                fraction: frac,
                stage_label: stage.to_string(),
                current_crate: asset.to_string(),
                compiled_crates: 0,
                total_crates: 0,
                log_line: Some(line),
            });
        };

        let effective_remote =
            Self::resolve_effective_remote_url(override_repo_dir, override_remote_url);
        let Some((owner, repo)) = Self::parse_github_owner_repo(&effective_remote) else {
            emit(
                &mut progress_cb,
                0.05,
                "Checking prebuilt release",
                "",
                format!(
                    "No prebuilt Linux binary available (remote '{}' is not a GitHub repository); falling back to Git + Cargo source build...",
                    effective_remote
                ),
            );
            return Ok(None);
        };

        emit(
            &mut progress_cb,
            0.05,
            "Checking latest GitHub Release",
            "",
            format!("Checking GitHub Releases for {}/{}…", owner, repo),
        );

        let api_url = format!(
            "https://api.github.com/repos/{}/{}/releases/latest",
            owner, repo
        );
        let meta_downloader = ProxiedFileDownloader::new().with_timeout(Duration::from_secs(20));
        let release_text = match meta_downloader.download_string_sync(&api_url) {
            Ok(txt) => txt,
            Err(err) => {
                emit(
                    &mut progress_cb,
                    0.08,
                    "Checking latest GitHub Release",
                    "",
                    format!(
                        "No prebuilt Linux binary available (release lookup failed: {}); falling back to Git + Cargo source build...",
                        err
                    ),
                );
                return Ok(None);
            }
        };

        let release_json: Value = match serde_json::from_str(&release_text) {
            Ok(val) => val,
            Err(err) => {
                emit(
                    &mut progress_cb,
                    0.08,
                    "Checking latest GitHub Release",
                    "",
                    format!(
                        "No prebuilt Linux binary available (invalid release JSON: {}); falling back to Git + Cargo source build...",
                        err
                    ),
                );
                return Ok(None);
            }
        };

        let release_tag = release_json
            .get("tag_name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(VERSION);
        let new_version = release_tag.trim_start_matches('v').to_string();

        if !Self::is_newer_semver(&new_version, VERSION) {
            emit(
                &mut progress_cb,
                0.10,
                "Checking latest GitHub Release",
                "",
                format!(
                    "No newer prebuilt Linux release available (latest GitHub Release v{} is not newer than running v{}); falling back to Git + Cargo source build...",
                    new_version, VERSION
                ),
            );
            return Ok(None);
        }

        let Some(asset) = Self::select_prebuilt_linux_asset(&release_json) else {
            emit(
                &mut progress_cb,
                0.10,
                "Checking latest GitHub Release",
                "",
                format!(
                    "No prebuilt Linux binary available (release {} has no Linux x86_64 binary/tarball asset); falling back to Git + Cargo source build...",
                    release_tag
                ),
            );
            return Ok(None);
        };

        emit(
            &mut progress_cb,
            0.14,
            &format!("Downloading prebuilt {}", release_tag),
            &asset.name,
            format!(
                "Downloading prebuilt Linux asset {} ({})…",
                asset.name, release_tag
            ),
        );

        struct StagingDir(PathBuf);
        impl StagingDir {
            fn path(&self) -> &Path {
                &self.0
            }
        }
        impl Drop for StagingDir {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let staging_path = std::env::temp_dir().join(format!(
            "luatools-update-{}-{}",
            std::process::id(),
            stamp
        ));
        fs::create_dir_all(&staging_path)
            .map_err(|e| format!("Failed to create prebuilt staging directory: {}", e))?;
        let staging_dir = StagingDir(staging_path);
        let downloaded_path = staging_dir.path().join(&asset.name);

        let bin_downloader = ProxiedFileDownloader::new().with_timeout(Duration::from_secs(120));
        let mut last_pct: i32 = -1;
        let dl_stage = format!("Downloading {}", asset.name);
        let dl_asset_name = asset.name.clone();
        let mut dl_cb = |p: f64| {
            let pct = (p.clamp(0.0, 1.0) * 100.0).round() as i32;
            if pct >= last_pct + 4 || pct == 100 || last_pct < 0 {
                last_pct = pct;
                let frac = 0.15 + (p.clamp(0.0, 1.0) as f32) * 0.57;
                progress_cb(UpdateProgressEvent {
                    fraction: frac,
                    stage_label: dl_stage.clone(),
                    current_crate: dl_asset_name.clone(),
                    compiled_crates: 0,
                    total_crates: 0,
                    log_line: (pct > 0 && pct % 20 == 0)
                        .then(|| format!("Downloading {}: {}%", dl_asset_name, pct)),
                });
            }
        };

        if let Err(err) =
            bin_downloader.download_file_sync(&asset.download_url, &downloaded_path, Some(&mut dl_cb))
        {
            emit(
                &mut progress_cb,
                0.15,
                "Downloading prebuilt binary",
                &asset.name,
                format!(
                    "No prebuilt Linux binary available (download of {} failed: {}); falling back to Git + Cargo source build...",
                    asset.name, err
                ),
            );
            return Ok(None);
        }

        if let Some(ref digest) = asset.digest {
            emit(
                &mut progress_cb,
                0.75,
                "Verifying SHA-256 digest",
                &asset.name,
                format!("Verifying SHA-256 digest for {}…", asset.name),
            );
            let hash_svc = FileHashService::new();
            let verified = hash_svc
                .verify_sha256_digest(&downloaded_path, &asset.name, digest)
                .map_err(|e| format!("Prebuilt release SHA-256 verification failed: {}", e))?;
            let short_hash = &verified.sha256[..verified.sha256.len().min(16)];
            emit(
                &mut progress_cb,
                0.78,
                "Verifying SHA-256 digest",
                &asset.name,
                format!("SHA-256 digest verified ({}…)", short_hash),
            );
        } else {
            emit(
                &mut progress_cb,
                0.76,
                "Verifying prebuilt asset",
                &asset.name,
                format!(
                    "Release asset {} has no SHA-256 digest metadata; verifying ELF64 executable header…",
                    asset.name
                ),
            );
        }

        let staged_bin = match asset.kind {
            PrebuiltAssetKind::RawBinary => downloaded_path.clone(),
            PrebuiltAssetKind::TarGz | PrebuiltAssetKind::TarXz => {
                emit(
                    &mut progress_cb,
                    0.80,
                    "Extracting prebuilt archive",
                    &asset.name,
                    format!("Pre-screening archive {} with tar -tf…", asset.name),
                );
                let extract_dir = staging_dir.path().join("extracted");
                match Self::extract_prebuilt_tarball(&downloaded_path, &extract_dir) {
                    Ok(bin_path) => {
                        emit(
                            &mut progress_cb,
                            0.85,
                            "Extracting prebuilt archive",
                            &asset.name,
                            format!("Extracted verified binary {}", bin_path.display()),
                        );
                        bin_path
                    }
                    Err((true, hard_err)) => return Err(hard_err),
                    Err((false, soft_err)) => {
                        emit(
                            &mut progress_cb,
                            0.82,
                            "Extracting prebuilt archive",
                            &asset.name,
                            format!(
                                "No prebuilt Linux binary available ({}); falling back to Git + Cargo source build...",
                                soft_err
                            ),
                        );
                        return Ok(None);
                    }
                }
            }
        };

        emit(
            &mut progress_cb,
            0.88,
            "Verifying ELF executable",
            "luatools",
            "Verifying Linux ELF64 executable header…".into(),
        );
        if let Err(elf_err) = Self::verify_elf_executable_file(&staged_bin) {
            if asset.digest.is_some() {
                return Err(format!("Prebuilt release ELF check failed: {}", elf_err));
            }
            emit(
                &mut progress_cb,
                0.88,
                "Verifying ELF executable",
                &asset.name,
                format!(
                    "No prebuilt Linux binary available ({}); falling back to Git + Cargo source build...",
                    elf_err
                ),
            );
            return Ok(None);
        }

        let mut perms = fs::metadata(&staged_bin)
            .map_err(|e| format!("Failed to stat staged binary: {}", e))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&staged_bin, perms)
            .map_err(|e| format!("Failed to chmod 0o755 staged binary: {}", e))?;

        emit(
            &mut progress_cb,
            0.91,
            "Installing prebuilt binary",
            "luatools",
            "Atomically installing verified prebuilt binary…".into(),
        );

        let mut primary_target: Option<PathBuf> = None;
        let mut permission_note: Option<String> = None;
        let mut updated_curr_exe = false;

        if let Ok(curr_exe) = std::env::current_exe() {
            if Self::is_path_writable(&curr_exe) {
                if Self::atomic_replace_executable(&staged_bin, &curr_exe).is_ok() {
                    emit(
                        &mut progress_cb,
                        0.93,
                        "Installing prebuilt binary",
                        "luatools",
                        format!("Atomically replaced {}", curr_exe.display()),
                    );
                    primary_target = Some(curr_exe);
                    updated_curr_exe = true;
                }
            } else {
                permission_note = Some(format!(
                    "system binary {} is read-only; installed user copy to ~/.local/bin/luatools",
                    curr_exe.display()
                ));
            }
        }

        if let Some(home) = dirs::home_dir() {
            let local_bin = home.join(".local/bin/luatools");
            if local_bin.exists() || !updated_curr_exe {
                match Self::atomic_replace_executable(&staged_bin, &local_bin) {
                    Ok(()) => {
                        emit(
                            &mut progress_cb,
                            0.95,
                            "Installing prebuilt binary",
                            "luatools",
                            format!("Installed {}", local_bin.display()),
                        );
                        if primary_target.is_none() {
                            primary_target = Some(local_bin);
                        }
                    }
                    Err(err) if primary_target.is_none() => {
                        return Err(format!(
                            "Cannot write prebuilt binary to running executable path or {}: {}",
                            local_bin.display(),
                            err
                        ));
                    }
                    Err(_) => {}
                }
            }
        }

        let Some(updated_exe) = primary_target else {
            return Err(
                "Cannot write prebuilt binary: neither the running executable path nor ~/.local/bin/luatools is writable."
                    .into(),
            );
        };

        let base_info = format!("prebuilt release {} ({})", release_tag, asset.name);
        let final_commit_info = match permission_note {
            Some(note) => format!("{} ({})", base_info, note),
            None => base_info,
        };

        progress_cb(UpdateProgressEvent {
            fraction: 1.0,
            stage_label: "Done updating via prebuilt release binary — restart to apply".into(),
            current_crate: String::new(),
            compiled_crates: 0,
            total_crates: 0,
            log_line: Some(format!(
                "Done updating via prebuilt release binary — restart to apply (v{}, {})",
                new_version, final_commit_info
            )),
        });

        Ok(Some(UpdateApplyResult {
            new_version,
            commit_info: final_commit_info,
            updated_exe,
        }))
    }

    fn find_cargo_bin() -> Option<PathBuf> {
        if let Some(home) = dirs::home_dir() {
            let rustup_cargo = home.join(".cargo/bin/cargo");
            if rustup_cargo.is_file() {
                return Some(rustup_cargo);
            }
        }
        let path_var = std::env::var_os("PATH")?;
        std::env::split_paths(&path_var)
            .map(|d| d.join("cargo"))
            .find(|p| p.is_file())
    }

    /// Estimates total dependency/workspace crates to compile cleanly before invoking Cargo.
    /// Counts `[[package]]` blocks in `Cargo.lock`, falling back to `cargo metadata`, then `120`.
    pub fn estimate_total_crates(crate_dir: &Path, cargo_bin: Option<&Path>) -> usize {
        let lock_path = crate_dir.join("Cargo.lock");
        if let Ok(lock_txt) = fs::read_to_string(&lock_path) {
            let count = lock_txt
                .lines()
                .filter(|line| line.trim() == "[[package]]")
                .count();
            if count > 0 {
                return count;
            }
        }

        if let Some(cargo) = cargo_bin {
            if let Ok(out) = Command::new(cargo)
                .args(["metadata", "--format-version=1"])
                .current_dir(crate_dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
            {
                if out.status.success() {
                    if let Ok(val) = serde_json::from_slice::<Value>(&out.stdout) {
                        if let Some(n) = val.get("packages").and_then(|p| p.as_array()).map(|a| a.len()) {
                            if n > 0 {
                                return n;
                            }
                        }
                    }
                }
            }
        }

        120
    }

    /// Full one-click update workflow (legacy `(f32, String)` callback wrapper).
    pub fn pull_and_update<F>(
        override_repo_dir: &str,
        override_remote_url: &str,
        mut progress_cb: F,
    ) -> Result<UpdateApplyResult, String>
    where
        F: FnMut(f32, String),
    {
        Self::pull_and_update_stream(override_repo_dir, override_remote_url, |ev| {
            let label = if ev.total_crates > 0 && ev.compiled_crates > 0 {
                if ev.current_crate.is_empty() {
                    format!(
                        "{}: {}/{} crates",
                        ev.stage_label, ev.compiled_crates, ev.total_crates
                    )
                } else {
                    format!(
                        "{}: {}/{} crates — {}",
                        ev.stage_label, ev.compiled_crates, ev.total_crates, ev.current_crate
                    )
                }
            } else if let Some(line) = ev.log_line {
                format!("{}: {}", ev.stage_label, line)
            } else {
                ev.stage_label
            };
            progress_cb(ev.fraction, label);
        })
    }

    /// Full one-click update workflow: tries prebuilt GitHub Release Linux binary first,
    /// falling back to live-streaming `git`, `cargo`, and atomic install if unavailable.
    pub fn pull_and_update_stream<F>(
        override_repo_dir: &str,
        override_remote_url: &str,
        mut progress_cb: F,
    ) -> Result<UpdateApplyResult, String>
    where
        F: FnMut(UpdateProgressEvent),
    {
        if let Some(prebuilt_result) = Self::try_install_prebuilt_release_stream(
            override_repo_dir,
            override_remote_url,
            &mut progress_cb,
        )? {
            return Ok(prebuilt_result);
        }

        // Source-build fallback requires git + cargo; return an actionable error if missing.
        Self::ensure_source_build_toolchain()?;

        let mut git_frac: f32 = 0.02;
        progress_cb(UpdateProgressEvent {
            fraction: git_frac,
            stage_label: "1/3 Pulling latest files from Git".into(),
            current_crate: String::new(),
            compiled_crates: 0,
            total_crates: 0,
            log_line: Some("Step 1/3: Syncing source repository with Git remote…".into()),
        });

        let (repo_dir, commit_info) =
            Self::pull_git_changes_streaming(override_repo_dir, override_remote_url, |line| {
                git_frac = (git_frac + 0.015).min(0.14);
                progress_cb(UpdateProgressEvent {
                    fraction: git_frac,
                    stage_label: "1/3 Pulling latest files from Git".into(),
                    current_crate: String::new(),
                    compiled_crates: 0,
                    total_crates: 0,
                    log_line: Some(line.to_string()),
                });
            })?;

        let crate_dir = Self::resolve_crate_dir(&repo_dir).ok_or_else(|| {
            format!(
                "Pulled Git repository at {}, but Cargo.toml was not found.",
                repo_dir.display()
            )
        })?;

        let new_version = fs::read_to_string(crate_dir.join("Cargo.toml"))
            .ok()
            .and_then(|t| Self::parse_cargo_version(&t))
            .unwrap_or_else(|| VERSION.to_string());

        let built_bin = if let Some(cargo) = Self::find_cargo_bin() {
            let total_est = Self::estimate_total_crates(&crate_dir, Some(&cargo));
            let mut tracker = CargoProgressTracker::new(total_est, 0.15, 0.88);
            progress_cb(UpdateProgressEvent {
                fraction: 0.15,
                stage_label: format!("2/3 Compiling Rust (v{})", new_version),
                current_crate: String::new(),
                compiled_crates: 0,
                total_crates: total_est,
                log_line: Some(format!(
                    "$ {} build --workspace --release -j 2 --color=never (est. {} crates)",
                    cargo.display(),
                    total_est
                )),
            });

            let mut build_cmd = Command::new(&cargo);
            build_cmd
                .args([
                    "build",
                    "--workspace",
                    "--release",
                    "-j",
                    "2",
                    "--color=never",
                ])
                .current_dir(&crate_dir);

            if let Err((err_msg, _)) = run_command_streaming(build_cmd, |line| {
                let ev = tracker.feed_line(line);
                progress_cb(ev);
            }) {
                return Err(format!("Cargo build failed: {}", err_msg));
            }

            // Prefer 32-bit i686 build for bridge_lib for native 32-bit Steam LD_PRELOAD
            let mut bridge_i686_cmd = Command::new(&cargo);
            bridge_i686_cmd
                .args([
                    "build",
                    "--manifest-path",
                    "bridge_lib/Cargo.toml",
                    "--target",
                    "i686-unknown-linux-gnu",
                    "--release",
                    "-j",
                    "2",
                    "--color=never",
                ])
                .current_dir(&crate_dir);
            let _ = run_command_streaming(bridge_i686_cmd, |_| {});

            // Also build debug-debug symbols for bridge_lib as fallback
            let mut bridge_tracker = CargoProgressTracker::new(6, 0.88, 0.92);
            bridge_tracker.stage_label = "Building bridge_lib symbols".into();
            progress_cb(UpdateProgressEvent {
                fraction: 0.88,
                stage_label: "Building bridge_lib symbols".into(),
                current_crate: "bridge_lib".into(),
                compiled_crates: tracker.compiled_crates,
                total_crates: tracker.total_crates,
                log_line: Some(
                    "$ cargo build --profile debug-debug -p bridge_lib -j 2 --color=never".into(),
                ),
            });
            let mut bridge_cmd = Command::new(&cargo);
            bridge_cmd
                .args([
                    "build",
                    "--profile",
                    "debug-debug",
                    "-p",
                    "bridge_lib",
                    "-j",
                    "2",
                    "--color=never",
                ])
                .current_dir(&crate_dir);
            let _ = run_command_streaming(bridge_cmd, |line| {
                let mut ev = bridge_tracker.feed_line(line);
                ev.compiled_crates = tracker.compiled_crates;
                ev.total_crates = tracker.total_crates;
                progress_cb(ev);
            });

            let bridge_32_path =
                crate_dir.join("target/i686-unknown-linux-gnu/release/libsteam_integration.so");
            let bridge_64_path = {
                let rel_so = crate_dir.join("target/release/libsteam_integration.so");
                let dbg_so = crate_dir.join("target/debug-debug/libsteam_integration.so");
                if rel_so.is_file() {
                    Some(rel_so)
                } else if dbg_so.is_file() {
                    Some(dbg_so)
                } else {
                    None
                }
            };
            let bridge_32 = bridge_32_path.is_file().then_some(bridge_32_path.as_path());
            let bridge_64 = bridge_64_path.as_deref();

            if bridge_32.is_some() || bridge_64.is_some() {
                let dist_dir = crate_dir.join("dist");
                let synced_dist = Self::sync_bridge_lib_layout(&dist_dir, bridge_32, bridge_64);
                if !synced_dist.is_empty() {
                    progress_cb(UpdateProgressEvent {
                        fraction: 0.93,
                        stage_label: "3/3 Installing binaries".into(),
                        current_crate: "libsteam_integration.so".into(),
                        compiled_crates: tracker.compiled_crates,
                        total_crates: tracker.total_crates,
                        log_line: Some(format!(
                            "Synced {} multilib bridge file(s) in {}",
                            synced_dist.len(),
                            dist_dir.display()
                        )),
                    });
                }
                if let Some(home) = dirs::home_dir() {
                    let lib_dir = home.join(".local/lib/luatools");
                    let synced_local = Self::sync_bridge_lib_layout(&lib_dir, bridge_32, bridge_64);
                    if !synced_local.is_empty() {
                        progress_cb(UpdateProgressEvent {
                            fraction: 0.94,
                            stage_label: "3/3 Installing binaries".into(),
                            current_crate: "libsteam_integration.so".into(),
                            compiled_crates: tracker.compiled_crates,
                            total_crates: tracker.total_crates,
                            log_line: Some(format!(
                                "Synced {} multilib bridge file(s) in {}",
                                synced_local.len(),
                                lib_dir.display()
                            )),
                        });
                    }
                }
            }
            let built_patch_so = crate_dir.join("target/release/libsteam_auto_patch_linux.so");
            if built_patch_so.is_file() {
                let dist_patch = crate_dir.join("dist/libsteam_auto_patch_linux.so");
                let _ = Self::atomic_replace_executable(&built_patch_so, &dist_patch);
                if let Some(home) = dirs::home_dir() {
                    let lib_dst = home.join(".local/lib/luatools/libsteam_auto_patch_linux.so");
                    let _ = Self::atomic_replace_executable(&built_patch_so, &lib_dst);
                }
                progress_cb(UpdateProgressEvent {
                    fraction: 0.95,
                    stage_label: "3/3 Installing binaries".into(),
                    current_crate: "libsteam_auto_patch_linux.so".into(),
                    compiled_crates: tracker.compiled_crates,
                    total_crates: tracker.total_crates,
                    log_line: Some(format!("Installed {}", dist_patch.display())),
                });
            }
            crate_dir.join("target/release/luatools")
        } else {
            let prebuilt = crate_dir.join("dist/luatools");
            if prebuilt.is_file() {
                progress_cb(UpdateProgressEvent {
                    fraction: 0.92,
                    stage_label: "Using prebuilt binary".into(),
                    current_crate: String::new(),
                    compiled_crates: 0,
                    total_crates: 0,
                    log_line: Some(format!("Using prebuilt binary at {}", prebuilt.display())),
                });
                prebuilt
            } else {
                return Err(
                    "Cargo is not installed and no prebuilt dist/luatools was found in the repository."
                        .into(),
                );
            }
        };

        if !built_bin.is_file() {
            return Err(format!(
                "Expected binary not found at {}",
                built_bin.display()
            ));
        }

        progress_cb(UpdateProgressEvent {
            fraction: 0.96,
            stage_label: "3/3 Installing updated executable".into(),
            current_crate: "luatools".into(),
            compiled_crates: 0,
            total_crates: 0,
            log_line: Some(format!(
                "Step 3/3: Atomically installing {}…",
                built_bin.display()
            )),
        });

        // 1. Update <crate_dir>/dist/luatools
        let dist_bin = crate_dir.join("dist/luatools");
        if Self::atomic_replace_executable(&built_bin, &dist_bin).is_ok() {
            progress_cb(UpdateProgressEvent {
                fraction: 0.97,
                stage_label: "3/3 Installing updated executable".into(),
                current_crate: "luatools".into(),
                compiled_crates: 0,
                total_crates: 0,
                log_line: Some(format!("Updated {}", dist_bin.display())),
            });
        }

        // 2. Check write permissions on the running binary before replacing (LT-50: graceful /usr/local/bin fallback)
        let mut primary_target = dist_bin;
        let mut permission_note: Option<String> = None;
        let mut updated_curr_exe = false;

        if let Ok(curr_exe) = std::env::current_exe() {
            if Self::is_path_writable(&curr_exe) {
                if Self::atomic_replace_executable(&built_bin, &curr_exe).is_ok() {
                    progress_cb(UpdateProgressEvent {
                        fraction: 0.985,
                        stage_label: "3/3 Installing updated executable".into(),
                        current_crate: "luatools".into(),
                        compiled_crates: 0,
                        total_crates: 0,
                        log_line: Some(format!("Atomically replaced {}", curr_exe.display())),
                    });
                    primary_target = curr_exe;
                    updated_curr_exe = true;
                }
            } else {
                permission_note = Some(format!(
                    "system binary {} is read-only; installed user copy to ~/.local/bin/luatools",
                    curr_exe.display()
                ));
            }
        }

        // 3. Install/update ~/.local/bin/luatools (also acts as fallback when /usr/local/bin is read-only)
        if let Some(home) = dirs::home_dir() {
            let local_bin = home.join(".local/bin/luatools");
            if local_bin.exists() || !updated_curr_exe {
                if Self::atomic_replace_executable(&built_bin, &local_bin).is_ok() {
                    progress_cb(UpdateProgressEvent {
                        fraction: 0.995,
                        stage_label: "3/3 Installing updated executable".into(),
                        current_crate: "luatools".into(),
                        compiled_crates: 0,
                        total_crates: 0,
                        log_line: Some(format!("Installed {}", local_bin.display())),
                    });
                    if !updated_curr_exe {
                        primary_target = local_bin;
                    }
                } else if !updated_curr_exe && !primary_target.is_file() {
                    return Err(format!(
                        "Cannot write updated binary to {} or {}. Run manually: sudo install -m 755 {} /usr/local/bin/luatools",
                        primary_target.display(),
                        local_bin.display(),
                        built_bin.display()
                    ));
                }
            }
        }

        let final_commit_info = match permission_note {
            Some(note) => format!("{} ({})", commit_info, note),
            None => commit_info,
        };

        progress_cb(UpdateProgressEvent {
            fraction: 1.0,
            stage_label: format!("Done updating to v{}", new_version),
            current_crate: String::new(),
            compiled_crates: 0,
            total_crates: 0,
            log_line: Some(format!(
                "Done updating to v{} ({}) — restart to apply!",
                new_version, final_commit_info
            )),
        });

        Ok(UpdateApplyResult {
            new_version,
            commit_info: final_commit_info,
            updated_exe: primary_target,
        })
    }

    pub fn relaunch_binary(exe_path: &Path) -> Result<(), String> {
        Command::new(exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Failed to relaunch {}: {}", exe_path.display(), e))
    }
}

fn compare_prerelease(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.split('.');
    let mut bi = b.split('.');
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(ap), Some(bp)) => {
                let ord = match (ap.parse::<u64>(), bp.parse::<u64>()) {
                    (Ok(an), Ok(bn)) => an.cmp(&bn),
                    (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                    (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                    (Err(_), Err(_)) => ap.cmp(bp),
                };
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git command");
        assert!(st.success(), "git {:?} failed in {}", args, dir.display());
    }

    #[test]
    fn detects_remote_git_commits_pulls_cleanly_and_replaces_binary_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let upstream = tmp.path().join("upstream");
        let work = tmp.path().join("work");
        fs::create_dir_all(&upstream).unwrap();

        // Initialize upstream repo with v2.0.0
        run_git(&upstream, &["init", "-b", "main"]);
        run_git(&upstream, &["config", "user.email", "test@lua.tools"]);
        run_git(&upstream, &["config", "user.name", "LuaTools Test"]);
        fs::write(
            upstream.join("Cargo.toml"),
            "[package]\nname = \"luatools-linux\"\nversion = \"2.0.0\"\n",
        )
        .unwrap();
        run_git(&upstream, &["add", "Cargo.toml"]);
        run_git(&upstream, &["commit", "-m", "Initial v2.0.0"]);

        // Clone into working directory
        let clone_st = Command::new("git")
            .args(["clone", upstream.to_str().unwrap(), work.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(clone_st.success());

        // Check when up-to-date -> available must be false
        let chk0 = UpdateService::check_once(work.to_str().unwrap(), "");
        assert!(!chk0.available);
        assert_eq!(chk0.commits_behind, 0);

        // Push a new commit to upstream bumping Cargo.toml to 2.1.0
        fs::write(
            upstream.join("Cargo.toml"),
            "[package]\nname = \"luatools-linux\"\nversion = \"2.1.0\"\n",
        )
        .unwrap();
        fs::write(upstream.join("CHANGELOG_UPDATE.txt"), "Fast update!\n").unwrap();
        run_git(&upstream, &["add", "."]);
        run_git(&upstream, &["commit", "-m", "Release v2.1.0"]);

        // Single background check detects 1 commit behind & remote version 2.1.0
        let chk1 = UpdateService::check_once(work.to_str().unwrap(), "");
        assert!(chk1.available);
        assert_eq!(chk1.commits_behind, 1);
        assert_eq!(chk1.remote_version.as_deref(), Some("2.1.0"));
        assert!(chk1
            .latest_commit
            .as_deref()
            .unwrap_or("")
            .contains("Release v2.1.0"));

        // Pull latest changes from git and verify working tree updated
        let (pulled_dir, commit_note) =
            UpdateService::pull_git_changes(work.to_str().unwrap(), "").unwrap();
        assert_eq!(pulled_dir, work);
        assert!(commit_note.contains("Release v2.1.0"));
        assert!(work.join("CHANGELOG_UPDATE.txt").exists());

        // Re-check -> now up to date again when running binary matches repo
        let chk2 = UpdateService::check_once(work.to_str().unwrap(), "");
        assert!(!chk2.available);

        // Even when local git HEAD == origin/main (behind == 0), an older installed binary
        // (by SemVer or by compiled commit SHA) must still report an update ready to build:
        let initial_sha = UpdateService::git_cmd(&work, &["rev-parse", "HEAD~1"]).unwrap();
        let chk_old_ver =
            UpdateService::check_once_with_build_info(work.to_str().unwrap(), "", "2.0-beta", "");
        assert!(chk_old_ver.available);
        assert!(chk_old_ver.summary.contains("Update ready to build"));

        let chk_old_sha = UpdateService::check_once_with_build_info(
            work.to_str().unwrap(),
            "",
            "2.1.0",
            &initial_sha,
        );
        assert!(chk_old_sha.available);
        assert_eq!(chk_old_sha.commits_behind, 1);
        assert!(chk_old_sha.summary.contains("1 commit(s) behind repo"));

        // Verify atomic binary replacement works cleanly
        let old_bin = tmp.path().join("bin/luatools");
        let new_bin = tmp.path().join("built/luatools");
        fs::create_dir_all(old_bin.parent().unwrap()).unwrap();
        fs::create_dir_all(new_bin.parent().unwrap()).unwrap();
        fs::write(&old_bin, b"OLD_BIN").unwrap();
        fs::write(&new_bin, b"NEW_BIN_V2_1").unwrap();

        UpdateService::atomic_replace_executable(&new_bin, &old_bin).unwrap();
        assert_eq!(fs::read(&old_bin).unwrap(), b"NEW_BIN_V2_1");

        // Verify prerelease SemVer ordering (2.0-beta < 2.0.0 < 2.1.0)
        assert!(UpdateService::is_newer_semver("2.0.0", "2.0-beta"));
        assert!(UpdateService::is_newer_semver("2.0.0-beta.2", "2.0-beta"));
        assert!(UpdateService::is_newer_semver(
            "2.0.0-beta.10",
            "2.0.0-beta.2"
        ));
        assert!(!UpdateService::is_newer_semver("2.0.0-beta", "2.0-beta"));
        assert!(!UpdateService::is_newer_semver("2.0-beta", "2.0.0"));
    }

    #[test]
    fn parses_cargo_compiling_and_building_progress_lines_monotonically() {
        let mut tracker = CargoProgressTracker::new(4, 0.15, 0.92);

        let e0 = tracker.feed_line("    Updating crates.io index");
        assert!(e0.fraction >= 0.15);
        assert_eq!(e0.stage_label, "Updating crate index");

        let e1 = tracker.feed_line("  Downloaded serde v1.0.219");
        assert!(e1.fraction >= e0.fraction);
        assert_eq!(e1.current_crate, "serde v1.0.219");

        let e2 = tracker.feed_line("   Compiling egui v0.31.1");
        assert_eq!(e2.compiled_crates, 1);
        assert_eq!(e2.total_crates, 4);
        assert_eq!(e2.current_crate, "egui v0.31.1");
        assert!(e2.fraction > e1.fraction);

        let e3 = tracker.feed_line("   Compiling tokio v1.44.0 (/tmp/tokio)");
        assert_eq!(e3.compiled_crates, 2);
        assert_eq!(e3.current_crate, "tokio v1.44.0");
        assert!(e3.fraction > e2.fraction);

        // Exact Cargo progress bar line updates total & done accurately without jumping backward
        let e4 = tracker.feed_line("   Building [===>     ] 48/162: egui, eframe");
        assert_eq!(e4.compiled_crates, 48);
        assert_eq!(e4.total_crates, 162);
        assert_eq!(e4.current_crate, "egui");
        assert!(e4.fraction >= e3.fraction);

        // Exceeding initial estimate dynamically bumps total_crates cleanly
        let mut small = CargoProgressTracker::new(2, 0.15, 0.92);
        let s1 = small.feed_line("   Compiling a v0.1.0");
        let s2 = small.feed_line("   Compiling b v0.1.0");
        let s3 = small.feed_line("   Compiling c v0.1.0");
        assert_eq!(s3.compiled_crates, 3);
        assert!(s3.total_crates > 3);
        assert!(s2.fraction >= s1.fraction);
        assert!(s3.fraction >= s2.fraction);
        assert!(s3.fraction < 0.92);

        // Linking and Finished advance smoothly to end_fraction
        let e5 = tracker.feed_line("   Linking target/release/luatools");
        assert!(e5.fraction >= e4.fraction);
        assert!(tracker.in_linking);

        let e6 = tracker.feed_line("    Finished `release` profile [optimized] target(s) in 41.2s");
        assert!(tracker.finished);
        assert!((e6.fraction - 0.92).abs() < 1e-5);
        assert_eq!(e6.compiled_crates, e6.total_crates);

        // Estimate crate count from Cargo.lock and fallback when missing
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(UpdateService::estimate_total_crates(tmp.path(), None), 120);
        fs::write(
            tmp.path().join("Cargo.lock"),
            "[[package]]\nname = \"a\"\n[[package]]\nname = \"b\"\n[[package]]\nname = \"c\"\n",
        )
        .unwrap();
        assert_eq!(UpdateService::estimate_total_crates(tmp.path(), None), 3);
    }

    fn make_valid_elf64_x86_64_bytes() -> Vec<u8> {
        let mut bytes = vec![0u8; 64];
        bytes[0..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2; // ELFCLASS64
        bytes[5] = 1; // ELFDATA2LSB
        bytes[6] = 1; // EV_CURRENT
        bytes[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
        bytes[18..20].copy_from_slice(&62u16.to_le_bytes()); // EM_X86_64
        bytes.extend_from_slice(b"LUATOOLS_ELF_TEST_PAYLOAD");
        bytes
    }

    #[test]
    fn select_prebuilt_linux_asset_prioritizes_binary_and_tarball_and_filters_incompatible() {
        let with_raw_and_tar = serde_json::json!({
            "tag_name": "v3.4.0",
            "assets": [
                {
                    "name": "luatools-3.4.0-linux-x86_64.deb",
                    "browser_download_url": "https://github.com/subsurfmod/LuaToolsLinux/releases/download/v3.4.0/luatools.deb"
                },
                {
                    "name": "luatools-3.4.0-linux-x86_64.rpm",
                    "browser_download_url": "https://github.com/subsurfmod/LuaToolsLinux/releases/download/v3.4.0/luatools.rpm"
                },
                {
                    "name": "luatools-3.4.0-linux-x86_64.AppImage",
                    "browser_download_url": "https://github.com/subsurfmod/LuaToolsLinux/releases/download/v3.4.0/luatools.AppImage"
                },
                {
                    "name": "luatools-linux-x86_64.sha256",
                    "browser_download_url": "https://github.com/subsurfmod/LuaToolsLinux/releases/download/v3.4.0/luatools-linux-x86_64.sha256"
                },
                {
                    "name": "luatools-windows-x86_64.exe",
                    "browser_download_url": "https://github.com/subsurfmod/LuaToolsLinux/releases/download/v3.4.0/luatools.exe"
                },
                {
                    "name": "luatools-linux-aarch64.tar.gz",
                    "browser_download_url": "https://github.com/subsurfmod/LuaToolsLinux/releases/download/v3.4.0/luatools-arm64.tar.gz"
                },
                {
                    "name": "luatools-linux-x86_64.tar.gz",
                    "browser_download_url": "https://github.com/subsurfmod/LuaToolsLinux/releases/download/v3.4.0/luatools-linux-x86_64.tar.gz",
                    "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                },
                {
                    "name": "luatools-linux-x86_64",
                    "browser_download_url": "https://github.com/subsurfmod/LuaToolsLinux/releases/download/v3.4.0/luatools-linux-x86_64",
                    "digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                }
            ]
        });

        let picked_raw = UpdateService::select_prebuilt_linux_asset(&with_raw_and_tar)
            .expect("should pick raw linux x86_64 binary");
        assert_eq!(picked_raw.name, "luatools-linux-x86_64");
        assert_eq!(picked_raw.kind, PrebuiltAssetKind::RawBinary);
        assert_eq!(
            picked_raw.digest.as_deref(),
            Some("sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );

        let tar_only = serde_json::json!({
            "tag_name": "v3.4.0",
            "assets": [
                {
                    "name": "luatools-linux-x86_64.deb",
                    "browser_download_url": "https://example.invalid/luatools.deb"
                },
                {
                    "name": "luatools-linux-x86_64.tar.gz.sig",
                    "browser_download_url": "https://example.invalid/luatools.tar.gz.sig"
                },
                {
                    "name": "luatools-macos-x86_64.tar.gz",
                    "browser_download_url": "https://example.invalid/luatools-macos.tar.gz"
                },
                {
                    "name": "luatools-linux-arm64.tar.gz",
                    "browser_download_url": "https://example.invalid/luatools-arm64.tar.gz"
                },
                {
                    "name": "luatools-linux-x86_64.tar.xz",
                    "browser_download_url": "https://example.invalid/luatools.tar.xz"
                },
                {
                    "name": "luatools-linux-amd64.tar.gz",
                    "browser_download_url": "https://example.invalid/luatools-amd64.tar.gz",
                    "digest": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                }
            ]
        });

        let picked_tar = UpdateService::select_prebuilt_linux_asset(&tar_only)
            .expect("should pick .tar.gz linux amd64 archive");
        assert_eq!(picked_tar.name, "luatools-linux-amd64.tar.gz");
        assert_eq!(picked_tar.kind, PrebuiltAssetKind::TarGz);
        assert_eq!(
            picked_tar.digest.as_deref(),
            Some("sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc")
        );

        let incompatible_only = serde_json::json!({
            "tag_name": "v3.4.0",
            "assets": [
                { "name": "luatools-linux-x86_64.deb", "browser_download_url": "https://example.invalid/a.deb" },
                { "name": "luatools-linux-x86_64.rpm", "browser_download_url": "https://example.invalid/a.rpm" },
                { "name": "luatools-linux-x86_64.AppImage", "browser_download_url": "https://example.invalid/a.AppImage" },
                { "name": "luatools-linux-x86_64.sha256", "browser_download_url": "https://example.invalid/a.sha256" },
                { "name": "luatools-linux-x86_64.zip", "browser_download_url": "https://example.invalid/a.zip" },
                { "name": "luatools-windows-x86_64.exe", "browser_download_url": "https://example.invalid/a.exe" },
                { "name": "luatools-macos-x86_64.tar.gz", "browser_download_url": "https://example.invalid/m.tar.gz" },
                { "name": "luatools-linux-aarch64.tar.gz", "browser_download_url": "https://example.invalid/arm.tar.gz" }
            ]
        });
        assert!(UpdateService::select_prebuilt_linux_asset(&incompatible_only).is_none());
    }

    #[test]
    fn verify_elf_executable_bytes_accepts_elf64_and_rejects_html_and_corrupt() {
        let valid_elf = make_valid_elf64_x86_64_bytes();
        assert!(UpdateService::verify_elf_executable_bytes(&valid_elf).is_ok());

        let html_err = b"<!DOCTYPE html><html><body>502 Bad Gateway</body></html>";
        assert!(UpdateService::verify_elf_executable_bytes(html_err).is_err());

        let corrupt_short = b"\x7fELF";
        assert!(UpdateService::verify_elf_executable_bytes(corrupt_short).is_err());

        let mut elf32 = make_valid_elf64_x86_64_bytes();
        elf32[4] = 1; // ELFCLASS32
        #[cfg(target_arch = "x86_64")]
        assert!(UpdateService::verify_elf_executable_bytes(&elf32).is_err());

        let mut elf_arm64 = make_valid_elf64_x86_64_bytes();
        elf_arm64[18..20].copy_from_slice(&183u16.to_le_bytes()); // EM_AARCH64
        #[cfg(target_arch = "x86_64")]
        assert!(UpdateService::verify_elf_executable_bytes(&elf_arm64).is_err());
    }

    #[test]
    fn install_prebuilt_binary_bytes_installs_atomically_with_0755_and_rejects_digest_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let target_bin = tmp.path().join("bin/luatools");
        fs::create_dir_all(target_bin.parent().unwrap()).unwrap();
        fs::write(&target_bin, b"ORIGINAL_BINARY").unwrap();

        let elf_bytes = make_valid_elf64_x86_64_bytes();
        let ref_file = tmp.path().join("ref_elf");
        fs::write(&ref_file, &elf_bytes).unwrap();
        let real_hash = FileHashService::new().hash_file(&ref_file, "luatools-linux-x86_64");
        let good_digest = format!("sha256:{}", real_hash.sha256);
        let bad_digest =
            "sha256:0000000000000000000000000000000000000000000000000000000000000000";

        // Reject SHA-256 digest mismatch and leave original binary untouched
        let mismatch_err = UpdateService::install_prebuilt_binary_bytes(
            &elf_bytes,
            "luatools-linux-x86_64",
            Some(bad_digest),
            &target_bin,
        );
        assert!(mismatch_err.is_err());
        assert!(mismatch_err.unwrap_err().contains("SHA-256 mismatch"));
        assert_eq!(fs::read(&target_bin).unwrap(), b"ORIGINAL_BINARY");

        // Reject non-ELF payload even without digest
        let html_err = UpdateService::install_prebuilt_binary_bytes(
            b"<html>not a binary</html>",
            "luatools-linux-x86_64",
            None,
            &target_bin,
        );
        assert!(html_err.is_err());
        assert_eq!(fs::read(&target_bin).unwrap(), b"ORIGINAL_BINARY");

        // Verified ELF64 with matching SHA-256 installs atomically with 0o755 permissions
        UpdateService::install_prebuilt_binary_bytes(
            &elf_bytes,
            "luatools-linux-x86_64",
            Some(&good_digest),
            &target_bin,
        )
        .unwrap();
        assert_eq!(fs::read(&target_bin).unwrap(), elf_bytes);
        let mode = fs::metadata(&target_bin).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }

    #[test]
    fn validate_tar_entries_blocks_traversal_and_toolchain_check_is_actionable() {
        assert!(UpdateService::validate_tar_entries("luatools\nREADME.md\n").is_ok());
        assert!(UpdateService::validate_tar_entries("luatools-v3.4.0/luatools\n").is_ok());
        assert!(UpdateService::validate_tar_entries("../luatools\n").is_err());
        assert!(UpdateService::validate_tar_entries("sub/../../etc/passwd\n").is_err());
        assert!(UpdateService::validate_tar_entries("/usr/bin/luatools\n").is_err());
        assert!(UpdateService::validate_tar_entries("\\evil\\luatools\n").is_err());
        assert!(UpdateService::validate_tar_entries("").is_err());

        assert!(UpdateService::check_source_build_toolchain_with(true, true).is_ok());
        assert!(UpdateService::check_source_build_toolchain_with(false, true)
            .unwrap_err()
            .contains("git"));
        assert!(UpdateService::check_source_build_toolchain_with(true, false)
            .unwrap_err()
            .contains("cargo"));
        let both_err = UpdateService::check_source_build_toolchain_with(false, false).unwrap_err();
        assert!(both_err.contains("git") && both_err.contains("cargo"));
    }
}
