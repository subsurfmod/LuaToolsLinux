//! SteamDepotInfo Service — fetches & caches depot metadata from `api.steamcmd.net`
//! with deadlock-free concurrent request deduplication.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::config::default_luatools_cache_dir;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentDepot {
    pub id: u32,
    pub size: u64,
    #[serde(default)]
    pub dlc_app_id: Option<u32>,
    #[serde(default)]
    pub is_shared: bool,
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub public_manifest_id: Option<String>,
    #[serde(default)]
    pub from_app_id: Option<u32>,
}

impl ContentDepot {
    pub fn is_dlc(&self) -> bool {
        self.dlc_app_id.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppDepotInfo {
    pub app_id: u32,
    #[serde(default)]
    pub depots: Vec<ContentDepot>,
    #[serde(default)]
    pub dlc_ids: Vec<u32>,
    #[serde(default)]
    pub launch_exes: Vec<String>,
    #[serde(default)]
    pub public_build_id: Option<String>,
}

struct SyncState {
    memory_cache: HashMap<u32, (Option<AppDepotInfo>, Instant)>,
    in_flight: HashSet<u32>,
}

#[derive(Clone)]
pub struct SteamDepotInfo {
    cache_dir: PathBuf,
    state: Arc<(Mutex<SyncState>, Condvar)>,
}

impl Default for SteamDepotInfo {
    fn default() -> Self {
        Self::new(None)
    }
}

impl SteamDepotInfo {
    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        let dir = cache_dir.unwrap_or_else(|| default_luatools_cache_dir().join("depotinfo"));
        let _ = fs::create_dir_all(&dir);
        Self {
            cache_dir: dir,
            state: Arc::new((
                Mutex::new(SyncState {
                    memory_cache: HashMap::new(),
                    in_flight: HashSet::new(),
                }),
                Condvar::new(),
            )),
        }
    }

    pub fn get(&self, app_id: u32) -> Option<AppDepotInfo> {
        let (lock, cvar) = &*self.state;
        {
            let mut st = lock.lock().unwrap();
            loop {
                if let Some((cached, ts)) = st.memory_cache.get(&app_id) {
                    if cached.is_some() || ts.elapsed() < Duration::from_secs(60) {
                        return cached.clone();
                    }
                }
                if !st.in_flight.contains(&app_id) {
                    st.in_flight.insert(app_id);
                    break;
                }
                let (next_st, timeout) = cvar.wait_timeout(st, Duration::from_secs(20)).unwrap();
                st = next_st;
                if timeout.timed_out() {
                    return None;
                }
            }
        }

        if let Some(disk) = self.load_disk_cache(app_id) {
            let mut st = lock.lock().unwrap();
            st.memory_cache
                .insert(app_id, (Some(disk.clone()), Instant::now()));
            st.in_flight.remove(&app_id);
            cvar.notify_all();
            return Some(disk);
        }

        let fetched = Self::fetch_from_api(app_id);
        if let Some(ref info) = fetched {
            self.save_disk_cache(app_id, info);
        }

        let mut st = lock.lock().unwrap();
        st.memory_cache
            .insert(app_id, (fetched.clone(), Instant::now()));
        st.in_flight.remove(&app_id);
        cvar.notify_all();
        fetched
    }

    pub fn invalidate(&self, app_id: u32) {
        let (lock, _) = &*self.state;
        {
            let mut st = lock.lock().unwrap();
            st.memory_cache.remove(&app_id);
        }
        let _ = fs::remove_file(self.cache_dir.join(format!("{}.json", app_id)));
    }

    fn load_disk_cache(&self, app_id: u32) -> Option<AppDepotInfo> {
        let raw = fs::read_to_string(self.cache_dir.join(format!("{}.json", app_id))).ok()?;
        serde_json::from_str::<AppDepotInfo>(&raw).ok()
    }

    fn save_disk_cache(&self, app_id: u32, info: &AppDepotInfo) {
        if let Ok(js) = serde_json::to_string(info) {
            let _ = fs::write(self.cache_dir.join(format!("{}.json", app_id)), js);
        }
    }

    fn fetch_from_api(app_id: u32) -> Option<AppDepotInfo> {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(15))
            .build();
        let url = format!("https://api.steamcmd.net/v1/info/{}", app_id);
        let data: Value = agent.get(&url).call().ok()?.into_json().ok()?;
        let app = data.get("data")?.get(app_id.to_string())?;

        let mut depots = Vec::new();
        let mut public_build_id = None;

        if let Some(depots_obj) = app.get("depots").and_then(|v| v.as_object()) {
            if let Some(bid) = depots_obj
                .get("branches")
                .and_then(|b| b.get("public"))
                .and_then(|p| p.get("buildid"))
            {
                public_build_id = bid
                    .as_str()
                    .map(ToString::to_string)
                    .or_else(|| bid.as_u64().map(|n| n.to_string()));
            }

            for (dep_key, dep_val) in depots_obj {
                let Ok(depot_id) = dep_key.parse::<u32>() else {
                    continue;
                };
                let Some(dmap) = dep_val.as_object() else {
                    continue;
                };

                let is_shared = dmap.contains_key("depotfromapp");
                let from_app_id = dmap.get("depotfromapp").and_then(|v| {
                    v.as_str()
                        .and_then(|s| s.parse::<u32>().ok())
                        .or_else(|| v.as_u64().map(|n| n as u32))
                });
                let dlc_app_id = dmap.get("dlcappid").and_then(|v| {
                    v.as_str()
                        .and_then(|s| s.parse::<u32>().ok())
                        .or_else(|| v.as_u64().map(|n| n as u32))
                });

                let mut os = None;
                let mut language = None;
                if let Some(cfg) = dmap.get("config").and_then(|c| c.as_object()) {
                    os = cfg
                        .get("oslist")
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string);
                    language = cfg
                        .get("dlclanguage")
                        .or_else(|| cfg.get("language"))
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string);
                }

                let mut public_manifest_id = None;
                let mut size = 0u64;
                if let Some(pub_m) = dmap.get("manifests").and_then(|m| m.get("public")) {
                    if let Some(pm_obj) = pub_m.as_object() {
                        public_manifest_id = pm_obj.get("gid").and_then(|v| {
                            v.as_str()
                                .map(ToString::to_string)
                                .or_else(|| v.as_u64().map(|n| n.to_string()))
                        });
                        size = pm_obj
                            .get("size")
                            .and_then(|v| {
                                v.as_str()
                                    .and_then(|s| s.parse::<u64>().ok())
                                    .or_else(|| v.as_u64())
                            })
                            .unwrap_or(0);
                    } else if let Some(gid_s) = pub_m.as_str() {
                        public_manifest_id = Some(gid_s.to_string());
                    }
                }

                depots.push(ContentDepot {
                    id: depot_id,
                    size,
                    dlc_app_id,
                    is_shared,
                    os,
                    language,
                    public_manifest_id,
                    from_app_id,
                });
            }
        }

        let mut dlc_ids = Vec::new();
        if let Some(dlc_csv) = app
            .get("extended")
            .and_then(|e| e.get("listofdlc"))
            .and_then(|v| v.as_str())
        {
            for part in dlc_csv.split(',') {
                if let Ok(id) = part.trim().parse::<u32>() {
                    dlc_ids.push(id);
                }
            }
        }

        Some(AppDepotInfo {
            app_id,
            depots,
            dlc_ids,
            launch_exes: Vec::new(),
            public_build_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn concurrent_disk_cache_reads_never_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let sample = AppDepotInfo {
            app_id: 1245620,
            depots: vec![ContentDepot {
                id: 1245621,
                size: 4096,
                dlc_app_id: None,
                is_shared: false,
                os: Some("windows".into()),
                language: Some("english".into()),
                public_manifest_id: Some("998877".into()),
                from_app_id: None,
            }],
            dlc_ids: vec![],
            launch_exes: vec![],
            public_build_id: Some("150000".into()),
        };
        fs::write(
            dir.path().join("1245620.json"),
            serde_json::to_string(&sample).unwrap(),
        )
        .unwrap();

        let svc = SteamDepotInfo::new(Some(dir.path().to_path_buf()));
        let mut handles = Vec::new();
        for _ in 0..6 {
            let s = svc.clone();
            handles.push(thread::spawn(move || s.get(1245620)));
        }
        for h in handles {
            let res = h.join().unwrap();
            assert!(res.is_some());
        }
    }
}
