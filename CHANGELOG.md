# Changelog

All notable changes to **LuaTools Linux** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [2.9.2-beta] - 2026-11-24

### Added & Fixed (Exhaustive Technical Parity with LuaTools & UsefulSteamTools Reference)
- **Process Lifecycle, Single-Instance & IPC (`src/main.rs`, `src/services/protocol_service.rs`, `src/services/steam_service.rs`)**:
  - Implemented single-instance mutual exclusion (`LT-01`) via `libc::flock(LOCK_EX | LOCK_NB)` on `$XDG_RUNTIME_DIR/luatools_instance_<uid>.lock`.
  - Implemented cross-instance Unix domain socket IPC (`LT-02`) forwarding `luatools://` URLs and focus requests to the primary GUI process.
  - Implemented `--minimized` and `--tray-locked` CLI startup flags (`LT-03`).
  - Fixed `ProtocolService` parsing of `/silent/` and query param `?silent=1|true` (`LT-12`, `LT-13`) and wired automatic background download/installation.
  - Hardened Steam lifecycle (`LT-18`): added `terminate_steam_instances()` and clean relaunch prioritizing `~/.local/bin/steam-sls` with `LD_AUDIT=SLSsteam.so`.
- **HTTP Server, DonateKeys Wire Format & Browser Plugin (`src/services/http_server.rs`, `src/services/donate_keys.rs`, `src/services/cef_bridge.rs`, `src/services/plugin_installer_service.rs`)**:
  - Implemented full CORS preflight (`OPTIONS`) handling (`LT-26`).
  - Implemented all core and extended store-plugin endpoints (`/has`, `/add`, `/remove`, `/open/fix`, `/open/settings`, `/rpc`, and 13+ extended routes) (`LT-27`, `LT-28`).
  - Fixed DonateKeys wire format (`LT-117`): payload now formatted as comma-separated `text/plain` `depotId:hexKey,...` to `http://167.235.229.108/donatekeys/send`.
  - Fixed LTSP store plugin installer (`LT-19`, `LT-23`): wired `luatools.js` deployment, fixed CDP `callServerMethod` parameter forwarding, and bypassed CEF CSP.
- **Tools, Fixes, Downloader & Extraction (`src/services/fixes_service.rs`, `src/services/lua_installer.rs`, `src/services/external_tools.rs`, `src/services/steamless_service.rs`, `src/services/depot_downloader.rs`)**:
  - Fixed `FixesService::is_safe_fix_id` to accept colons (`:`) for `online-fix:<uuid>` (`LT-95`) and configured `WINEDLLOVERRIDES` in Proton prefix registry.
  - Fixed bare `.lua` archive crash in `lua_installer.rs` (`LT-33`) by validating PK zip magic bytes before archive opening.
  - Fixed Steamless (`LT-106`): wait for process exit and replace target executable with `.unpacked.exe`.
  - Fixed Goldberg emulator (`LT-99`): downloads Windows `emu-win-release.7z` DLLs alongside Linux `.so` for Proton games.
  - Fixed DepotDownloader (`LT-89`): added `-manifestfile` switch support and deepened `DepotDownloaderMod` binary detection.
- **Steam Integration & SLSsteam Sync (`bridge_lib/src/lib.rs`, `assets/luatools-steam-launcher`, `install.sh`, `src/services/slssteam_service.rs`, `src/services/manifest_binding_service.rs`)**:
  - Fixed 32-bit Steam crash: provided `assets/luatools-steam-launcher` prioritizing `steam-sls` (`LD_AUDIT=SLSsteam.so`) and stripping 64-bit `libsteam_integration.so` from `LD_PRELOAD`.
  - Fixed Manifest Binding (`LT-86`): synchronized pins to SLSsteam `ManifestOverrides` in `~/.config/SLSsteam/config.yaml`.
  - Wired bridge readers for legacy CD keys, stats, and pinned manifests with C exports.

---

## [2.9.1-beta] - 2026-11-24

### Audited & Improved (All 78 Files Changed Since `v2.6.1`)
- **Services & Parsers (21 files — `src/parsers/lua_parser.rs`, `src/services/*.rs`)**:
  - **Parsing & Hashing**: Stripped UTF-8 BOM and fixed multi-dash comment toggling and empty-input newline handling in `lua_parser.rs`; replaced full-file `fs::read` (OOM risk on large game assets) with 64 KiB streaming single-pass SHA-256/SHA-1/MD5 hashing in `asset_hash.rs` and `file_hash.rs`.
  - **Networking, Proxies & Telemetry**: Fixed inverted Umami `website`/`hostname` payload fields in `analytics_service.rs`; replaced no-op DoH stub with a real TTL-cached JSON-over-HTTPS resolver in `doh_resolver.rs`; redacted Hubcap `api_key` from error strings, hardened `Content-Disposition` filename parsing (`OnceLock`, RFC 5987, path-traversal guard), and made downloads atomic via `.part` temp files in `api_client.rs`, `github_proxy.rs`, and `proxied_file_downloader.rs` (64 MiB cap + 10s connect timeouts).
  - **Fixes, Depots, Plugins & Tools**: Fixed `depot_downloader.rs` holding `bridge_service` mutex across child execution, switched depot keys delimiter to `;` with `0o600` unique temp files + RAII cleanup, and wired bridge hex depot keys; added path-traversal guards, backup pre-verification, and mid-apply rollback across `fixes_service.rs`, `plugin_add_service.rs`, and `plugin_installer_service.rs`; fixed Linux Wine/.NET invocation and `.unpacked.exe` replacement in `steamless_service.rs`; hardened `caches.rs` (atomic writes, placeholder SHA-256 rejection, poison-safe locks), `donate_keys.rs`, `external_tools.rs`, `mode_migration.rs`, `plugin_log.rs`, `steam_auto_crack_service.rs`, and `steam_bridge_service.rs`.
- **OSTPlatform Linux Backend (17 files — `src/steam_bridge/ost_platform/*.rs`)**:
  - **Memory, PE & Syscall Safety**: Fixed swapped PE32/PE64 optional header & section offsets, eliminated unaligned cast UB, and added checked arithmetic in `pe.rs`; fixed `dynamic_library.rs` `/proc/self/maps` parsing and `dlopen`/`dlclose` handle tracking; replaced raw `&'static str` pointer borrow with scoped `with_cstr` in `numbers.rs`; fixed `hash.rs` streaming reads and `sha256_of_process_memory` error handling; switched `directory_watch.rs` to `poll()` + `eventfd` with clean `Drop` thread join; fixed `trap.rs` Linux `SIGTRAP` `si_code` decoding and CAS handle validation; returned real `pthread_t` handles in `thread.rs`.
  - **Parsing, Process, Dialog & Credentials**: Added recursion/size/varint guards in `bkv_parser.rs`; reused single overlap buffer in `byte_search.rs`; prevented `zenity`/`yad` flag injection & added headless guard in `dialog.rs`; fixed surrogate-safe UTF-16 conversions in `encoding.rs`; hardened `http.rs`, `memory.rs`, `process.rs` (sub-second FILETIME + `setsid` reaper), `remote_process.rs` (ELF `e_machine` check + safe `LD_PRELOAD` builder), and `steam_credential_store.rs`.
- **Steam Bridge, Interop, NetPacket & Credentials (23 files — `src/steam_bridge/**`)**:
  - **Core Bridge & Tickets**: Fixed `ticket_store.rs` and `credentials/legacycdkey.rs` VDF quote/comment brace counting and in-place upserts (preserving existing Steam fields/nested blocks with atomic writes); fixed `credentials/eticketclient.rs` hex decoding; hardened `ipc.rs` (`1 MiB` frame cap, `0o600` socket, `RwLock` handler dispatch, 32-thread cap); added wildcard-aware `parse_pattern_and_mask()` and `(app_id, depot_id)` keying in `manifest_hooks.rs`; fixed `preload_loader.rs` `(deleted)` suffix stripping, `kill` PID safety, and shell escaping; fixed `cloud_redirect.rs`, `denuvo_auth.rs`, `token_bridge.rs`, and `mod.rs` state tracking.
  - **Interop & NetPacket**: Wrapped all `extern "C"` hooks and IPC callbacks in `catch_unwind`, fixed trampoline UAF and real ASLR address resolution in `interop_ipc.rs` and `interop_misc.rs`, fixed ownership/encrypted ticket bounds & probe eviction in `interop_ipc_isteamuser.rs`, wired `interop_ipc_isteamutils.rs` handlers, fixed varint overflow & response framing in `netpacket_eticket.rs`, `netpacket_ownershipticket.rs`, `netpacket_manifestprobe.rs`, and `userstats.rs`.
- **UI, Tests & Packaging (17 files — `src/ui/*.rs`, `tests/ui_visual_qa.rs`, `README.md`, `install.sh`, `uninstall.sh`, `packaging/*`)**:
  - Wired syntax highlighting & live validation (`validate_lua_text`) in `lua_editor.rs`; added `Esc` modal dismissal, input validation, `Copy` buttons, and Quick Setup sync across `dialogs.rs`; fixed offline/404 cover retry storms, top-bar pill width, and panic-free QA setup in `app.rs`; improved Home/Discover/Library/Depots/Settings/Guide UX in `views_primary.rs`, `views_workbench.rs`, and `views_guide.rs`; expanded workspace tests to **339 passing (0 warnings)**.
  - Fixed `install.sh` / `uninstall.sh` to install and remove `libsteam_integration.so` and `libsteam_auto_patch_linux.so` cleanly on both prebuilt and `--system` installs, and cleaned up `packaging/build-packages.sh` & `packaging/luatools-linux.spec`.
  - Reformatted `README.md` to match the Leonida layout with zero emojis while preserving all LuaTools Linux documentation and screenshots.

### Complete File Audit List (`v2.6.1` → `v2.9.1-beta`)
- **Config, Packaging & Docs (10)**:
  - Modified: `Cargo.toml`, `Cargo.lock`, `bridge_lib/Cargo.toml`, `steam_auto_patch_linux/Cargo.toml`, `install.sh`, `uninstall.sh`, `packaging/build-packages.sh`, `packaging/luatools-linux.spec`, `README.md`, `CHANGELOG.md`
- **Parsers & Services (21)**:
  - Modified: `src/parsers/lua_parser.rs`, `src/services/api_client.rs`, `src/services/caches.rs`, `src/services/depot_downloader.rs`, `src/services/donate_keys.rs`, `src/services/external_tools.rs`, `src/services/fixes_service.rs`, `src/services/mod.rs`, `src/services/steam_auto_crack_service.rs`, `src/services/steam_bridge_service.rs`
  - Added: `src/services/analytics_service.rs`, `src/services/asset_hash.rs`, `src/services/doh_resolver.rs`, `src/services/file_hash.rs`, `src/services/github_proxy.rs`, `src/services/mode_migration.rs`, `src/services/plugin_add_service.rs`, `src/services/plugin_installer_service.rs`, `src/services/plugin_log.rs`, `src/services/proxied_file_downloader.rs`, `src/services/steamless_service.rs`
- **Steam Bridge Core, Credentials, Interop & NetPacket (23)**:
  - Modified: `src/steam_bridge/ipc.rs`, `src/steam_bridge/mod.rs`, `src/steam_bridge/preload_loader.rs`, `src/steam_bridge/ticket_store.rs`
  - Added: `src/steam_bridge/cloud_redirect.rs`, `src/steam_bridge/denuvo_auth.rs`, `src/steam_bridge/manifest_hooks.rs`, `src/steam_bridge/token_bridge.rs`, `src/steam_bridge/credentials/appticket.rs`, `src/steam_bridge/credentials/eticketclient.rs`, `src/steam_bridge/credentials/legacycdkey.rs`, `src/steam_bridge/credentials/mod.rs`, `src/steam_bridge/interop/interop_ipc.rs`, `src/steam_bridge/interop/interop_ipc_isteamuser.rs`, `src/steam_bridge/interop/interop_ipc_isteamutils.rs`, `src/steam_bridge/interop/interop_misc.rs`, `src/steam_bridge/interop/ipc.rs`, `src/steam_bridge/interop/mod.rs`, `src/steam_bridge/netpacket/mod.rs`, `src/steam_bridge/netpacket/netpacket_eticket.rs`, `src/steam_bridge/netpacket/netpacket_manifestprobe.rs`, `src/steam_bridge/netpacket/netpacket_ownershipticket.rs`, `src/steam_bridge/netpacket/userstats.rs`
- **OSTPlatform Linux Backend (17)**:
  - Added: `src/steam_bridge/ost_platform/bkv_parser.rs`, `src/steam_bridge/ost_platform/byte_search.rs`, `src/steam_bridge/ost_platform/dialog.rs`, `src/steam_bridge/ost_platform/directory_watch.rs`, `src/steam_bridge/ost_platform/dynamic_library.rs`, `src/steam_bridge/ost_platform/encoding.rs`, `src/steam_bridge/ost_platform/hash.rs`, `src/steam_bridge/ost_platform/http.rs`, `src/steam_bridge/ost_platform/memory.rs`, `src/steam_bridge/ost_platform/mod.rs`, `src/steam_bridge/ost_platform/numbers.rs`, `src/steam_bridge/ost_platform/pe.rs`, `src/steam_bridge/ost_platform/process.rs`, `src/steam_bridge/ost_platform/remote_process.rs`, `src/steam_bridge/ost_platform/steam_credential_store.rs`, `src/steam_bridge/ost_platform/thread.rs`, `src/steam_bridge/ost_platform/trap.rs`
- **UI & Visual QA (8)**:
  - Modified: `src/ui/app.rs`, `src/ui/dialogs.rs`, `src/ui/mod.rs`, `src/ui/views_guide.rs`, `src/ui/views_primary.rs`, `src/ui/views_workbench.rs`, `tests/ui_visual_qa.rs`
  - Added: `src/ui/lua_editor.rs`

---

## [2.9.0-beta] - 2026-11-24

### Merged & Cleaned
- **Consolidated `beta/v2.7.3-beta`, `beta/v2.8.2-beta`, and `2.8.4-beta`**:
  - Preserved all `v2.7.3-beta` fixes (Steam cover image resolution via hashed `appdetails` + local `librarycache`, `Kill Steam` all-instance terminator, and hardened `bridge_lib/src/lib.rs`) alongside the new Linux interop, NetPacket, credentials, `TokenBridge`, and `OSTPlatform` modules.
  - Removed the accidentally committed duplicate `LuaToolsLinux-beta-v2.7.3-beta/` repository copy, `workflows/integrate-modules.json`, and 31 redundant/stray `.md` / `.txt` status dump files so only canonical repository docs (`README.md`, `CHANGELOG.md`, `LICENSE`) remain at root.

### Audited & Fixed
- **Restored `bridge_lib/src/lib.rs` & `steam_bridge_service.rs`**: Reverted the accidental truncation of `bridge_lib/src/lib.rs` (from 1,058 lines down to 98 lines) and broken imports in `src/services/steam_bridge_service.rs`, and restored `BridgeState` / `state()` / `set_integrated()` / `is_integrated()` in `src/steam_bridge/mod.rs`.
- **`OSTPlatform` Linux Modules (`pe.rs`, `hash.rs`, `numbers.rs`, `http.rs`, `dialog.rs`, `trap.rs`, `thread.rs`)**:
  - Eliminated double-free UB in `pe::ByteBuffer::new`, fixed infinite loop and missing address seek in `hash::sha256_of_process_memory`, fixed `numbers.rs` missing implementations & invalid `Self::`/UTF-16 byte pushes, migrated `http.rs` to valid `ureq 2.12` APIs (`send_bytes`, `request`, timeouts, `:port` URL parsing, UTF-8-safe truncation), fixed `dialog.rs` `Command::output()` `Result` matching & `zenity` flags, added missing `HANDLER` atomic + Box reclamation in `trap.rs`, and fixed `thread.rs` test assertion.
- **Interop, NetPacket, Credentials & TokenBridge (`interop/*`, `netpacket/*`, `credentials/*`, `ticket_store.rs`, `token_bridge.rs`)**:
  - Removed duplicate broken `call_original` transmute in `interop_ipc.rs`, replaced `fnv` dependency with `const fn fnv1a_hash`, switched handler callbacks to `Arc<dyn Fn>` + manual `Debug`, eliminated double `patch.install()` and non-`Send` raw pointer statics in `interop_misc.rs`, fixed response buffer slices in `interop_ipc_isteamuser.rs` so mutations write back to caller buffers, fixed inverted protobuf varint continuation bit in `netpacket_eticket.rs`, fixed `ManifestProbeState` static/thread-safety & 8-byte `jobid_target` reads in `netpacket_manifestprobe.rs`, fixed `TicketStore` methods pasted outside `impl TicketStore` + added `TicketStore::global()`, fixed `legacycdkey.rs` reserved keyword `override` & C++ syntax bugs, isolated `appticket.rs` tests from `$HOME`, and fixed `bst://` query-string parsing and handler dispatch in `token_bridge.rs`.

---

## [2.7.3-beta] - 2026-11-24

### Added
- **`Kill Steam` Button & All-Instance Steam Terminator (`src/steam_bridge/preload_loader.rs`, `src/ui/app.rs`, `src/ui/views_workbench.rs`)**:
  - Added a dedicated **`Kill Steam`** button directly beside **`Restart Steam w/ Bridge`** / **`Launch Steam w/ Bridge`** in the top-right bar and next to **`Restart Steam`** in **`Settings › General & Steam`**.
  - Scans `/proc` via `find_all_steam_pids()` / `kill_all_steam_instances()` to cleanly terminate all running Steam instances (`steam`, `steamwebhelper`, `steam.sh`, `steamservice`, `gameoverlayui`, `fossilize_replay`, `steam-runtime-*`, and Steam-scoped `reaper`/`srt-bwrap` wrappers) using `SIGTERM` with a 2-second grace window followed by `SIGKILL` for stubborn helpers — while strictly excluding `luatools`, debuggers, browsers, and unrelated apps.

### Fixed
- **Fixed Game Cover Images Showing AppIDs Instead of Artwork (`src/services/caches.rs`, `src/ui/app.rs`, `src/ui/views_primary.rs`)**:
  - Modern Steam Store titles serve hashed asset URLs (`<appid>/<sha1>/header.jpg`) that return HTTP `404` on legacy unhashed CDN paths. `CoverCache::ensure` now checks Steam's local `appcache/librarycache/` (including nested hashed directories), reads cached `details/<appid>.json`, sends a standard browser `User-Agent`, validates decoded image bytes (auto-purging corrupt cached files), and automatically falls back to Steam's lightweight `appdetails?filters=basic` endpoint to resolve modern hashed `header_image` / `capsule_image` URLs.
  - Wired featured (`large_capsule_image`), store search (`tiny_image`), and `AcquireDetailsLoaded` (`header_image`) URLs into `cover_known_urls` and an in-flight retry queue (`cover_retry_urls`) so newly loaded hashed cover URLs are never dropped when an initial unhashed request is already in flight.
  - Added a 45-second failure cooldown (`cover_failed_at`) to eliminate per-frame thread spam on Home when offline or for unresolvable AppIDs.
- **Resolved All 24 Compilation Errors, 47 Warnings & 8 Runtime Bugs from `beta/v2.7.2-alpha`**:
  - **Services (`steamless_service.rs`, `github_proxy.rs`, `proxied_file_downloader.rs`, `analytics_service.rs`, `depot_downloader.rs`)**: Fixed field/type mismatches (`install_path`, `PathBuf`), raw binary byte downloads in `ProxiedFileDownloader::download_bytes`, invalid `ureq::Response::new`/`Transport::new` calls, mid-stream mirror fallback cleanup in `GithubProxy::download_async`, and missing HTTP timeouts.
  - **OST Platform & Steam Bridge (`ost_platform/memory.rs`, `dynamic_library.rs`, `directory_watch.rs`, `steam_credential_store.rs`, `bkv_parser.rs`, `byte_search.rs`, `process.rs`, `encoding.rs`, `manifest_hooks.rs`, `cloud_redirect.rs`, `denuvo_auth.rs`)**: Fixed infinite loop in `memory::is_readable`, glibc `dlinfo(RTLD_DI_LINKMAP)` module base/directory lookup, non-blocking `inotify` watcher deadlock/early-exit, UB `static mut STORE` & partial-field overwrites in `steam_credential_store`, dropped child entries in `bkv_parser::read_object`, zero-padded offset corruption in `byte_search::find_in_file`, non-const `HashMap` statics in `manifest_hooks`, `&mut self` receiver mismatch in `CloudRedirectService::initialize`, and `/proc/<pid>/stat` parsing with spaces in `comm`.
  - **UI (`lua_editor.rs`, `views_workbench.rs`)**: Fixed shadowed `changed` flag and live re-parse state in `LuaEditor` and rendered the previously discarded install `ProgressBar` in `views_workbench.rs`.

---

## [2.6.1] - 2026-11-23

### Fixed
- **CRITICAL — Eliminated Steam `SIGABRT` Crash & `steamwebhelper` Interference in `libsteam_integration.so` (BUG-20241123-001)**:
  - **Fixed Swapped ELF64 Header Byte Offsets & Symbol-Table Overflow/Hang (`src/steam_bridge/binary_reader.rs`)**: `BinaryFile::open` previously read `e_shentsize` (`58..60`), `e_shnum` (`60..62`), and `e_shstrndx` (`62..64`) from swapped byte offsets, skipped the 64-byte header length check, used unchecked offset arithmetic, and looped up to `sec.size` times when `sec.entsize == 0` (triggering slice-bounds panics, arithmetic wrap, or OOM `SIGABRT` on real ELF64 `.so` libraries). All header, program-header, section-header, and symbol-table reads now use strict checked bounds, validate `entsize >= 24`, reject short pattern masks in `scan_range`, and skip `SHT_NOBITS` / non-allocated sections.
  - **Stopped `LD_PRELOAD` Activation in `steamwebhelper` & Child Processes (`bridge_lib/src/lib.rs`, `src/steam_bridge/preload_loader.rs`)**: `libsteam_integration_init()` now inspects `/proc/self/comm` and `/proc/self/exe` before spawning any thread and activates strictly inside the main `steam` client process (staying completely dormant in `steamwebhelper`, `sh`/`bash`, `luatools`, `srt-bwrap`, `reaper`, and launched games unless `LUATOOLS_BRIDGE_FORCE=1` is set). Also scoped `scan_steam_binaries()` to target Steam binaries (`steamclient.so`, `steamui.so`, `steam`) with canonical-path deduplication instead of loading every `*.so` (including 150+ MB `libcef.so`) into RAM.
  - **Fixed ASLR Runtime Address Translation & `mprotect`/`mmap` Validation (`src/steam_bridge/memory_patch.rs`, `src/steam_bridge/binary_reader.rs`, `bridge_lib/src/lib.rs`)**: Converted ELF virtual addresses using `steamclient.so`'s live load base from `/proc/self/maps` (`find_loaded_module_mapping` + `vaddr_to_runtime_addr`), polling briefly for `dlopen("steamclient.so")` after starting IPC. `MemoryPatch::install` now verifies `[target, target + len)` resides inside a mapped `r-x` executable page via `/proc/self/maps` before reading/patching, uses dynamic `sysconf(_SC_PAGESIZE)`, logs `errno` on `mmap`/`mprotect` failures, `munmap`s trampolines on error/uninstall/drop, and restores `PROT_READ | PROT_EXEC` (W^X) after patching.
  - **Added Panic Hook, `catch_unwind` Containment & Removed All `.unwrap()` in `bridge_lib` / `src/steam_bridge/` (`Cargo.toml`, `bridge_lib/src/lib.rs`, `src/steam_bridge/ipc.rs`)**: Switched release panic strategy from `abort` to `unwind`, installed a diagnostic panic hook writing to `stderr`, `/tmp/steam_integration_panic.log`, and `~/.local/share/luatools/telemetry/steam_integration_panic.log`, wrapped the background thread and every `extern "C"` hook and IPC handler in `std::panic::catch_unwind`, replaced all 40+ lock `.unwrap()` calls with poison-tolerant accessors, added safe no-op fallback logging when pattern TOMLs are missing, and capped ownership/encrypted ticket buffer sizes to 64 KiB.
- **Fixed `TicketStore` VDF Parsing & Upsert Data Corruption (`src/steam_bridge/ticket_store.rs`)**:
  - Fixed off-by-one bug in `split_kv()` (`line[key_end..]` instead of `line[key_end + 1..]`) which previously returned the whitespace separator instead of actual `SteamID` / `AppTicket` / `ETicket` values.
  - Fixed `upsert_app_block()` so updating an existing app replaces it in place (instead of silently deleting it) and inserting a new app writes it inside `"apps" { ... }` before the closing brace, with exact `"apps"` section matching (ignoring keys like `"steamapps"`).
- **Fixed `find_steam_pid()`, Launcher Trailing Colons & Steam Restart UX (`src/steam_bridge/preload_loader.rs`, `src/ui/app.rs`, `src/ui/views_workbench.rs`)**:
  - `find_steam_pid()` now matches only actual `steam` client processes instead of falsely matching `steamwebhelper`, `luatools-steam-launcher`, or `steam-sls`.
  - Fixed trailing-colon `LD_PRELOAD` / `LD_LIBRARY_PATH` in `luatools-steam-launcher` (`${LD_PRELOAD:+:$LD_PRELOAD}`) and added `--gdb` and `--lldb` interactive debugging flags.
  - `launch_steam_with_bridge()` now checks live Steam PID (not stale cached status), prefers `<steam_root>/steam.sh -shutdown`, waits up to 10 s for clean exit, detaches child stdio, and refreshes bridge status; `trigger_restart_steam()` falls back gracefully when Millennium RPC is unavailable.
  - Fixed `Settings › Steam Integration` system-preload checkbox being unconditionally reset to `false` on click, refreshed stale `libsteam_integration.so` atomically during `Install Integration` / self-update, cleared `cover_in_flight` on failed cover fetches, guarded corrupt `appinfo.vdf` string table offsets/counts, purged corrupt cached pattern TOMLs, and removed remaining UI/service `.unwrap()` panics (`views_workbench.rs`, `dialogs.rs`, `play_unowned_service.rs`, `steam_bridge_service.rs`).
- **Fixed Workspace Build, Install & Packaging Scripts (`Cargo.toml`, `Makefile`, `install.sh`, `uninstall.sh`, `packaging/`)**:
  - Added `[profile.debug-debug]` (`inherits = "release"`, `debug = 2`, `strip = false`) and built `libsteam_integration.so` with debug symbols by default for symbolicated `gdb` backtraces.
  - Updated `Makefile`, `install.sh`, `uninstall.sh`, `packaging/build-packages.sh`, and `packaging/luatools-linux.spec` to build/bundle/clean up the full workspace (`luatools` + `libsteam_integration.so` + `libsteam_auto_patch_linux.so`) and detect changes in `bridge_lib/` and `steam_auto_patch_linux/`.

---

## [2.6.0] - 2026-11-23

### Added
- **Native Linux `SteamAutoPatch` Engine & Goldberg Toolchain (`steam_auto_patch_linux/`, `src/services/steam_auto_crack_service.rs`, `src/services/external_tools.rs`)**:
  - Dedicated `steam-auto-patch-linux` workspace crate (`rlib` + `cdylib`) wired directly into `luatools-linux` (`SteamAutoCrackService`) for zero-Wine, zero-`.NET` Steamworks emulator deployment on Linux.
  - Deploys regular or experimental Goldberg Steam Emulator binaries across native Linux games (`libsteam_api.so` with automatic ELF32/ELF64 `e_ident[EI_CLASS]` architecture detection, `steam_api64.so`, `steam_api.so`) and Proton/Wine titles (`steam_api64.dll`, `steam_api.dll`) while preserving `.bak` backups of original game binaries.
  - Automatically provisions `steam_settings/` (`steam_appid.txt`, portable `configs.user.ini` local save overrides, and `steam_interfaces.txt` extracted directly from raw ELF `.so` and PE `.dll` binaries across all modern and legacy Steamworks interface prefixes).
  - Added one-click **Goldberg Emulator (Native Linux)** (`Detanup01/gbe_fork`) installer in **`Settings › Updates & Toolchain`** with multi-format archive extraction (`.tar.bz2`, `.tar.zst`, `.7z`, `.tar.xz`, `.tar.gz`, `.zip`), staging binaries into `~/.local/share/luatools/goldberg/` and adding `zstd`, `bzip2`, and `p7zip`/`7zip` runtime packages to `install.sh` and `packaging/luatools-linux.spec`.
- **Complete Steam Inlibrary Fix (`bridge_lib/src/lib.rs`)**:
  - `BIsSubscribedApp` hook returning `true` for configured/redirected apps, `GetAppId` hook mapping per-app spoof targets (`480`), `GetAppOwnershipTicket` & `RequestEncryptedAppTicket` / `GetEncryptedAppTicket` hooks backed by `TicketStore` (`localconfig.vdf`) with structured ownership/encrypted ticket synthesis, `GetSteamID` hook, `DepotKeyIntegration` 32-byte AES-256 key injection, and `FetchManifestCode` / `FetchManifestCodeEx` IPC handlers.
  - Automatic startup hydration in `libsteam_integration.so` loading `~/.local/share/luatools/bridge_overrides.json`, `manifest_pins.json`, `<steam_root>/config/stplug-in/*.lua`, and `localconfig.vdf` tickets — plus live reload on `lua_hot_reload` IPC events.
- **One-Click Game Setup & Top-Right Steam Launch Pill (`src/ui/dialogs.rs`, `src/ui/app.rs`)**:
  - Added **Quick Setup: Apply Recommended Fixes Now** at the top of **`Library › More › Integration Actions…`**, enabling Play-Unowned (Steam Inlibrary Fix), setting the `480` AppID online compatibility redirect, and applying native Goldberg `SteamAutoPatch` in one click.
  - Interactive top-right **`Launch Steam w/ Bridge`** / **`Restart Steam w/ Bridge`** / **`Install Bridge`** pill launching `~/.local/bin/luatools-steam-launcher` without root (gracefully shutting down any un-bridged Steam process first when restarting).
- **Illustrated `How to Use` Manual with Embedded App Screenshots (`src/ui/views_guide.rs`, `assets/guide/`)**:
  - Rewrote the in-app `How to Use` manual (`Ctrl+6` / `F1`) into 6 focused tabs (`1. Quick Start`, `2. Adding Games & .lua`, `3. Depots, Vault & Binding`, `4. One-Click Fixes & Library`, `5. Steam Interop, Wired & Cloud`, and `6. FAQ, Shortcuts & Bug Reports`) with a **Quick Reference — Common Tasks** table and 1-click workspace navigation buttons.
  - Embedded 6 high-DPI application screenshots (`01_home_overview.png` through `06_settings_wired_cloud.png`) lazily decoded into GPU textures on demand, complete with a **Click-to-Enlarge Preview** modal (`Esc` or click to close), and added a visual walkthrough to `README.md`.

### Fixed
- **Fixed Unresponsive `Apply Recommended Fixes Now` Button (`src/ui/dialogs.rs`)**: Wired `apply_quick_setup = true;` inside the button click handler (which previously only updated feedback text without executing the setup routine) and replaced unsupported `⚡` / `●` / `○` glyphs with crisp vector badges.
- **Fixed `steam_auto_patch_linux` Binary `.so`/`.dll` Interface Extraction & Path Bugs**:
  - Replaced `fs::read_to_string` (which failed with UTF-8 `InvalidData` on binary ELF/PE files) with lossy byte scanning over pre-compiled regexes reading the `.bak` original binary and writing to `<binary_dir>/steam_settings/steam_interfaces.txt`.
  - Fixed single-file extension checks (`libsteam_api.so`, `steam_api64.so`, `steam_api.so`, `steam_api64.dll`, `steam_api.dll`) with ELF32/ELF64 class detection and removed the non-root `[profile.release]` Cargo workspace warning.
- **Fixed `bridge_lib` Ticket Buffer Safety & `DepotKeyIntegration` Warnings**: Zero-initialized and populated ownership/encrypted ticket buffers instead of returning `true` on uninitialized caller buffers, tracked async `RequestEncryptedAppTicket` handles, decoded 64-char hex depot keys into 32-byte AES buffers, and resolved all compiler & Clippy `-D warnings` diagnostics.
- **Fixed Top-Bar Pill Width & Modal Backdrop Contrast (`src/ui/app.rs`, `src/ui/dialogs.rs`)**: Widened the top-right `StripBuilder` column so `Restart Steam w/ Bridge` never clips, wired `Install Bridge` to open `Settings › Steam Integration`, and placed modal windows on `Order::Foreground` above a dark backdrop scrim.

---

## [2.5.1] - 2026-11-23

### Added
- **Complete GUI Integration for the Steam Client Interop Layer**:
  - **`Settings › Steam Integration` Sub-Tab (`src/ui/views_workbench.rs`)**: Live Steam process & IPC detection badges, one-click `libsteam_integration.so` staging/build & `luatools-steam-launcher` generation, `LD_PRELOAD` mode toggle (per-launch wrapper vs. system-wide `/etc/ld.so.preload`), ELF64 `steamclient.so` / `steamui.so` SHA-256 & pattern scanner card, and `inotify` `stplug-in/` Hot-Reload controls + event log.
  - **`Library › More › Integration Actions…` & Process Integration Modal (`src/ui/dialogs.rs`)**: Per-game Play-Unowned redirect set toggle, AppID redirect (`480` spoof) editor, legacy CD key & 64-char hex depot decryption key controls, Steam stat overrides, and per-app `LD_PRELOAD` Game Process Integration (enable/disable, `.so` library file picker, custom launch args, and **Launch with Integration**).
  - **`Depots & Vault › Manifest Binding` Sub-Tab (`src/ui/views_workbench.rs`)**: Lists pinned depot manifests (auto-imported from `.lua` `setManifestid` directives + manual pins), adds/removes pins, re-syncs pins over Unix socket IPC, and displays live **`Active (Bound)`** vs. **`Pending (Saved)`** status badges.
  - **Home Hot-Reload & Bridge Status Strip (`src/ui/views_primary.rs`)**: Live `inotify` watcher running/paused badge, latest `.lua` file change readout, and 1-click Start/Stop toggle on Home.
  - **Cloud Redirect Revival Card (`Settings › SLSsteam & Store Bridge`)**: Live `CloudRedirect` status badge (`Active` / `Installed` / `Not Installed`), quick AppID adder, per-game cloud sync checkboxes, and **Sync to CloudRedirect** (`cloudredirect.conf` writer + IPC notification).
- **Automatic `libsteam_integration.so` Workspace Build & Staging**:
  - `install.sh`, `UpdateService::pull_and_update`, and `SteamBridgeService::ensure_integration_lib_staged` now build the full Cargo workspace (`luatools` + `bridge_lib`) and stage `libsteam_integration.so` into `~/.local/lib/luatools/libsteam_integration.so` automatically.
- **Steam Client Interop Layer (`src/steam_bridge/` & `bridge_lib/`)** (from `2.4.0-beta`, hardened for `2.5.1`):
  - **`binary_reader.rs`**: ELF64 section, symbol, dynamic-symbol, and SHA-256 reader for `steamclient.so` and `steamui.so`.
  - **`memory_patch.rs`**: 14-byte 64-bit RIP-relative indirect jump trampolines (`FF 25 00 00 00 00 <u64>`) safe across full 64-bit ASLR address space.
  - **`preload_loader.rs`**, **`ticket_store.rs`**, **`pattern_db.rs`**, **`ipc.rs`**, and **`bridge_lib`** (`cdylib` hooking `BIsSubscribedApp` & `GetAppId` and serving Unix socket IPC).

### Fixed
- **Fixed `memory_patch.rs` Double-Install & 32-bit ASLR Relocation Overflow**: Replaced `0xE9` signed 32-bit displacements (which overflow > ±2 GiB between `mmap` trampolines and shared libraries under Linux ASLR) with 14-byte 64-bit indirect jumps, and removed the duplicate `patch.install(14)` call in `bridge_lib` after `patch_by_offset` had already installed the patch.
- **Fixed `IpcServer::start` `EADDRINUSE` Crash (`src/steam_bridge/ipc.rs`)**: Cloned the bound `UnixListener` into the accept thread instead of re-binding the already-bound Unix socket path, added read/write timeouts + `Shutdown::Write` framing, and added `RemoveStat`, `RemoveLegacyCdKey`, and `RemoveDepotKey` IPC messages.
- **Fixed `BinaryFile` Section Name Indexing & Slice Bounds (`src/steam_bridge/binary_reader.rs`)**: Indexed `section_names` by section table index rather than `.shstrtab` byte offset (`name_offset`) so named section scans resolve `.text`/`.rodata` accurately, and clamped section ranges against truncated/`SHT_NOBITS` sections.
- **Fixed Stubbed IPC & Persistence in Services**:
  - `ManifestBindingService` now sends `SetManifestPin` / `RemoveManifestPin` (instead of `Ping`), tracks `ManifestBound` confirmation, imports `.lua` `setManifestid` entries, and persists pins to `manifest_pins.json`.
  - `HotReloadService` uses non-blocking `inotify` polling so `stop()` exits cleanly within 150 ms instead of blocking indefinitely, maintains a 30-event ring buffer, and emits `lua_hot_reload` IPC events.
  - `CloudRedirectService` and `GameProcessInjectionService` now persist configurations across restarts and isolate unit tests from `$HOME`.
- **Resolved All 18 Workspace Compiler / Clippy Errors & Cargo Profile Warning**: Cleaned up unused imports/variables, `missing_safety_doc`, `type_complexity`, `new_without_default`, `manual_strip`, `collapsible_if`, and non-root `[profile.release]` workspace warning across `luatools-linux` and `bridge_lib`.

---

## [2.3.1] - 2026-11-23

### Added
- **Native Wired Suite Installer (`WiredService`, `src/services/wired_service.rs`)**:
  - Installs and manages **ACCELA** (`~/.local/share/ACCELA` + `~/.config/Tachibana Labs/ACCELA.conf`) and **SLSsteam** (`~/.local/share/SLSsteam/SLSsteam.so`, `library-inject.so`, `~/.config/SLSsteam/config.yaml` defaults, and `~/.local/bin/steam-sls` `LD_AUDIT` wrapper) natively in Rust without executing remote `curl | bash` pipes.
  - Supports upstream `ciscosweater/enter-the-wired` (`deps.tar.gz` / `ACCELAINSTALL` / `.AppImage`) and `AceSLS/SLSsteam` release assets (`.7z` via `7z`/`7zz`/`7za` and `.tar.zst` via `tar --zstd`, plus `.tar.gz` and `.zip`).
  - Added **Wired Suite (Native ACCELA & SLSsteam Installer)** card in **`Settings › SLSsteam & Store Bridge`** with live installation badges, download/extract progress bar, **Install Both (Wired)**, **Install SLSsteam**, **Install ACCELA**, **Launch ACCELA**, and **Uninstall Wired**.
- **Steam Library ACF Registration (`SteamLibraryService`, `src/services/steam_library_service.rs`)**:
  - Added **`+ Add to Steam Library`** in **Discover** and **`Register / Unregister in Steam Library (ACF)`** in **`Library › More`**.
  - Generates valid Valve KeyValues `"AppState"` `appmanifest_<appid>.acf` manifests (`"Universe" "1"`, `"StateFlags" "4"`), creates `<steamapps>/common/<installdir>`, updates `libraryfolders.vdf` `"apps"` blocks, and registers `AdditionalApps` + `FakeAppId: 480` in `SLSsteam`.
- **Multi-App `PlayUnownedService` (`src/services/play_unowned_service.rs`)**:
  - Generates and syncs `~/.local/share/Steam/config/stplug-in/play_unowned_games.lua` across all managed `AdditionalApps` using a multi-app `UNOWNED_APPS` Lua table and nil-safe `type(steam_api) == "table"` guards.
  - Controllable globally via **`Settings › SLSsteam & Store Bridge › Sync PlayUnowned Plugin Now`** and per-game via **`Library › More › Enable / Disable Play Unowned`**.
- **`SteamAutoCrackService` (`SteamAutoPatch`) Integration (`src/services/steam_auto_crack_service.rs`)**:
  - Added per-game **`Run SteamAutoPatch`** action in **`Library › More`** (alongside folder selection in **`Settings › Updates & Toolchain`**), automatically invoking Linux `dotnet` or `wine` wrappers via `ExternalToolsManager`.
- **Compile-Time Build Commit SHA Lag Detection (`build.rs`, `src/services/update_service.rs`)**:
  - Embeds `LUATOOLS_BUILD_GIT_SHA` at compile time so `UpdateService::check_once` detects when the running binary is older than local or remote Git `HEAD` (both by SemVer and by `BUILD_GIT_SHA..origin/main` commit distance), even when the local checkout already has `HEAD == origin/main`.
  - Updated the **Local Repo** input hint in **`Settings › Updates & Toolchain`** to display the actual auto-detected repository path on disk.

### Fixed
- **Resolved All `beta/v2.2.0-beta` Compilation Errors**:
  - Removed duplicate `cef_bridge`, `http_server`, and `tools_manager` struct initializers (`E0062`) in `LuaToolsApp::new` (`src/ui/app.rs`).
  - Fixed undeclared `steam_installation` and `slssteam_service_clone` variables (`E0425`) and missing `wired_service` / `steam_auto_crack` fields (`E0063`) in `LuaToolsApp::new_headless_demo`.
  - Registered missing `pub mod steam_auto_crack_service;` in `src/services/mod.rs` and fixed calls to non-existent `ExternalToolsManager::get_tool_path` / `launch_steamautocrack` methods.
- **Eliminated `SLSsteam` `config.yaml` Shared-State Desync**:
  - Removed isolated `Arc<Mutex<SlssteamService>>` clones inside `PlayUnownedService` and `SteamLibraryService` so all services mutate `&mut app.slssteam_service` directly without overwriting `config.yaml` with stale in-memory copies.
- **Fixed `WiredService` Upstream Downloads, Extraction & Detection**:
  - Added mandatory `User-Agent` headers to all GitHub API requests (fixing HTTP `403 Forbidden` responses).
  - Fixed `AceSLS/SLSsteam` asset selection failing on `.7z` / `.tar.zst` releases, fixed `.tar.bz2`/`.tar.zst` archives previously being fed into `GzDecoder`, fixed `is_accela_installed()` always returning `false`, added ZipSlip path-traversal validation to archive extraction, and isolated unit tests from `$HOME`.
- **Fixed `SteamLibraryService` VDF/ACF Corruption Bugs**:
  - Replaced invalid `appmanifest_<appid>\n{` headers (rejected by Steam on startup) with standard `"AppState"` KeyValues format.
  - Fixed literal `"\\n"` string escape bug in `add_to_library_folders` that prevented `libraryfolders.vdf` `"apps"` insertion or appended invalid top-level blocks, and fixed `read_app_name` regex failing on multi-line `.acf` files.
- **Fixed `PlayUnownedService` Multi-Game Overwrite & Nil Global Crash**:
  - Fixed `enable_for_app` overwriting `play_unowned_games.lua` with a single `%APP_ID%` (which previously erased earlier games), removed invalid `addappid(0, 1, "placeholder")` declarations, and guarded `steam_api` indexing for Lua runtimes that only expose `addappid` / `setManifestid`.

---

## [2.1.0] - 2026-11-23

### Added
- **Interactive `How to Use` Manual Workspace (`src/ui/views_guide.rs`, `Ctrl+6` / `F1`)**:
  - 5-part interactive guide (`1. Quick Start`, `2. Adding Games & .lua`, `3. Depots, Vault & Downloads`, `4. Fixes, SLSsteam & Launch`, and `5. Bridge, FAQ & Bug Reports`) featuring syntax-highlighted `.lua` directive examples, Linux path reference cards, keyboard shortcuts table, troubleshooting recipes, and 1-click workspace navigation buttons.
- **Privacy-First Bug-Fixing Telemetry, Consent Popup & Diagnostic Exporter (`src/services/telemetry_service.rs`, `src/ui/dialogs.rs`)**:
  - **First-Launch Consent Modal**: Explains what diagnostic metadata is collected vs. never collected, with immediate **`Turn Off Telemetry`**, **`Keep Enabled`**, and **`Telemetry Settings ›`** actions.
  - **Automatic PII Scrubbing**: Redacts `$HOME` and `/home/<user>` paths to `~`, `smm_*` Hubcap API keys, 64-character hex depot decryption keys, JWTs, `Bearer` tokens, and email addresses before writing to disk or exporting.
  - **Crash & Warning Ring-Buffer**: Stores up to 200 scrubbed events in `~/.local/share/luatools/telemetry/events.jsonl` (`0600` permissions) and installs a chained Rust panic hook (`last_panic.txt`) capturing `file:line:col` crash locations.
  - **`Settings › Telemetry & Logs` Sub-Tab**: Master opt-out checkbox, **Review Privacy Notice** popup button, **Clear Stored Data**, live event log table, 1-click **Report Bug on GitHub ›** (pre-filled issue), **Copy / Export Diagnostic Markdown** (UTF-8 char-boundary safe), and optional HTTPS JSON / Discord webhook collector URL with **Test Endpoint**.
- **Depot Decryption Key File Support (`-depotkeys`) & `-manifestfile` (`src/services/depot_downloader.rs`)**:
  - Automatically collects per-app `.lua` and `SLSsteam` 64-character hex depot keys into a restricted (`0600`) temporary keys file for `DepotDownloader` / `DepotDownloaderMod` and deletes it immediately after download completion.

### Fixed
- Fixed `DepotDownloaderService::build_download_args` unit-test argument mismatch (`E0061`).
- Restored live `lua.tools` `/api/denuvo/*` fix catalog & download endpoints in `src/services/api_client.rs` (replacing 404 `/api/drm_fix/*` paths while retaining refactored `DRMFix*` domain structs).
- Wired live `SteamInstallation` and `SlssteamService` state into `DepotDownloaderService`, filtered keyless/manifestless DLC entitlement AppIDs out of depot download selections, and normalized progress percentages across tool updates and depot downloads.
- Restored `SteamAutoCrack` and `Steamless` executable discovery candidates & legacy tool subdirectories in `ExternalToolsManager` alongside neutral `SteamAutoPatch` / `StubRemover` naming, and added `#[serde(alias = "cef_injector_enabled")]` backwards compatibility in `AppSettings`.
- Fixed UTF-8 / short-key slice panic in `format_key_preview` (`src/ui/dialogs.rs`), switched binary `appinfo.vdf` entry size parsing from signed `i32` to `u32` (`src/parsers/vdf_binary.rs`), synced `SLSsteam` on `LuaVault` variant apply, honored `FastFetch` and `luatools://` source pre-selection, and resolved all `cargo clippy --all-targets -- -D warnings` lints.

---

## [2.0-beta] - 2026-11-23 — Complete Native Rust Rewrite (Beta)

### Added
- **100% Native Rust Architecture**: Ground-up rewrite replacing Python 3 + PyQt6 + `httpx` + `PyYAML` + `websockets` + `Pillow` + `vdf` with a single stripped native Rust binary (`eframe`/`egui` + `ureq`/`rustls` + `tungstenite` + `tiny_http`).
- **One-Shot Background Git Update Check & Corner Self-Updater**:
  - `UpdateService` (`src/services/update_service.rs`) runs a single non-blocking Git remote check on startup against `origin` (`https://github.com/subsurfmod/LuaToolsLinux.git` default).
  - Displays a compact **`⬆ Update`** button in the **top-right corner** when new commits or a newer version are available; clicking pulls from Git, compiles a stripped release binary, atomically replaces the installed executable via `rename()` (avoiding Linux `ETXTBSY`), and switches to a 1-click **`↻ Restart`** button (also available via `luatools --update`).
- **Turnkey Installation & Native Linux Packaging**:
  - Universal `install.sh` supporting Fedora/RHEL (`dnf`), Ubuntu/Debian (`apt`), Arch/SteamOS (`pacman`), and openSUSE (`zypper`), automatic `rustup` bootstrap if needed, SVG icon + `.desktop` launcher installation, and `luatools://` `xdg-mime` registration.
  - `packaging/build-packages.sh` & `packaging/luatools-linux.spec` building portable `.tar.gz`, native `.rpm`, and native `.deb` packages.
- **Official `SteamRE/DepotDownloader` Crash Hardening (`src/services/depot_downloader.rs`)**:
  - Removed unsupported CLI flags, added `.NET` runtime stability guards (`stdin=null`, `DOTNET_EnableWriteXorExecute=0`, `DOTNET_EnableDiagnostics=0`, `DOTNET_DbgEnableMiniDump=0`, `RLIMIT_CORE=0`, `-max-downloads 8`), preferred framework-dependent `DepotDownloader.dll` when host `dotnet` is installed on modern `glibc` / `OpenSSL` distros, and added 1-click terminal QR / Steam Guard session login (`-remember-password`).
- **Papercut Monotone-Dark UI, Top-Right Toast Notifications & Rich Typography**:
  - Non-blocking top-right dismissible toast notifications, tactile layered construction-paper monotone dark palette (`#0C0D10` / `#101115` / `#181A20`), `STIX Two Text Bold` display headings + `Open Sans` body copy + monospace code editors, `StripBuilder` + `egui::Sides` zero-overlap layouts, binary `appinfo.vdf` (v27–v29) Launch Options editor, global Quick Command Bar (`Ctrl+K` / `Ctrl+L`), `.lua`/`.manifest`/`.zip` drag-and-drop, XWayland (`WINIT_UNIX_BACKEND=x11`) stability guard, and headless software rasterizer visual QA (`tests/ui_visual_qa.rs`).
- **Full 100% On-Disk Compatibility with v1.x**:
  - Seamlessly reads and writes `~/.config/luatools/settings.json`, `~/.local/share/luatools/auth.json`, `~/.local/share/luatools/luavault/`, `~/.config/SLSsteam/config.yaml`, `~/.local/share/Steam/config/stplug-in/`, and `.luatools-fix/` SHA-256 backup records.

### Fixed & Hardened (Carried Forward from v1.1 Audit)
- Eliminated recursive mutex deadlock in `SteamDepotInfo` using deduplicated `Condvar` flight tracking.
- Fixed `SteamLinkParser` so numeric words in game titles (e.g. `"Warhammer 40000"`, `"Battlefield 2042"`) trigger Store Search instead of being misparsed as AppIDs.
- Fixed `SLSsteam` `AdditionalApps` YAML sequence serialization, `libraryfolders.vdf` primary library deduplication, ZipSlip archive traversal protection, and separation of 64-char hex depot decryption keys vs. app tokens.

---

## [1.0.0] - 2024-11-23

### Added
- Initial Python/PyQt6 release of LuaTools Linux (`Games`, `Depots`, `Fixes`, `External Tools`, `LuaVault`, `LuaFileParser`, `SteamDepotInfo`, `DepotDownloaderService`, `SLSsteam` YAML sync).
