//! GameProcessInjectionService — configures per-game `LD_PRELOAD` shared-library
//! integration, persists per-app settings, and launches games with `LD_PRELOAD` set.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::models::config::default_luatools_data_dir;
use crate::steam_bridge::ipc::{IpcClient, IpcMessage};
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
}

impl Default for GameProcessInjectionService {
    fn default() -> Self {
        Self::with_storage(Some(
            default_luatools_data_dir().join("game_injections.json"),
        ))
    }
}

impl GameProcessInjectionService {
    pub fn new() -> Self {
        Self::with_storage(None)
    }

    pub fn with_storage(storage_path: Option<PathBuf>) -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let game_lib = home.join(".local/lib/luatools/libgame_integration.so");
        let default_library = if game_lib.is_file() {
            game_lib
        } else {
            default_integration_lib_path()
        };

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

        let mut cmd = Command::new(game_exe);
        let existing_preload = std::env::var("LD_PRELOAD").unwrap_or_default();
        let new_preload = if existing_preload.is_empty() {
            lib_path
        } else {
            format!("{}:{}", lib_path, existing_preload)
        };
        cmd.env("LD_PRELOAD", new_preload);

        // Configure Wine/Proton DLL overrides if executable is a Windows binary
        let is_windows_binary = game_exe
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("exe"))
            .unwrap_or(false);

        if is_windows_binary {
            let existing_overrides = std::env::var("WINEDLLOVERRIDES").unwrap_or_default();
            let new_overrides = if existing_overrides.is_empty() {
                "steam_api=n,b;steam_api64=n,b;version=n,b".to_string()
            } else if !existing_overrides.contains("steam_api") {
                format!("steam_api=n,b;steam_api64=n,b;{}", existing_overrides)
            } else {
                existing_overrides
            };
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
                    let _ = this.inject_into_running(pid, &this.default_library);
                    this.statuses
                        .lock()
                        .unwrap()
                        .insert(app_id, InjectionStatus::Injected);
                    let _ = this.notify_bridge(app_id, true);
                    break;
                }
            }
        })
    }

    /// Launches `app_id` via `steam -applaunch <app_id>` (or a discovered binary in `install_dir`)
    /// with `LD_PRELOAD` set to the configured integration library.
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

        let extra = vec!["-applaunch".to_string(), app_id.to_string()];
        let mut cmd = self.build_launch_command(app_id, Path::new("steam"), &extra)?;
        if let Some(dir) = install_dir.filter(|d| d.is_dir()) {
            cmd.current_dir(dir);
        }
        cmd.spawn().map_err(|e| {
            self.statuses
                .lock()
                .unwrap()
                .insert(app_id, InjectionStatus::Failed(e.to_string()));
            format!("Failed to launch Steam with LD_PRELOAD: {}", e)
        })?;
        Ok(format!(
            "Launched AppID {} via Steam with LD_PRELOAD integration.",
            app_id
        ))
    }

    pub fn inject_into_running(&self, pid: u32, library_path: &Path) -> Result<(), String> {
        let mem_path = format!("/proc/{}/mem", pid);
        if fs::File::open(&mem_path).is_err() {
            return Err(format!(
                "Cannot access process {} memory (requires CAP_SYS_PTRACE / Yama scope)",
                pid
            ));
        }
        eprintln!(
            "[game_injection] Would inject {} into pid {} (ptrace prototype)",
            library_path.display(),
            pid
        );
        Ok(())
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
        let svc = GameProcessInjectionService::with_storage(Some(path.clone()));
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

        let reloaded = GameProcessInjectionService::with_storage(Some(path));
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
}
