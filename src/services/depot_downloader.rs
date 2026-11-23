//! Official `SteamRE/DepotDownloader` integration — binary/DLL discovery, GitHub release
//! installer, Fedora/.NET runtime crash hardening (`stdin=null`, `W^X=0`, `RLIMIT_CORE=0`),
//! optional `-remember-password` / `-qr` Steam session login, and streaming depot downloads.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
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

#[derive(Debug, Clone, Default)]
pub struct DepotDownloadOptions {
    pub custom_tool_path: String,
    pub steam_username: String,
}

const DEPOTDOWNLOADER_REPO: &str = "SteamRE/DepotDownloader";
const TOOL_CHECK_INTERVAL_SECS: u64 = 6 * 3600;

#[derive(Debug, Clone)]
pub struct DepotDownloaderService {
    pub tool_dir: PathBuf,
    pub steam_installation: Option<SteamInstallation>,
    pub slssteam_service: Option<SlssteamService>,
    pub bridge_service: Option<std::sync::Arc<std::sync::Mutex<crate::services::steam_bridge_service::SteamBridgeService>>>,
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
        Self {
            tool_dir: dir,
            steam_installation: None,
            slssteam_service: None,
            bridge_service: None,
        }
    }

    pub fn with_steam(&mut self, steam: SteamInstallation, sls: SlssteamService) {
        self.steam_installation = Some(steam);
        self.slssteam_service = Some(sls);
    }

    pub fn with_bridge(&mut self, bridge: crate::services::steam_bridge_service::SteamBridgeService) {
        self.bridge_service = Some(std::sync::Arc::new(std::sync::Mutex::new(bridge)));
    }

    pub fn find_executable(&self, custom_tool_path: Option<&str>) -> Option<PathBuf> {
        if let Some(wp) = custom_tool_path.map(str::trim).filter(|s| !s.is_empty()) {
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
            home.join(".local/share/LuaTools/depotdownloader"),
            home.join("Applications"),
        ];

        for d in &search_dirs {
            if let Some(found) = Self::scan_dir_for_bin(d) {
                return Some(found);
            }
        }

        which_in_path("DepotDownloader").or_else(|| which_in_path("depotdownloader"))
    }

    fn scan_dir_for_bin(dir: &Path) -> Option<PathBuf> {
        if !dir.exists() {
            return None;
        }
        // When system `dotnet` is installed (e.g. Fedora 44/45 with native glibc 2.44 / OpenSSL 3.5),
        // prefer `DepotDownloader.dll` so it runs on the host .NET runtime instead of a older bundled runtime.
        let has_system_dotnet = which_in_path("dotnet").is_some();
        let candidates: &[&str] = if has_system_dotnet {
            &["DepotDownloader.dll", "DepotDownloader", "depotdownloader"]
        } else {
            &["DepotDownloader", "depotdownloader", "DepotDownloader.dll"]
        };

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

    pub fn is_mod_binary(tool_path: &Path) -> bool {
        tool_path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase().contains("mod"))
            .unwrap_or(false)
    }

    pub fn build_download_args(
        target_app: u32,
        selection: &DepotSelection,
        download_dir: &Path,
        steam_username: &str,
        depot_keys_file: Option<&Path>,
    ) -> Vec<String> {
        Self::build_download_args_for_tool(
            target_app,
            selection,
            download_dir,
            steam_username,
            depot_keys_file,
            false,
        )
    }

    pub fn build_download_args_for_tool(
        target_app: u32,
        selection: &DepotSelection,
        download_dir: &Path,
        steam_username: &str,
        depot_keys_file: Option<&Path>,
        is_mod: bool,
    ) -> Vec<String> {
        let mut args = vec![
            "-app".to_string(),
            target_app.to_string(),
            "-depot".to_string(),
            selection.depot_id.to_string(),
        ];
        if let Some(gid) = selection
            .manifest_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            args.push("-manifest".to_string());
            args.push(gid.to_string());
        }
        if is_mod {
            if let Some(mp) = selection.manifest_path.as_deref().filter(|p| p.is_file()) {
                args.push("-manifestfile".to_string());
                args.push(mp.to_string_lossy().to_string());
            }
        }
        if let Some(keys_path) = depot_keys_file {
            args.push("-depotkeys".to_string());
            args.push(keys_path.to_string_lossy().to_string());
        }
        args.extend([
            "-dir".to_string(),
            download_dir.to_string_lossy().to_string(),
            "-max-downloads".to_string(),
            "8".to_string(),
            "-validate".to_string(),
        ]);

        let clean_user = steam_username.trim();
        if !clean_user.is_empty() && !clean_user.eq_ignore_ascii_case("anonymous") {
            args.push("-username".to_string());
            args.push(clean_user.to_string());
            args.push("-remember-password".to_string());
        }
        args
    }

    pub fn ensure_tool<F>(
        &self,
        custom_tool_path: Option<&str>,
        mut progress_cb: F,
    ) -> Option<PathBuf>
    where
        F: FnMut(f32, String),
    {
        let meta_file = self.tool_dir.join("meta.json");
        if let Some(existing) = self.find_executable(custom_tool_path) {
            if let Ok(raw) = fs::read_to_string(&meta_file) {
                if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                    let last = v.get("last_check").and_then(|x| x.as_u64()).unwrap_or(0);
                    let repo = v.get("repo").and_then(|x| x.as_str()).unwrap_or("");
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    if repo == DEPOTDOWNLOADER_REPO
                        && now.saturating_sub(last) < TOOL_CHECK_INTERVAL_SECS
                    {
                        return Some(existing);
                    }
                }
            } else {
                return Some(existing);
            }
        }

        self.update_tool(&mut progress_cb)
            .or_else(|| self.find_executable(custom_tool_path))
    }

    fn update_tool<F>(&self, progress_cb: &mut F) -> Option<PathBuf>
    where
        F: FnMut(f32, String),
    {
        let _ = fs::create_dir_all(&self.tool_dir);
        progress_cb(
            5.0,
            "Checking GitHub for official SteamRE/DepotDownloader release...".into(),
        );

        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(60))
            .build();

        let url = format!(
            "https://api.github.com/repos/{}/releases/latest",
            DEPOTDOWNLOADER_REPO
        );
        let resp = agent
            .get(&url)
            .set("User-Agent", "LuaToolsLinux/2.1")
            .call()
            .ok()?;
        let rel = resp.into_json::<Value>().ok()?;
        let tag = rel
            .get("tag_name")
            .and_then(|v| v.as_str())
            .unwrap_or("latest");
        let assets = rel.get("assets").and_then(|v| v.as_array())?;

        let has_system_dotnet = which_in_path("dotnet").is_some();
        let chosen = if has_system_dotnet {
            assets
                .iter()
                .find(|a| {
                    let n = a
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    n.ends_with(".zip") && n.contains("framework")
                })
                .or_else(|| Self::pick_linux_x64_asset(assets))
        } else {
            Self::pick_linux_x64_asset(assets)
        };

        let asset = chosen?;
        let dl_url = asset.get("browser_download_url").and_then(|v| v.as_str())?;
        let aname = asset
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("depotdownloader.zip");
        let zip_path = self.tool_dir.join(aname);

        progress_cb(10.0, format!("Downloading DepotDownloader {}...", tag));
        let dl_resp = agent.get(dl_url).call().ok()?;
        stream_response_to_file(dl_resp, &zip_path, &mut |frac, msg| {
            progress_cb((10.0 + frac * 80.0).clamp(10.0, 90.0), msg);
        })
        .ok()?;

        progress_cb(92.0, "Extracting archive...".into());
        if let Ok(zf) = File::open(&zip_path) {
            if let Ok(mut archive) = ZipArchive::new(zf) {
                let _ = archive.extract(&self.tool_dir);
            }
        }
        let _ = fs::remove_file(&zip_path);

        let exe = Self::scan_dir_for_bin(&self.tool_dir)?;
        let _ = Self::build_invocation(&exe);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = fs::write(
            self.tool_dir.join("meta.json"),
            serde_json::to_string_pretty(&json!({
                "version": tag,
                "repo": DEPOTDOWNLOADER_REPO,
                "last_check": now,
                "exe_path": exe,
            }))
            .unwrap_or_default(),
        );
        progress_cb(100.0, format!("DepotDownloader {} ready", tag));
        Some(exe)
    }

    fn pick_linux_x64_asset(assets: &[Value]) -> Option<&Value> {
        assets
            .iter()
            .find(|a| {
                let n = a
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                n.ends_with(".zip")
                    && n.contains("linux")
                    && !n.contains("arm")
                    && !n.contains("musl")
            })
            .or_else(|| {
                assets.iter().find(|a| {
                    let n = a
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    n.ends_with(".zip") && n.contains("linux") && !n.contains("arm")
                })
            })
    }

    pub fn resolve_keys(
        steam_inst: Option<&SteamInstallation>,
        sls: Option<&SlssteamService>,
        bridge: Option<&crate::services::steam_bridge_service::SteamBridgeService>,
        app_id: u32,
    ) -> BTreeMap<u32, String> {
        let mut keys = BTreeMap::new();
        if let Some(sls) = sls {
            keys = sls.config.depot_keys.clone();
        }
        if let Some(bridge) = bridge {
            if let Ok(mut store) = bridge.ticket_store.lock() {
                if let Ok(tickets) = store.get(app_id) {
                    if let Some(key) = tickets.app_ticket {
                        // AppTicket is stored as hex, use it as depot key for the app itself
                        keys.insert(app_id, key);
                    }
                }
            }
        }
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

    /// Writes depot keys to a temporary file for DepotDownloader -depotkeys support
    pub fn write_depot_keys_file(keys: &BTreeMap<u32, String>) -> Option<PathBuf> {
        if keys.is_empty() {
            return None;
        }
        let dir = std::env::temp_dir().join("luatools_depot_keys");
        let _ = fs::create_dir_all(&dir);
        let file_path = dir.join("depot_keys.txt");
        let mut content = String::new();
        for (depot_id, key) in keys {
            content.push_str(&format!("{} {}\n", depot_id, key));
        }
        if fs::write(&file_path, content).is_ok() {
            Some(file_path)
        } else {
            None
        }
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
            .find(|p| p.is_file())
    }

    /// Opens an interactive terminal window running `DepotDownloader -qr -remember-password`
    /// (or `-username <user> -remember-password`) so the user can authenticate their Steam
    /// session once and reuse cached credentials during background downloads.
    pub fn launch_interactive_login(
        &self,
        custom_tool_path: Option<&str>,
        steam_username: &str,
    ) -> Result<String, String> {
        let exe = self
            .ensure_tool(custom_tool_path, |_, _| {})
            .ok_or_else(|| "Could not locate or download DepotDownloader".to_string())?;
        let prefix = Self::build_invocation(&exe)?;

        let mut shell_cmd = prefix
            .iter()
            .map(|s| shell_quote(s))
            .collect::<Vec<_>>()
            .join(" ");
        let clean_user = steam_username.trim();
        if !clean_user.is_empty() && !clean_user.eq_ignore_ascii_case("anonymous") {
            shell_cmd.push_str(&format!(
                " -username {} -remember-password",
                shell_quote(clean_user)
            ));
        } else {
            shell_cmd.push_str(" -qr -remember-password");
        }
        let full_script = format!(
            "DOTNET_EnableWriteXorExecute=0 DOTNET_EnableDiagnostics=0 {} ; echo ''; read -rp 'Press Enter to close...' _",
            shell_cmd
        );

        let terminals: &[(&str, &[&str])] = &[
            ("ptyxis", &["--", "bash", "-lc"]),
            ("kgx", &["--", "bash", "-lc"]),
            ("gnome-terminal", &["--", "bash", "-lc"]),
            ("konsole", &["-e", "bash", "-lc"]),
            ("xfce4-terminal", &["-x", "bash", "-lc"]),
            ("kitty", &["bash", "-lc"]),
            ("alacritty", &["-e", "bash", "-lc"]),
            ("foot", &["bash", "-lc"]),
            ("xterm", &["-e", "bash", "-lc"]),
        ];

        for &(term, args) in terminals {
            if let Some(term_bin) = which_in_path(term) {
                let mut cmd = Command::new(term_bin);
                cmd.args(args).arg(&full_script);
                if cmd.spawn().is_ok() {
                    return Ok(format!(
                        "Opened {} for Steam login — complete QR / Steam Guard once to cache your session.",
                        term
                    ));
                }
            }
        }

        Err(format!(
            "No desktop terminal found. Run manually: {}",
            shell_cmd
        ))
    }

    pub fn download_depot<F>(
        &self,
        app_id: u32,
        selection: &DepotSelection,
        download_dir: &Path,
        opts: &DepotDownloadOptions,
        mut progress_cb: F,
    ) -> DepotRunResult
    where
        F: FnMut(f32, String),
    {
        let custom_opt = opts.custom_tool_path.trim();
        let Some(exe) = self.ensure_tool(
            (!custom_opt.is_empty()).then_some(custom_opt),
            &mut progress_cb,
        ) else {
            return DepotRunResult {
                depot_id: selection.depot_id,
                ok: false,
                error: Some("DepotDownloader not found (check internet or Settings)".into()),
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
        let bridge = self.bridge_service.as_ref().map(|b| b.lock().unwrap());
        let depot_keys = Self::resolve_keys(
            self.steam_installation.as_ref(),
            self.slssteam_service.as_ref(),
            bridge.as_ref().map(|b| &**b),
            app_id,
        );
        let keys_file = if !depot_keys.is_empty() {
            let keys_path = std::env::temp_dir().join(format!(
                "luatools_depotkeys_{}_{}.txt",
                std::process::id(),
                selection.depot_id
            ));
            let content = depot_keys
                .iter()
                .map(|(id, key)| format!("{};{}", id, key))
                .collect::<Vec<_>>()
                .join("\n");
            if fs::write(&keys_path, content).is_ok() {
                let _ = fs::set_permissions(&keys_path, fs::Permissions::from_mode(0o600));
                Some(keys_path)
            } else {
                None
            }
        } else {
            None
        };

        let is_mod = Self::is_mod_binary(&exe);
        let dl_args = Self::build_download_args_for_tool(
            target_app,
            selection,
            download_dir,
            &opts.steam_username,
            keys_file.as_deref(),
            is_mod,
        );

        let mut cmd = Command::new(&prefix[0]);
        for p in &prefix[1..] {
            cmd.arg(p);
        }
        cmd.args(&dl_args);

        // Critical Fedora 45 / headless desktop .NET runtime guards:
        // 1. Null stdin prevents Console.ReadKey / password prompt InvalidOperationException -> SIGABRT
        // 2. W^X=0 & Diagnostics=0 prevent .NET Native/CoreCLR crashes on glibc 2.44 / SELinux
        // 3. RLIMIT_CORE=0 + DbgEnableMiniDump=0 stop systemd-coredump / Crash Reporter storms
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("DOTNET_EnableWriteXorExecute", "0")
            .env("DOTNET_EnableDiagnostics", "0")
            .env("DOTNET_DbgEnableMiniDump", "0")
            .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
            .env("DOTNET_SYSTEM_GLOBALIZATION_INVARIANT", "1");
        disable_core_dumps(&mut cmd);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                if let Some(ref kp) = keys_file {
                    let _ = fs::remove_file(kp);
                }
                return DepotRunResult {
                    depot_id: selection.depot_id,
                    ok: false,
                    error: Some(e.to_string()),
                };
            }
        };

        let stderr_handle = child.stderr.take().map(|stderr| {
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                let mut lines = Vec::new();
                for line in reader.lines().map_while(Result::ok) {
                    let clean = line.trim();
                    if !clean.is_empty() {
                        lines.push(clean.to_string());
                    }
                }
                lines
            })
        });

        let pct_re = Regex::new(r"(\d{1,3}\.\d{2})%").unwrap();
        let mut last_stdout = String::new();
        let mut recent_stdout = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                let clean = line.trim().to_string();
                if clean.is_empty() {
                    continue;
                }
                last_stdout = clean.clone();
                recent_stdout.push(clean.clone());
                if recent_stdout.len() > 6 {
                    recent_stdout.remove(0);
                }
                let pct = pct_re
                    .captures(&clean)
                    .and_then(|c| c.get(1))
                    .and_then(|m| m.as_str().parse::<f32>().ok())
                    .unwrap_or(-1.0);
                progress_cb(pct, clean);
            }
        }

        let stderr_lines = stderr_handle
            .and_then(|h| h.join().ok())
            .unwrap_or_default();

        let wait_res = child.wait();
        if let Some(ref kp) = keys_file {
            let _ = fs::remove_file(kp);
        }

        match wait_res {
            Ok(status) if status.success() => DepotRunResult {
                depot_id: selection.depot_id,
                ok: true,
                error: None,
            },
            Ok(status) => {
                let combined = recent_stdout
                    .iter()
                    .chain(stderr_lines.iter())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" | ");
                let detail = summarize_depotdownloader_failure(
                    status.code(),
                    status.signal(),
                    &last_stdout,
                    &stderr_lines,
                    &combined,
                );
                DepotRunResult {
                    depot_id: selection.depot_id,
                    ok: false,
                    error: Some(detail),
                }
            }
            Err(e) => DepotRunResult {
                depot_id: selection.depot_id,
                ok: false,
                error: Some(e.to_string()),
            },
        }
    }
}

fn summarize_depotdownloader_failure(
    exit_code: Option<i32>,
    signal: Option<i32>,
    last_stdout: &str,
    stderr_lines: &[String],
    combined: &str,
) -> String {
    let low = combined.to_ascii_lowercase();
    if low.contains("no subscription")
        || low.contains("accessdenied")
        || low.contains("access denied")
        || low.contains("not available from this account")
        || low.contains("401")
    {
        return "Steam requires an account that owns this depot — set your Steam Username and click 'Sign In (QR)' first.".into();
    }
    if low.contains("invalidpassword")
        || low.contains("accountlogondenied")
        || low.contains("steam guard")
        || low.contains("twofactor")
        || low.contains("console.read")
        || low.contains("invalidoperationexception")
    {
        return "Steam session expired or needs Steam Guard — click 'Sign In (QR)' to refresh saved credentials.".into();
    }
    let meaningful_err = stderr_lines
        .iter()
        .rev()
        .find(|l| !l.starts_with("at ") && !l.starts_with("---"))
        .cloned()
        .unwrap_or_else(|| last_stdout.to_string());
    if let Some(sig) = signal {
        if meaningful_err.is_empty() {
            format!(
                "DepotDownloader terminated by signal {} (install 'dotnet-runtime-8.0' or sign in via QR)",
                sig
            )
        } else {
            format!("Signal {}: {}", sig, meaningful_err)
        }
    } else if meaningful_err.is_empty() {
        format!("Exited with code {}", exit_code.unwrap_or(1))
    } else {
        meaningful_err
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn disable_core_dumps(cmd: &mut Command) {
    #[repr(C)]
    struct Rlimit {
        rlim_cur: u64,
        rlim_max: u64,
    }
    extern "C" {
        fn setrlimit(resource: c_int, rlim: *const Rlimit) -> c_int;
    }
    use std::ffi::c_int;
    const RLIMIT_CORE: c_int = 4;
    unsafe {
        cmd.pre_exec(|| {
            let zero = Rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            let _ = setrlimit(RLIMIT_CORE, &zero);
            Ok(())
        });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_args_omit_unsupported_flags_and_anonymous_username() {
        let sel = DepotSelection {
            depot_id: 3321461,
            manifest_id: Some("1669007755731719796".into()),
            manifest_path: Some(PathBuf::from("/tmp/3321461_1669007755731719796.manifest")),
            size: 1024,
            from_app_id: None,
            language: None,
            os: None,
            is_shared: false,
            is_dlc: false,
        };
        let args = DepotDownloaderService::build_download_args(
            3321460,
            &sel,
            Path::new("/tmp/out"),
            "",
            None,
        );
        assert!(!args.iter().any(|a| a == "-depotkey" || a == "-depotkeys"));
        assert!(!args.iter().any(|a| a == "-manifestfile"));
        assert!(!args.iter().any(|a| a == "-username"));
        assert!(args.windows(2).any(|w| w == ["-max-downloads", "8"]));
        assert!(args
            .windows(2)
            .any(|w| w == ["-manifest", "1669007755731719796"]));
    }

    #[test]
    fn download_args_include_remember_password_and_depotkeys_when_present() {
        let sel = DepotSelection {
            depot_id: 731,
            manifest_id: None,
            manifest_path: None,
            size: 0,
            from_app_id: Some(730),
            language: None,
            os: None,
            is_shared: false,
            is_dlc: false,
        };
        let keys_file = Path::new("/tmp/luatools_depotkeys.txt");
        let args = DepotDownloaderService::build_download_args(
            730,
            &sel,
            Path::new("/tmp/cs2"),
            "my_steam_user",
            Some(keys_file),
        );
        assert!(args.windows(2).any(|w| w == ["-username", "my_steam_user"]));
        assert!(args.iter().any(|a| a == "-remember-password"));
        assert!(args
            .windows(2)
            .any(|w| w == ["-depotkeys", "/tmp/luatools_depotkeys.txt"]));
    }
}
