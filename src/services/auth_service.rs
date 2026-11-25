//! Authentication service — Discord PKCE OAuth (`:53789`) & 6-character Discord bot code sign-in.
//! Compatible with `~/.local/share/luatools/auth.json` (encrypted as auth.json.enc).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tiny_http::{Header, Response, Server};
use url::Url;

use crate::models::config::default_luatools_data_dir;
use crate::services::credential_encryption::{EncryptionService, migrate_auth_to_encrypted};

pub const SUPABASE_URL: &str = "https://db.lua.tools";
pub const SUPABASE_ANON_KEY: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.\
eyJpYXQiOjE3NzYwMzkzNzYsImV4cCI6MTg5MzQ1NjAwMCwicm9sZSI6ImFub24iLCJpc3MiOiJzdXBhYmFzZSJ9.\
f_-K38u3odjltP-g_67FVmG32Vg-_-k-lNBvIaVUVBM";
pub const API_BASE_URL: &str = "https://lua.tools";
pub const OAUTH_CALLBACK_PORT: u16 = 53789;
pub const OAUTH_CALLBACK_URL: &str = "http://localhost:53789/callback";
pub const BOT_ACCOUNT_DOMAIN: &str = "@bot.lua.tools";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthSession {
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub expires_at: f64,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub avatar_url: String,
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[derive(Debug, Clone)]
pub struct AuthService {
    pub auth_file: PathBuf,
    session: Arc<Mutex<Option<AuthSession>>>,
}

impl Default for AuthService {
    fn default() -> Self {
        Self::new(None)
    }
}

impl AuthService {
    pub fn new(auth_file: Option<PathBuf>) -> Self {
        let path = auth_file.unwrap_or_else(|| default_luatools_data_dir().join("auth.json"));
        Self {
            auth_file: path,
            session: Arc::new(Mutex::new(None)),
        }
    }

    /// Ensures encryption service is initialized and attempts to migrate plaintext auth.json
    fn ensure_encryption(&self) {
        let _ = EncryptionService::init_global();
        let _ = migrate_auth_to_encrypted();
    }

    pub fn initialize(&self) -> bool {
        self.ensure_encryption();
        let Some(stored) = self.load_stored() else {
            return false;
        };
        if stored.refresh_token.is_empty() {
            return false;
        }
        *self.session.lock().unwrap() = Some(stored.clone());
        if stored.expires_at > now_secs() + 120.0 {
            return true;
        }
        if self.refresh().is_ok() {
            true
        } else {
            self.sign_out();
            false
        }
    }

    pub fn snapshot(&self) -> Option<AuthSession> {
        self.session.lock().unwrap().clone()
    }

    pub fn is_signed_in(&self) -> bool {
        self.session
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| !s.refresh_token.is_empty())
            .unwrap_or(false)
    }

    pub fn display_name(&self) -> String {
        self.session
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.display_name.clone())
            .unwrap_or_default()
    }

    pub fn email(&self) -> String {
        self.session
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.email.clone())
            .unwrap_or_default()
    }

    pub fn get_valid_access_token(&self) -> Result<String, String> {
        let needs_refresh = {
            let guard = self.session.lock().unwrap();
            let Some(sess) = guard.as_ref() else {
                return Err(
                    "Not signed in. Sign in from Settings › Account & Providers first.".into(),
                );
            };
            if sess.refresh_token.is_empty() {
                return Err("Not signed in.".into());
            }
            if sess.expires_at > now_secs() + 120.0 {
                return Ok(sess.access_token.clone());
            }
            true
        };

        if needs_refresh {
            let refreshed = self.refresh()?;
            return Ok(refreshed.access_token);
        }
        Err("Unable to resolve access token.".into())
    }

    pub fn sign_out(&self) {
        *self.session.lock().unwrap() = None;
        let _ = fs::remove_file(&self.auth_file);
        let _ = fs::remove_file(self.auth_file.with_extension("enc"));
    }

    pub fn sign_in_with_browser(&self, timeout_secs: u64) -> Result<AuthSession, String> {
        let mut raw_bytes = [0u8; 48];
        rand::thread_rng().fill_bytes(&mut raw_bytes);
        let verifier = URL_SAFE_NO_PAD.encode(raw_bytes);

        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());

        let auth_url = format!(
            "{}/auth/v1/authorize?provider=discord&redirect_to={}&code_challenge={}&code_challenge_method=s256",
            SUPABASE_URL,
            urlencoding::encode(OAUTH_CALLBACK_URL),
            challenge
        );

        let server = Server::http(("127.0.0.1", OAUTH_CALLBACK_PORT)).map_err(|e| {
            format!(
                "Failed to bind OAuth callback port {}: {}",
                OAUTH_CALLBACK_PORT, e
            )
        })?;

        let _ = Command::new("xdg-open").arg(&auth_url).spawn();

        let deadline = SystemTime::now() + Duration::from_secs(timeout_secs);
        let mut auth_code: Option<String> = None;
        let mut auth_err: Option<String> = None;

        while SystemTime::now() < deadline {
            if let Ok(Some(req)) = server.recv_timeout(Duration::from_millis(400)) {
                let full_url = format!("http://localhost{}", req.url());
                if let Ok(parsed) = Url::parse(&full_url) {
                    let mut code_q = None;
                    let mut err_q = None;
                    for (k, v) in parsed.query_pairs() {
                        if k == "code" {
                            code_q = Some(v.to_string());
                        } else if k == "error_description" || k == "error" {
                            err_q = Some(v.to_string());
                        }
                    }

                    if code_q.is_none() && err_q.is_none() {
                        let _ =
                            req.respond(Response::from_string("Not Found").with_status_code(404));
                        continue;
                    }

                    let ok = code_q.is_some();
                    let html = Self::callback_html(ok, err_q.as_deref());
                    let header =
                        Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap();
                    let _ = req.respond(Response::from_string(html).with_header(header));

                    auth_code = code_q;
                    auth_err = err_q;
                    break;
                }
            }
        }

        if let Some(e) = auth_err {
            return Err(e);
        }
        let Some(code) = auth_code else {
            return Err("Timed out waiting for Discord browser callback.".into());
        };

        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .build();
        let resp = agent
            .post(&format!("{}/auth/v1/token?grant_type=pkce", SUPABASE_URL))
            .set("apikey", SUPABASE_ANON_KEY)
            .send_json(json!({
                "auth_code": code,
                "code_verifier": verifier,
            }))
            .map_err(|e| format!("Token exchange failed: {}", e))?;

        let data: serde_json::Value = resp
            .into_json()
            .map_err(|e| format!("Invalid session JSON: {}", e))?;
        Ok(self.apply_supabase_session(&data))
    }

    pub fn sign_in_with_code(&self, code: &str) -> Result<AuthSession, String> {
        let cleaned = code.trim().to_ascii_uppercase();
        if cleaned.is_empty() {
            return Err("Please enter a 6-character Discord bot code.".into());
        }

        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .build();

        let redeem_resp = agent
            .post(&format!("{}/api/auth/code/redeem", API_BASE_URL))
            .send_json(json!({ "code": cleaned }));

        let redeem_val: serde_json::Value = match redeem_resp {
            Ok(r) => r.into_json().map_err(|e| e.to_string())?,
            Err(ureq::Error::Status(410, _)) => {
                return Err("Code has expired. Generate a new one in Discord.".into())
            }
            Err(ureq::Error::Status(400 | 404, _)) => return Err("Invalid Discord code.".into()),
            Err(e) => return Err(format!("Code redemption failed: {}", e)),
        };

        let token_hash = redeem_val
            .get("token")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "Server returned an empty magic-link token.".to_string())?;

        let verify_resp = agent
            .post(&format!("{}/auth/v1/verify", SUPABASE_URL))
            .set("apikey", SUPABASE_ANON_KEY)
            .send_json(json!({
                "type": "magiclink",
                "token_hash": token_hash,
            }))
            .map_err(|e| format!("Token verification failed: {}", e))?;

        let session_val: serde_json::Value = verify_resp.into_json().map_err(|e| e.to_string())?;
        Ok(self.apply_supabase_session(&session_val))
    }

    fn refresh(&self) -> Result<AuthSession, String> {
        let refresh_tok = {
            let guard = self.session.lock().unwrap();
            guard
                .as_ref()
                .map(|s| s.refresh_token.clone())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "No refresh token.".to_string())?
        };

        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .build();
        let resp = agent
            .post(&format!(
                "{}/auth/v1/token?grant_type=refresh_token",
                SUPABASE_URL
            ))
            .set("apikey", SUPABASE_ANON_KEY)
            .send_json(json!({ "refresh_token": refresh_tok }))
            .map_err(|e| format!("Session refresh failed: {}", e))?;

        let data: serde_json::Value = resp.into_json().map_err(|e| e.to_string())?;
        Ok(self.apply_supabase_session(&data))
    }

    fn apply_supabase_session(&self, data: &serde_json::Value) -> AuthSession {
        let user = data.get("user").cloned().unwrap_or_default();
        let meta = user.get("user_metadata").cloned().unwrap_or_default();
        let custom = meta.get("custom_claims").cloned().unwrap_or_default();

        let display_name = custom
            .get("global_name")
            .and_then(|v| v.as_str())
            .or_else(|| meta.get("full_name").and_then(|v| v.as_str()))
            .or_else(|| meta.get("name").and_then(|v| v.as_str()))
            .or_else(|| user.get("email").and_then(|v| v.as_str()))
            .unwrap_or("LuaTools User")
            .to_string();

        let expires_in = data
            .get("expires_in")
            .and_then(|v| v.as_f64())
            .unwrap_or(3600.0);

        let session = AuthSession {
            access_token: data
                .get("access_token")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            refresh_token: data
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            expires_at: now_secs() + expires_in,
            display_name,
            email: user
                .get("email")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            avatar_url: meta
                .get("avatar_url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        };

        *self.session.lock().unwrap() = Some(session.clone());
        self.save_stored(&session);
        session
    }

    fn load_stored(&self) -> Option<AuthSession> {
        // Try encrypted file first
        let encrypted_path = self.auth_file.with_extension("enc");
        if encrypted_path.exists() {
            let global = EncryptionService::global();
            let guard = global.lock().unwrap();
            if let Some(service) = guard.as_ref() {
                if let Ok(payload_str) = fs::read_to_string(&encrypted_path) {
                    if let Ok(payload) = serde_json::from_str::<crate::services::credential_encryption::EncryptedPayload>(&payload_str) {
                        if let Ok(session) = service.decrypt(&payload) {
                            return Some(session);
                        }
                    }
                }
            }
        }

        // Fallback to plaintext (for migration)
        let raw = fs::read_to_string(&self.auth_file).ok()?;
        serde_json::from_str::<AuthSession>(&raw).ok()
    }

    fn save_stored(&self, session: &AuthSession) {
        // Try to use encryption
        let global = EncryptionService::global();
        let guard = global.lock().unwrap();
        if let Some(service) = guard.as_ref() {
            if let Ok(payload) = service.encrypt(session) {
                let encrypted_path = self.auth_file.with_extension("enc");
                if let Some(parent) = encrypted_path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if let Ok(json) = serde_json::to_string_pretty(&payload) {
                    let tmp = encrypted_path.with_extension("tmp");
                    if fs::write(&tmp, json).is_ok() {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
                        }
                        let _ = fs::rename(&tmp, &encrypted_path);
                        // Remove plaintext if it exists
                        let _ = fs::remove_file(&self.auth_file);
                        return;
                    }
                }
            }
        }

        // Fallback to plaintext (should not happen if encryption is initialized)
        if let Some(parent) = self.auth_file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let Ok(json) = serde_json::to_string_pretty(session) else {
            return;
        };
        let tmp = self.auth_file.with_extension("tmp");
        if fs::write(&tmp, json).is_ok() {
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
            let _ = fs::rename(&tmp, &self.auth_file);
        }
    }

    fn callback_html(ok: bool, err: Option<&str>) -> String {
        let title = if ok {
            "AUTHENTICATED"
        } else {
            "AUTHENTICATION FAILED"
        };
        let accent = if ok { "#3B82F6" } else { "#EF4444" };
        let raw_msg = if ok {
            "You can close this tab and return to LuaTools Linux."
        } else {
            err.unwrap_or("Please try signing in again from LuaTools Linux.")
        };
        let msg = raw_msg
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;");
        format!(
            "<!doctype html><html><head><meta charset='utf-8'><title>LuaTools Linux</title>\
             <style>body{{background:#09090B;color:#F4F4F6;font-family:Inter,sans-serif;\
             display:flex;align-items:center;justify-content:center;height:100vh;margin:0;}}\
             .card{{padding:2.5rem 3rem;background:#141418;border:1px solid #2B2B34;\
             border-left:4px solid {accent};border-radius:0;}}\
             h1{{color:{accent};font-size:1.2rem;letter-spacing:0.08em;margin:0 0 .6rem;}}\
             p{{color:#A1A1AA;margin:0;}}</style></head>\
             <body><div class='card'><h1>{title}</h1><p>{msg}</p></div></body></html>"
        )
    }
}
