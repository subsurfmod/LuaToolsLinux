//! WiredService — native Rust installer & manager for ACCELA (`ciscosweater/enter-the-wired`)
//! and `SLSsteam` (`AceSLS/SLSsteam`) without blind `curl | bash` execution.
//!
//! Handles GitHub release resolution with proper `User-Agent` headers, `.tar.gz` / `.zip` /
//! `.tar.zst` / `.7z` extraction, ZipSlip path-traversal protection, `~/.local/share/ACCELA`
//! and `~/.local/share/SLSsteam` installation, `ACCELA.conf` & `SLSsteam/config.yaml` defaults,
//! and a `~/.local/bin/steam-sls` `LD_AUDIT` launcher helper.
use crate::services::file_hash::{FileHash, FileHashService};

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use flate2::read::GzDecoder;
use serde_json::Value;
use tar::Archive;
use ureq::AgentBuilder;
use zip::ZipArchive;

use crate::services::api_client::stream_response_to_file;
use crate::services::depot_downloader::which_in_path;
use crate::VERSION;

const ACCELA_REPO: &str = "ciscosweater/enter-the-wired";
const ACCELA_FALLBACK_URL: &str =
    "https://github.com/ciscosweater/enter-the-wired/releases/download/latest/deps.tar.gz";
const SLSSTEAM_REPO: &str = "AceSLS/SLSsteam";

#[derive(Debug, Clone)]
pub struct WiredStatus {
    pub accela_installed: bool,
    pub slssteam_installed: bool,
    pub accela_dir: PathBuf,
    pub slssteam_dir: PathBuf,
    pub slssteam_sha256: Option<String>,
    pub library_inject_sha256: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WiredService {
    pub share_dir: PathBuf,
    pub config_dir: PathBuf,
    pub bin_dir: PathBuf,
    pub steam_root: Option<PathBuf>,
    uses_system_home: bool,
}

impl Default for WiredService {
    fn default() -> Self {
        Self::new(None)
    }
}

impl WiredService {
    /// Creates a `WiredService`. Pass `None` to manage `~/.local/share/{ACCELA,SLSsteam}` and
    /// `~/.config`, or `Some(sandbox_root)` in unit/QA tests to stay 100% isolated from `$HOME`.
    pub fn new(sandbox_root: Option<PathBuf>) -> Self {
        match sandbox_root {
            Some(root) => Self {
                share_dir: root.join("share"),
                config_dir: root.join("config"),
                bin_dir: root.join("bin"),
                steam_root: None,
                uses_system_home: false,
            },
            None => {
                let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
                Self {
                    share_dir: home.join(".local/share"),
                    config_dir: home.join(".config"),
                    bin_dir: home.join(".local/bin"),
                    steam_root: None,
                    uses_system_home: true,
                }
            }
        }
    }

    pub fn with_steam_root(mut self, steam_root: Option<PathBuf>) -> Self {
        self.steam_root = steam_root;
        self
    }

    pub fn accela_dir(&self) -> PathBuf {
        self.share_dir.join("ACCELA")
    }

    pub fn slssteam_dir(&self) -> PathBuf {
        self.share_dir.join("SLSsteam")
    }

    pub fn is_accela_installed(&self) -> bool {
        dir_has_entries(&self.accela_dir())
    }

    pub fn is_slssteam_installed(&self) -> bool {
        if self.slssteam_dir().join("SLSsteam.so").is_file() {
            return true;
        }
        if self.uses_system_home {
            if let Some(home) = dirs::home_dir() {
                if home
                    .join(".var/app/com.valvesoftware.Steam/.local/share/SLSsteam/SLSsteam.so")
                    .is_file()
                {
                    return true;
                }
            }
        }
        false
    }

    pub fn is_fully_installed(&self) -> bool {
        self.is_accela_installed() && self.is_slssteam_installed()
    }

    /// Cached SHA-256 lookup keyed by `(path, file_size, mtime_secs)` so UI frames never re-hash
    /// multi-MB `.so` files from disk on every repaint (LT-65).
    fn cached_sha256(path: &Path, label: &str) -> Option<String> {
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock};
        use std::time::UNIX_EPOCH;

        static CACHE: OnceLock<Mutex<HashMap<PathBuf, (u64, u64, String)>>> = OnceLock::new();
        let meta = fs::metadata(path).ok()?;
        if !meta.is_file() {
            return None;
        }
        let len = meta.len();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if let Ok(guard) = cache.lock() {
            if let Some((c_len, c_mtime, hash)) = guard.get(path) {
                if *c_len == len && *c_mtime == mtime {
                    return Some(hash.clone());
                }
            }
        }
        let computed = FileHash::try_new(path, label).ok()?.sha256;
        if let Ok(mut guard) = cache.lock() {
            if guard.len() > 64 {
                guard.clear();
            }
            guard.insert(path.to_path_buf(), (len, mtime, computed.clone()));
        }
        Some(computed)
    }

    /// Computes the SHA-256 hash of the installed `SLSsteam.so` unlocker binary (LT-65).
    pub fn slssteam_sha256(&self) -> Option<String> {
        let p = self.slssteam_dir().join("SLSsteam.so");
        Self::cached_sha256(&p, "SLSsteam.so")
    }

    /// Computes the SHA-256 hash of the installed `library-inject.so` bridge library (LT-65).
    pub fn library_inject_sha256(&self) -> Option<String> {
        let p = self.slssteam_dir().join("library-inject.so");
        Self::cached_sha256(&p, "library-inject.so")
    }

    pub fn status(&self) -> WiredStatus {
        WiredStatus {
            accela_installed: self.is_accela_installed(),
            slssteam_installed: self.is_slssteam_installed(),
            accela_dir: self.accela_dir(),
            slssteam_dir: self.slssteam_dir(),
            slssteam_sha256: self.slssteam_sha256(),
            library_inject_sha256: self.library_inject_sha256(),
        }
    }

    pub fn launch_accela(&self) -> Result<String, String> {
        let dir = self.accela_dir();
        if !self.is_accela_installed() {
            return Err("ACCELA is not installed yet. Click Install ACCELA first.".into());
        }
        let candidates = ["ACCELA", "accela", "run.sh", "ACCELA.sh", "ACCELA.AppImage"];
        let exe = find_executable_in(&dir, &candidates).ok_or_else(|| {
            format!(
                "Installed ACCELA folder ({}) has no recognized launcher binary.",
                dir.display()
            )
        })?;
        Command::new(&exe)
            .current_dir(&dir)
            .spawn()
            .map_err(|e| format!("Failed to launch {}: {}", exe.display(), e))?;
        Ok(format!("Launched ACCELA ({})", exe.display()))
    }

    pub fn install_accela<F>(&self, mut progress_cb: F) -> Result<PathBuf, String>
    where
        F: FnMut(f32, String),
    {
        progress_cb(0.05, "Checking ACCELA release on GitHub...".into());
        let agent = AgentBuilder::new().timeout(Duration::from_secs(90)).build();
        let ua = format!("LuaToolsLinux/{}", VERSION);

        let api_url = format!(
            "https://api.github.com/repos/{}/releases/latest",
            ACCELA_REPO
        );
        let mut chosen_url = ACCELA_FALLBACK_URL.to_string();
        let mut chosen_name = "deps.tar.gz".to_string();
        let mut tag = "latest".to_string();

        if let Ok(resp) = agent.get(&api_url).set("User-Agent", &ua).call() {
            if let Ok(rel) = resp.into_json::<Value>() {
                if let Some(t) = rel.get("tag_name").and_then(|v| v.as_str()) {
                    tag = t.to_string();
                }
                if let Some(assets) = rel.get("assets").and_then(|v| v.as_array()) {
                    if let Some(asset) = assets.iter().find(|a| {
                        let n = a
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        n.starts_with("deps")
                            || n.ends_with(".tar.gz")
                            || n.ends_with(".appimage")
                            || n.ends_with(".zip")
                    }) {
                        if let Some(u) = asset.get("browser_download_url").and_then(|v| v.as_str())
                        {
                            chosen_url = u.to_string();
                        }
                        if let Some(n) = asset.get("name").and_then(|v| v.as_str()) {
                            chosen_name = n.to_string();
                        }
                    }
                }
            }
        }

        let tmp_dir = std::env::temp_dir().join(format!("luatools_accela_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp_dir);
        fs::create_dir_all(&tmp_dir).map_err(|e| e.to_string())?;
        let archive_path = tmp_dir.join(&chosen_name);

        progress_cb(0.15, format!("Downloading ACCELA ({})...", tag));
        let dl_resp = agent
            .get(&chosen_url)
            .set("User-Agent", &ua)
            .call()
            .map_err(|e| {
                let _ = fs::remove_dir_all(&tmp_dir);
                format!("ACCELA download failed: {}", e)
            })?;

        if let Err(e) = stream_response_to_file(dl_resp, &archive_path, &mut |p, m| {
            progress_cb(0.15 + p * 0.65, m);
        }) {
            let _ = fs::remove_dir_all(&tmp_dir);
            return Err(e);
        }

        progress_cb(
            0.82,
            "Installing ACCELA into ~/.local/share/ACCELA...".into(),
        );
        let install_res = self.finish_accela_from_archive(&archive_path, &tmp_dir);
        let _ = fs::remove_dir_all(&tmp_dir);
        let installed_dir = install_res?;

        progress_cb(
            1.0,
            format!("ACCELA installed at {}", installed_dir.display()),
        );
        Ok(installed_dir)
    }

    pub fn finish_accela_from_archive(
        &self,
        archive_path: &Path,
        work_dir: &Path,
    ) -> Result<PathBuf, String> {
        let dest = self.accela_dir();
        fs::create_dir_all(&dest).map_err(|e| e.to_string())?;

        let lower = archive_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        if lower.ends_with(".appimage") {
            let target = dest.join("ACCELA.AppImage");
            fs::copy(archive_path, &target).map_err(|e| e.to_string())?;
            let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o755));
            self.ensure_accela_conf();
            return Ok(dest);
        }

        let extract_dir = work_dir.join("extracted");
        fs::create_dir_all(&extract_dir).map_err(|e| e.to_string())?;
        extract_archive(archive_path, &extract_dir)?;

        let installer_bin = extract_dir.join("ACCELAINSTALL");
        if installer_bin.is_file() && self.uses_system_home {
            let _ = fs::set_permissions(&installer_bin, fs::Permissions::from_mode(0o755));
            let st = Command::new(&installer_bin)
                .current_dir(&extract_dir)
                .status()
                .map_err(|e| format!("Failed to run ACCELAINSTALL: {}", e))?;
            if !st.success() {
                return Err(format!("ACCELAINSTALL exited with status {}", st));
            }
        } else {
            copy_dir_recursive(&extract_dir, &dest)?;
        }

        self.ensure_accela_conf();
        Ok(dest)
    }

    fn ensure_accela_conf(&self) {
        let conf_dir = self.config_dir.join("Tachibana Labs");
        let conf_path = conf_dir.join("ACCELA.conf");
        if conf_path.exists() {
            return;
        }
        if fs::create_dir_all(&conf_dir).is_ok() {
            let default_conf = "[General]\n\
                auto_skip_single_choice=true\n\
                library_mode=true\n\
                max_downloads=16\n\
                sls_config_management=true\n\
                slssteam_mode=true\n\
                use_steamless=true\n";
            let _ = fs::write(conf_path, default_conf);
        }
    }

    pub fn install_slssteam<F>(&self, mut progress_cb: F) -> Result<PathBuf, String>
    where
        F: FnMut(f32, String),
    {
        progress_cb(0.05, "Checking SLSsteam release on GitHub...".into());
        let agent = AgentBuilder::new().timeout(Duration::from_secs(90)).build();
        let ua = format!("LuaToolsLinux/{}", VERSION);
        let url = format!(
            "https://api.github.com/repos/{}/releases/latest",
            SLSSTEAM_REPO
        );

        let resp = agent
            .get(&url)
            .set("User-Agent", &ua)
            .call()
            .map_err(|e| format!("Failed to query GitHub ({}): {}", SLSSTEAM_REPO, e))?;
        let release: Value = resp
            .into_json()
            .map_err(|e| format!("Failed to parse SLSsteam release JSON: {}", e))?;

        let tag = release
            .get("tag_name")
            .and_then(|v| v.as_str())
            .unwrap_or("latest");
        let assets = release
            .get("assets")
            .and_then(|v| v.as_array())
            .ok_or_else(|| "No assets found in SLSsteam release.".to_string())?;

        let has_7z = find_7z_bin().is_some();
        let has_zstd = which_in_path("tar").is_some() && which_in_path("zstd").is_some();

        // Select the best non-debug archive supported by installed host tools
        let chosen = select_slssteam_asset(assets, has_7z, has_zstd)
            .ok_or_else(|| "No compatible SLSsteam release archive found.".to_string())?;

        let download_url = chosen
            .get("browser_download_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing browser_download_url on SLSsteam asset.".to_string())?;
        let file_name = chosen
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("SLSsteam-release.tar.zst");

        let tmp_dir =
            std::env::temp_dir().join(format!("luatools_slssteam_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp_dir);
        fs::create_dir_all(&tmp_dir).map_err(|e| e.to_string())?;
        let archive_path = tmp_dir.join(file_name);

        progress_cb(
            0.15,
            format!("Downloading SLSsteam {} ({})...", tag, file_name),
        );
        let dl_resp = agent
            .get(download_url)
            .set("User-Agent", &ua)
            .call()
            .map_err(|e| {
                let _ = fs::remove_dir_all(&tmp_dir);
                format!("SLSsteam download failed: {}", e)
            })?;

        if let Err(e) = stream_response_to_file(dl_resp, &archive_path, &mut |p, m| {
            progress_cb(0.15 + p * 0.65, m);
        }) {
            let _ = fs::remove_dir_all(&tmp_dir);
            return Err(e);
        }

        progress_cb(
            0.85,
            "Extracting SLSsteam.so and configuring launcher...".into(),
        );
        let install_res = self.finish_slssteam_from_archive(&archive_path, &tmp_dir);
        let _ = fs::remove_dir_all(&tmp_dir);
        let so_path = install_res?;

        progress_cb(1.0, format!("SLSsteam installed ({})", so_path.display()));
        Ok(so_path)
    }

    pub fn finish_slssteam_from_archive(
        &self,
        archive_path: &Path,
        work_dir: &Path,
    ) -> Result<PathBuf, String> {
        let extract_dir = work_dir.join("extracted");
        fs::create_dir_all(&extract_dir).map_err(|e| e.to_string())?;
        extract_archive(archive_path, &extract_dir)?;

        let found_so = find_file_recursive(&extract_dir, "SLSsteam.so")
            .ok_or_else(|| "Extracted SLSsteam archive did not contain SLSsteam.so.".to_string())?;

        let sls_dir = self.slssteam_dir();
        fs::create_dir_all(&sls_dir).map_err(|e| e.to_string())?;

        let target_so = sls_dir.join("SLSsteam.so");
        fs::copy(&found_so, &target_so).map_err(|e| e.to_string())?;
        let _ = fs::set_permissions(&target_so, fs::Permissions::from_mode(0o755));
        // LT-49 & LT-65: Cryptographic SHA-256 active unlocker verification
        let fhs = FileHashService::new();
        let _ = fhs.hash_file(&target_so, "SLSsteam.so");

        if let Some(inj) = find_file_recursive(&extract_dir, "library-inject.so") {
            let target_inj = sls_dir.join("library-inject.so");
            if fs::copy(&inj, &target_inj).is_ok() {
                let _ = fs::set_permissions(&target_inj, fs::Permissions::from_mode(0o755));
            }
        }

        // Seed default ~/.config/SLSsteam/config.yaml if user doesn't have one yet
        let cfg_dir = self.config_dir.join("SLSsteam");
        let cfg_path = cfg_dir.join("config.yaml");
        if !cfg_path.exists() && fs::create_dir_all(&cfg_dir).is_ok() {
            if let Some(tpl_cfg) = find_file_recursive(&extract_dir, "config.yaml") {
                let _ = fs::copy(tpl_cfg, &cfg_path);
            } else {
                let _ = fs::write(
                    &cfg_path,
                    "PlayNotOwnedGames: true\nLimitDownloadsToSteamLibrary: false\n",
                );
            }
        }

        // Install a clean user-local `steam-sls` wrapper script in `~/.local/bin/steam-sls`
        let _ = self.write_steam_sls_wrapper(&target_so, &sls_dir);

        Ok(target_so)
    }

    /// Generates and installs a clean user-local `steam-sls` wrapper script in `~/.local/bin/steam-sls`.
    /// Includes optional background auto-start `luatools --minimized --tray-locked &` (LT-20).
    pub fn write_steam_sls_wrapper(
        &self,
        target_so: &Path,
        sls_dir: &Path,
    ) -> Result<PathBuf, String> {
        fs::create_dir_all(&self.bin_dir)
            .map_err(|e| format!("Failed to create bin dir: {}", e))?;
        let wrapper = self.bin_dir.join("steam-sls");

        // Build LD_PRELOAD with our integration library
        let integration_lib = "/home/tejas/.local/lib/luatools/libsteam_integration.so";

        // Build LD_AUDIT for SLSsteam (only library-inject.so to avoid 32-bit crashes with SLSsteam.so)
        let audit = if sls_dir.join("library-inject.so").is_file() {
            format!(
                "{}:{}",
                sls_dir.join("library-inject.so").display(),
                target_so.display()
            )
        } else {
            target_so.display().to_string()
        };

        let script = format!(
            "#!/usr/bin/env bash\n\
             # Auto-start LuaTools companion in background minimized to tray if available (LT-20)\n\
             if command -v luatools >/dev/null 2>&1; then\n\
                 luatools --minimized --tray-locked &\n\
             elif [ -x \"$HOME/.local/bin/luatools\" ]; then\n\
                 \"$HOME/.local/bin/luatools\" --minimized --tray-locked &\n\
             fi\n\
             # Set target platform for LuaTools depot filtering (auto-detect or override via env)\n\
             export LUATOOLS_TARGET_OS=\"${{LUATOOLS_TARGET_OS:-linux}}\"\n\
             export LUATOOLS_TARGET_ARCH=\"${{LUATOOLS_TARGET_ARCH:-x86_64}}\"\n\
             # Include LuaTools integration library via LD_PRELOAD\n\
             export LD_PRELOAD=\"{integration_lib}${{LD_PRELOAD:+:$LD_PRELOAD}}\"\n\
             # SLSsteam via LD_AUDIT (only library-inject.so to avoid 32-bit crashes with SLSsteam.so)\n\
             export LD_AUDIT=\"{audit}\"\n\
             exec steam \"$@\"\n",
            integration_lib = integration_lib,
            audit = audit
        );
        fs::write(&wrapper, script)
            .map_err(|e| format!("Failed to write steam-sls wrapper: {}", e))?;
        let _ = fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755));
        Ok(wrapper)
    }

    pub fn install_all<F>(&self, mut progress_cb: F) -> Result<(), String>
    where
        F: FnMut(f32, String),
    {
        progress_cb(
            0.0,
            "Starting Wired suite installation (ACCELA + SLSsteam)...".into(),
        );
        self.install_accela(|p, m| progress_cb(p * 0.5, m))?;
        self.install_slssteam(|p, m| progress_cb(0.5 + p * 0.5, m))?;
        progress_cb(
            1.0,
            "Wired installation complete (ACCELA + SLSsteam ready).".into(),
        );
        Ok(())
    }

    pub fn uninstall_all(&self) -> Result<(), String> {
        if self.accela_dir().exists() {
            fs::remove_dir_all(self.accela_dir())
                .map_err(|e| format!("Failed to remove ACCELA: {}", e))?;
        }
        if self.slssteam_dir().exists() {
            fs::remove_dir_all(self.slssteam_dir())
                .map_err(|e| format!("Failed to remove SLSsteam: {}", e))?;
        }
        let wrapper = self.bin_dir.join("steam-sls");
        if wrapper.exists() {
            let _ = fs::remove_file(wrapper);
        }
        Ok(())
    }
}

fn select_slssteam_asset(assets: &[Value], has_7z: bool, has_zstd: bool) -> Option<&Value> {
    let name_lower = |a: &Value| -> String {
        a.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase()
    };

    // 1. Native Rust-extractable tar.gz / zip (non-debug)
    if let Some(a) = assets.iter().find(|a| {
        let n = name_lower(a);
        !n.contains("debug")
            && (n.ends_with(".tar.gz") || n.ends_with(".tgz") || n.ends_with(".zip"))
    }) {
        return Some(a);
    }

    // 2. SLSsteam-Any-release.7z when 7z/7za/7zz is installed
    if has_7z {
        if let Some(a) = assets.iter().find(|a| {
            let n = name_lower(a);
            !n.contains("debug") && n.ends_with(".7z")
        }) {
            return Some(a);
        }
    }

    // 3. SLSsteam-Arch-release-x86_64.pkg.tar.zst when tar + zstd are installed
    if has_zstd {
        if let Some(a) = assets.iter().find(|a| {
            let n = name_lower(a);
            !n.contains("debug") && n.ends_with(".tar.zst")
        }) {
            return Some(a);
        }
    }

    // 4. Fallback to any non-debug archive so extract_archive can report the exact missing tool
    assets.iter().find(|a| {
        let n = name_lower(a);
        !n.contains("debug")
            && (n.ends_with(".tar.zst")
                || n.ends_with(".7z")
                || n.ends_with(".tar.xz")
                || n.ends_with(".tar.bz2"))
    })
}

pub fn find_7z_bin() -> Option<PathBuf> {
    for cmd in ["7z", "7zz", "7za"] {
        if let Some(p) = which_in_path(cmd) {
            return Some(p);
        }
    }
    None
}

pub fn extract_archive(archive: &Path, dest: &Path) -> Result<(), String> {
    let lower = archive
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    if lower.ends_with(".zip") {
        return extract_zip_safe(archive, dest);
    }
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        let file = File::open(archive).map_err(|e| e.to_string())?;
        let gz = GzDecoder::new(file);
        let mut tar = Archive::new(gz);
        for entry in tar.entries().map_err(|e| e.to_string())? {
            let mut entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path().map_err(|e| e.to_string())?;
            if path.components().any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::RootDir)) {
                return Err(format!("Rejected unsafe path-traversal entry in tar: {}", path.display()));
            }
            entry.unpack_in(dest).map_err(|e| e.to_string())?;
        }
        return Ok(());
    }
    if lower.ends_with(".tar.zst") || lower.ends_with(".tar.xz") || lower.ends_with(".tar.bz2") {
        let mut cmd = Command::new("tar");
        if lower.ends_with(".tar.zst") {
            cmd.arg("--zstd");
        }
        let out = cmd
            .arg("-xf")
            .arg(archive)
            .arg("-C")
            .arg(dest)
            .output()
            .map_err(|e| format!("Failed to invoke tar to extract {}: {}", lower, e))?;
        if !out.status.success() {
            return Err(format!(
                "tar failed extracting {}: {}",
                archive.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        return Ok(());
    }
    if lower.ends_with(".7z") {
        let bin = find_7z_bin().ok_or_else(|| {
            "7-Zip ('7z') is required to extract .7z archives (install p7zip / 7zip or zstd)."
                .to_string()
        })?;
        // Pre-screen 7z file listing for path traversal entries
        let list_out = Command::new(&bin)
            .arg("l")
            .arg("-ba")
            .arg(archive)
            .output()
            .map_err(|e| format!("Failed to inspect 7z archive: {}", e))?;
        if list_out.status.success() {
            let listing = String::from_utf8_lossy(&list_out.stdout);
            for line in listing.lines() {
                // Check for actual path traversal patterns (../ or ..\ as path components)
                // Not just ".." anywhere in the filename (e.g., "docs/LICENSE" contains ".." in "LICENSE")
                // Only flag if it's a path component: starts with ../ or ..\, or contains /../ or \..\ or ends with /.. or \..
                let is_traversal = line.starts_with("../")
                    || line.starts_with("..\\")
                    || line.contains("/../")
                    || line.contains("\\..\\")
                    || line.ends_with("/..")
                    || line.ends_with("\\..")
                    || line == ".."
                    || line == "../"
                    || line == "..\\";
                if is_traversal {
                    return Err(format!("Rejected unsafe path-traversal entry in 7z: {}", line.trim()));
                }
            }
        }
        let out = Command::new(bin)
            .arg("x")
            .arg("-y")
            .arg(format!("-o{}", dest.display()))
            .arg(archive)
            .output()
            .map_err(|e| format!("Failed to run 7z: {}", e))?;
        if !out.status.success() {
            return Err(format!(
                "7z extraction failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        return Ok(());
    }

    Err(format!("Unsupported archive format: {}", archive.display()))
}

fn extract_zip_safe(archive: &Path, dest: &Path) -> Result<(), String> {
    let file = File::open(archive).map_err(|e| e.to_string())?;
    let mut zip = ZipArchive::new(file).map_err(|e| e.to_string())?;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let Some(rel) = entry.enclosed_name().map(|p| p.to_path_buf()) else {
            return Err(format!(
                "Rejected unsafe path-traversal entry in archive: {}",
                entry.name()
            ));
        };
        let path = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&path).map_err(|e| e.to_string())?;
        } else {
            if let Some(p) = path.parent() {
                fs::create_dir_all(p).map_err(|e| e.to_string())?;
            }
            let mut out = File::create(&path).map_err(|e| e.to_string())?;
            io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn dir_has_entries(dir: &Path) -> bool {
    fs::read_dir(dir)
        .ok()
        .and_then(|mut it| it.next())
        .is_some()
}

fn find_file_recursive(root: &Path, target_name: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if path.file_name().and_then(|s| s.to_str()) == Some(target_name) {
                return Some(path);
            }
        } else if path.is_dir() {
            if let Some(found) = find_file_recursive(&path, target_name) {
                return Some(found);
            }
        }
    }
    None
}

fn find_executable_in(root: &Path, names: &[&str]) -> Option<PathBuf> {
    for &n in names {
        if let Some(p) = find_file_recursive(root, n) {
            return Some(p);
        }
    }
    if let Ok(entries) = fs::read_dir(root) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file()
                && p.file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_ascii_lowercase().ends_with(".appimage"))
                    .unwrap_or(false)
            {
                return Some(p);
            }
        }
    }
    None
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(src).map_err(|e| e.to_string())?.flatten() {
        let p = entry.path();
        let target = dst.join(entry.file_name());
        if p.is_dir() {
            copy_dir_recursive(&p, &target)?;
        } else {
            fs::copy(&p, &target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    #[test]
    fn installs_slssteam_and_accela_in_isolated_sandbox_and_rejects_zipslip() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = WiredService::new(Some(tmp.path().join("sandbox")));
        assert!(!svc.is_accela_installed());
        assert!(!svc.is_slssteam_installed());
        assert!(!svc.is_fully_installed());

        // 1. Build a synthetic SLSsteam release zip with nested bin/SLSsteam.so
        let sls_zip = tmp.path().join("SLSsteam-release.zip");
        {
            let f = File::create(&sls_zip).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            zw.start_file("SLSsteam/bin/SLSsteam.so", SimpleFileOptions::default())
                .unwrap();
            zw.write_all(b"\x7fELF_FAKE_SLSSTEAM_SO").unwrap();
            zw.start_file("SLSsteam/res/config.yaml", SimpleFileOptions::default())
                .unwrap();
            zw.write_all(b"PlayNotOwnedGames: true\n").unwrap();
            zw.finish().unwrap();
        }
        let work_sls = tmp.path().join("work_sls");
        let installed_so = svc
            .finish_slssteam_from_archive(&sls_zip, &work_sls)
            .unwrap();
        assert!(installed_so.is_file());
        assert!(svc.is_slssteam_installed());
        assert!(svc.bin_dir.join("steam-sls").is_file());
        let wrapper_content = fs::read_to_string(svc.bin_dir.join("steam-sls")).unwrap();
        assert!(wrapper_content.contains("luatools --minimized --tray-locked &"));
        assert!(svc.config_dir.join("SLSsteam/config.yaml").is_file());

        // 2. Build a synthetic ACCELA AppImage & install
        let appimg = tmp.path().join("ACCELA.AppImage");
        fs::write(&appimg, b"#!/bin/sh\necho ACCELA\n").unwrap();
        let work_acc = tmp.path().join("work_acc");
        svc.finish_accela_from_archive(&appimg, &work_acc).unwrap();
        assert!(svc.is_accela_installed());
        assert!(svc.is_fully_installed());
        assert!(svc.config_dir.join("Tachibana Labs/ACCELA.conf").is_file());

        // 3. Reject ZipSlip path traversal archive
        let bad_zip = tmp.path().join("evil.zip");
        {
            let f = File::create(&bad_zip).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            zw.start_file("../../escaped.sh", SimpleFileOptions::default())
                .unwrap();
            zw.write_all(b"boom").unwrap();
            zw.finish().unwrap();
        }
        assert!(extract_archive(&bad_zip, &tmp.path().join("out")).is_err());

        // 4. Uninstall cleans both directories and launcher wrapper
        svc.uninstall_all().unwrap();
        assert!(!svc.is_accela_installed());
        assert!(!svc.is_slssteam_installed());
        assert!(!svc.bin_dir.join("steam-sls").exists());
    }
}
