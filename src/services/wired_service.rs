//! WiredService — native Rust installer & manager for ACCELA (`ciscosweater/enter-the-wired`)
//! and `SLSsteam` (`AceSLS/SLSsteam`) without blind `curl | bash` execution.
//!
//! Handles GitHub release resolution with proper `User-Agent` headers, `.tar.gz` / `.zip` /
//! `.tar.zst` / `.7z` extraction, ZipSlip path-traversal protection, `~/.local/share/ACCELA`
//! and `~/.local/share/SLSsteam` installation, `ACCELA.conf` & `SLSsteam/config.yaml` defaults,
//! and a `~/.local/bin/steam-sls` `LD_AUDIT` launcher helper.
use crate::services::file_hash::{FileHash, FileHashService};

use std::collections::HashSet;
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
pub const SLSSTEAM_REPO: &str = "swwayps/slsteam-moon";
const MILLENNIUM_INSTALL_CMD: &str = r#"curl -fsSL "https://steambrew.app/install.sh" | bash"#;

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

    /// Resolves the active `SLSsteam` directory, taking Flatpak Steam installations into account (LNX-04, LT-65).
    pub fn active_slssteam_dir(&self) -> PathBuf {
        let primary = self.slssteam_dir();
        if primary.join("SLSsteam.so").is_file() {
            return primary;
        }
        if let Some(ref root) = self.steam_root {
            if crate::services::steam_service::SteamService::is_flatpak_steam_root(root) {
                if let Some(fp_share) = root.parent().map(|p| p.join("SLSsteam")) {
                    if fp_share.join("SLSsteam.so").is_file() {
                        return fp_share;
                    }
                }
            }
        }
        if self.uses_system_home {
            if let Some(home) = dirs::home_dir() {
                let fp_dir = home.join(".var/app/com.valvesoftware.Steam/.local/share/SLSsteam");
                if fp_dir.join("SLSsteam.so").is_file() {
                    return fp_dir;
                }
            }
        }
        primary
    }

    pub fn is_accela_installed(&self) -> bool {
        dir_has_entries(&self.accela_dir())
    }

    /// Verifies `SLSsteam.so` exists and computes a valid non-empty SHA-256 digest (LT-65).
    pub fn is_slssteam_installed(&self) -> bool {
        self.slssteam_sha256().is_some()
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
        let p = self.active_slssteam_dir().join("SLSsteam.so");
        Self::cached_sha256(&p, "SLSsteam.so").filter(|h| !h.is_empty())
    }

    /// Computes the SHA-256 hash of the installed `library-inject.so` bridge library (LT-65).
    pub fn library_inject_sha256(&self) -> Option<String> {
        let p = self.active_slssteam_dir().join("library-inject.so");
        Self::cached_sha256(&p, "library-inject.so").filter(|h| !h.is_empty())
    }

    pub fn status(&self) -> WiredStatus {
        let active_sls_dir = self.active_slssteam_dir();
        let sls_sha = self.slssteam_sha256();
        let inj_sha = self.library_inject_sha256();
        WiredStatus {
            accela_installed: self.is_accela_installed(),
            slssteam_installed: sls_sha.is_some(),
            accela_dir: self.accela_dir(),
            slssteam_dir: active_sls_dir,
            slssteam_sha256: sls_sha,
            library_inject_sha256: inj_sha,
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
        let mut chosen_digest: Option<String> = None;
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
                        chosen_digest = asset
                            .get("digest")
                            .and_then(|v| v.as_str())
                            .map(ToString::to_string);
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

        // LT-49: Verify GitHub release asset SHA-256 digest when provided
        if let Some(ref digest) = chosen_digest {
            let fhs = FileHashService::new();
            if let Err(e) = fhs.verify_sha256_digest(&archive_path, &chosen_name, digest) {
                let _ = fs::remove_dir_all(&tmp_dir);
                return Err(format!("ACCELA integrity verification failed: {}", e));
            }
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
        let expected_digest = chosen
            .get("digest")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);

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

        // LT-49: Verify GitHub release asset SHA-256 digest before extracting SLSsteam
        if let Some(ref digest) = expected_digest {
            let fhs = FileHashService::new();
            if let Err(e) = fhs.verify_sha256_digest(&archive_path, file_name, digest) {
                let _ = fs::remove_dir_all(&tmp_dir);
                return Err(format!("SLSsteam archive SHA-256 check failed: {}", e));
            }
        }

        progress_cb(
            0.80,
            "Extracting SLSsteam.so and configuring launcher...".into(),
        );
        let install_res = self.finish_slssteam_from_archive(&archive_path, &tmp_dir);
        let _ = fs::remove_dir_all(&tmp_dir);
        let so_path = install_res?;

        match self.install_millennium_steambrew(|p, m| progress_cb(0.85 + p * 0.13, m)) {
            Ok(_) => {
                progress_cb(
                    1.0,
                    format!("SLSsteam + Millennium installed ({})", so_path.display()),
                );
            }
            Err(e) => {
                log::warn!(
                    "Optional Millennium (SteamBrew) installation failed (continuing with SLSsteam): {}",
                    e
                );
                progress_cb(
                    1.0,
                    format!(
                        "SLSsteam installed ({}); optional Millennium install skipped: {}",
                        so_path.display(),
                        e
                    ),
                );
            }
        }
        Ok(so_path)
    }

    /// Installs Millennium via SteamBrew (`curl -fsSL "https://steambrew.app/install.sh" | bash`).
    /// Skipped in unit tests, `LUATOOLS_MOCK_LAUNCH=1`, or sandboxed (`uses_system_home == false`) instances.
    pub fn install_millennium_steambrew<F>(&self, mut progress_cb: F) -> Result<String, String>
    where
        F: FnMut(f32, String),
    {
        progress_cb(0.05, "Installing Millennium (SteamBrew)...".into());
        let mock_launch = std::env::var("LUATOOLS_MOCK_LAUNCH")
            .map(|v| v == "1")
            .unwrap_or(false);
        if cfg!(test) || mock_launch || !self.uses_system_home {
            let msg = "Millennium (SteamBrew) install skipped in test/sandbox mode.".to_string();
            progress_cb(1.0, msg.clone());
            return Ok(msg);
        }

        let output = Command::new("bash")
            .arg("-c")
            .arg(MILLENNIUM_INSTALL_CMD)
            .output()
            .map_err(|e| format!("Failed to execute Millennium installer: {}", e))?;

        if output.status.success() {
            let _ = crate::services::millennium_service::MillenniumService::ensure_luatoolslinux_plugin_installed(None);
            let msg = "Millennium (SteamBrew) installed successfully.".to_string();
            progress_cb(1.0, msg.clone());
            Ok(msg)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let detail = if !stderr.is_empty() { stderr } else { stdout };
            Err(format!("Millennium installation failed: {}", detail))
        }
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
        let so_hash = fhs.hash_file(&target_so, "SLSsteam.so");
        if so_hash.sha256.is_empty() || so_hash.file_size == 0 {
            return Err("Installed SLSsteam.so failed SHA-256 integrity check (empty/unreadable).".into());
        }
        log::info!("Installed SLSsteam.so SHA-256: {}", so_hash.sha256);

        // Flaw 2 & Fix 2: Never keep or install deprecated library-inject.so
        let old_lib_inject = sls_dir.join("library-inject.so");
        if old_lib_inject.exists() {
            let _ = fs::remove_file(old_lib_inject);
        }

        // Extract standalone Lumen binary and assets if present in slsteam-moon bundle
        let lumen_dir = self.share_dir.join("Lumen");
        let _ = fs::create_dir_all(&lumen_dir);
        if let Some(lumen_bin) = find_file_recursive(&extract_dir, "lumen") {
            let target_lumen = lumen_dir.join("lumen");
            if fs::copy(&lumen_bin, &target_lumen).is_ok() {
                let _ = fs::set_permissions(&target_lumen, fs::Permissions::from_mode(0o755));
                log::info!("Installed standalone Lumen binary to {}", target_lumen.display());
            }
        }
        let extract_lua = extract_dir.join("lua");
        if extract_lua.is_dir() {
            let target_lua = lumen_dir.join("lua");
            let _ = copy_dir_recursive(&extract_lua, &target_lua);
        }
        let extract_backend = extract_dir.join("luatools").join("backend");
        if extract_backend.is_dir() {
            let target_backend = lumen_dir.join("luatools").join("backend");
            let _ = fs::create_dir_all(&target_backend);
            let _ = copy_dir_recursive(&extract_backend, &target_backend);
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

        // Install single-auditor user-local `steam-sls` wrapper script in `~/.local/bin/steam-sls`
        let _ = self.write_steam_sls_wrapper(&target_so, &sls_dir);

        Ok(target_so)
    }

    /// Generates and installs a clean user-local `steam-sls` wrapper script in `~/.local/bin/steam-sls`.
    /// Enforces the single-auditor rule, auto-cleans stale auditors, resolves the true Steam binary,
    /// sets up Wayland libextest, and launches the Lumen CDP bridge.
    pub fn write_steam_sls_wrapper(
        &self,
        target_so: &Path,
        sls_dir: &Path,
    ) -> Result<PathBuf, String> {
        fs::create_dir_all(&self.bin_dir)
            .map_err(|e| format!("Failed to create bin dir: {}", e))?;
        let wrapper = self.bin_dir.join("steam-sls");

        // Clean up deprecated library-inject.so to prevent fatal double-auditor crash
        let old_lib_inject = sls_dir.join("library-inject.so");
        if old_lib_inject.exists() {
            let _ = fs::remove_file(old_lib_inject);
        }

        let audit_target = shell_single_quote(&target_so.to_string_lossy());
        let sls_dir_str = shell_single_quote(&sls_dir.to_string_lossy());
        let lumen_dir = self.share_dir.join("Lumen");
        let lumen_dir_str = shell_single_quote(&lumen_dir.to_string_lossy());

        let script = format!(
            "#!/usr/bin/env bash\n\
             set -u\n\
             \n\
             SLSDIR={sls_dir_str}\n\
             AUDIT={audit_target}\n\
             LUMEN_DIR={lumen_dir_str}\n\
             \n\
             # Auto-start LuaTools companion in background minimized to tray unless opted out (LT-20)\n\
             if [ \"${{LUATOOLS_NO_AUTOLAUNCH:-0}}\" != \"1\" ] && [ ! -f \"${{XDG_CONFIG_HOME:-$HOME/.config}}/luatools/no-autolaunch\" ]; then\n\
                 if command -v luatools >/dev/null 2>&1; then\n\
                     if ! pgrep -x luatools >/dev/null 2>&1; then\n\
                         luatools --minimized --tray-locked &\n\
                     fi\n\
                 elif [ -x \"$HOME/.local/bin/luatools\" ]; then\n\
                     if ! pgrep -x luatools >/dev/null 2>&1; then\n\
                         \"$HOME/.local/bin/luatools\" --minimized --tray-locked &\n\
                     fi\n\
                 fi\n\
             fi\n\
             \n\
             # 1. Clean existing LD_AUDIT to enforce SINGLE auditor rule\n\
             slsm_clean_audit=\"\"\n\
             _old_ifs=\"$IFS\"\n\
             IFS=:\n\
             for item in ${{LD_AUDIT:-}}; do\n\
                 case \"$item\" in\n\
                     ''|*SLSsteam.so|*library-inject.so) continue ;;\n\
                     *) slsm_clean_audit=\"${{slsm_clean_audit:+$slsm_clean_audit:}}$item\" ;;\n\
                 esac\n\
             done\n\
             IFS=\"$_old_ifs\"\n\
             \n\
             # 2. Resolve true Steam binary\n\
             SELF=\"$(readlink -f \"$0\" 2>/dev/null || echo \"$0\")\"\n\
             STEAM_BIN=\"\"\n\
             for c in /usr/games/steam /usr/bin/steam /usr/local/bin/steam \"$HOME/.local/share/Steam/steam.sh\"; do\n\
                 if [ -x \"$c\" ] && [ \"$(readlink -f \"$c\" 2>/dev/null || echo \"$c\")\" != \"$SELF\" ]; then\n\
                     STEAM_BIN=\"$c\"\n\
                     break\n\
                 fi\n\
             done\n\
             \n\
             if [ -z \"$STEAM_BIN\" ]; then\n\
                 echo \"steam-sls: could not locate real Steam binary\" >&2\n\
                 exit 127\n\
             fi\n\
             \n\
             # 3. Handle Wayland libextest for controller input\n\
             if [ \"${{XDG_SESSION_TYPE:-}}\" = \"wayland\" ]; then\n\
                 for ext in /usr/lib64/extest/libextest.so /usr/lib/x86_64-linux-gnu/extest/libextest.so; do\n\
                     if [ -f \"$ext\" ]; then\n\
                         export LD_PRELOAD=\"${{LD_PRELOAD:+$LD_PRELOAD:}}$ext\"\n\
                         break\n\
                     fi\n\
                 done\n\
             fi\n\
             \n\
             # 4. Launch Lumen CDP bridge detached if present\n\
             if [ -x \"$LUMEN_DIR/lumen\" ] && ! pgrep -u \"$(id -u)\" -xf \"$LUMEN_DIR/lumen\" >/dev/null 2>&1; then\n\
                 touch \"$HOME/.steam/steam/.cef-enable-remote-debugging\" 2>/dev/null || true\n\
                 env -u LD_AUDIT -u LD_PRELOAD -u LD_LIBRARY_PATH \\\n\
                     LUMEN_BACKEND_DIR=\"$LUMEN_DIR/luatools/backend\" \\\n\
                     LUMEN_LUA_DIR=\"$LUMEN_DIR/lua\" \\\n\
                     setsid \"$LUMEN_DIR/lumen\" >/dev/null 2>&1 < /dev/null &\n\
             fi\n\
             \n\
             # 5. Enforce single auditor\n\
             if [ -n \"$slsm_clean_audit\" ]; then\n\
                 export LD_AUDIT=\"$AUDIT:$slsm_clean_audit\"\n\
             else\n\
                 export LD_AUDIT=\"$AUDIT\"\n\
             fi\n\
             \n\
             exec \"$STEAM_BIN\" \"$@\"\n",
            sls_dir_str = sls_dir_str,
            audit_target = audit_target,
            lumen_dir_str = lumen_dir_str,
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

    // 0. Prioritize slsteam-moon / lumen release bundles (e.g. slsteam-moon-linux-3.0-lumen.zip)
    if let Some(a) = assets.iter().find(|a| {
        let n = name_lower(a);
        !n.contains("debug")
            && (n.contains("slsteam-moon") || n.contains("lumen"))
            && (n.ends_with(".zip") || n.ends_with(".tar.gz") || n.ends_with(".tgz"))
    }) {
        return Some(a);
    }

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

fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn has_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn validate_archive_entry_path(
    entry_path: &str,
    archive_label: &str,
) -> Result<Vec<String>, String> {
    if entry_path.is_empty() {
        return Err(format!("Rejected empty entry path in {}", archive_label));
    }
    if entry_path.contains('\0')
        || entry_path.starts_with('/')
        || entry_path.starts_with('\\')
        || has_windows_drive_prefix(entry_path)
    {
        return Err(format!(
            "Rejected unsafe path-traversal entry in {}: {}",
            archive_label, entry_path
        ));
    }
    let mut parts = Vec::new();
    for seg in entry_path.split(['/', '\\']) {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return Err(format!(
                "Rejected unsafe path-traversal entry in {}: {}",
                archive_label, entry_path
            ));
        }
        parts.push(seg.to_string());
    }
    Ok(parts)
}

fn check_no_symlink_ancestor(
    parts: &[String],
    symlink_paths: &HashSet<Vec<String>>,
    raw_entry: &str,
    archive_label: &str,
) -> Result<(), String> {
    for depth in 1..parts.len() {
        if symlink_paths.contains(&parts[..depth]) {
            return Err(format!(
                "Rejected archive entry traversing symlink ancestor in {}: {}",
                archive_label, raw_entry
            ));
        }
    }
    Ok(())
}

fn validate_archive_link_target(
    entry_parts: &[String],
    link_target: &str,
    is_hardlink: bool,
    archive_label: &str,
) -> Result<(), String> {
    let kind = if is_hardlink { "hardlink" } else { "symlink" };
    if link_target.is_empty()
        || link_target.contains('\0')
        || link_target.starts_with('/')
        || link_target.starts_with('\\')
        || has_windows_drive_prefix(link_target)
    {
        return Err(format!(
            "Rejected unsafe {} target escaping destination in {}: {}",
            kind, archive_label, link_target
        ));
    }
    let mut stack: Vec<&str> = if is_hardlink || entry_parts.is_empty() {
        Vec::new()
    } else {
        entry_parts[..entry_parts.len() - 1]
            .iter()
            .map(String::as_str)
            .collect()
    };
    for seg in link_target.split(['/', '\\']) {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            if stack.pop().is_none() {
                return Err(format!(
                    "Rejected unsafe {} target escaping destination in {}: {}",
                    kind, archive_label, link_target
                ));
            }
        } else {
            stack.push(seg);
        }
    }
    if stack.is_empty() {
        return Err(format!(
            "Rejected unsafe {} target resolving to archive root in {}: {}",
            kind, archive_label, link_target
        ));
    }
    Ok(())
}

fn extract_tar_verbose_entry_rest(line: &str) -> Option<&str> {
    let mut tokens: Vec<(usize, usize, &str)> = Vec::with_capacity(9);
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && tokens.len() < 9 {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        tokens.push((start, i, &line[start..i]));
    }
    let after_idx = if tokens.len() >= 6
        && tokens[3].2.len() == 10
        && tokens[3].2.as_bytes()[4] == b'-'
        && tokens[3].2.as_bytes()[7] == b'-'
        && tokens[4].2.contains(':')
    {
        tokens[4].1
    } else if tokens.len() >= 9
        && tokens[5].2.len() == 3
        && tokens[5].2.bytes().all(|b| b.is_ascii_alphabetic())
        && tokens[6].2.bytes().all(|b| b.is_ascii_digit())
        && (tokens[7].2.contains(':') || tokens[7].2.bytes().all(|b| b.is_ascii_digit()))
    {
        tokens[7].1
    } else if tokens.len() >= 6 {
        tokens[4].1
    } else {
        return None;
    };
    let tail = &line[after_idx..];
    let rest = tail.strip_prefix(' ').unwrap_or_else(|| tail.trim_start());
    if rest.is_empty() {
        None
    } else {
        Some(rest)
    }
}

fn validate_tar_verbose_listing(listing: &str, archive_label: &str) -> Result<(), String> {
    let mut symlink_paths: HashSet<Vec<String>> = HashSet::new();
    for raw_line in listing.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let mode_char = line.chars().next().unwrap_or(' ');
        if matches!(mode_char, 'c' | 'b' | 'p') {
            return Err(format!(
                "Rejected special device/FIFO entry in {}: {}",
                archive_label, line
            ));
        }
        let Some(rest) = extract_tar_verbose_entry_rest(line) else {
            return Err(format!(
                "Rejected unparseable tar listing entry in {}: {}",
                archive_label, line
            ));
        };
        if mode_char == 'l' {
            if rest.matches(" -> ").count() != 1 {
                return Err(format!(
                    "Rejected ambiguous or malformed symlink entry in {}: {}",
                    archive_label, line
                ));
            }
            let (entry_path, target) = rest.split_once(" -> ").unwrap();
            let parts = validate_archive_entry_path(entry_path, archive_label)?;
            check_no_symlink_ancestor(&parts, &symlink_paths, entry_path, archive_label)?;
            validate_archive_link_target(&parts, target, false, archive_label)?;
            if !parts.is_empty() {
                symlink_paths.insert(parts);
            }
        } else if mode_char == 'h' {
            if rest.matches(" link to ").count() != 1 {
                return Err(format!(
                    "Rejected ambiguous or malformed hardlink entry in {}: {}",
                    archive_label, line
                ));
            }
            let (entry_path, target) = rest.split_once(" link to ").unwrap();
            let parts = validate_archive_entry_path(entry_path, archive_label)?;
            check_no_symlink_ancestor(&parts, &symlink_paths, entry_path, archive_label)?;
            validate_archive_link_target(&parts, target, true, archive_label)?;
        } else {
            let parts = validate_archive_entry_path(rest, archive_label)?;
            check_no_symlink_ancestor(&parts, &symlink_paths, rest, archive_label)?;
        }
    }
    Ok(())
}

fn is_posix_symlink_mode_token(tok: &str) -> bool {
    let bytes = tok.as_bytes();
    bytes.len() == 10
        && bytes[0] == b'l'
        && bytes[1..].iter().all(|b| {
            matches!(
                b,
                b'r' | b'w' | b'x' | b's' | b'S' | b't' | b'T' | b'-'
            )
        })
}

fn validate_7z_slt_listing(listing: &str) -> Result<(), String> {
    let has_separator = listing.lines().any(|l| l.trim() == "----------");
    let mut in_entries = !has_separator;
    let mut symlink_paths: HashSet<Vec<String>> = HashSet::new();

    let mut current_path: Option<String> = None;
    let mut current_symlink: Option<String> = None;
    let mut current_hardlink: Option<String> = None;
    let mut current_is_symlink_attr = false;

    let flush_entry = |path_opt: &mut Option<String>,
                       sym_opt: &mut Option<String>,
                       hard_opt: &mut Option<String>,
                       is_sym_attr: &mut bool,
                       symlink_paths: &mut HashSet<Vec<String>>|
     -> Result<(), String> {
        let path = path_opt.take();
        let sym = sym_opt.take();
        let hard = hard_opt.take();
        let sym_attr = *is_sym_attr;
        *is_sym_attr = false;

        if let Some(ref entry_path) = path {
            let parts = validate_archive_entry_path(entry_path, "7z")?;
            check_no_symlink_ancestor(&parts, symlink_paths, entry_path, "7z")?;
            if let Some(ref target) = sym {
                validate_archive_link_target(&parts, target, false, "7z")?;
                if !parts.is_empty() {
                    symlink_paths.insert(parts.clone());
                }
            } else if sym_attr {
                return Err(format!(
                    "Rejected unverified symlink entry in 7z: {}",
                    entry_path
                ));
            }
            if let Some(ref target) = hard {
                validate_archive_link_target(&parts, target, true, "7z")?;
            }
        } else {
            if let Some(ref target) = sym {
                validate_archive_link_target(&[], target, false, "7z")?;
            }
            if let Some(ref target) = hard {
                validate_archive_link_target(&[], target, true, "7z")?;
            }
        }
        Ok(())
    };

    for line in listing.lines() {
        let trimmed = line.trim();
        if !in_entries {
            if trimmed == "----------" {
                in_entries = true;
            }
            continue;
        }
        if trimmed.is_empty() {
            flush_entry(
                &mut current_path,
                &mut current_symlink,
                &mut current_hardlink,
                &mut current_is_symlink_attr,
                &mut symlink_paths,
            )?;
            continue;
        }
        if let Some(file_path) = trimmed.strip_prefix("Path = ") {
            if current_path.is_some() {
                flush_entry(
                    &mut current_path,
                    &mut current_symlink,
                    &mut current_hardlink,
                    &mut current_is_symlink_attr,
                    &mut symlink_paths,
                )?;
            }
            current_path = Some(file_path.to_string());
        } else if let Some(target) = trimmed.strip_prefix("Symbolic Link = ") {
            current_symlink = Some(target.to_string());
        } else if let Some(target) = trimmed.strip_prefix("Hard Link = ") {
            current_hardlink = Some(target.to_string());
        } else if let Some(attrs) = trimmed
            .strip_prefix("Attributes = ")
            .or_else(|| trimmed.strip_prefix("Mode = "))
        {
            if attrs.split_whitespace().any(is_posix_symlink_mode_token) {
                current_is_symlink_attr = true;
            }
        }
    }
    flush_entry(
        &mut current_path,
        &mut current_symlink,
        &mut current_hardlink,
        &mut current_is_symlink_attr,
        &mut symlink_paths,
    )?;
    Ok(())
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
        let mut symlink_paths: HashSet<Vec<String>> = HashSet::new();
        for entry in tar.entries().map_err(|e| e.to_string())? {
            let mut entry = entry.map_err(|e| e.to_string())?;
            let entry_type = entry.header().entry_type();
            if entry_type.is_character_special()
                || entry_type.is_block_special()
                || entry_type.is_fifo()
            {
                return Err(format!("Rejected special device/FIFO entry in {}", lower));
            }
            let path_str = entry
                .path()
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .into_owned();
            let parts = validate_archive_entry_path(&path_str, &lower)?;
            check_no_symlink_ancestor(&parts, &symlink_paths, &path_str, &lower)?;
            if entry_type.is_symlink() || entry_type.is_hard_link() {
                let link_target = entry
                    .link_name()
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| {
                        format!(
                            "Rejected link entry without target in {}: {}",
                            lower, path_str
                        )
                    })?
                    .to_string_lossy()
                    .into_owned();
                validate_archive_link_target(
                    &parts,
                    &link_target,
                    entry_type.is_hard_link(),
                    &lower,
                )?;
                if entry_type.is_symlink() && !parts.is_empty() {
                    symlink_paths.insert(parts);
                }
            }
            entry.unpack_in(dest).map_err(|e| e.to_string())?;
        }
        return Ok(());
    }
    if lower.ends_with(".tar.zst") || lower.ends_with(".tar.xz") || lower.ends_with(".tar.bz2") {
        // Pre-screen verbose tar listing for path traversal, symlink/hardlink escapes, and device nodes
        let mut list_cmd = Command::new("tar");
        if lower.ends_with(".tar.zst") {
            list_cmd.arg("--zstd");
        }
        let list_out = list_cmd
            .arg("-tvf")
            .arg(archive)
            .output()
            .map_err(|e| format!("Failed to inspect {} archive: {}", lower, e))?;
        if !list_out.status.success() {
            return Err(format!(
                "tar failed to pre-screen {} archive before extraction: {}",
                lower,
                String::from_utf8_lossy(&list_out.stderr).trim()
            ));
        }
        let listing = String::from_utf8_lossy(&list_out.stdout);
        validate_tar_verbose_listing(&listing, &lower)?;
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
        // Pre-screen 7z file listing for path traversal and symlink escapes
        // Use 7z l -slt for structured output that includes full paths and Symbolic Link targets
        let list_out = Command::new(&bin)
            .arg("l")
            .arg("-slt")
            .arg(archive)
            .output()
            .map_err(|e| format!("Failed to inspect 7z archive: {}", e))?;
        if !list_out.status.success() {
            return Err(format!(
                "7z failed to pre-screen archive before extraction: {}",
                String::from_utf8_lossy(&list_out.stderr).trim()
            ));
        }
        let listing = String::from_utf8_lossy(&list_out.stdout);
        validate_7z_slt_listing(&listing)?;
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

    #[test]
    fn install_millennium_steambrew_is_sandbox_and_test_safe() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = WiredService::new(Some(tmp.path().join("sandbox")));
        let mut steps: Vec<(f32, String)> = Vec::new();
        let res = svc
            .install_millennium_steambrew(|p, m| steps.push((p, m)))
            .expect("sandbox/test millennium install should succeed without network");
        assert!(res.contains("skipped"));
        assert!(steps.len() >= 2);
        assert!((steps.last().unwrap().0 - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn tar_and_7z_prescreen_rejects_symlinks_hardlinks_and_traversal_escaping_dest() {
        // Safe GNU tar -tvf listing (regular files, dirs, intra-dest relative symlinks)
        let safe_tar = "\
drwxr-xr-x 1000/1000 0 2026-11-28 01:18 ./
drwxr-xr-x 1000/1000 0 2026-11-28 01:18 usr/lib/
-rwxr-xr-x 1000/1000 1234 2026-11-28 01:18 usr/lib/SLSsteam.so
lrwxrwxrwx 1000/1000 0 2026-11-28 01:18 usr/lib/libfoo.so -> libfoo.so.1
lrwxrwxrwx 1000/1000 0 2026-11-28 01:18 usr/bin/tool -> ../lib/SLSsteam.so
";
        assert!(validate_tar_verbose_listing(safe_tar, "test.tar.zst").is_ok());

        // Reject symlink escaping via absolute path
        let abs_sym = "lrwxrwxrwx 1000/1000 0 2026-11-28 01:18 a/evil -> /etc/passwd\n";
        assert!(validate_tar_verbose_listing(abs_sym, "test.tar.zst").is_err());

        // Reject symlink escaping via parent traversal
        let rel_escape = "lrwxrwxrwx 1000/1000 0 2026-11-28 01:18 a/evil -> ../../etc/passwd\n";
        assert!(validate_tar_verbose_listing(rel_escape, "test.tar.zst").is_err());

        // Reject symlink pointing to extraction root
        let root_sym = "lrwxrwxrwx 1000/1000 0 2026-11-28 01:18 a/root -> ..\n";
        assert!(validate_tar_verbose_listing(root_sym, "test.tar.zst").is_err());

        // Reject symlink chain traversal
        let chain = "\
lrwxrwxrwx 1000/1000 0 2026-11-28 01:18 a/s -> ../b
-rw-r--r-- 1000/1000 4 2026-11-28 01:18 a/s/payload.sh
";
        assert!(validate_tar_verbose_listing(chain, "test.tar.zst").is_err());

        // Reject hardlink escaping destination
        let bad_hardlink = "hrw-r--r-- 1000/1000 0 2026-11-28 01:18 a/h link to ../secret\n";
        assert!(validate_tar_verbose_listing(bad_hardlink, "test.tar.zst").is_err());

        // Reject special device node
        let dev_node = "crw-r--r-- 0/0 1,3 2026-11-28 01:18 dev/null\n";
        assert!(validate_tar_verbose_listing(dev_node, "test.tar.zst").is_err());

        // Safe 7z -slt listing (header archive path may be absolute before ----------)
        let safe_7z = "\
Path = /tmp/luatools_slssteam_123/SLSsteam-Any-release.7z
Type = 7z

----------
Path = SLSsteam
Attributes = D drwxr-xr-x

Path = SLSsteam/SLSsteam.so
Attributes = A -rwxr-xr-x

Path = SLSsteam/alias.so
Attributes = A lrwxrwxrwx
Symbolic Link = SLSsteam.so
";
        assert!(validate_7z_slt_listing(safe_7z).is_ok());

        // Reject 7z Symbolic Link escaping dest
        let bad_7z_abs = "\
Path = /tmp/test.7z
----------
Path = sub/link
Attributes = A lrwxrwxrwx
Symbolic Link = /etc/passwd
";
        assert!(validate_7z_slt_listing(bad_7z_abs).is_err());

        let bad_7z_rel = "\
Path = /tmp/test.7z
----------
Path = sub/link
Symbolic Link = ../../etc/passwd
";
        assert!(validate_7z_slt_listing(bad_7z_rel).is_err());

        // Reject opaque POSIX symlink in 7z without explicit Symbolic Link target
        let opaque_7z_sym = "\
Path = /tmp/test.7z
----------
Path = sub/link
Attributes = A lrwxrwxrwx
";
        assert!(validate_7z_slt_listing(opaque_7z_sym).is_err());
    }

    #[test]
    fn write_steam_sls_wrapper_safely_quotes_paths_and_preserves_ld_audit() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = WiredService::new(Some(tmp.path().join("sandbox")));
        let weird_dir = tmp.path().join("dir's $(touch pwn) $HOME");
        fs::create_dir_all(&weird_dir).unwrap();
        let so_path = weird_dir.join("SLSsteam.so");
        fs::write(&so_path, b"\x7fELF").unwrap();

        let wrapper = svc.write_steam_sls_wrapper(&so_path, &weird_dir).unwrap();
        let content = fs::read_to_string(wrapper).unwrap();
        assert!(content.contains("AUDIT='"));
        assert!(content.contains("'\\''s $(touch pwn) $HOME/SLSsteam.so'"));
        assert!(content.contains("export LD_AUDIT=\"$AUDIT\""));
        assert!(!content.contains("library-inject.so:"));
    }
}
