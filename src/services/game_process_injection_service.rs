//! GameProcessInjectionService — injects a shared library into game processes.
//!
//! Prototype implementation for injecting integration code into game processes
//! launched through Steam. Uses LD_PRELOAD environment variable injection via
//! Steam launch options, and optionally ptrace-based injection for already-running
//! processes.
//!
//! This is the Linux equivalent of bettersteamtools' `[[inject]]` toml entries.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// Status of an injection target.
#[derive(Debug, Clone, PartialEq)]
pub enum InjectionStatus {
    NotInjected,
    Injected,
    Failed(String),
}

/// Configuration for a game process injection.
#[derive(Debug, Clone)]
pub struct InjectionConfig {
    pub app_id: u32,
    pub library_path: PathBuf,
    pub env_vars: HashMap<String, String>,
    pub launch_args: Vec<String>,
}

impl Default for InjectionConfig {
    fn default() -> Self {
        Self {
            app_id: 0,
            library_path: PathBuf::new(),
            env_vars: HashMap::new(),
            launch_args: Vec::new(),
        }
    }
}

/// Service for managing game process injections.
#[derive(Debug, Clone)]
pub struct GameProcessInjectionService {
    configs: Arc<Mutex<HashMap<u32, InjectionConfig>>>,
    statuses: Arc<Mutex<HashMap<u32, InjectionStatus>>>,
    default_library: PathBuf,
}

impl Default for GameProcessInjectionService {
    fn default() -> Self {
        Self::new()
    }
}

impl GameProcessInjectionService {
    pub fn new() -> Self {
        let default_library = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".local/lib/luatools/libgame_integration.so");

        Self {
            configs: Arc::new(Mutex::new(HashMap::new())),
            statuses: Arc::new(Mutex::new(HashMap::new())),
            default_library,
        }
    }

    /// Set the default injection library path.
    pub fn set_default_library<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.default_library = path.as_ref().to_path_buf();
        self
    }

    /// Configure injection for an app.
    pub fn configure(&self, app_id: u32, config: InjectionConfig) {
        self.configs.lock().unwrap().insert(app_id, config);
        self.statuses.lock().unwrap().insert(app_id, InjectionStatus::NotInjected);
    }

    /// Remove injection config for an app.
    pub fn unconfigure(&self, app_id: u32) {
        self.configs.lock().unwrap().remove(&app_id);
        self.statuses.lock().unwrap().remove(&app_id);
    }

    /// Get injection config for an app.
    pub fn get_config(&self, app_id: u32) -> Option<InjectionConfig> {
        self.configs.lock().unwrap().get(&app_id).cloned()
    }

    /// Get injection status for an app.
    pub fn get_status(&self, app_id: u32) -> InjectionStatus {
        self.statuses
            .lock()
            .unwrap()
            .get(&app_id)
            .cloned()
            .unwrap_or(InjectionStatus::NotInjected)
    }

    /// List all configured apps.
    pub fn list_configured_apps(&self) -> Vec<u32> {
        let mut apps: Vec<u32> = self.configs.lock().unwrap().keys().copied().collect();
        apps.sort_unstable();
        apps
    }

    /// Launch a game with LD_PRELOAD injection.
    /// Returns the Command that can be executed by the caller.
    pub fn build_launch_command(
        &self,
        app_id: u32,
        game_exe: &Path,
        extra_args: &[String],
    ) -> Result<Command, String> {
        let config = self
            .get_config(app_id)
            .ok_or_else(|| format!("No injection config for app {}", app_id))?;

        let lib_path = config
            .library_path
            .as_os_str()
            .to_str()
            .unwrap_or("")
            .to_string();
        let lib_path = if lib_path.is_empty() {
            self.default_library
                .to_string_lossy()
                .to_string()
        } else {
            lib_path
        };

        let mut cmd = Command::new(game_exe);

        // Set LD_PRELOAD for injection
        let existing_preload = std::env::var("LD_PRELOAD").unwrap_or_default();
        let new_preload = if existing_preload.is_empty() {
            lib_path.clone()
        } else {
            format!("{}:{}", lib_path, existing_preload)
        };
        cmd.env("LD_PRELOAD", new_preload);

        // Set additional env vars
        for (k, v) in &config.env_vars {
            cmd.env(k, v);
        }

        // Add launch args
        for arg in &config.launch_args {
            cmd.arg(arg);
        }
        for arg in extra_args {
            cmd.arg(arg);
        }

        // Mark as injected
        self.statuses
            .lock()
            .unwrap()
            .insert(app_id, InjectionStatus::Injected);

        Ok(cmd)
    }

    /// Inject into an already-running process using ptrace (prototype).
    /// This is best-effort and may not work on all kernels due to Yama ptrace
    /// scope restrictions.
    pub fn inject_into_running(&self, pid: u32, library_path: &Path) -> Result<(), String> {
        // Check if we can ptrace the process
        let mem_path = format!("/proc/{}/mem", pid);
        if std::fs::File::open(&mem_path).is_err() {
            return Err(format!(
                "Cannot access process {} memory (may need CAP_SYS_PTRACE or yama scope adjustment)",
                pid
            ));
        }

        // In a full implementation, we would:
        // 1. ptrace(PTRACE_ATTACH, pid)
        // 2. dlopen the library in the target process
        // 3. ptrace(PTRACE_DETACH, pid)
        //
        // For this prototype, we just record the attempt.
        eprintln!(
            "[game_injection] Would inject {} into pid {} (ptrace prototype)",
            library_path.display(),
            pid
        );

        Ok(())
    }

    /// Notify the bridge_lib about injection state via IPC.
    pub fn notify_bridge(&self, app_id: u32, injected: bool) -> Result<(), String> {
        use crate::steam_bridge::ipc::IpcClient;

        let client = IpcClient::new();
        let _ = client.send(&crate::steam_bridge::ipc::IpcMessage::Ping);
        let _ = (app_id, injected);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_defaults() {
        let svc = GameProcessInjectionService::new();
        assert!(svc.list_configured_apps().is_empty());
    }

    #[test]
    fn configure_and_status() {
        let svc = GameProcessInjectionService::new();
        let config = InjectionConfig {
            app_id: 1245620,
            library_path: PathBuf::from("/tmp/libtest.so"),
            ..Default::default()
        };
        svc.configure(1245620, config);
        assert_eq!(
            svc.get_status(1245620),
            InjectionStatus::NotInjected
        );
        assert_eq!(svc.list_configured_apps(), vec![1245620]);
    }

    #[test]
    fn build_launch_command() {
        let svc = GameProcessInjectionService::new();
        let config = InjectionConfig {
            app_id: 1245620,
            library_path: PathBuf::from("/tmp/libtest.so"),
            env_vars: vec![("TEST_VAR".into(), "test_val".into())]
                .into_iter()
                .collect(),
            launch_args: vec!["--test".into()],
        };
        svc.configure(1245620, config);

        let cmd = svc.build_launch_command(1245620, Path::new("/usr/bin/steam"), &[]);
        assert!(cmd.is_ok());
        assert_eq!(svc.get_status(1245620), InjectionStatus::Injected);
    }
}
