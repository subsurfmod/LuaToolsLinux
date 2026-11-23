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
        description: "SteamStub DRM unpacker for protected game executables (.exe via Wine).",
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

        let mut version = String::new();
        for d in &search_dirs {
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
            .timeout(Duration::from_secs(90))
            .build();
        let ua = format!("LuaToolsLinux/{}", crate::VERSION);

        for repo in repos {
            progress_cb(0.05, format!("Querying GitHub releases ({})...", repo));
            let url = format!("https://api.github.com/repos/{}/releases/latest", repo);
            let Ok(resp) = agent.get(&url).set("User-Agent", &ua).call() else {
                continue;
            };
            let Ok(rel) = resp.into_json::<Value>() else {
                continue;
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

            let Some(asset) = chosen else {
                continue;
            };
            let Some(dl_url) = asset.get("browser_download_url").and_then(|v| v.as_str()) else {
                continue;
            };
            let aname = asset
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("tool.zip");
            let archive_path = tool_dir.join(aname);

            progress_cb(0.1, format!("Downloading {} {}...", spec.name, tag));
            let Ok(dl_resp) = agent.get(dl_url).set("User-Agent", &ua).call() else {
                continue;
            };
            stream_response_to_file(dl_resp, &archive_path, &mut progress_cb)?;

            progress_cb(0.9, "Extracting archive...".into());
            let extract_res = extract_archive(&archive_path, &tool_dir);
            let _ = fs::remove_file(&archive_path);
            extract_res?;

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

        Err(format!(
            "Failed to locate compatible release binary for {}",
            spec.name
        ))
    }

    pub fn launch_steamautopatch(&self, app_id: u32, folder: &Path) -> Result<String, String> {
        let info = self.inspect_tool("steamautopatch");
        let exe = info.exe_path.ok_or_else(|| {
            "SteamAutoPatch is not installed yet. Install Goldberg Emulator (Native Linux) or SteamAutoPatch in Settings › Updates & Toolchain.".to_string()
        })?;
        let prefix = DepotDownloaderService::build_invocation(&exe)?;
        let mut cmd = Command::new(&prefix[0]);
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
                "crack"
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
        let info = self.inspect_tool("stub_remover");
        let exe = info.exe_path.ok_or_else(|| {
            "StubRemover is not installed yet. Click INSTALL / UPDATE first.".to_string()
        })?;
        let prefix = DepotDownloaderService::build_invocation(&exe)?;
        let mut cmd = Command::new(&prefix[0]);
        for p in &prefix[1..] {
            cmd.arg(p);
        }
        cmd.arg(target_exe);
        cmd.spawn().map_err(|e| e.to_string())?;
        Ok(format!(
            "Launched StubRemover on {}",
            target_exe.file_name().unwrap_or_default().to_string_lossy()
        ))
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
        let full_lower = path.to_string_lossy().to_ascii_lowercase();
        let is_exp = full_lower.contains("experimental");
        let is_x86 =
            full_lower.contains("/x32/") || full_lower.contains("/x86/") || fname == "steam_api.so";

        match fname {
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

    pub fn collect_files_recursive(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(dir) else {
            return out;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(collect_files_recursive(&p));
            } else if p.is_file() {
                out.push(p);
            }
        }
        out
    }
}

fn find_candidate_recursive(dir: &Path, candidates: &[&str]) -> Option<PathBuf> {
    if !dir.exists() {
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
        let p = e.path();
        if p.is_dir() {
            if let Some(found) = find_candidate_recursive(&p, candidates) {
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
