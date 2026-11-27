//! Official `SteamRE/DepotDownloader` integration — binary/DLL discovery, GitHub release
//! installer, Fedora/.NET runtime crash hardening (`stdin=null`, `W^X=0`, `RLIMIT_CORE=0`),
//! optional `-remember-password` / `-qr` Steam session login, and streaming depot downloads.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde_json::{json, Value};
use zip::ZipArchive;

use crate::models::config::default_luatools_data_dir;
use crate::models::steam::SteamInstallation;
use crate::parsers::lua_parser::LuaFileParser;
use crate::parsers::manifest_binary::{read_manifest_file, ManifestFile, ManifestMetadata};
use crate::services::api_client::HubcapService;
use crate::services::donate_keys::DonateKeysService;
use crate::services::file_hash::FileHashService;
use crate::services::proxied_file_downloader::ProxiedFileDownloader;
use crate::services::slssteam_service::SlssteamService;

static KEYS_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn pct_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(\d{1,3}\.\d{2})%").unwrap())
}

fn is_valid_depot_key(key: &str) -> bool {
    let k = key.trim();
    k.len() == 64 && k.chars().all(|c| c.is_ascii_hexdigit())
}

struct TempFileGuard(PathBuf);

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[derive(Debug, Clone)]
pub struct DepotSelection {
    pub depot_id: u32,
    pub manifest_id: Option<String>,
    pub manifest_path: Option<PathBuf>,
    pub size: u64,
    pub from_app_id: Option<u32>,
    pub language: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
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
    pub hubcap_api_key: Option<String>,
    // LT-67, LT-68: DepotDownloaderMod defaults with concurrency/timeout
    pub max_concurrent_downloads: Option<usize>,
    pub download_timeout_secs: Option<u64>,
}

impl DepotDownloadOptions {
    /// Returns the effective max concurrent downloads (default: 3)
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent_downloads.unwrap_or(3)
    }

    /// Returns the effective download timeout (default: 10 minutes = 600s)
    pub fn download_timeout(&self) -> Duration {
        Duration::from_secs(self.download_timeout_secs.unwrap_or(600))
    }
}

const DEPOTDOWNLOADER_MOD_REPO: &str = "SteamAutoCracks/DepotDownloaderMod";
const DEPOTDOWNLOADER_REPO: &str = "SteamRE/DepotDownloader";
const TOOL_CHECK_INTERVAL_SECS: u64 = 6 * 3600;

#[derive(Debug, Clone)]
pub struct DepotDownloaderService {
    pub tool_dir: PathBuf,
    pub steam_installation: Option<SteamInstallation>,
    pub slssteam_service: Option<SlssteamService>,
    pub bridge_service: Option<
        std::sync::Arc<std::sync::Mutex<crate::services::steam_bridge_service::SteamBridgeService>>,
    >,
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

    pub fn with_bridge(
        &mut self,
        bridge: crate::services::steam_bridge_service::SteamBridgeService,
    ) {
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
                .join("depotdownloadermod"),
            default_luatools_data_dir()
                .join("tools")
                .join("depotdownloader"),
            home.join(".local/share/LuaTools/depotdownloadermod"),
            home.join(".local/share/LuaTools/depotdownloader"),
            home.join("Applications"),
        ];

        for d in &search_dirs {
            if let Some(found) = Self::scan_dir_for_bin(d) {
                return Some(found);
            }
        }

        which_in_path("DepotDownloaderMod")
            .or_else(|| which_in_path("depotdownloadermod"))
            .or_else(|| which_in_path("DepotDownloader"))
            .or_else(|| which_in_path("depotdownloader"))
    }

    fn scan_dir_for_bin(dir: &Path) -> Option<PathBuf> {
        Self::scan_dir_for_bin_depth(dir, 0)
    }

    fn scan_dir_for_bin_depth(dir: &Path, depth: usize) -> Option<PathBuf> {
        if depth > 4 || !dir.is_dir() {
            return None;
        }
        // When system `dotnet` is installed (e.g. Fedora 44/45 with native glibc 2.44 / OpenSSL 3.5),
        // prefer `DepotDownloader.dll` / `DepotDownloaderMod.dll` so it runs on the host .NET runtime instead of an older bundled runtime.
        let has_system_dotnet = which_in_path("dotnet").is_some();
        let candidates: &[&str] = if has_system_dotnet {
            &[
                "DepotDownloaderMod.dll",
                "DepotDownloader.dll",
                "DepotDownloaderMod",
                "DepotDownloader",
                "depotdownloader",
                "DepotDownloaderMod.exe",
                "DepotDownloader.exe",
            ]
        } else {
            &[
                "DepotDownloaderMod",
                "DepotDownloader",
                "depotdownloader",
                "DepotDownloaderMod.dll",
                "DepotDownloader.dll",
                "DepotDownloaderMod.exe",
                "DepotDownloader.exe",
            ]
        };

        for name in candidates {
            let direct = dir.join(name);
            if direct.is_file() {
                return Some(direct);
            }
        }
        if let Ok(entries) = fs::read_dir(dir) {
            let mut subdirs = Vec::new();
            for e in entries.flatten() {
                if let Ok(ft) = e.file_type() {
                    let is_dir = ft.is_dir() || (ft.is_symlink() && e.path().is_dir());
                    if is_dir {
                        subdirs.push(e.path());
                    }
                }
            }
            subdirs.sort();
            for sub in subdirs {
                if let Some(found) = Self::scan_dir_for_bin_depth(&sub, depth + 1) {
                    return Some(found);
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
        let name_match = tool_path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase().contains("mod"))
            .unwrap_or(false);
        if name_match {
            return true;
        }
        tool_path
            .parent()
            .and_then(|p| p.file_name())
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
            true,
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

        // Add -os and -arch flags if specified in selection
        if let Some(ref os) = selection.os {
            args.push("-os".to_string());
            args.push(os.clone());
        }
        if let Some(ref arch) = selection.arch {
            args.push("-arch".to_string());
            args.push(arch.clone());
        }

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
            if let Some(mp) = selection
                .manifest_path
                .as_deref()
                .filter(|p| !p.as_os_str().is_empty())
            {
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
        } else {
            // Only use -qr for anonymous login; -remember-password requires -username or explicit QR flow
            // DepotDownloader requires -username when using -remember-password
            args.push("-qr".to_string());
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
                    if (repo == DEPOTDOWNLOADER_MOD_REPO || repo == DEPOTDOWNLOADER_REPO)
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
        let downloader = ProxiedFileDownloader::new().with_timeout(Duration::from_secs(90));
        let has_system_dotnet = which_in_path("dotnet").is_some();

        // LT-67: Prefer DepotDownloaderMod first (supports -manifestfile & -depotkeys), falling back to SteamRE/DepotDownloader
        for repo in [DEPOTDOWNLOADER_MOD_REPO, DEPOTDOWNLOADER_REPO] {
            progress_cb(
                5.0,
                format!("Checking GitHub for {} release...", repo),
            );
            let url = format!("https://api.github.com/repos/{}/releases/latest", repo);
            let Ok(rel_txt) = downloader.download_string_sync(&url) else {
                continue;
            };
            let Ok(rel) = serde_json::from_str::<Value>(&rel_txt) else {
                continue;
            };
            let tag = rel
                .get("tag_name")
                .and_then(|v| v.as_str())
                .unwrap_or("latest");
            let Some(assets) = rel.get("assets").and_then(|v| v.as_array()) else {
                continue;
            };

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

            let Some(asset) = chosen else {
                continue;
            };
            let Some(dl_url) = asset.get("browser_download_url").and_then(|v| v.as_str()) else {
                continue;
            };
            let raw_aname = asset
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("depotdownloader.zip");
            let safe_aname = Path::new(raw_aname)
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|s| !s.is_empty() && *s != "." && *s != "..")
                .unwrap_or("depotdownloader.zip");
            let zip_path = self.tool_dir.join(safe_aname);

            progress_cb(10.0, format!("Downloading {} {}...", repo, tag));
            let dl_label = format!("Downloading {} {}...", repo, tag);
            let mut dl_cb = |frac: f64| {
                progress_cb((10.0 + (frac as f32) * 80.0).clamp(10.0, 90.0), dl_label.clone());
            };
            if downloader
                .download_file_sync(dl_url, &zip_path, Some(&mut dl_cb))
                .is_err()
            {
                let _ = fs::remove_file(&zip_path);
                continue;
            }

            // LT-67 / LT-49: Verify GitHub release asset SHA-256 digest when provided
            if let Some(digest) = asset.get("digest").and_then(|v| v.as_str()) {
                let fhs = FileHashService::new();
                if let Err(e) = fhs.verify_sha256_digest(&zip_path, safe_aname, digest) {
                    log::warn!("DepotDownloader asset digest check failed ({}): {}", repo, e);
                    let _ = fs::remove_file(&zip_path);
                    continue;
                }
            }

            progress_cb(92.0, "Extracting archive...".into());
            let extract_ok = File::open(&zip_path)
                .ok()
                .and_then(|zf| ZipArchive::new(zf).ok())
                .and_then(|mut archive| archive.extract(&self.tool_dir).ok())
                .is_some();
            let _ = fs::remove_file(&zip_path);
            if !extract_ok {
                continue;
            }

            let Some(exe) = Self::scan_dir_for_bin(&self.tool_dir) else {
                continue;
            };
            let _ = Self::build_invocation(&exe);
            let fhs = FileHashService::new();
            let exe_sha = fhs
                .hash_file(
                    &exe,
                    exe.file_name().and_then(|s| s.to_str()).unwrap_or("DepotDownloader"),
                )
                .sha256;
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
                    "sha256": exe_sha,
                }))
                .unwrap_or_default(),
            );
            progress_cb(100.0, format!("{} {} ready", repo, tag));
            return Some(exe);
        }

        None
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
            for (did, k) in &sls.config.depot_keys {
                if is_valid_depot_key(k) {
                    keys.insert(*did, k.trim().to_ascii_lowercase());
                }
            }
        }
        if let Some(bridge) = bridge {
            // Pull explicit depot keys configured on the Steam bridge overrides
            let ov = bridge.snapshot_overrides();
            for (did, k) in ov.depot_keys {
                if is_valid_depot_key(&k) {
                    keys.insert(did, k.trim().to_ascii_lowercase());
                }
            }
            if let Ok(mut store) = bridge.ticket_store.lock() {
                if let Ok(tickets) = store.get(app_id) {
                    if let Some(key) = tickets.app_ticket {
                        // Only include if it is actually a 64-char hex depot key
                        if is_valid_depot_key(&key) {
                            keys.insert(app_id, key.trim().to_ascii_lowercase());
                        }
                    }
                }
            }
        }
        if let Some(inst) = steam_inst {
            let config_vdf = inst.root.join("config/config.vdf");
            if config_vdf.is_file() {
                for (did, k) in DonateKeysService::extract_keys_from_vdf(&config_vdf) {
                    if let Ok(depot_id_u32) = did.parse::<u32>() {
                        if is_valid_depot_key(&k) {
                            keys.insert(depot_id_u32, k.trim().to_ascii_lowercase());
                        }
                    }
                }
            }
            let lua_path = inst.stplug_in_path.join(format!("{}.lua", app_id));
            if let Some(lua) = LuaFileParser::parse_file(&lua_path, app_id) {
                let disabled_ids: std::collections::HashSet<u32> =
                    lua.disabled_entries.iter().map(|de| de.id).collect();
                for e in lua.entries {
                    if disabled_ids.contains(&e.id) {
                        continue;
                    }
                    if let Some(k) = e.key {
                        if is_valid_depot_key(&k) {
                            keys.insert(e.id, k.trim().to_ascii_lowercase());
                        }
                    }
                }
            }
        }
        keys
    }

    /// Writes depot keys to a private (`0o600`) temporary file in DepotDownloader's `depotid;hexkey` format.
    pub fn write_depot_keys_file(keys: &BTreeMap<u32, String>) -> Option<PathBuf> {
        let valid_lines: Vec<String> = keys
            .iter()
            .filter(|(_, key)| is_valid_depot_key(key))
            .map(|(depot_id, key)| format!("{};{}", depot_id, key.trim().to_ascii_lowercase()))
            .collect();
        if valid_lines.is_empty() {
            return None;
        }
        let seq = KEYS_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let file_path = std::env::temp_dir().join(format!(
            "luatools_depotkeys_{}_{}.txt",
            std::process::id(),
            seq
        ));
        let mut content = valid_lines.join("\n");
        content.push('\n');
        if fs::write(&file_path, content).is_ok() {
            let _ = fs::set_permissions(&file_path, fs::Permissions::from_mode(0o600));
            Some(file_path)
        } else {
            None
        }
    }

    /// Reads and decodes real `ManifestMetadata` from a local binary `.manifest` file (LT-60, LT-61).
    pub fn inspect_manifest(manifest_path: &Path) -> Option<ManifestMetadata> {
        read_manifest_file(manifest_path)
            .ok()
            .or_else(|| ManifestFile::try_read(manifest_path))
    }

    /// Resolves or downloads the manifest for a depot, consuming & verifying decoded `ManifestMetadata` (LT-44, LT-60, LT-61).
    pub fn resolve_manifests(
        steam_inst: Option<&SteamInstallation>,
        depot_id: u32,
        manifest_id: &str,
        hubcap_api_key: Option<&str>,
    ) -> Option<PathBuf> {
        if let Some(existing) = Self::resolve_manifest_path(steam_inst, depot_id, manifest_id) {
            if let Some(meta) = Self::inspect_manifest(&existing) {
                if meta.depot_id == 0 || meta.depot_id == depot_id {
                    log::info!(
                        "Using cached manifest {} (depot={}, gid={}, files={}, size={} bytes)",
                        existing.display(),
                        meta.depot_id,
                        meta.manifest_gid,
                        meta.file_count,
                        meta.total_size
                    );
                    return Some(existing);
                }
                log::warn!(
                    "Cached manifest {} has mismatched depot_id {} (expected {}); re-downloading",
                    existing.display(),
                    meta.depot_id,
                    depot_id
                );
            } else {
                log::warn!(
                    "Cached manifest {} failed binary protobuf decode; removing and re-downloading",
                    existing.display()
                );
            }
            let _ = fs::remove_file(&existing);
        }
        let inst = steam_inst?;
        let clean_gid = manifest_id.trim();
        let gid: u64 = clean_gid.parse().ok()?;
        let target_dir = inst.root.join("depotcache");
        let dest = target_dir.join(format!("{}_{}.manifest", depot_id, clean_gid));
        match HubcapService::download_raw_manifest_to_file_verified(
            depot_id,
            gid,
            &dest,
            hubcap_api_key,
            None,
        ) {
            Ok(meta) => {
                log::info!(
                    "Downloaded verified manifest {} (depot={}, gid={}, files={}, size={} bytes)",
                    dest.display(),
                    meta.depot_id,
                    meta.manifest_gid,
                    meta.file_count,
                    meta.total_size
                );
                Some(dest)
            }
            Err(e) => {
                log::warn!("Manifest download/validation failed for {}_{}: {}", depot_id, clean_gid, e);
                None
            }
        }
    }

    /// Validate AES-256 decryption key against encrypted filenames in a manifest binary (LT-62).
    pub fn validate_depot_aes_key(manifest_path: &Path, hex_key: &str) -> bool {
        let Ok(bytes) = hex::decode(hex_key.trim()) else {
            return false;
        };
        if bytes.len() != 32 {
            return false;
        }
        let mut key_arr = [0u8; 32];
        key_arr.copy_from_slice(&bytes);
        match crate::parsers::manifest_binary::validate_aes_key(manifest_path, &key_arr) {
            Ok(valid) => valid,
            Err(_) => ManifestFile::key_looks_valid(manifest_path, &key_arr),
        }
    }

    pub fn resolve_manifest_path(
        steam_inst: Option<&SteamInstallation>,
        depot_id: u32,
        manifest_id: &str,
    ) -> Option<PathBuf> {
        let inst = steam_inst?;
        let clean_gid = manifest_id.trim();
        if clean_gid.is_empty() || !clean_gid.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let fname = format!("{}_{}.manifest", depot_id, clean_gid);
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

    /// Builds interactive Steam login flags for `DepotDownloader` (LT-68).
    /// Omits `-remember-password` on QR-only login when no username is set.
    pub fn build_interactive_login_args(steam_username: &str) -> Vec<String> {
        let clean_user = steam_username.trim();
        if !clean_user.is_empty() && !clean_user.eq_ignore_ascii_case("anonymous") {
            vec![
                "-username".to_string(),
                clean_user.to_string(),
                "-remember-password".to_string(),
            ]
        } else {
            vec!["-qr".to_string()]
        }
    }

    /// Opens an interactive terminal window running `DepotDownloader -qr`
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
        for arg in Self::build_interactive_login_args(steam_username) {
            shell_cmd.push(' ');
            shell_cmd.push_str(&shell_quote(&arg));
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

    /// Downloads multiple depots with bounded concurrency (`opts.max_concurrent()`) and cooperative
    /// `Arc<AtomicBool>` cancellation (LT-68). Results preserve the input order of `selections`.
    pub fn download_depots_batch<F>(
        &self,
        app_id: u32,
        selections: &[DepotSelection],
        dest_root: &Path,
        opts: &DepotDownloadOptions,
        cancel: Option<Arc<AtomicBool>>,
        progress_cb: F,
    ) -> Vec<DepotRunResult>
    where
        F: Fn(u32, f32, String) + Send + Sync,
    {
        if selections.is_empty() {
            return Vec::new();
        }
        let concurrency = opts.max_concurrent().max(1).min(selections.len());
        let queue: Mutex<VecDeque<(usize, DepotSelection)>> = Mutex::new(
            selections
                .iter()
                .cloned()
                .enumerate()
                .collect(),
        );
        let results: Mutex<Vec<Option<DepotRunResult>>> =
            Mutex::new(vec![None; selections.len()]);
        let progress_ref = &progress_cb;

        thread::scope(|scope| {
            for _ in 0..concurrency {
                let q = &queue;
                let res_out = &results;
                let cancel_clone = cancel.clone();
                scope.spawn(move || loop {
                    let next_item = {
                        let mut guard = q.lock().unwrap_or_else(|e| e.into_inner());
                        guard.pop_front()
                    };
                    let Some((idx, sel)) = next_item else {
                        break;
                    };
                    let depot_id = sel.depot_id;
                    let sub_dir = dest_root.join(format!("depot_{}", depot_id));
                    let run_res = self.download_depot_cancellable(
                        app_id,
                        &sel,
                        &sub_dir,
                        opts,
                        cancel_clone.clone(),
                        |pct, msg| progress_ref(depot_id, pct, msg),
                    );
                    let mut out = res_out.lock().unwrap_or_else(|e| e.into_inner());
                    out[idx] = Some(run_res);
                });
            }
        });

        results
            .into_inner()
            .unwrap_or_else(|e| e.into_inner())
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                r.unwrap_or_else(|| DepotRunResult {
                    depot_id: selections[i].depot_id,
                    ok: false,
                    error: Some("Depot download worker exited unexpectedly".into()),
                })
            })
            .collect()
    }

    pub fn download_depot<F>(
        &self,
        app_id: u32,
        selection: &DepotSelection,
        download_dir: &Path,
        opts: &DepotDownloadOptions,
        progress_cb: F,
    ) -> DepotRunResult
    where
        F: FnMut(f32, String),
    {
        self.download_depot_cancellable(app_id, selection, download_dir, opts, None, progress_cb)
    }

    /// Downloads a single depot with manifest protobuf metadata inspection (LT-60/61),
    /// AES-256 key pre-validation (LT-62), timeout watchdog, and cooperative cancellation +
    /// partial temp cleanup (LT-68).
    pub fn download_depot_cancellable<F>(
        &self,
        app_id: u32,
        selection: &DepotSelection,
        download_dir: &Path,
        opts: &DepotDownloadOptions,
        cancel: Option<Arc<AtomicBool>>,
        mut progress_cb: F,
    ) -> DepotRunResult
    where
        F: FnMut(f32, String),
    {
        if cancel
            .as_deref()
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(false)
        {
            return DepotRunResult {
                depot_id: selection.depot_id,
                ok: false,
                error: Some("Download cancelled".into()),
            };
        }

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

        let mut effective_selection = selection.clone();
        if effective_selection.manifest_path.is_none() {
            if let Some(ref gid) = effective_selection.manifest_id {
                let resolved = Self::resolve_manifests(
                    self.steam_installation.as_ref(),
                    effective_selection.depot_id,
                    gid,
                    opts.hubcap_api_key.as_deref(),
                );
                effective_selection.manifest_path = resolved;
            }
        }
        let target_app = effective_selection.from_app_id.unwrap_or(app_id);
        let depot_keys = {
            let bridge = self
                .bridge_service
                .as_ref()
                .map(|b| b.lock().unwrap_or_else(|e| e.into_inner()));
            Self::resolve_keys(
                self.steam_installation.as_ref(),
                self.slssteam_service.as_ref(),
                bridge.as_deref(),
                app_id,
            )
            // `bridge` mutex guard is dropped immediately here before spawning the child process
        };

        // LT-60 / LT-61 / LT-62: Consume decoded manifest metadata & validate AES-256 depot key before spawning
        if let Some(ref mp) = effective_selection.manifest_path {
            if let Some(meta) = Self::inspect_manifest(mp) {
                if meta.depot_id != 0 && meta.depot_id != effective_selection.depot_id {
                    return DepotRunResult {
                        depot_id: selection.depot_id,
                        ok: false,
                        error: Some(format!(
                            "Local manifest {} belongs to depot {}, not {}",
                            mp.display(),
                            meta.depot_id,
                            effective_selection.depot_id
                        )),
                    };
                }
                progress_cb(
                    0.0,
                    format!(
                        "Verified manifest GID {} ({} file(s), {} bytes)",
                        meta.manifest_gid, meta.file_count, meta.total_size
                    ),
                );
                if let Some(hex_key) = depot_keys.get(&effective_selection.depot_id) {
                    if !Self::validate_depot_aes_key(mp, hex_key) {
                        return DepotRunResult {
                            depot_id: selection.depot_id,
                            ok: false,
                            error: Some(format!(
                                "Depot {} decryption key failed AES-256 validation against encrypted manifest {}",
                                effective_selection.depot_id,
                                mp.display()
                            )),
                        };
                    }
                }
            }
        }

        let keys_guard = Self::write_depot_keys_file(&depot_keys).map(TempFileGuard);

        let is_mod = Self::is_mod_binary(&exe);
        let dl_args = Self::build_download_args_for_tool(
            target_app,
            &effective_selection,
            download_dir,
            &opts.steam_username,
            keys_guard.as_ref().map(|g| g.0.as_path()),
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

        // LT-68: Read stdout on a worker thread so the timeout watchdog can interrupt hung processes!
        let (out_tx, out_rx) = std::sync::mpsc::channel::<String>();
        let stdout_handle = child.stdout.take().map(|stdout| {
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    let clean = line.trim().to_string();
                    if !clean.is_empty() && out_tx.send(clean).is_err() {
                        break;
                    }
                }
            })
        });

        let pct_re = pct_regex();
        let mut last_stdout = String::new();
        let mut recent_stdout = Vec::new();
        let timeout = opts.download_timeout();
        let start = std::time::Instant::now();

        let wait_res = loop {
            while let Ok(clean) = out_rx.try_recv() {
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

            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => {
                    if cancel
                        .as_deref()
                        .map(|c| c.load(Ordering::Relaxed))
                        .unwrap_or(false)
                    {
                        let _ = child.kill();
                        let _ = child.wait();
                        cleanup_partial_depot_temp(download_dir);
                        break Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "Download cancelled",
                        ));
                    }
                    if start.elapsed() >= timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        cleanup_partial_depot_temp(download_dir);
                        break Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!("Download timed out after {} seconds", timeout.as_secs()),
                        ));
                    }
                    match out_rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(clean) => {
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
                        Err(_) => {}
                    }
                }
                Err(e) => break Err(e),
            }
        };

        if let Some(h) = stdout_handle {
            let _ = h.join();
        }
        while let Ok(clean) = out_rx.try_recv() {
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
        let stderr_lines = stderr_handle
            .and_then(|h| h.join().ok())
            .unwrap_or_default();
        drop(keys_guard);

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
    // Handle SteamKit2.AsyncJobFailedException
    if low.contains("asyncjobfailedexception") || low.contains("async job failed") {
        return "DepotDownloader async job failed — try clicking 'Sign In (QR)' to authenticate with Steam, then retry.".into();
    }
    // Handle authentication failures
    if low.contains("authenticate") || low.contains("authentication failed") || low.contains("failed to authenticate") {
        return "Failed to authenticate with Steam — set your Steam Username and click 'Sign In (QR)' first.".into();
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

fn cleanup_partial_depot_temp(download_dir: &Path) {
    let staging = download_dir.join(".DepotDownloader");
    if staging.is_dir() {
        let _ = fs::remove_dir_all(&staging);
    }
    if let Ok(entries) = fs::read_dir(download_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file() {
                if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                    if name.ends_with(".tmp") || name.ends_with(".part") {
                        let _ = fs::remove_file(&p);
                    }
                }
            }
        }
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn disable_core_dumps(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(|| {
            let zero = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            let _ = libc::setrlimit(libc::RLIMIT_CORE, &zero);
            Ok(())
        });
    }
}

pub fn which_in_path(binary: &str) -> Option<PathBuf> {
    if binary.is_empty() || binary.contains('/') {
        return None;
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let full = dir.join(binary);
        if let Ok(meta) = fs::metadata(&full) {
            if meta.is_file() && (meta.permissions().mode() & 0o111 != 0) {
                return Some(full);
            }
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
            arch: None,
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
        assert!(args
            .windows(2)
            .any(|w| w == ["-manifestfile", "/tmp/3321461_1669007755731719796.manifest"]));
        assert!(!args.iter().any(|a| a == "-username"));
        // -remember-password requires -username, so for anonymous login we only use -qr
        assert!(!args.iter().any(|a| a == "-remember-password"), "-remember-password should NOT be used without -username");
        assert!(args.iter().any(|a| a == "-qr"), "-qr should be present for anonymous login");
        assert!(args.windows(2).any(|w| w == ["-max-downloads", "8"]));
        assert!(args
            .windows(2)
            .any(|w| w == ["-manifest", "1669007755731719796"]));
    }

    #[test]
    fn scan_dir_for_bin_depth_finds_depotdownloadermod_nested() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("sub/net8.0");
        fs::create_dir_all(&nested).unwrap();
        let mod_bin = nested.join("DepotDownloaderMod.dll");
        fs::write(&mod_bin, b"MZ").unwrap();

        let found = DepotDownloaderService::scan_dir_for_bin_depth(dir.path(), 0);
        assert_eq!(found, Some(mod_bin));
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
            arch: None,
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

    #[test]
    fn write_depot_keys_file_uses_semicolon_format_and_private_permissions() {
        let mut keys = BTreeMap::new();
        keys.insert(1245621, "AB".repeat(32));
        keys.insert(999, "not-a-valid-key".into());

        let path = DepotDownloaderService::write_depot_keys_file(&keys).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let _ = fs::remove_file(&path);

        assert_eq!(content, format!("1245621;{}\n", "ab".repeat(32)));
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn resolve_keys_reads_bridge_depot_keys_and_rejects_invalid_manifest_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let bridge = crate::services::steam_bridge_service::SteamBridgeService::with_paths(
            None,
            Some(tmp.path().to_path_buf()),
        );
        let key = "ef".repeat(32);
        bridge.set_depot_key(55501, key.clone());

        let resolved = DepotDownloaderService::resolve_keys(None, None, Some(&bridge), 55500);
        assert_eq!(resolved.get(&55501).map(String::as_str), Some(key.as_str()));

        let inst = SteamInstallation {
            root: tmp.path().to_path_buf(),
            config_path: tmp.path().join("config"),
            stplug_in_path: tmp.path().join("config/stplug-in"),
            libraries: Vec::new(),
        };
        assert!(DepotDownloaderService::resolve_manifest_path(
            Some(&inst),
            55501,
            "../../etc/passwd"
        )
        .is_none());
    }

    #[test]
    fn interactive_login_args_omit_remember_password_for_qr_and_batch_respects_cancel() {
        let qr_args = DepotDownloaderService::build_interactive_login_args("");
        assert_eq!(qr_args, vec!["-qr"]);
        assert!(!qr_args.iter().any(|a| a == "-remember-password"));

        let user_args = DepotDownloaderService::build_interactive_login_args("alice");
        assert_eq!(user_args, vec!["-username", "alice", "-remember-password"]);

        let tmp = tempfile::tempdir().unwrap();
        let svc = DepotDownloaderService::new(Some(tmp.path().join("dd")));
        let cancel = Arc::new(AtomicBool::new(true));
        let sel = DepotSelection {
            depot_id: 731,
            manifest_id: None,
            manifest_path: None,
            size: 0,
            from_app_id: None,
            language: None,
            os: None,
            arch: None,
            is_shared: false,
            is_dlc: false,
        };
        let opts = DepotDownloadOptions {
            max_concurrent_downloads: Some(2),
            ..Default::default()
        };
        let res = svc.download_depots_batch(
            730,
            &[sel],
            tmp.path(),
            &opts,
            Some(cancel),
            |_, _, _| {},
        );
        assert_eq!(res.len(), 1);
        assert!(!res[0].ok);
        assert_eq!(res[0].error.as_deref(), Some("Download cancelled"));
    }
}
