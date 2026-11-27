//! GameProcessInjectionService — configures per-game `LD_PRELOAD` shared-library
//! integration, persists per-app settings, and launches games with `LD_PRELOAD` set.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::models::config::default_luatools_data_dir;
use crate::steam_bridge::denuvo_auth::DenuvoAuth;
use crate::steam_bridge::ipc::{IpcClient, IpcMessage};
use crate::steam_bridge::ost_platform::pe::Image;
use crate::steam_bridge::ost_platform::process::enumerate_modules;
use crate::steam_bridge::ost_platform::remote_process::{
    get_architecture, inject_library, Architecture, InjectStatus,
};
use crate::steam_bridge::preload_loader::default_integration_lib_path;

/// Status of an injection target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InjectionStatus {
    NotInjected,
    Injected,
    Failed(String),
}

/// Configuration for a game process injection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InjectionConfig {
    pub app_id: u32,
    pub library_path: PathBuf,
    #[serde(default)]
    pub env_vars: HashMap<String, String>,
    #[serde(default)]
    pub launch_args: Vec<String>,
}

/// Service for managing per-game `LD_PRELOAD` process integrations.
#[derive(Debug, Clone)]
pub struct GameProcessInjectionService {
    configs: Arc<Mutex<HashMap<u32, InjectionConfig>>>,
    statuses: Arc<Mutex<HashMap<u32, InjectionStatus>>>,
    default_library: PathBuf,
    storage_path: Option<PathBuf>,
    denuvo_auth: Arc<DenuvoAuth>,
}

impl Default for GameProcessInjectionService {
    fn default() -> Self {
        Self::with_storage(
            Some(default_luatools_data_dir().join("game_injections.json")),
            DenuvoAuth::new(),
        )
    }
}

impl GameProcessInjectionService {
    pub fn new() -> Self {
        Self::with_storage(None, DenuvoAuth::new())
    }

    pub fn with_storage(storage_path: Option<PathBuf>, denuvo_auth: DenuvoAuth) -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let candidates = [
            home.join(".local/lib/luatools/libgame_integration.so"),
            home.join(".local/lib/luatools/libsteam_integration.so"),
            PathBuf::from("/usr/local/lib/luatools/libsteam_integration.so"),
            PathBuf::from("/usr/lib/luatools/libsteam_integration.so"),
        ];
        let default_library = candidates
            .into_iter()
            .find(|p| p.is_file())
            .unwrap_or_else(default_integration_lib_path);

        let mut map = HashMap::new();
        if let Some(ref path) = storage_path {
            if let Ok(raw) = fs::read_to_string(path) {
                if let Ok(list) = serde_json::from_str::<Vec<InjectionConfig>>(&raw) {
                    for cfg in list {
                        map.insert(cfg.app_id, cfg);
                    }
                }
            }
        }

        Self {
            configs: Arc::new(Mutex::new(map)),
            statuses: Arc::new(Mutex::new(HashMap::new())),
            default_library,
            storage_path,
            denuvo_auth: Arc::new(denuvo_auth),
        }
    }

    fn persist(&self, map: &HashMap<u32, InjectionConfig>) {
        let Some(ref path) = self.storage_path else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let mut list: Vec<&InjectionConfig> = map.values().collect();
        list.sort_by_key(|c| c.app_id);
        if let Ok(js) = serde_json::to_string_pretty(&list) {
            let _ = fs::write(path, js);
        }
    }

    pub fn default_library(&self) -> &Path {
        &self.default_library
    }

    pub fn set_default_library<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.default_library = path.as_ref().to_path_buf();
        self
    }

    pub fn is_configured(&self, app_id: u32) -> bool {
        self.configs.lock().unwrap().contains_key(&app_id)
    }

    pub fn configure(&self, app_id: u32, config: InjectionConfig) {
        let mut guard = self.configs.lock().unwrap();
        guard.insert(app_id, config);
        self.persist(&guard);
        drop(guard);
        self.statuses
            .lock()
            .unwrap()
            .insert(app_id, InjectionStatus::NotInjected);

        // BST-54: Scan the configured game executable/folder for Denuvo protection
        if let Some(cfg) = self.get_config(app_id) {
            if cfg.library_path.is_file() {
                let _ = self.denuvo_auth.scan_executable(&cfg.library_path);
            } else if cfg.library_path.is_dir() {
                let _ = self.denuvo_auth.scan_game_folder(app_id, &cfg.library_path);
            }
        }
    }

    pub fn unconfigure(&self, app_id: u32) {
        let mut guard = self.configs.lock().unwrap();
        guard.remove(&app_id);
        self.persist(&guard);
        drop(guard);
        self.statuses.lock().unwrap().remove(&app_id);
    }

    pub fn get_config(&self, app_id: u32) -> Option<InjectionConfig> {
        self.configs.lock().unwrap().get(&app_id).cloned()
    }

    pub fn get_status(&self, app_id: u32) -> InjectionStatus {
        self.statuses
            .lock()
            .unwrap()
            .get(&app_id)
            .cloned()
            .unwrap_or(InjectionStatus::NotInjected)
    }

    pub fn list_configured_apps(&self) -> Vec<u32> {
        let mut apps: Vec<u32> = self.configs.lock().unwrap().keys().copied().collect();
        apps.sort_unstable();
        apps
    }

    pub fn build_launch_command(
        &self,
        app_id: u32,
        game_exe: &Path,
        extra_args: &[String],
    ) -> Result<Command, String> {
        let config = self
            .get_config(app_id)
            .ok_or_else(|| format!("No integration config for AppID {}", app_id))?;

        let lib_str = config.library_path.to_string_lossy().trim().to_string();
        let lib_path = if lib_str.is_empty() {
            self.default_library.to_string_lossy().to_string()
        } else {
            lib_str
        };

        // Configure Wine/Proton launch command wrapper if executable is a Windows binary
        let is_windows_binary = game_exe
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("exe"))
            .unwrap_or(false);

        let mut cmd = if is_windows_binary {
            let runner = std::env::var("WINE").unwrap_or_else(|_| {
                crate::services::depot_downloader::which_in_path("wine")
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| "wine".to_string())
            });
            let mut c = Command::new(runner);
            c.arg(game_exe);
            c
        } else {
            Command::new(game_exe)
        };

        let existing_preload = std::env::var("LD_PRELOAD").unwrap_or_default();
        let new_preload = if existing_preload.is_empty() {
            lib_path.clone()
        } else {
            format!("{}:{}", lib_path, existing_preload)
        };
        cmd.env("LD_PRELOAD", new_preload);
        // Force bridge activation inside the launched game process (BST-62)
        cmd.env("LUATOOLS_BRIDGE_FORCE", "1");

        if is_windows_binary {
            if game_exe.is_file() {
                Self::validate_pe_executable(game_exe)?;
            }
            let existing_overrides = std::env::var("WINEDLLOVERRIDES").unwrap_or_default();
            let mut new_overrides = if existing_overrides.is_empty() {
                "steam_api=n,b;steam_api64=n,b;version=n,b".to_string()
            } else if !existing_overrides.contains("steam_api") {
                format!("steam_api=n,b;steam_api64=n,b;{}", existing_overrides)
            } else {
                existing_overrides
            };

            // If a custom Windows .dll is configured as the injection target, override it in Wine
            if let Some(ext) = config.library_path.extension().and_then(|e| e.to_str()) {
                if ext.eq_ignore_ascii_case("dll") {
                    if let Some(stem) = config.library_path.file_stem().and_then(|s| s.to_str()) {
                        if !new_overrides.contains(stem) {
                            new_overrides = format!("{}=n,b;{}", stem, new_overrides);
                        }
                    }
                }
            }
            cmd.env("WINEDLLOVERRIDES", new_overrides);
        }

        for (k, v) in &config.env_vars {
            cmd.env(k, v);
        }
        for arg in &config.launch_args {
            cmd.arg(arg);
        }
        for arg in extra_args {
            cmd.arg(arg);
        }

        self.statuses
            .lock()
            .unwrap()
            .insert(app_id, InjectionStatus::Injected);
        let _ = self.notify_bridge(app_id, true);

        Ok(cmd)
    }

    /// Launch a game with LD_PRELOAD integration (BST-62).
    ///
    /// Configures `app_id` with `lib_path`, verifies PE executable headers if Windows binary,
    /// configures `WINEDLLOVERRIDES`, sets `LUATOOLS_BRIDGE_FORCE=1`, and launches the game
    /// via `steam-sls` or direct executable.
    pub fn launch_game_with_preload(&self, app_id: u32, lib_path: &Path) -> Result<(), String> {
        let config = InjectionConfig {
            app_id,
            library_path: lib_path.to_path_buf(),
            ..Default::default()
        };
        self.configure(app_id, config);
        self.launch_with_integration(app_id, None).map(|_| ())
    }

    /// Finds game binary in installation directory. Prefers Linux binaries then `.exe`.
    pub fn find_game_binary_in_dir(dir: &Path) -> Option<PathBuf> {
        if !dir.is_dir() {
            return None;
        }
        let entries = fs::read_dir(dir).ok()?;
        let mut exe_fallback = None;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if ext.eq_ignore_ascii_case("exe") && exe_fallback.is_none() {
                        exe_fallback = Some(path.clone());
                    }
                } else {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if let Ok(meta) = path.metadata() {
                            if meta.permissions().mode() & 0o111 != 0 {
                                return Some(path);
                            }
                        }
                    }
                }
            }
        }
        exe_fallback
    }

    /// Spawns a background daemon thread that monitors running processes for `expected_exe` (or AppID).
    pub fn spawn_process_watcher(
        &self,
        app_id: u32,
        expected_exe: Option<String>,
    ) -> std::thread::JoinHandle<()> {
        let this = self.clone();
        std::thread::spawn(move || {
            let target_exe = expected_exe.unwrap_or_default();
            for _ in 0..60 {
                // Poll every 500ms up to 30s
                std::thread::sleep(std::time::Duration::from_millis(500));
                let mut found_pid: Option<u32> = None;
                if let Ok(proc_entries) = fs::read_dir("/proc") {
                    for entry in proc_entries.flatten() {
                        let path = entry.path();
                        if let Some(fname) = path.file_name().and_then(|n| n.to_str()) {
                            if let Ok(pid) = fname.parse::<u32>() {
                                let comm_path = path.join("comm");
                                if let Ok(comm) = fs::read_to_string(&comm_path) {
                                    let comm_trim = comm.trim();
                                    if !target_exe.is_empty()
                                        && comm_trim.eq_ignore_ascii_case(&target_exe)
                                    {
                                        found_pid = Some(pid);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }

                if let Some(pid) = found_pid {
                    log::info!(
                        "[process_watcher] Found target process {} (PID {}) for AppID {}",
                        target_exe,
                        pid,
                        app_id
                    );
                    // BST-54: Scan running process for Denuvo protection
                    let rep = crate::steam_bridge::denuvo_auth::scan_process_protection(pid);
                    if rep.denuvo_detected {
                        this.denuvo_auth.register_denuvo_app(app_id);
                    }
                    let status = match this.inject_into_running(pid, &this.default_library) {
                        Ok(()) => {
                            let _ = this.notify_bridge(app_id, true);
                            InjectionStatus::Injected
                        }
                        Err(e) => {
                            let _ = this.notify_bridge(app_id, false);
                            InjectionStatus::Failed(e)
                        }
                    };
                    this.statuses.lock().unwrap().insert(app_id, status);
                    break;
                }
            }
        })
    }

    /// Launches `app_id` via `steam-sls -applaunch <app_id>` (or a discovered binary in `install_dir`)
    /// with `LD_PRELOAD` set to the configured integration library without crashing 32-bit Steam.
    pub fn launch_with_integration(
        &self,
        app_id: u32,
        install_dir: Option<&Path>,
    ) -> Result<String, String> {
        if !self.is_configured(app_id) {
            self.configure(
                app_id,
                InjectionConfig {
                    app_id,
                    library_path: self.default_library.clone(),
                    ..Default::default()
                },
            );
        }

        // Detect if install_dir contains a Windows .exe binary
        let detected_binary = install_dir.and_then(Self::find_game_binary_in_dir);
        let is_windows_game = detected_binary
            .as_ref()
            .and_then(|p| p.extension().and_then(|e| e.to_str()))
            .map(|e| e.eq_ignore_ascii_case("exe"))
            .unwrap_or(false);

        // BST-54: Proactive Denuvo protection scan on executable or install folder
        if let Some(bin) = &detected_binary {
            let report = self.denuvo_auth.scan_executable(bin);
            if report.denuvo_detected {
                self.denuvo_auth.register_denuvo_app(app_id);
            }
        } else if let Some(dir) = install_dir {
            let _ = self.denuvo_auth.scan_game_folder(app_id, dir);
        }

        // Find steam-sls launcher (or fallback to luatools-steam-launcher or steam)
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let candidates = [
            home.join(".local/bin/steam-sls"),
            home.join(".local/bin/luatools-steam-launcher"),
            PathBuf::from("/usr/local/bin/luatools-steam-launcher"),
            PathBuf::from("steam-sls"),
            PathBuf::from("steam"),
        ];
        let launcher = candidates
            .iter()
            .find(|p| p.is_file() || p.as_os_str() == "steam-sls" || p.as_os_str() == "steam")
            .cloned()
            .unwrap_or_else(|| PathBuf::from("steam"));

        let extra = vec!["-applaunch".to_string(), app_id.to_string()];
        let mut cmd = self.build_launch_command(app_id, &launcher, &extra)?;
        if let Some(dir) = install_dir.filter(|d| d.is_dir()) {
            cmd.current_dir(dir);
        }

        // If launching Steam to run a Windows game, ensure WINEDLLOVERRIDES is set
        if is_windows_game {
            let existing_overrides = std::env::var("WINEDLLOVERRIDES").unwrap_or_default();
            let mut new_overrides = if existing_overrides.is_empty() {
                "steam_api=n,b;steam_api64=n,b;version=n,b".to_string()
            } else if !existing_overrides.contains("steam_api") {
                format!("steam_api=n,b;steam_api64=n,b;{}", existing_overrides)
            } else {
                existing_overrides
            };

            if let Some(cfg) = self.get_config(app_id) {
                if let Some(stem) = cfg.library_path.file_stem().and_then(|s| s.to_str()) {
                    if cfg.library_path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("dll")) {
                        if !new_overrides.contains(stem) {
                            new_overrides = format!("{}=n,b;{}", stem, new_overrides);
                        }
                    }
                }
            }
            cmd.env("WINEDLLOVERRIDES", new_overrides);

            // Deploy Goldberg emulator fallback if available (BST-62)
            if let Some(dir) = install_dir {
                if self.has_goldberg_fallback() {
                    let _ = self.apply_goldberg_fallback(app_id, dir);
                }
            }
        }

        // Guard execution during unit/integration tests to prevent spawning live Steam
        if std::env::var("LUATOOLS_MOCK_LAUNCH").map(|v| v == "1").unwrap_or(false)
            || cfg!(test)
        {
            let _target_exe = detected_binary
                .as_ref()
                .and_then(|p| p.file_name().and_then(|n| n.to_str()))
                .map(ToString::to_string);
            self.statuses
                .lock()
                .unwrap()
                .insert(app_id, InjectionStatus::Injected);
            return Ok(format!(
                "Mock launched AppID {} with integration (test environment guard).",
                app_id
            ));
        }

        cmd.spawn().map_err(|e| {
            self.statuses
                .lock()
                .unwrap()
                .insert(app_id, InjectionStatus::Failed(e.to_string()));
            format!("Failed to launch Steam with integration: {}", e)
        })?;

        // BST-62: Automatically spawn process watcher to catch the game process
        // even if Steam is already running and drops the launcher's LD_PRELOAD
        let target_exe = detected_binary
            .as_ref()
            .and_then(|p| p.file_name().and_then(|n| n.to_str()))
            .map(ToString::to_string);
        self.spawn_process_watcher(app_id, target_exe);

        Ok(format!(
            "Launched AppID {} via steam-sls with integration.",
            app_id
        ))
    }

    /// Check if Goldberg emulator fallback is available on the system (BST-62).
    pub fn has_goldberg_fallback(&self) -> bool {
        let goldberg_path = default_luatools_data_dir().join("goldberg");
        let cfg = steam_auto_patch_linux::AutoPatchConfig {
            goldberg_path,
            ..Default::default()
        };
        steam_auto_patch_linux::SteamAutoPatch::new(cfg)
            .map(|p| p.check_goldberg())
            .unwrap_or(false)
    }

    /// Automatically deploy Goldberg fallback emulator into a Windows game folder (BST-62).
    pub fn apply_goldberg_fallback(&self, app_id: u32, game_dir: &Path) -> Result<bool, String> {
        let goldberg_path = default_luatools_data_dir().join("goldberg");
        let cfg = steam_auto_patch_linux::AutoPatchConfig {
            apply_path: game_dir.to_path_buf(),
            goldberg_path,
            app_id: Some(app_id),
            generate_interfaces: true,
            ..Default::default()
        };
        let patcher = steam_auto_patch_linux::SteamAutoPatch::new(cfg)
            .map_err(|e| format!("Goldberg fallback configuration failed: {}", e))?;
        patcher.apply().map_err(|e| format!("Goldberg fallback patching failed: {}", e))
    }

    /// Validates that `path` is a valid PE executable via `ost_platform::pe::Image`.
    pub fn validate_pe_executable(path: &Path) -> Result<Image, String> {
        let img = Image::new(&path.to_string_lossy())
            .map_err(|e| format!("Invalid PE executable {}: {}", path.display(), e))?;
        if !img.is_valid() {
            return Err(format!("Invalid PE image: {}", path.display()));
        }
        Ok(img)
    }

    pub fn inject_into_running(&self, pid: u32, library_path: &Path) -> Result<(), String> {
        if pid == 0 {
            return Err("Invalid target PID 0".to_string());
        }

        let arch = get_architecture(pid);
        if arch == Architecture::Unknown {
            return Err(format!(
                "Cannot determine target architecture for PID {} (process exited or inaccessible)",
                pid
            ));
        }

        let modules = enumerate_modules(pid);
        if modules.is_empty() {
            return Err(format!(
                "No mapped modules found for PID {} (cannot inspect /proc/{}/maps)",
                pid, pid
            ));
        }

        for m in &modules {
            if m.native_path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
                && m.native_path.is_file()
            {
                Self::validate_pe_executable(&m.native_path)?;
                // BST-54: Proactive scan on target PE module
                let rep = self.denuvo_auth.scan_executable(&m.native_path);
                if rep.denuvo_detected {
                    log::info!(
                        "[denuvo] Proactive PE scan detected Denuvo ({:?}) in {}",
                        rep.method,
                        m.native_path.display()
                    );
                }
            }
        }

        let lib_name = library_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if !lib_name.is_empty()
            && modules.iter().any(|m| {
                m.native_path == library_path
                    || m.native_path.file_name().and_then(|n| n.to_str()) == Some(lib_name)
            })
        {
            log::info!(
                "[game_injection] Library {} already loaded in PID {} ({})",
                library_path.display(),
                pid,
                arch.as_str()
            );
            return Ok(());
        }

        match inject_library(pid, library_path) {
            InjectStatus::Ok => {
                log::info!(
                    "[game_injection] Successfully injected {} into PID {} ({})",
                    library_path.display(),
                    pid,
                    arch.as_str()
                );
                Ok(())
            }
            InjectStatus::ResolveLoadLibraryFailed => Err(format!(
                "PID {} ({}, {} modules inspected) attached cleanly via ptrace, but dlopen address resolution failed ({})",
                pid,
                arch.as_str(),
                modules.len(),
                InjectStatus::ResolveLoadLibraryFailed.as_str()
            )),
            other => Err(format!(
                "Failed to inject {} into PID {} ({}): {}",
                library_path.display(),
                pid,
                arch.as_str(),
                other.as_str()
            )),
        }
    }

    pub fn notify_bridge(&self, app_id: u32, injected: bool) -> Result<(), String> {
        let mut payload = HashMap::new();
        payload.insert("app_id".into(), app_id.to_string());
        payload.insert("injected".into(), injected.to_string());
        let client = IpcClient::new();
        let _ = client.send(&IpcMessage::Event {
            kind: "game_injection".into(),
            payload,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configures_persists_and_builds_launch_command() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("injections.json");
        let svc = GameProcessInjectionService::with_storage(Some(path.clone()), DenuvoAuth::new());
        assert!(svc.list_configured_apps().is_empty());

        let config = InjectionConfig {
            app_id: 1245620,
            library_path: PathBuf::from("/tmp/libtest.so"),
            env_vars: [("TEST_VAR".into(), "test_val".into())]
                .into_iter()
                .collect(),
            launch_args: vec!["--test".into()],
        };
        svc.configure(1245620, config);

        let reloaded = GameProcessInjectionService::with_storage(Some(path), DenuvoAuth::new());
        assert_eq!(reloaded.list_configured_apps(), vec![1245620]);
        assert!(reloaded
            .build_launch_command(1245620, Path::new("/usr/bin/steam"), &[])
            .is_ok());
        assert_eq!(reloaded.get_status(1245620), InjectionStatus::Injected);

        // Windows binary test for WINEDLLOVERRIDES
        let wine_cmd = reloaded
            .build_launch_command(1245620, Path::new("eldenring.exe"), &[])
            .unwrap();
        let envs: Vec<_> = wine_cmd.get_envs().collect();
        let has_wine_override = envs.iter().any(|(k, v)| {
            *k == "WINEDLLOVERRIDES" && v.map(|s| s.to_string_lossy().contains("steam_api=n,b")).unwrap_or(false)
        });
        assert!(has_wine_override);
    }

    #[test]
    fn validates_pe_executables_and_rejects_invalid_live_injection_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let bad_exe = tmp.path().join("bad.exe");
        fs::write(&bad_exe, b"not a valid PE file").unwrap();
        assert!(GameProcessInjectionService::validate_pe_executable(&bad_exe).is_err());

        // Build minimal valid PE32+ header
        let mut pe = vec![0u8; 0x200];
        pe[0..2].copy_from_slice(&0x5A4Du16.to_le_bytes()); // MZ
        pe[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        pe[0x80..0x84].copy_from_slice(&0x0000_4550u32.to_le_bytes()); // PE\0\0
        pe[0x84 + 16..0x84 + 18].copy_from_slice(&240u16.to_le_bytes());
        pe[0x84 + 20..0x84 + 22].copy_from_slice(&0x20Bu16.to_le_bytes());
        let good_exe = tmp.path().join("good.exe");
        fs::write(&good_exe, &pe).unwrap();
        assert!(GameProcessInjectionService::validate_pe_executable(&good_exe).is_ok());

        let svc = GameProcessInjectionService::with_storage(None, DenuvoAuth::new());
        svc.configure(
            1245620,
            InjectionConfig {
                app_id: 1245620,
                library_path: PathBuf::from("/tmp/libtest.so"),
                ..Default::default()
            },
        );
        assert!(svc.build_launch_command(1245620, &bad_exe, &[]).is_err());
        assert!(svc.build_launch_command(1245620, &good_exe, &[]).is_ok());

        assert!(svc
            .inject_into_running(0, Path::new("/nonexistent.so"))
            .is_err());
        assert!(svc
            .inject_into_running(std::process::id(), Path::new("/nonexistent.so"))
            .is_err());
    }
}
