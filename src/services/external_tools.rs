//! ExternalToolsManager — inspects, updates, and launches `DepotDownloaderMod`,
//! `Goldberg Emulator`, `SteamAutoPatch`, and `StubRemover` with Linux `wine` / `dotnet` wrappers.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use walkdir_lite::collect_files_recursive;

use crate::models::config::default_luatools_data_dir;
use crate::services::api_client::stream_response_to_file;
use crate::services::depot_downloader::{which_in_path, DepotDownloaderService};
use crate::services::github_proxy::GithubProxy;
use crate::services::wired_service::extract_archive;

#[derive(Debug, Clone)]
pub struct ExternalToolSpec {
    pub key: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub github_repo: &'static str,
    pub fallback_repo: Option<&'static str>,
    pub exe_candidates: &'static [&'static str],
}

pub const TOOLS: &[ExternalToolSpec] = &[
    ExternalToolSpec {
        key: "depotdownloader",
        name: "DepotDownloaderMod",
        description:
            "High-concurrency Steam CDN depot downloader with manifestfile & depotkey support.",
        github_repo: "SteamAutoCracks/DepotDownloaderMod",
        fallback_repo: Some("SteamRE/DepotDownloader"),
        exe_candidates: &[
            "DepotDownloaderMod",
            "DepotDownloader",
            "DepotDownloaderMod.dll",
            "DepotDownloader.dll",
            "DepotDownloaderMod.exe",
        ],
    },
    ExternalToolSpec {
        key: "goldberg",
        name: "Goldberg Emulator (Native Linux)",
        description:
            "Native Linux & Proton Steamworks API replacement libraries (libsteam_api.so / steam_api64.dll) used by native SteamAutoPatch.",
        github_repo: "Detanup01/gbe_fork",
        fallback_repo: None,
        exe_candidates: &[
            "libsteam_api.so",
            "steam_api64.so",
            "steam_api.so",
            "steam_api64.dll",
            "steam_api.dll",
        ],
    },
    ExternalToolSpec {
        key: "steamautopatch",
        name: "SteamAutoPatch",
        description:
            "Automated Goldberg Steam emulator & configuration applier for game directories.",
        github_repo: "SteamAutoCracks/Steam-auto-crack",
        fallback_repo: None,
        exe_candidates: &[
            "SteamAutoPatch.CLI",
            "SteamAutoPatch",
            "SteamAutoCrack.CLI",
            "SteamAutoCrack",
            "SteamAutoPatch.CLI.dll",
            "SteamAutoCrack.CLI.dll",
            "SteamAutoPatch.CLI.exe",
            "SteamAutoCrack.CLI.exe",
            "SteamAutoPatch.exe",
            "SteamAutoCrack.exe",
        ],
    },
    ExternalToolSpec {
        key: "stub_remover",
        name: "StubRemover",
        description: "SteamStub unpacker for protected game executables (.exe via Wine).",
        github_repo: "atom0s/Steamless",
        fallback_repo: None,
        exe_candidates: &[
            "StubRemover.CLI",
            "StubRemover",
            "Steamless.CLI",
            "Steamless",
            "StubRemover.CLI.dll",
            "Steamless.CLI.dll",
            "StubRemover.CLI.exe",
            "Steamless.CLI.exe",
            "StubRemover.exe",
            "Steamless.exe",
        ],
    },
];

#[derive(Debug, Clone)]
pub struct ToolStatusInfo {
    pub installed: bool,
    pub version: String,
    pub exe_path: Option<PathBuf>,
    pub runtime_note: String,
}

#[derive(Debug, Clone)]
pub struct ExternalToolsManager {
    pub tools_dir: PathBuf,
}

impl Default for ExternalToolsManager {
    fn default() -> Self {
        Self::new(None)
    }
}

impl ExternalToolsManager {
    pub fn new(tools_dir: Option<PathBuf>) -> Self {
        let dir = tools_dir.unwrap_or_else(|| default_luatools_data_dir().join("tools"));
        let _ = fs::create_dir_all(&dir);
        Self { tools_dir: dir }
    }

    pub fn goldberg_root(&self) -> PathBuf {
        self.tools_dir
            .parent()
            .map(|p| p.join("goldberg"))
            .unwrap_or_else(|| self.tools_dir.join("goldberg"))
    }

    pub fn spec(key: &str) -> Option<&'static ExternalToolSpec> {
        TOOLS.iter().find(|t| t.key == key)
    }

    pub fn inspect_tool(&self, key: &str) -> ToolStatusInfo {
        let Some(spec) = Self::spec(key) else {
            return ToolStatusInfo {
                installed: false,
                version: String::new(),
                exe_path: None,
                runtime_note: String::new(),
            };
        };

        let mut search_dirs = vec![self.tools_dir.join(key)];
        match key {
            "depotdownloader" => {
                search_dirs.push(default_luatools_data_dir().join("depotdownloader"));
                if let Some(home) = dirs::home_dir() {
                    search_dirs.push(home.join(".local/share/millennium/plugins/luatools/backend"));
                }
            }
            "goldberg" => {
                search_dirs.push(self.goldberg_root());
            }
            "steamautopatch" => {
                search_dirs.push(self.tools_dir.join("steamautocrack"));
            }
            "stub_remover" => {
                search_dirs.push(self.tools_dir.join("steamless"));
            }
            _ => {}
        }

        for d in &search_dirs {
            let mut version = String::new();
            if let Ok(raw) = fs::read_to_string(d.join("meta.json")) {
                if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                    if let Some(ver) = v.get("version").and_then(|x| x.as_str()) {
                        if !ver.is_empty() {
                            version = ver.to_string();
                        }
                    }
                }
            }
            if let Some(found) = find_candidate_recursive(d, spec.exe_candidates) {
                let note = runtime_note_for(&found);
                return ToolStatusInfo {
                    installed: true,
                    version: if version.is_empty() {
                        "Local".into()
                    } else {
                        version
                    },
                    exe_path: Some(found),
                    runtime_note: note,
                };
            }
        }

        ToolStatusInfo {
            installed: false,
            version: String::new(),
            exe_path: None,
            runtime_note: String::new(),
        }
    }

    pub fn update_tool<F>(&self, key: &str, mut progress_cb: F) -> Result<PathBuf, String>
    where
        F: FnMut(f32, String),
    {
        let spec = Self::spec(key).ok_or_else(|| format!("Unknown tool '{}'", key))?;
        let tool_dir = self.tools_dir.join(key);
        fs::create_dir_all(&tool_dir).map_err(|e| e.to_string())?;

        let mut repos = vec![spec.github_repo];
        if let Some(fb) = spec.fallback_repo {
            repos.push(fb);
        }

        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(90))
            .build();
        let ua = format!("LuaToolsLinux/{}", crate::VERSION);
        let mut last_err: Option<String> = None;

        for repo in repos {
            progress_cb(0.05, format!("Querying GitHub releases ({})...", repo));
            let url = format!("https://api.github.com/repos/{}/releases/latest", repo);
            let resp = match agent.get(&url).set("User-Agent", &ua).call() {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(format!("GitHub API error ({}): {}", repo, e));
                    continue;
                }
            };
            let rel = match resp.into_json::<Value>() {
                Ok(v) => v,
                Err(e) => {
                    last_err = Some(format!("Invalid release JSON ({}): {}", repo, e));
                    continue;
                }
            };
            let tag = rel
                .get("tag_name")
                .and_then(|v| v.as_str())
                .unwrap_or("latest");
            let Some(assets) = rel.get("assets").and_then(|v| v.as_array()) else {
                continue;
            };

            let is_supported_archive = |n: &str| {
                n.ends_with(".zip")
                    || n.ends_with(".tar.gz")
                    || n.ends_with(".tgz")
                    || n.ends_with(".tar.bz2")
                    || n.ends_with(".tar.xz")
                    || n.ends_with(".tar.zst")
                    || n.ends_with(".7z")
            };

            let mut assets_to_download = Vec::new();
            if key == "goldberg" {
                // For Goldberg Emulator (LT-99, Section 1.3 #7), download both Linux (.so)
                // and Windows (emu-win-release.7z / steam_api.dll / steam_api64.dll) so Proton
                // Windows games can be cracked offline.
                let linux_asset = assets.iter().find(|a| {
                    let n = a
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    is_supported_archive(&n)
                        && n.contains("linux")
                        && !n.contains("arm")
                        && !n.contains("debug")
                });
                let win_asset = assets.iter().find(|a| {
                    let n = a
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    is_supported_archive(&n)
                        && (n.contains("emu-win") || n.contains("win"))
                        && !n.contains("arm")
                        && !n.contains("debug")
                });
                if let Some(la) = linux_asset {
                    assets_to_download.push(la);
                }
                if let Some(wa) = win_asset {
                    if !assets_to_download.iter().any(|a| a.get("name") == wa.get("name")) {
                        assets_to_download.push(wa);
                    }
                }
            } else {
                let chosen = assets
                    .iter()
                    .find(|a| {
                        let n = a
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        is_supported_archive(&n)
                            && n.contains("linux")
                            && !n.contains("arm")
                            && !n.contains("debug")
                    })
                    .or_else(|| {
                        assets.iter().find(|a| {
                            let n = a
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_ascii_lowercase();
                            is_supported_archive(&n)
                                && !n.contains("osx")
                                && !n.contains("macos")
                                && !n.contains("darwin")
                                && !n.contains("arm")
                                && !n.contains("debug")
                        })
                    });
                if let Some(c) = chosen {
                    assets_to_download.push(c);
                }
            }

            if assets_to_download.is_empty() {
                continue;
            }

            let mut dl_failed = false;
            for (idx, asset) in assets_to_download.iter().enumerate() {
                let Some(dl_url) = asset.get("browser_download_url").and_then(|v| v.as_str()) else {
                    dl_failed = true;
                    break;
                };
                let raw_aname = asset
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool.zip");
                let safe_aname = Path::new(raw_aname)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .filter(|s| !s.is_empty() && *s != "." && *s != "..")
                    .unwrap_or("tool.zip");
                let archive_path = tool_dir.join(safe_aname);

                let base_pct = 0.1 + (idx as f32) * 0.4;
                progress_cb(base_pct, format!("Downloading {} {} ({})", spec.name, tag, safe_aname));
                let mut dl_resp_opt = None;
                if GithubProxy::is_github(dl_url) {
                    let proxy = GithubProxy::default();
                    for candidate in proxy.candidates(dl_url) {
                        if let Ok(r) = agent.get(&candidate).set("User-Agent", &ua).call() {
                            dl_resp_opt = Some(r);
                            break;
                        }
                    }
                } else if let Ok(r) = agent.get(dl_url).set("User-Agent", &ua).call() {
                    dl_resp_opt = Some(r);
                }

                let dl_resp = match dl_resp_opt {
                    Some(r) => r,
                    None => {
                        last_err = Some(format!("Download failed ({}): {}", repo, dl_url));
                        dl_failed = true;
                        break;
                    }
                };
                if let Err(e) = stream_response_to_file(dl_resp, &archive_path, &mut |frac, msg| {
                    progress_cb((base_pct + frac.clamp(0.0, 1.0) * 0.35).clamp(0.1, 0.88), msg);
                }) {
                    let _ = fs::remove_file(&archive_path);
                    last_err = Some(format!("Download stream failed ({}): {}", repo, e));
                    dl_failed = true;
                    break;
                }

                progress_cb(0.85 + (idx as f32) * 0.05, format!("Extracting {}...", safe_aname));
                let extract_res = extract_archive(&archive_path, &tool_dir);
                let _ = fs::remove_file(&archive_path);
                if let Err(e) = extract_res {
                    last_err = Some(format!("Extraction failed ({}): {}", repo, e));
                    dl_failed = true;
                    break;
                }
            }

            if dl_failed {
                continue;
            }

            if key == "goldberg" {
                stage_goldberg_layout(&tool_dir, &self.goldberg_root());
            }

            let info = self.inspect_tool(key);
            if let Some(exe) = info.exe_path {
                if key != "goldberg" {
                    let _ = DepotDownloaderService::build_invocation(&exe);
                }
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let meta_json = serde_json::to_string_pretty(&json!({
                    "version": tag,
                    "repo": repo,
                    "last_check": now,
                    "exe_path": exe,
                }))
                .unwrap_or_default();
                let _ = fs::write(tool_dir.join("meta.json"), &meta_json);
                if key == "goldberg" {
                    let _ = fs::write(self.goldberg_root().join("meta.json"), &meta_json);
                }
                progress_cb(1.0, format!("{} {} installed", spec.name, tag));
                return Ok(exe);
            }
        }

        Err(last_err.unwrap_or_else(|| {
            format!(
                "Failed to locate compatible release binary for {}",
                spec.name
            )
        }))
    }

    pub fn launch_steamautopatch(&self, app_id: u32, folder: &Path) -> Result<String, String> {
        if !folder.exists() {
            return Err(format!(
                "Target directory does not exist: {}",
                folder.display()
            ));
        }
        let info = self.inspect_tool("steamautopatch");
        let exe = info.exe_path.ok_or_else(|| {
            "SteamAutoPatch is not installed yet. Install Goldberg Emulator (Native Linux) or SteamAutoPatch in Settings › Updates & Toolchain.".to_string()
        })?;
        let prefix = DepotDownloaderService::build_invocation(&exe)?;
        let Some(first) = prefix.first() else {
            return Err("Failed to construct SteamAutoPatch command invocation.".into());
        };
        let mut cmd = Command::new(first);
        for p in &prefix[1..] {
            cmd.arg(p);
        }
        let fname_lower = exe
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        if fname_lower.contains("cli") && app_id > 0 {
            let subcmd = if fname_lower.contains("steamautocrack") {
                "apply"
            } else {
                "patch"
            };
            cmd.args([
                subcmd,
                &folder.to_string_lossy(),
                "--appid",
                &app_id.to_string(),
            ]);
        }
        cmd.spawn().map_err(|e| e.to_string())?;
        Ok(format!("Launched {}", exe.display()))
    }

    pub fn launch_stub_remover(&self, target_exe: &Path) -> Result<String, String> {
        if !target_exe.is_file() {
            return Err(format!(
                "Target executable does not exist: {}",
                target_exe.display()
            ));
        }
        let info = self.inspect_tool("stub_remover");
        let exe = info.exe_path.ok_or_else(|| {
            "StubRemover is not installed yet. Click INSTALL / UPDATE first.".to_string()
        })?;
        let prefix = DepotDownloaderService::build_invocation(&exe)?;
        let Some(first) = prefix.first() else {
            return Err("Failed to construct StubRemover command invocation.".into());
        };
        let mut cmd = Command::new(first);
        for p in &prefix[1..] {
            cmd.arg(p);
        }
        cmd.arg(target_exe);

        // Wait for Steamless.CLI.exe process completion (LT-106, Section 1.3 #8)
        let output = cmd.output().map_err(|e| format!("Failed to execute StubRemover: {}", e))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let msg = if !stderr.trim().is_empty() {
                stderr.trim()
            } else if !stdout.trim().is_empty() {
                stdout.trim()
            } else {
                "Unknown Steamless error"
            };
            return Err(format!(
                "StubRemover exited with code {}: {}",
                output.status.code().unwrap_or(-1),
                msg
            ));
        }

        // Steamless produces `<target_exe>.unpacked.exe` or `<target_exe_stem>.unpacked.exe`
        let full_unpacked = PathBuf::from(format!("{}.unpacked.exe", target_exe.display()));
        let stem_unpacked = target_exe.with_extension("unpacked.exe");
        let unpacked = if full_unpacked.is_file() {
            Some(full_unpacked)
        } else if stem_unpacked.is_file() {
            Some(stem_unpacked)
        } else {
            None
        };

        if let Some(unpacked_path) = unpacked {
            let backup_path = PathBuf::from(format!("{}.orig.bak", target_exe.display()));
            if !backup_path.exists() {
                let _ = fs::copy(target_exe, &backup_path);
            }
            fs::rename(&unpacked_path, target_exe).map_err(|e| {
                format!(
                    "Failed to replace '{}' with unpacked executable: {}",
                    target_exe.display(),
                    e
                )
            })?;
            Ok(format!(
                "Successfully unpacked SteamStub and replaced {}",
                target_exe.file_name().unwrap_or_default().to_string_lossy()
            ))
        } else {
            Ok(format!(
                "StubRemover completed on {} (no .unpacked.exe output detected; file may not be protected with SteamStub)",
                target_exe.file_name().unwrap_or_default().to_string_lossy()
            ))
        }
    }
}

fn stage_goldberg_layout(extracted: &Path, goldberg_root: &Path) {
    let reg_x64 = goldberg_root.join("regular/x64");
    let reg_x86 = goldberg_root.join("regular/x86");
    let exp_x64 = goldberg_root.join("experimental/x64");
    let exp_x86 = goldberg_root.join("experimental/x86");
    for d in [&reg_x64, &reg_x86, &exp_x64, &exp_x86] {
        let _ = fs::create_dir_all(d);
    }

    for path in collect_files_recursive(extracted) {
        let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let fname_lower = fname.to_ascii_lowercase();
        let full_lower = path.to_string_lossy().to_ascii_lowercase();
        let is_exp = full_lower.contains("experimental");
        let is_x86 = full_lower.contains("/x32/")
            || full_lower.contains("/x86/")
            || full_lower.contains("/linux32/")
            || full_lower.contains("/i386/")
            || full_lower.contains("/i686/")
            || fname_lower == "steam_api.so";

        match fname_lower.as_str() {
            "libsteam_api.so" | "steam_api64.so" | "steam_api.so" => {
                let dst_dir = match (is_x86, is_exp) {
                    (false, false) => &reg_x64,
                    (false, true) => &exp_x64,
                    (true, false) => &reg_x86,
                    (true, true) => &exp_x86,
                };
                let target_name = if is_x86 {
                    "steam_api.so"
                } else {
                    "steam_api64.so"
                };
                let _ = fs::copy(&path, dst_dir.join(target_name));
                let _ = fs::copy(&path, dst_dir.join("libsteam_api.so"));
            }
            "steam_api64.dll" => {
                let dst_dir = if is_exp { &exp_x64 } else { &reg_x64 };
                let _ = fs::copy(&path, dst_dir.join("steam_api64.dll"));
            }
            "steam_api.dll" => {
                let dst_dir = if is_exp { &exp_x86 } else { &reg_x86 };
                let _ = fs::copy(&path, dst_dir.join("steam_api.dll"));
            }
            _ => {}
        }
    }
}

mod walkdir_lite {
    use std::fs;
    use std::path::{Path, PathBuf};

    const MAX_DEPTH: usize = 8;

    pub fn collect_files_recursive(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        collect_depth(dir, 0, &mut out);
        out
    }

    fn collect_depth(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > MAX_DEPTH {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let Ok(ft) = e.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            let p = e.path();
            if ft.is_dir() {
                collect_depth(&p, depth + 1, out);
            } else if ft.is_file() {
                out.push(p);
            }
        }
    }
}

fn find_candidate_recursive(dir: &Path, candidates: &[&str]) -> Option<PathBuf> {
    find_candidate_depth(dir, candidates, 0)
}

fn find_candidate_depth(dir: &Path, candidates: &[&str], depth: usize) -> Option<PathBuf> {
    if depth > 6 || !dir.is_dir() {
        return None;
    }
    for &c in candidates {
        let direct = dir.join(c);
        if direct.is_file() {
            return Some(direct);
        }
    }
    let entries = fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        if e.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            if let Some(found) = find_candidate_depth(&e.path(), candidates, depth + 1) {
                return Some(found);
            }
        }
    }
    None
}

fn runtime_note_for(exe: &Path) -> String {
    match exe
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "so" => "Native Linux (.so)".into(),
        "exe" => {
            if which_in_path("wine").is_some() {
                "Wine ready".into()
            } else {
                "Requires Wine".into()
            }
        }
        "dll" => {
            let fname = exe
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if fname.starts_with("steam_api") {
                "Proton / Wine DLL".into()
            } else if which_in_path("dotnet").is_some() {
                ".NET ready".into()
            } else {
                "Requires dotnet".into()
            }
        }
        _ => "Native Linux".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_goldberg_layout_separates_x64_x86_linux32_and_experimental() {
        let tmp = tempfile::tempdir().unwrap();
        let extracted = tmp.path().join("extracted");
        let gb_root = tmp.path().join("goldberg");

        fs::create_dir_all(extracted.join("release/regular/x64")).unwrap();
        fs::create_dir_all(extracted.join("release/regular/linux32")).unwrap();
        fs::create_dir_all(extracted.join("release/experimental/x64")).unwrap();
        fs::write(
            extracted.join("release/regular/x64/libsteam_api.so"),
            b"X64_REG",
        )
        .unwrap();
        fs::write(
            extracted.join("release/regular/linux32/libsteam_api.so"),
            b"X86_REG",
        )
        .unwrap();
        fs::write(
            extracted.join("release/experimental/x64/Steam_api64.dll"),
            b"X64_EXP_DLL",
        )
        .unwrap();

        stage_goldberg_layout(&extracted, &gb_root);

        assert_eq!(
            fs::read(gb_root.join("regular/x64/libsteam_api.so")).unwrap(),
            b"X64_REG"
        );
        assert_eq!(
            fs::read(gb_root.join("regular/x86/steam_api.so")).unwrap(),
            b"X86_REG"
        );
        assert_eq!(
            fs::read(gb_root.join("experimental/x64/steam_api64.dll")).unwrap(),
            b"X64_EXP_DLL"
        );
    }

    #[test]
    fn inspect_tool_does_not_leak_version_from_empty_dir_and_validates_launch_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let tools_dir = tmp.path().join("tools");
        let mgr = ExternalToolsManager::new(Some(tools_dir.clone()));

        // Stale meta.json in primary dir with no binary
        fs::create_dir_all(tools_dir.join("stub_remover")).unwrap();
        fs::write(
            tools_dir.join("stub_remover/meta.json"),
            r#"{"version":"stale-0.1"}"#,
        )
        .unwrap();
        // Actual binary in legacy dir without meta.json
        fs::create_dir_all(tools_dir.join("steamless")).unwrap();
        fs::write(tools_dir.join("steamless/Steamless.CLI.exe"), b"MZ").unwrap();

        let status = mgr.inspect_tool("stub_remover");
        assert!(status.installed);
        assert_eq!(status.version, "Local");

        assert!(mgr
            .launch_stub_remover(&tmp.path().join("missing.exe"))
            .is_err());
        assert!(mgr
            .launch_steamautopatch(1245620, &tmp.path().join("missing_dir"))
            .is_err());
    }
}
