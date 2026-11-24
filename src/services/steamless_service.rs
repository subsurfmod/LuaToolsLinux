//! SteamlessService - SteamStub conversion via Steamless CLI.
//!
//! Runs the Steamless.CLI tool (atom0s/Steamless) to unpack SteamStub-protected
//! Windows executables via Wine / .NET invocation on Linux.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use crate::services::external_tools::ExternalToolsManager;

use crate::services::depot_downloader::DepotDownloaderService;

#[derive(Debug, Clone)]
pub struct SteamlessResult {
    pub patched: u32,
    pub unchanged: u32,
    pub total: u32,
    pub error: Option<String>,
}

impl SteamlessResult {
    pub fn is_success(&self) -> bool {
        self.error.is_none() && self.patched > 0
    }

    pub fn failed(&self) -> bool {
        self.error.is_some()
    }
}

pub struct SteamlessService {
    #[allow(dead_code)]
    depot_downloader: DepotDownloaderService,
    tools_dir: PathBuf,
}

impl Default for SteamlessService {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let tools_dir = home.join(".local/share/luatools/steamless");
        Self {
            depot_downloader: DepotDownloaderService::new(None),
            tools_dir,
        }
    }
}

impl SteamlessService {
    pub fn new(depot_downloader: DepotDownloaderService) -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let tools_dir = home.join(".local/share/luatools/steamless");
        Self {
            depot_downloader,
            tools_dir,
        }
    }

    pub fn with_tools_dir(depot_downloader: DepotDownloaderService, tools_dir: PathBuf) -> Self {
        Self {
            depot_downloader,
            tools_dir,
        }
    }

    pub fn tool_dir(&self) -> &Path {
        &self.tools_dir
    }

    /// Check if the Steamless CLI tool is available in `tools_dir`.
    pub fn has_tool(&self) -> bool {
        self.find_cli_in_dir(&self.tools_dir).is_some()
    }

    /// Canonical path to the Steamless CLI executable.
    pub fn cli_path(&self) -> PathBuf {
        self.tools_dir.join("Steamless.CLI.exe")
    }

    fn find_cli_in_dir(&self, dir: &Path) -> Option<PathBuf> {
        let direct = dir.join("Steamless.CLI.exe");
        if direct.is_file() {
            return Some(direct);
        }
        let entries = fs::read_dir(dir).ok()?;
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                let name = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if (name.contains("steamless") || name.contains("stubremover"))
                    && (name.ends_with(".exe") || name.ends_with(".dll"))
                {
                    return Some(p);
                }
            }
        }
        None
    }

    /// Ensure the Steamless CLI tool is available.
    /// Returns the path to the CLI executable, or None if not available.
    pub fn ensure_tool(&self) -> Option<PathBuf> {
        let mgr = ExternalToolsManager::new(Some(self.tools_dir.clone()));
        if let Some(exe) = mgr.inspect_tool("stub_remover").exe_path {
            return Some(exe);
        }
        if let Some(p) = self.find_cli_in_dir(&self.tools_dir) {
            return Some(p);
        }

        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let search_dirs = [
            home.join(".local/share/luatools/steamless"),
            home.join(".local/share/luatools/tools/stub_remover"),
            home.join(".local/share/LuaTools/steamless"),
        ];

        for dir in &search_dirs {
            if let Some(p) = self.find_cli_in_dir(dir) {
                return Some(p);
            }
        }

        None
    }

    /// Process game executables synchronously via ExternalToolsManager / Steamless CLI.
    /// Returns the number of patched executables, unchanged executables, and total count (LT-69).
    pub fn patch_game(&self, _app_id: u32, install_path: &Path) -> SteamlessResult {
        let cli = match self.ensure_tool() {
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

        let exes = match find_exes(install_path) {
            Ok(e) => e,
            Err(e) => {
                return SteamlessResult {
                    patched: 0,
                    unchanged: 0,
                    total: 0,
                    error: Some(format!("Failed to scan game directory: {}", e)),
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
                Ok(true) => patched += 1,
                Ok(false) | Err(_) => unchanged += 1,
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

const SKIP_PATTERNS: &[&str] = &[
    "redist",
    "vcredist",
    "directx",
    "dxsetup",
    "vc_redist",
    "dotnet",
    "setup",
    "install",
    "uninstall",
    "crashreport",
    "crashhandler",
    "bugsplat",
    "easyanticheat",
    "battleye",
    "be_service",
    "_launcher",
    "bootstrap",
    "cefprocess",
    "cef_process",
    "steamwebhelper",
    "uplay",
];

fn should_skip_name(lower: &str) -> bool {
    SKIP_PATTERNS.iter().any(|s| lower.contains(s))
}

fn unpacked_output_path(exe: &Path) -> PathBuf {
    let mut os = OsString::from(exe.as_os_str());
    os.push(".unpacked.exe");
    PathBuf::from(os)
}

fn backup_output_path(exe: &Path) -> PathBuf {
    let mut os = OsString::from(exe.as_os_str());
    os.push(".orig.bak");
    PathBuf::from(os)
}

/// Find executable files (.exe) in a directory, recursively skipping common non-game directories.
fn find_exes(dir: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    if !dir.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Game directory not found: {}", dir.display()),
        ));
    }

    let mut exes = Vec::new();
    let walker = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            if entry.file_type().is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    let lower = name.to_ascii_lowercase();
                    return !should_skip_name(&lower);
                }
            }
            true
        });

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if entry.file_type().is_file() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                let lower = name.to_ascii_lowercase();
                if lower.ends_with(".exe")
                    && !lower.ends_with(".unpacked.exe")
                    && !should_skip_name(&lower)
                {
                    exes.push(path.to_path_buf());
                }
            }
        }
    }

    exes.sort();
    Ok(exes)
}

/// Run Steamless CLI on a single executable using the appropriate Linux runtime wrapper
/// (`wine` for `.exe`, `dotnet` for `.dll`, or direct execution for native binaries).
fn run_steamless(cli: &Path, exe: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let prefix = DepotDownloaderService::build_invocation(cli).map_err(std::io::Error::other)?;
    let first = prefix
        .first()
        .ok_or_else(|| std::io::Error::other("Empty Steamless invocation"))?;

    let mut cmd = Command::new(first);
    for arg in &prefix[1..] {
        cmd.arg(arg);
    }
    let output = cmd
        .arg(exe)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;

    if output.status.success() {
        let unpacked_full = unpacked_output_path(exe);
        let unpacked_stem = exe.with_extension("unpacked.exe");
        let unpacked = if unpacked_full.is_file() {
            Some(unpacked_full)
        } else if unpacked_stem.is_file() {
            Some(unpacked_stem)
        } else {
            None
        };

        if let Some(unpacked) = unpacked {
            let backup = backup_output_path(exe);
            if !backup.exists() {
                fs::copy(exe, &backup)?;
            }
            fs::rename(&unpacked, exe)?;
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_creation_and_case_insensitive_tool_detection() {
        let tmp = tempfile::tempdir().unwrap();
        let dd = DepotDownloaderService::new(Some(tmp.path().join("dd")));
        let tools_dir = tmp.path().join("steamless");
        fs::create_dir_all(&tools_dir).unwrap();

        let svc = SteamlessService::with_tools_dir(dd, tools_dir.clone());
        assert!(!svc.has_tool());

        fs::write(tools_dir.join("Steamless.CLI.dll"), b"dll").unwrap();
        assert!(svc.has_tool());
    }

    #[test]
    fn test_find_exes_skips_non_game_dirs_and_unpacked_files() {
        let dir = tempfile::tempdir().unwrap();
        let game_dir = dir.path().join("game");
        fs::create_dir_all(game_dir.join("_CommonRedist/DirectX")).unwrap();
        fs::create_dir_all(game_dir.join("EasyAntiCheat")).unwrap();
        fs::write(game_dir.join("game.exe"), b"fake exe").unwrap();
        fs::write(game_dir.join("game.exe.unpacked.exe"), b"old unpacked").unwrap();
        fs::write(game_dir.join("launcher.exe"), b"fake launcher").unwrap();
        fs::write(
            game_dir.join("_CommonRedist/DirectX/helper.exe"),
            b"redist helper",
        )
        .unwrap();
        fs::write(
            game_dir.join("EasyAntiCheat/start_protected_game.exe"),
            b"eac",
        )
        .unwrap();

        let exes = find_exes(&game_dir).unwrap();
        assert_eq!(exes.len(), 2);
        assert!(exes.iter().any(|p| p.ends_with("game.exe")));
        assert!(exes.iter().any(|p| p.ends_with("launcher.exe")));

        assert!(find_exes(&dir.path().join("missing")).is_err());
    }

    #[test]
    fn test_unpacked_output_path_preserves_original_case_and_extension() {
        assert_eq!(
            unpacked_output_path(Path::new("/games/Game.EXE")),
            PathBuf::from("/games/Game.EXE.unpacked.exe")
        );
        assert_eq!(
            unpacked_output_path(Path::new("/games/MyGame.v2.exe")),
            PathBuf::from("/games/MyGame.v2.exe.unpacked.exe")
        );
    }

    #[test]
    fn test_patch_game_synchronous_execution_returns_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let dd = DepotDownloaderService::new(Some(tmp.path().join("dd")));
        let tools_dir = tmp.path().join("steamless");
        fs::create_dir_all(&tools_dir).unwrap();
        let svc = SteamlessService::with_tools_dir(dd, tools_dir);

        let game_dir = tmp.path().join("game");
        fs::create_dir_all(&game_dir).unwrap();

        // When tool is not found, patch_game returns a clear error synchronously
        let res = svc.patch_game(1245620, &game_dir);
        assert!(!res.is_success());
        assert_eq!(res.patched, 0);
        assert!(res.error.is_some());
    }
}
