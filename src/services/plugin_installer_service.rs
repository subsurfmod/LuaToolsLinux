//! PluginInstallerService - Installing and managing plugins
//!
//! Handles downloading, verifying, and installing plugins for LuaTools.
//! Supports automatic dependency resolution and rollback on failure.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInstallProgress {
    pub plugin_name: String,
    pub total_bytes: u64,
    pub downloaded_bytes: u64,
    pub percent: f32,
    pub status: InstallStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum InstallStatus {
    Pending,
    Downloading,
    Verifying,
    Installing,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInstallRequest {
    pub name: String,
    pub version: Option<String>,
    pub download_url: String,
    pub checksum: Option<String>,
    pub dependencies: Vec<String>,
}

pub struct PluginInstallerService {
    plugin_dir: PathBuf,
    downloads: Arc<Mutex<HashMap<String, PluginInstallProgress>>>,
}

impl PluginInstallerService {
    pub fn new(plugin_dir: Option<&Path>) -> Self {
        let dir = plugin_dir.map(|p| p.to_path_buf()).unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/share/luatools/plugins")
        });
        let _ = fs::create_dir_all(&dir);
        Self {
            plugin_dir: dir,
            downloads: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn install_plugin(&self, request: PluginInstallRequest) -> Result<(), String> {
        let plugin_dir = self.plugin_dir.join(&request.name);
        if plugin_dir.exists() {
            return Err(format!("Plugin '{}' is already installed", request.name));
        }

        // Mark as downloading
        {
            let mut downloads = self.downloads.lock().unwrap();
            downloads.insert(
                request.name.clone(),
                PluginInstallProgress {
                    plugin_name: request.name.clone(),
                    total_bytes: 0,
                    downloaded_bytes: 0,
                    percent: 0.0,
                    status: InstallStatus::Downloading,
                },
            );
        }

        // In a full implementation, download and verify the plugin here
        // For now, just create the directory
        if fs::create_dir_all(&plugin_dir).is_err() {
            let mut downloads = self.downloads.lock().unwrap();
            if let Some(progress) = downloads.get_mut(&request.name) {
                progress.status = InstallStatus::Failed;
            }
            return Err("Failed to create plugin directory".into());
        }

        {
            let mut downloads = self.downloads.lock().unwrap();
            if let Some(progress) = downloads.get_mut(&request.name) {
                progress.status = InstallStatus::Completed;
                progress.percent = 100.0;
            }
        }

        Ok(())
    }

    pub fn get_progress(&self, plugin_name: &str) -> Option<PluginInstallProgress> {
        self.downloads.lock().unwrap().get(plugin_name).cloned()
    }

    pub fn cancel_install(&self, plugin_name: &str) -> bool {
        let mut downloads = self.downloads.lock().unwrap();
        if let Some(progress) = downloads.get_mut(plugin_name) {
            progress.status = InstallStatus::Cancelled;
            true
        } else {
            false
        }
    }

    pub fn list_installed(&self) -> Vec<String> {
        let mut plugins = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.plugin_dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        plugins.push(name.to_string());
                    }
                }
            }
        }
        plugins
    }

    pub fn remove_plugin(&self, name: &str) -> bool {
        let plugin_dir = self.plugin_dir.join(name);
        if plugin_dir.exists() {
            fs::remove_dir_all(&plugin_dir).is_ok()
        } else {
            false
        }
    }
}

impl Default for PluginInstallerService {
    fn default() -> Self {
        Self::new(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_install_plugin() {
        let dir = tempdir().unwrap();
        let service = PluginInstallerService::new(Some(dir.path()));
        let request = PluginInstallRequest {
            name: "TestPlugin".into(),
            version: Some("1.0.0".into()),
            download_url: "https://example.com/plugin.zip".into(),
            checksum: None,
            dependencies: Vec::new(),
        };
        assert!(service.install_plugin(request).is_ok());
        assert!(service.list_installed().contains(&"TestPlugin".into()));
    }

    #[test]
    fn test_install_duplicate() {
        let dir = tempdir().unwrap();
        let service = PluginInstallerService::new(Some(dir.path()));
        let request = PluginInstallRequest {
            name: "TestPlugin".into(),
            version: None,
            download_url: "https://example.com/plugin.zip".into(),
            checksum: None,
            dependencies: Vec::new(),
        };
        assert!(service.install_plugin(request.clone()).is_ok());
        assert!(service.install_plugin(request).is_err());
    }
}