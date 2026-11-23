//! Metadata & Cover Caches: `SteamAppInfoCache`, `SteamAppListCache`, `CoverCache`, `HardwareAppIdService`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::models::config::default_luatools_cache_dir;

const STEAM_PLACEHOLDER_SHA256: &str =
    "732ec27ff347292488e3341f00d0750a31f42c44f55da4e04bc8f61d7eb1b6f3";
const STEAM_HTTP_UA: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
const HARDWARE_APPID_URL: &str =
    "https://raw.githubusercontent.com/jsnli/steamappidlist/master/data/hardware_appid.json";
const TTL_14_DAYS: u64 = 14 * 24 * 3600;
#[allow(dead_code)]
const RATE_LIMIT_WINDOW_SECS: u64 = 200;
#[allow(dead_code)]
const MAX_REQ_PER_WINDOW: usize = 190;
#[allow(dead_code)]
const REQUEST_TIMESTAMPS_FILE: &str = "request_timestamps.json";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppFilterData {
    pub app_type: Option<String>,
    pub genres: Vec<String>,
    pub windows: bool,
    pub mac: bool,
    pub linux: bool,
    pub release_year: Option<i32>,
    pub release_date: Option<String>,
    pub release_date_text: Option<String>,
    pub is_free: bool,
    pub metacritic: Option<i32>,
    pub reviews: Option<u64>,
    pub is_adult: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppOverview {
    pub short_description: Option<String>,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
    pub genres: Vec<String>,
}

impl AppOverview {
    pub fn has_description(&self) -> bool {
        !self
            .short_description
            .as_ref()
            .map(|s| s.trim().is_empty())
            .unwrap_or(true)
    }
    pub fn has_developer(&self) -> bool {
        !self.developers.is_empty()
    }
    pub fn has_distinct_publisher(&self) -> bool {
        !self.publishers.is_empty()
            && !self
                .publishers
                .iter()
                .any(|p| self.developers.iter().any(|d| d.eq_ignore_ascii_case(p)))
    }
    pub fn has_genres(&self) -> bool {
        !self.genres.is_empty()
    }
    pub fn has_anything(&self) -> bool {
        self.has_description() || self.has_developer() || self.has_genres()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GameDetails {
    pub app_id: u32,
    pub name: String,
    pub app_type: String,
    pub base_app_id: Option<u32>,
    pub genres: Vec<String>,
    pub header_image: String,
    pub release_date: String,
    pub is_free: bool,
    // Extended fields for filtering
    pub windows: bool,
    pub mac: bool,
    pub linux: bool,
    pub release_date_text: Option<String>,
    pub release_year: Option<i32>,
    pub metacritic: Option<i32>,
    pub reviews: Option<u64>,
    pub is_adult: bool,
    pub short_description: Option<String>,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
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
        let payload: Value = agent
            .get(&url)
            .set("User-Agent", STEAM_HTTP_UA)
            .call()
            .ok()?
            .into_json()
            .ok()?;
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
            // Extended fields
            windows: obj
                .get("platforms")
                .and_then(|p| p.get("windows"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            mac: obj
                .get("platforms")
                .and_then(|p| p.get("mac"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            linux: obj
                .get("platforms")
                .and_then(|p| p.get("linux"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            release_date_text: obj
                .get("release_date")
                .and_then(|r| r.get("date"))
                .and_then(|v| v.as_str())
                .map(ToString::to_string),
            release_year: obj
                .get("release_date")
                .and_then(|r| r.get("date"))
                .and_then(|v| v.as_str())
                .and_then(|s| {
                    let year_str = s.split_whitespace().last().unwrap_or(s);
                    year_str.parse::<i32>().ok()
                }),
            metacritic: obj
                .get("metacritic")
                .and_then(|m| m.get("score"))
                .and_then(|v| v.as_i64())
                .map(|n| n as i32),
            reviews: obj
                .get("recommendations")
                .and_then(|r| r.get("total"))
                .and_then(|v| v.as_u64()),
            is_adult: obj
                .get("content_descriptors")
                .and_then(|cd| cd.get("ids"))
                .and_then(|ids| ids.as_array())
                .map(|arr| {
                    arr.iter()
                        .any(|v| v.as_i64().map(|id| id == 3 || id == 4).unwrap_or(false))
                })
                .unwrap_or(false),
            short_description: obj
                .get("short_description")
                .and_then(|v| v.as_str())
                .map(ToString::to_string),
            developers: obj
                .get("developers")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(ToString::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            publishers: obj
                .get("publishers")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(ToString::to_string)
                        .collect()
                })
                .unwrap_or_default(),
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
    details_cache_dir: PathBuf,
    steam_root: Option<PathBuf>,
    last_api_req: Arc<Mutex<Option<Instant>>>,
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
        Self {
            cache_dir: dir,
            details_cache_dir: default_luatools_cache_dir().join("details"),
            steam_root: None,
            last_api_req: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_steam_root(&mut self, root: Option<PathBuf>) {
        self.steam_root = root;
    }

    pub fn set_details_cache_dir(&mut self, dir: PathBuf) {
        self.details_cache_dir = dir;
    }

    fn is_valid_image_bytes(bytes: &[u8]) -> bool {
        if bytes.len() <= 100 {
            return false;
        }
        let has_magic = &bytes[0..3] == b"\xFF\xD8\xFF" || &bytes[0..4] == b"\x89PNG";
        if !has_magic {
            return false;
        }
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = format!("{:x}", hasher.finalize());
        if digest == STEAM_PLACEHOLDER_SHA256 {
            return false;
        }
        image::load_from_memory(bytes).is_ok()
    }

    fn is_low_res_capsule_url(url: &str) -> bool {
        let lower = url.to_ascii_lowercase();
        lower.contains("capsule_sm_120") || lower.contains("capsule_231x87")
    }

    pub fn get_cached_path(&self, app_id: u32) -> Option<PathBuf> {
        let p = self.cache_dir.join(format!("{}.jpg", app_id));
        let bytes = fs::read(&p).ok()?;
        if bytes.len() > 100 && (&bytes[0..3] == b"\xFF\xD8\xFF" || &bytes[0..4] == b"\x89PNG") {
            return Some(p);
        }
        // Purge corrupt / non-image cache entries so the cover can be re-fetched cleanly
        let _ = fs::remove_file(&p);
        None
    }

    fn find_local_steam_cover(&self, app_id: u32) -> Option<PathBuf> {
        let libcache = self.steam_root.as_ref()?.join("appcache/librarycache");
        if !libcache.is_dir() {
            return None;
        }

        let mut candidates = vec![
            libcache.join(format!("{}_header.jpg", app_id)),
            libcache.join(format!("{}/header.jpg", app_id)),
            libcache.join(format!("{}_capsule_616x353.jpg", app_id)),
            libcache.join(format!("{}/capsule_616x353.jpg", app_id)),
        ];

        // Modern Steam stores hashed cover assets under appcache/librarycache/<appid>/<sha1>/header.jpg
        let app_dir = libcache.join(app_id.to_string());
        if let Ok(entries) = fs::read_dir(&app_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        let lower = name.to_ascii_lowercase();
                        if (lower.starts_with("header") || lower.contains("capsule_616x353"))
                            && (lower.ends_with(".jpg")
                                || lower.ends_with(".jpeg")
                                || lower.ends_with(".png"))
                        {
                            candidates.push(path);
                        }
                    }
                } else if path.is_dir() {
                    let hashed_header = path.join("header.jpg");
                    if hashed_header.is_file() {
                        candidates.push(hashed_header);
                    }
                    let hashed_capsule = path.join("capsule_616x353.jpg");
                    if hashed_capsule.is_file() {
                        candidates.push(hashed_capsule);
                    }
                }
            }
        }

        for cand in candidates {
            if let Ok(bytes) = fs::read(&cand) {
                if Self::is_valid_image_bytes(&bytes) {
                    let dest = self.cache_dir.join(format!("{}.jpg", app_id));
                    if fs::write(&dest, &bytes).is_ok() {
                        return Some(dest);
                    }
                    return Some(cand);
                }
            }
        }
        None
    }

    fn extract_cover_urls_from_value(data: &Value) -> Vec<String> {
        let mut urls = Vec::new();
        for key in ["header_image", "capsule_image", "capsule_imagev5"] {
            if let Some(u) = data
                .get(key)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
            {
                urls.push(u.to_string());
            }
        }
        urls
    }

    fn urls_from_details_cache(&self, app_id: u32) -> Vec<String> {
        let path = self.details_cache_dir.join(format!("{}.json", app_id));
        let Ok(raw) = fs::read_to_string(&path) else {
            return Vec::new();
        };
        let Ok(val) = serde_json::from_str::<Value>(&raw) else {
            return Vec::new();
        };
        Self::extract_cover_urls_from_value(&val)
    }

    fn fetch_urls_from_appdetails(&self, agent: &ureq::Agent, app_id: u32) -> Vec<String> {
        {
            let mut last = self.last_api_req.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(prev) = *last {
                let elapsed = prev.elapsed();
                if elapsed < Duration::from_millis(150) {
                    thread::sleep(Duration::from_millis(150) - elapsed);
                }
            }
            *last = Some(Instant::now());
        }

        let url = format!(
            "https://store.steampowered.com/api/appdetails?appids={}&filters=basic",
            app_id
        );
        let Ok(resp) = agent.get(&url).set("User-Agent", STEAM_HTTP_UA).call() else {
            return Vec::new();
        };
        let Ok(payload) = resp.into_json::<Value>() else {
            return Vec::new();
        };
        let Some(entry) = payload.get(app_id.to_string()) else {
            return Vec::new();
        };
        if entry.get("success").and_then(|v| v.as_bool()) != Some(true) {
            return Vec::new();
        }
        let Some(data) = entry.get("data") else {
            return Vec::new();
        };
        Self::extract_cover_urls_from_value(data)
    }

    fn download_and_cache(&self, agent: &ureq::Agent, app_id: u32, url: &str) -> Option<PathBuf> {
        let resp = agent
            .get(url)
            .set("User-Agent", STEAM_HTTP_UA)
            .set("Accept", "image/jpeg,image/png,image/*;q=0.8")
            .call()
            .ok()?;
        let mut bytes = Vec::new();
        resp.into_reader()
            .take(4 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .ok()?;
        if !Self::is_valid_image_bytes(&bytes) {
            return None;
        }
        let dest = self.cache_dir.join(format!("{}.jpg", app_id));
        if fs::write(&dest, &bytes).is_ok() {
            return Some(dest);
        }
        None
    }

    pub fn ensure(&self, app_id: u32, custom_url: Option<&str>) -> Option<PathBuf> {
        if let Some(existing) = self.get_cached_path(app_id) {
            return Some(existing);
        }
        if let Some(local) = self.find_local_steam_cover(app_id) {
            return Some(local);
        }

        let mut high_res = Vec::new();
        let mut deferred_low_res = Vec::new();

        if let Some(u) = custom_url.map(str::trim).filter(|s| !s.is_empty()) {
            if Self::is_low_res_capsule_url(u) {
                deferred_low_res.push(u.to_string());
            } else {
                high_res.push(u.to_string());
            }
        }

        // Fast local details JSON lookup (already contains hashed modern CDN URLs when cached)
        for u in self.urls_from_details_cache(app_id) {
            if Self::is_low_res_capsule_url(&u) {
                deferred_low_res.push(u);
            } else {
                high_res.push(u);
            }
        }

        // Legacy unhashed Steam CDN mirrors (fast direct hit for classic catalog entries)
        high_res.push(format!(
            "https://shared.fastly.steamstatic.com/store_item_assets/steam/apps/{}/header.jpg",
            app_id
        ));
        high_res.push(format!(
            "https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/{}/header.jpg",
            app_id
        ));
        high_res.push(format!(
            "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/header.jpg",
            app_id
        ));

        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(12))
            .build();

        let mut tried = HashSet::new();
        for url in high_res {
            if !tried.insert(url.clone()) {
                continue;
            }
            if let Some(dest) = self.download_and_cache(&agent, app_id, &url) {
                return Some(dest);
            }
        }

        // Modern Steam games (2024+) serve hashed asset URLs (<appid>/<sha1>/header.jpg) that 404
        // on unhashed CDN paths. Query Steam's lightweight basic appdetails endpoint to resolve them.
        for url in self.fetch_urls_from_appdetails(&agent, app_id) {
            if !tried.insert(url.clone()) {
                continue;
            }
            if let Some(dest) = self.download_and_cache(&agent, app_id, &url) {
                return Some(dest);
            }
        }

        // Final fallbacks (e.g. search thumbnail or unhashed 616x353 capsule)
        deferred_low_res.push(format!(
            "https://shared.fastly.steamstatic.com/store_item_assets/steam/apps/{}/capsule_616x353.jpg",
            app_id
        ));
        for url in deferred_low_res {
            if !tried.insert(url.clone()) {
                continue;
            }
            if let Some(dest) = self.download_and_cache(&agent, app_id, &url) {
                return Some(dest);
            }
        }

        None
    }
}

#[cfg(test)]
mod cover_tests {
    use super::*;

    fn sample_png_bytes() -> Vec<u8> {
        let mut buf = Vec::new();
        let img = image::RgbaImage::from_fn(32, 16, |x, y| {
            image::Rgba([(x * 8) as u8, (y * 16) as u8, 180, 255])
        });
        let mut cursor = std::io::Cursor::new(&mut buf);
        img.write_to(&mut cursor, image::ImageFormat::Png).unwrap();
        buf
    }

    #[test]
    fn cover_cache_finds_local_steam_librarycache_hashed_header() {
        let tmp = tempfile::tempdir().unwrap();
        let steam_root = tmp.path().join("Steam");
        let hashed_dir = steam_root.join("appcache/librarycache/4435490/deadbeef1234");
        fs::create_dir_all(&hashed_dir).unwrap();
        fs::write(hashed_dir.join("header.jpg"), sample_png_bytes()).unwrap();

        let mut cache = CoverCache::new(Some(tmp.path().join("covers")));
        cache.set_steam_root(Some(steam_root));

        let found = cache.ensure(4435490, None);
        assert!(found.is_some());
        assert!(cache.get_cached_path(4435490).is_some());
    }

    #[test]
    fn cover_cache_extracts_urls_from_details_json_and_purges_corrupt_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let covers_dir = tmp.path().join("covers");
        let details_dir = tmp.path().join("details");
        fs::create_dir_all(&covers_dir).unwrap();
        fs::create_dir_all(&details_dir).unwrap();

        // Corrupt cached HTML file pretending to be a cover
        let bad = covers_dir.join("999.jpg");
        fs::write(&bad, vec![b'<'; 256]).unwrap();

        let mut cache = CoverCache::new(Some(covers_dir));
        cache.set_details_cache_dir(details_dir.clone());
        assert!(cache.get_cached_path(999).is_none());
        assert!(!bad.exists());

        fs::write(
            details_dir.join("4435490.json"),
            r#"{"header_image":"https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/4435490/abc123/header.jpg","capsule_image":"https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/4435490/abc123/capsule_231x87.jpg"}"#,
        )
        .unwrap();
        let urls = cache.urls_from_details_cache(4435490);
        assert_eq!(urls.len(), 2);
        assert!(urls[0].ends_with("/abc123/header.jpg"));
        assert!(CoverCache::is_low_res_capsule_url(&urls[1]));
        assert!(!CoverCache::is_low_res_capsule_url(&urls[0]));
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
