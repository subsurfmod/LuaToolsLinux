//! SteamAutoCrackService — launches `SteamAutoPatch` (`SteamAutoCrack`) for configured games or folders
//! with automatic Linux `dotnet` / `wine` runtime invocation via `ExternalToolsManager`.

use std::path::{Path, PathBuf};

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
        self.tools_manager.inspect_tool("steamautopatch").installed
    }

    pub fn tool_path(&self) -> Option<PathBuf> {
        self.tools_manager.inspect_tool("steamautopatch").exe_path
    }

    pub fn launch_for_app(&self, app_id: u32, install_path: &Path) -> Result<String, String> {
        self.tools_manager
            .launch_steamautopatch(app_id, install_path)
    }

    pub fn launch_for_folder(&self, folder: &Path) -> Result<String, String> {
        self.tools_manager.launch_steamautopatch(0, folder)
    }
}

impl Default for SteamAutoCrackService {
    fn default() -> Self {
        Self {
            tools_manager: ExternalToolsManager::new(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_uninstalled_and_returns_helpful_error_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = ExternalToolsManager::new(Some(tmp.path().join("tools")));
        let svc = SteamAutoCrackService::new(mgr);
        assert!(!svc.is_available());
        assert!(svc.tool_path().is_none());
        let err = svc.launch_for_folder(tmp.path()).unwrap_err();
        assert!(err.contains("SteamAutoPatch"));
    }
}
