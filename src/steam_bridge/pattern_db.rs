//! Pattern database for Steam build compatibility.
//!
//! The integration computes SHA-256 of `steamclient.so` and `steamui.so`, then
//! downloads a matching pattern TOML from the upstream pattern tracker
//! (`OpenSteam001/steam-monitor`). Each TOML contains byte patterns and offsets
//! for the integration points used by the memory patcher.
//!
//! Lookup order (every launch):
//!   1. GitHub raw — canonical source
//!   2. jsDelivr CDN — fallback if GitHub raw is unreachable
//!   3. Local cache — used only when remote is unreachable

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ureq;

use crate::steam_bridge::binary_reader::BinaryFile;

pub const PATTERN_TRACKER_GITHUB_RAW: &str =
    "https://raw.githubusercontent.com/OpenSteam001/steam-monitor/pattern";
pub const PATTERN_TRACKER_CDN: &str =
    "https://cdn.jsdelivr.net/gh/OpenSteam001/steam-monitor@pattern";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternChannel {
    Pattern,
    Ipc,
}

impl PatternChannel {
    pub fn as_str(&self) -> &'static str {
        match self {
            PatternChannel::Pattern => "pattern",
            PatternChannel::Ipc => "ipc",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryComponent {
    SteamClient,
    SteamUi,
}

impl BinaryComponent {
    pub fn as_str(&self) -> &'static str {
        match self {
            BinaryComponent::SteamClient => "steamclient",
            BinaryComponent::SteamUi => "steamui",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatternFile {
    pub component: Option<String>,
    pub build_id: Option<String>,
    pub sha256: Option<String>,
    #[serde(default)]
    pub patterns: BTreeMap<String, PatternEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatternEntry {
    pub pattern: Option<String>,
    pub mask: Option<String>,
    pub offset: Option<i64>,
    pub address: Option<u64>,
    pub description: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PatternError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("Pattern not found for {0}:{1}")]
    NotFound(String, String),
    #[error("Parse error: {0}")]
    Parse(String),
}

#[derive(Debug, Clone)]
pub struct PatternDb {
    cache_dir: PathBuf,
    http_agent: ureq::Agent,
}

impl Default for PatternDb {
    fn default() -> Self {
        Self::new()
    }
}

impl PatternDb {
    pub fn new() -> Self {
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("luatools")
            .join("pattern");
        let _ = fs::create_dir_all(&cache_dir);
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(15))
            .build();
        Self { cache_dir, http_agent: agent }
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn lookup(
        &self,
        component: BinaryComponent,
        channel: PatternChannel,
        sha256_hex: &str,
    ) -> Result<PatternFile, PatternError> {
        let cache_name =
            format!("{}_{}_{}.toml", channel.as_str(), component.as_str(), sha256_hex);
        let cache_path = self.cache_dir.join(&cache_name);

        if cache_path.exists() {
            let content = fs::read_to_string(&cache_path)?;
            let parsed: PatternFile = toml::from_str(&content).map_err(|e| {
                PatternError::Parse(e.to_string())
            })?;
            return Ok(parsed);
        }

        let url = format!(
            "{}/{}/{}/{}.toml",
            PATTERN_TRACKER_GITHUB_RAW,
            channel.as_str(),
            component.as_str(),
            sha256_hex
        );

        let content = self
            .fetch_url(&url)
            .or_else(|_| {
                let fb_url = format!(
                    "{}/{}/{}/{}.toml",
                    PATTERN_TRACKER_CDN,
                    channel.as_str(),
                    component.as_str(),
                    sha256_hex
                );
                self.fetch_url(&fb_url)
            })
            .map_err(|_e| {
                PatternError::NotFound(
                    component.as_str().to_string(),
                    sha256_hex.to_string(),
                )
            })?;

        let _ = fs::write(&cache_path, &content);

        let parsed: PatternFile = toml::from_str(&content).map_err(|e| {
            PatternError::Parse(e.to_string())
        })?;
        Ok(parsed)
    }

    fn fetch_url(&self, url: &str) -> Result<String, PatternError> {
        let resp = self
            .http_agent
            .get(url)
            .call()
            .map_err(|e| PatternError::Http(e.to_string()))?;
        if resp.status() != 200 {
            return Err(PatternError::Http(format!("HTTP {}", resp.status())));
        }
        resp.into_string().map_err(|e| PatternError::Http(e.to_string()))
    }

    pub fn get_pattern(
        &self,
        component: BinaryComponent,
        channel: PatternChannel,
        sha256_hex: &str,
        pattern_name: &str,
    ) -> Result<PatternEntry, PatternError> {
        let file = self.lookup(component, channel, sha256_hex)?;
        file.patterns
            .get(pattern_name)
            .cloned()
            .ok_or_else(|| {
                PatternError::NotFound(
                    pattern_name.to_string(),
                    sha256_hex.to_string(),
                )
            })
    }

    pub fn lookup_by_binary(
        &self,
        binary: &BinaryFile,
        channel: PatternChannel,
        component: BinaryComponent,
    ) -> Result<PatternFile, PatternError> {
        self.lookup(component, channel, binary.sha256_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_channel_as_str() {
        assert_eq!(PatternChannel::Pattern.as_str(), "pattern");
        assert_eq!(PatternChannel::Ipc.as_str(), "ipc");
    }

    #[test]
    fn binary_component_as_str() {
        assert_eq!(BinaryComponent::SteamClient.as_str(), "steamclient");
        assert_eq!(BinaryComponent::SteamUi.as_str(), "steamui");
    }

    #[test]
    fn default_pattern_file_is_empty() {
        let pf = PatternFile::default();
        assert!(pf.patterns.is_empty());
    }
}
