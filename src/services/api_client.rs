//! LuaTools API Client, Steam Store search/featured client, and HubcapService.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use regex::Regex;
use serde_json::Value;

use crate::services::auth_service::{AuthService, API_BASE_URL, SUPABASE_ANON_KEY, SUPABASE_URL};

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
            .timeout(Duration::from_secs(120))
            .build()
    }

    pub fn get_stats(&self, key: &str) -> Option<HubcapStats> {
        let url = format!(
            "{}/api/v1/user/stats?api_key={}",
            HUBCAP_BASE_URL,
            urlencoding::encode(key.trim())
        );
        let val: Value = Self::agent().get(&url).call().ok()?.into_json().ok()?;
        Some(HubcapStats {
            user_id: val
                .get("user_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            daily_usage: val.get("daily_usage").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            daily_limit: val.get("daily_limit").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            can_make_requests: val
                .get("can_make_requests")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        })
    }

    pub fn check_status(&self, key: &str, app_id: u32) -> Option<bool> {
        let url = format!("{}/api/v1/status/{}", HUBCAP_BASE_URL, app_id);
        let val: Value = Self::agent()
            .get(&url)
            .set("Authorization", &format!("Bearer {}", key.trim()))
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
        fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
        let url = format!(
            "{}/api/v1/manifest/{}?api_key={}",
            HUBCAP_BASE_URL,
            app_id,
            urlencoding::encode(key.trim())
        );
        let resp = match Self::agent().get(&url).call() {
            Ok(r) => r,
            Err(ureq::Error::Status(401, _)) => return Err("Invalid Hubcap API key.".into()),
            Err(ureq::Error::Status(429, _)) => return Err("Hubcap daily limit reached.".into()),
            Err(ureq::Error::Status(404, _)) => {
                return Err("No manifest available on Hubcap for this game.".into())
            }
            Err(e) => return Err(format!("Hubcap download failed: {}", e)),
        };

        let out = dest_dir.join(format!("{}.zip", app_id));
        stream_response_to_file(resp, &out, &mut progress_cb)?;
        Ok(out)
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
            .timeout(Duration::from_secs(60))
            .build()
    }

    pub fn search_store(&self, query: &str) -> Result<Vec<SteamSearchItem>, String> {
        let url = format!(
            "https://store.steampowered.com/api/storesearch/?term={}&l=english&cc=US",
            urlencoding::encode(query.trim())
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
                let aid = item.get("id").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
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
        let url = "https://store.steampowered.com/api/featuredcategories?cc=us&l=english";
        let Ok(resp) = Self::agent()
            .get(url)
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
            appid: val
                .get("appid")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
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
                                id: t
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                                name: t
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                                slug: String::new(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                out.push(DRMFixFixItem {
                    id: f
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
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
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "Server returned an empty download URL.".to_string())?;

        fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
        let resp = Self::agent()
            .get(signed_url)
            .call()
            .map_err(|e| format!("Signed download failed: {}", e))?;

        let cd = resp.header("content-disposition").unwrap_or("");
        let fname =
            filename_from_content_disposition(cd).unwrap_or_else(|| fallback_name.to_string());
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

fn filename_from_content_disposition(cd: &str) -> Option<String> {
    if cd.is_empty() {
        return None;
    }
    let re = Regex::new(r#"(?i)filename\*?=(?:UTF-8'')?"?([^";\r\n]+)"?"#).ok()?;
    let raw = re.captures(cd)?.get(1)?.as_str();
    let decoded = urlencoding::decode(raw).ok()?;
    Path::new(decoded.as_ref())
        .file_name()
        .and_then(|s| s.to_str())
        .map(ToString::to_string)
}

pub fn stream_response_to_file<F>(
    resp: ureq::Response,
    dest: &Path,
    progress_cb: &mut F,
) -> Result<(), String>
where
    F: FnMut(f32, String),
{
    let total = resp
        .header("content-length")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let mut reader = resp.into_reader();
    let mut out = File::create(dest).map_err(|e| e.to_string())?;

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
            let pct = (downloaded as f32) / (total as f32);
            progress_cb(
                pct,
                format!(
                    "Downloading... {} KB / {} KB",
                    downloaded / 1024,
                    total / 1024
                ),
            );
        }
    }
    out.flush().map_err(|e| e.to_string())?;
    Ok(())
}
