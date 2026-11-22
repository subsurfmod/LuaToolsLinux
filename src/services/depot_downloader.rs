//! DepotDownloaderMod / DepotDownloader service — tool discovery, GitHub auto-update,
//! key/manifest resolution, and streaming multi-depot / single-depot execution.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde_json::{json, Value};
use zip::ZipArchive;

use crate::models::config::default_luatools_data_dir;
use crate::models::steam::SteamInstallation;
use crate::parsers::lua_parser::LuaFileParser;
use crate::services::api_client::stream_response_to_file;
use crate::services::slssteam_service::SlssteamService;

#[derive(Debug, Clone)]
pub struct DepotSelection {
    pub depot_id: u32,
    pub manifest_id: Option<String>,
    pub manifest_path: Option<PathBuf>,
    pub size: u64,
    pub from_app_id: Option<u32>,
    pub language: Option<String>,
    pub os: Option<String>,
    pub is_shared: bool,
    pub is_dlc: bool,
}

#[derive(Debug, Clone)]
pub struct DepotRunResult {
    pub depot_id: u32,
    pub ok: bool,
    pub error: Option<String>,
}

const GITHUB_REPOS: &[(&str, &str)] = &[
    ("SteamAutoCracks/DepotDownloaderMod", "DepotDownloaderMod"),
    ("SteamRE/DepotDownloader", "DepotDownloader"),
];
const TOOL_CHECK_INTERVAL_SECS: u64 = 6 * 3600;

#[derive(Debug, Clone)]
pub struct DepotDownloaderService {
    pub tool_dir: PathBuf,
}

impl Default for DepotDownloaderService {
    fn default() -> Self {
        Self::new(None)
    }
}

impl DepotDownloaderService {
    pub fn new(tool_dir: Option<PathBuf>) -> Self {
        let dir = tool_dir.unwrap_or_else(|| default_luatools_data_dir().join("depotdownloader"));
        let _ = fs::create_dir_all(&dir);
        Self { tool_dir: dir }
    }

    pub fn find_executable(&self, custom_workshop_path: Option<&str>) -> Option<PathBuf> {
        if let Some(wp) = custom_workshop_path
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let p = PathBuf::from(wp);
            if p.is_file() {
                return Some(p);
            }
            if p.is_dir() {
                if let Some(found) = Self::scan_dir_for_bin(&p) {
                    return Some(found);
                }
            }
        }

        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let search_dirs = [
            self.tool_dir.clone(),
            default_luatools_data_dir()
                .join("tools")
                .join("depotdownloader"),
            home.join(".local/share/millennium/plugins/luatools/backend"),
            home.join(".local/share/LuaTools/depotdownloader"),
            home.join("Applications"),
        ];

        for d in &search_dirs {
            if let Some(found) = Self::scan_dir_for_bin(d) {
                return Some(found);
            }
        }

        which_in_path("DepotDownloaderMod").or_else(|| which_in_path("DepotDownloader"))
    }

    fn scan_dir_for_bin(dir: &Path) -> Option<PathBuf> {
        if !dir.exists() {
            return None;
        }
        let candidates = [
            "DepotDownloaderMod",
            "DepotDownloader",
            "DepotDownloaderMod.dll",
            "DepotDownloader.dll",
            "DepotDownloaderMod.exe",
        ];
        for name in candidates {
            let direct = dir.join(name);
            if direct.is_file() {
                return Some(direct);
            }
        }
        if let Ok(entries) = fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if let Some(sub) = Self::scan_dir_for_bin(&p) {
                        return Some(sub);
                    }
                }
            }
        }
        None
    }

    pub fn build_invocation(tool_path: &Path) -> Result<Vec<String>, String> {
        let ext = tool_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "dll" => {
                let dotnet = which_in_path("dotnet").ok_or_else(|| {
                    format!(
                        ".NET runtime ('dotnet') is required to execute {}",
                        tool_path.display()
                    )
                })?;
                Ok(vec![
                    dotnet.to_string_lossy().to_string(),
                    tool_path.to_string_lossy().to_string(),
                ])
            }
            "exe" => {
                let wine = which_in_path("wine").ok_or_else(|| {
                    format!(
                        "Wine is required to execute Windows binary {}",
                        tool_path.display()
                    )
                })?;
                Ok(vec![
                    wine.to_string_lossy().to_string(),
                    tool_path.to_string_lossy().to_string(),
                ])
            }
            _ => {
                if let Ok(meta) = fs::metadata(tool_path) {
                    let mode = meta.permissions().mode();
                    if mode & 0o111 == 0 {
                        let _ = fs::set_permissions(
                            tool_path,
                            fs::Permissions::from_mode(mode | 0o755),
                        );
                    }
                }
                Ok(vec![tool_path.to_string_lossy().to_string()])
            }
        }
    }

    pub fn ensure_tool<F>(
        &self,
        custom_workshop_path: Option<&str>,
        mut progress_cb: F,
    ) -> Option<PathBuf>
    where
        F: FnMut(f32, String),
    {
        let meta_file = self.tool_dir.join("meta.json");
        if let Some(existing) = self.find_executable(custom_workshop_path) {
            if let Ok(raw) = fs::read_to_string(&meta_file) {
                if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                    let last = v.get("last_check").and_then(|x| x.as_u64()).unwrap_or(0);
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    if now.saturating_sub(last) < TOOL_CHECK_INTERVAL_SECS {
                        return Some(existing);
                    }
                }
            } else {
                return Some(existing);
            }
        }

        self.update_tool(&mut progress_cb)
            .or_else(|| self.find_executable(custom_workshop_path))
    }

    fn update_tool<F>(&self, progress_cb: &mut F) -> Option<PathBuf>
    where
        F: FnMut(f32, String),
    {
        let _ = fs::create_dir_all(&self.tool_dir);
        progress_cb(
            0.05,
            "Checking GitHub for Linux DepotDownloader releases...".into(),
        );

        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(60))
            .build();

        for &(repo, label) in GITHUB_REPOS {
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
                    n.ends_with(".zip") && n.contains("linux") && !n.contains("arm")
                })
                .or_else(|| {
                    assets.iter().find(|a| {
                        let n = a
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        n.ends_with(".zip") && n.contains("linux")
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
                .unwrap_or("depotdownloader.zip");
            let zip_path = self.tool_dir.join(aname);

            progress_cb(0.1, format!("Downloading {} {}...", label, tag));
            let Ok(dl_resp) = agent.get(dl_url).call() else {
                continue;
            };
            if stream_response_to_file(dl_resp, &zip_path, progress_cb).is_err() {
                continue;
            }

            progress_cb(0.92, "Extracting archive...".into());
            if let Ok(zf) = File::open(&zip_path) {
                if let Ok(mut archive) = ZipArchive::new(zf) {
                    let _ = archive.extract(&self.tool_dir);
                }
            }
            let _ = fs::remove_file(&zip_path);

            if let Some(exe) = Self::scan_dir_for_bin(&self.tool_dir) {
                let _ = Self::build_invocation(&exe);
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let _ = fs::write(
                    self.tool_dir.join("meta.json"),
                    serde_json::to_string_pretty(&json!({
                        "version": tag,
                        "repo": repo,
                        "last_check": now,
                        "exe_path": exe,
                    }))
                    .unwrap_or_default(),
                );
                progress_cb(1.0, format!("{} {} ready", label, tag));
                return Some(exe);
            }
        }
        None
    }

    pub fn resolve_keys(
        steam_inst: Option<&SteamInstallation>,
        sls: &SlssteamService,
        app_id: u32,
    ) -> BTreeMap<u32, String> {
        let mut keys = sls.config.depot_keys.clone();
        if let Some(inst) = steam_inst {
            let lua_path = inst.stplug_in_path.join(format!("{}.lua", app_id));
            if let Some(lua) = LuaFileParser::parse_file(&lua_path, app_id) {
                for e in lua.entries {
                    if let Some(k) = e.key {
                        keys.insert(e.id, k);
                    }
                }
            }
        }
        keys
    }

    pub fn resolve_manifest_path(
        steam_inst: Option<&SteamInstallation>,
        depot_id: u32,
        manifest_id: &str,
    ) -> Option<PathBuf> {
        let inst = steam_inst?;
        let fname = format!("{}_{}.manifest", depot_id, manifest_id);
        let mut dirs = vec![
            inst.root.join("depotcache"),
            inst.root.join("steamapps").join("depotcache"),
        ];
        for lib in &inst.libraries {
            dirs.push(lib.depotcache());
            dirs.push(lib.steamapps_depotcache());
        }
        dirs.into_iter()
            .map(|d| d.join(&fname))
            .find(|p| p.exists())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn download_depot<F>(
        &self,
        app_id: u32,
        selection: &DepotSelection,
        download_dir: &Path,
        keys: &BTreeMap<u32, String>,
        custom_workshop_path: Option<&str>,
        mut progress_cb: F,
    ) -> DepotRunResult
    where
        F: FnMut(f32, String),
    {
        let Some(exe) = self.ensure_tool(custom_workshop_path, &mut progress_cb) else {
            return DepotRunResult {
                depot_id: selection.depot_id,
                ok: false,
                error: Some("DepotDownloaderMod / DepotDownloader not found".into()),
            };
        };

        let prefix = match Self::build_invocation(&exe) {
            Ok(p) => p,
            Err(e) => {
                return DepotRunResult {
                    depot_id: selection.depot_id,
                    ok: false,
                    error: Some(e),
                }
            }
        };

        if let Err(e) = fs::create_dir_all(download_dir) {
            return DepotRunResult {
                depot_id: selection.depot_id,
                ok: false,
                error: Some(e.to_string()),
            };
        }

        let target_app = selection.from_app_id.unwrap_or(app_id);
        let mut cmd = Command::new(&prefix[0]);
        for p in &prefix[1..] {
            cmd.arg(p);
        }
        cmd.args([
            "-app",
            &target_app.to_string(),
            "-depot",
            &selection.depot_id.to_string(),
            "-dir",
            &download_dir.to_string_lossy(),
            "-username",
            "anonymous",
            "-max-downloads",
            "32",
            "-validate",
        ]);

        if let Some(mp) = &selection.manifest_path {
            if mp.exists() {
                cmd.arg("-manifestfile").arg(mp);
            }
        }
        if let Some(k) = keys.get(&selection.depot_id) {
            cmd.arg("-depotkey").arg(k);
        }
        if let Some(gid) = &selection.manifest_id {
            cmd.arg("-manifest").arg(gid);
        }

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return DepotRunResult {
                    depot_id: selection.depot_id,
                    ok: false,
                    error: Some(e.to_string()),
                }
            }
        };

        let stderr_handle = child.stderr.take().map(|stderr| {
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                let mut last_err = String::new();
                for line in reader.lines().map_while(Result::ok) {
                    let clean = line.trim();
                    if !clean.is_empty() {
                        last_err = clean.to_string();
                    }
                }
                last_err
            })
        });

        let pct_re = Regex::new(r"(\d{1,3}\.\d{2})%").unwrap();
        let mut last_line = String::new();
        if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                let clean = line.trim().to_string();
                if clean.is_empty() {
                    continue;
                }
                last_line = clean.clone();
                let pct = pct_re
                    .captures(&clean)
                    .and_then(|c| c.get(1))
                    .and_then(|m| m.as_str().parse::<f32>().ok())
                    .unwrap_or(0.0);
                progress_cb(pct, clean);
            }
        }

        let last_err = stderr_handle
            .and_then(|h| h.join().ok())
            .unwrap_or_default();
        if last_line.is_empty() && !last_err.is_empty() {
            last_line = last_err;
        }

        match child.wait() {
            Ok(status) if status.success() => DepotRunResult {
                depot_id: selection.depot_id,
                ok: true,
                error: None,
            },
            Ok(status) => DepotRunResult {
                depot_id: selection.depot_id,
                ok: false,
                error: Some(format!(
                    "Exited with code {}: {}",
                    status.code().unwrap_or(-1),
                    last_line
                )),
            },
            Err(e) => DepotRunResult {
                depot_id: selection.depot_id,
                ok: false,
                error: Some(e.to_string()),
            },
        }
    }
}

pub fn which_in_path(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let full = dir.join(binary);
        if full.is_file() {
            return Some(full);
        }
    }
    None
}
