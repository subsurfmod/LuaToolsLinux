//! ManifestBindingService — enforces and tracks pinned manifests on Steam depots.
//!
//! Synchronizes pinned depot manifest GIDs with `bridge_lib` over Unix socket IPC
//! (`SetManifestPin` / `RemoveManifestPin` → `ManifestBound`), persists pins to disk,
//! and imports active `setManifestid` declarations from `.lua` files.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::models::config::default_luatools_data_dir;
use crate::parsers::lua_parser::LuaContents;
use crate::services::slssteam_service::SlssteamService;
use crate::steam_bridge::ipc::{IpcClient, IpcMessage};

/// Represents a manifest pin for a specific app+depot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestPin {
    pub app_id: u32,
    pub depot_id: u32,
    pub manifest_id: String,
    pub size: u64,
    #[serde(default)]
    pub active_in_bridge: bool,
}

/// Service that tracks manifest pins, persists them locally, and binds them in `bridge_lib`.
#[derive(Debug, Clone)]
pub struct ManifestBindingService {
    pins: Arc<Mutex<BTreeMap<(u32, u32), ManifestPin>>>,
    storage_path: Option<PathBuf>,
}

impl Default for ManifestBindingService {
    fn default() -> Self {
        Self::with_storage(Some(default_luatools_data_dir().join("manifest_pins.json")))
    }
}

impl ManifestBindingService {
    pub fn new() -> Self {
        Self::with_storage(None)
    }

    pub fn with_storage(storage_path: Option<PathBuf>) -> Self {
        let mut map = BTreeMap::new();
        if let Some(ref path) = storage_path {
            if let Ok(raw) = fs::read_to_string(path) {
                if let Ok(list) = serde_json::from_str::<Vec<ManifestPin>>(&raw) {
                    for mut p in list {
                        p.active_in_bridge = false;
                        map.insert((p.app_id, p.depot_id), p);
                    }
                }
            }
        }
        Self {
            pins: Arc::new(Mutex::new(map)),
            storage_path,
        }
    }

    fn persist(&self, map: &BTreeMap<(u32, u32), ManifestPin>) {
        let Some(ref path) = self.storage_path else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let list: Vec<&ManifestPin> = map.values().collect();
        if let Ok(js) = serde_json::to_string_pretty(&list) {
            let _ = fs::write(path, js);
        }
    }

    /// Registers a manifest pin for `app_id` + `depot_id` and pushes it over IPC to `bridge_lib`.
    pub fn set_pin(&self, app_id: u32, depot_id: u32, manifest_id: String, size: u64) -> bool {
        let bound = self
            .notify_set_pin(app_id, depot_id, &manifest_id, size)
            .unwrap_or(false);
        let pin = ManifestPin {
            app_id,
            depot_id,
            manifest_id: manifest_id.clone(),
            size,
            active_in_bridge: bound,
        };
        let mut guard = self.pins.lock().unwrap();
        guard.insert((app_id, depot_id), pin);
        self.persist(&guard);

        // Connect manifest pin into SLSsteam config.yaml so overrides take effect in Steam
        let mut sls = SlssteamService::default();
        sls.set_manifest_override(depot_id, manifest_id);
        let _ = sls.save();

        bound
    }

    /// Removes a manifest pin and notifies `bridge_lib`.
    pub fn remove_pin(&self, app_id: u32, depot_id: u32) {
        let mut guard = self.pins.lock().unwrap();
        guard.remove(&(app_id, depot_id));
        self.persist(&guard);
        drop(guard);
        let _ = IpcClient::new().send(&IpcMessage::RemoveManifestPin { app_id, depot_id });

        // Remove manifest override from SLSsteam config.yaml
        let mut sls = SlssteamService::default();
        sls.remove_manifest_override(depot_id);
        let _ = sls.save();
    }

    /// Imports active `setManifestid(...)` declarations from a parsed `.lua` file if not already tracked.
    pub fn sync_from_lua(&self, app_id: u32, lua: &LuaContents) {
        let mut guard = self.pins.lock().unwrap();
        let mut changed = false;
        for entry in &lua.entries {
            if let Some(ref gid) = entry.manifest_id {
                let key = (app_id, entry.id);
                let sz = entry.size_on_disk.unwrap_or(0);
                let needs_update = guard
                    .get(&key)
                    .map(|p| p.manifest_id != *gid)
                    .unwrap_or(true);
                if needs_update {
                    let bound = self
                        .notify_set_pin(app_id, entry.id, gid, sz)
                        .unwrap_or(false);
                    guard.insert(
                        key,
                        ManifestPin {
                            app_id,
                            depot_id: entry.id,
                            manifest_id: gid.clone(),
                            size: sz,
                            active_in_bridge: bound,
                        },
                    );
                    changed = true;
                }
            }
        }
        if changed {
            self.persist(&guard);
        }
    }

    /// Re-sends all stored pins to `bridge_lib` over IPC and returns how many confirmed active binding.
    pub fn resync_all_to_bridge(&self) -> usize {
        let mut guard = self.pins.lock().unwrap();
        let mut active_n = 0usize;
        for pin in guard.values_mut() {
            let bound = self
                .notify_set_pin(pin.app_id, pin.depot_id, &pin.manifest_id, pin.size)
                .unwrap_or(false);
            pin.active_in_bridge = bound;
            if bound {
                active_n += 1;
            }
        }
        self.persist(&guard);
        active_n
    }

    pub fn get_pins_for_app(&self, app_id: u32) -> Vec<ManifestPin> {
        self.pins
            .lock()
            .unwrap()
            .iter()
            .filter(|((aid, _), _)| *aid == app_id)
            .map(|(_, pin)| pin.clone())
            .collect()
    }

    pub fn get_pin(&self, app_id: u32, depot_id: u32) -> Option<ManifestPin> {
        self.pins.lock().unwrap().get(&(app_id, depot_id)).cloned()
    }

    pub fn list_pinned_depots(&self) -> Vec<(u32, u32)> {
        self.pins.lock().unwrap().keys().copied().collect()
    }

    fn notify_set_pin(
        &self,
        app_id: u32,
        depot_id: u32,
        manifest_id: &str,
        size: u64,
    ) -> Result<bool, String> {
        let client = IpcClient::new();
        match client.send(&IpcMessage::SetManifestPin {
            app_id,
            depot_id,
            manifest_id: manifest_id.to_string(),
            size,
        })? {
            Some(IpcMessage::ManifestBound { active, .. }) => Ok(active),
            Some(IpcMessage::Pong { active, .. }) => Ok(active),
            _ => Ok(false),
        }
    }

    pub fn sync_from_slssteam(&self, sls: &SlssteamService) {
        let mut guard = self.pins.lock().unwrap();
        let mut changed = false;
        for (&depot_id, gid) in &sls.config.manifest_overrides {
            let existing_key = guard.keys().find(|(_, d)| *d == depot_id).copied();
            if let Some(key) = existing_key {
                if let Some(p) = guard.get_mut(&key) {
                    if p.manifest_id != *gid {
                        p.manifest_id = gid.clone();
                        changed = true;
                    }
                }
            } else {
                guard.insert(
                    (0, depot_id),
                    ManifestPin {
                        app_id: 0,
                        depot_id,
                        manifest_id: gid.clone(),
                        size: 0,
                        active_in_bridge: false,
                    },
                );
                changed = true;
            }
        }
        if changed {
            self.persist(&guard);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_persist_and_remove_pins() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("pins.json");
        let svc = ManifestBindingService::with_storage(Some(path.clone()));
        svc.set_pin(1245620, 1245621, "abc123def456".into(), 1024000);
        svc.set_pin(1245620, 1245622, "pin2".into(), 200);
        svc.set_pin(999999, 1245621, "pin3".into(), 300);

        let reloaded = ManifestBindingService::with_storage(Some(path));
        let pin = reloaded.get_pin(1245620, 1245621).unwrap();
        assert_eq!(pin.manifest_id, "abc123def456");
        assert_eq!(pin.size, 1024000);
        assert_eq!(reloaded.get_pins_for_app(1245620).len(), 2);

        reloaded.remove_pin(1245620, 1245621);
        assert!(reloaded.get_pin(1245620, 1245621).is_none());
    }
}
