//! Pattern database for Steam build compatibility.
//!
//! The integration computes SHA-256 of `steamclient.so` and `steamui.so`, then
//! downloads a matching pattern TOML from the upstream pattern tracker
//! (`OpenSteam001/steam-monitor`). Each TOML contains byte patterns and offsets
//! for the integration points used by the memory patcher.
//!
//! Lookup order (every launch):
//!   1. Local cache — fast offline-first lookup (auto-purged if corrupt)
//!   2. GitHub raw — canonical upstream source
//!   3. jsDelivr CDN — fallback if GitHub raw is unreachable

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ureq;

use crate::steam_bridge::binary_reader::{BinaryFile, ElfClass};

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

impl PatternEntry {
    pub fn is_generic_compiler_prologue(&self) -> bool {
        if let Some(ref p) = self.pattern {
            PatternDb::is_generic_compiler_prologue(p)
        } else {
            false
        }
    }
}

impl PatternFile {
    pub fn is_fallback(&self) -> bool {
        self.build_id.as_deref() == Some("linux_fallback")
    }
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
        Self::with_cache_dir(cache_dir)
    }

    pub fn with_cache_dir(cache_dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&cache_dir);
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(15))
            .build();
        Self {
            cache_dir,
            http_agent: agent,
        }
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
        self.lookup_for_arch(component, channel, sha256_hex, !cfg!(target_arch = "x86"))
    }

    /// Lookup pattern file parameterized by architecture (32-bit vs 64-bit) (BST-22).
    pub fn lookup_for_arch(
        &self,
        component: BinaryComponent,
        channel: PatternChannel,
        sha256_hex: &str,
        is_64bit: bool,
    ) -> Result<PatternFile, PatternError> {
        let cache_name = format!(
            "{}_{}_{}.toml",
            channel.as_str(),
            component.as_str(),
            sha256_hex
        );
        let cache_path = self.cache_dir.join(&cache_name);

        if cache_path.exists() {
            if let Ok(content) = fs::read_to_string(&cache_path) {
                if let Ok(parsed) = toml::from_str::<PatternFile>(&content) {
                    // If cached file is a fallback, ensure its patterns match the requested architecture
                    let matches_arch = if parsed.build_id.as_deref() == Some("linux_fallback") {
                        if let Some(entry) = parsed.patterns.get("bbuild_async_send_frame")
                            .or_else(|| parsed.patterns.get("BBuildAndAsyncSendFrame"))
                        {
                            let pattern_str = entry.pattern.as_deref().unwrap_or("");
                            let is_pattern_64 = pattern_str.starts_with("55 48 89 E5");
                            is_pattern_64 == is_64bit
                        } else {
                            true
                        }
                    } else {
                        true
                    };
                    if matches_arch {
                        return Ok(parsed);
                    }
                }
            }
            // Remove mismatched or corrupt cache entry so we can re-fetch cleanly from upstream
            let _ = fs::remove_file(&cache_path);
        }

        let url = format!(
            "{}/{}/{}/{}.toml",
            PATTERN_TRACKER_GITHUB_RAW,
            channel.as_str(),
            component.as_str(),
            sha256_hex
        );

        let fetch_res = self
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
            });

        let content = match fetch_res {
            Ok(c) => {
                let _ = fs::write(&cache_path, &c);
                c
            }
            Err(_) => {
                let fb_pf = Self::fallback_pattern_file_for_arch(component, channel, sha256_hex, is_64bit);
                if let Ok(c) = toml::to_string(&fb_pf) {
                    let _ = fs::write(&cache_path, &c);
                    c
                } else {
                    return Ok(fb_pf);
                }
            }
        };

        let parsed: PatternFile =
            toml::from_str(&content).map_err(|e| PatternError::Parse(e.to_string()))?;
        Ok(parsed)
    }

    /// Returns true if a pattern represents a generic compiler prologue (push rbp/ebp; mov rbp,rsp / ebp,esp)
    /// without distinguishing function-specific instructions.
    pub fn is_generic_compiler_prologue(pattern_hex: &str) -> bool {
        let clean = pattern_hex
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_uppercase();
        // 64-bit generic prologues (push rbp; mov rbp, rsp...)
        if clean.starts_with("55 48 89 E5") {
            return true;
        }
        // 32-bit generic prologues (push ebp; mov ebp, esp...)
        if clean.starts_with("55 89 E5") {
            return true;
        }
        false
    }

    /// Built-in fallback patterns for Linux Steam binaries when upstream tracker lacks Linux entries.
    pub fn fallback_pattern_file(
        component: BinaryComponent,
        channel: PatternChannel,
        sha256_hex: &str,
    ) -> PatternFile {
        Self::fallback_pattern_file_for_arch(component, channel, sha256_hex, !cfg!(target_arch = "x86"))
    }

    /// Built-in fallback patterns for Linux Steam binaries parameterized by architecture (32-bit vs 64-bit).
    pub fn fallback_pattern_file_for_arch(
        component: BinaryComponent,
        _channel: PatternChannel,
        sha256_hex: &str,
        is_64bit: bool,
    ) -> PatternFile {
        let mut patterns = BTreeMap::new();
        match component {
            BinaryComponent::SteamClient => {
                if !is_64bit {
                    // 32-bit x86 patterns (i686 Linux steamclient.so)
                    patterns.insert(
                        "BuildDepotDependency".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 57 56 53 83 EC ? 8B 45 ? 8D 45".to_string()),
                            mask: Some("xxxxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("BuildDepotDependency prolog (i686 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "YldLoadDepotManifest".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 57 56 53 83 EC ? 8B 45 ? 89 7D".to_string()),
                            mask: Some("xxxxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("YldLoadDepotManifest prolog (i686 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "ConfigStoreGetBinary".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 57 56 53 83 EC ? 8B 45 ? 8B 7D".to_string()),
                            mask: Some("xxxxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("ConfigStoreGetBinary prolog (i686 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "ConfigStore_GetBinary".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 57 56 53 83 EC ? 8B 45 ? 8B 7D".to_string()),
                            mask: Some("xxxxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("ConfigStore_GetBinary alias prolog (i686 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "bis_subscribed_app".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 53 83 EC ? 89 75 ? 89 7D".to_string()),
                            mask: Some("xxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("BIsSubscribedApp prolog (i686 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "get_steam_id".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 8B 47 ? 85 C0 74 ? 8B 80".to_string()),
                            mask: Some("xxxxx?xxx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("GetSteamID prolog (i686 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "depot_key_integration".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 57 56 53 83 EC ? 8B 5D ? 89 5D".to_string()),
                            mask: Some("xxxxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("DepotKeyIntegration fallback (i686 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "SendCallbackToPipe".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 57 56 53 83 EC ? 89 75 ? 89 7D ?".to_string()),
                            mask: Some("xxxxxxxx?xx?xx?".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("SendCallbackToPipe prolog (i686 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "bbuild_async_send_frame".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 57 56 53 83 EC ? 8B 45 ? 89 5D".to_string()),
                            mask: Some("xxxxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("BBuildAndAsyncSendFrame prolog (i686 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "BBuildAndAsyncSendFrame".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 57 56 53 83 EC ? 8B 45 ? 89 5D".to_string()),
                            mask: Some("xxxxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("BBuildAndAsyncSendFrame alias prolog (i686 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "recv_pkt".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 57 56 53 83 EC ? 8B 75 ? 89 7D".to_string()),
                            mask: Some("xxxxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("RecvPkt prolog (i686 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "RecvPkt".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 57 56 53 83 EC ? 8B 75 ? 89 7D".to_string()),
                            mask: Some("xxxxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("RecvPkt alias prolog (i686 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "CUtlMemoryGrow".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 53 83 EC ? 8B 5D ? 85 DB".to_string()),
                            mask: Some("xxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("CUtlMemoryGrow prolog (i686 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "MarkLicenseAsChanged".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 57 56 53 83 EC ? 8B 7D ? 8B 75".to_string()),
                            mask: Some("xxxxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("MarkLicenseAsChanged prolog (i686 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "ProcessPendingLicenseUpdates".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 53 83 EC ? 8B 45 ? 85 C0".to_string()),
                            mask: Some("xxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("ProcessPendingLicenseUpdates prolog (i686 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "GetPackageInfo".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 56 53 83 EC ? 8B 75 ? 8B 5D".to_string()),
                            mask: Some("xxxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("GetPackageInfo prolog (i686 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "CheckAppOwnership".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 57 56 53 83 EC ? 8B 45 ? 89 44".to_string()),
                            mask: Some("xxxxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("CheckAppOwnership prolog (i686 Linux steamclient.so)".to_string()),
                        },
                    );
                } else {
                    // 64-bit x86_64 patterns
                    patterns.insert(
                        "BuildDepotDependency".to_string(),
                        PatternEntry {
                            pattern: Some("55 48 89 E5 41 57 41 56 41 55 41 54 53 48 81 EC ? ? 00 00".to_string()),
                            mask: Some("xxxxxxxxxxxxxxxx??xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("BuildDepotDependency prolog (x86_64 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "YldLoadDepotManifest".to_string(),
                        PatternEntry {
                            pattern: Some("55 48 89 E5 41 57 41 56 41 55 41 54 53 48 83 EC ? 49 89".to_string()),
                            mask: Some("xxxxxxxxxxxxxxxx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("YldLoadDepotManifest prolog (x86_64 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "ConfigStoreGetBinary".to_string(),
                        PatternEntry {
                            pattern: Some("55 48 89 E5 41 57 41 56 41 55 41 54 53 48 83 EC ? 48 89".to_string()),
                            mask: Some("xxxxxxxxxxxxxxxx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("ConfigStoreGetBinary prolog (x86_64 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "ConfigStore_GetBinary".to_string(),
                        PatternEntry {
                            pattern: Some("55 48 89 E5 41 57 41 56 41 55 41 54 53 48 83 EC ? 48 89".to_string()),
                            mask: Some("xxxxxxxxxxxxxxxx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("ConfigStore_GetBinary alias prolog".to_string()),
                        },
                    );
                    patterns.insert(
                        "bis_subscribed_app".to_string(),
                        PatternEntry {
                            pattern: Some("55 48 89 E5 41 54 53 48 83 EC ? 89 75 ? 48 89 7D".to_string()),
                            mask: Some("xxxxxxxxxx?xx?xxx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("BIsSubscribedApp prolog".to_string()),
                        },
                    );
                    patterns.insert(
                        "get_steam_id".to_string(),
                        PatternEntry {
                            pattern: Some("55 48 89 E5 48 8B 47 ? 48 85 C0 74 ? 48 8B 80".to_string()),
                            mask: Some("xxxxxxx?xxxx?xxx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("GetSteamID prolog".to_string()),
                        },
                    );
                    patterns.insert(
                        "depot_key_integration".to_string(),
                        PatternEntry {
                            pattern: Some("55 48 89 E5 41 56 41 55 41 54 53 48 83 EC ? 48 89".to_string()),
                            mask: Some("xxxxxxxxxxxxxx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("DepotKeyIntegration fallback".to_string()),
                        },
                    );
                    patterns.insert(
                        "SendCallbackToPipe".to_string(),
                        PatternEntry {
                            pattern: Some("55 48 89 E5 41 57 41 56 41 55 41 54 53 48 83 EC ? 89 75 ? 89 7D ?".to_string()),
                            mask: Some("xxxxxxxxxxxxxxxx?xx?xx?".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("SendCallbackToPipe prolog (x86_64/i686 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "bbuild_async_send_frame".to_string(),
                        PatternEntry {
                            pattern: Some("55 48 89 E5 41 57 41 56 41 55 41 54 53 48 83 EC ? 48 89 7D".to_string()),
                            mask: Some("xxxxxxxxxxxxxxxx?xxx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("BBuildAndAsyncSendFrame prolog (x86_64 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "BBuildAndAsyncSendFrame".to_string(),
                        PatternEntry {
                            pattern: Some("55 48 89 E5 41 57 41 56 41 55 41 54 53 48 83 EC ? 48 89 7D".to_string()),
                            mask: Some("xxxxxxxxxxxxxxxx?xxx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("BBuildAndAsyncSendFrame alias prolog".to_string()),
                        },
                    );
                    patterns.insert(
                        "recv_pkt".to_string(),
                        PatternEntry {
                            pattern: Some("55 48 89 E5 41 56 41 55 41 54 53 48 83 EC ? 48 89 75".to_string()),
                            mask: Some("xxxxxxxxxxxxxx?xxx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("RecvPkt prolog (x86_64 Linux steamclient.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "RecvPkt".to_string(),
                        PatternEntry {
                            pattern: Some("55 48 89 E5 41 56 41 55 41 54 53 48 83 EC ? 48 89 75".to_string()),
                            mask: Some("xxxxxxxxxxxxxx?xxx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("RecvPkt alias prolog".to_string()),
                        },
                    );

                    // === Fallback entries for interop_package.rs (SteamClient) ===
                    // CUtlMemoryGrow: called from InitFakeLicense, hook installed at line 813
                    patterns.insert(
                        "CUtlMemoryGrow".to_string(),
                        PatternEntry {
                            // Typical utlmemory grow: push rbp; mov rbp,rsp; push rbx; sub rsp,0x20; mov edi,esi...
                            pattern: Some("55 48 89 E5 53 48 83 EC ? 89 F3".to_string()),
                            mask: Some("xxxxxxxx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("CUtlMemoryGrow prolog (x86_64 Linux steamclient.so)".to_string()),
                        },
                    );
                    // MarkLicenseAsChanged: hook installed at line 837
                    patterns.insert(
                        "MarkLicenseAsChanged".to_string(),
                        PatternEntry {
                            // push rbp; mov rbp,rsp; push r15; push r14; push r13; push r12; push rbx; sub rsp,0x38
                            pattern: Some("55 48 89 E5 41 57 41 56 41 55 41 54 53 48 83 EC ?".to_string()),
                            mask: Some("xxxxxxxxxxxxxxxx?".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("MarkLicenseAsChanged prolog (x86_64 Linux steamclient.so)".to_string()),
                        },
                    );
                    // ProcessPendingLicenseUpdates: hook installed at line 861
                    patterns.insert(
                        "ProcessPendingLicenseUpdates".to_string(),
                        PatternEntry {
                            // push rbp; mov rbp,rsp; push rbx; sub rsp,0x20; mov rdi,rdi...
                            pattern: Some("55 48 89 E5 53 48 83 EC ? 48 89 F8".to_string()),
                            mask: Some("xxxxxxxx?xxx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("ProcessPendingLicenseUpdates prolog (x86_64 Linux steamclient.so)".to_string()),
                        },
                    );
                    // GetPackageInfo: resolved at line 885 (no hook, just call)
                    patterns.insert(
                        "GetPackageInfo".to_string(),
                        PatternEntry {
                            // push rbp; mov rbp,rsp; push r14; push r13; push r12; push rbx; sub rsp,0x28
                            pattern: Some("55 48 89 E5 41 56 41 55 41 54 53 48 83 EC ?".to_string()),
                            mask: Some("xxxxxxxxxxxxxx?".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("GetPackageInfo prolog (x86_64 Linux steamclient.so)".to_string()),
                        },
                    );
                    // CheckAppOwnership: hook installed at line 899
                    patterns.insert(
                        "CheckAppOwnership".to_string(),
                        PatternEntry {
                            // push rbp; mov rbp,rsp; push r15; push r14; push r13; push r12; push rbx; sub rsp,0x48
                            pattern: Some("55 48 89 E5 41 57 41 56 41 55 41 54 53 48 83 EC ?".to_string()),
                            mask: Some("xxxxxxxxxxxxxxxx?".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("CheckAppOwnership prolog (x86_64 Linux steamclient.so)".to_string()),
                        },
                    );
                }
            }
            BinaryComponent::SteamUi => {
                if !is_64bit {
                    // 32-bit x86 patterns for SteamUI (i686 Linux steamui.so)
                    patterns.insert(
                        "FillInAppOverview".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 57 56 53 83 EC ? 8B 45 ? 8D 45".to_string()),
                            mask: Some("xxxxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("FillInAppOverview prolog (i686 Linux steamui.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "GetAppByID".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 56 53 83 EC ? 89 75 ? 89 7D".to_string()),
                            mask: Some("xxxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("GetAppByID prolog (i686 Linux steamui.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "MarkAppChange".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 53 83 EC ? 89 75 ? 89 45".to_string()),
                            mask: Some("xxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("MarkAppChange prolog (i686 Linux steamui.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "RepeatedFieldUint32Add".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 53 83 EC ? 8B 45 ? 89 45".to_string()),
                            mask: Some("xxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("RepeatedFieldUint32Add prolog (i686 Linux steamui.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "BuildCompleteAppOverviewChange".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 57 56 53 83 EC ? 8B 45 ? 89 44".to_string()),
                            mask: Some("xxxxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("BuildCompleteAppOverviewChange prolog (i686 Linux steamui.so)".to_string()),
                        },
                    );
                    patterns.insert(
                        "CSteamUIAppControllerRunFrame".to_string(),
                        PatternEntry {
                            pattern: Some("55 89 E5 57 56 53 83 EC ? 8B 45 ? 89 7D".to_string()),
                            mask: Some("xxxxxxxx?xx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("CSteamUIAppControllerRunFrame prolog (i686 Linux steamui.so)".to_string()),
                        },
                    );
                } else {
                    // 64-bit x86_64 patterns
                    // FillInAppOverview: Fixed mask length (16 bytes pattern -> 16 chars mask)
                    patterns.insert(
                        "FillInAppOverview".to_string(),
                        PatternEntry {
                            pattern: Some("55 48 89 E5 41 57 41 56 41 55 41 54 53 48 81 EC".to_string()),
                            mask: Some("xxxxxxxxxxxxxxxx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("FillInAppOverview prolog (x86_64 Linux steamui.so)".to_string()),
                        },
                    );

                    // === Fallback entries for interop_steamui.rs (SteamUI) ===
                    // GetAppByID: resolved at line 381
                    patterns.insert(
                        "GetAppByID".to_string(),
                        PatternEntry {
                            // push rbp; mov rbp,rsp; push r14; push r13; push r12; push rbx; sub rsp,0x28
                            pattern: Some("55 48 89 E5 41 56 41 55 41 54 53 48 83 EC ?".to_string()),
                            mask: Some("xxxxxxxxxxxxxx?".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("GetAppByID prolog (x86_64 Linux steamui.so)".to_string()),
                        },
                    );
                    // MarkAppChange: resolved at line 395
                    patterns.insert(
                        "MarkAppChange".to_string(),
                        PatternEntry {
                            // push rbp; mov rbp,rsp; push rbx; sub rsp,0x20; mov esi,edi...
                            pattern: Some("55 48 89 E5 53 48 83 EC ? 89 F0".to_string()),
                            mask: Some("xxxxxxxx?xx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("MarkAppChange prolog (x86_64 Linux steamui.so)".to_string()),
                        },
                    );
                    // RepeatedFieldUint32Add: resolved at line 409
                    patterns.insert(
                        "RepeatedFieldUint32Add".to_string(),
                        PatternEntry {
                            // push rbp; mov rbp,rsp; push rbx; sub rsp,0x10; mov rdi,rsi...
                            pattern: Some("55 48 89 E5 53 48 83 EC ? 48 89 F7".to_string()),
                            mask: Some("xxxxxxxx?xxx".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("RepeatedFieldUint32Add prolog (x86_64 Linux steamui.so)".to_string()),
                        },
                    );
                    // BuildCompleteAppOverviewChange: hook installed at line 447
                    patterns.insert(
                        "BuildCompleteAppOverviewChange".to_string(),
                        PatternEntry {
                            // push rbp; mov rbp,rsp; push r15; push r14; push r13; push r12; push rbx; sub rsp,0x58
                            pattern: Some("55 48 89 E5 41 57 41 56 41 55 41 54 53 48 83 EC ?".to_string()),
                            mask: Some("xxxxxxxxxxxxxxxx?".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("BuildCompleteAppOverviewChange prolog (x86_64 Linux steamui.so)".to_string()),
                        },
                    );
                    // CSteamUIAppControllerRunFrame: hook installed at line 471
                    patterns.insert(
                        "CSteamUIAppControllerRunFrame".to_string(),
                        PatternEntry {
                            // push rbp; mov rbp,rsp; push r15; push r14; push r13; push r12; push rbx; sub rsp,0x48
                            pattern: Some("55 48 89 E5 41 57 41 56 41 55 41 54 53 48 83 EC ?".to_string()),
                            mask: Some("xxxxxxxxxxxxxxxx?".to_string()),
                            offset: Some(0),
                            address: None,
                            description: Some("CSteamUIAppControllerRunFrame prolog (x86_64 Linux steamui.so)".to_string()),
                        },
                    );
                }
            }
        }
        PatternFile {
            component: Some(component.as_str().to_string()),
            build_id: Some("linux_fallback".to_string()),
            sha256: Some(sha256_hex.to_string()),
            patterns,
        }
    }

    fn fetch_url(&self, url: &str) -> Result<String, PatternError> {
        let resp = self
            .http_agent
            .get(url)
            .set("User-Agent", &format!("LuaToolsLinux/{}", crate::VERSION))
            .call()
            .map_err(|e| PatternError::Http(e.to_string()))?;
        if resp.status() != 200 {
            return Err(PatternError::Http(format!("HTTP {}", resp.status())));
        }
        resp.into_string()
            .map_err(|e| PatternError::Http(e.to_string()))
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
            .ok_or_else(|| PatternError::NotFound(pattern_name.to_string(), sha256_hex.to_string()))
    }

    pub fn lookup_by_binary(
        &self,
        binary: &BinaryFile,
        channel: PatternChannel,
        component: BinaryComponent,
    ) -> Result<PatternFile, PatternError> {
        let is_64bit = binary.class == ElfClass::Class64;
        self.lookup_for_arch(component, channel, binary.sha256_hex(), is_64bit)
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

    #[test]
    fn test_linux_fallback_patterns_available() {
        let pf = PatternDb::fallback_pattern_file(
            BinaryComponent::SteamClient,
            PatternChannel::Pattern,
            "1122334455667788",
        );

        // Core SteamClient patterns required regardless of target arch
        assert!(pf.patterns.contains_key("BuildDepotDependency"));
        assert!(pf.patterns.contains_key("YldLoadDepotManifest"));
        assert!(pf.patterns.contains_key("ConfigStoreGetBinary"));
        assert!(pf.patterns.contains_key("bis_subscribed_app"));
        assert!(pf.patterns.contains_key("get_steam_id"));
        assert!(pf.patterns.contains_key("SendCallbackToPipe"));
        assert!(pf.patterns.contains_key("BBuildAndAsyncSendFrame"));
        assert!(pf.patterns.contains_key("RecvPkt"));
        assert!(pf.patterns.contains_key("CUtlMemoryGrow"));
        assert!(pf.patterns.contains_key("MarkLicenseAsChanged"));
        assert!(pf.patterns.contains_key("ProcessPendingLicenseUpdates"));
        assert!(pf.patterns.contains_key("GetPackageInfo"));
        assert!(pf.patterns.contains_key("CheckAppOwnership"));
        assert_eq!(pf.build_id, Some("linux_fallback".to_string()));

        let pf_ui = PatternDb::fallback_pattern_file(
            BinaryComponent::SteamUi,
            PatternChannel::Pattern,
            "1122334455667788",
        );
        assert!(pf_ui.patterns.contains_key("FillInAppOverview"));
        assert!(pf_ui.patterns.contains_key("GetAppByID"));
        assert!(pf_ui.patterns.contains_key("MarkAppChange"));
        assert!(pf_ui.patterns.contains_key("RepeatedFieldUint32Add"));
        assert!(pf_ui.patterns.contains_key("BuildCompleteAppOverviewChange"));
        assert!(pf_ui.patterns.contains_key("CSteamUIAppControllerRunFrame"));

        // Verify both 32-bit (i686) and 64-bit (x86_64) variants have full coverage
        for &is_64bit in &[false, true] {
            let client = PatternDb::fallback_pattern_file_for_arch(
                BinaryComponent::SteamClient,
                PatternChannel::Pattern,
                "test_sha",
                is_64bit,
            );
            assert!(client.patterns.contains_key("BuildDepotDependency"));
            assert!(client.patterns.contains_key("YldLoadDepotManifest"));
            assert!(client.patterns.contains_key("ConfigStoreGetBinary"));
            assert!(client.patterns.contains_key("bis_subscribed_app"));
            assert!(client.patterns.contains_key("get_steam_id"));
            assert!(client.patterns.contains_key("SendCallbackToPipe"));
            assert!(client.patterns.contains_key("BBuildAndAsyncSendFrame"));
            assert!(client.patterns.contains_key("RecvPkt"));
            assert!(client.patterns.contains_key("CUtlMemoryGrow"));
            assert!(client.patterns.contains_key("MarkLicenseAsChanged"));
            assert!(client.patterns.contains_key("ProcessPendingLicenseUpdates"));
            assert!(client.patterns.contains_key("GetPackageInfo"));
            assert!(client.patterns.contains_key("CheckAppOwnership"));

            let ui = PatternDb::fallback_pattern_file_for_arch(
                BinaryComponent::SteamUi,
                PatternChannel::Pattern,
                "test_sha",
                is_64bit,
            );
            assert!(ui.patterns.contains_key("FillInAppOverview"));
            assert!(ui.patterns.contains_key("GetAppByID"));
            assert!(ui.patterns.contains_key("MarkAppChange"));
            assert!(ui.patterns.contains_key("RepeatedFieldUint32Add"));
            assert!(ui.patterns.contains_key("BuildCompleteAppOverviewChange"));
            assert!(ui.patterns.contains_key("CSteamUIAppControllerRunFrame"));
        }
    }

    #[test]
    fn test_linux_fallback_patterns_valid_masks_and_wildcards() {
        for &is_64bit in &[false, true] {
            for &comp in &[BinaryComponent::SteamClient, BinaryComponent::SteamUi] {
                let pf = PatternDb::fallback_pattern_file_for_arch(
                    comp,
                    PatternChannel::Pattern,
                    "deadbeef",
                    is_64bit,
                );
                for (name, entry) in &pf.patterns {
                    let pat_str = entry
                        .pattern
                        .as_ref()
                        .unwrap_or_else(|| panic!("missing pattern for {}", name));
                    let mask_str = entry
                        .mask
                        .as_ref()
                        .unwrap_or_else(|| panic!("missing mask for {}", name));
                    let parsed = crate::steam_bridge::manifest_hooks::parse_pattern_and_mask(
                        pat_str,
                        Some(mask_str),
                    );
                    assert!(
                        parsed.is_some(),
                        "parse_pattern_and_mask failed for {} (64bit={}): pat='{}', mask='{}'",
                        name,
                        is_64bit,
                        pat_str,
                        mask_str
                    );
                    let pat_bytes: Vec<&str> = pat_str.split_whitespace().collect();
                    assert_eq!(
                        pat_bytes.len(),
                        mask_str.len(),
                        "Length mismatch for {} (64bit={}): pat has {} bytes, mask has {} chars",
                        name,
                        is_64bit,
                        pat_bytes.len(),
                        mask_str.len()
                    );
                    for (i, (&b, m)) in pat_bytes.iter().zip(mask_str.chars()).enumerate() {
                        let b_wild = b == "?" || b == "??";
                        let m_wild = m == '?';
                        assert_eq!(
                            b_wild,
                            m_wild,
                            "Wildcard mismatch for {} at [{}] (64bit={}): byte='{}', mask='{}'",
                            name,
                            i,
                            is_64bit,
                            b,
                            m
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn test_bst22_lookup_by_binary_architecture_routing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db = PatternDb::with_cache_dir(temp_dir.path().to_path_buf());

        // Fake 32-bit binary
        let mut elf32 = vec![0u8; 64];
        elf32[0..4].copy_from_slice(b"\x7fELF");
        elf32[4] = 1; // Class32
        elf32[5] = 1; // Lsb
        elf32[6] = 1;
        let bin32 = BinaryFile::from_bytes(PathBuf::from("/tmp/fake_steamclient32.so"), elf32).unwrap();

        // Fake 64-bit binary
        let mut elf64 = vec![0u8; 64];
        elf64[0..4].copy_from_slice(b"\x7fELF");
        elf64[4] = 2; // Class64
        elf64[5] = 1; // Lsb
        elf64[6] = 1;
        let bin64 = BinaryFile::from_bytes(PathBuf::from("/tmp/fake_steamclient64.so"), elf64).unwrap();

        let pf32 = db.lookup_by_binary(&bin32, PatternChannel::Pattern, BinaryComponent::SteamClient).unwrap();
        let entry32 = pf32.patterns.get("bbuild_async_send_frame").unwrap();
        assert!(entry32.pattern.as_ref().unwrap().starts_with("55 89 E5"), "32-bit binary must get 32-bit pattern");

        let pf64 = db.lookup_by_binary(&bin64, PatternChannel::Pattern, BinaryComponent::SteamClient).unwrap();
        let entry64 = pf64.patterns.get("bbuild_async_send_frame").unwrap();
        assert!(entry64.pattern.as_ref().unwrap().starts_with("55 48 89 E5"), "64-bit binary must get 64-bit pattern");
    }
}
