//! Interface generator for `steam_interfaces.txt` — scans raw ELF `.so` and PE `.dll` binaries safely.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Context;
use regex::Regex;

pub const DEFAULT_INTERFACE_PREFIXES: &[&str] = &[
    "SteamClient",
    "SteamGameServer",
    "SteamGameServerStats",
    "SteamUser",
    "SteamFriends",
    "SteamUtils",
    "SteamMatchMaking",
    "SteamMatchMakingServers",
    "SteamApps",
    "SteamInput",
    "SteamNetworking",
    "SteamNetworkingSockets",
    "SteamNetworkingMessages",
    "SteamNetworkingUtils",
    "SteamParties",
    "STEAMUSERSTATS_INTERFACE_VERSION",
    "STEAMAPPS_INTERFACE_VERSION",
    "STEAMREMOTESTORAGE_INTERFACE_VERSION",
    "STEAMSCREENSHOTS_INTERFACE_VERSION",
    "STEAMHTTP_INTERFACE_VERSION",
    "STEAMUNIFIEDMESSAGES_INTERFACE_VERSION",
    "STEAMUGC_INTERFACE_VERSION",
    "STEAMAPPLIST_INTERFACE_VERSION",
    "STEAMMUSIC_INTERFACE_VERSION",
    "STEAMMUSICREMOTE_INTERFACE_VERSION",
    "STEAMHTMLSURFACE_INTERFACE_VERSION_",
    "STEAMINVENTORY_INTERFACE_V",
    "SteamController",
    "SteamMasterServerUpdater",
    "STEAMVIDEO_INTERFACE_V",
];

pub struct InterfaceGenerator {
    compiled_patterns: Vec<Regex>,
}

impl Default for InterfaceGenerator {
    fn default() -> Self {
        let mut compiled_patterns: Vec<Regex> = DEFAULT_INTERFACE_PREFIXES
            .iter()
            .filter_map(|name| Regex::new(&format!(r"{}\d{{3}}", regex::escape(name))).ok())
            .collect();

        if let Ok(re) = Regex::new(r"STEAMCONTROLLER_INTERFACE_VERSION\d{3}") {
            compiled_patterns.push(re);
        }
        if let Ok(re) = Regex::new(r"STEAMCONTROLLER_INTERFACE_VERSION\b") {
            compiled_patterns.push(re);
        }

        Self { compiled_patterns }
    }
}

impl InterfaceGenerator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn generate_interfaces(&self, so_path: &Path, force: bool) -> anyhow::Result<Vec<String>> {
        let interfaces_path = so_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("steam_settings")
            .join("steam_interfaces.txt");
        if interfaces_path.exists() && !force {
            return Ok(Vec::new());
        }

        let raw_bytes = std::fs::read(so_path)
            .with_context(|| format!("Failed to read Steam API binary: {}", so_path.display()))?;
        let text_lossy = String::from_utf8_lossy(&raw_bytes);

        let mut interfaces = BTreeSet::new();
        for regex in &self.compiled_patterns {
            for mat in regex.find_iter(&text_lossy) {
                interfaces.insert(mat.as_str().to_string());
            }
        }

        Ok(interfaces.into_iter().collect())
    }

    pub fn write_interfaces_file(
        &self,
        interfaces: &[String],
        output_path: &Path,
    ) -> anyhow::Result<()> {
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = interfaces
            .iter()
            .map(|s| format!("{}\n", s))
            .collect::<String>();
        std::fs::write(output_path, content).context("Failed to write steam_interfaces.txt")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_interfaces_from_text_and_binary_elf() {
        let gen = InterfaceGenerator::new();
        let dir = tempfile::tempdir().unwrap();
        let so_path = dir.path().join("libsteam_api.so");

        let mut raw = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0xff, 0xfe, 0x00];
        raw.extend_from_slice(
            b"SteamClient020\0SteamUser023\0STEAMCONTROLLER_INTERFACE_VERSION008\0",
        );
        raw.extend_from_slice(&[0x80, 0x91, 0xa2, 0xb3]);
        std::fs::write(&so_path, &raw).unwrap();

        let result = gen.generate_interfaces(&so_path, true).unwrap();
        assert!(result.contains(&"SteamClient020".to_string()));
        assert!(result.contains(&"SteamUser023".to_string()));
        assert!(result.contains(&"STEAMCONTROLLER_INTERFACE_VERSION008".to_string()));
    }
}
