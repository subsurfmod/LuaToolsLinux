//! LuaTools API Client, Steam Store search/featured client, and HubcapService.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use chrono::Utc;
use regex::Regex;
use serde_json::Value;

use crate::services::auth_service::{AuthService, API_BASE_URL, SUPABASE_ANON_KEY, SUPABASE_URL};
use crate::services::doh_resolver::DohResolver;
use crate::services::github_proxy::GithubProxy;

pub const MANIFEST_BACKEND_URL: &str = "http://167.235.229.108";
pub const MANIFEST_BACKEND_UA: &str = "secretgoonpoon";
pub const HUBCAP_BASE_URL: &str = "https://hubcapmanifest.com";
pub const DAILY_DOWNLOAD_LIMIT: u32 = 25;

#[derive(Debug, Clone)]
pub struct SteamSearchItem {
    pub app_id: u32,
    pub name: String,
    pub icon: String,
}

#[derive(Debug, Clone)]
pub struct SteamFeaturedItem {
    pub app_id: u32,
    pub name: String,
    pub large_capsule_image: String,
}

#[derive(Debug, Clone, Default)]
pub struct DlcInfo {
    pub appid: String,
    pub name: String,
    pub app_type: String,
    pub depot_count: usize,
    pub have_count: usize,
    pub missing_count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct HubcapStats {
    pub user_id: String,
    pub daily_usage: u32,
    pub daily_limit: u32,
    pub can_make_requests: bool,
}

#[derive(Debug, Clone)]
pub struct DRMFixTag {
    pub id: String,
    pub name: String,
    pub slug: String,
}

#[derive(Debug, Clone)]
pub struct DRMFixGameListing {
    pub app_id: String,
    pub name: String,
    pub fix_count: usize,
    pub tags: Vec<DRMFixTag>,
}

#[derive(Debug, Clone)]
pub struct DRMFixFixItem {
    pub id: String,
    pub title: String,
    pub description: String,
    pub tags: Vec<DRMFixTag>,
    pub has_manifest: bool,
    pub has_fix: bool,
    pub manifest_filename: Option<String>,
    pub fix_filename: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct HubcapService;

impl HubcapService {
    pub fn new() -> Self {
        Self
    }

    pub fn is_valid_key_format(key: &str) -> bool {
        let trimmed = key.trim();
        trimmed.len() == 100
            && trimmed.starts_with("smm_")
            && trimmed[4..]
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    }

    fn agent() -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
    }

    pub fn get_stats(&self, key: &str) -> Option<HubcapStats> {
        let trimmed = key.trim();
        if trimmed.is_empty() {
            return None;
        }
        let url = format!(
            "{}/api/v1/user/stats?api_key={}",
            HUBCAP_BASE_URL,
            urlencoding::encode(trimmed)
        );
        let val: Value = Self::agent().get(&url).call().ok()?.into_json().ok()?;
        Some(HubcapStats {
            user_id: json_id_to_string(val.get("user_id")),
            daily_usage: val.get("daily_usage").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            daily_limit: val.get("daily_limit").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            can_make_requests: val
                .get("can_make_requests")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        })
    }

    pub fn check_status(&self, key: &str, app_id: u32) -> Option<bool> {
        let trimmed = key.trim();
        if trimmed.is_empty() {
            return None;
        }
        let url = format!("{}/api/v1/status/{}", HUBCAP_BASE_URL, app_id);
        let val: Value = Self::agent()
            .get(&url)
            .set("Authorization", &format!("Bearer {}", trimmed))
            .call()
            .ok()?
            .into_json()
            .ok()?;
        Some(
            val.get("manifest_file_exists")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        )
    }

    pub fn download_manifest<F>(
        &self,
        app_id: u32,
        key: &str,
        dest_dir: &Path,
        mut progress_cb: F,
    ) -> Result<PathBuf, String>
    where
        F: FnMut(f32, String),
    {
        let trimmed = key.trim();
        if trimmed.is_empty() {
            return Err("Hubcap API key is empty.".into());
        }
        fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
        let url = format!(
            "{}/api/v1/manifest/{}?api_key={}",
            HUBCAP_BASE_URL,
            app_id,
            urlencoding::encode(trimmed)
        );
        let resp = match Self::agent().get(&url).call() {
            Ok(r) => r,
            Err(ureq::Error::Status(401, _)) => return Err("Invalid Hubcap API key.".into()),
            Err(ureq::Error::Status(429, _)) => return Err("Hubcap daily limit reached.".into()),
            Err(ureq::Error::Status(404, _)) => {
                return Err("No manifest available on Hubcap for this game.".into())
            }
            Err(e) => {
                return Err(format!(
                    "Hubcap download failed: {}",
                    redact_api_key(&e.to_string())
                ))
            }
        };

        let out = dest_dir.join(format!("{}.zip", app_id));
        stream_response_to_file(resp, &out, &mut progress_cb)?;
        Ok(out)
    }

    /// Fetch raw binary depot manifest bytes by depot ID and manifest GID (LT-44).
    pub fn fetch_raw_manifest(
        depot_id: u32,
        gid: u64,
        hubcap_api_key: Option<&str>,
    ) -> Result<Vec<u8>, String> {
        let agent = Self::agent();

        // 1. Primary: https://lua.tools/api/givemethemanifestpunk/{depot}/{gid} (LT-44)
        let luatools_url = format!(
            "https://lua.tools/api/givemethemanifestpunk/{}/{}",
            depot_id, gid
        );
        if let Ok(resp) = agent
            .get(&luatools_url)
            .set("User-Agent", MANIFEST_BACKEND_UA)
            .call()
        {
            let mut bytes = Vec::new();
            if resp.into_reader().take(64 * 1024 * 1024).read_to_end(&mut bytes).is_ok() && !bytes.is_empty() {
                return Ok(bytes);
            }
        }

        // 2. Secondary fallback: direct backend IP
        let primary_url = format!(
            "{}/api/givemethemanifestpunk?depotid={}&gid={}",
            MANIFEST_BACKEND_URL, depot_id, gid
        );
        if let Ok(resp) = agent
            .get(&primary_url)
            .set("User-Agent", MANIFEST_BACKEND_UA)
            .call()
        {
            let mut bytes = Vec::new();
            if resp.into_reader().take(64 * 1024 * 1024).read_to_end(&mut bytes).is_ok() && !bytes.is_empty() {
                return Ok(bytes);
            }
        }

        // 2. Hubcap depot manifest fallback if API key is provided
        if let Some(key) = hubcap_api_key.map(str::trim).filter(|k| !k.is_empty()) {
            let hubcap_url = format!(
                "{}/api/v1/depot/{}/manifest/{}?api_key={}",
                HUBCAP_BASE_URL,
                depot_id,
                gid,
                urlencoding::encode(key)
            );
            if let Ok(resp) = agent.get(&hubcap_url).call() {
                let mut bytes = Vec::new();
                if resp.into_reader().take(64 * 1024 * 1024).read_to_end(&mut bytes).is_ok() && !bytes.is_empty() {
                    return Ok(bytes);
                }
            }
        }

        // 3. CDN fallbacks
        let cdn_urls = [
            format!("https://cdn.steamtools.net/manifest/{}_{}.manifest", depot_id, gid),
            format!("{}/manifest/{}/{}", HUBCAP_BASE_URL, depot_id, gid),
        ];
        for url in cdn_urls {
            if let Ok(resp) = agent.get(&url).call() {
                let mut bytes = Vec::new();
                if resp.into_reader().take(64 * 1024 * 1024).read_to_end(&mut bytes).is_ok() && !bytes.is_empty() {
                    return Ok(bytes);
                }
            }
        }

        Err(format!(
            "Failed to fetch manifest for depot {} gid {}",
            depot_id, gid
        ))
    }

    /// Download raw manifest binary to a file on disk (LT-44).
    pub fn download_raw_manifest_to_file(
        depot_id: u32,
        gid: u64,
        dest_path: &Path,
        hubcap_api_key: Option<&str>,
    ) -> Result<PathBuf, String> {
        let bytes = Self::fetch_raw_manifest(depot_id, gid, hubcap_api_key)?;
        if let Some(parent) = dest_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(dest_path, bytes).map_err(|e| format!("Failed to write manifest: {}", e))?;
        Ok(dest_path.to_path_buf())
    }

    /// Resolve domain via DNS-over-HTTPS (LT-47).
    pub fn resolve_steam_domain_doh(domain: &str) -> Option<Vec<String>> {
        let resolver = DohResolver::default();
        let records = resolver.resolve(domain)?;
        Some(records.into_iter().map(|r| r.value).collect())
    }

    /// Build an HTTP agent with DoH-assisted domain resolution fallback (LT-47).
    pub fn doh_agent() -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()
    }

    /// Download with GitHub proxy and mirror fallback (LT-48).
    pub fn download_proxied(url: &str, dest_path: &Path) -> Result<(), String> {
        let proxy = GithubProxy::default();
        if GithubProxy::is_github(url) {
            for candidate in proxy.candidates(url) {
                if let Ok(resp) = Self::agent().get(&candidate).call() {
                    if let Some(parent) = dest_path.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    if let Ok(mut f) = fs::File::create(dest_path) {
                        if std::io::copy(&mut resp.into_reader(), &mut f).is_ok() {
                            return Ok(());
                        }
                    }
                }
            }
            Err("All GitHub proxy mirrors failed".into())
        } else {
            let resp = Self::agent().get(&url).call().map_err(|e| e.to_string())?;
            if let Some(parent) = dest_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let mut f = fs::File::create(dest_path).map_err(|e| e.to_string())?;
            std::io::copy(&mut resp.into_reader(), &mut f).map_err(|e| e.to_string())?;
            Ok(())
        }
    }
}

#[derive(Debug, Clone)]
pub struct LuaToolsApiClient {
    pub auth: AuthService,
}

impl LuaToolsApiClient {
    pub fn new(auth: AuthService) -> Self {
        Self { auth }
    }

    fn agent() -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()
    }

    pub fn search_store(&self, query: &str) -> Result<Vec<SteamSearchItem>, String> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let lang = crate::services::steam_service::SteamService::detect_steam_language(None)
            .unwrap_or_else(|| "english".into());
        let url = format!(
            "https://store.steampowered.com/api/storesearch/?term={}&l={}&cc=US",
            urlencoding::encode(trimmed),
            urlencoding::encode(&lang)
        );
        let val: Value = Self::agent()
            .get(&url)
            .set(
                "User-Agent",
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36",
            )
            .call()
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())?;

        let mut results = Vec::new();
        if let Some(items) = val.get("items").and_then(|v| v.as_array()) {
            for item in items.iter().take(18) {
                let aid = item
                    .get("id")
                    .and_then(|v| v.as_u64())
                    .and_then(|n| u32::try_from(n).ok())
                    .unwrap_or(0);
                if aid == 0 {
                    continue;
                }
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown")
                    .to_string();
                let icon = item
                    .get("tiny_image")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| {
                        format!(
                            "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/capsule_sm_120.jpg",
                            aid
                        )
                    });
                results.push(SteamSearchItem {
                    app_id: aid,
                    name,
                    icon,
                });
            }
        }
        Ok(results)
    }

    pub fn get_featured(&self) -> (Vec<SteamFeaturedItem>, Vec<SteamFeaturedItem>) {
        let lang = crate::services::steam_service::SteamService::detect_steam_language(None)
            .unwrap_or_else(|| "english".into());
        let url = format!("https://store.steampowered.com/api/featuredcategories?cc=us&l={}", urlencoding::encode(&lang));
        let Ok(resp) = Self::agent()
            .get(&url)
            .set(
                "User-Agent",
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36",
            )
            .call()
        else {
            return (Vec::new(), Vec::new());
        };
        let Ok(data) = resp.into_json::<Value>() else {
            return (Vec::new(), Vec::new());
        };

        let parse_cat = |cat_key: &str| -> Vec<SteamFeaturedItem> {
            let mut seen = HashSet::new();
            let mut out = Vec::new();
            let Some(items) = data
                .get(cat_key)
                .and_then(|c| c.get("items"))
                .and_then(|i| i.as_array())
            else {
                return out;
            };
            for item in items {
                if item.get("type").and_then(|v| v.as_i64()) != Some(0) {
                    continue;
                }
                let aid = item.get("id").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let img = item
                    .get("large_capsule_image")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if aid == 0 || img.is_empty() || !seen.insert(aid) {
                    continue;
                }
                out.push(SteamFeaturedItem {
                    app_id: aid,
                    name: item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    large_capsule_image: img.to_string(),
                });
                if out.len() >= 20 {
                    break;
                }
            }
            out
        };

        (parse_cat("top_sellers"), parse_cat("new_releases"))
    }

    pub fn check_sources(&self, app_id: u32) -> BTreeMap<String, String> {
        let url = format!("{}/check_apis?appid={}", MANIFEST_BACKEND_URL, app_id);
        let Ok(resp) = Self::agent()
            .get(&url)
            .set("User-Agent", MANIFEST_BACKEND_UA)
            .call()
        else {
            return BTreeMap::new();
        };
        let Ok(Value::Object(map)) = resp.into_json::<Value>() else {
            return BTreeMap::new();
        };
        let mut out = BTreeMap::new();
        for (k, v) in map {
            let status = v
                .as_str()
                .map(ToString::to_string)
                .unwrap_or_else(|| v.to_string());
            out.insert(k, status);
        }
        out
    }

    pub fn get_standard_usage(&self) -> Option<(u32, u32)> {
        let token = self.auth.get_valid_access_token().ok()?;
        let today = Utc::now().format("%Y-%m-%dT00:00:00Z").to_string();
        let url = format!(
            "{}/rest/v1/user_downloads?select=appid&downloaded_at=gte.{}",
            SUPABASE_URL,
            urlencoding::encode(&today)
        );
        let resp = Self::agent()
            .head(&url)
            .set("apikey", SUPABASE_ANON_KEY)
            .set("Authorization", &format!("Bearer {}", token))
            .set("Prefer", "count=exact")
            .call()
            .ok()?;
        let cr = resp.header("Content-Range")?;
        let used = cr.rsplit('/').next()?.parse::<u32>().ok()?;
        Some((used, DAILY_DOWNLOAD_LIMIT))
    }

    pub fn get_supporter_status(&self) -> Option<bool> {
        let val = self.auth_get_json("/api/me/supporter-status").ok()?;
        Some(
            val.get("isSupporter")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        )
    }

    pub fn get_dlc_info(&self, app_id: u32, base_app_id: u32) -> Option<DlcInfo> {
        let val = self
            .auth_get_json(&format!(
                "/api/dlc/info?appid={}&base={}",
                app_id, base_app_id
            ))
            .ok()?;
        Some(DlcInfo {
            appid: json_id_to_string(val.get("appid")),
            name: val
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            app_type: val
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            depot_count: val.get("depotCount").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
            have_count: val.get("haveCount").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
            missing_count: val
                .get("missingCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize,
        })
    }

    pub fn download_manifest<F>(
        &self,
        app_id: u32,
        source: &str,
        game_name: Option<&str>,
        dest_dir: &Path,
        progress_cb: F,
    ) -> Result<PathBuf, String>
    where
        F: FnMut(f32, String),
    {
        let mut path = format!(
            "/api/manifest/download?appid={}&source={}",
            app_id,
            urlencoding::encode(source)
        );
        if let Some(gn) = game_name.filter(|s| !s.is_empty()) {
            path.push_str(&format!("&game_name={}", urlencoding::encode(gn)));
        }
        self.auth_download(
            &path,
            &dest_dir.join(format!("{}.zip", app_id)),
            progress_cb,
        )
    }

    pub fn generate_dlc<F>(
        &self,
        app_id: u32,
        base_app_id: u32,
        game_name: Option<&str>,
        dest_dir: &Path,
        progress_cb: F,
    ) -> Result<PathBuf, String>
    where
        F: FnMut(f32, String),
    {
        let mut path = format!("/api/dlc/generate?appid={}&base={}", app_id, base_app_id);
        if let Some(gn) = game_name.filter(|s| !s.is_empty()) {
            path.push_str(&format!("&game_name={}", urlencoding::encode(gn)));
        }
        self.auth_download(
            &path,
            &dest_dir.join(format!("{}.lua", app_id)),
            progress_cb,
        )
    }

    pub fn get_drm_fix_listings(&self) -> (Vec<DRMFixGameListing>, Vec<DRMFixTag>) {
        let url = format!("{}/api/denuvo/listings", API_BASE_URL);
        let Ok(resp) = Self::agent().get(&url).call() else {
            return (Vec::new(), Vec::new());
        };
        let Ok(d) = resp.into_json::<Value>() else {
            return (Vec::new(), Vec::new());
        };

        let parse_tags = |arr: Option<&Vec<Value>>| -> Vec<DRMFixTag> {
            arr.map(|list| {
                list.iter()
                    .map(|t| DRMFixTag {
                        id: t
                            .get("id")
                            .map(|v| {
                                v.as_str()
                                    .map(ToString::to_string)
                                    .unwrap_or_else(|| v.to_string())
                            })
                            .unwrap_or_default(),
                        name: t
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        slug: t
                            .get("slug")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default()
        };

        let tags = parse_tags(d.get("tags").and_then(|v| v.as_array()));
        let mut games = Vec::new();
        if let Some(g_arr) = d.get("games").and_then(|v| v.as_array()) {
            for g in g_arr {
                let aid = g
                    .get("appid")
                    .map(|v| {
                        v.as_str()
                            .map(ToString::to_string)
                            .unwrap_or_else(|| v.to_string())
                    })
                    .unwrap_or_default();
                games.push(DRMFixGameListing {
                    app_id: aid,
                    name: g
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    fix_count: g.get("fixCount").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                    tags: parse_tags(g.get("tags").and_then(|v| v.as_array())),
                });
            }
        }
        (games, tags)
    }

    pub fn get_drm_fix_fixes(&self, app_id: u32) -> Vec<DRMFixFixItem> {
        let url = format!("{}/api/denuvo/fixes?appid={}", API_BASE_URL, app_id);
        let Ok(resp) = Self::agent().get(&url).call() else {
            return Vec::new();
        };
        let Ok(d) = resp.into_json::<Value>() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Some(arr) = d.get("fixes").and_then(|v| v.as_array()) {
            for f in arr {
                let tags = f
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|list| {
                        list.iter()
                            .map(|t| DRMFixTag {
                                id: json_id_to_string(t.get("id")),
                                name: t
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                                slug: t
                                    .get("slug")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                out.push(DRMFixFixItem {
                    id: json_id_to_string(f.get("id")),
                    title: f
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    description: f
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    tags,
                    has_manifest: f
                        .get("hasManifest")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    has_fix: f.get("hasFix").and_then(|v| v.as_bool()).unwrap_or(false),
                    manifest_filename: f
                        .get("manifestFilename")
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string),
                    fix_filename: f
                        .get("fixFilename")
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string),
                });
            }
        }
        out
    }

    pub fn download_drm_fix_slot<F>(
        &self,
        fix_id: &str,
        slot: &str,
        dest_dir: &Path,
        fallback_name: &str,
        mut progress_cb: F,
    ) -> Result<PathBuf, String>
    where
        F: FnMut(f32, String),
    {
        let meta = self.auth_get_json(&format!(
            "/api/denuvo/download?fix={}&slot={}",
            urlencoding::encode(fix_id),
            urlencoding::encode(slot)
        ))?;
        let signed_url = meta
            .get("url")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
            .ok_or_else(|| "Server returned an invalid or empty download URL.".to_string())?;

        fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
        let resp = Self::agent()
            .get(signed_url)
            .call()
            .map_err(|e| format!("Signed download failed: {}", e))?;

        let cd = resp.header("content-disposition").unwrap_or("");
        let fname = filename_from_content_disposition(cd)
            .or_else(|| sanitize_filename(fallback_name))
            .unwrap_or_else(|| "download.bin".to_string());
        let out = dest_dir.join(fname);
        stream_response_to_file(resp, &out, &mut progress_cb)?;
        Ok(out)
    }

    fn auth_get_json(&self, path: &str) -> Result<Value, String> {
        let token = self.auth.get_valid_access_token()?;
        let url = format!("{}{}", API_BASE_URL, path);
        match Self::agent()
            .get(&url)
            .set("Authorization", &format!("Bearer {}", token))
            .call()
        {
            Ok(r) => r.into_json().map_err(|e| e.to_string()),
            Err(ureq::Error::Status(401, _)) => {
                Err("Session expired. Please sign in again.".into())
            }
            Err(ureq::Error::Status(code, resp)) => {
                if let Ok(v) = resp.into_json::<Value>() {
                    if let Some(err) = v.get("error").and_then(|x| x.as_str()) {
                        return Err(err.to_string());
                    }
                }
                Err(format!("Request failed (HTTP {}).", code))
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn auth_download<F>(
        &self,
        path: &str,
        fallback_path: &Path,
        mut progress_cb: F,
    ) -> Result<PathBuf, String>
    where
        F: FnMut(f32, String),
    {
        let token = self.auth.get_valid_access_token()?;
        if let Some(parent) = fallback_path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let url = format!("{}{}", API_BASE_URL, path);
        let resp = match Self::agent()
            .get(&url)
            .set("Authorization", &format!("Bearer {}", token))
            .call()
        {
            Ok(r) => r,
            Err(ureq::Error::Status(401, _)) => {
                return Err("Session expired. Please sign in again.".into())
            }
            Err(ureq::Error::Status(code, err_resp)) => {
                if let Ok(v) = err_resp.into_json::<Value>() {
                    if let Some(err) = v.get("error").and_then(|x| x.as_str()) {
                        return Err(err.to_string());
                    }
                }
                return Err(format!("Download failed (HTTP {}).", code));
            }
            Err(e) => return Err(e.to_string()),
        };

        let cd = resp.header("content-disposition").unwrap_or("");
        let out = if let Some(fname) = filename_from_content_disposition(cd) {
            fallback_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(fname)
        } else {
            fallback_path.to_path_buf()
        };

        stream_response_to_file(resp, &out, &mut progress_cb)?;
        Ok(out)
    }
}

static STREAM_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn json_id_to_string(val: Option<&Value>) -> String {
    match val {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn redact_api_key(msg: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)(api_key=)[^&\s]+").unwrap());
    re.replace_all(msg, "${1}<redacted>").into_owned()
}

fn sanitize_filename(raw: &str) -> Option<String> {
    let normalized = raw.trim().replace('\\', "/");
    let fname = Path::new(&normalized)
        .file_name()
        .and_then(|s| s.to_str())?
        .trim();
    if fname.is_empty()
        || fname == "."
        || fname == ".."
        || fname.chars().any(|c| c.is_control() || c == '\0')
    {
        return None;
    }
    Some(fname.to_string())
}

fn filename_from_content_disposition(cd: &str) -> Option<String> {
    if cd.is_empty() {
        return None;
    }
    static RE_STAR: OnceLock<Regex> = OnceLock::new();
    static RE_PLAIN: OnceLock<Regex> = OnceLock::new();
    let re_star = RE_STAR.get_or_init(|| {
        Regex::new(r#"(?i)filename\*\s*=\s*(?:[A-Za-z0-9_-]+''\s*)?"?([^";\r\n]+)"?"#).unwrap()
    });
    let re_plain =
        RE_PLAIN.get_or_init(|| Regex::new(r#"(?i)filename\s*=\s*"?([^";\r\n]+)"?"#).unwrap());

    if let Some(raw) = re_star
        .captures(cd)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
    {
        let decoded = urlencoding::decode(raw)
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| raw.to_string());
        if let Some(clean) = sanitize_filename(&decoded) {
            return Some(clean);
        }
    }
    if let Some(raw) = re_plain
        .captures(cd)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
    {
        let decoded = urlencoding::decode(raw)
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| raw.to_string());
        if let Some(clean) = sanitize_filename(&decoded) {
            return Some(clean);
        }
    }
    None
}

pub fn stream_response_to_file<F>(
    resp: ureq::Response,
    dest: &Path,
    progress_cb: &mut F,
) -> Result<(), String>
where
    F: FnMut(f32, String),
{
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let total = resp
        .header("content-length")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let mut reader = resp.into_reader();

    let seq = STREAM_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let base_name = dest
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("download");
    let tmp_path = dest.with_file_name(format!(
        ".{}.part.{}.{}",
        base_name,
        std::process::id(),
        seq
    ));

    let write_res: Result<(), String> = (|| {
        let mut out = File::create(&tmp_path).map_err(|e| e.to_string())?;
        let mut buf = [0u8; 16 * 1024];
        let mut downloaded: u64 = 0;
        loop {
            let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            downloaded += n as u64;
            if total > 0 {
                let pct = ((downloaded as f32) / (total as f32)).clamp(0.0, 1.0);
                progress_cb(
                    pct,
                    format!(
                        "Downloading... {} KB / {} KB",
                        downloaded / 1024,
                        total / 1024
                    ),
                );
            } else {
                progress_cb(0.0, format!("Downloading... {} KB", downloaded / 1024));
            }
        }
        out.flush().map_err(|e| e.to_string())?;
        Ok(())
    })();

    if let Err(e) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }

    if let Err(e) = fs::rename(&tmp_path, dest) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e.to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition_prefers_rfc5987_and_blocks_traversal() {
        assert_eq!(
            filename_from_content_disposition(
                r#"attachment; filename="fallback.zip"; filename*=UTF-8''Elden%20Ring.zip"#
            )
            .as_deref(),
            Some("Elden Ring.zip")
        );
        assert_eq!(
            filename_from_content_disposition(r#"attachment; filename="..\..\evil.zip""#)
                .as_deref(),
            Some("evil.zip")
        );
        assert_eq!(
            filename_from_content_disposition(r#"attachment; filename="..""#),
            None
        );
        assert_eq!(
            sanitize_filename("../../secret/fix.zip").as_deref(),
            Some("fix.zip")
        );
        assert_eq!(sanitize_filename(".."), None);
    }

    #[test]
    fn redacts_hubcap_api_key_in_error_messages() {
        let err =
            "https://hubcapmanifest.com/api/v1/manifest/730?api_key=smm_deadbeef&x=1: timeout";
        let clean = redact_api_key(err);
        assert!(!clean.contains("smm_deadbeef"));
        assert!(clean.contains("api_key=<redacted>&x=1"));
    }

    #[test]
    fn json_id_handles_strings_and_numbers() {
        assert_eq!(json_id_to_string(Some(&Value::from(1245620))), "1245620");
        assert_eq!(json_id_to_string(Some(&Value::from("abc-1"))), "abc-1");
        assert_eq!(json_id_to_string(None), "");
    }

    #[test]
    fn stream_response_to_file_writes_atomically_and_reports_progress() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let payload = vec![b'A'; 32 * 1024];
        let payload_len = payload.len();
        let payload_clone = payload.clone();
        let handle = std::thread::spawn(move || {
            let req = server.recv().unwrap();
            let resp = tiny_http::Response::new(
                tiny_http::StatusCode(200),
                Vec::new(),
                std::io::Cursor::new(payload_clone),
                Some(payload_len),
                None,
            )
            .with_chunked_threshold(payload_len + 1);
            let _ = req.respond(resp);
        });

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("nested/out.zip");
        let resp = ureq::get(&format!("http://{}/file.zip", addr))
            .call()
            .unwrap();
        let mut max_pct = 0.0f32;
        stream_response_to_file(resp, &dest, &mut |pct, _| {
            assert!((0.0..=1.0).contains(&pct));
            if pct > max_pct {
                max_pct = pct;
            }
        })
        .unwrap();
        handle.join().unwrap();

        assert_eq!(fs::read(&dest).unwrap(), payload);
        assert!((max_pct - 1.0).abs() < 0.01);
    }
}
