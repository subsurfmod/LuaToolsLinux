//! ExternalToolsManager — inspects, updates, and launches `DepotDownloaderMod`,
//! `SteamAutoCrack`, and `Steamless` with Linux `wine` / `dotnet` wrappers.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use serde_json::{json, Value};
use tar::Archive;
use zip::ZipArchive;

use crate::models::config::default_luatools_data_dir;
use crate::services::api_client::stream_response_to_file;
use crate::services::depot_downloader::{which_in_path, DepotDownloaderService};

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
        key: "steamautocrack",
        name: "SteamAutoCrack",
        description:
            "Automated Goldberg Steam emulator & configuration applier for game directories.",
        github_repo: "SteamAutoCracks/Steam-auto-crack",
        fallback_repo: None,
        exe_candidates: &[
            "SteamAutoCrack.CLI",
            "SteamAutoCrack",
            "SteamAutoCrack.CLI.exe",
            "SteamAutoCrack.exe",
        ],
    },
    ExternalToolSpec {
        key: "steamless",
        name: "Steamless",
        description: "SteamStub DRM unpacker for protected game executables (.exe via Wine).",
        github_repo: "atom0s/Steamless",
        fallback_repo: None,
        exe_candidates: &[
            "Steamless.CLI",
            "Steamless",
            "Steamless.CLI.exe",
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
        if key == "depotdownloader" {
            search_dirs.push(default_luatools_data_dir().join("depotdownloader"));
            if let Some(home) = dirs::home_dir() {
                search_dirs.push(home.join(".local/share/millennium/plugins/luatools/backend"));
            }
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

        for repo in repos {
            progress_cb(0.05, format!("Querying GitHub releases ({})...", repo));
            let url = format!("https://api.github.com/repos/{}/releases/latest", repo);
            let Ok(resp) = agent
                .get(&url)
                .set("User-Agent", "LuaToolsLinux/2.0")
                .call()
            else {
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

            let chosen = assets
                .iter()
                .find(|a| {
                    let n = a
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    (n.ends_with(".zip") || n.ends_with(".tar.gz"))
                        && n.contains("linux")
                        && !n.contains("arm")
                })
                .or_else(|| {
                    assets.iter().find(|a| {
                        let n = a
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        n.ends_with(".zip") || n.ends_with(".tar.gz")
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
            let Ok(dl_resp) = agent.get(dl_url).call() else {
                continue;
            };
            stream_response_to_file(dl_resp, &archive_path, &mut progress_cb)?;

            progress_cb(0.9, "Extracting archive...".into());
            if aname.to_ascii_lowercase().ends_with(".zip") {
                let zf = File::open(&archive_path).map_err(|e| e.to_string())?;
                let mut archive = ZipArchive::new(zf).map_err(|e| e.to_string())?;
                archive.extract(&tool_dir).map_err(|e| e.to_string())?;
            } else {
                let tf = File::open(&archive_path).map_err(|e| e.to_string())?;
                let gz = GzDecoder::new(tf);
                let mut archive = Archive::new(gz);
                archive.unpack(&tool_dir).map_err(|e| e.to_string())?;
            }
            let _ = fs::remove_file(&archive_path);

            let info = self.inspect_tool(key);
            if let Some(exe) = info.exe_path {
                let _ = DepotDownloaderService::build_invocation(&exe);
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let _ = fs::write(
                    tool_dir.join("meta.json"),
                    serde_json::to_string_pretty(&json!({
                        "version": tag,
                        "repo": repo,
                        "last_check": now,
                        "exe_path": exe,
                    }))
                    .unwrap_or_default(),
                );
                progress_cb(1.0, format!("{} {} installed", spec.name, tag));
                return Ok(exe);
            }
        }

        Err(format!(
            "Failed to locate compatible release binary for {}",
            spec.name
        ))
    }

    pub fn launch_steamautocrack(&self, app_id: u32, folder: &Path) -> Result<String, String> {
        let info = self.inspect_tool("steamautocrack");
        let exe = info.exe_path.ok_or_else(|| {
            "SteamAutoCrack is not installed yet. Click INSTALL / UPDATE first.".to_string()
        })?;
        let prefix = DepotDownloaderService::build_invocation(&exe)?;
        let mut cmd = Command::new(&prefix[0]);
        for p in &prefix[1..] {
            cmd.arg(p);
        }
        if exe
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase().contains("cli"))
            .unwrap_or(false)
            && app_id > 0
        {
            cmd.args([
                "crack",
                &folder.to_string_lossy(),
                "--appid",
                &app_id.to_string(),
            ]);
        }
        cmd.spawn().map_err(|e| e.to_string())?;
        Ok(format!("Launched {}", exe.display()))
    }

    pub fn launch_steamless(&self, target_exe: &Path) -> Result<String, String> {
        let info = self.inspect_tool("steamless");
        let exe = info.exe_path.ok_or_else(|| {
            "Steamless is not installed yet. Click INSTALL / UPDATE first.".to_string()
        })?;
        let prefix = DepotDownloaderService::build_invocation(&exe)?;
        let mut cmd = Command::new(&prefix[0]);
        for p in &prefix[1..] {
            cmd.arg(p);
        }
        cmd.arg(target_exe);
        cmd.spawn().map_err(|e| e.to_string())?;
        Ok(format!(
            "Launched Steamless on {}",
            target_exe.file_name().unwrap_or_default().to_string_lossy()
        ))
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
        "exe" => {
            if which_in_path("wine").is_some() {
                "Wine ready".into()
            } else {
                "Requires Wine".into()
            }
        }
        "dll" => {
            if which_in_path("dotnet").is_some() {
                ".NET ready".into()
            } else {
                "Requires dotnet".into()
            }
        }
        _ => "Native Linux".into(),
    }
}
