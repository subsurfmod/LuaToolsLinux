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
        let name = info.name.trim();
        if !is_safe_plugin_name(name) {
            return Err(
                "Plugin name is required and must not contain path separators or traversal segments"
                    .into(),
            );
        }
        if info.version.trim().is_empty() {
            return Err("Plugin version is required".into());
        }
        let url = info.download_url.trim();
        if url.is_empty() {
            return Err("Plugin download URL is required".into());
        }
        if !url.starts_with("http://")
            && !url.starts_with("https://")
            && !url.starts_with("file://")
        {
            return Err("Plugin download URL must start with http://, https://, or file://".into());
        }
        if let Some(ref csum) = info.checksum {
            let clean = csum
                .trim()
                .strip_prefix("sha256:")
                .or_else(|| csum.trim().strip_prefix("SHA256:"))
                .unwrap_or(csum.trim());
            if clean.len() != 64 || !clean.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(
                    "Plugin checksum must be a valid 64-character hex SHA-256 digest".into(),
                );
            }
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

        let clean_name = info.name.trim();
        let plugin_dir = self.plugin_dir.join(clean_name);
        if fs::create_dir_all(&plugin_dir).is_err() {
            return PluginInstallResult {
                success: false,
                plugin_name: clean_name.to_string(),
                installed_path: None,
                error_message: Some("Failed to create plugin directory".into()),
            };
        }

        let mut normalized = info.clone();
        normalized.name = clean_name.to_string();
        let meta_path = plugin_dir.join("plugin.json");
        if let Ok(js) = serde_json::to_string_pretty(&normalized) {
            if let Err(e) = fs::write(&meta_path, js) {
                let _ = fs::remove_dir_all(&plugin_dir);
                return PluginInstallResult {
                    success: false,
                    plugin_name: clean_name.to_string(),
                    installed_path: None,
                    error_message: Some(format!("Failed to write plugin metadata: {}", e)),
                };
            }
        }

        PluginInstallResult {
            success: true,
            plugin_name: clean_name.to_string(),
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
                        let meta_path = path.join("plugin.json");
                        if let Ok(raw) = fs::read_to_string(&meta_path) {
                            if let Ok(info) = serde_json::from_str::<PluginInfo>(&raw) {
                                plugins.push(info);
                                continue;
                            }
                        }
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
        plugins.sort_by(|a, b| a.name.cmp(&b.name));
        plugins
    }

    pub fn remove_plugin(&self, name: &str) -> bool {
        let clean = name.trim();
        if !is_safe_plugin_name(clean) {
            return false;
        }
        let plugin_dir = self.plugin_dir.join(clean);
        if plugin_dir.is_dir() {
            fs::remove_dir_all(&plugin_dir).is_ok()
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
        let dir = tempdir().unwrap();
        let service = PluginAddService::new(Some(dir.path()));
        let info = PluginInfo {
            name: "TestPlugin".into(),
            version: "1.0.0".into(),
            author: "Test".into(),
            description: "Test plugin".into(),
            download_url: "https://example.com/plugin.zip".into(),
            checksum: Some("ab".repeat(32)),
            dependencies: Vec::new(),
            min_version: None,
        };
        assert!(service.validate_plugin(&info).is_ok());

        for bad_name in ["", "   ", "../escape", "a/b", "a\\b", "..", "."] {
            let mut bad = info.clone();
            bad.name = bad_name.into();
            assert!(
                service.validate_plugin(&bad).is_err(),
                "expected '{}' to be rejected",
                bad_name
            );
        }

        let mut bad_url = info.clone();
        bad_url.download_url = "ftp://example.com/p.zip".into();
        assert!(service.validate_plugin(&bad_url).is_err());

        let mut bad_checksum = info.clone();
        bad_checksum.checksum = Some("not-a-sha256".into());
        assert!(service.validate_plugin(&bad_checksum).is_err());
    }

    #[test]
    fn test_add_list_and_remove_plugin_persists_metadata_and_blocks_traversal() {
        let dir = tempdir().unwrap();
        let service = PluginAddService::new(Some(dir.path()));
        let info = PluginInfo {
            name: "TestPlugin".into(),
            version: "2.3.4".into(),
            author: "LuaTools".into(),
            description: "Real description".into(),
            download_url: "https://example.com/plugin.zip".into(),
            checksum: None,
            dependencies: vec!["core".into()],
            min_version: Some("2.9.0".into()),
        };
        let result = service.add_plugin(&info);
        assert!(result.success);
        assert!(result.installed_path.is_some());

        let listed = service.list_plugins();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].version, "2.3.4");
        assert_eq!(listed[0].author, "LuaTools");
        assert_eq!(listed[0].dependencies, vec!["core"]);

        assert!(!service.remove_plugin(".."));
        assert!(!service.remove_plugin("../TestPlugin"));
        assert!(service.remove_plugin("TestPlugin"));
        assert!(service.list_plugins().is_empty());
    }
}
