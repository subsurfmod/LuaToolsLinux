//! UpdateService — one-shot background Git update check, `git pull` + native rebuild,
//! atomic executable replacement (immune to Linux `ETXTBSY`), and one-click relaunch.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::config::default_luatools_data_dir;
use crate::services::steam_service::expand_tilde;
use crate::{BUILD_GIT_SHA, VERSION};

pub const DEFAULT_GIT_REMOTE_URL: &str = "https://github.com/subsurfmod/LuaToolsLinux.git";

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
                    let fetch_res = if origin_url.is_some() && explicit_remote.is_none() {
                        Self::git_cmd(dir, &["fetch", "--quiet", "origin"])
                    } else {
                        Self::git_cmd(dir, &["fetch", "--quiet", rem_url])
                    };

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

                    let upstream_candidates = if origin_url.is_some() && explicit_remote.is_none() {
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

    fn check_github_remote_version(remote_url: &str) -> Option<(String, Option<String>)> {
        let trimmed = remote_url
            .trim()
            .trim_end_matches(".git")
            .trim_end_matches('/');
        let slug = trimmed.strip_prefix("https://github.com/")?;
        let mut parts = slug.split('/');
        let owner = parts.next()?;
        let repo = parts.next()?;

        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(6))
            .build();

        // Check latest release first
        let rel_url = format!(
            "https://api.github.com/repos/{}/{}/releases/latest",
            owner, repo
        );
        if let Ok(resp) = agent
            .get(&rel_url)
            .set("User-Agent", "LuaToolsLinux/2.0")
            .call()
        {
            if let Ok(val) = resp.into_json::<Value>() {
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
                if let Ok(resp) = agent.get(&raw_url).call() {
                    if let Ok(txt) = resp.into_string() {
                        if let Some(ver) = Self::parse_cargo_version(&txt) {
                            return Some((ver, Some(format!("{}/{} ({})", owner, repo, branch))));
                        }
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
        let meta = if override_repo_dir.trim().is_empty() {
            Self::load_meta()
        } else {
            SourceRepoMeta::default()
        };
        let explicit_remote = (!override_remote_url.trim().is_empty())
            .then(|| override_remote_url.trim().to_string());

        if let Some(dir) = Self::discover_repo_dir(override_repo_dir) {
            if dir.join(".git").exists() {
                if let Some(ref rem) = explicit_remote {
                    if Self::git_cmd(&dir, &["remote", "get-url", "origin"]).is_ok() {
                        let _ = Self::git_cmd(&dir, &["remote", "set-url", "origin", rem]);
                    } else {
                        let _ = Self::git_cmd(&dir, &["remote", "add", "origin", rem]);
                    }
                }

                // Try clean fast-forward pull first, then rebase+autostash fallback
                if Self::git_cmd(&dir, &["pull", "--ff-only"]).is_err() {
                    Self::git_cmd(&dir, &["pull", "--rebase", "--autostash"])
                        .map_err(|e| format!("git pull failed: {}", e))?;
                }

                let head_info = Self::git_cmd(&dir, &["log", "-1", "--format=%h — %s"])
                    .unwrap_or_else(|_| "updated".into());
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

        let out = Command::new("git")
            .args(["clone", "--depth", "1", &rem])
            .arg(&target_dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|e| format!("Failed to invoke git clone: {}", e))?;
        if !out.status.success() {
            return Err(format!(
                "git clone failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }

        let head_info = Self::git_cmd(&target_dir, &["log", "-1", "--format=%h — %s"])
            .unwrap_or_else(|_| "cloned".into());
        Self::save_meta(Some(&target_dir), Some(&rem));
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
        fs::copy(src_bin, &tmp).map_err(|e| format!("Copy failed: {}", e))?;
        let mut perms = fs::metadata(&tmp).map_err(|e| e.to_string())?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&tmp, perms).map_err(|e| e.to_string())?;
        fs::rename(&tmp, dst_bin).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("Atomic rename onto {} failed: {}", dst_bin.display(), e)
        })?;
        Ok(())
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

    /// Full one-click update workflow: `git pull` -> `cargo build --release` -> atomic binary update.
    pub fn pull_and_update<F>(
        override_repo_dir: &str,
        override_remote_url: &str,
        mut progress_cb: F,
    ) -> Result<UpdateApplyResult, String>
    where
        F: FnMut(f32, String),
    {
        progress_cb(0.15, "1/3 Pulling latest files from Git...".into());
        let (repo_dir, commit_info) =
            Self::pull_git_changes(override_repo_dir, override_remote_url)?;

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

        progress_cb(
            0.45,
            format!("2/3 Building native release binary (v{})...", new_version),
        );

        let built_bin = if let Some(cargo) = Self::find_cargo_bin() {
            let out = Command::new(cargo)
                .args(["build", "--workspace", "--release", "-j", "2"])
                .current_dir(&crate_dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .map_err(|e| format!("Failed to run cargo build: {}", e))?;
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let tail = stderr.lines().rev().take(6).collect::<Vec<_>>();
                let msg: Vec<_> = tail.into_iter().rev().collect();
                return Err(format!("Cargo build failed: {}", msg.join(" | ")));
            }
            let built_so = crate_dir.join("target/release/libsteam_integration.so");
            if built_so.is_file() {
                if let Some(home) = dirs::home_dir() {
                    let lib_dst = home.join(".local/lib/luatools/libsteam_integration.so");
                    let _ = Self::atomic_replace_executable(&built_so, &lib_dst);
                }
            }
            crate_dir.join("target/release/luatools")
        } else {
            let prebuilt = crate_dir.join("dist/luatools");
            if prebuilt.is_file() {
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

        progress_cb(0.85, "3/3 Installing updated executable...".into());

        // 1. Update <crate_dir>/dist/luatools
        let dist_bin = crate_dir.join("dist/luatools");
        let _ = Self::atomic_replace_executable(&built_bin, &dist_bin);

        // 2. Update ~/.local/bin/luatools if installed there
        let mut primary_target = dist_bin;
        if let Some(home) = dirs::home_dir() {
            let local_bin = home.join(".local/bin/luatools");
            if local_bin.exists() && Self::atomic_replace_executable(&built_bin, &local_bin).is_ok()
            {
                primary_target = local_bin;
            }
        }

        // 3. Update the currently running binary path if writable
        if let Ok(curr_exe) = std::env::current_exe() {
            if Self::atomic_replace_executable(&built_bin, &curr_exe).is_ok() {
                primary_target = curr_exe;
            }
        }

        progress_cb(
            1.0,
            format!(
                "Updated to v{} ({}) — ready to restart!",
                new_version, commit_info
            ),
        );

        Ok(UpdateApplyResult {
            new_version,
            commit_info,
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
}
