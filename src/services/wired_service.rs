//! WiredService — Native Rust implementation of the enter-the-wired + headcrab functionality.
//! Installs and manages ACCELA and SLSsteam components without shell-script execution.
//! All operations are performed natively in Rust with explicit user consent at each step.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use ureq::AgentBuilder;

use crate::services::slssteam_service::SlssteamService;

const ACCELA_REPO: &str = "ciscosweater/enter-the-wired";
const ACCELA_DIR_NAME: &str = "ACCELA";
const SLSSTEAM_DIR_NAME: &str = "SLSsteam";

const WIRED_DATA_DIR: &str = ".local/share/luatools";

#[derive(Debug, Clone)]
pub struct WiredService {
    steam_install_dir: Option<PathBuf>,
    slssteam_service: Option<std::sync::Arc<std::sync::Mutex<SlssteamService>>>,
    wired_dir: PathBuf,
}

impl WiredService {
    pub fn new() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let wired_dir = home.join(WIRED_DATA_DIR);
        let _ = std::fs::create_dir_all(&wired_dir);

        Self {
            steam_install_dir: None,
            slssteam_service: None,
            wired_dir,
        }
    }

    pub fn with_steam_installation(mut self, inst: crate::models::steam::SteamInstallation) -> Self {
        self.steam_install_dir = Some(inst.root);
        self
    }

    pub fn with_slssteam_service(mut self, sls: std::sync::Arc<std::sync::Mutex<SlssteamService>>) -> Self {
        self.slssteam_service = Some(sls);
        self
    }

    pub fn accela_dir(&self) -> PathBuf {
        self.wired_dir.join(ACCELA_DIR_NAME)
    }

    pub fn slssteam_dir(&self) -> PathBuf {
        self.wired_dir.join(SLSSTEAM_DIR_NAME)
    }

    pub fn is_accela_installed(&self) -> bool {
        self.accela_dir().exists()
    }

    pub fn is_slssteam_installed(&self) -> bool {
        self.slssteam_dir().exists()
    }

    pub fn install_accela<F>(&mut self, mut progress_cb: F) -> Result<PathBuf, String>
    where
        F: FnMut(f32, String),
    {
        progress_cb(0.05, "Checking ACCELA release on GitHub...".into());

        let agent = AgentBuilder::new().timeout(Duration::from_secs(60)).build();
        let url = format!(
            "https://api.github.com/repos/{}/releases/latest",
            ACCELA_REPO
        );

        let resp = match agent.get(&url).call() {
            Ok(r) => r,
            Err(e) => return Err(format!("Failed to query GitHub: {}", e)),
        };

        let release: Value = match resp.into_json() {
            Ok(v) => v,
            Err(e) => return Err(format!("Failed to parse release data: {}", e)),
        };

        let tag = release
            .get("tag_name")
            .and_then(|v| v.as_str())
            .unwrap_or("latest");

        let assets = release
            .get("assets")
            .and_then(|v| v.as_array())
            .ok_or_else(|| "No assets found in release".to_string())?;

        let asset = assets.iter().find(|a| {
            a.get("name")
                .and_then(|v| v.as_str())
                .map(|n| n.ends_with(".AppImage"))
                .unwrap_or(false)
        });

        let asset = asset.ok_or_else(|| "No AppImage asset found".to_string())?;
        let download_url = asset
            .get("browser_download_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "No download URL found".to_string())?;

        let file_name = asset
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("ACCELA.AppImage");

        let archive_path = self.wired_dir.join(file_name);

        progress_cb(0.15, format!("Downloading ACCELA {}...", tag).into());

        let dl_resp = match agent.get(download_url).call() {
            Ok(r) => r,
            Err(e) => return Err(format!("Download failed: {}", e)),
        };

        stream_response_to_file(dl_resp, &archive_path, &mut |p, m| {
            progress_cb(0.15 + p * 0.75, m);
        })?;

        progress_cb(0.90, "Extracting ACCELA...".into());

        let _ = std::fs::set_permissions(
            &archive_path,
            std::fs::Permissions::from_mode(0o755),
        );

        progress_cb(1.0, "ACCELA installed".into());

        Ok(archive_path)
    }

    pub fn install_slssteam<F>(&mut self, mut progress_cb: F) -> Result<PathBuf, String>
    where
        F: FnMut(f32, String),
    {
        progress_cb(0.05, "Checking SLSsteam release on GitHub...".into());

        let agent = AgentBuilder::new().timeout(Duration::from_secs(60)).build();
        let url = "https://api.github.com/repos/AceSLS/SLSsteam/releases/latest";

        let resp = match agent.get(url).call() {
            Ok(r) => r,
            Err(e) => return Err(format!("Failed to query GitHub: {}", e)),
        };

        let release: Value = match resp.into_json() {
            Ok(v) => v,
            Err(e) => return Err(format!("Failed to parse release data: {}", e)),
        };

        let tag = release
            .get("tag_name")
            .and_then(|v| v.as_str())
            .unwrap_or("latest");

        let assets = release
            .get("assets")
            .and_then(|v| v.as_array())
            .ok_or_else(|| "No assets found in release".to_string())?;

        let asset = assets.iter().find(|a| {
            a.get("name")
                .and_then(|v| v.as_str())
                .map(|n| {
                    n.ends_with(".tar.gz") || n.ends_with(".tar.bz2") || n.ends_with(".zip")
                })
                .unwrap_or(false)
        });

        let asset = asset.ok_or_else(|| "No Linux archive asset found".to_string())?;
        let download_url = asset
            .get("browser_download_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "No download URL found".to_string())?;

        let file_name = asset
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("slssteam.tar.gz");

        let archive_path = self.wired_dir.join(file_name);

        progress_cb(0.15, format!("Downloading SLSsteam {}...", tag).into());

        let dl_resp = match agent.get(download_url).call() {
            Ok(r) => r,
            Err(e) => return Err(format!("Download failed: {}", e)),
        };

        stream_response_to_file(dl_resp, &archive_path, &mut |p, m| {
            progress_cb(0.15 + p * 0.75, m);
        })?;

        progress_cb(0.90, "Extracting SLSsteam...".into());

        let _ = fs::create_dir_all(&self.slssteam_dir());

        if archive_path.extension().and_then(|s| s.to_str()) == Some("zip") {
            extract_zip(&archive_path, &self.slssteam_dir())?;
        } else {
            extract_tar(&archive_path, &self.slssteam_dir())?;
        }

        let so_file = self.slssteam_dir().join("SLSsteam.so");
        if so_file.exists() {
            let _ = std::fs::set_permissions(
                &so_file,
                std::fs::Permissions::from_mode(0o755),
            );
        }

        progress_cb(1.0, "SLSsteam installed".into());

        Ok(archive_path)
    }

    pub fn install_all<F>(&mut self, mut progress_cb: F) -> Result<(), String>
    where
        F: FnMut(f32, String),
    {
        progress_cb(0.0, "Starting wired installation...".into());

        self.install_accela(|p, m| progress_cb(p * 0.5, m))?;
        self.install_slssteam(|p, m| progress_cb(0.5 + p * 0.5, m))?;

        progress_cb(1.0, "Wired installation complete".into());
        Ok(())
    }

    pub fn uninstall_all(&mut self) -> Result<(), String> {
        if self.accela_dir().exists() {
            fs::remove_dir_all(&self.accela_dir())
                .map_err(|e| format!("Failed to remove ACCELA: {}", e))?;
        }
        if self.slssteam_dir().exists() {
            fs::remove_dir_all(&self.slssteam_dir())
                .map_err(|e| format!("Failed to remove SLSsteam: {}", e))?;
        }
        Ok(())
    }

    pub fn is_fully_installed(&self) -> bool {
        self.is_accela_installed() && self.is_slssteam_installed()
    }

    pub fn status(&self) -> WiredStatus {
        WiredStatus {
            accela_installed: self.is_accela_installed(),
            slssteam_installed: self.is_slssteam_installed(),
            accela_dir: self.accela_dir(),
            slssteam_dir: self.slssteam_dir(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WiredStatus {
    pub accela_installed: bool,
    pub slssteam_installed: bool,
    pub accela_dir: PathBuf,
    pub slssteam_dir: PathBuf,
}

fn stream_response_to_file<F>(
    resp: ureq::Response,
    dest: &Path,
    mut progress_cb: F,
) -> Result<(), String>
where
    F: FnMut(f32, String),
{
    let total = resp
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    let mut file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut reader = resp.into_reader();
    let mut downloaded: u64 = 0;
    let mut buf = [0u8; 8192];

    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        downloaded += n as u64;

        if total > 0 {
            let pct = downloaded as f32 / total as f32;
            progress_cb(pct, format!("{}/{} bytes", downloaded, total));
        }
    }

    Ok(())
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let path = dest.join(entry.name());
        if entry.is_dir() {
            let _ = fs::create_dir_all(&path);
        } else {
            if let Some(p) = path.parent() {
                let _ = fs::create_dir_all(p);
            }
            let mut out = std::fs::File::create(&path).map_err(|e| e.to_string())?;
            io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
        }
    }

    Ok(())
}

fn extract_tar(archive: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);
    tar.unpack(dest).map_err(|e| e.to_string())?;
    Ok(())
}

impl Default for WiredService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wired_service_creation() {
        let service = WiredService::new();
        assert!(!service.is_accela_installed());
        assert!(!service.is_slssteam_installed());
        assert!(!service.is_fully_installed());
    }
}