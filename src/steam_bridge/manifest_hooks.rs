//! Manifest dependency hooks for Linux.
//!
//! Port of usefulsteamtools `Interop_Manifest.cpp` hooks:
//! - `BuildDepotDependency` - Patches depot entries' gid/size
//! - `YldLoadDepotManifest` - Pre-seeds manifest cache
//!
//! On Linux, these hooks are installed via pattern scanning in `libsteam_integration.so`
//! using 14-byte RIP-relative indirect jumps.

use std::collections::{HashMap, HashSet};
use std::os::raw::c_void;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use super::binary_reader::{BinaryFile, LoadedModuleMapping};
use super::memory_patch::{self, MemoryPatch, JMP_ABS64_LEN};
use super::pattern_db::{BinaryComponent, PatternChannel, PatternDb, PatternFile};

/// Single depot manifest entry (0x20 / 32 bytes) matching Steam's internal structure:
/// DepotId (0x00), AppId (0x04), ManifestGid (0x08), ManifestSize (0x10), DlcAppId (0x18),
/// LcsRequired (0x1C), bNotNewTarget (0x1D), SharedInstall (0x1E), Padding (0x1F).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepotEntry {
    pub depot_id: u32,
    pub app_id: u32,
    pub manifest_gid: u64,
    pub manifest_size: u64,
    pub dlc_app_id: u32,
    pub lcs_required: u8,
    pub b_not_new_target: u8,
    pub shared_install: u8,
    pub padding: u8,
}

impl DepotEntry {
    pub fn new(
        app_id: u32,
        depot_id: u32,
        manifest_gid: u64,
        manifest_size: u64,
        dlc_app_id: u32,
    ) -> Self {
        Self {
            depot_id,
            app_id,
            manifest_gid,
            manifest_size,
            dlc_app_id,
            lcs_required: 0,
            b_not_new_target: 0,
            shared_install: 0,
            padding: 0,
        }
    }
}

/// Valve Tier1 CUtlMemory structure
#[repr(C)]
#[derive(Debug)]
pub struct CUtlMemory<T> {
    pub memory: *mut T,
    pub allocation_count: i32,
    pub grow_size: i32,
}

/// Valve Tier1 CUtlVector structure
#[repr(C)]
#[derive(Debug)]
pub struct CUtlVector<T> {
    pub memory: CUtlMemory<T>,
    pub size: i32,
}

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

/// Function pointer type for `BuildDepotDependency`:
/// `bool BuildDepotDependency(void* pUserAppMgr, AppId_t AppId, void* pUserConfig, CUtlVector<DepotEntry>* pDepotInfo, CUtlVector<DepotEntry>* pSharedDepotInfo, void* pSteamApp, uint32* pBuildId, bool* pbBetaFallback)`
pub type BuildDepotDependencyFn = unsafe extern "C" fn(
    *mut c_void,                 // pUserAppMgr
    u32,                         // AppId
    *mut c_void,                 // pUserConfig
    *mut CUtlVector<DepotEntry>, // pDepotInfo
    *mut CUtlVector<DepotEntry>, // pSharedDepotInfo
    *mut c_void,                 // pSteamApp
    *mut u32,                    // pBuildId
    *mut bool,                   // pbBetaFallback
) -> bool;

/// Function pointer type for `YldLoadDepotManifest`:
/// `i64 YldLoadDepotManifest(void* a1, void* a2, int appId, uint32_t depotId, uint64_t manifestGid, const char* branch)`
pub type YldLoadDepotManifestFn = unsafe extern "C" fn(
    *mut c_void,                 // a1
    *mut c_void,                 // a2
    i32,                         // appId
    u32,                         // depotId
    u64,                         // manifestGid
    *const std::os::raw::c_char, // branch
) -> i64;

/// Storage for original function pointers.
static ORIGINAL_BUILD_DEPOT_DEPENDENCY: OnceLock<Mutex<Option<BuildDepotDependencyFn>>> =
    OnceLock::new();
static ORIGINAL_YLD_LOAD_DEPOT_MANIFEST: OnceLock<Mutex<Option<YldLoadDepotManifestFn>>> =
    OnceLock::new();

/// Storage for installed memory patches.
static INSTALLED_MANIFEST_PATCHES: OnceLock<Mutex<Vec<MemoryPatch>>> = OnceLock::new();

fn original_build_depot_dependency() -> &'static Mutex<Option<BuildDepotDependencyFn>> {
    ORIGINAL_BUILD_DEPOT_DEPENDENCY.get_or_init(|| Mutex::new(None))
}

fn original_yld_load_depot_manifest() -> &'static Mutex<Option<YldLoadDepotManifestFn>> {
    ORIGINAL_YLD_LOAD_DEPOT_MANIFEST.get_or_init(|| Mutex::new(None))
}

fn installed_manifest_patches() -> &'static Mutex<Vec<MemoryPatch>> {
    INSTALLED_MANIFEST_PATCHES.get_or_init(|| Mutex::new(Vec::new()))
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

pub const ARCHIVE_BASE_URL: &str = "https://manifest.luastools.xyz";
pub const MAX_MANIFEST_BODY_BYTES: usize = 64 * 1024 * 1024;
pub const MIN_MANIFEST_BODY_BYTES: usize = 16;
pub const PAYLOAD_MAGIC: [u8; 4] = [0xD0, 0x17, 0xF6, 0x71]; // 0x71F617D0
pub const EOF_MAGIC: [u8; 4] = [0xAB, 0x15, 0xC4, 0x32];     // 0x32C415AB

/// Validate manifest payload against Steam binary manifest magic headers:
/// starts with PAYLOAD_MAGIC (`0x71F617D0`) and ends with EOF_MAGIC (`0x32C415AB`).
pub fn looks_like_manifest(data: &[u8]) -> bool {
    if data.len() < MIN_MANIFEST_BODY_BYTES {
        return false;
    }
    data.starts_with(&PAYLOAD_MAGIC) && data.ends_with(&EOF_MAGIC)
}

/// Checks if a file exists on disk and is a valid binary manifest (BST-44).
/// If the file exists but fails validation, it is removed so a fresh copy can be fetched.
pub fn is_valid_cached_manifest(path: &Path) -> bool {
    if let Ok(data) = std::fs::read(path) {
        if looks_like_manifest(&data) {
            return true;
        }
        log::warn!(
            "[manifest] cached manifest at {:?} failed validation (magic/EOF mismatch or truncated), re-fetching",
            path
        );
        let _ = std::fs::remove_file(path);
    }
    false
}

static NEGATIVE_CACHE: OnceLock<Mutex<HashMap<(u32, u64), Instant>>> = OnceLock::new();
static IN_FLIGHT_MANIFESTS: OnceLock<Mutex<HashSet<(u32, u64)>>> = OnceLock::new();

fn negative_cache() -> &'static Mutex<HashMap<(u32, u64), Instant>> {
    NEGATIVE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn in_flight_manifests() -> &'static Mutex<HashSet<(u32, u64)>> {
    IN_FLIGHT_MANIFESTS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Clear negative cache (useful in tests or when new manifests are uploaded).
pub fn clear_manifest_negative_cache() {
    negative_cache().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Discover active Linux Steam installation root.
pub fn get_default_steam_root() -> Option<std::path::PathBuf> {
    if let Ok(val) = std::env::var("STEAM_ROOT") {
        let p = std::path::PathBuf::from(val);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let h = std::path::PathBuf::from(home);
        for rel in &[
            ".local/share/Steam",
            ".steam/steam",
            ".steam/root",
            ".var/app/com.valvesoftware.Steam/.local/share/Steam",
        ] {
            let p = h.join(rel);
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

/// Atomically write manifest payload to destination file (`<dest>.tmp.<pid>.<rand> -> rename`).
pub fn write_manifest_atomic(dest: &std::path::Path, body: &[u8]) -> bool {
    if let Some(parent) = dest.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            log::warn!("[manifest] failed to create parent dir {}: {}", parent.display(), e);
            return false;
        }
    }

    let pid = std::process::id();
    let tmp = dest.with_extension(format!("tmp.{}.{}", pid, rand_nanos()));
    {
        let mut file = match std::fs::File::create(&tmp) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("[manifest] cannot create temp {}: {}", tmp.display(), e);
                return false;
            }
        };
        use std::io::Write;
        if let Err(e) = file.write_all(body) {
            log::warn!("[manifest] write failed for {}: {}", tmp.display(), e);
            let _ = std::fs::remove_file(&tmp);
            return false;
        }
        if let Err(e) = file.sync_all() {
            log::warn!("[manifest] sync failed for {}: {}", tmp.display(), e);
            let _ = std::fs::remove_file(&tmp);
            return false;
        }
    }

    if let Err(e) = std::fs::rename(&tmp, dest) {
        log::warn!("[manifest] rename {} -> {} failed: {}", tmp.display(), dest.display(), e);
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    true
}

fn rand_nanos() -> u32 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(12345)
}

/// Pre-seed manifest cache check and on-demand fetch for a depot.
///
/// If `<steam_root>/depotcache/<depot>_<gid>.manifest` exists and is non-empty, returns `true`.
/// Otherwise, checks negative cache (TTL 10 min), acquires in-flight deduplication, and
/// fetches the `.manifest` file from `https://manifest.luastools.xyz/m/{depot}/{gid}`.
/// Validates payload magic (`0x71F617D0` .. `0x32C415AB`) and writes atomically to `depotcache`.
/// On 404 (definitive miss), records in negative cache, queues `report_missing_manifest`, and returns `false`.
pub fn ensure_manifest_cached(
    steam_root: &std::path::Path,
    app_id: u32,
    depot_id: u32,
    gid: u64,
) -> bool {
    ensure_manifest_cached_with_timeout(steam_root, app_id, depot_id, gid, 5000, 30000, false)
}

pub fn ensure_manifest_cached_with_timeout(
    steam_root: &std::path::Path,
    app_id: u32,
    depot_id: u32,
    gid: u64,
    connect_timeout_ms: u64,
    recv_timeout_ms: u64,
    bypass_negative_cache: bool,
) -> bool {
    if depot_id == 0 || gid == 0 {
        return false;
    }
    let manifest_path = steam_root
        .join("depotcache")
        .join(format!("{}_{}.manifest", depot_id, gid));

    let default_url = format!("{}/m/{}/{}", ARCHIVE_BASE_URL, depot_id, gid);
    ensure_manifest_cached_from_url(
        &manifest_path,
        &default_url,
        app_id,
        depot_id,
        gid,
        connect_timeout_ms,
        recv_timeout_ms,
        bypass_negative_cache,
    )
}

pub fn ensure_manifest_cached_from_url(
    manifest_path: &std::path::Path,
    url: &str,
    app_id: u32,
    depot_id: u32,
    gid: u64,
    connect_timeout_ms: u64,
    recv_timeout_ms: u64,
    bypass_negative_cache: bool,
) -> bool {
    if depot_id == 0 || gid == 0 {
        return false;
    }

    if is_valid_cached_manifest(manifest_path) {
        return true;
    }

    if !bypass_negative_cache {
        let neg = negative_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = neg.get(&(depot_id, gid)) {
            if t.elapsed() < Duration::from_secs(600) {
                return false;
            }
        }
    }

    {
        let mut in_flight = in_flight_manifests().lock().unwrap_or_else(|e| e.into_inner());
        if !in_flight.insert((depot_id, gid)) {
            return false;
        }
    }

    struct InFlightGuard(u32, u64);
    impl Drop for InFlightGuard {
        fn drop(&mut self) {
            in_flight_manifests()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&(self.0, self.1));
        }
    }
    let _guard = InFlightGuard(depot_id, gid);

    if is_valid_cached_manifest(manifest_path) {
        return true;
    }

    log::info!(
        "[manifest] pre-seed download: fetching app={} depot={} gid={} from {}",
        app_id,
        depot_id,
        gid,
        url
    );

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_millis(connect_timeout_ms.max(500)))
        .timeout_read(Duration::from_millis(recv_timeout_ms.max(1000)))
        .build();

    let call_res = agent.get(url).call();
    match call_res {
        Ok(resp) => {
            if resp.status() == 200 {
                use std::io::Read;
                let mut body = Vec::new();
                let mut reader = resp.into_reader().take((MAX_MANIFEST_BODY_BYTES + 1) as u64);
                if reader.read_to_end(&mut body).is_err() || body.len() > MAX_MANIFEST_BODY_BYTES {
                    log::warn!(
                        "[manifest] manifest for depot={} gid={} exceeded {} bytes or read failed",
                        depot_id,
                        gid,
                        MAX_MANIFEST_BODY_BYTES
                    );
                    return false;
                }

                if !looks_like_manifest(&body) {
                    log::warn!(
                        "[manifest] rejected body for depot={} gid={} ({} bytes): magic header/EOF mismatch",
                        depot_id,
                        gid,
                        body.len()
                    );
                    return false;
                }

                if write_manifest_atomic(manifest_path, &body) {
                    log::info!(
                        "[manifest] cached app={} depot={} gid={} ({} bytes) -> {}",
                        app_id,
                        depot_id,
                        gid,
                        body.len(),
                        manifest_path.display()
                    );
                    return true;
                }
                false
            } else {
                log::warn!(
                    "[manifest] unexpected HTTP status {} for depot={} gid={}",
                    resp.status(),
                    depot_id,
                    gid
                );
                false
            }
        }
        Err(ureq::Error::Status(404, _)) => {
            negative_cache()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert((depot_id, gid), Instant::now());
            log::info!(
                "[manifest] archive 404: app={} depot={} gid={} not found on remote",
                app_id,
                depot_id,
                gid
            );
            report_missing_manifest(depot_id, gid);
            false
        }
        Err(e) => {
            log::warn!(
                "[manifest] transient fetch failure for app={} depot={} gid={}: {}",
                app_id,
                depot_id,
                gid,
                e
            );
            false
        }
    }
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

/// Install manifest hooks in `steamclient.so`.
pub fn install_manifest_hooks(
    steam_root: &std::path::Path,
    mapping: &LoadedModuleMapping,
) -> Result<usize, String> {
    let expected_class = if cfg!(target_pointer_width = "64") {
        super::binary_reader::ElfClass::Class64
    } else {
        super::binary_reader::ElfClass::Class32
    };

    let binaries = super::binary_reader::scan_steam_binaries(steam_root);

    let steam_client = binaries.into_iter().find(|b| {
        b.class == expected_class
            && b.path
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
        Ok(pf) => {
            if pf.is_fallback() {
                log::warn!("[manifest] fallback patterns detected; skipping memory hooks");
                return Err("Fallback patterns not permitted for manifest hooks".to_string());
            }
            pf
        }
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
    if entry.is_generic_compiler_prologue() {
        log::warn!("[manifest] pattern for '{}' is generic compiler prologue; skipping", hook_name);
        return None;
    }

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

/// Replacement function for `BuildDepotDependency`.
/// Exact signature:
/// `bool BuildDepotDependency(void* pUserAppMgr, AppId_t AppId, void* pUserConfig, CUtlVector<DepotEntry>* pDepotInfo, CUtlVector<DepotEntry>* pSharedDepotInfo, void* pSteamApp, uint32* pBuildId, bool* pbBetaFallback)`
unsafe extern "C" fn build_depot_dependency_replacement(
    p_user_app_mgr: *mut c_void,
    app_id: u32,
    p_user_config: *mut c_void,
    p_depot_info: *mut CUtlVector<DepotEntry>,
    p_shared_depot_info: *mut CUtlVector<DepotEntry>,
    p_steam_app: *mut c_void,
    p_build_id: *mut u32,
    pb_beta_fallback: *mut bool,
) -> bool {
    panic::catch_unwind(AssertUnwindSafe(|| {
        let orig = *original_build_depot_dependency()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(orig_fn) = orig else {
            return false;
        };

        let result = orig_fn(
            p_user_app_mgr,
            app_id,
            p_user_config,
            p_depot_info,
            p_shared_depot_info,
            p_steam_app,
            p_build_id,
            pb_beta_fallback,
        );

        if !result {
            return false;
        }

        // Empty-depot PICS mid-refresh guard ("Instant-Complete" Bug Fix, lines 164-172):
        // When Package 0 injects thousands of Lua AppIDs at login, Steam runs a massive PICS
        // refresh during which an app's old depot list is briefly dropped before new appinfo
        // arrives (nCount = 0). Without protection, Steam writes an empty depot config and
        // stamps the game "Fully Installed" at 0 bytes.
        // Failing this call makes Steam bail on its clean failure path and keep the app's
        // previous depot config until appinfo finishes landing.
        if app_id != 0
            && (p_depot_info.is_null() || (*p_depot_info).size <= 0)
            && crate::steam_bridge::interop::interop_package::has_depot_ex(app_id, false)
        {
            log::warn!(
                "[manifest] BuildDepotDependency: app {} returned 0 depots (appinfo mid-refresh) - failing call so Steam retains existing depot config",
                app_id
            );
            return false;
        }

        // Record genuine depots seen before overrides
        if !p_depot_info.is_null() && (*p_depot_info).size > 0 && !(*p_depot_info).memory.memory.is_null() {
            let slice = std::slice::from_raw_parts((*p_depot_info).memory.memory, (*p_depot_info).size as usize);
            record_depots(Some(slice));
        }
        if !p_shared_depot_info.is_null() && (*p_shared_depot_info).size > 0 && !(*p_shared_depot_info).memory.memory.is_null() {
            let slice = std::slice::from_raw_parts((*p_shared_depot_info).memory.memory, (*p_shared_depot_info).size as usize);
            record_depots(Some(slice));
        }

        // Apply manifest overrides in place
        if !p_depot_info.is_null() && (*p_depot_info).size > 0 && !(*p_depot_info).memory.memory.is_null() {
            let slice = std::slice::from_raw_parts_mut(
                (*p_depot_info).memory.memory,
                (*p_depot_info).size as usize,
            );
            apply_depot_overrides(slice);
        }

        result
    }))
    .unwrap_or(false)
}

/// Replacement function for `YldLoadDepotManifest`.
/// Exact signature:
/// `i64 YldLoadDepotManifest(void* a1, void* a2, int appId, uint32_t depotId, uint64_t manifestGid, const char* branch)`
unsafe extern "C" fn yld_load_depot_manifest_replacement(
    a1: *mut c_void,
    a2: *mut c_void,
    app_id: i32,
    depot_id: u32,
    manifest_gid: u64,
    branch: *const std::os::raw::c_char,
) -> i64 {
    panic::catch_unwind(AssertUnwindSafe(|| {
        if depot_id != 0 && manifest_gid != 0 && crate::steam_bridge::interop::interop_package::has_depot(depot_id) {
            log::debug!(
                "[manifest] YldLoadDepotManifest: pre-seed app={} depot={} gid={}",
                app_id,
                depot_id,
                manifest_gid
            );
            let steam_root = std::env::var("STEAM_ROOT")
                .ok()
                .map(std::path::PathBuf::from)
                .or_else(get_default_steam_root);
            if let Some(ref root) = steam_root {
                ensure_manifest_cached(root, app_id as u32, depot_id, manifest_gid);
            }
        }

        let orig = *original_yld_load_depot_manifest()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(orig_fn) = orig {
            orig_fn(a1, a2, app_id, depot_id, manifest_gid, branch)
        } else {
            0
        }
    }))
    .unwrap_or(0)
}

fn install_build_depot_hook(target_addr: u64) -> bool {
    if target_addr == 0 {
        return false;
    }

    // Install the memory patch with pre-activation trampoline callback
    let patch = unsafe {
        memory_patch::patch_by_offset_with_callback(
            target_addr,
            0,
            build_depot_dependency_replacement as *const () as *mut u8,
            |tramp| {
                if tramp != 0 {
                    *original_build_depot_dependency()
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = Some(std::mem::transmute::<usize, BuildDepotDependencyFn>(
                            tramp,
                        ));
                }
            },
        )
    };

    match patch {
        Ok(patch) => {
            installed_manifest_patches()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(patch);
            log::info!("Installed BuildDepotDependency hook at 0x{:x}", target_addr);
            true
        }
        Err(e) => {
            log::warn!("Failed to install BuildDepotDependency hook: {}", e);
            false
        }
    }
}

fn install_manifest_load_hook(target_addr: u64, _steam_root: &std::path::Path) -> bool {
    if target_addr == 0 {
        return false;
    }

    // Install the memory patch with pre-activation trampoline callback
    let patch = unsafe {
        memory_patch::patch_by_offset_with_callback(
            target_addr,
            0,
            yld_load_depot_manifest_replacement as *const () as *mut u8,
            |tramp| {
                if tramp != 0 {
                    *original_yld_load_depot_manifest()
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = Some(std::mem::transmute::<usize, YldLoadDepotManifestFn>(
                            tramp,
                        ));
                }
            },
        )
    };

    match patch {
        Ok(patch) => {
            installed_manifest_patches()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(patch);
            log::info!("Installed YldLoadDepotManifest hook at 0x{:x}", target_addr);
            true
        }
        Err(e) => {
            log::warn!("Failed to install YldLoadDepotManifest hook: {}", e);
            false
        }
    }
}

/// Uninstall all manifest hooks and restore original functions.
pub fn uninstall_manifest_hooks() -> Result<(), String> {
    // Clear original function pointers
    *original_build_depot_dependency()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *original_yld_load_depot_manifest()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;

    // Uninstall all patches
    let mut patches = installed_manifest_patches()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for mut patch in patches.drain(..) {
        unsafe { patch.uninstall() }?;
    }
    Ok(())
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
            DepotEntry::new(12345, 123456, 111, 512, 0),
            DepotEntry::new(99999, 123456, 222, 256, 0),
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
    fn test_depot_entry_memory_layout_and_cutlvector() {
        assert_eq!(std::mem::size_of::<DepotEntry>(), 32);
        let entry = DepotEntry::new(1245620, 1245621, 987654321, 1024, 0);
        assert_eq!(entry.depot_id, 1245621);
        assert_eq!(entry.app_id, 1245620);
        assert_eq!(entry.manifest_gid, 987654321);
        assert_eq!(entry.manifest_size, 1024);

        // Test in-place vector patching via CUtlVector
        set_manifest_override(1245620, 1245621, ManifestOverride::new(1122334455, Some(2048)));
        let mut data = vec![entry];
        let cutl_vec = CUtlVector {
            memory: CUtlMemory {
                memory: data.as_mut_ptr(),
                allocation_count: 1,
                grow_size: 0,
            },
            size: 1,
        };

        unsafe {
            let slice = std::slice::from_raw_parts_mut(cutl_vec.memory.memory, cutl_vec.size as usize);
            apply_depot_overrides(slice);
        }

        assert_eq!(data[0].manifest_gid, 1122334455);
        assert_eq!(data[0].manifest_size, 2048);
        remove_manifest_override(1245620, 1245621);
    }

    #[test]
    fn test_ensure_manifest_cached_requires_valid_manifest_magic() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("depotcache");
        std::fs::create_dir_all(&cache).unwrap();

        let manifest_path = cache.join("100_200.manifest");
        std::fs::write(&manifest_path, b"").unwrap();
        assert!(!ensure_manifest_cached(tmp.path(), 10, 100, 200));

        // Arbitrary non-manifest bytes must be rejected and deleted
        std::fs::write(&manifest_path, b"not-a-valid-manifest-bytes").unwrap();
        assert!(!ensure_manifest_cached(tmp.path(), 10, 100, 200));
        assert!(!manifest_path.exists(), "Corrupted cached manifest should be removed");

        // Valid binary manifest bytes (PAYLOAD_MAGIC + padding + EOF_MAGIC) must be accepted
        let mut valid_bytes = Vec::new();
        valid_bytes.extend_from_slice(&PAYLOAD_MAGIC);
        valid_bytes.extend_from_slice(&[0u8; 16]);
        valid_bytes.extend_from_slice(&EOF_MAGIC);
        std::fs::write(&manifest_path, &valid_bytes).unwrap();
        assert!(ensure_manifest_cached(tmp.path(), 10, 100, 200));
        assert!(!ensure_manifest_cached(tmp.path(), 10, 0, 200));
    }
}
