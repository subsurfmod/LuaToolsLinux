//! SteamBridgeService — high-level service connecting the Linux Steam interop layer
//! (`binary_reader`, `pattern_db`, `memory_patch`, `preload_loader`, `ticket_store`, `ipc`)
//! with the LuaTools Linux GUI.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::models::config::default_luatools_data_dir;
use crate::models::steam::SteamInstallation;
use crate::services::steam_service::detect_steam_install;
use crate::services::update_service::UpdateService;
use crate::steam_bridge::{
    binary_reader::{scan_steam_binaries, BinaryFile},
    credentials::legacycdkey::LegacyCdKeyStore,
    denuvo_auth::{DenuvoAuthConfig, DenuvoAuthService},
    ipc::{self, IpcClient, IpcMessage},
    memory_patch::MemoryPatch,
    ost_platform::steam_credential_store::{
        get_active_user as cred_get_active_user, initialize as cred_initialize, Status as CredStatus,
    },
    pattern_db::{BinaryComponent, PatternChannel, PatternDb},
    preload_loader::{self, find_steam_pid, steam_integrated, LD_SO_PRELOAD_PATH},
    ticket_store::{AppTicketData, TicketStore},
    token_bridge::{BstUri, TokenBridge},
};

/// Result of bridge installation or binary pattern scan.
#[derive(Debug, Clone)]
pub struct BridgeInstallResult {
    pub success: bool,
    pub message: String,
    pub steam_client_patterns: Option<String>,
    pub steam_ui_patterns: Option<String>,
}

/// Persistent per-app / per-depot bridge overrides managed from the GUI.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BridgeOverrides {
    #[serde(default)]
    pub redirect_apps: BTreeSet<u32>,
    #[serde(default)]
    pub app_id_spoofs: BTreeMap<u32, u32>,
    #[serde(default)]
    pub legacy_cd_keys: BTreeMap<u32, String>,
    #[serde(default)]
    pub depot_keys: BTreeMap<u32, String>,
    #[serde(default)]
    pub stats: BTreeMap<u32, BTreeMap<String, String>>,
}

/// Snapshot of the active bridge service state for GUI rendering.
#[derive(Debug, Clone, Default)]
pub struct BridgeStatus {
    pub steam_detected: bool,
    pub steam_root: Option<PathBuf>,
    pub steam_pid: Option<u32>,
    pub steam_client_found: bool,
    pub steam_ui_found: bool,
    pub steam_client_sha256: Option<String>,
    pub steam_ui_sha256: Option<String>,
    pub integration_active: bool,
    pub integration_lib_installed: bool,
    pub integration_lib_path: Option<PathBuf>,
    pub launcher_installed: bool,
    pub system_preload_enabled: bool,
    pub patterns_loaded: bool,
    pub steam_client_patterns: Option<String>,
    pub steam_ui_patterns: Option<String>,
    pub tickets_loaded: bool,
    pub apps_with_tickets: Vec<u32>,
    pub active_account_id: Option<u32>,
    pub active_universe: Option<String>,
    pub install_progress: f32,
    pub install_status: String,
}

/// Main service handle.
#[derive(Clone)]
pub struct SteamBridgeService {
    pub status: Arc<Mutex<BridgeStatus>>,
    pub ticket_store: Arc<Mutex<TicketStore>>,
    pub legacy_cd_key_store: Arc<Mutex<LegacyCdKeyStore>>,
    pub denuvo_auth: Arc<Mutex<DenuvoAuthService>>,
    pub token_bridge: Arc<Mutex<TokenBridge>>,
    pub overrides: Arc<Mutex<BridgeOverrides>>,
    pub pattern_db: PatternDb,
    pub installed_patches: Arc<Mutex<Vec<MemoryPatch>>>,
    steam_installation: Option<SteamInstallation>,
    config_dir: PathBuf,
    is_sandboxed: bool,
    overrides_path: PathBuf,
    lib_target_path: PathBuf,
    launcher_path: PathBuf,
}

impl fmt::Debug for SteamBridgeService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SteamBridgeService")
            .field("status", &self.status)
            .field("ticket_store", &self.ticket_store)
            .field("legacy_cd_key_store", &self.legacy_cd_key_store)
            .field("overrides", &self.overrides)
            .field("steam_installation", &self.steam_installation)
            .field("config_dir", &self.config_dir)
            .field("is_sandboxed", &self.is_sandboxed)
            .finish()
    }
}

impl Default for SteamBridgeService {
    fn default() -> Self {
        Self::new()
    }
}

impl SteamBridgeService {
    pub fn new() -> Self {
        let installation = detect_steam_install(None);
        Self::with_paths(installation, None)
    }

    pub fn with_paths(
        installation: Option<SteamInstallation>,
        sandbox_dir: Option<PathBuf>,
    ) -> Self {
        let (data_dir, lib_target_path, launcher_path) = match sandbox_dir {
            Some(ref sb) => (
                sb.clone(),
                sb.join("lib/libsteam_integration.so"),
                sb.join("bin/luatools-steam-launcher"),
            ),
            None => {
                let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
                (
                    default_luatools_data_dir(),
                    preload_loader::default_integration_lib_path(),
                    home.join(".local/bin/luatools-steam-launcher"),
                )
            }
        };

        let is_sandboxed = sandbox_dir.is_some();
        let config_dir = installation
            .as_ref()
            .map(|i| i.config_path.clone())
            .unwrap_or_else(|| {
                sandbox_dir
                    .as_ref()
                    .map(|s| s.join("steam_config"))
                    .unwrap_or_else(TicketStore::default_path)
            });

        if !is_sandboxed {
            cred_initialize(&config_dir);
        }

        let mut ticket_store = TicketStore::new(&config_dir);
        let _ = ticket_store.load();
        let apps_with_tickets = ticket_store.list_app_ids().unwrap_or_default();

        let mut legacy_cd_key_store = LegacyCdKeyStore::new(&config_dir);
        let _ = legacy_cd_key_store.load();

        let mut token_bridge = TokenBridge::new();
        token_bridge.register_handler(Box::new(|uri: &BstUri| {
            if uri.is_valid() {
                Some(uri.clone())
            } else {
                None
            }
        }));

        let (active_account_id, active_universe) = if !is_sandboxed {
            let mut acct = 0u32;
            let mut univ = String::new();
            if cred_get_active_user(&mut acct, &mut univ) == CredStatus::Ok {
                (Some(acct), Some(univ))
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

        let overrides_path = data_dir.join("bridge_overrides.json");
        let overrides: BridgeOverrides = fs::read_to_string(&overrides_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        let sys_preload = is_in_ld_so_preload(&lib_target_path);
        let status = BridgeStatus {
            steam_detected: installation.is_some(),
            steam_root: installation.as_ref().map(|i| i.root.clone()),
            steam_pid: if !is_sandboxed {
                find_steam_pid()
            } else {
                None
            },
            integration_lib_installed: lib_target_path.is_file(),
            integration_lib_path: lib_target_path.is_file().then(|| lib_target_path.clone()),
            launcher_installed: launcher_path.is_file(),
            system_preload_enabled: sys_preload,
            integration_active: !is_sandboxed && check_ipc_or_maps_active(),
            tickets_loaded: installation.is_some(),
            apps_with_tickets,
            active_account_id,
            active_universe,
            ..Default::default()
        };

        Self {
            status: Arc::new(Mutex::new(status)),
            ticket_store: Arc::new(Mutex::new(ticket_store)),
            legacy_cd_key_store: Arc::new(Mutex::new(legacy_cd_key_store)),
            denuvo_auth: Arc::new(Mutex::new(DenuvoAuthService::new())),
            token_bridge: Arc::new(Mutex::new(token_bridge)),
            overrides: Arc::new(Mutex::new(overrides)),
            pattern_db: PatternDb::new(),
            installed_patches: Arc::new(Mutex::new(Vec::new())),
            steam_installation: installation,
            config_dir,
            is_sandboxed,
            overrides_path,
            lib_target_path,
            launcher_path,
        }
    }

    pub fn set_steam_installation(&mut self, installation: Option<SteamInstallation>) {
        self.steam_installation = installation.clone();
        if let Some(inst) = &installation {
            self.config_dir = inst.config_path.clone();
            if !self.is_sandboxed {
                cred_initialize(&inst.config_path);
            }
            let mut cd_store = self
                .legacy_cd_key_store
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *cd_store = LegacyCdKeyStore::new(&inst.config_path);
            let _ = cd_store.load();
        }
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        status.steam_detected = installation.is_some();
        status.steam_root = installation.as_ref().map(|i| i.root.clone());
        status.steam_pid = if !self.is_sandboxed {
            find_steam_pid()
        } else {
            None
        };
        status.integration_lib_installed = self.lib_target_path.is_file();
        status.integration_lib_path = self
            .lib_target_path
            .is_file()
            .then(|| self.lib_target_path.clone());
        status.launcher_installed = self.launcher_path.is_file();
        status.system_preload_enabled = is_in_ld_so_preload(&self.lib_target_path);
        status.integration_active = !self.is_sandboxed && check_ipc_or_maps_active();
        // Reset per-installation scan metadata so stale state never leaks across roots
        status.steam_client_found = false;
        status.steam_ui_found = false;
        status.steam_client_sha256 = None;
        status.steam_ui_sha256 = None;
        status.patterns_loaded = false;
        status.steam_client_patterns = None;
        status.steam_ui_patterns = None;
        if let Some(inst) = &installation {
            let mut store = self.ticket_store.lock().unwrap_or_else(|e| e.into_inner());
            *store = TicketStore::new(&inst.config_path);
            let _ = store.load();
            status.tickets_loaded = true;
            status.apps_with_tickets = store.list_app_ids().unwrap_or_default();
            if !self.is_sandboxed {
                let mut acct = 0u32;
                let mut univ = String::new();
                if cred_get_active_user(&mut acct, &mut univ) == CredStatus::Ok {
                    status.active_account_id = Some(acct);
                    status.active_universe = Some(univ);
                } else {
                    status.active_account_id = None;
                    status.active_universe = None;
                }
            }
        } else {
            status.tickets_loaded = false;
            status.apps_with_tickets.clear();
            status.active_account_id = None;
            status.active_universe = None;
        }
    }

    pub fn refresh_steam_detection(&mut self) {
        let installation = detect_steam_install(None);
        self.set_steam_installation(installation);
    }

    fn save_overrides(&self, ov: &BridgeOverrides) {
        if let Some(parent) = self.overrides_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(js) = serde_json::to_string_pretty(ov) {
            let tmp = self
                .overrides_path
                .with_extension(format!("json.tmp.{}", std::process::id()));
            if fs::write(&tmp, &js).is_ok() && fs::rename(&tmp, &self.overrides_path).is_err() {
                let _ = fs::remove_file(&tmp);
                let _ = fs::write(&self.overrides_path, &js);
            }
        }
    }

    pub fn snapshot_overrides(&self) -> BridgeOverrides {
        self.overrides
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Scans `steamclient.so` and `steamui.so`, records SHA-256 hashes, and queries `PatternDb`.
    pub fn scan_and_load_patterns(&mut self) -> Result<BridgeInstallResult, String> {
        let inst = self
            .steam_installation
            .as_ref()
            .ok_or_else(|| "Steam not detected.".to_string())?;

        let binaries = scan_steam_binaries(&inst.root);
        let mut steam_client: Option<BinaryFile> = None;
        let mut steam_ui: Option<BinaryFile> = None;

        for binary in &binaries {
            let name = binary
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let path_low = binary.path.to_string_lossy().to_ascii_lowercase();
            let is_64 = path_low.contains("linux64") || path_low.contains("x86_64");
            if name == "steamclient.so" {
                if steam_client.is_none() || is_64 {
                    steam_client = Some(binary.clone());
                }
            } else if name == "steamui.so" {
                if steam_ui.is_none() || is_64 {
                    steam_ui = Some(binary.clone());
                }
            } else if steam_client.is_none()
                && name.contains("steamclient")
                && name.ends_with(".so")
            {
                steam_client = Some(binary.clone());
            } else if steam_ui.is_none() && name.contains("steamui") && name.ends_with(".so") {
                steam_ui = Some(binary.clone());
            }
        }

        let mut sc_patterns = None;
        let mut su_patterns = None;
        let mut notes = Vec::new();

        if let Some(client) = &steam_client {
            match self.pattern_db.lookup_by_binary(
                client,
                PatternChannel::Pattern,
                BinaryComponent::SteamClient,
            ) {
                Ok(pf) => {
                    let keys: Vec<_> = pf.patterns.keys().cloned().collect();
                    sc_patterns = Some(keys.join(", "));
                    notes.push(format!("steamclient: {} pattern(s)", keys.len()));
                }
                Err(_) => {
                    let sha = client.sha256_hex();
                    notes.push(format!(
                        "steamclient SHA-256 {} (no remote TOML published yet)",
                        sha.get(..12).unwrap_or(sha)
                    ));
                }
            }
        }

        if let Some(ui) = &steam_ui {
            match self.pattern_db.lookup_by_binary(
                ui,
                PatternChannel::Pattern,
                BinaryComponent::SteamUi,
            ) {
                Ok(pf) => {
                    let keys: Vec<_> = pf.patterns.keys().cloned().collect();
                    su_patterns = Some(keys.join(", "));
                    notes.push(format!("steamui: {} pattern(s)", keys.len()));
                }
                Err(_) => {
                    let sha = ui.sha256_hex();
                    notes.push(format!(
                        "steamui SHA-256 {} (no remote TOML published yet)",
                        sha.get(..12).unwrap_or(sha)
                    ));
                }
            }
        }

        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            status.steam_client_found = steam_client.is_some();
            status.steam_ui_found = steam_ui.is_some();
            status.steam_client_sha256 = steam_client.as_ref().map(|b| b.sha256.clone());
            status.steam_ui_sha256 = steam_ui.as_ref().map(|b| b.sha256.clone());
            status.steam_client_patterns = sc_patterns.clone();
            status.steam_ui_patterns = su_patterns.clone();
            status.patterns_loaded = sc_patterns.is_some() || su_patterns.is_some();
        }

        let summary = if notes.is_empty() {
            format!(
                "Scanned {} ELF file(s) under {}",
                binaries.len(),
                inst.root.display()
            )
        } else {
            format!("Scanned {} ELF(s) — {}", binaries.len(), notes.join(" · "))
        };

        Ok(BridgeInstallResult {
            success: true,
            message: summary,
            steam_client_patterns: sc_patterns,
            steam_ui_patterns: su_patterns,
        })
    }

    /// Locates or builds `libsteam_integration.so` and atomically stages/refreshes it
    /// at `~/.local/lib/luatools/libsteam_integration.so`.
    pub fn ensure_integration_lib_staged(&self) -> Result<PathBuf, String> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(repo) = UpdateService::discover_repo_dir("") {
            candidates.push(repo.join("target/i686-unknown-linux-gnu/release/libsteam_integration.so"));
            candidates.push(repo.join("target/i686-unknown-linux-gnu/debug-debug/libsteam_integration.so"));
            candidates.push(repo.join("target/i686-unknown-linux-gnu/debug/libsteam_integration.so"));
            candidates.push(repo.join("dist/32/libsteam_integration.so"));
            candidates.push(repo.join("dist/libsteam_integration32.so"));
            candidates.push(repo.join("dist/libsteam_integration.so"));
            candidates.push(repo.join("target/debug-debug/libsteam_integration.so"));
            candidates.push(repo.join("target/release/libsteam_integration.so"));
            candidates.push(repo.join("target/debug/libsteam_integration.so"));
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(dir.join("libsteam_integration.so"));
                candidates.push(dir.join("../lib/luatools/libsteam_integration.so"));
            }
        }
        candidates.push(PathBuf::from("/usr/lib/luatools/libsteam_integration.so"));

        if let Some(src) = candidates.into_iter().find(|p| p.is_file()) {
            let needs_copy = if !self.lib_target_path.is_file() {
                true
            } else {
                let same_file = fs::canonicalize(&src)
                    .ok()
                    .zip(fs::canonicalize(&self.lib_target_path).ok())
                    .is_some_and(|(a, b)| a == b);
                if same_file {
                    false
                } else {
                    match (fs::metadata(&src), fs::metadata(&self.lib_target_path)) {
                        (Ok(sm), Ok(dm)) => {
                            let newer = sm
                                .modified()
                                .ok()
                                .zip(dm.modified().ok())
                                .is_some_and(|(s, d)| s > d);
                            newer || sm.len() != dm.len()
                        }
                        _ => true,
                    }
                }
            };
            if needs_copy {
                UpdateService::atomic_replace_executable(&src, &self.lib_target_path)?;
                if let Some(parent) = self.lib_target_path.parent() {
                    let _ = fs::create_dir_all(parent.join("32"));
                    let _ = fs::copy(&src, parent.join("32/libsteam_integration.so"));
                    let _ = fs::copy(&src, parent.join("libsteam_integration32.so"));
                }
            }
            return Ok(self.lib_target_path.clone());
        }

        if self.lib_target_path.is_file() {
            return Ok(self.lib_target_path.clone());
        }

        // Attempt on-demand cargo build -p bridge_lib (preferring debug-debug symbols)
        if let Some(repo) = UpdateService::discover_repo_dir("") {
            if repo.join("bridge_lib/Cargo.toml").is_file() {
                let cargo_bin = dirs::home_dir()
                    .map(|h| h.join(".cargo/bin/cargo"))
                    .filter(|p| p.is_file())
                    .unwrap_or_else(|| PathBuf::from("cargo"));

                let out_i686 = Command::new(&cargo_bin)
                    .args([
                        "build",
                        "--manifest-path",
                        "bridge_lib/Cargo.toml",
                        "--target",
                        "i686-unknown-linux-gnu",
                        "--release",
                        "-j",
                        "2",
                    ])
                    .current_dir(&repo)
                    .output();
                let built_i686 = repo.join("target/i686-unknown-linux-gnu/release/libsteam_integration.so");
                if let Ok(ref out) = out_i686 {
                    if out.status.success() && built_i686.is_file() {
                        UpdateService::atomic_replace_executable(&built_i686, &self.lib_target_path)?;
                        if let Some(parent) = self.lib_target_path.parent() {
                            let _ = fs::create_dir_all(parent.join("32"));
                            let _ = fs::copy(&built_i686, parent.join("32/libsteam_integration.so"));
                            let _ = fs::copy(&built_i686, parent.join("libsteam_integration32.so"));
                        }
                        return Ok(self.lib_target_path.clone());
                    }
                }

                let out = Command::new(&cargo_bin)
                    .args([
                        "build",
                        "--profile",
                        "debug-debug",
                        "-p",
                        "bridge_lib",
                        "-j",
                        "2",
                    ])
                    .current_dir(&repo)
                    .output()
                    .map_err(|e| format!("Failed to invoke cargo to build bridge_lib: {}", e))?;
                let built_dbg = repo.join("target/debug-debug/libsteam_integration.so");
                if out.status.success() && built_dbg.is_file() {
                    UpdateService::atomic_replace_executable(&built_dbg, &self.lib_target_path)?;
                    return Ok(self.lib_target_path.clone());
                }

                let out_rel = Command::new(&cargo_bin)
                    .args(["build", "--release", "-p", "bridge_lib", "-j", "2"])
                    .current_dir(&repo)
                    .output()
                    .map_err(|e| format!("Failed to invoke cargo to build bridge_lib: {}", e))?;
                let built_rel = repo.join("target/release/libsteam_integration.so");
                if out_rel.status.success() && built_rel.is_file() {
                    UpdateService::atomic_replace_executable(&built_rel, &self.lib_target_path)?;
                    return Ok(self.lib_target_path.clone());
                }
            }
        }

        Err(format!(
            "libsteam_integration.so not found at {} and could not be built automatically.",
            self.lib_target_path.display()
        ))
    }

    /// Stages `libsteam_integration.so`, writes `~/.local/bin/luatools-steam-launcher`,
    /// and optionally registers `/etc/ld.so.preload`.
    pub fn install_integration(
        &mut self,
        enable_system_preload: bool,
    ) -> Result<BridgeInstallResult, String> {
        let inst = self
            .steam_installation
            .as_ref()
            .ok_or_else(|| "Steam installation not detected.".to_string())?;

        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        status.install_progress = 5.0;
        status.install_status = "Scanning Steam binaries...".to_string();
        drop(status);

        let lib_path = match self.ensure_integration_lib_staged() {
            Ok(p) => p,
            Err(e) => {
                let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
                status.install_progress = 0.0;
                status.install_status = format!("Install failed: {}", e);
                return Err(e);
            }
        };

        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            status.install_progress = 40.0;
            status.install_status = "Generating Steam launch wrapper...".to_string();
        }

        let steam_exe = if inst.root.join("steam.sh").is_file() {
            inst.root.join("steam.sh")
        } else {
            PathBuf::from("steam")
        };

        if let Some(parent) = self.launcher_path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
                status.install_progress = 0.0;
                status.install_status = format!("Install failed: {}", e);
                return Err(e.to_string());
            }
        }
        if let Err(e) =
            preload_loader::generate_ld_preload_launcher(&steam_exe, &lib_path, &self.launcher_path)
        {
            let msg = format!("Failed to generate launcher: {}", e);
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            status.install_progress = 0.0;
            status.install_status = msg.clone();
            return Err(msg);
        }

        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            status.install_progress = 60.0;
            status.install_status = "Configuring Steam preload...".to_string();
        }

        let mut preload_ok = false;
        if enable_system_preload {
            match preload_loader::enable_system_preload(&lib_path) {
                Ok(()) => preload_ok = true,
                Err(e) => {
                    // Non-fatal: per-launch launcher still works without root.
                    // We continue and tell the user system-wide preload needs admin.
                    let _ = e;
                }
            }
        }

        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            status.integration_lib_installed = true;
            status.integration_lib_path = Some(lib_path);
            status.launcher_installed = self.launcher_path.is_file();
            status.system_preload_enabled = preload_ok;
            status.install_progress = 90.0;
            status.install_status = "Finalizing Steam integration...".to_string();
        }

        let preload_note = if enable_system_preload && !preload_ok {
            " (system ld.so.preload needs administrator/root — using per-launch launcher instead)"
        } else if preload_ok {
            " (system ld.so.preload enabled)"
        } else {
            " (per-launch launcher, no root required)"
        };

        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            status.integration_active = check_ipc_or_maps_active();
            status.install_progress = 100.0;
            status.install_status = "Integration ready".to_string();
        }

        Ok(BridgeInstallResult {
            success: true,
            message: format!(
                "Installed Steam integration launcher at {}{}",
                self.launcher_path.display(),
                preload_note
            ),
            steam_client_patterns: None,
            steam_ui_patterns: None,
        })
    }

    pub fn uninstall_integration(&mut self) -> Result<(), String> {
        if is_in_ld_so_preload(&self.lib_target_path) {
            preload_loader::disable_system_preload(&self.lib_target_path)
                .map_err(|e| format!("Failed to clean /etc/ld.so.preload (needs root): {}", e))?;
        }
        if self.launcher_path.exists() {
            fs::remove_file(&self.launcher_path)
                .map_err(|e| format!("Failed to remove launcher: {}", e))?;
        }
        if self.lib_target_path.exists() {
            fs::remove_file(&self.lib_target_path)
                .map_err(|e| format!("Failed to remove integration library: {}", e))?;
        }
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        status.integration_active = false;
        status.integration_lib_installed = self.lib_target_path.is_file();
        status.integration_lib_path = self
            .lib_target_path
            .is_file()
            .then(|| self.lib_target_path.clone());
        status.launcher_installed = self.launcher_path.is_file();
        status.system_preload_enabled = is_in_ld_so_preload(&self.lib_target_path);
        Ok(())
    }

    // ===== Per-Game & Per-Depot Integration Actions (Local Persistence + Live IPC) =====

    pub fn add_redirect_app(&self, app_id: u32) {
        {
            let mut ov = self.overrides.lock().unwrap_or_else(|e| e.into_inner());
            ov.redirect_apps.insert(app_id);
            self.save_overrides(&ov);
        }
        let _ = self.send_ipc_message(IpcMessage::AddPlayUnownedApp { app_id });
    }

    pub fn remove_redirect_app(&self, app_id: u32) {
        {
            let mut ov = self.overrides.lock().unwrap_or_else(|e| e.into_inner());
            ov.redirect_apps.remove(&app_id);
            self.save_overrides(&ov);
        }
        let _ = self.send_ipc_message(IpcMessage::RemovePlayUnownedApp { app_id });
    }

    pub fn is_redirect_app(&self, app_id: u32) -> bool {
        self.overrides
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .redirect_apps
            .contains(&app_id)
    }

    pub fn set_app_id_spoof(&self, app_id: u32, spoof_id: u32) {
        {
            let mut ov = self.overrides.lock().unwrap_or_else(|e| e.into_inner());
            ov.app_id_spoofs.insert(app_id, spoof_id);
            self.save_overrides(&ov);
        }
        let _ = self.send_ipc_message(IpcMessage::SetAppIdSpoof { app_id, spoof_id });
    }

    pub fn remove_app_id_spoof(&self, app_id: u32) {
        {
            let mut ov = self.overrides.lock().unwrap_or_else(|e| e.into_inner());
            ov.app_id_spoofs.remove(&app_id);
            self.save_overrides(&ov);
        }
        let _ = self.send_ipc_message(IpcMessage::RemoveAppIdSpoof { app_id });
    }

    pub fn get_app_id_spoof(&self, app_id: u32) -> Option<u32> {
        self.overrides
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .app_id_spoofs
            .get(&app_id)
            .copied()
    }

    pub fn set_legacy_cd_key(&self, app_id: u32, key: String) {
        let key = key.trim().to_string();
        if key.is_empty() {
            self.remove_legacy_cd_key(app_id);
            return;
        }
        {
            let mut ov = self.overrides.lock().unwrap_or_else(|e| e.into_inner());
            ov.legacy_cd_keys.insert(app_id, key.clone());
            self.save_overrides(&ov);
        }
        let _ = self
            .legacy_cd_key_store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_override(app_id, key.clone());
        let _ = self.send_ipc_message(IpcMessage::SetLegacyCdKey { app_id, key });
    }

    pub fn remove_legacy_cd_key(&self, app_id: u32) {
        {
            let mut ov = self.overrides.lock().unwrap_or_else(|e| e.into_inner());
            ov.legacy_cd_keys.remove(&app_id);
            self.save_overrides(&ov);
        }
        let _ = self
            .legacy_cd_key_store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove_override(app_id);
        let _ = self.send_ipc_message(IpcMessage::RemoveLegacyCdKey { app_id });
    }

    pub fn get_legacy_cd_key(&self, app_id: u32) -> Option<String> {
        if let Some(key) = self
            .overrides
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .legacy_cd_keys
            .get(&app_id)
            .cloned()
        {
            return Some(key);
        }
        self.legacy_cd_key_store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(app_id)
            .ok()
            .and_then(|d| d.override_key.clone())
    }

    /// Resolves a legacy CD key from `LegacyCdKeyStore`, falling back to deterministic synthesis
    /// for the active Steam account (from `steam_credential_store::get_active_user`).
    pub fn resolve_legacy_cd_key(&self, app_id: u32) -> Option<String> {
        let account_id = self.active_steam_user().map(|(id, _)| id).unwrap_or(0);
        let mut store = self
            .legacy_cd_key_store
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        LegacyCdKeyStore::resolve(&mut store, app_id, account_id)
            .ok()
            .flatten()
    }

    /// Returns `(account_id, universe)` for the active Steam user if detected.
    pub fn active_steam_user(&self) -> Option<(u32, String)> {
        if !self.is_sandboxed {
            let mut account_id = 0u32;
            let mut universe = String::new();
            if cred_get_active_user(&mut account_id, &mut universe) == CredStatus::Ok {
                return Some((account_id, universe));
            }
        }
        let status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        status
            .active_account_id
            .zip(status.active_universe.clone())
    }

    pub fn configure_denuvo_auth(&self, config: DenuvoAuthConfig) {
        self.denuvo_auth
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .configure(config);
    }

    pub fn register_denuvo_app(&self, app_id: u32) {
        self.denuvo_auth
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .register_denuvo_app(app_id);
    }

    pub fn unregister_denuvo_app(&self, app_id: u32) {
        self.denuvo_auth
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .unregister_denuvo_app(app_id);
    }

    pub fn is_denuvo_app(&self, app_id: u32) -> bool {
        self.denuvo_auth
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_denuvo_protected(app_id)
    }

    pub fn scan_executable_protection(
        &self,
        exe_path: &Path,
    ) -> crate::steam_bridge::denuvo_auth::ProtectionScanReport {
        self.denuvo_auth
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .scan_executable(exe_path)
    }

    pub fn scan_game_folder_protection(
        &self,
        app_id: u32,
        game_dir: &Path,
    ) -> crate::steam_bridge::denuvo_auth::ProtectionScanReport {
        self.denuvo_auth
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .scan_game_folder(app_id, game_dir)
    }

    pub fn scan_process_protection(
        &self,
        pid: u32,
    ) -> crate::steam_bridge::denuvo_auth::ProtectionScanReport {
        crate::steam_bridge::denuvo_auth::scan_process_protection(pid)
    }

    pub fn dispatch_bst_uri(&self, uri: &str) -> Option<BstUri> {
        self.token_bridge
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .handle_uri(uri)
    }

    pub fn set_depot_key(&self, depot_id: u32, key: String) {
        let key = key.trim().to_ascii_lowercase();
        if key.is_empty() {
            self.remove_depot_key(depot_id);
            return;
        }
        {
            let mut ov = self.overrides.lock().unwrap_or_else(|e| e.into_inner());
            ov.depot_keys.insert(depot_id, key.clone());
            self.save_overrides(&ov);
        }
        let _ = self.send_ipc_message(IpcMessage::SetDepotKey { depot_id, key });
    }

    pub fn remove_depot_key(&self, depot_id: u32) {
        {
            let mut ov = self.overrides.lock().unwrap_or_else(|e| e.into_inner());
            ov.depot_keys.remove(&depot_id);
            self.save_overrides(&ov);
        }
        let _ = self.send_ipc_message(IpcMessage::RemoveDepotKey { depot_id });
    }

    pub fn get_depot_key(&self, depot_id: u32) -> Option<String> {
        self.overrides
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .depot_keys
            .get(&depot_id)
            .cloned()
    }

    pub fn set_stat(&self, app_id: u32, stat_name: String, stat_value: String) {
        let stat_name = stat_name.trim().to_string();
        if stat_name.is_empty() {
            return;
        }
        {
            let mut ov = self.overrides.lock().unwrap_or_else(|e| e.into_inner());
            ov.stats
                .entry(app_id)
                .or_default()
                .insert(stat_name.clone(), stat_value.clone());
            self.save_overrides(&ov);
        }
        let _ = self.send_ipc_message(IpcMessage::SetStat {
            app_id,
            stat_name,
            stat_value,
        });
    }

    pub fn remove_stat(&self, app_id: u32, stat_name: &str) {
        let stat_name = stat_name.trim();
        {
            let mut ov = self.overrides.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(map) = ov.stats.get_mut(&app_id) {
                map.remove(stat_name);
                if map.is_empty() {
                    ov.stats.remove(&app_id);
                }
            }
            self.save_overrides(&ov);
        }
        let _ = self.send_ipc_message(IpcMessage::RemoveStat {
            app_id,
            stat_name: stat_name.to_string(),
        });
    }

    pub fn get_stats(&self, app_id: u32) -> BTreeMap<String, String> {
        self.overrides
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stats
            .get(&app_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn get_tickets(&self, app_id: u32) -> Option<AppTicketData> {
        self.ticket_store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(app_id)
            .ok()
    }

    pub fn set_tickets(&self, data: AppTicketData) -> Result<(), String> {
        let app_id = data.app_id;
        let steam_id = data.steam_id.clone();
        let app_ticket = data.app_ticket.clone();
        let e_ticket = data.e_ticket.clone();
        let apps_with_tickets = {
            let mut store = self.ticket_store.lock().unwrap_or_else(|e| e.into_inner());
            store.set(data).map_err(|e| e.to_string())?;
            store.list_app_ids().unwrap_or_default()
        };
        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            status.tickets_loaded = true;
            status.apps_with_tickets = apps_with_tickets;
        }
        let _ = self.send_ipc_message(IpcMessage::SetTicket {
            app_id,
            steam_id,
            app_ticket,
            e_ticket,
        });
        Ok(())
    }

    pub fn remove_tickets(&self, app_id: u32) -> Result<(), String> {
        let apps_with_tickets = {
            let mut store = self.ticket_store.lock().unwrap_or_else(|e| e.into_inner());
            store.remove(app_id).map_err(|e| e.to_string())?;
            store.list_app_ids().unwrap_or_default()
        };
        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            status.apps_with_tickets = apps_with_tickets;
        }
        let _ = self.send_ipc_message(IpcMessage::RemoveTicket { app_id });
        Ok(())
    }

    pub fn list_ticket_apps(&self) -> Vec<u32> {
        self.ticket_store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .list_app_ids()
            .unwrap_or_default()
    }

    pub fn status(&self) -> BridgeStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn pattern_db(&self) -> &PatternDb {
        &self.pattern_db
    }

    pub fn send_ipc_message(
        &self,
        msg: ipc::IpcMessage,
    ) -> Result<Option<ipc::IpcMessage>, String> {
        let client = IpcClient::new();
        client.send(&msg)
    }
}

fn is_in_ld_so_preload(lib_path: &Path) -> bool {
    let target = lib_path.to_string_lossy();
    fs::read_to_string(LD_SO_PRELOAD_PATH)
        .map(|s| s.lines().any(|l| l.trim() == target.as_ref()))
        .unwrap_or(false)
}

fn check_ipc_or_maps_active() -> bool {
    if IpcClient::new().ping().unwrap_or(false) {
        return true;
    }
    steam_integrated()
}

static BRIDGE_SERVICE: Mutex<Option<SteamBridgeService>> = Mutex::new(None);

pub fn init_global_service() -> SteamBridgeService {
    let svc = SteamBridgeService::new();
    *BRIDGE_SERVICE.lock().unwrap_or_else(|e| e.into_inner()) = Some(svc.clone());
    svc
}

pub fn global_service() -> Option<SteamBridgeService> {
    BRIDGE_SERVICE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_bridge_overrides_cleanly_in_sandbox() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = SteamBridgeService::with_paths(None, Some(tmp.path().to_path_buf()));

        svc.add_redirect_app(1245620);
        svc.set_app_id_spoof(1245620, 480);
        svc.set_legacy_cd_key(1245620, "XXXX-YYYY-ZZZZ".into());
        svc.set_depot_key(1245621, "ab".repeat(32));
        svc.set_stat(1245620, "kills".into(), "42".into());

        let reloaded = SteamBridgeService::with_paths(None, Some(tmp.path().to_path_buf()));
        assert!(reloaded.is_redirect_app(1245620));
        assert_eq!(reloaded.get_app_id_spoof(1245620), Some(480));
        assert_eq!(
            reloaded.get_legacy_cd_key(1245620).as_deref(),
            Some("XXXX-YYYY-ZZZZ")
        );
        assert_eq!(
            reloaded.get_stats(1245620).get("kills").map(String::as_str),
            Some("42")
        );

        reloaded.remove_stat(1245620, "kills");
        reloaded.remove_legacy_cd_key(1245620);
        reloaded.remove_app_id_spoof(1245620);
        reloaded.remove_redirect_app(1245620);
        assert!(!reloaded.is_redirect_app(1245620));
        assert!(reloaded.get_stats(1245620).is_empty());
        assert_eq!(
            reloaded.get_depot_key(1245621).as_deref(),
            Some("ab".repeat(32).as_str())
        );
    }

    #[test]
    fn set_steam_installation_none_clears_stale_status_and_tickets_sync_status() {
        let tmp = tempfile::tempdir().unwrap();
        let mut svc = SteamBridgeService::with_paths(None, Some(tmp.path().to_path_buf()));

        let ticket = AppTicketData {
            app_id: 730,
            steam_id: Some("76561198000000001".into()),
            app_ticket: Some("aa".repeat(32)),
            e_ticket: None,
            last_update: None,
        };
        svc.set_tickets(ticket).unwrap();
        assert!(svc.status().apps_with_tickets.contains(&730));

        svc.remove_tickets(730).unwrap();
        assert!(!svc.status().apps_with_tickets.contains(&730));

        // Clearing Steam installation must wipe stale detection/pattern/ticket status
        {
            let mut st = svc.status.lock().unwrap();
            st.steam_client_found = true;
            st.patterns_loaded = true;
            st.tickets_loaded = true;
            st.apps_with_tickets = vec![730];
        }
        svc.set_steam_installation(None);
        let cleared = svc.status();
        assert!(!cleared.steam_detected);
        assert!(!cleared.steam_client_found);
        assert!(!cleared.patterns_loaded);
        assert!(!cleared.tickets_loaded);
        assert!(cleared.apps_with_tickets.is_empty());
    }

    #[test]
    fn wires_denuvo_token_bridge_and_legacy_cd_key_store_in_sandbox() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = SteamBridgeService::with_paths(None, Some(tmp.path().to_path_buf()));

        svc.set_legacy_cd_key(1245620, "KEY-ABCD-EFGH".into());
        assert_eq!(
            svc.get_legacy_cd_key(1245620).as_deref(),
            Some("KEY-ABCD-EFGH")
        );
        assert_eq!(
            svc.resolve_legacy_cd_key(1245620).as_deref(),
            Some("KEY-ABCD-EFGH")
        );

        let bst = svc.dispatch_bst_uri("bst://1245620/session_tok").unwrap();
        assert_eq!(bst.app_id, 1245620);
        assert_eq!(bst.token, "session_tok");

        svc.configure_denuvo_auth(DenuvoAuthConfig {
            enabled: true,
            pool_steam_id: Some(76_561_198_028_121_353),
            ..Default::default()
        });
        svc.register_denuvo_app(1245620);
        assert!(svc.is_denuvo_app(1245620));
        svc.unregister_denuvo_app(1245620);
    }
}
