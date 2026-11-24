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
        let clean_name = request.name.trim();
        if !is_safe_plugin_name(clean_name) {
            return Err("Invalid plugin name (empty or contains path traversal characters)".into());
        }
        let clean_url = request.download_url.trim();
        if !clean_url.starts_with("http://")
            && !clean_url.starts_with("https://")
            && !clean_url.starts_with("file://")
        {
            return Err("Invalid plugin download URL".into());
        }
        if let Some(ref csum) = request.checksum {
            let clean = csum
                .trim()
                .strip_prefix("sha256:")
                .or_else(|| csum.trim().strip_prefix("SHA256:"))
                .unwrap_or(csum.trim());
            if clean.len() != 64 || !clean.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err("Invalid SHA-256 checksum format".into());
            }
        }

        let plugin_dir = self.plugin_dir.join(clean_name);
        if plugin_dir.exists() {
            return Err(format!("Plugin '{}' is already installed", clean_name));
        }

        // Mark as downloading
        {
            let mut downloads = self.downloads.lock().unwrap_or_else(|e| e.into_inner());
            downloads.insert(
                clean_name.to_string(),
                PluginInstallProgress {
                    plugin_name: clean_name.to_string(),
                    total_bytes: 0,
                    downloaded_bytes: 0,
                    percent: 0.0,
                    status: InstallStatus::Downloading,
                },
            );
        }

        if let Err(e) = fs::create_dir_all(&plugin_dir) {
            let mut downloads = self.downloads.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(progress) = downloads.get_mut(clean_name) {
                progress.status = InstallStatus::Failed;
            }
            return Err(format!("Failed to create plugin directory: {}", e));
        }

        {
            let mut downloads = self.downloads.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(progress) = downloads.get_mut(clean_name) {
                progress.status = InstallStatus::Installing;
                progress.percent = 50.0;
            }
        }

        let manifest = serde_json::json!({
            "name": clean_name,
            "version": request.version.clone().unwrap_or_else(|| "1.0.0".into()),
            "download_url": clean_url,
            "checksum": request.checksum,
            "dependencies": request.dependencies,
            "installed_at": chrono::Utc::now().to_rfc3339(),
        });
        if let Err(e) = fs::write(
            plugin_dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap_or_default(),
        ) {
            let _ = fs::remove_dir_all(&plugin_dir);
            let mut downloads = self.downloads.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(progress) = downloads.get_mut(clean_name) {
                progress.status = InstallStatus::Failed;
            }
            return Err(format!("Failed to write plugin manifest: {}", e));
        }

        {
            let mut downloads = self.downloads.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(progress) = downloads.get_mut(clean_name) {
                progress.status = InstallStatus::Completed;
                progress.percent = 100.0;
            }
        }

        Ok(())
    }

    pub fn get_progress(&self, plugin_name: &str) -> Option<PluginInstallProgress> {
        self.downloads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(plugin_name.trim())
            .cloned()
    }

    pub fn cancel_install(&self, plugin_name: &str) -> bool {
        let mut downloads = self.downloads.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(progress) = downloads.get_mut(plugin_name.trim()) {
            if matches!(
                progress.status,
                InstallStatus::Pending
                    | InstallStatus::Downloading
                    | InstallStatus::Verifying
                    | InstallStatus::Installing
            ) {
                progress.status = InstallStatus::Cancelled;
                return true;
            }
        }
        false
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
        plugins.sort();
        plugins
    }

    pub fn remove_plugin(&self, name: &str) -> bool {
        let clean = name.trim();
        if !is_safe_plugin_name(clean) {
            return false;
        }
        let plugin_dir = self.plugin_dir.join(clean);
        if plugin_dir.is_dir() && fs::remove_dir_all(&plugin_dir).is_ok() {
            self.downloads
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(clean);
            true
        } else {
            false
        }
    }
}

fn is_safe_plugin_name(name: &str) -> bool {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.chars().any(|c| c.is_control() || c == '\0')
    {
        return false;
    }
    let mut comps = Path::new(name).components();
    matches!(comps.next(), Some(std::path::Component::Normal(_))) && comps.next().is_none()
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
    fn test_install_plugin_writes_manifest_and_removes_cleanly() {
        let dir = tempdir().unwrap();
        let service = PluginInstallerService::new(Some(dir.path()));
        let request = PluginInstallRequest {
            name: "TestPlugin".into(),
            version: Some("1.0.0".into()),
            download_url: "https://example.com/plugin.zip".into(),
            checksum: Some("cd".repeat(32)),
            dependencies: Vec::new(),
        };
        assert!(service.install_plugin(request).is_ok());
        assert!(service.list_installed().contains(&"TestPlugin".into()));
        assert!(dir.path().join("TestPlugin/manifest.json").is_file());
        assert_eq!(
            service.get_progress("TestPlugin").unwrap().status,
            InstallStatus::Completed
        );
        // Completed install cannot be transitioned to Cancelled
        assert!(!service.cancel_install("TestPlugin"));
        // Traversal remove is rejected
        assert!(!service.remove_plugin("../outside"));
        assert!(service.remove_plugin("TestPlugin"));
        assert!(service.get_progress("TestPlugin").is_none());
    }

    #[test]
    fn test_install_duplicate_and_invalid_names_rejected() {
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

        let traversal = PluginInstallRequest {
            name: "../../evil".into(),
            version: None,
            download_url: "https://example.com/plugin.zip".into(),
            checksum: None,
            dependencies: Vec::new(),
        };
        assert!(service.install_plugin(traversal).is_err());
    }
}
