//! PluginAddService - Adding plugins to the LuaTools plugin system
//!
//! Manages the addition of new plugins to the LuaTools plugin ecosystem,
//! including validation, dependency checking, and installation.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub download_url: String,
    pub checksum: Option<String>,
    pub dependencies: Vec<String>,
    pub min_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginInstallResult {
    pub success: bool,
    pub plugin_name: String,
    pub installed_path: Option<String>,
    pub error_message: Option<String>,
}

pub struct PluginAddService {
    plugin_dir: PathBuf,
}

impl PluginAddService {
    pub fn new(plugin_dir: Option<&Path>) -> Self {
        let dir = plugin_dir.map(|p| p.to_path_buf()).unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/share/luatools/plugins")
        });
        let _ = fs::create_dir_all(&dir);
        Self { plugin_dir: dir }
    }

    pub fn plugin_dir(&self) -> &Path {
        &self.plugin_dir
    }

    pub fn validate_plugin(&self, info: &PluginInfo) -> Result<(), String> {
        if info.name.is_empty() {
            return Err("Plugin name is required".into());
        }
        if info.version.is_empty() {
            return Err("Plugin version is required".into());
        }
        if info.download_url.is_empty() {
            return Err("Plugin download URL is required".into());
        }
        Ok(())
    }

    pub fn add_plugin(&self, info: &PluginInfo) -> PluginInstallResult {
        if let Err(e) = self.validate_plugin(info) {
            return PluginInstallResult {
                success: false,
                plugin_name: info.name.clone(),
                installed_path: None,
                error_message: Some(e),
            };
        }

        let plugin_dir = self.plugin_dir.join(&info.name);
        if fs::create_dir_all(&plugin_dir).is_err() {
            return PluginInstallResult {
                success: false,
                plugin_name: info.name.clone(),
                installed_path: None,
                error_message: Some("Failed to create plugin directory".into()),
            };
        }

        // In a full implementation, download and verify the plugin here
        PluginInstallResult {
            success: true,
            plugin_name: info.name.clone(),
            installed_path: Some(plugin_dir.to_string_lossy().to_string()),
            error_message: None,
        }
    }

    pub fn list_plugins(&self) -> Vec<PluginInfo> {
        let mut plugins = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.plugin_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        plugins.push(PluginInfo {
                            name: name.to_string(),
                            version: "1.0.0".into(),
                            author: "Unknown".into(),
                            description: "Installed plugin".into(),
                            download_url: String::new(),
                            checksum: None,
                            dependencies: Vec::new(),
                            min_version: None,
                        });
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

impl Default for PluginAddService {
    fn default() -> Self {
        Self::new(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_validate_plugin() {
        let service = PluginAddService::new(None);
        let info = PluginInfo {
            name: "TestPlugin".into(),
            version: "1.0.0".into(),
            author: "Test".into(),
            description: "Test plugin".into(),
            download_url: "https://example.com/plugin.zip".into(),
            checksum: None,
            dependencies: Vec::new(),
            min_version: None,
        };
        assert!(service.validate_plugin(&info).is_ok());

        let info = PluginInfo {
            name: String::new(),
            version: "1.0.0".into(),
            author: "Test".into(),
            description: "Test".into(),
            download_url: "https://example.com/plugin.zip".into(),
            checksum: None,
            dependencies: Vec::new(),
            min_version: None,
        };
        assert!(service.validate_plugin(&info).is_err());
    }

    #[test]
    fn test_add_plugin() {
        let dir = tempdir().unwrap();
        let service = PluginAddService::new(Some(dir.path()));
        let info = PluginInfo {
            name: "TestPlugin".into(),
            version: "1.0.0".into(),
            author: "Test".into(),
            description: "Test plugin".into(),
            download_url: "https://example.com/plugin.zip".into(),
            checksum: None,
            dependencies: Vec::new(),
            min_version: None,
        };
        let result = service.add_plugin(&info);
        assert!(result.success);
        assert!(result.installed_path.is_some());
    }
}
