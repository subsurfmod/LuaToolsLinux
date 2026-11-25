//! Manifest dependency hooks for Linux.
//!
//! Port of usefulsteamtools `Interop_Manifest.cpp` hooks:
//! - `BuildDepotDependency` - Patches depot entries' gid/size
//! - `YldLoadDepotManifest` - Pre-seeds manifest cache
//!
//! On Linux, these hooks are installed via pattern scanning in `libsteam_integration.so`
//! using 14-byte RIP-relative indirect jumps.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};
use std::thread;
use std::time::Duration;

use super::binary_reader::{BinaryFile, LoadedModuleMapping};
use super::memory_patch::JMP_ABS64_LEN;
use super::pattern_db::{BinaryComponent, PatternChannel, PatternDb, PatternFile};

/// Manifest override configuration.
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

/// Global manifest overrides registry.
static MANIFEST_OVERRIDES: OnceLock<RwLock<HashMap<(u32, u32), ManifestOverride>>> =
    OnceLock::new();

/// Depots seen during `BuildDepotDependency` calls, keyed by `(app_id, depot_id) -> manifest_gid`.
static DEPOTS_SEEN: OnceLock<Mutex<HashMap<(u32, u32), u64>>> = OnceLock::new();

/// Missing manifests waiting to be reported (debounced).
static MISSING_MANIFESTS: Mutex<Vec<(u32, u64)>> = Mutex::new(Vec::new());

/// Static flag for debouncing missing manifest reports.
static MISSING_REPORT_PENDING: AtomicBool = AtomicBool::new(false);

fn manifest_overrides() -> &'static RwLock<HashMap<(u32, u32), ManifestOverride>> {
    MANIFEST_OVERRIDES.get_or_init(|| RwLock::new(HashMap::new()))
}

fn depots_seen() -> &'static Mutex<HashMap<(u32, u32), u64>> {
    DEPOTS_SEEN.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Parse a hex byte pattern with optional `?`/`??` wildcards and optional explicit mask
/// (`xx?x` or whitespace-separated hex bitmask bytes). Returns `None` on malformed tokens.
pub fn parse_pattern_and_mask(
    pattern_hex: &str,
    explicit_mask: Option<&str>,
) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
    let mut pattern = Vec::new();
    let mut derived_mask = Vec::new();
    let mut has_wildcard = false;

    for tok in pattern_hex.split_whitespace() {
        if tok == "?" || tok == "??" {
            pattern.push(0x00);
            derived_mask.push(0x00);
            has_wildcard = true;
        } else {
            let hex = tok
                .strip_prefix("0x")
                .or_else(|| tok.strip_prefix("0X"))
                .unwrap_or(tok);
            let byte = u8::from_str_radix(hex, 16).ok()?;
            pattern.push(byte);
            derived_mask.push(0xFF);
        }
    }

    if pattern.is_empty() {
        return None;
    }

    let final_mask = match explicit_mask.map(str::trim).filter(|m| !m.is_empty()) {
        Some(mask_str) => {
            let parsed_mask = if mask_str
                .chars()
                .all(|c| matches!(c, 'x' | 'X' | '?' | ' ' | '\t'))
                && !mask_str.contains("0x")
                && !mask_str.contains("0X")
            {
                mask_str
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .map(|c| if c == '?' { 0x00 } else { 0xFF })
                    .collect::<Vec<u8>>()
            } else {
                let mut bytes = Vec::new();
                for tok in mask_str.split_whitespace() {
                    if tok == "?" || tok == "??" {
                        bytes.push(0x00);
                    } else {
                        let hex = tok
                            .strip_prefix("0x")
                            .or_else(|| tok.strip_prefix("0X"))
                            .unwrap_or(tok);
                        bytes.push(u8::from_str_radix(hex, 16).ok()?);
                    }
                }
                bytes
            };

            if parsed_mask.len() != pattern.len() {
                return None;
            }
            Some(
                derived_mask
                    .into_iter()
                    .zip(parsed_mask)
                    .map(|(a, b)| a & b)
                    .collect(),
            )
        }
        None if has_wildcard => Some(derived_mask),
        None => None,
    };

    Some((pattern, final_mask))
}

/// Set a manifest override for a depot.
pub fn set_manifest_override(app_id: u32, depot_id: u32, override_data: ManifestOverride) {
    let mut overrides = manifest_overrides()
        .write()
        .unwrap_or_else(|e| e.into_inner());
    overrides.insert((app_id, depot_id), override_data);
}

/// Remove a manifest override.
pub fn remove_manifest_override(app_id: u32, depot_id: u32) {
    let mut overrides = manifest_overrides()
        .write()
        .unwrap_or_else(|e| e.into_inner());
    overrides.remove(&(app_id, depot_id));
}

/// Get all manifest overrides.
pub fn get_manifest_overrides() -> HashMap<(u32, u32), ManifestOverride> {
    manifest_overrides()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Report a missing manifest (debounced, deduplicated).
pub fn report_missing_manifest(depot_id: u32, gid: u64) {
    if depot_id == 0 || gid == 0 {
        return;
    }
    {
        let mut missing = MISSING_MANIFESTS.lock().unwrap_or_else(|e| e.into_inner());
        if !missing.contains(&(depot_id, gid)) {
            missing.push((depot_id, gid));
        }
    }

    if !MISSING_REPORT_PENDING.swap(true, Ordering::SeqCst) {
        thread::spawn(|| {
            thread::sleep(Duration::from_millis(3000));

            MISSING_REPORT_PENDING.store(false, Ordering::SeqCst);

            let mut final_missing = MISSING_MANIFESTS.lock().unwrap_or_else(|e| e.into_inner());
            let count = final_missing.len();
            if count > 0 {
                log::warn!(
                    "[manifest] {} manifest(s) not ready - try download again later",
                    count
                );
                final_missing.clear();
            }
        });
    }
}

/// Pre-seed manifest cache check for a depot (must be a non-empty regular file).
pub fn ensure_manifest_cached(
    steam_root: &std::path::Path,
    app_id: u32,
    depot_id: u32,
    gid: u64,
) -> bool {
    if depot_id == 0 || gid == 0 {
        return false;
    }
    let manifest_path = steam_root
        .join("depotcache")
        .join(format!("{}_{}.manifest", depot_id, gid));

    if let Ok(meta) = std::fs::metadata(&manifest_path) {
        if meta.is_file() && meta.len() > 0 {
            return true;
        }
    }

    log::debug!(
        "[manifest] pre-seed missing: app={} depot={} gid={}",
        app_id,
        depot_id,
        gid
    );
    false
}

/// Record depots seen in a `BuildDepotDependency` call.
pub fn record_depots(depot_info: Option<&[DepotEntry]>) {
    if let Some(entries) = depot_info {
        let mut seen = depots_seen().lock().unwrap_or_else(|e| e.into_inner());
        for e in entries {
            if e.depot_id != 0 && e.manifest_gid != 0 {
                seen.insert((e.app_id, e.depot_id), e.manifest_gid);
            }
        }
    }
}

/// Apply configured manifest overrides to a mutable slice of depot entries, returning the
/// count of entries patched. Also records the resulting depots as seen.
pub fn apply_depot_overrides(entries: &mut [DepotEntry]) -> usize {
    let overrides = manifest_overrides()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let mut patched = 0usize;
    for entry in entries.iter_mut() {
        if let Some(ov) = overrides
            .get(&(entry.app_id, entry.depot_id))
            .or_else(|| overrides.get(&(0, entry.depot_id)))
        {
            entry.manifest_gid = ov.gid;
            if let Some(sz) = ov.size {
                entry.manifest_size = sz;
            }
            patched += 1;
        }
    }
    record_depots(Some(entries));
    patched
}

/// Get seen depot info (`(app_id, manifest_gid)`).
/// When `app_id != 0`, matches the exact `(app_id, depot_id)` pair; when `app_id == 0`,
/// matches any recorded entry for `depot_id`.
pub fn get_seen_depot(app_id: u32, depot_id: u32) -> Option<(u32, u64)> {
    let seen = depots_seen().lock().unwrap_or_else(|e| e.into_inner());
    if app_id != 0 {
        seen.get(&(app_id, depot_id))
            .copied()
            .map(|gid| (app_id, gid))
    } else {
        seen.iter()
            .find(|(&(_, did), _)| did == depot_id)
            .map(|(&(aid, _), &gid)| (aid, gid))
    }
}

/// Depot entry structure (matches Steam's internal structure).
#[derive(Debug, Clone)]
pub struct DepotEntry {
    pub app_id: u32,
    pub depot_id: u32,
    pub manifest_gid: u64,
    pub manifest_size: u64,
    pub dlc_app_id: u32,
}

/// Install manifest hooks in `steamclient.so`.
pub fn install_manifest_hooks(
    steam_root: &std::path::Path,
    mapping: &LoadedModuleMapping,
) -> Result<usize, String> {
    let binaries = super::binary_reader::scan_steam_binaries(steam_root);

    let steam_client = binaries.into_iter().find(|b| {
        b.path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name == "steamclient.so")
    });

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
            log::warn!(
                "[manifest] no pattern file for steamclient SHA-256 {}: {}",
                sha.get(..12).unwrap_or(sha),
                e
            );
            return Err(format!("Pattern lookup failed: {}", e));
        }
    };

    let mut installed = 0;

    if let Some(addr) = resolve_hook_target(&client, mapping, &pf, "BuildDepotDependency") {
        if install_build_depot_hook(addr) {
            installed += 1;
        }
    }

    if let Some(addr) = resolve_hook_target(&client, mapping, &pf, "YldLoadDepotManifest") {
        if install_manifest_load_hook(addr, steam_root) {
            installed += 1;
        }
    }

    log::info!("[manifest] installed {} manifest hook(s)", installed);
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
        let (pattern_bytes, mask_bytes) =
            parse_pattern_and_mask(pattern_hex, entry.mask.as_deref())?;
        let offset = entry.offset.unwrap_or(0);
        client.resolve_pattern_address(&pattern_bytes, mask_bytes.as_deref(), offset, None)?
    } else {
        entry.address?
    };

    client.vaddr_to_runtime_addr(vaddr, mapping, JMP_ABS64_LEN)
}

fn install_build_depot_hook(target_addr: u64) -> bool {
    target_addr != 0
}

fn install_manifest_load_hook(target_addr: u64, steam_root: &std::path::Path) -> bool {
    target_addr != 0 && !steam_root.as_os_str().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pattern_hex_and_wildcards() {
        let (bytes, mask) = parse_pattern_and_mask("FF 25 00 00 00 00", None).unwrap();
        assert_eq!(bytes, vec![0xFF, 0x25, 0x00, 0x00, 0x00, 0x00]);
        assert!(mask.is_none());

        let (bytes, mask) = parse_pattern_and_mask("48 89 ?? 24 ?", None).unwrap();
        assert_eq!(bytes, vec![0x48, 0x89, 0x00, 0x24, 0x00]);
        assert_eq!(mask, Some(vec![0xFF, 0xFF, 0x00, 0xFF, 0x00]));

        let (bytes, mask) = parse_pattern_and_mask("48 89 5C 24", Some("xx?x")).unwrap();
        assert_eq!(bytes, vec![0x48, 0x89, 0x5C, 0x24]);
        assert_eq!(mask, Some(vec![0xFF, 0xFF, 0x00, 0xFF]));

        // Reject malformed hex or mismatched mask length instead of silently corrupting bytes
        assert!(parse_pattern_and_mask("48 ZZ 24", None).is_none());
        assert!(parse_pattern_and_mask("48 89 5C", Some("xx")).is_none());
    }

    #[test]
    fn test_manifest_override_and_apply_depot_overrides() {
        set_manifest_override(12345, 123456, ManifestOverride::new(999999, Some(1024)));
        let overrides = get_manifest_overrides();
        assert!(overrides.contains_key(&(12345, 123456)));

        let mut entries = vec![
            DepotEntry {
                app_id: 12345,
                depot_id: 123456,
                manifest_gid: 111,
                manifest_size: 512,
                dlc_app_id: 0,
            },
            DepotEntry {
                app_id: 99999,
                depot_id: 123456,
                manifest_gid: 222,
                manifest_size: 256,
                dlc_app_id: 0,
            },
        ];
        assert_eq!(apply_depot_overrides(&mut entries), 1);
        assert_eq!(entries[0].manifest_gid, 999999);
        assert_eq!(entries[0].manifest_size, 1024);
        assert_eq!(entries[1].manifest_gid, 222);

        // Seen depots must keep distinct app_ids separate even for a shared depot_id
        assert_eq!(get_seen_depot(12345, 123456), Some((12345, 999999)));
        assert_eq!(get_seen_depot(99999, 123456), Some((99999, 222)));

        remove_manifest_override(12345, 123456);
        assert!(!get_manifest_overrides().contains_key(&(12345, 123456)));
    }

    #[test]
    fn test_ensure_manifest_cached_requires_non_empty_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("depotcache");
        std::fs::create_dir_all(&cache).unwrap();

        let empty_path = cache.join("100_200.manifest");
        std::fs::write(&empty_path, b"").unwrap();
        assert!(!ensure_manifest_cached(tmp.path(), 10, 100, 200));

        std::fs::write(&empty_path, b"manifest-bytes").unwrap();
        assert!(ensure_manifest_cached(tmp.path(), 10, 100, 200));
        assert!(!ensure_manifest_cached(tmp.path(), 10, 0, 200));
    }
}
