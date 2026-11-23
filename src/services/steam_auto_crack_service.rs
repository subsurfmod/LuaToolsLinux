//! SteamAutoCrackService - Launch Goldberg Steam emulator (SteamAutoPatch)
//! Ported from LuaToolsGui.Services.SteamAutoCrackService

use std::path::PathBuf;
use std::process::Command;

use crate::services::external_tools::ExternalToolsManager;

#[derive(Debug, Clone)]
pub struct SteamAutoCrackService {
    pub tools_manager: ExternalToolsManager,
}

impl SteamAutoCrackService {
    pub fn new(tools_manager: ExternalToolsManager) -> Self {
        Self { tools_manager }
    }

    pub fn is_available(&self) -> bool {
        self.tools_manager.get_tool_path("steamautocrack").is_some()
    }

    pub fn launch_for_app(&self, app_id: u32, install_path: &PathBuf) -> Result<String, String> {
        self.tools_manager.launch_steamautocrack(app_id, install_path)
    }

    pub fn launch_for_folder(&self, folder: &PathBuf) -> Result<String, String> {
        let exe = self.tools_manager.get_tool_path("steamautocrack")
            .ok_or_else(|| "SteamAutoPatch is not installed. Click INSTALL / UPDATE first.")?;
        
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg(folder);
        
        cmd.spawn()
            .map_err(|e| format!("Failed to launch SteamAutoPatch: {}", e))?;
            
        Ok(format!("SteamAutoPatch launched for folder: {}", folder.display()))
    }
}

impl Default for SteamAutoCrackService {
    fn default() -> Self {
        Self {
            tools_manager: ExternalToolsManager::new(None),
        }
    }
}
