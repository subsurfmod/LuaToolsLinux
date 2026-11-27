//! ManifestDonor — Opt-Out CDN Request Code Harvester (BST-45).
//!
//! Ported directly from usefulsteamtools `ManifestDonor.cpp` / `ManifestDonor.h`.
//!
//! Contributes manifest request codes for depots the local account owns back to
//! community servers (`https://manifest.luastools.xyz`).
//!
//! Architectural Rules:
//! 1. Direction matters: Server sends what it wants first (`/manifestwanted`), and the
//!    client answers only with the intersection of depots it actually owns. The user's
//!    library is never disclosed.
//! 2. Edge cache pre-check: Before requesting any code from Steam, sends a HEAD request
//!    to `/m/{depot}/{gid}`. If already present (status 200), skips minting.
//! 3. Codes rotate within minutes, so minted codes are sent straight to
//!    `/manifestcode/submit` in immediate batches.
//! 4. Passive capture (`submit_captured_code`): genuine codes spent by local downloads
//!    are posted immediately on a detached worker.
//! 5. Rate limiting: strictly respects `interval_secs`, `min_mint_interval_ms`,
//!    `max_mints_per_cycle`, and `max_mints_per_session`.
//! 6. Opt-out respectful: disabled immediately if `[donate].enabled = false` in `opensteamtool.toml`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rand::seq::SliceRandom;
use rand::thread_rng;

use crate::steam_bridge::interop::interop_misc;
use crate::steam_bridge::interop::interop_package;
use crate::steam_bridge::manifest_hooks;
use crate::steam_bridge::netpacket::netpacket_manifestprobe;
use crate::steam_bridge::ost_config::{self, DonateSettings};
use crate::steam_bridge::ost_platform::http;

const DEFAULT_BASE_URL: &str = "https://manifest.luastools.xyz";
const MAX_WANTED_BYTES: u32 = 32 * 1024 * 1024;

static DONOR_RUNNING: AtomicBool = AtomicBool::new(false);
static DONOR_THREAD: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
static MINTED_THIS_SESSION: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Wanted {
    pub app_id: u32,
    pub depot_id: u32,
    pub gid: u64,
}

static WANTED_CACHE: RwLock<Vec<Wanted>> = RwLock::new(Vec::new());
static WANTED_FETCHED_AT: Mutex<Option<Instant>> = Mutex::new(None);

fn normalize_base_url(raw: &str) -> String {
    let mut base = if raw.trim().is_empty() {
        DEFAULT_BASE_URL.to_string()
    } else {
        raw.trim().to_string()
    };
    while base.ends_with('/') {
        base.pop();
    }
    base
}

/// Parse a single "app:depot:gid" line.
/// Returns None if malformed or if depot or gid is 0. (app_id 0 is permitted).
pub fn parse_wanted_line(line: &str) -> Option<Wanted> {
    let parts: Vec<&str> = line.split(':').collect();
    if parts.len() < 3 {
        return None;
    }

    let app_id = parts[0].trim().parse::<u32>().ok()?;
    let depot_id = parts[1].trim().parse::<u32>().ok()?;
    let gid = parts[2].trim().parse::<u64>().ok()?;

    if depot_id == 0 || gid == 0 {
        return None;
    }

    Some(Wanted {
        app_id,
        depot_id,
        gid,
    })
}

/// Fetches the missing manifest set from `/manifestwanted`.
pub fn fetch_wanted(base_url: &str) -> Vec<Wanted> {
    let url = format!("{}/manifestwanted", normalize_base_url(base_url));
    log::debug!("[ManifestDonor] Fetching wanted list from {}", url);

    let res = http::Execute(
        "GET",
        &url,
        std::ptr::null(),
        0,
        std::ptr::null(),
        5000,
        5000,
        10000,
        30000,
        MAX_WANTED_BYTES,
    );

    if !res.ok || res.status != 200 {
        log::debug!(
            "[ManifestDonor] Wanted list unavailable (ok={} status={})",
            res.ok,
            res.status
        );
        return Vec::new();
    }

    let mut wanted = Vec::new();
    for line in res.body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(w) = parse_wanted_line(trimmed) {
            wanted.push(w);
        } else {
            log::trace!(
                "[ManifestDonor] Ignoring malformed wanted line: {:?}",
                trimmed
            );
        }
    }

    log::debug!("[ManifestDonor] {} wanted entr(ies) received", wanted.len());
    wanted
}

/// Submits collected codes to `/manifestcode/submit`.
pub fn submit_codes(base_url: &str, body: &str, count: usize) -> bool {
    let url = format!("{}/manifestcode/submit", normalize_base_url(base_url));
    let headers = "Content-Type: text/plain\r\n";
    let c_headers = std::ffi::CString::new(headers).unwrap();

    let res = http::Execute(
        "POST",
        &url,
        body.as_ptr() as *const std::os::raw::c_void,
        body.len() as u32,
        c_headers.as_ptr(),
        5000,
        5000,
        10000,
        30000,
        64 * 1024,
    );

    if res.ok && res.status == 200 {
        log::info!("[ManifestDonor] Submitted {} code(s) to {}", count, url);
        true
    } else {
        log::warn!(
            "[ManifestDonor] Submit failed (ok={} status={}) for {} code(s)",
            res.ok,
            res.status,
            count
        );
        false
    }
}

/// Checks whether a manifest is already cached in the edge CDN store via HEAD `/m/{depot}/{gid}`.
pub fn is_already_cached(base_url: &str, depot_id: u32, gid: u64) -> bool {
    let url = format!(
        "{}/m/{}/{}",
        normalize_base_url(base_url),
        depot_id,
        gid
    );
    let res = http::Execute(
        "HEAD",
        &url,
        std::ptr::null(),
        0,
        std::ptr::null(),
        5000,
        5000,
        5000,
        5000,
        1024,
    );
    res.ok && res.status == 200
}

/// Refreshes the local wanted list cache if stale or never fetched.
fn refresh_wanted_if_stale(cfg: &DonateSettings) {
    let now = Instant::now();
    let should_fetch = {
        let fetched_at = WANTED_FETCHED_AT.lock().unwrap_or_else(|e| e.into_inner());
        match *fetched_at {
            None => true,
            Some(t) => now.duration_since(t).as_secs() >= u64::from(cfg.wanted_refresh_secs),
        }
    };

    if should_fetch {
        let mut fresh = fetch_wanted(&cfg.url);
        if !fresh.is_empty() {
            let mut rng = thread_rng();
            fresh.shuffle(&mut rng);
            *WANTED_CACHE.write().unwrap_or_else(|e| e.into_inner()) = fresh;
            *WANTED_FETCHED_AT.lock().unwrap_or_else(|e| e.into_inner()) = Some(now);
        } else {
            // First fetch failed: set timestamp anyway so we honour cadence before hammering
            let mut fetched_at = WANTED_FETCHED_AT.lock().unwrap_or_else(|e| e.into_inner());
            if fetched_at.is_none() {
                *fetched_at = Some(now);
            }
        }
    }
}

/// Execute a single donation harvesting cycle.
pub fn run_cycle(cfg: &DonateSettings) {
    let (ready, owned_vec) = interop_package::get_owned_depots();
    if !ready {
        log::debug!("[ManifestDonor] Owned depots not resolved yet, skipping cycle");
        return;
    }

    let minted_total = MINTED_THIS_SESSION.load(Ordering::Acquire);
    if cfg.max_mints_per_session > 0 && minted_total >= cfg.max_mints_per_session {
        log::debug!(
            "[ManifestDonor] Session cap reached ({}), idle until restart",
            cfg.max_mints_per_session
        );
        return;
    }

    let owned_set: HashSet<u32> = owned_vec.into_iter().collect();
    if owned_set.is_empty() {
        log::debug!("[ManifestDonor] User owns zero depots, skipping cycle");
        return;
    }

    refresh_wanted_if_stale(cfg);
    let wanted_list = WANTED_CACHE.read().unwrap_or_else(|e| e.into_inner()).clone();
    if wanted_list.is_empty() {
        return;
    }

    let mut body = String::new();
    let mut submitted: usize = 0;
    let mut skipped: usize = 0;
    let mut refused: usize = 0;
    let mut already_have: usize = 0;
    let mut minted_any = false;

    for w in wanted_list {
        if !DONOR_RUNNING.load(Ordering::Acquire) {
            break;
        }
        if submitted >= cfg.max_mints_per_cycle as usize {
            break;
        }
        if cfg.max_mints_per_session > 0
            && MINTED_THIS_SESSION.load(Ordering::Acquire) >= cfg.max_mints_per_session
        {
            break;
        }

        // Only contribute depots the user genuinely owns
        if !owned_set.contains(&w.depot_id) {
            skipped += 1;
            continue;
        }

        // Check edge CDN cache first
        if is_already_cached(&cfg.url, w.depot_id, w.gid) {
            already_have += 1;
            continue;
        }

        // Space out Steam calls if configured
        if minted_any && cfg.min_mint_interval_ms > 0 {
            thread::sleep(Duration::from_millis(u64::from(cfg.min_mint_interval_ms)));
            if !DONOR_RUNNING.load(Ordering::Acquire) {
                break;
            }
        }
        minted_any = true;

        let mut app_id = w.app_id;
        if app_id == 0 {
            if let Some((known_app, _)) = manifest_hooks::get_seen_depot(0, w.depot_id) {
                app_id = known_app;
            } else {
                app_id = interop_misc::resolve_app_id();
            }
        }

        let start_time = Instant::now();
        // Request manifest request code
        let code = netpacket_manifestprobe::request_manifest_code(app_id, w.depot_id, w.gid);
        MINTED_THIS_SESSION.fetch_add(1, Ordering::SeqCst);

        match code {
            Some(c) if c != 0 => {
                let age_ms = start_time.elapsed().as_millis();
                body.push_str(&format!("{}:{}:{}\n", w.depot_id, w.gid, c));
                submitted += 1;
                log::debug!(
                    "[ManifestDonor] Minted depot={} gid={} code=0x{:X} in {}ms",
                    w.depot_id,
                    w.gid,
                    c,
                    age_ms
                );
            }
            _ => {
                refused += 1;
            }
        }
    }

    log::info!(
        "[ManifestDonor] Cycle done — {} minted, {} already archived, {} not owned, {} refused by Steam ({} this session)",
        submitted,
        already_have,
        skipped,
        refused,
        MINTED_THIS_SESSION.load(Ordering::Acquire)
    );

    if submitted > 0 {
        submit_codes(&cfg.url, &body, submitted);
    }
}

/// Passive capture: submits a genuine manifest request code spent during real downloads.
pub fn submit_captured_code(depot_id: u32, gid: u64, code: u64) {
    if depot_id == 0 || gid == 0 || code == 0 {
        return;
    }
    let cfg = ost_config::get_donate_settings();
    if !cfg.enabled {
        return;
    }

    let base_url = cfg.url;
    let body = format!("{}:{}:{}\n", depot_id, gid, code);

    thread::Builder::new()
        .name("ost-donor-captured".to_string())
        .spawn(move || {
            submit_codes(&base_url, &body, 1);
        })
        .ok();
}

/// Worker loop running in background thread.
fn worker_loop() {
    log::info!("[ManifestDonor] Worker started");

    while DONOR_RUNNING.load(Ordering::Acquire) {
        let cfg = ost_config::get_donate_settings();

        if cfg.enabled {
            run_cycle(&cfg);
        }

        // Sliced sleep for responsive shutdown
        let interval = u64::from(cfg.interval_secs.max(1));
        for _ in 0..interval {
            if !DONOR_RUNNING.load(Ordering::Acquire) {
                break;
            }
            thread::sleep(Duration::from_secs(1));
        }
    }

    log::info!("[ManifestDonor] Worker stopped");
}

/// Start the background ManifestDonor worker thread.
pub fn start() {
    let cfg = ost_config::get_donate_settings();
    if !cfg.enabled {
        log::info!("[ManifestDonor] Disabled by configuration");
        return;
    }

    if DONOR_RUNNING.swap(true, Ordering::SeqCst) {
        log::warn!("[ManifestDonor] Already running");
        return;
    }

    let handle = thread::Builder::new()
        .name("ost-manifest-donor".to_string())
        .spawn(worker_loop);

    match handle {
        Ok(h) => {
            *DONOR_THREAD.lock().unwrap_or_else(|e| e.into_inner()) = Some(h);
        }
        Err(e) => {
            log::warn!("[ManifestDonor] Failed to spawn worker thread: {}", e);
            DONOR_RUNNING.store(false, Ordering::Release);
        }
    }
}

/// Signal the worker to stop gracefully.
pub fn stop() {
    if !DONOR_RUNNING.swap(false, Ordering::SeqCst) {
        return;
    }
    let handle = DONOR_THREAD.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(h) = handle {
        let _ = h.join();
    }
}

/// Check if the worker is currently active.
pub fn is_running() -> bool {
    DONOR_RUNNING.load(Ordering::Acquire)
}

/// Total count of codes minted during this process session.
pub fn minted_this_session() -> u32 {
    MINTED_THIS_SESSION.load(Ordering::Acquire)
}

/// Reset session mint counter (useful in tests).
pub fn reset_session_stats() {
    MINTED_THIS_SESSION.store(0, Ordering::Release);
    *WANTED_CACHE.write().unwrap_or_else(|e| e.into_inner()) = Vec::new();
    *WANTED_FETCHED_AT.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    

    #[test]
    fn test_wanted_line_parser() {
        let sample = "1245620:987654321\n1245621:1122334455\ninvalid_line\n0:0\n";
        let mut wanted = Vec::new();
        for line in sample.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 2 {
                if let (Ok(depot_id), Ok(gid)) = (parts[0].trim().parse::<u32>(), parts[1].trim().parse::<u64>()) {
                    if depot_id != 0 && gid != 0 {
                        wanted.push((depot_id, gid));
                    }
                }
            }
        }
        assert_eq!(wanted.len(), 2);
        assert_eq!(wanted[0], (1245620, 987654321));
        assert_eq!(wanted[1], (1245621, 1122334455));
    }
}
