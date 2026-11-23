//! SteamBridgeService — high-level service that connects the Linux bridge layer
//! (ELF scanning, pattern lookup, memory patching, ticket store) with the rest
//! of LuaTools Linux.
//!
//! This is the runtime component that lets LuaTools interoperate with Steam on
//! Linux:
//!   1. Detect Steam installation (existing steam_service).
//!   2. Scan steamclient.so / steamui.so with the binary reader.
//!   3. Look up byte patterns from the pattern database.
//!   4. Install the integration shared library into Steam via LD_PRELOAD.
//!   5. Manage per-app tickets (AppTicket/ETicket/SteamID) in localconfig.vdf.
//!   6. Expose integration functions used by .lua files in stplug-in/.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::steam_bridge::{
    binary_reader::{BinaryFile, scan_steam_binaries},
    ipc::{self, IpcClient},
    memory_patch::MemoryPatch,
    pattern_db::{BinaryComponent, PatternChannel, PatternDb},
    preload_loader::{self, steam_integrated},
    ticket_store::{AppTicketData, TicketStore},
};
use crate::models::steam::SteamInstallation;
use crate::services::steam_service::detect_steam_install;

/// Result of bridge installation.
#[derive(Debug, Clone)]
pub struct BridgeInstallResult {
    pub success: bool,
    pub message: String,
    pub steam_client_patterns: Option<String>,
    pub steam_ui_patterns: Option<String>,
}

/// The active bridge service state.
#[derive(Debug, Clone, Default)]
pub struct BridgeStatus {
    pub steam_detected: bool,
    pub steam_root: Option<PathBuf>,
    pub steam_client_found: bool,
    pub steam_ui_found: bool,
    pub integration_active: bool,
    pub integration_lib_path: Option<PathBuf>,
    pub patterns_loaded: bool,
    pub tickets_loaded: bool,
    pub apps_with_tickets: Vec<u32>,
}

/// Main service handle.
#[derive(Debug, Clone)]
pub struct SteamBridgeService {
    pub status: Arc<Mutex<BridgeStatus>>,
    pub ticket_store: Arc<Mutex<TicketStore>>,
    pub pattern_db: PatternDb,
    pub installed_patches: Arc<Mutex<Vec<MemoryPatch>>>,
    steam_installation: Option<SteamInstallation>,
}

impl Default for SteamBridgeService {
    fn default() -> Self {
        Self::new()
    }
}

impl SteamBridgeService {
    /// Create a new SteamBridgeService and attempt to detect Steam.
    pub fn new() -> Self {
        let installation = detect_steam_install(None);
        let config_dir = installation
            .as_ref()
            .map(|i| i.config_path.clone())
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".steam/steam/config")
            });

        let mut ticket_store = TicketStore::new(&config_dir);
        let _ = ticket_store.load();

        let status = BridgeStatus {
            steam_detected: installation.is_some(),
            steam_root: installation.as_ref().map(|i| i.root.clone()),
            ..Default::default()
        };

        Self {
            status: Arc::new(Mutex::new(status)),
            ticket_store: Arc::new(Mutex::new(ticket_store)),
            pattern_db: PatternDb::new(),
            installed_patches: Arc::new(Mutex::new(Vec::new())),
            steam_installation: installation,
        }
    }

    /// Re-detect Steam installation and refresh state.
    pub fn refresh_steam_detection(&mut self) {
        let installation = detect_steam_install(None);
        self.steam_installation = installation.clone();

        let mut status = self.status.lock().unwrap();
        status.steam_detected = installation.is_some();
        status.steam_root = installation.as_ref().map(|i| i.root.clone());
        status.integration_active = steam_integrated();

        if let Some(inst) = &installation {
            let config_dir = inst.config_path.clone();
            let mut store = self.ticket_store.lock().unwrap();
            *store = TicketStore::new(&config_dir);
            let _ = store.load();
            status.tickets_loaded = true;
            status.apps_with_tickets = store.list_app_ids().unwrap_or_default();
        }
    }

    /// Scan Steam binaries (steamclient.so, steamui.so) and load patterns from
    /// the pattern database.
    pub fn scan_and_load_patterns(&mut self) -> Result<BridgeInstallResult, String> {
        let inst = self
            .steam_installation
            .as_ref()
            .ok_or_else(|| "Steam not detected".to_string())?;

        let mut status = self.status.lock().unwrap();
        let steam_root = &inst.root;

        let binaries = scan_steam_binaries(steam_root);

        let mut steam_client: Option<BinaryFile> = None;
        let mut steam_ui: Option<BinaryFile> = None;

        for binary in &binaries {
            let name = binary.path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.contains("steamclient") {
                steam_client = Some(binary.clone());
                status.steam_client_found = true;
            } else if name.contains("steamui") {
                steam_ui = Some(binary.clone());
                status.steam_ui_found = true;
            }
        }

        let mut sc_patterns = None;
        let mut su_patterns = None;

        if let Some(client) = &steam_client {
            match self
                .pattern_db
                .lookup_by_binary(client, PatternChannel::Pattern, BinaryComponent::SteamClient)
            {
                Ok(pf) => {
                    sc_patterns = Some(format!(
                        "{:?}",
                        pf.patterns.keys().collect::<Vec<_>>()
                    ));
                    status.patterns_loaded = true;
                }
                Err(e) => {
                    return Err(format!(
                        "steamclient pattern lookup failed: {} (sha256={})",
                        e,
                        client.sha256_hex()
                    ));
                }
            }
        }

        if let Some(ui) = &steam_ui {
            match self
                .pattern_db
                .lookup_by_binary(ui, PatternChannel::Pattern, BinaryComponent::SteamUi)
            {
                Ok(pf) => {
                    su_patterns = Some(format!(
                        "{:?}",
                        pf.patterns.keys().collect::<Vec<_>>()
                    ));
                }
                Err(e) => {
                    return Err(format!(
                        "steamui pattern lookup failed: {} (sha256={})",
                        e,
                        ui.sha256_hex()
                    ));
                }
            }
        }

        drop(status);

        Ok(BridgeInstallResult {
            success: true,
            message: format!(
                "Scanned {} Steam binaries. steamclient={} steamui={}",
                binaries.len(),
                steam_client.is_some(),
                steam_ui.is_some()
            ),
            steam_client_patterns: sc_patterns,
            steam_ui_patterns: su_patterns,
        })
    }

    /// Install the integration shared library into Steam via LD_PRELOAD.
    pub fn install_integration(
        &mut self,
        enable_system_preload: bool,
    ) -> Result<BridgeInstallResult, String> {
        let inst = self
            .steam_installation
            .as_ref()
            .ok_or_else(|| "Steam not detected".to_string())?;

        let lib_path = preload_loader::default_integration_lib_path();

        if !lib_path.exists() {
            let msg = format!(
                "Integration library not found at {}. \
                 Build libsteam_integration.so separately and place it there, \
                 then re-run install_integration.",
                lib_path.display()
            );
            let mut status = self.status.lock().unwrap();
            status.integration_active = false;
            return Ok(BridgeInstallResult {
                success: false,
                message: msg,
                steam_client_patterns: None,
                steam_ui_patterns: None,
            });
        }

        let steam_exe = inst.root.join("steam");
        let launcher = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".local/bin/luatools-steam-launcher");

        preload_loader::generate_ld_preload_launcher(&steam_exe, &lib_path, &launcher)
            .map_err(|e| format!("Failed to generate launcher: {}", e))?;

        if enable_system_preload {
            preload_loader::enable_system_preload(&lib_path).map_err(|e| {
                format!(
                    "Failed to enable system ld.so.preload (may need root): {}",
                    e
                )
            })?;
        }

        let mut status = self.status.lock().unwrap();
        status.integration_lib_path = Some(lib_path);
        status.integration_active = steam_integrated();

        Ok(BridgeInstallResult {
            success: true,
            message: format!(
                "Integration library registered. Launcher: {}. System preload: {}.",
                launcher.display(),
                enable_system_preload
            ),
            steam_client_patterns: None,
            steam_ui_patterns: None,
        })
    }

    /// Uninstall / remove the integration.
    pub fn uninstall_integration(&mut self) -> Result<(), String> {
        let lib_path = preload_loader::default_integration_lib_path();
        preload_loader::disable_system_preload(&lib_path).map_err(|e| e.to_string())?;
        let mut status = self.status.lock().unwrap();
        status.integration_active = false;
        status.integration_lib_path = None;
        Ok(())
    }

    /// Get ticket data for an app.
    pub fn get_tickets(&self, app_id: u32) -> Option<AppTicketData> {
        self.ticket_store.lock().unwrap().get(app_id).ok()
    }

    /// Set ticket data for an app.
    pub fn set_tickets(&self, data: AppTicketData) -> Result<(), String> {
        self.ticket_store
            .lock()
            .unwrap()
            .set(data)
            .map_err(|e| e.to_string())
    }

    /// Remove ticket data for an app.
    pub fn remove_tickets(&self, app_id: u32) -> Result<(), String> {
        self.ticket_store
            .lock()
            .unwrap()
            .remove(app_id)
            .map_err(|e| e.to_string())
    }

    /// List all apps that have stored tickets.
    pub fn list_ticket_apps(&self) -> Vec<u32> {
        self.ticket_store
            .lock()
            .unwrap()
            .list_app_ids()
            .unwrap_or_default()
    }

    /// Get the current bridge status.
    pub fn status(&self) -> BridgeStatus {
        self.status.lock().unwrap().clone()
    }

    /// Get a reference to the pattern DB (for advanced pattern queries).
    pub fn pattern_db(&self) -> &PatternDb {
        &self.pattern_db
    }

    /// Send an IPC message to the integration library running inside Steam.
    pub fn send_ipc_message(
        &self,
        msg: ipc::IpcMessage,
    ) -> Result<Option<ipc::IpcMessage>, String> {
        let client = IpcClient::new();
        client.send(&msg)
    }
}

/// Global singleton accessor (used by the integration library via IPC or FFI).
static BRIDGE_SERVICE: Mutex<Option<SteamBridgeService>> = Mutex::new(None);

/// Initialize the global bridge service.
pub fn init_global_service() -> SteamBridgeService {
    let svc = SteamBridgeService::new();
    *BRIDGE_SERVICE.lock().unwrap() = Some(svc.clone());
    svc
}

/// Get the global bridge service.
pub fn global_service() -> Option<SteamBridgeService> {
    BRIDGE_SERVICE.lock().unwrap().clone()
}
