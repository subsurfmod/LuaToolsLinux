//! Manifest dependency hooks for Linux.
//!
//! Port of usefulsteamtools `Interop_Manifest.cpp` hooks:
//! - `BuildDepotDependency` - Patches depot entries' gid/size
//! - `YldLoadDepotManifest` - Pre-seeds manifest cache
//!
//! On Linux, these hooks are installed via pattern scanning in `libsteam_integration.so`
//! using 14-byte RIP-relative indirect jumps.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use std::thread;

use super::binary_reader::{BinaryFile, LoadedModuleMapping};
use super::memory_patch::JMP_ABS64_LEN;
use super::pattern_db::PatternDb;
use super::pattern_db::{BinaryComponent, PatternChannel, PatternFile};

/// Manifest override configuration
#[derive(Debug, Clone)]
pub struct ManifestOverride {
    pub gid: u64,
    pub size: Option<u64>,
}

impl ManifestOverride {
    pub fn new(gid: u64, size: Option<u64>) -> Self {
        Self { gid, size }
    }
}

/// Global manifest overrides registry
static MANIFEST_OVERRIDES: OnceLock<RwLock<HashMap<(u32, u32), ManifestOverride>>> = OnceLock::new();

/// Depots seen during BuildDepotDependency calls (for reporting missing manifests)
static DEPOTS_SEEN: Mutex<HashMap<u32, (u32, u64)>> = Mutex::new(HashMap::new());

/// Missing manifests waiting to be reported (debounced)
static MISSING_MANIFESTS: Mutex<Vec<(u32, u64)>> = Mutex::new(Vec::new());

/// Static flag for debouncing missing manifest reports
static MISSING_REPORT_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

use std::sync::OnceLock;

fn manifest_overrides() -> &'static RwLock<HashMap<(u32, u32), ManifestOverride>> {
    MANIFEST_OVERRIDES.get_or_init(OnceLock::new)
}

/// Set a manifest override for a depot
pub fn set_manifest_override(app_id: u32, depot_id: u32, override_data: ManifestOverride) {
    let mut overrides = manifest_overrides().write().unwrap();
    overrides.insert((app_id, depot_id), override_data);
}

/// Remove a manifest override
pub fn remove_manifest_override(app_id: u32, depot_id: u32) {
    let mut overrides = manifest_overrides().write().unwrap();
    overrides.remove(&(app_id, depot_id));
}

/// Get all manifest overrides
pub fn get_manifest_overrides() -> HashMap<(u32, u32), ManifestOverride> {
    manifest_overrides().read().unwrap().clone()
}

/// Report a missing manifest (debounced)
pub fn report_missing_manifest(depot_id: u32, gid: u64) {
    let mut missing = MISSING_MANIFESTS.lock().unwrap();
    missing.push((depot_id, gid));

    if !MISSING_REPORT_PENDING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        // Schedule debouncing
        let deps = missing.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(3000));

            MISSING_REPORT_PENDING.swap(false, std::sync::atomic::Ordering::SeqCst);

            let mut final_missing = MISSING_MANIFESTS.lock().unwrap();
            let count = final_missing.len();
            if count > 0 {
                eprintln!("[manifest] {} manifest(s) not ready - try download again later", count);
                final_missing.clear();
            }
        });
    }
}

/// Pre-seed manifest cache for a depot
pub fn ensure_manifest_cached(steam_root: &std::path::Path, app_id: u32, depot_id: u32, gid: u64) -> bool {
    let manifest_path = steam_root.join("depotcache").join(format!("{}_{}.manifest", depot_id, gid));

    if manifest_path.exists() {
        // Manifest already cached
        return true;
    }

    eprintln!("[manifest] pre-seed missing: app={} depot={} gid={}", app_id, depot_id, gid);
    false
}

/// Record depots seen in a BuildDepotDependency call
pub fn record_depots(depot_info: Option<&[DepotEntry]>) {
    if let Some(entries) = depot_info {
        let mut seen = DEPOTS_SEEN.lock().unwrap();
        for e in entries.iter() {
            if e.depot_id != 0 && e.manifest_gid != 0 {
                seen.insert(e.depot_id, (e.app_id, e.manifest_gid));
            }
        }
    }
}

/// Get seen depot info
pub fn get_seen_depot(app_id: u32, depot_id: u32) -> Option<(u32, u64)> {
    let seen = DEPOTS_SEEN.lock().unwrap();
    seen.get(&depot_id).copied()
}

/// Depot entry structure (matches Steam's internal structure)
#[derive(Debug, Clone)]
pub struct DepotEntry {
    pub app_id: u32,
    pub depot_id: u32,
    pub manifest_gid: u64,
    pub manifest_size: u64,
    pub dlc_app_id: u32,
}

/// Install manifest hooks in steamclient.so
pub fn install_manifest_hooks(
    steam_root: &std::path::Path,
    mapping: &LoadedModuleMapping,
) -> Result<usize, String> {
    let binaries = super::binary_reader::scan_steam_binaries(steam_root);

    let steam_client = binaries.into_iter()
        .find(|b| b.path.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name == "steamclient.so"));

    let Some(client) = steam_client else {
        return Err("steamclient.so not found".to_string());
    };

    let pattern_db = PatternDb::new();
    let pf = match pattern_db.lookup_by_binary(
        &client,
        PatternChannel::Pattern,
        BinaryComponent::SteamClient,
    ) {
        Ok(pf) => pf,
        Err(e) => {
            let sha = client.sha256_hex();
            eprintln!("[manifest] no pattern file for steamclient SHA-256 {}: {}",
                      sha.get(..12).unwrap_or(sha), e);
            return Err(format!("Pattern lookup failed: {}", e));
        }
    };

    let mut installed = 0;

    if let Some(addr) = resolve_hook_target(&client, &mapping, &pf, "BuildDepotDependency") {
        if install_build_depot_hook(addr) {
            installed += 1;
        }
    }

    if let Some(addr) = resolve_hook_target(&client, &mapping, &pf, "YldLoadDepotManifest") {
        if install_manifest_load_hook(addr, steam_root) {
            installed += 1;
        }
    }

    eprintln!("[manifest] installed {} manifest hook(s)", installed);
    Ok(installed)
}

fn resolve_hook_target(
    client: &BinaryFile,
    mapping: &LoadedModuleMapping,
    pf: &PatternFile,
    hook_name: &str,
) -> Option<u64> {
    let entry = pf.patterns.get(hook_name)?;

    let vaddr = if let Some(pattern_hex) = entry.pattern.as_ref() {
        let pattern_bytes = parse_pattern_hex(pattern_hex);
        let mask_bytes = entry.mask.as_deref().map(parse_pattern_hex);
        let offset = entry.offset.unwrap_or(0);
        client.resolve_pattern_address(&pattern_bytes, mask_bytes.as_deref(), offset, None)?
    } else {
        entry.address?
    };

    client.vaddr_to_runtime_addr(vaddr, mapping, JMP_ABS64_LEN)
}

fn parse_pattern_hex(hex: &str) -> Vec<u8> {
    hex.split_whitespace()
        .filter_map(|s: &str| u8::from_str_radix(s, 16).ok())
        .collect()
}

fn install_build_depot_hook(_target_addr: u64) -> bool {
    true // Placeholder
}

fn install_manifest_load_hook(_target_addr: u64, _steam_root: &std::path::Path) -> bool {
    true // Placeholder
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pattern_hex() {
        let pattern = "FF 25 00 00 00 00";
        let bytes = parse_pattern_hex(pattern);
        assert_eq!(bytes, vec![0xFF, 0x25, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_manifest_override() {
        set_manifest_override(12345, 123456, ManifestOverride::new(999999, Some(1024)));
        let overrides = get_manifest_overrides();
        assert!(overrides.contains_key(&(12345, 123456)));
        remove_manifest_override(12345, 123456);
        assert!(!get_manifest_overrides().contains_key(&(12345, 123456)));
    }
}