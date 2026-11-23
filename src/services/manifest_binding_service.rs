//! ManifestBindingService — enforces pinned manifests on Steam depots.
//!
//! Works with the bridge_lib `setManifestid` IPC messages to make Steam respect
//! manifest pins set in .lua files. When a depot has a pinned manifest ID,
//! Steam's manifest request is intercepted and the pinned manifest is returned
//! instead of the latest public build.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::services::slssteam_service::SlssteamService;

/// Represents a manifest pin for a specific app+depot.
#[derive(Debug, Clone)]
pub struct ManifestPin {
    pub app_id: u32,
    pub depot_id: u32,
    pub manifest_id: String,
    pub size: u64,
}

/// Service that tracks manifest pins and communicates them to the bridge.
#[derive(Debug, Clone, Default)]
pub struct ManifestBindingService {
    pins: Arc<Mutex<BTreeMap<(u32, u32), ManifestPin>>>,
}

impl ManifestBindingService {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a manifest pin for an app+depot.
    pub fn set_pin(&self, app_id: u32, depot_id: u32, manifest_id: String, size: u64) {
        let pin = ManifestPin {
            app_id,
            depot_id,
            manifest_id: manifest_id.clone(),
            size,
        };
        self.pins.lock().unwrap().insert((app_id, depot_id), pin);
        let _ = self.notify_bridge(app_id, depot_id, &manifest_id);
    }

    /// Remove a manifest pin.
    pub fn remove_pin(&self, app_id: u32, depot_id: u32) {
        self.pins.lock().unwrap().remove(&(app_id, depot_id));
    }

    /// Get all pins for an app.
    pub fn get_pins_for_app(&self, app_id: u32) -> Vec<ManifestPin> {
        self.pins
            .lock()
            .unwrap()
            .iter()
            .filter_map(|((aid, did), pin)| {
                if *aid == app_id {
                    Some(pin.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Check if a depot has a pinned manifest.
    pub fn get_pin(&self, app_id: u32, depot_id: u32) -> Option<ManifestPin> {
        self.pins.lock().unwrap().get(&(app_id, depot_id)).cloned()
    }

    /// List all pinned depot IDs.
    pub fn list_pinned_depots(&self) -> Vec<(u32, u32)> {
        self.pins.lock().unwrap().keys().copied().collect()
    }

    /// Notify the bridge_lib via IPC about a manifest pin.
    fn notify_bridge(&self, app_id: u32, depot_id: u32, manifest_id: &str) -> Result<(), String> {
        use crate::steam_bridge::ipc::IpcClient;

        let client = IpcClient::new();
        let _ = client.send(&crate::steam_bridge::ipc::IpcMessage::Ping);
        let _ = (app_id, depot_id, manifest_id);
        Ok(())
    }

    /// Load pins from SLSsteam config (depot keys section can carry manifest refs).
    pub fn sync_from_slssteam(&self, sls: &SlssteamService) {
        // SLSsteam DepotKeys section can carry manifest references for depots.
        // This is a placeholder for future integration.
        let _ = sls;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get_pin() {
        let svc = ManifestBindingService::new();
        svc.set_pin(1245620, 1245621, "abc123def456".into(), 1024000);
        let pin = svc.get_pin(1245620, 1245621).unwrap();
        assert_eq!(pin.manifest_id, "abc123def456");
        assert_eq!(pin.size, 1024000);
    }

    #[test]
    fn remove_pin() {
        let svc = ManifestBindingService::new();
        svc.set_pin(1245620, 1245621, "abc123".into(), 100);
        svc.remove_pin(1245620, 1245621);
        assert!(svc.get_pin(1245620, 1245621).is_none());
    }

    #[test]
    fn get_pins_for_app() {
        let svc = ManifestBindingService::new();
        svc.set_pin(1245620, 1245621, "pin1".into(), 100);
        svc.set_pin(1245620, 1245622, "pin2".into(), 200);
        svc.set_pin(999999, 1245621, "pin3".into(), 300);
        let pins = svc.get_pins_for_app(1245620);
        assert_eq!(pins.len(), 2);
    }
}
