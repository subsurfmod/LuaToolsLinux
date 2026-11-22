//! LuaVault — content-addressed SHA-256 storage for per-game `.lua` variants.
//! 100% disk-compatible with LuaTools Linux v1.x `~/.local/share/luatools/luavault/<appid>/index.json`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::models::config::default_luatools_data_dir;
use crate::models::steam::SteamInstallation;
use crate::parsers::lua_parser::LuaFileParser;

pub const KIND_DEFAULT: &str = "default";
pub const KIND_BUILD: &str = "build";
pub const KIND_PRESET: &str = "preset";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LuaVariant {
    pub hash: String,
    pub kind: String,
    #[serde(default)]
    pub build_id: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default = "now_iso")]
    pub captured_at: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub manifest_ids: BTreeMap<String, String>,
    #[serde(default)]
    pub depot_count: usize,
    #[serde(default)]
    pub dlc_count: usize,
}

fn now_iso() -> String {
    Utc::now()
        .naive_utc()
        .format("%Y-%m-%dT%H:%M:%S%.6f")
        .to_string()
}

pub fn parse_timestamp_key(ts: &str) -> i64 {
    if let Ok(dt) = DateTime::parse_from_rfc3339(ts) {
        return dt.timestamp_micros();
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
    ] {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(ts, fmt) {
            return ndt.and_utc().timestamp_micros();
        }
    }
    0
}

impl LuaVariant {
    pub fn is_pinned(&self) -> bool {
        !self.manifest_ids.is_empty()
    }

    pub fn display_label(&self) -> String {
        if let Some(lbl) = &self.label {
            if !lbl.trim().is_empty() {
                return lbl.trim().to_string();
            }
        }
        if let Some(bid) = &self.build_id {
            if !bid.is_empty() {
                return format!("Build {}", bid);
            }
        }
        if self.kind == KIND_DEFAULT {
            return "Default".to_string();
        }
        let short_date = self
            .captured_at
            .get(0..16)
            .unwrap_or(&self.captured_at)
            .replace('T', " ");
        format!("Preset ({})", short_date)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VaultIndex {
    #[serde(default)]
    pub app_id: u32,
    #[serde(default)]
    pub variants: Vec<LuaVariant>,
    #[serde(default)]
    pub edit_base_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LuaVault {
    pub steam_installation: Option<SteamInstallation>,
    pub vault_root: PathBuf,
}

impl LuaVault {
    pub fn new(steam_installation: Option<SteamInstallation>, vault_root: Option<PathBuf>) -> Self {
        let root = vault_root.unwrap_or_else(|| default_luatools_data_dir().join("luavault"));
        let _ = fs::create_dir_all(&root);
        Self {
            steam_installation,
            vault_root: root,
        }
    }

    fn app_dir(&self, app_id: u32) -> PathBuf {
        self.vault_root.join(app_id.to_string())
    }

    fn index_path(&self, app_id: u32) -> PathBuf {
        self.app_dir(app_id).join("index.json")
    }

    fn variant_path(&self, app_id: u32, hash_str: &str) -> PathBuf {
        self.app_dir(app_id).join(format!("{}.lua", hash_str))
    }

    pub fn live_path(&self, app_id: u32) -> Option<PathBuf> {
        let inst = self.steam_installation.as_ref()?;
        Some(inst.stplug_in_path.join(format!("{}.lua", app_id)))
    }

    pub fn hash_bytes(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    }

    pub fn hash_file(path: &Path) -> Option<String> {
        let bytes = fs::read(path).ok()?;
        Some(Self::hash_bytes(&bytes))
    }

    fn load_index(&self, app_id: u32) -> VaultIndex {
        let path = self.index_path(app_id);
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(mut idx) = serde_json::from_str::<VaultIndex>(&raw) {
                if idx.app_id == 0 {
                    idx.app_id = app_id;
                }
                return idx;
            }
        }
        VaultIndex {
            app_id,
            variants: Vec::new(),
            edit_base_hash: None,
        }
    }

    fn save_index(&self, app_id: u32, index: &VaultIndex) -> bool {
        let dir = self.app_dir(app_id);
        if fs::create_dir_all(&dir).is_err() {
            return false;
        }
        let path = self.index_path(app_id);
        if path.exists() {
            let _ = fs::copy(&path, path.with_extension("bak"));
        }
        let tmp = path.with_extension("tmp");
        let Ok(json) = serde_json::to_string_pretty(index) else {
            return false;
        };
        if fs::write(&tmp, json).is_err() {
            return false;
        }
        fs::rename(&tmp, &path).is_ok()
    }

    pub fn get_variants(&self, app_id: u32) -> Vec<LuaVariant> {
        let mut vars = self.load_index(app_id).variants;
        vars.sort_by(|a, b| {
            parse_timestamp_key(&b.captured_at).cmp(&parse_timestamp_key(&a.captured_at))
        });
        vars
    }

    pub fn get_active_hash(&self, app_id: u32) -> Option<String> {
        let live = self.live_path(app_id)?;
        if live.exists() {
            Self::hash_file(&live)
        } else {
            None
        }
    }

    pub fn get_active_variant(&self, app_id: u32) -> Option<LuaVariant> {
        let active = self.get_active_hash(app_id)?;
        self.get_variants(app_id)
            .into_iter()
            .find(|v| v.hash == active)
    }

    pub fn read_text(&self, app_id: u32, hash_str: &str) -> String {
        fs::read_to_string(self.variant_path(app_id, hash_str)).unwrap_or_default()
    }

    pub fn read_live_text(&self, app_id: u32) -> String {
        self.live_path(app_id)
            .and_then(|p| fs::read_to_string(p).ok())
            .unwrap_or_default()
    }

    pub fn set_edit_base(&self, app_id: u32, hash_str: Option<String>) {
        let mut idx = self.load_index(app_id);
        if idx.edit_base_hash != hash_str {
            idx.edit_base_hash = hash_str;
            self.save_index(app_id, &idx);
        }
    }

    fn describe_variant(
        app_id: u32,
        text: &str,
        hash_str: String,
        kind: &str,
        build_id: Option<String>,
        source: Option<String>,
    ) -> LuaVariant {
        let parsed = LuaFileParser::parse_text(text, app_id);
        let mut pins = BTreeMap::new();
        for (k, v) in &parsed.active_pins {
            pins.insert(k.to_string(), v.clone());
        }
        LuaVariant {
            hash: hash_str,
            kind: kind.to_string(),
            build_id,
            label: None,
            captured_at: now_iso(),
            source,
            manifest_ids: pins,
            depot_count: parsed.depot_count(),
            dlc_count: parsed.dlc_count(),
        }
    }

    pub fn capture(
        &self,
        app_id: u32,
        lua_path: &Path,
        kind: &str,
        build_id: Option<String>,
        source: Option<String>,
    ) -> Option<LuaVariant> {
        let bytes = fs::read(lua_path).ok()?;
        let hash_str = Self::hash_bytes(&bytes);
        let text = String::from_utf8_lossy(&bytes);

        let mut index = self.load_index(app_id);
        if let Some(existing) = index.variants.iter_mut().find(|v| v.hash == hash_str) {
            if existing.build_id.is_none() && build_id.is_some() {
                existing.build_id = build_id;
                existing.kind = KIND_BUILD.to_string();
                if existing.source.is_none() {
                    existing.source = source;
                }
                let cloned = existing.clone();
                self.save_index(app_id, &index);
                return Some(cloned);
            }
            return Some(existing.clone());
        }

        let _ = fs::create_dir_all(self.app_dir(app_id));
        fs::write(self.variant_path(app_id, &hash_str), &bytes).ok()?;

        let variant = Self::describe_variant(app_id, &text, hash_str, kind, build_id, source);
        index.variants.push(variant.clone());
        self.save_index(app_id, &index);
        Some(variant)
    }

    pub fn sync_default_from_live(&self, app_id: u32) -> bool {
        let Some(live_path) = self.live_path(app_id) else {
            return false;
        };
        let Ok(bytes) = fs::read(&live_path) else {
            return false;
        };
        let live_hash = Self::hash_bytes(&bytes);
        let mut index = self.load_index(app_id);

        if index.variants.iter().any(|v| v.hash == live_hash) || index.edit_base_hash.is_some() {
            return false;
        }

        let _ = fs::create_dir_all(self.app_dir(app_id));
        if fs::write(self.variant_path(app_id, &live_hash), &bytes).is_err() {
            return false;
        }

        let text = String::from_utf8_lossy(&bytes);
        let default_var =
            Self::describe_variant(app_id, &text, live_hash, KIND_DEFAULT, None, None);
        index.variants.push(default_var);
        self.save_index(app_id, &index)
    }

    pub fn apply(&self, app_id: u32, variant_hash: &str) -> bool {
        let Some(live_path) = self.live_path(app_id) else {
            return false;
        };
        let source_path = self.variant_path(app_id, variant_hash);
        if !source_path.exists() {
            return false;
        }

        self.sync_default_from_live(app_id);
        if let Some(parent) = live_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if fs::copy(&source_path, &live_path).is_ok() {
            self.set_edit_base(app_id, None);
            true
        } else {
            false
        }
    }

    pub fn write_live(&self, app_id: u32, text: &str) -> bool {
        let Some(live_path) = self.live_path(app_id) else {
            return false;
        };
        if let Some(parent) = live_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(&live_path, text).is_ok()
    }

    pub fn save_text(&self, app_id: u32, text: &str, label: Option<&str>) -> Option<LuaVariant> {
        let bytes = text.as_bytes();
        let hash_str = Self::hash_bytes(bytes);
        let _ = fs::create_dir_all(self.app_dir(app_id));
        fs::write(self.variant_path(app_id, &hash_str), bytes).ok()?;

        let mut index = self.load_index(app_id);
        let clean_label = label
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);

        if let Some(existing) = index.variants.iter_mut().find(|v| v.hash == hash_str) {
            if clean_label.is_some() {
                existing.label = clean_label;
            }
            let out = existing.clone();
            index.edit_base_hash = None;
            self.save_index(app_id, &index);
            return Some(out);
        }

        let mut var = Self::describe_variant(app_id, text, hash_str, KIND_PRESET, None, None);
        var.label = clean_label;
        index.variants.push(var.clone());
        index.edit_base_hash = None;
        self.save_index(app_id, &index);
        Some(var)
    }

    pub fn rename(&self, app_id: u32, hash_str: &str, new_label: Option<&str>) {
        let mut index = self.load_index(app_id);
        if let Some(v) = index.variants.iter_mut().find(|v| v.hash == hash_str) {
            v.label = new_label
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string);
            self.save_index(app_id, &index);
        }
    }

    pub fn delete(&self, app_id: u32, hash_str: &str) -> bool {
        if self.get_active_hash(app_id).as_deref() == Some(hash_str) {
            return false;
        }
        let mut index = self.load_index(app_id);
        let before = index.variants.len();
        index.variants.retain(|v| v.hash != hash_str);
        if index.variants.len() < before {
            let _ = fs::remove_file(self.variant_path(app_id, hash_str));
            self.save_index(app_id, &index);
            return true;
        }
        false
    }

    pub fn adopt_loose_build_luas(&self, app_id: u32) {
        let Some(inst) = &self.steam_installation else {
            return;
        };
        let Ok(entries) = fs::read_dir(&inst.stplug_in_path) else {
            return;
        };
        let prefix = format!("{}_", app_id);
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) != Some("lua") {
                continue;
            }
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                if let Some(build_id) = stem.strip_prefix(&prefix) {
                    if !build_id.is_empty() {
                        let _ =
                            self.capture(app_id, &p, KIND_BUILD, Some(build_id.to_string()), None);
                    }
                }
            }
        }
    }
}
