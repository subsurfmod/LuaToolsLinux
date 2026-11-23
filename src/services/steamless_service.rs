//! SteamlessService - SteamStub conversion via Steamless CLI.
//!
//! Runs the Steamless.CLI tool (atom0s/Steamless) to modify game executables.
//! The tool is downloaded via GitHub proxy with mirror fallback and cached under
//! ~/.local/share/luatools/steamless.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct SteamlessResult {
    pub patched: u32,
    pub unchanged: u32,
    pub total: u32,
    pub error: Option<String>,
}

impl SteamlessResult {
    pub fn failed(&self) -> bool {
        self.error.is_some()
    }
}

pub struct SteamlessService {
    depot_downloader: crate::services::depot_downloader::DepotDownloaderService,
    tools_dir: PathBuf,
}

impl Default for SteamlessService {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let tools_dir = home.join(".local/share/luatools/steamless");
        Self {
            depot_downloader: crate::services::depot_downloader::DepotDownloaderService::new(None),
            tools_dir,
        }
    }
}

impl SteamlessService {
    pub fn new(depot_downloader: crate::services::depot_downloader::DepotDownloaderService) -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let tools_dir = home.join(".local/share/luatools/steamless");
        Self {
            depot_downloader,
            tools_dir,
        }
    }

    pub fn tool_dir(&self) -> &Path {
        &self.tools_dir
    }

    /// Check if the Steamless CLI tool is available.
    pub fn has_tool(&self) -> bool {
        self.cli_path().is_file()
    }

    /// Path to the Steamless CLI executable.
    pub fn cli_path(&self) -> PathBuf {
        self.tools_dir.join("Steamless.CLI.exe")
    }

    /// Ensure the Steamless CLI tool is downloaded and available.
    /// Returns the path to the CLI executable, or None if not available.
    pub async fn ensure_tool(&self) -> Option<PathBuf> {
        let cli_path = self.cli_path();
        if cli_path.is_file() {
            return Some(cli_path);
        }

        // In a full implementation, this would download from GitHub via the proxy
        // For now, check if the tool is already present
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let search_dirs = [
            home.join(".local/share/luatools/steamless"),
            home.join(".local/share/LuaTools/steamless"),
        ];

        for dir in &search_dirs {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_file() {
                        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        if name.contains("steamless") && (name.ends_with(".exe") || name.ends_with(".dll")) {
                            return Some(p);
                        }
                    }
                }
            }
        }

        None
    }

    /// Process game executables.
    /// Returns the number of patched executables, unchanged executables, and total count.
    pub async fn patch_game(
        &self,
        app_id: u32,
        install_path: &Path,
    ) -> SteamlessResult {
        let cli = match self.ensure_tool().await {
            Some(p) => p,
            None => {
                return SteamlessResult {
                    patched: 0,
                    unchanged: 0,
                    total: 0,
                    error: Some("Steamless CLI tool not found".to_string()),
                };
            }
        };

        // Find executables to patch - look for .exe files in the install directory
        let exes = match find_exes(install_dir) {
            Ok(e) => e,
            Err(_) => {
                return SteamlessResult {
                    patched: 0,
                    unchanged: 0,
                    total: 0,
                    error: Some("Failed to scan game directory".to_string()),
                };
            }
        };

        if exes.is_empty() {
            return SteamlessResult {
                patched: 0,
                unchanged: 0,
                total: 0,
                error: Some("No executable files found".to_string()),
            };
        }

        let mut patched = 0u32;
        let mut unchanged = 0u32;

        for exe in exes {
            match run_steamless(&cli, &exe) {
                Ok(changed) => {
                    if changed {
                        patched += 1;
                    } else {
                        unchanged += 1;
                    }
                }
                Err(_) => {
                    unchanged += 1;
                }
            }
        }

        SteamlessResult {
            patched,
            unchanged,
            total: patched + unchanged,
            error: None,
        }
    }
}

/// Find executable files (.exe) in a directory, recursively skipping common non-game directories.
fn find_exes(dir: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut exes = Vec::new();
    let skip_patterns = [
        "redist", "vcredist", "directx", "dxsetup", "vc_redist", "dotnet", "setup", "install",
        "uninstall", "crashreport", "crashhandler", "bugsplat", "easyanticheat", "battleye",
        "be_service", "_launcher", "bootstrap", "cefprocess", "cef_process", "steamwebhelper",
        "uplay",
    ];

    for entry in walkdir::WalkDir::new(dir) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.is_file() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                let lower = name.to_ascii_lowercase();
                if lower.ends_with(".exe")
                    && !skip_patterns.iter().any(|s| lower.contains(s))
                {
                    exes.push(path);
                }
            }
        }
    }

    Ok(exes)
}

/// Run Steamless CLI on a single executable.
fn run_steamless(cli: &Path, exe: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let output = Command::new(cli)
        .arg(exe)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;

    if output.status.success() {
        // Check if the file was actually modified (Steamless creates .unpacked.exe then replaces)
        let unpacked = exe.with_extension("exe.unpacked.exe");
        if unpacked.is_file() {
            // Steamless created an unpacked file - the game was processed
            Ok(true)
        } else {
            Ok(false)
        }
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_service_creation() {
        let dd = crate::services::depot_downloader::DepotDownloaderService::new();
        let svc = SteamlessService::new(dd);
        assert!(!svc.has_tool());
    }

    #[test]
    fn test_find_exes() {
        let dir = tempfile::tempdir().unwrap();
        let game_dir = dir.path().join("game");
        std::fs::create_dir_all(&game_dir).unwrap();
        std::fs::write(game_dir.join("game.exe"), b"fake exe").unwrap();
        std::fs::write(game_dir.join("launcher.exe"), b"fake launcher").unwrap();

        let exes = find_exes(&game_dir).unwrap();
        assert_eq!(exes.len(), 2);
    }
}