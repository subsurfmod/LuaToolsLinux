//! Interface generator for steam_interfaces.txt

use std::collections::HashSet;
use std::path::Path;
use anyhow::Context;
use regex::Regex;

pub struct InterfaceGenerator {
    interface_names: Vec<String>,
}

impl Default for InterfaceGenerator {
    fn default() -> Self {
        Self {
            interface_names: vec![
                "SteamClient".to_string(),
                "SteamGameServer".to_string(),
                "SteamGameServerStats".to_string(),
                "SteamUser".to_string(),
                "SteamFriends".to_string(),
                "SteamUtils".to_string(),
                "SteamMatchMaking".to_string(),
                "SteamMatchMakingServers".to_string(),
                "STEAMUSERSTATS_INTERFACE_VERSION".to_string(),
                "STEAMAPPS_INTERFACE_VERSION".to_string(),
                "SteamNetworking".to_string(),
                "STEAMREMOTESTORAGE_INTERFACE_VERSION".to_string(),
                "STEAMSCREENSHOTS_INTERFACE_VERSION".to_string(),
                "STEAMHTTP_INTERFACE_VERSION".to_string(),
                "STEAMUNIFIEDMESSAGES_INTERFACE_VERSION".to_string(),
                "STEAMUGC_INTERFACE_VERSION".to_string(),
                "STEAMAPPLIST_INTERFACE_VERSION".to_string(),
                "STEAMMUSIC_INTERFACE_VERSION".to_string(),
                "STEAMMUSICREMOTE_INTERFACE_VERSION".to_string(),
                "STEAMHTMLSURFACE_INTERFACE_VERSION_".to_string(),
                "STEAMINVENTORY_INTERFACE_V".to_string(),
                "STEAMAPPLIST_INTERFACE_VERSION".to_string(),
                "STEAMMUSIC_INTERFACE_VERSION".to_string(),
                "STEAMMUSICREMOTE_INTERFACE_VERSION".to_string(),
                "STEAMHTMLSURFACE_INTERFACE_VERSION_".to_string(),
                "STEAMINVENTORY_INTERFACE_V".to_string(),
                "SteamController".to_string(),
                "SteamMasterServerUpdater".to_string(),
                "STEAMVIDEO_INTERFACE_V".to_string(),
            ],
        }
    }
}

impl InterfaceGenerator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn generate_interfaces(&self, so_path: &Path, force: bool) -> anyhow::Result<Vec<String>> {
        let interfaces_path = Path::new("steam_settings/steam_interfaces.txt");
        if interfaces_path.exists() && !force {
            return Ok(Vec::new());
        }

        let mut interfaces = HashSet::new();
        let so_content = std::fs::read_to_string(so_path)
            .context("Failed to read SO file")?;

        for name in &self.interface_names {
            let pattern = format!(r"{}\d{{3}}", regex::escape(name));
            let regex = Regex::new(&pattern)?;
            for mat in regex.find_iter(&so_content) {
                interfaces.insert(mat.as_str().to_string());
            }
            
            // Special case for SteamController
            if name == "SteamController" {
                let regex = Regex::new(r"STEAMCONTROLLER_INTERFACE_VERSION\d{3}")?;
                for mat in regex.find_iter(&so_content) {
                    interfaces.insert(mat.as_str().to_string());
                }
                let regex = Regex::new("STEAMCONTROLLER_INTERFACE_VERSION")?;
                for mat in regex.find_iter(&so_content) {
                    interfaces.insert(mat.as_str().to_string());
                }
            }
        }

        Ok(interfaces.into_iter().collect())
    }

    pub fn write_interfaces_file(&self, interfaces: &[String], output_path: &Path) -> anyhow::Result<()> {
        let content = interfaces.iter().map(|s| format!("{}\n", s)).collect::<String>();
        std::fs::write(output_path, content).context("Failed to write steam_interfaces.txt")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_generate_interfaces_empty() {
        let gen = InterfaceGenerator::new();
        let dir = tempfile::tempdir().unwrap();
        let so_path = dir.path().join("test.so");
        std::fs::write(&so_path, "SteamClient001\nSteamUser002").unwrap();

        let result = gen.generate_interfaces(&so_path, true).unwrap();
        assert!(result.contains(&"SteamClient001".to_string()));
        assert!(result.contains(&"SteamUser002".to_string()));
    }
}