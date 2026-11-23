//! SteamBridgeService — high-level service connecting the Linux Steam interop layer
//! (`binary_reader`, `pattern_db`, `memory_patch`, `preload_loader`, `ticket_store`, `ipc`)
//! with the LuaTools Linux GUI.

use std::collections::{BTreeMap, BTreeSet};
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
    ipc::{self, IpcClient, IpcMessage},
    memory_patch::MemoryPatch,
    pattern_db::{BinaryComponent, PatternChannel, PatternDb},
    preload_loader::{self, find_steam_pid, steam_integrated, LD_SO_PRELOAD_PATH},
    ticket_store::{AppTicketData, TicketStore},
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
}

/// Main service handle.
#[derive(Debug, Clone)]
pub struct SteamBridgeService {
    pub status: Arc<Mutex<BridgeStatus>>,
    pub ticket_store: Arc<Mutex<TicketStore>>,
    pub overrides: Arc<Mutex<BridgeOverrides>>,
    pub pattern_db: PatternDb,
    pub installed_patches: Arc<Mutex<Vec<MemoryPatch>>>,
    steam_installation: Option<SteamInstallation>,
    overrides_path: PathBuf,
    lib_target_path: PathBuf,
    launcher_path: PathBuf,
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

        let config_dir = installation
            .as_ref()
            .map(|i| i.config_path.clone())
            .unwrap_or_else(|| {
                sandbox_dir
                    .as_ref()
                    .map(|s| s.join("steam_config"))
                    .unwrap_or_else(TicketStore::default_path)
            });

        let mut ticket_store = TicketStore::new(&config_dir);
        let _ = ticket_store.load();
        let apps_with_tickets = ticket_store.list_app_ids().unwrap_or_default();

        let overrides_path = data_dir.join("bridge_overrides.json");
        let overrides: BridgeOverrides = fs::read_to_string(&overrides_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        let sys_preload = is_in_ld_so_preload(&lib_target_path);
        let status = BridgeStatus {
            steam_detected: installation.is_some(),
            steam_root: installation.as_ref().map(|i| i.root.clone()),
            steam_pid: if sandbox_dir.is_none() {
                find_steam_pid()
            } else {
                None
            },
            integration_lib_installed: lib_target_path.is_file(),
            integration_lib_path: lib_target_path.is_file().then(|| lib_target_path.clone()),
            launcher_installed: launcher_path.is_file(),
            system_preload_enabled: sys_preload,
            integration_active: sandbox_dir.is_none() && check_ipc_or_maps_active(),
            tickets_loaded: installation.is_some(),
            apps_with_tickets,
            ..Default::default()
        };

        Self {
            status: Arc::new(Mutex::new(status)),
            ticket_store: Arc::new(Mutex::new(ticket_store)),
            overrides: Arc::new(Mutex::new(overrides)),
            pattern_db: PatternDb::new(),
            installed_patches: Arc::new(Mutex::new(Vec::new())),
            steam_installation: installation,
            overrides_path,
            lib_target_path,
            launcher_path,
        }
    }

    pub fn set_steam_installation(&mut self, installation: Option<SteamInstallation>) {
        self.steam_installation = installation.clone();
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        status.steam_detected = installation.is_some();
        status.steam_root = installation.as_ref().map(|i| i.root.clone());
        status.steam_pid = find_steam_pid();
        status.integration_lib_installed = self.lib_target_path.is_file();
        status.launcher_installed = self.launcher_path.is_file();
        status.system_preload_enabled = is_in_ld_so_preload(&self.lib_target_path);
        status.integration_active = check_ipc_or_maps_active();
        if let Some(inst) = &installation {
            let mut store = self.ticket_store.lock().unwrap_or_else(|e| e.into_inner());
            *store = TicketStore::new(&inst.config_path);
            let _ = store.load();
            status.tickets_loaded = true;
            status.apps_with_tickets = store.list_app_ids().unwrap_or_default();
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
            let _ = fs::write(&self.overrides_path, js);
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
                .unwrap_or("");
            if name.contains("steamclient") {
                steam_client = Some(binary.clone());
            } else if name.contains("steamui") {
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
            candidates.push(repo.join("target/debug-debug/libsteam_integration.so"));
            candidates.push(repo.join("target/release/libsteam_integration.so"));
            candidates.push(repo.join("dist/libsteam_integration.so"));
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

        let lib_path = self.ensure_integration_lib_staged()?;

        let steam_exe = if inst.root.join("steam.sh").is_file() {
            inst.root.join("steam.sh")
        } else {
            PathBuf::from("steam")
        };

        if let Some(parent) = self.launcher_path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        preload_loader::generate_ld_preload_launcher(&steam_exe, &lib_path, &self.launcher_path)
            .map_err(|e| format!("Failed to generate launcher: {}", e))?;

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

        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        status.integration_lib_installed = true;
        status.integration_lib_path = Some(lib_path);
        status.launcher_installed = self.launcher_path.is_file();
        status.system_preload_enabled = preload_ok;
        status.integration_active = check_ipc_or_maps_active();

        let preload_note = if enable_system_preload && !preload_ok {
            " (system ld.so.preload needs administrator/root — using per-launch launcher instead)"
        } else if preload_ok {
            " (system ld.so.preload enabled)"
        } else {
            " (per-launch launcher, no root required)"
        };

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
            let _ = fs::remove_file(&self.launcher_path);
        }
        if self.lib_target_path.exists() {
            let _ = fs::remove_file(&self.lib_target_path);
        }
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        status.integration_active = false;
        status.integration_lib_installed = false;
        status.integration_lib_path = None;
        status.launcher_installed = false;
        status.system_preload_enabled = false;
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
        {
            let mut ov = self.overrides.lock().unwrap_or_else(|e| e.into_inner());
            ov.legacy_cd_keys.insert(app_id, key.clone());
            self.save_overrides(&ov);
        }
        let _ = self.send_ipc_message(IpcMessage::SetLegacyCdKey { app_id, key });
    }

    pub fn remove_legacy_cd_key(&self, app_id: u32) {
        {
            let mut ov = self.overrides.lock().unwrap_or_else(|e| e.into_inner());
            ov.legacy_cd_keys.remove(&app_id);
            self.save_overrides(&ov);
        }
        let _ = self.send_ipc_message(IpcMessage::RemoveLegacyCdKey { app_id });
    }

    pub fn get_legacy_cd_key(&self, app_id: u32) -> Option<String> {
        self.overrides
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .legacy_cd_keys
            .get(&app_id)
            .cloned()
    }

    pub fn set_depot_key(&self, depot_id: u32, key: String) {
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

    pub fn set_stat(&self, app_id: u32, stat_name: String, stat_value: String) {
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
        self.ticket_store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set(data)
            .map_err(|e| e.to_string())?;
        let _ = self.send_ipc_message(IpcMessage::SetTicket {
            app_id,
            steam_id,
            app_ticket,
            e_ticket,
        });
        Ok(())
    }

    pub fn remove_tickets(&self, app_id: u32) -> Result<(), String> {
        self.ticket_store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(app_id)
            .map_err(|e| e.to_string())?;
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
    }
}
