//! Metadata & Cover Caches: `SteamAppInfoCache`, `SteamAppListCache`, `CoverCache`, `HardwareAppIdService`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::models::config::default_luatools_cache_dir;

const STEAM_PLACEHOLDER_SHA256: &str =
    "732ec27ff347292488e3341f00d0750a31f42c44f55da4e04bc8f61d7eb1b6f3";
const HARDWARE_APPID_URL: &str =
    "https://raw.githubusercontent.com/jsnli/steamappidlist/master/data/hardware_appid.json";
const TTL_14_DAYS: u64 = 14 * 24 * 3600;

#[derive(Debug, Clone, Default)]
pub struct GameDetails {
    pub app_id: u32,
    pub name: String,
    pub app_type: String,
    pub base_app_id: Option<u32>,
    pub genres: Vec<String>,
    pub header_image: String,
    pub release_date: String,
    pub is_free: bool,
}

impl GameDetails {
    pub fn is_dlc(&self) -> bool {
        self.app_type.eq_ignore_ascii_case("dlc")
    }
}

#[derive(Debug, Clone)]
pub struct SteamAppInfoCache {
    cache_dir: PathBuf,
    mem: Arc<Mutex<HashMap<u32, GameDetails>>>,
    last_req: Arc<Mutex<Option<Instant>>>,
}

impl Default for SteamAppInfoCache {
    fn default() -> Self {
        Self::new(None)
    }
}

impl SteamAppInfoCache {
    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        let dir = cache_dir.unwrap_or_else(|| default_luatools_cache_dir().join("details"));
        let _ = fs::create_dir_all(&dir);
        Self {
            cache_dir: dir,
            mem: Arc::new(Mutex::new(HashMap::new())),
            last_req: Arc::new(Mutex::new(None)),
        }
    }

    pub fn get(&self, app_id: u32) -> Option<GameDetails> {
        if let Some(hit) = self.mem.lock().unwrap().get(&app_id).cloned() {
            return Some(hit);
        }

        let cache_file = self.cache_dir.join(format!("{}.json", app_id));
        if let Ok(raw) = fs::read_to_string(&cache_file) {
            if let Ok(val) = serde_json::from_str::<Value>(&raw) {
                if let Some(d) = Self::parse_details(app_id, &val) {
                    self.mem.lock().unwrap().insert(app_id, d.clone());
                    return Some(d);
                }
            }
        }

        // Gentle 350ms rate limit against Steam Store API
        {
            let mut last = self.last_req.lock().unwrap();
            if let Some(prev) = *last {
                let elapsed = prev.elapsed();
                if elapsed < Duration::from_millis(350) {
                    thread::sleep(Duration::from_millis(350) - elapsed);
                }
            }
            *last = Some(Instant::now());
        }

        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(15))
            .build();
        let url = format!(
            "https://store.steampowered.com/api/appdetails?appids={}&l=english",
            app_id
        );
        let payload: Value = agent.get(&url).call().ok()?.into_json().ok()?;
        let entry = payload.get(app_id.to_string())?;
        if entry.get("success").and_then(|v| v.as_bool()) != Some(true) {
            return None;
        }
        let data = entry.get("data")?;
        if let Ok(serialized) = serde_json::to_string(data) {
            let _ = fs::write(&cache_file, serialized);
        }
        let details = Self::parse_details(app_id, data)?;
        self.mem.lock().unwrap().insert(app_id, details.clone());
        Some(details)
    }

    fn parse_details(app_id: u32, data: &Value) -> Option<GameDetails> {
        let obj = data.as_object()?;
        let genres = obj
            .get("genres")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|g| {
                        g.get("description")
                            .and_then(|d| d.as_str())
                            .map(ToString::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();

        let base_app_id = obj
            .get("fullgame")
            .and_then(|fg| fg.get("appid"))
            .and_then(|v| {
                v.as_u64()
                    .map(|n| n as u32)
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u32>().ok()))
            });

        let release_date = obj
            .get("release_date")
            .and_then(|r| r.get("date"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        Some(GameDetails {
            app_id: obj
                .get("steam_appid")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32)
                .unwrap_or(app_id),
            name: obj
                .get("name")
                .and_then(|v| v.as_str())
                .map(ToString::to_string)
                .unwrap_or_else(|| format!("App {}", app_id)),
            app_type: obj
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            base_app_id,
            genres,
            header_image: obj
                .get("header_image")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            release_date,
            is_free: obj
                .get("is_free")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        })
    }
}

#[derive(Debug, Clone)]
pub struct SteamAppListCache {
    cache_path: PathBuf,
    map: Arc<Mutex<HashMap<u32, String>>>,
    loaded: Arc<Mutex<bool>>,
}

impl Default for SteamAppListCache {
    fn default() -> Self {
        Self::new(None)
    }
}

impl SteamAppListCache {
    pub fn new(cache_path: Option<PathBuf>) -> Self {
        let p = cache_path.unwrap_or_else(|| default_luatools_cache_dir().join("applist.json"));
        if let Some(parent) = p.parent() {
            let _ = fs::create_dir_all(parent);
        }
        Self {
            cache_path: p,
            map: Arc::new(Mutex::new(HashMap::new())),
            loaded: Arc::new(Mutex::new(false)),
        }
    }

    pub fn ensure_loaded(&self) {
        {
            let mut l = self.loaded.lock().unwrap();
            if *l {
                return;
            }
            *l = true;
        }

        let mut fresh_enough = false;
        if let Ok(raw) = fs::read_to_string(&self.cache_path) {
            if let Ok(parsed) = serde_json::from_str::<HashMap<String, String>>(&raw) {
                let mut guard = self.map.lock().unwrap();
                for (k, v) in parsed {
                    if let Ok(aid) = k.parse::<u32>() {
                        guard.insert(aid, v);
                    }
                }
                if let Ok(meta) = fs::metadata(&self.cache_path) {
                    if let Ok(mtime) = meta.modified() {
                        if SystemTime::now()
                            .duration_since(mtime)
                            .map(|d| d.as_secs() < TTL_14_DAYS)
                            .unwrap_or(false)
                        {
                            fresh_enough = true;
                        }
                    }
                }
            }
        }

        if fresh_enough {
            return;
        }

        let map_ref = Arc::clone(&self.map);
        let path_clone = self.cache_path.clone();
        thread::spawn(move || {
            let agent = ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(30))
                .build();
            let Ok(resp) = agent.get("https://applist.morrenus.xyz/").call() else {
                return;
            };
            let Ok(val) = resp.into_json::<Value>() else {
                return;
            };
            let apps_opt = val
                .get("applist")
                .and_then(|a| a.get("apps"))
                .and_then(|x| x.as_array())
                .or_else(|| val.as_array());
            let Some(apps) = apps_opt else {
                return;
            };

            let mut new_map = HashMap::new();
            let mut disk_map = HashMap::new();
            for item in apps {
                let aid = item.get("appid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if aid > 0 && !name.is_empty() {
                    new_map.insert(aid, name.to_string());
                    disk_map.insert(aid.to_string(), name.to_string());
                }
            }
            if !new_map.is_empty() {
                *map_ref.lock().unwrap() = new_map;
                if let Ok(js) = serde_json::to_string(&disk_map) {
                    let _ = fs::write(&path_clone, js);
                }
            }
        });
    }

    pub fn get_name(&self, app_id: u32) -> Option<String> {
        self.ensure_loaded();
        self.map.lock().unwrap().get(&app_id).cloned()
    }
}

#[derive(Debug, Clone)]
pub struct CoverCache {
    pub cache_dir: PathBuf,
}

impl Default for CoverCache {
    fn default() -> Self {
        Self::new(None)
    }
}

impl CoverCache {
    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        let dir = cache_dir.unwrap_or_else(|| default_luatools_cache_dir().join("covers"));
        let _ = fs::create_dir_all(&dir);
        Self { cache_dir: dir }
    }

    pub fn get_cached_path(&self, app_id: u32) -> Option<PathBuf> {
        let p = self.cache_dir.join(format!("{}.jpg", app_id));
        if let Ok(meta) = fs::metadata(&p) {
            if meta.len() > 100 {
                return Some(p);
            }
        }
        None
    }

    pub fn ensure(&self, app_id: u32, custom_url: Option<&str>) -> Option<PathBuf> {
        if let Some(existing) = self.get_cached_path(app_id) {
            return Some(existing);
        }

        let mut candidates = Vec::new();
        if let Some(u) = custom_url.filter(|s| !s.is_empty()) {
            candidates.push(u.to_string());
        }
        candidates.push(format!(
            "https://shared.fastly.steamstatic.com/store_item_assets/steam/apps/{}/header.jpg",
            app_id
        ));
        candidates.push(format!(
            "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/header.jpg",
            app_id
        ));

        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(15))
            .build();

        for url in candidates {
            let Ok(resp) = agent.get(&url).call() else {
                continue;
            };
            let mut bytes = Vec::new();
            if resp
                .into_reader()
                .take(4 * 1024 * 1024)
                .read_to_end(&mut bytes)
                .is_err()
            {
                continue;
            }
            if bytes.len() < 4 || (&bytes[0..3] != b"\xFF\xD8\xFF" && &bytes[0..4] != b"\x89PNG") {
                continue;
            }
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let digest = format!("{:x}", hasher.finalize());
            if digest == STEAM_PLACEHOLDER_SHA256 {
                continue;
            }
            let dest = self.cache_dir.join(format!("{}.jpg", app_id));
            if fs::write(&dest, &bytes).is_ok() {
                return Some(dest);
            }
        }
        None
    }
}

#[derive(Debug, Clone)]
pub struct HardwareAppIdService {
    cache_path: PathBuf,
    ids: Arc<Mutex<HashSet<u32>>>,
    loaded: Arc<Mutex<bool>>,
}

impl Default for HardwareAppIdService {
    fn default() -> Self {
        Self::new(None)
    }
}

impl HardwareAppIdService {
    pub fn new(cache_path: Option<PathBuf>) -> Self {
        let p =
            cache_path.unwrap_or_else(|| default_luatools_cache_dir().join("hardware_appid.json"));
        if let Some(parent) = p.parent() {
            let _ = fs::create_dir_all(parent);
        }
        Self {
            cache_path: p,
            ids: Arc::new(Mutex::new(HashSet::new())),
            loaded: Arc::new(Mutex::new(false)),
        }
    }

    pub fn is_hardware(&self, app_id: u32) -> bool {
        self.ensure_loaded();
        self.ids.lock().unwrap().contains(&app_id)
    }

    fn ensure_loaded(&self) {
        {
            let mut l = self.loaded.lock().unwrap();
            if *l {
                return;
            }
            *l = true;
        }

        if let Ok(raw) = fs::read_to_string(&self.cache_path) {
            if let Ok(arr) = serde_json::from_str::<Vec<Value>>(&raw) {
                let mut set = self.ids.lock().unwrap();
                for item in arr {
                    if let Some(aid) = item.get("appid").and_then(|v| v.as_u64()) {
                        set.insert(aid as u32);
                    }
                }
                return;
            }
        }

        let ids_ref = Arc::clone(&self.ids);
        let path_clone = self.cache_path.clone();
        thread::spawn(move || {
            let agent = ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(15))
                .build();
            let Ok(resp) = agent.get(HARDWARE_APPID_URL).call() else {
                return;
            };
            let Ok(text) = resp.into_string() else {
                return;
            };
            let _ = fs::write(&path_clone, &text);
            if let Ok(arr) = serde_json::from_str::<Vec<Value>>(&text) {
                let mut set = ids_ref.lock().unwrap();
                for item in arr {
                    if let Some(aid) = item.get("appid").and_then(|v| v.as_u64()) {
                        set.insert(aid as u32);
                    }
                }
            }
        });
    }
}
