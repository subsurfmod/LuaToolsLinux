//! PluginInstallerService - Installing and managing plugins
//!
//! Handles downloading, verifying, and installing plugins for LuaTools,
//! including remote LTSP (`madoiscool/LTSP`) download, payload validation,
//! dual-path deployment matching `cef_bridge::find_luatools_js`, and
//! Millennium coexistence reconciliation.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::services::plugin_log::PluginLog;

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

        PluginLog::global().info(
            Some("PluginInstaller"),
            &format!("Installed plugin '{}'", clean_name),
        );
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

    pub fn default_luatools_js() -> &'static str {
        DEFAULT_LUATOOLS_JS
    }

    /// Validates that a downloaded or provided `luatools.js` payload is non-empty
    /// and not an HTML error page.
    pub fn validate_js_payload(js_content: &str) -> Result<(), String> {
        let trimmed = js_content.trim();
        if trimmed.is_empty() {
            return Err("LTSP luatools.js payload is empty".into());
        }
        if trimmed.len() < 16 {
            return Err("LTSP luatools.js payload is suspiciously short".into());
        }
        let lower_prefix: String = trimmed.chars().take(64).collect::<String>().to_ascii_lowercase();
        if lower_prefix.starts_with("<!doctype")
            || lower_prefix.starts_with("<html")
            || lower_prefix.starts_with("<?xml")
        {
            return Err("LTSP payload appears to be an HTML/XML error page, not JavaScript".into());
        }
        Ok(())
    }

    /// Validates and writes `luatools.js` to BOTH searched paths matching
    /// `cef_bridge::find_luatools_js` and `find_installed_luatools_js`:
    /// 1. `self.plugin_dir.join("luatools/luatools.js")`
    /// 2. `<steam_root>/steamui/LuaTools/luatools.js` (when `steam_root` is provided)
    pub fn install_ltsp_js_payload(
        &self,
        js_content: &str,
        steam_root: Option<&Path>,
    ) -> Result<PathBuf, String> {
        Self::validate_js_payload(js_content)?;

        // 1. Canonical local plugin directory path: <plugin_dir>/luatools/luatools.js
        let primary = self.plugin_dir.join("luatools").join("luatools.js");
        write_file_atomic(&primary, js_content.as_bytes())
            .map_err(|e| format!("Failed to write {}: {}", primary.display(), e))?;

        if let Some(parent) = primary.parent() {
            let manifest = serde_json::json!({
                "name": "luatools",
                "version": crate::VERSION,
                "description": "LuaTools Steam Store Plugin (LTSP)",
                "main": "luatools.js",
                "installed_at": chrono::Utc::now().to_rfc3339(),
            });
            let _ = write_file_atomic(
                &parent.join("manifest.json"),
                serde_json::to_string_pretty(&manifest)
                    .unwrap_or_default()
                    .as_bytes(),
            );
        }

        // 2. Canonical Steam steamui path: <steam_root>/steamui/LuaTools/luatools.js
        let mut steamui_written: Option<PathBuf> = None;
        if let Some(sr) = steam_root {
            let steamui_target = sr.join("steamui/LuaTools/luatools.js");
            write_file_atomic(&steamui_target, js_content.as_bytes())
                .map_err(|e| format!("Failed to write {}: {}", steamui_target.display(), e))?;
            steamui_written = Some(steamui_target);
        }

        PluginLog::global().info(
            Some("PluginInstaller"),
            &format!(
                "Installed luatools.js ({} bytes) to {}{}",
                js_content.len(),
                primary.display(),
                steamui_written
                    .as_ref()
                    .map(|p| format!(" and {}", p.display()))
                    .unwrap_or_default()
            ),
        );

        Ok(steamui_written.unwrap_or(primary))
    }

    pub fn install_default_luatools_js(
        &self,
        custom_target: Option<&Path>,
    ) -> Result<PathBuf, String> {
        let target_file = if let Some(p) = custom_target {
            let is_js_file = p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("js"))
                .unwrap_or(false);
            if p.is_dir() || !is_js_file {
                p.join("luatools.js")
            } else {
                p.to_path_buf()
            }
        } else {
            self.plugin_dir.join("luatools").join("luatools.js")
        };

        write_file_atomic(&target_file, DEFAULT_LUATOOLS_JS.as_bytes())
            .map_err(|e| format!("Failed to write luatools.js: {}", e))?;

        if let Some(parent) = target_file.parent() {
            let manifest = serde_json::json!({
                "name": "luatools",
                "version": crate::VERSION,
                "description": "LuaTools Steam Store Plugin (LTSP)",
                "main": "luatools.js",
                "installed_at": chrono::Utc::now().to_rfc3339(),
            });
            let _ = write_file_atomic(
                &parent.join("manifest.json"),
                serde_json::to_string_pretty(&manifest)
                    .unwrap_or_default()
                    .as_bytes(),
            );
        }

        Ok(target_file)
    }

    pub fn is_luatools_js_installed(&self, steam_root: Option<&Path>) -> bool {
        self.find_installed_luatools_js(steam_root).is_some()
    }

    pub fn find_installed_luatools_js(&self, steam_root: Option<&Path>) -> Option<PathBuf> {
        let mut candidates = Vec::new();
        if let Some(sr) = steam_root {
            candidates.push(sr.join("steamui/LuaTools/luatools.js"));
        }
        candidates.push(self.plugin_dir.join("luatools").join("luatools.js"));
        if let Some(h) = dirs::home_dir() {
            candidates.push(h.join(".local/share/luatools/plugins/luatools/luatools.js"));
            candidates.push(h.join(".local/share/Steam/steamui/LuaTools/luatools.js"));
            candidates.push(h.join(".local/share/millennium/plugins/luatools/public/luatools.js"));
        }
        candidates.into_iter().find(|p| p.is_file())
    }

    fn extract_luatools_js_from_zip(zip_bytes: &[u8]) -> Result<String, String> {
        if zip_bytes.is_empty() {
            return Err("Downloaded LTSP plugin.zip was empty".into());
        }
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
            .map_err(|e| format!("Invalid plugin.zip archive: {}", e))?;
        for i in 0..archive.len() {
            if let Ok(mut file) = archive.by_index(i) {
                let name = file.name().to_ascii_lowercase();
                if name.ends_with("luatools.js") {
                    let mut js_content = String::new();
                    if file.read_to_string(&mut js_content).is_ok() {
                        Self::validate_js_payload(&js_content)?;
                        return Ok(js_content);
                    }
                }
            }
        }
        Err("luatools.js not found inside plugin.zip".into())
    }

    pub fn download_and_install_ltsp(
        &self,
        steam_root: Option<&Path>,
    ) -> Result<PathBuf, String> {
        // Try fetching the latest LTSP release from canonical repo madoiscool/LTSP with User-Agent;
        // if GitHub API is rate-limited, try direct latest release asset download;
        // on any network/validation error, fall back cleanly to the embedded luatools.js bundle.
        let downloaded_js: Option<String> = (|| -> Result<String, String> {
            let api_url = "https://api.github.com/repos/madoiscool/LTSP/releases/latest";
            let ua = format!("LuaToolsLinux/{}", crate::VERSION);
            let downloader =
                crate::services::proxied_file_downloader::ProxiedFileDownloader::new();

            let release_asset = (|| -> Result<(String, Option<String>), String> {
                let response = ureq::get(api_url)
                    .set("User-Agent", &ua)
                    .set("Accept", "application/vnd.github+json")
                    .timeout(std::time::Duration::from_secs(12))
                    .call()
                    .map_err(|e| format!("Failed to fetch LTSP release info: {}", e))?;
                let release: serde_json::Value = response
                    .into_json()
                    .map_err(|e| format!("Failed to parse LTSP release JSON: {}", e))?;

                let assets = release
                    .get("assets")
                    .and_then(|a| a.as_array())
                    .ok_or("No assets in LTSP release")?;
                let plugin_zip = assets
                    .iter()
                    .find(|a| {
                        a.get("name")
                            .and_then(|n| n.as_str())
                            .map(|n| n.eq_ignore_ascii_case("plugin.zip") || n.ends_with(".zip"))
                            .unwrap_or(false)
                    })
                    .ok_or("plugin.zip not found in LTSP release")?;

                let download_url = plugin_zip
                    .get("browser_download_url")
                    .and_then(|u| u.as_str())
                    .ok_or("No download URL in LTSP asset")?
                    .to_string();
                let expected_sha256 = plugin_zip
                    .get("digest")
                    .and_then(|d| d.as_str())
                    .map(|d| d.strip_prefix("sha256:").unwrap_or(d).to_string());
                Ok((download_url, expected_sha256))
            })();

            let (download_url, expected_sha256) = match release_asset {
                Ok(pair) => pair,
                Err(api_err) => {
                    PluginLog::global().warn(
                        Some("PluginInstaller"),
                        &format!(
                            "LTSP GitHub API query failed ({}); trying direct latest download",
                            api_err
                        ),
                    );
                    (
                        "https://github.com/madoiscool/LTSP/releases/latest/download/plugin.zip"
                            .to_string(),
                        None,
                    )
                }
            };

            let zip_bytes = downloader
                .download_bytes_sync(&download_url)
                .map_err(|e| format!("Failed to download plugin.zip: {}", e))?;
            if zip_bytes.is_empty() {
                return Err("Downloaded LTSP plugin.zip was empty".into());
            }

            if let Some(ref expected) = expected_sha256 {
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(&zip_bytes);
                let actual = hex::encode(hasher.finalize());
                if !actual.eq_ignore_ascii_case(expected) {
                    return Err(format!(
                        "SHA-256 mismatch: expected {}, got {}",
                        expected, actual
                    ));
                }
            }

            Self::extract_luatools_js_from_zip(&zip_bytes)
        })()
        .map_err(|e| {
            PluginLog::global().warn(
                Some("PluginInstaller"),
                &format!("Remote LTSP fetch fell back to embedded bundle: {}", e),
            );
            e
        })
        .ok();

        let target = if let Some(js_content) = downloaded_js {
            self.install_ltsp_js_payload(&js_content, steam_root)?
        } else {
            self.install_ltsp_js_payload(DEFAULT_LUATOOLS_JS, steam_root)?
        };

        let _ = self.register_steam_plugin(steam_root);
        let _ = crate::services::millennium_service::MillenniumService::register_luatools_plugin(
            steam_root,
        );
        Ok(target)
    }

    fn register_steam_plugin(&self, steam_root: Option<&Path>) -> Result<(), String> {
        let plugins_dir = if let Some(sr) = steam_root {
            sr.join("steamui/plugins")
        } else if let Some(h) = dirs::home_dir() {
            h.join(".local/share/Steam/steamui/plugins")
        } else {
            return Ok(());
        };

        let plugins_json = plugins_dir.join("plugins.json");
        let plugin_entry = serde_json::json!({
            "name": "LuaToolsSteamPlugin",
            "path": "../LuaTools/luatools.js",
            "enabled": true
        });

        let mut plugins = if plugins_json.exists() {
            fs::read_to_string(&plugins_json)
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .unwrap_or(serde_json::json!([]))
        } else {
            serde_json::json!([])
        };

        if let Some(arr) = plugins.as_array_mut() {
            arr.retain(|p| {
                p.get("name").and_then(|n| n.as_str()) != Some("LuaToolsSteamPlugin")
            });
            arr.push(plugin_entry);
        }

        let content = serde_json::to_string_pretty(&plugins)
            .map_err(|e| format!("Failed to serialize plugins.json: {}", e))?;
        write_file_atomic(&plugins_json, content.as_bytes())
            .map_err(|_| "Failed to register plugin in plugins.json".to_string())
    }

    pub fn ensure_luatools_js_installed(
        &self,
        steam_root: Option<&Path>,
    ) -> Result<PathBuf, String> {
        if let Some(p) = self.find_installed_luatools_js(steam_root) {
            return Ok(p);
        }
        self.install_ltsp_js_payload(DEFAULT_LUATOOLS_JS, steam_root)
    }
}

fn write_file_atomic(target: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = target.with_extension("tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, target)
}

pub const DEFAULT_LUATOOLS_JS: &str = r##"// LuaTools Store Plugin (LTSP) - Injected into Steam Store
(function() {
  if (window.__luatoolsStorePluginInjected) return;
  window.__luatoolsStorePluginInjected = true;

  function getAppId() {
    const match = window.location.pathname.match(/\/app\/(\d+)/);
    return match ? parseInt(match[1], 10) : 0;
  }

  function initLuaToolsButton() {
    const appId = getAppId();
    if (!appId) return;

    if (window.Millennium && window.Millennium.callServerMethod) {
      window.Millennium.callServerMethod("luatools", "HasLuaToolsForApp", { appid: appId })
        .then(function(res) {
          let data = {};
          try { data = typeof res === "string" ? JSON.parse(res) : res; } catch(e) {}
          const exists = data.exists || data.has || false;
          renderButton(appId, exists);
        })
        .catch(function() {
          renderButton(appId, false);
        });
    }
  }

  function renderButton(appId, exists) {
    const existing = document.getElementById("luatools-store-action-btn");
    if (existing) existing.remove();
    const target = document.querySelector(".game_area_purchase_game") ||
                   document.querySelector(".queue_actions_ctn") ||
                   document.querySelector("#game_area_purchase");
    if (!target) return;

    const btn = document.createElement("div");
    btn.id = "luatools-store-action-btn";
    btn.style.cssText = "margin: 10px 0; padding: 10px 16px; border-radius: 4px; font-size: 14px; font-weight: bold; cursor: pointer; display: inline-block; text-align: center; transition: all 0.2s;";

    if (exists) {
      btn.style.background = "#3d4450";
      btn.style.color = "#e5e5e5";
      btn.innerText = "✓ In LuaTools (Click to Remove)";
      btn.onclick = function() {
        btn.innerText = "Removing...";
        window.Millennium.callServerMethod("luatools", "DeleteLuaToolsForApp", { appid: appId })
          .then(function() {
            renderButton(appId, false);
          });
      };
    } else {
      btn.style.background = "#5c7e10";
      btn.style.color = "#d2efa9";
      btn.innerText = "+ Add via LuaTools";
      btn.onclick = function() {
        btn.innerText = "Queuing in LuaTools...";
        window.Millennium.callServerMethod("luatools", "StartAddViaLuaTools", { appid: appId })
          .then(function() {
            btn.innerText = "✓ Added! (Refresh Steam)";
            btn.style.background = "#2a475e";
          });
      };
    }

    target.parentNode.insertBefore(btn, target);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", initLuaToolsButton);
  } else {
    initLuaToolsButton();
  }
})();
"##;

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
    use std::io::Write;
    use tempfile::tempdir;
    use zip::write::SimpleFileOptions;

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

    #[test]
    fn test_install_default_luatools_js() {
        let dir = tempdir().unwrap();
        let service = PluginInstallerService::new(Some(dir.path()));
        let installed = service.install_default_luatools_js(None).unwrap();
        assert!(installed.is_file());
        assert_eq!(installed, dir.path().join("luatools/luatools.js"));
        let content = fs::read_to_string(&installed).unwrap();
        assert!(content.contains("LuaTools Store Plugin (LTSP)"));
        assert!(service.is_luatools_js_installed(None));
    }

    #[test]
    fn test_ltsp_payload_validation_and_dual_searched_paths() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins");
        let steam_root = dir.path().join("Steam");
        fs::create_dir_all(&steam_root).unwrap();

        let service = PluginInstallerService::new(Some(&plugin_dir));

        // Empty and HTML error payloads are rejected
        assert!(service.install_ltsp_js_payload("", Some(&steam_root)).is_err());
        assert!(service
            .install_ltsp_js_payload("<!DOCTYPE html><html><body>404</body></html>", Some(&steam_root))
            .is_err());

        // Valid JS payload lands in BOTH searched paths matching cef_bridge::find_luatools_js
        let payload = "// Remote LTSP build\n(function(){ window.__ltspLoaded = true; })();\n";
        let installed = service
            .install_ltsp_js_payload(payload, Some(&steam_root))
            .unwrap();

        let path_plugin = plugin_dir.join("luatools/luatools.js");
        let path_steamui = steam_root.join("steamui/LuaTools/luatools.js");
        assert!(path_plugin.is_file());
        assert!(path_steamui.is_file());
        assert_eq!(installed, path_steamui);
        assert_eq!(fs::read_to_string(&path_plugin).unwrap(), payload);
        assert_eq!(fs::read_to_string(&path_steamui).unwrap(), payload);
        assert_eq!(
            crate::services::cef_bridge::find_luatools_js(Some(&steam_root)),
            Some(path_steamui.clone())
        );

        // Verify ZIP extraction helper validates non-empty JS
        let mut zip_buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_buf));
            zw.start_file("public/luatools.js", SimpleFileOptions::default())
                .unwrap();
            zw.write_all(payload.as_bytes()).unwrap();
            zw.finish().unwrap();
        }
        let extracted = PluginInstallerService::extract_luatools_js_from_zip(&zip_buf).unwrap();
        assert_eq!(extracted, payload);
    }
}
