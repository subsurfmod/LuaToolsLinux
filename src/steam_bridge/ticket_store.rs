//! Ticket store for the Steam client on Linux.
//!
//! On Windows the integration reads/writes `AppTicket` and `ETicket` from
//! `HKCU\Software\Valve\Steam\Apps\<AppId>`. On Linux, Steam stores the
//! equivalent data in `localconfig.vdf` under the Steam config directory.
//! This module reads/writes those values so ticket-based features work on Linux.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

pub const TICKETS_SIDECAR_FILENAME: &str = "tickets_store.json";

/// Error type for ticket store operations.
#[derive(Debug, thiserror::Error)]
pub enum TicketError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("App {0} not found in ticket store")]
    NotFound(u32),
    #[error("Parse error: {0}")]
    Parse(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppTicketData {
    pub app_id: u32,
    #[serde(default)]
    pub steam_id: Option<String>,
    #[serde(default)]
    pub app_ticket: Option<String>,
    #[serde(default)]
    pub e_ticket: Option<String>,
    #[serde(default)]
    pub last_update: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct TicketStore {
    config_dir: PathBuf,
    cache: BTreeMap<u32, AppTicketData>,
    async_calls: BTreeMap<u64, u32>,
}

impl TicketStore {
    pub fn global() -> &'static Mutex<TicketStore> {
        static GLOBAL_TICKET_STORE: OnceLock<Mutex<TicketStore>> = OnceLock::new();
        GLOBAL_TICKET_STORE.get_or_init(|| Mutex::new(TicketStore::default()))
    }

    pub fn new(config_dir: &Path) -> Self {
        Self {
            config_dir: Self::resolve_config_dir(config_dir),
            cache: BTreeMap::new(),
            async_calls: BTreeMap::new(),
        }
    }

    pub fn new_for_steam_root(steam_root: &Path) -> Self {
        Self::new(steam_root)
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn sidecar_path(&self) -> PathBuf {
        self.config_dir.join(TICKETS_SIDECAR_FILENAME)
    }

    fn active_account_from_loginusers(steam_root: &Path) -> Option<u32> {
        let loginusers = steam_root.join("config").join("loginusers.vdf");
        let content = fs::read_to_string(loginusers).ok()?;
        let mut current_sid: Option<u64> = None;
        let mut best_ts: Option<(u64, u64)> = None;
        for line in content.lines() {
            let trimmed = line.trim();
            if let Some((tok, rest)) = Self::parse_quoted_token(trimmed) {
                let trailing = rest.trim();
                if (trailing.is_empty() || trailing == "{")
                    && !tok.is_empty()
                    && tok.bytes().all(|b| b.is_ascii_digit())
                {
                    if let Ok(sid) = tok.parse::<u64>() {
                        if sid >= 0x0110_0001_0000_0000 {
                            current_sid = Some(sid);
                            continue;
                        }
                    }
                }
            }
            if let (Some(sid), Some((k, v))) = (current_sid, Self::split_kv(trimmed)) {
                if k.eq_ignore_ascii_case("MostRecent") && v.trim() == "1" {
                    return Some((sid & 0xFFFF_FFFF) as u32);
                }
                if k.eq_ignore_ascii_case("Timestamp") {
                    if let Ok(ts) = v.trim().parse::<u64>() {
                        if best_ts.is_none_or(|(b, _)| ts > b) {
                            best_ts = Some((ts, sid));
                        }
                    }
                }
            }
        }
        best_ts.map(|(_, sid)| (sid & 0xFFFF_FFFF) as u32)
    }

    fn select_active_userdata_config(
        steam_root: &Path,
        allow_without_localconfig: bool,
    ) -> Option<PathBuf> {
        let userdata = steam_root.join("userdata");
        let entries = fs::read_dir(&userdata).ok()?;
        let preferred_acc = Self::active_account_from_loginusers(steam_root);

        let mut best_with_vdf: Option<(SystemTime, bool, u32, PathBuf)> = None;
        let mut best_account_dir: Option<(bool, SystemTime, u32, PathBuf)> = None;

        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.is_empty() || name == "0" || !name.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let Ok(acc_id) = name.parse::<u32>() else {
                continue;
            };
            if acc_id == 0 {
                continue;
            }

            let is_preferred = preferred_acc == Some(acc_id);
            let cfg_dir = p.join("config");
            let vdf_path = cfg_dir.join("localconfig.vdf");

            if let Ok(meta) = fs::metadata(&vdf_path) {
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                let key = (mtime, is_preferred, acc_id);
                if best_with_vdf
                    .as_ref()
                    .is_none_or(|(bm, bp, ba, _)| key > (*bm, *bp, *ba))
                {
                    best_with_vdf = Some((mtime, is_preferred, acc_id, cfg_dir));
                }
            } else if allow_without_localconfig {
                let dir_mtime = fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                let key = (is_preferred, dir_mtime, acc_id);
                if best_account_dir
                    .as_ref()
                    .is_none_or(|(bp, bm, ba, _)| key > (*bp, *bm, *ba))
                {
                    best_account_dir = Some((is_preferred, dir_mtime, acc_id, cfg_dir));
                }
            }
        }

        best_with_vdf
            .map(|(_, _, _, p)| p)
            .or_else(|| best_account_dir.map(|(_, _, _, p)| p))
    }

    /// Resolves `<steam_root>` or `<steam_root>/config` to the active user's
    /// `<steam_root>/userdata/<account_id>/config` directory when `userdata/` is present,
    /// while falling back to `config_dir` for isolated unit-test directories.
    pub fn resolve_config_dir(config_dir: &Path) -> PathBuf {
        let is_already_userdata_cfg = config_dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case("config"))
            && config_dir
                .parent()
                .and_then(|acc| {
                    let acc_ok = acc.file_name().and_then(|n| n.to_str()).is_some_and(|s| {
                        !s.is_empty() && s != "0" && s.bytes().all(|b| b.is_ascii_digit())
                    });
                    let ud_ok = acc
                        .parent()
                        .and_then(|u| u.file_name())
                        .and_then(|n| n.to_str())
                        .is_some_and(|u| u.eq_ignore_ascii_case("userdata"));
                    Some(acc_ok && ud_ok)
                })
                .unwrap_or(false);
        if is_already_userdata_cfg {
            return config_dir.to_path_buf();
        }

        let candidate_steam_root = if config_dir.join("userdata").is_dir() {
            Some(config_dir)
        } else if config_dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case("config"))
        {
            config_dir.parent().filter(|p| p.join("userdata").is_dir())
        } else if config_dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case("userdata"))
            && config_dir.is_dir()
        {
            config_dir.parent()
        } else {
            None
        };

        if let Some(root) = candidate_steam_root {
            let allow_without_vdf = !config_dir.join("localconfig.vdf").exists()
                && !config_dir.join(TICKETS_SIDECAR_FILENAME).exists();
            if let Some(resolved) = Self::select_active_userdata_config(root, allow_without_vdf) {
                return resolved;
            }
        }

        config_dir.to_path_buf()
    }

    pub fn default_path() -> PathBuf {
        // Resolve active userdata/<account_id>/config/localconfig.vdf preferring most-recent mtime
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let steam_roots = [
            home.join(".steam/steam"),
            home.join(".local/share/Steam"),
            home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        ];

        let mut best_global: Option<(SystemTime, PathBuf)> = None;
        for root in &steam_roots {
            if let Some(cfg_dir) = Self::select_active_userdata_config(root, false) {
                let mtime = fs::metadata(cfg_dir.join("localconfig.vdf"))
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                if best_global.as_ref().is_none_or(|(bm, _)| mtime > *bm) {
                    best_global = Some((mtime, cfg_dir));
                }
            }
        }
        if let Some((_, best)) = best_global {
            return best;
        }

        for root in &steam_roots {
            if let Some(cfg_dir) = Self::select_active_userdata_config(root, true) {
                return cfg_dir;
            }
        }

        for root in &steam_roots {
            let cfg_dir = root.join("config");
            if cfg_dir.join("localconfig.vdf").exists() {
                return cfg_dir;
            }
        }

        steam_roots[0].join("config")
    }

    pub fn load(&mut self) -> Result<(), TicketError> {
        let vdf_path = self.config_dir.join("localconfig.vdf");
        if vdf_path.exists() {
            self._parse_vdf(&vdf_path)?;
        } else {
            let alt = self.config_dir.join("localconfig.vdf.bak");
            if alt.exists() {
                self._parse_vdf(&alt)?;
            } else {
                self.cache.clear();
            }
        }

        // Merge companion JSON sidecar (`tickets_store.json`) so AppTicket/ETicket/SteamID
        // survive Steam client rewrites that strip unknown VDF keys under `"apps"`.
        let sidecar = self._load_sidecar();
        for (app_id, sc) in sidecar {
            self.cache
                .entry(app_id)
                .and_modify(|existing| {
                    if existing.steam_id.as_deref().unwrap_or("").is_empty() {
                        existing.steam_id = sc.steam_id.clone();
                    }
                    if existing.app_ticket.as_deref().unwrap_or("").is_empty() {
                        existing.app_ticket = sc.app_ticket.clone();
                    }
                    if existing.e_ticket.as_deref().unwrap_or("").is_empty() {
                        existing.e_ticket = sc.e_ticket.clone();
                    }
                    if existing.last_update.is_none() {
                        existing.last_update = sc.last_update;
                    }
                })
                .or_insert(sc);
        }

        Ok(())
    }

    fn is_apps_section_key(trimmed: &str) -> bool {
        trimmed.eq_ignore_ascii_case("\"apps\"")
    }

    /// Count `{` and `}` characters outside quoted strings and `//` line comments.
    pub(crate) fn count_unquoted_braces(line: &str) -> (u32, u32) {
        let mut opens = 0u32;
        let mut closes = 0u32;
        let mut in_quotes = false;
        let mut escaped = false;
        let mut chars = line.chars().peekable();

        while let Some(ch) = chars.next() {
            if in_quotes {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_quotes = false;
                }
                continue;
            }
            match ch {
                '"' => in_quotes = true,
                '/' if chars.peek() == Some(&'/') => break,
                '{' => opens += 1,
                '}' => closes += 1,
                _ => {}
            }
        }
        (opens, closes)
    }

    /// Escape a string value for safe emission inside a VDF double-quoted token.
    pub(crate) fn escape_vdf_value(val: &str) -> String {
        let mut out = String::with_capacity(val.len());
        for ch in val.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => {}
                _ => out.push(ch),
            }
        }
        out
    }

    fn parse_quoted_token(s: &str) -> Option<(String, &str)> {
        let rest = s.strip_prefix('"')?;
        let mut out = String::new();
        let mut escaped = false;
        for (idx, ch) in rest.char_indices() {
            if escaped {
                match ch {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    '\\' => out.push('\\'),
                    '"' => out.push('"'),
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                return Some((out, &rest[idx + 1..]));
            } else {
                out.push(ch);
            }
        }
        None
    }

    fn write_atomic(path: &Path, content: &str) -> Result<(), TicketError> {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("localconfig.vdf");
        let tmp_path = path.with_file_name(format!(".{}.tmp.{}", file_name, std::process::id()));
        fs::write(&tmp_path, content)?;
        fs::rename(&tmp_path, path)?;
        Ok(())
    }

    fn write_atomic_0600(path: &Path, content: &str) -> Result<(), TicketError> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(TICKETS_SIDECAR_FILENAME);
        let tmp_path = path.with_file_name(format!(".{}.tmp.{}", file_name, std::process::id()));
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)?;
        f.write_all(content.as_bytes())?;
        f.flush()?;
        drop(f);
        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600))?;
        fs::rename(&tmp_path, path)?;
        Ok(())
    }

    fn _load_sidecar(&self) -> BTreeMap<u32, AppTicketData> {
        let path = self.sidecar_path();
        let Ok(raw) = fs::read_to_string(&path) else {
            return BTreeMap::new();
        };
        let Ok(mut map) = serde_json::from_str::<BTreeMap<u32, AppTicketData>>(&raw) else {
            return BTreeMap::new();
        };
        for (&app_id, entry) in &mut map {
            entry.app_id = app_id;
        }
        map
    }

    fn _save_sidecar(&self) -> Result<(), TicketError> {
        let persisted: BTreeMap<u32, &AppTicketData> = self
            .cache
            .iter()
            .filter(|(_, d)| {
                d.steam_id.as_deref().is_some_and(|s| !s.is_empty())
                    || d.app_ticket.as_deref().is_some_and(|s| !s.is_empty())
                    || d.e_ticket.as_deref().is_some_and(|s| !s.is_empty())
            })
            .map(|(&k, v)| (k, v))
            .collect();
        let json = serde_json::to_string_pretty(&persisted)
            .map_err(|e| TicketError::Parse(e.to_string()))?;
        Self::write_atomic_0600(&self.sidecar_path(), &json)
    }

    fn _parse_vdf(&mut self, path: &Path) -> Result<(), TicketError> {
        let content = fs::read_to_string(path)?;
        let app_blocks = Self::extract_app_blocks(&content);
        let mut parsed = BTreeMap::new();
        for (appid_str, block) in app_blocks {
            if let Ok(app_id) = appid_str.parse::<u32>() {
                let mut data = AppTicketData {
                    app_id,
                    ..Default::default()
                };
                for line in block.lines() {
                    let line = line.trim();
                    if let Some((key, val)) = Self::split_kv(line) {
                        if val.is_empty() {
                            continue;
                        }
                        match key.as_str() {
                            "SteamID" => data.steam_id = Some(val),
                            "AppTicket" => data.app_ticket = Some(val),
                            "ETicket" => data.e_ticket = Some(val),
                            _ => {}
                        }
                    }
                }
                parsed.insert(app_id, data);
            }
        }

        // Also parse native Steam "apptickets" flat block of "<appid>" "<hex>" (BST-49/BST-53)
        if let Some(sec_idx) = content.find("\"apptickets\"") {
            if let Some(open_rel) = content[sec_idx..].find('{') {
                let open = sec_idx + open_rel + 1;
                let close = content[open..].find('}').map(|c| open + c).unwrap_or(content.len());
                let slice = &content[open..close];
                for line in slice.lines() {
                    let line = line.trim();
                    if let Some((k, v)) = Self::split_kv(line) {
                        if let Ok(app_id) = k.parse::<u32>() {
                            if !v.is_empty() {
                                parsed.entry(app_id).and_modify(|d| {
                                    if d.app_ticket.as_deref().unwrap_or("").is_empty() {
                                        d.app_ticket = Some(v.clone());
                                    }
                                }).or_insert_with(|| AppTicketData {
                                    app_id,
                                    app_ticket: Some(v),
                                    ..Default::default()
                                });
                            }
                        }
                    }
                }
            }
        }

        self.cache = parsed;
        Ok(())
    }

    pub(crate) fn extract_app_blocks(content: &str) -> Vec<(String, String)> {
        let mut blocks = Vec::new();
        let mut in_apps = false;
        let mut seen_apps_open_brace = false;
        let mut depth = 0u32;
        let mut current_appid: Option<String> = None;
        let mut current_block = String::new();

        for line in content.lines() {
            let trimmed = line.trim();

            if !in_apps && Self::is_apps_section_key(trimmed) {
                in_apps = true;
                seen_apps_open_brace = false;
                depth = 0;
                continue;
            }

            if !in_apps {
                continue;
            }

            let prev_depth = depth;
            let (opens, closes) = Self::count_unquoted_braces(trimmed);
            if opens > 0 {
                seen_apps_open_brace = true;
            }
            depth = depth.saturating_add(opens).saturating_sub(closes);

            if prev_depth == 1 && depth == 1 {
                if let Some(appid) = Self::extract_quoted_number(trimmed) {
                    if let Some(prev) = current_appid.replace(appid) {
                        blocks.push((prev, current_block.clone()));
                    }
                    current_block.clear();
                    continue;
                }
            }

            if depth >= 2 && current_appid.is_some() {
                current_block.push_str(trimmed);
                current_block.push('\n');
            } else if prev_depth >= 2 && depth == 1 {
                if let Some(prev) = current_appid.take() {
                    blocks.push((prev, current_block.clone()));
                    current_block.clear();
                }
            }

            if seen_apps_open_brace && depth == 0 {
                if let Some(prev) = current_appid.take() {
                    blocks.push((prev, current_block.clone()));
                    current_block.clear();
                }
                in_apps = false;
                seen_apps_open_brace = false;
            }
        }

        if let Some(aid) = current_appid.take() {
            blocks.push((aid, current_block));
        }

        blocks
    }

    pub(crate) fn extract_quoted_number(line: &str) -> Option<String> {
        let line = line.trim();
        let (inner, rest) = Self::parse_quoted_token(line)?;
        // Must be a section header (no value token after the closing quote).
        if !rest.trim().is_empty() {
            return None;
        }
        if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
            return Some(inner);
        }
        None
    }

    pub(crate) fn split_kv(line: &str) -> Option<(String, String)> {
        let line = line.trim();
        let (key, rest) = Self::parse_quoted_token(line)?;
        if key.is_empty() {
            return None;
        }
        let rest = rest.trim();
        if rest.is_empty() {
            return None;
        }
        let val = if rest.starts_with('"') {
            Self::parse_quoted_token(rest)?.0
        } else {
            rest.split_whitespace().next().unwrap_or("").to_string()
        };
        Some((key, val))
    }

    pub fn get(&mut self, app_id: u32) -> Result<AppTicketData, TicketError> {
        if let Some(data) = self.cache.get(&app_id) {
            return Ok(data.clone());
        }
        self.load()?;
        self.cache
            .get(&app_id)
            .cloned()
            .ok_or(TicketError::NotFound(app_id))
    }

    pub fn set(&mut self, data: AppTicketData) -> Result<(), TicketError> {
        if self.cache.is_empty() {
            let _ = self.load();
        }
        self._persist_app(&data)?;
        self.cache.insert(data.app_id, data);
        self._save_sidecar()?;
        Ok(())
    }

    pub fn remove(&mut self, app_id: u32) -> Result<(), TicketError> {
        if self.cache.is_empty() {
            let _ = self.load();
        }
        self.cache.remove(&app_id);
        self._remove_app_from_vdf(app_id)?;
        if self.sidecar_path().exists() || !self.cache.is_empty() {
            self._save_sidecar()?;
        }
        Ok(())
    }

    pub fn has_app_ticket(&mut self, app_id: u32) -> bool {
        self.get(app_id)
            .map(|d| d.app_ticket.is_some())
            .unwrap_or(false)
    }

    pub fn has_e_ticket(&mut self, app_id: u32) -> bool {
        self.get(app_id)
            .map(|d| d.e_ticket.is_some())
            .unwrap_or(false)
    }

    fn _persist_app(&self, data: &AppTicketData) -> Result<(), TicketError> {
        fs::create_dir_all(&self.config_dir)?;
        let vdf_path = self.config_dir.join("localconfig.vdf");
        let content = if vdf_path.exists() {
            fs::read_to_string(&vdf_path)?
        } else {
            String::from("\"UserLocalConfigStore\"\n{\n    \"Software\"\n    {\n        \"Valve\"\n        {\n            \"Steam\"\n            {\n                \"apps\"\n                {\n                }\n            }\n        }\n    }\n}\n")
        };
        let new_content = Self::upsert_app_block(&content, data.app_id, data);
        Self::write_atomic(&vdf_path, &new_content)
    }

    fn _remove_app_from_vdf(&self, app_id: u32) -> Result<(), TicketError> {
        let vdf_path = self.config_dir.join("localconfig.vdf");
        if !vdf_path.exists() {
            return Ok(());
        }
        let content = fs::read_to_string(&vdf_path)?;
        let new_content = Self::remove_app_block(&content, app_id);
        Self::write_atomic(&vdf_path, &new_content)
    }

    fn format_new_app_block(app_id: u32, data: &AppTicketData) -> String {
        let appid_str = app_id.to_string();
        let steam_id = Self::escape_vdf_value(data.steam_id.as_deref().unwrap_or(""));
        let app_ticket = Self::escape_vdf_value(data.app_ticket.as_deref().unwrap_or(""));
        let e_ticket = Self::escape_vdf_value(data.e_ticket.as_deref().unwrap_or(""));
        format!(
            "        \"{}\"\n        {{\n            \"appid\" \"{}\"\n            \"SteamID\" \"{}\"\n            \"AppTicket\" \"{}\"\n            \"ETicket\" \"{}\"\n        }}\n",
            appid_str, appid_str, steam_id, app_ticket, e_ticket
        )
    }

    fn upsert_app_block(content: &str, app_id: u32, data: &AppTicketData) -> String {
        let appid_str = app_id.to_string();
        let steam_id = Self::escape_vdf_value(data.steam_id.as_deref().unwrap_or(""));
        let app_ticket = Self::escape_vdf_value(data.app_ticket.as_deref().unwrap_or(""));
        let e_ticket = Self::escape_vdf_value(data.e_ticket.as_deref().unwrap_or(""));

        let lines: Vec<&str> = content.lines().collect();
        let mut result = String::with_capacity(content.len() + 256);
        let mut in_apps = false;
        let mut seen_apps_open_brace = false;
        let mut depth = 0u32;
        let mut inserted = false;
        let mut in_target_app = false;
        let mut saw_appid = false;
        let mut saw_steam_id = false;
        let mut saw_app_ticket = false;
        let mut saw_e_ticket = false;

        for line in &lines {
            let trimmed = line.trim();

            if !in_apps && !inserted && Self::is_apps_section_key(trimmed) {
                in_apps = true;
                seen_apps_open_brace = false;
                depth = 0;
                result.push_str(line);
                result.push('\n');
                continue;
            }

            if !in_apps {
                result.push_str(line);
                result.push('\n');
                continue;
            }

            let prev_depth = depth;
            let (opens, closes) = Self::count_unquoted_braces(trimmed);
            if opens > 0 {
                seen_apps_open_brace = true;
            }
            depth = depth.saturating_add(opens).saturating_sub(closes);

            if in_target_app {
                // Closing brace of the target app block: emit any missing ticket keys before `}`
                if prev_depth >= 2 && depth == 1 {
                    if !saw_appid {
                        result.push_str(&format!("            \"appid\" \"{}\"\n", appid_str));
                    }
                    if !saw_steam_id {
                        result.push_str(&format!("            \"SteamID\" \"{}\"\n", steam_id));
                    }
                    if !saw_app_ticket {
                        result.push_str(&format!("            \"AppTicket\" \"{}\"\n", app_ticket));
                    }
                    if !saw_e_ticket {
                        result.push_str(&format!("            \"ETicket\" \"{}\"\n", e_ticket));
                    }
                    result.push_str(line);
                    result.push('\n');
                    in_target_app = false;
                    continue;
                }

                // Direct key-value lines inside the target app block (depth == 2):
                // update ticket keys in place and preserve all other Steam fields & sub-blocks.
                if prev_depth == 2 && depth == 2 {
                    if let Some((key, _)) = Self::split_kv(trimmed) {
                        let indent_len = line.len() - line.trim_start().len();
                        let indent = &line[..indent_len];
                        match key.as_str() {
                            "appid" => {
                                saw_appid = true;
                                result
                                    .push_str(&format!("{}\"appid\" \"{}\"\n", indent, appid_str));
                                continue;
                            }
                            "SteamID" => {
                                saw_steam_id = true;
                                result
                                    .push_str(&format!("{}\"SteamID\" \"{}\"\n", indent, steam_id));
                                continue;
                            }
                            "AppTicket" => {
                                saw_app_ticket = true;
                                result.push_str(&format!(
                                    "{}\"AppTicket\" \"{}\"\n",
                                    indent, app_ticket
                                ));
                                continue;
                            }
                            "ETicket" => {
                                saw_e_ticket = true;
                                result
                                    .push_str(&format!("{}\"ETicket\" \"{}\"\n", indent, e_ticket));
                                continue;
                            }
                            _ => {}
                        }
                    }
                }

                result.push_str(line);
                result.push('\n');
                continue;
            }

            if prev_depth == 1 && depth == 1 {
                if let Some(aid) = Self::extract_quoted_number(trimmed) {
                    if aid == appid_str {
                        in_target_app = true;
                        inserted = true;
                        saw_appid = false;
                        saw_steam_id = false;
                        saw_app_ticket = false;
                        saw_e_ticket = false;
                        result.push_str(line);
                        result.push('\n');
                        continue;
                    }
                }
            }

            // Closing brace of `"apps"` — insert new app block BEFORE writing `}`
            if seen_apps_open_brace && depth == 0 {
                if !inserted {
                    result.push_str(&Self::format_new_app_block(app_id, data));
                    inserted = true;
                }
                result.push_str(line);
                result.push('\n');
                in_apps = false;
                seen_apps_open_brace = false;
                continue;
            }

            result.push_str(line);
            result.push('\n');
        }

        if !inserted {
            result.push_str("\"apps\"\n{\n");
            result.push_str(&Self::format_new_app_block(app_id, data));
            result.push_str("}\n");
        }

        result
    }

    fn remove_app_block(content: &str, app_id: u32) -> String {
        let appid_str = app_id.to_string();
        let lines: Vec<&str> = content.lines().collect();
        let mut result = String::with_capacity(content.len());
        let mut in_apps = false;
        let mut seen_apps_open_brace = false;
        let mut depth = 0u32;
        let mut skipping_target = false;

        for line in &lines {
            let trimmed = line.trim();

            if !in_apps && Self::is_apps_section_key(trimmed) {
                in_apps = true;
                seen_apps_open_brace = false;
                depth = 0;
                result.push_str(line);
                result.push('\n');
                continue;
            }

            if !in_apps {
                result.push_str(line);
                result.push('\n');
                continue;
            }

            let prev_depth = depth;
            let (opens, closes) = Self::count_unquoted_braces(trimmed);
            if opens > 0 {
                seen_apps_open_brace = true;
            }
            depth = depth.saturating_add(opens).saturating_sub(closes);

            if skipping_target {
                if prev_depth >= 2 && depth == 1 {
                    skipping_target = false;
                }
                continue;
            }

            if prev_depth == 1 && depth == 1 {
                if let Some(aid) = Self::extract_quoted_number(trimmed) {
                    if aid == appid_str {
                        skipping_target = true;
                        continue;
                    }
                }
            }

            result.push_str(line);
            result.push('\n');

            if seen_apps_open_brace && depth == 0 {
                in_apps = false;
                seen_apps_open_brace = false;
            }
        }

        result
    }

    pub fn list_app_ids(&mut self) -> Result<Vec<u32>, TicketError> {
        self.load()?;
        Ok(self.cache.keys().copied().collect())
    }

    /// Record an async call handle for encrypted ticket tracking.
    pub fn record_async_call(&mut self, async_call: u64, app_id: u32) {
        self.async_calls.insert(async_call, app_id);
    }

    /// Consume an async call handle and return the associated app_id.
    pub fn take_async_call(&mut self, async_call: u64) -> Option<u32> {
        self.async_calls.remove(&async_call)
    }

    /// Get encrypted ticket (ETicket) bytes for an app.
    pub fn get_encrypted_ticket(&mut self, app_id: u32) -> Option<Vec<u8>> {
        self.load().ok()?;
        self.cache
            .get(&app_id)
            .and_then(|d| d.e_ticket.as_ref())
            .and_then(|s| hex::decode(s).ok())
    }

    /// Get spoofed SteamID for an app (returns 0 when unset or invalid).
    pub fn get_spoof_steam_id(&mut self, app_id: u32) -> u64 {
        if self.load().is_err() {
            return 0;
        }
        if let Some(d) = self.cache.get(&app_id) {
            if let Some(sid) = d.steam_id.as_ref().and_then(|s| s.parse::<u64>().ok()) {
                if sid >= 0x0110_0001_0000_0000 {
                    return sid;
                }
            }
            if let Some(ticket_hex) = d.app_ticket.as_ref() {
                if let Ok(bytes) = hex::decode(ticket_hex) {
                    let sid = crate::steam_bridge::credentials::appticket::extract_steam_id_from_ticket_bytes(&bytes);
                    if sid != 0 {
                        return sid;
                    }
                }
            }
        }
        0
    }

    /// Set spoofed SteamID for an app (BST-55).
    pub fn set_spoof_steam_id(&mut self, app_id: u32, sid: u64) {
        if self.cache.is_empty() {
            let _ = self.load();
        }
        let entry = self.cache.entry(app_id).or_insert_with(|| AppTicketData {
            app_id,
            steam_id: None,
            app_ticket: None,
            e_ticket: None,
            last_update: None,
        });
        entry.steam_id = Some(sid.to_string());
        let _ = self._save_sidecar();
    }
}

impl Default for TicketStore {
    fn default() -> Self {
        Self::new(&Self::default_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn extract_app_blocks_parses_simple_vdf() {
        let vdf = r#""apps"
        {
            "1245620"
            {
                "appid" "1245620"
                "SteamID" "76561198028121353"
                "AppTicket" "deadbeef"
                "ETicket" "cafebabe"
            }
            "2778580"
            {
                "appid" "2778580"
                "AppTicket" "0000"
            }
        }"#;
        let blocks = TicketStore::extract_app_blocks(vdf);
        assert_eq!(blocks.len(), 2);
        let map: BTreeMap<String, String> = blocks.into_iter().collect();
        assert!(map.get("1245620").unwrap().contains("76561198028121353"));
        assert!(map.get("1245620").unwrap().contains("deadbeef"));
        assert!(map.get("2778580").unwrap().contains("0000"));
    }

    #[test]
    fn split_kv_extracts_real_quoted_values() {
        assert_eq!(
            TicketStore::split_kv(r#""SteamID" "76561198028121353""#),
            Some(("SteamID".into(), "76561198028121353".into()))
        );
        assert_eq!(
            TicketStore::split_kv("\"AppTicket\"\t\t\"deadbeef\""),
            Some(("AppTicket".into(), "deadbeef".into()))
        );
        assert_eq!(
            TicketStore::split_kv("\"ETicket\" \"\""),
            Some(("ETicket".into(), "".into()))
        );
    }

    #[test]
    fn set_update_load_and_remove_roundtrip_preserves_vdf_structure() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = TicketStore::new(tmp.path());

        store
            .set(AppTicketData {
                app_id: 1245620,
                steam_id: Some("76561198028121353".into()),
                app_ticket: Some("deadbeef".into()),
                e_ticket: Some("cafebabe".into()),
                last_update: None,
            })
            .unwrap();

        store
            .set(AppTicketData {
                app_id: 2778580,
                steam_id: Some("76561198028121353".into()),
                app_ticket: Some("11223344".into()),
                e_ticket: None,
                last_update: None,
            })
            .unwrap();

        // Update existing app 1245620 — must NOT delete it!
        store
            .set(AppTicketData {
                app_id: 1245620,
                steam_id: Some("76561198028121353".into()),
                app_ticket: Some("feedface".into()),
                e_ticket: Some("01020304".into()),
                last_update: None,
            })
            .unwrap();

        let mut reloaded = TicketStore::new(tmp.path());
        reloaded.load().unwrap();
        let d1 = reloaded.get(1245620).unwrap();
        assert_eq!(d1.steam_id.as_deref(), Some("76561198028121353"));
        assert_eq!(d1.app_ticket.as_deref(), Some("feedface"));
        assert_eq!(d1.e_ticket.as_deref(), Some("01020304"));

        let d2 = reloaded.get(2778580).unwrap();
        assert_eq!(d2.app_ticket.as_deref(), Some("11223344"));

        reloaded.remove(1245620).unwrap();
        let mut reloaded2 = TicketStore::new(tmp.path());
        reloaded2.load().unwrap();
        assert!(reloaded2.get(1245620).is_err());
        assert_eq!(
            reloaded2.get(2778580).unwrap().app_ticket.as_deref(),
            Some("11223344")
        );
    }

    #[test]
    fn ignores_non_apps_keys_containing_apps_substring() {
        let vdf = r#""UserLocalConfigStore"
{
    "steamapps" "C:/Steam/steamapps"
    "apps"
    {
        "480"
        {
            "SteamID" "76561198000000001"
        }
    }
}
"#;
        let blocks = TicketStore::extract_app_blocks(vdf);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].0, "480");
    }

    #[test]
    fn upsert_preserves_existing_steam_fields_nested_blocks_and_quoted_braces() {
        let tmp = tempfile::tempdir().unwrap();
        let vdf = r#""UserLocalConfigStore"
{
    "Software"
    {
        "Valve"
        {
            "Steam"
            {
                "apps"
                {
                    "1245620"
                    {
                        "LaunchOptions" "gamemoderun %command% --cfg={\"a\":1}"
                        "Playtime" "3600"
                        "LegacyCDKey" "ABCD-EFGH-IJKL-MNOP"
                        "cloud"
                        {
                            "last_sync_state" "synchronized"
                        }
                    }
                }
            }
        }
    }
}
"#;
        fs::write(tmp.path().join("localconfig.vdf"), vdf).unwrap();

        let mut store = TicketStore::new(tmp.path());
        store
            .set(AppTicketData {
                app_id: 1245620,
                steam_id: Some("76561198028121353".into()),
                app_ticket: Some("deadbeef".into()),
                e_ticket: Some("cafebabe".into()),
                last_update: None,
            })
            .unwrap();

        let written = fs::read_to_string(tmp.path().join("localconfig.vdf")).unwrap();
        assert!(written.contains("\"LaunchOptions\" \"gamemoderun %command% --cfg={\\\"a\\\":1}\""));
        assert!(written.contains("\"Playtime\" \"3600\""));
        assert!(written.contains("\"LegacyCDKey\" \"ABCD-EFGH-IJKL-MNOP\""));
        assert!(written.contains("\"last_sync_state\" \"synchronized\""));

        let mut reloaded = TicketStore::new(tmp.path());
        let data = reloaded.get(1245620).unwrap();
        assert_eq!(data.steam_id.as_deref(), Some("76561198028121353"));
        assert_eq!(data.app_ticket.as_deref(), Some("deadbeef"));
        assert_eq!(data.e_ticket.as_deref(), Some("cafebabe"));
    }

    #[test]
    fn extract_quoted_number_ignores_kv_lines_and_handles_escaped_quotes() {
        assert_eq!(
            TicketStore::extract_quoted_number("\"1245620\""),
            Some("1245620".into())
        );
        assert_eq!(
            TicketStore::extract_quoted_number("\"1245620\" \"not_a_section\""),
            None
        );
        assert_eq!(
            TicketStore::split_kv("\"LaunchOptions\" \"echo \\\"hello\\\"\""),
            Some(("LaunchOptions".into(), "echo \"hello\"".into()))
        );
    }

    #[test]
    fn resolves_steam_root_and_steam_config_to_active_userdata_config() {
        let tmp = tempfile::tempdir().unwrap();
        let steam_root = tmp.path().join("Steam");
        let root_cfg = steam_root.join("config");
        let older_user_cfg = steam_root.join("userdata/111111/config");
        let newer_user_cfg = steam_root.join("userdata/222222/config");
        fs::create_dir_all(&root_cfg).unwrap();
        fs::create_dir_all(&older_user_cfg).unwrap();
        fs::create_dir_all(&newer_user_cfg).unwrap();

        fs::write(
            older_user_cfg.join("localconfig.vdf"),
            "\"UserLocalConfigStore\"\n{\n    \"apps\"\n    {\n    }\n}\n",
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        fs::write(
            newer_user_cfg.join("localconfig.vdf"),
            "\"UserLocalConfigStore\"\n{\n    \"apps\"\n    {\n    }\n}\n",
        )
        .unwrap();

        // Both <steam_root>/config and <steam_root> must resolve to userdata/222222/config
        let mut store_from_cfg = TicketStore::new(&root_cfg);
        assert_eq!(store_from_cfg.config_dir(), newer_user_cfg.as_path());

        let store_from_root = TicketStore::new_for_steam_root(&steam_root);
        assert_eq!(store_from_root.config_dir(), newer_user_cfg.as_path());

        store_from_cfg
            .set(AppTicketData {
                app_id: 480,
                steam_id: Some("76561198000222222".into()),
                app_ticket: Some("aabbccdd".into()),
                e_ticket: Some("11223344".into()),
                last_update: Some(1_700_000_000),
            })
            .unwrap();
        assert!(!root_cfg.join("localconfig.vdf").exists());
        assert!(newer_user_cfg.join(TICKETS_SIDECAR_FILENAME).is_file());
    }

    #[test]
    fn sidecar_0600_survives_steam_rewriting_localconfig_vdf() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = TicketStore::new(tmp.path());
        store
            .set(AppTicketData {
                app_id: 1245620,
                steam_id: Some("76561198028121353".into()),
                app_ticket: Some("deadbeef".into()),
                e_ticket: Some("cafebabe".into()),
                last_update: Some(1_700_000_123),
            })
            .unwrap();

        let sidecar = store.sidecar_path();
        assert!(sidecar.is_file());
        let mode = fs::metadata(&sidecar).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        // Simulate Steam client rewriting localconfig.vdf and stripping AppTicket/ETicket/SteamID
        let rewritten_by_steam = r#""UserLocalConfigStore"
{
    "Software"
    {
        "Valve"
        {
            "Steam"
            {
                "apps"
                {
                    "1245620"
                    {
                        "Playtime" "999"
                    }
                }
            }
        }
    }
}
"#;
        fs::write(tmp.path().join("localconfig.vdf"), rewritten_by_steam).unwrap();

        let mut reloaded = TicketStore::new(tmp.path());
        let recovered = reloaded.get(1245620).unwrap();
        assert_eq!(recovered.steam_id.as_deref(), Some("76561198028121353"));
        assert_eq!(recovered.app_ticket.as_deref(), Some("deadbeef"));
        assert_eq!(recovered.e_ticket.as_deref(), Some("cafebabe"));
        assert_eq!(recovered.last_update, Some(1_700_000_123));

        reloaded.remove(1245620).unwrap();
        let mut after_rm = TicketStore::new(tmp.path());
        assert!(after_rm.get(1245620).is_err());
    }
}
