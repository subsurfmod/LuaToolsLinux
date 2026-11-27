# Changelog

All notable changes to **LuaTools Linux** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [4.0.0] - 2026-11-27

> **Major Release (`v4.0.0`)**: Reaches full production maturity across the native Rust/`egui` desktop app, dual-architecture (`i686` 32-bit + `x86_64` 64-bit) Linux Steam bridge, live streaming self-updater, authoritative Steam lifecycle management, and hardened sandboxed Lua 5.4 execution — verified with **603 passing workspace tests**.

### Highlights
- **Live Self-Updater & Unified UI Feedback**: Built a true streaming self-updater progress bar with dynamic crate estimation, monotonic progress, SHA-256 GitHub release digest verification, and a live dark monospace Rust compiler feed (`egui::ScrollArea` sticky auto-scroll) in both the updater modal and **Settings › Updates & Tools**. Added a unified `ActionProgressModal` and audited every button across the app for immediate in-progress and completion feedback with zero silent clicks.
- **Authoritative Steam Lifecycle & Topbar Controls**: Upgraded Steam PID detection to verify authoritative PID lockfiles (`~/.steam/steam.pid`, Flatpak) with non-zombie and `cmdline` validation; topbar now consistently displays `Restart Steam` and `Kill Steam` whenever Steam is active, with graceful `-shutdown` (timeout + `SIGKILL` fallback, Flatpak support) and crash-diagnosing detached spawn.
- **Dual-Arch Steam Bridge, NetPacket & Protocol Parity**: Completed `ELFCLASS32`/`ELFCLASS64` binary pattern scanning and `ElfClass` filtering, POSIX `SIGTRAP` SysV `ucontext_t` trap handling, full `BST-46` KV1 schema resolution, `CMsgProtoBufHeader` NetPacket manifest/cloud/probe codecs, `PersonaStateMsg` LEB128 varint tags, `CMsgClientGetUserStatsResponse` protobuf encoding with live `drain_achievements` delivery, local-filesystem `CloudRedirectHost` RPCs, active-user `LegacyCdKeyStore` (`0600` sidecar), and `bst://refreshachievements/<appid|all>`.
- **Packaging & Distribution**: Shipped native `.deb`, `.rpm`, and portable `.tar.gz` release bundles alongside dual-arch `dist/32` & `dist/64` bridge libraries and full GNU GPL-3.0 `LICENSE` text.

### Added & Improved
- **Store Plugin, Millennium & CEF (`http_server.rs`, `plugin_add_service.rs`, `millennium_service.rs`, `cef_bridge.rs`)**: Completed `:6767` store-plugin routes, headless `PluginAddService` pipeline, `MillenniumService` Linux detection, and CEF `:8080` debug probe.
- **Core Services & Verification**: Wired `DownloadQueue`, `DohResolver`, `ModeMigration`, `ManifestFile`/AES key validation, `AppliedFixIndexService`, and GitHub release SHA-256 digest verification across `update_service` and `wired_service`.
- **Lua 5.4 Sandbox & Bridge Directives (`lua_parser.rs`, `lua_installer.rs`, `bridge_lib`)**: Hardened `mlua` 5.4 VM straight-line execution, `SafeSsrfResolver`, `fetch_manifest_code(_ex)`, and `bridge_directives.json` propagation.

### Removed
- **UI Localization Removed**: Removed UI localization (`localization.rs`, `en.json`, `zh.json`) in favor of a direct, streamlined English UI across all views, dialogs, and workbench controls.

---

## [3.3.11] - 2026-11-26

### Fixed
- **Memory patching (`memory_patch.rs`, `netpacket_core.rs`)**: Removed `Clone` from `MemoryPatch` and moved patches into `NETPACKET_PATCHES` to eliminate use-after-unmap and double-`munmap`; published trampolines before writing JMPs; wrapped FFI hooks in `catch_unwind`; fixed IA-32 32-bit JMP encoding and gated 64-bit `LD_PRELOAD` out of 32-bit Steam.
- **Lua VM (`lua_parser.rs`)**: Fixed deterministic `Arc::try_unwrap` panic in `execute_lua_script`; materialized `LuaContents.entries`; added 2-arg `addappid`, 2-arg `setStat`, and platform directives; restricted stdlib (`TABLE | STRING | MATH | UTF8`) with 16 MiB memory and 500k instruction limits; added SSRF filtering, timeouts, and `headers_tbl` to `http_get`/`http_post`; wired dynamic scripts into `LuaFileParser::parse_text`.
- **Valve binary KV1 (`keyvalues1.rs`, `userstats.rs`, `cloud_redirect_service.rs`, `bridge_lib`)**: Fixed bare `0x08`/`0x0B` end markers, `0x04` 4-byte LE `Pointer`, `0x05` NUL-terminated UTF-16LE `WideString`, and `0x0A` `Int64`; added `write_kv1` serializer and real Steam bitmask/schema achievement extraction; wired KV1 across `userstats`, `cloud_redirect_service`, and `bridge_lib`.
- **Services & CLI**: Added headless `0600` file fallback to `EncryptionService::init_global()` (`credential_encryption.rs`); fixed LTSP repo slug (`madoiscool/LTSP`), `User-Agent`, fallback, and canonical install path (`plugin_installer_service.rs`); fixed `--silent` CLI flag (`main.rs`); fixed `LocalizationService` load order (`localization.rs`); moved `DepotDownloader` stdout reading off the timeout thread (`depot_downloader.rs`); fixed `bst://refreshachievements/<appid>` parsing (`token_bridge.rs`); rejected leading `/` paths in `7z` screening (`wired_service.rs`).
- **Workspace & Repo**: Synchronized version (`3.3.11`) and license (`GPL-3.0-or-later`) across workspace crates and packaging scripts; purged legacy branches, old tags/releases, and stale build artifacts.

---

## [3.2.9] - 2026-11-26

### Automatic Proton/Depot Switching & Platform Compatibility
- **Lua Platform Compatibility Fields** (`src/parsers/lua_parser.rs`): Added `os`, `arch`, `proton`, and `native` fields to `LuaEntry` struct with `matches_platform()`, `get_proton_version()`, and `is_native_linux()` helper methods.
- **New Lua Directives** (`src/parsers/lua_parser.rs`): Implemented `setOS(appid, "linux|windows|macos")`, `setArch(appid, "x86_64|arm64")`, `setProton(appid, "GE-Proton-9-25|Proton-Experimental|Proton-Stable")`, and `setNative(appid)` directives for Lua configs.
- **Platform-Filtered Depot Injection** (`bridge_lib/src/lib.rs`): Modified `get_all_depot_ids()` to filter cached Lua entries by target platform (`LUATOOLS_TARGET_OS`/`LUATOOLS_TARGET_ARCH` env vars set by `steam-sls` wrapper). Added `CachedLuaEntry` struct with platform metadata.
- **Steam Launcher Platform Env Vars** (`src/services/wired_service.rs`): Updated `steam-sls` wrapper to export `LUATOOLS_TARGET_OS` and `LUATOOLS_TARGET_ARCH` environment variables.
- **Bridge Lib Platform Initialization** (`bridge_lib/src/lib.rs`): Added `set_target_platform()` called from `run_integration()` to read env vars and set target platform for depot filtering.
- **DepotDownloader OS/Arch Support** (`src/services/depot_downloader.rs`): Added `arch` field to `DepotSelection` and `-os`/`-arch` flags to download arguments.
- **Depot Info Arch Field** (`src/services/depot_info.rs`, `src/ui/app.rs`, `src/ui/views_workbench.rs`): Added `arch` field to `ContentDepot` struct and updated all initializers.
- **C Export for Platform Setting** (`bridge_lib/src/lib.rs`): Added `set_target_platform_c()` C export for external callers.
- **Version Bump**: Updated to v3.2.9 to avoid updater breakage.

---

## [3.2.8] - 2026-11-26

### Package 0 License Synthesis & Depot Download Fixes
- **Package 0 DepotIdVec Population** (`src/steam_bridge/interop/interop_package.rs`): Fixed `init_fake_license_once()` and `notify_license_changed()` to populate BOTH `AppIdVec` (for license ownership checks) AND `DepotIdVec` (for Steam's content system to recognize downloadable depots). This fixes the "0B install" issue where games showed as owned but files couldn't be downloaded.
- **Enhanced `has_depot()` Check** (`src/steam_bridge/interop/interop_package.rs`): Now checks both the owned depots cache AND Package 0's DepotIdVec directly, ensuring Lua-unlocked depots are recognized as downloadable.
- **Cached Lua Entries** (`bridge_lib/src/lib.rs`): Added `update_cached_lua_entries()` during `load_persisted_state()` to collect all depot IDs from parsed `stplug-in/*.lua` files, ensuring `get_all_depot_ids()` includes all Lua-configured depots.
- **7z Path Traversal Fix** (`src/services/wired_service.rs`): Fixed false positive rejection of legitimate files like `docs/LICENSE` by only matching actual path traversal patterns (`../`, `..\`, `/../`, `\..\`, `..` at start/end) instead of any `..` substring.
- **DepotDownloader `-remember-password` Fix** (`src/services/depot_downloader.rs`): Fixed conditional logic - now only includes `-remember-password` when a username is provided; for anonymous login uses only `-qr` flag.
- **Version Bump**: Updated to v3.2.8 to avoid updater breakage.

---

## [3.2.7] - 2026-11-25

All notable changes to **LuaTools Linux** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [3.2.7] - 2026-11-25

### Steam Launcher Fixes & Depot Download Improvements
- **Steam Launcher LD_PRELOAD Export** (`src/steam_bridge/preload_loader.rs`): Fixed launcher script to properly export `LD_PRELOAD` with our integration library (`libsteam_integration.so`). Previously `NEW_LD_PRELOAD` was computed but never exported, causing the bridge library not to load.
- **LD_AUDIT Conflict Resolution** (`src/steam_bridge/preload_loader.rs`): Only use `LD_AUDIT` with SLSsteam libraries when not using `LD_PRELOAD` for our integration, preventing "wrong ELF class: ELFCLASS64" crashes on 32-bit Steam wrappers.
- **DepotDownloader Auth Error Handling** (`src/services/depot_downloader.rs`): Added handling for `SteamKit2.AsyncJobFailedException` and authentication failures with clear user guidance to click 'Sign In (QR)'.
- **UI Steam State Button** (`src/ui/app.rs`): Top-right button now correctly shows "Launch Steam" / "Restart Steam" / "Setup Steam" / "Rescan Steam" based on Steam running state and integration readiness. "Kill Steam" button available in Integration tab.
- **Version Bump**: Updated to v3.2.7 to avoid updater breakage.

---

## [3.2.6] - 2026-11-25

### v3.2.2 Audit Complete Fix & Steamless Native Integration
- **DepotDownloader `-remember-password` Fix** (`src/services/depot_downloader.rs`): Fixed conditional logic to always include `-remember-password` alongside both `-username` and `-qr` flags, preventing "`-remember-password can not be used without -username or -qr`" error.
- **Manifest URL Protocol Guard** (`src/services/download_queue.rs`): Added early rejection of `manifest://` URLs in generic download queue with clear error message directing to dedicated manifest download functions (HubcapService/LuaToolsApiClient).
- **AppliedFixRecord game_dir Deserialization** (`src/services/applied_fix_index.rs`, `src/services/fixes_service.rs`): Added custom deserializer `deserialize_option_path_buf` with `#[serde(default, deserialize_with)]` to gracefully handle missing/null/invalid `game_dir` values in JSON records.
- **Steamless Native Integration** (`src/ui/app.rs`, `src/services/steamless_service.rs`): Connected `patch_game_native()` (using steamflatline) to the "Remove SteamStub" UI button with fallback to CLI tool if native fails. Removes external Steamless dependency for SteamStub DRM removal.

---

## [3.2.4] - 2026-11-25

### 0B Install Issue Fix & Steamflatline Native Port Complete
- **License NetPacket Handler** (`src/steam_bridge/netpacket/netpacket_core.rs`): Fixed STUB handler for eMsg 780 (CMsgClientLicenseList) to actually call `netpacket_licenses::handle_recv_client_license_list_post_ipc()` for processing licenses.
- **Depot ID Resolution** (`src/steam_bridge/interop/bridge_lib.rs`, `interop_package.rs`): Implemented `get_all_depot_ids()` to collect depot IDs from Lua config (`addappid` directives), SLSsteam config (`additional_apps`), and GUI-managed bridge overrides. Added `take_pending_additions/removals()` for dynamic license updates.
- **Manifest Hooks** (`src/steam_bridge/manifest_hooks.rs`): Fixed `BuildDepotDependency` and `YldLoadDepotManifest` hook installation - now properly resolves runtime addresses and installs 14-byte RIP-relative JMP patches.
- **Steamflatline Native Port** (`src/steamflatline/`): Completed all SteamStub variant unpackers (v1.0, v2.0, v2.1, v3.0, v3.1 for x86 and x64) with proper PE parsing, XOR, XTEA, AES-256-CBC crypto.
- **XTEA Implementation**: Verified XTEA pass1/pass2 decryption matches Steamless behavior.
- **AES IV Rebuild**: Fixed `aes_rebuild_iv()` to match Steamless `AesHelper.RebuildIv()` behavior.
- **Version Bump**: Updated to v3.2.4 to avoid updater breakage.

---

## [3.2.3] - 2026-11-25

### v3.2.2 Audit Findings Fixed & Steamless Integration
- **7z Path Traversal Fix** (`src/services/wired_service.rs`): Fixed false positive rejection of legitimate files like `docs/LICENSE` by only matching actual path traversal patterns (`../`, `..\\`, `/../`, `\..\`) instead of any `..` substring.
- **DepotDownloader `-remember-password` Flag** (`src/services/depot_downloader.rs`): Fixed error "`-remember-password can not be used without -username or -qr`" by conditionally adding `-qr` flag when no username is provided.
- **DRM Decryption Keys** (`src/steam_bridge/interop/interop_decryption.rs`): Wired depot decryption keys from `bridge_lib` depot_keys store via new `read_depot_key()` FFI call.
- **Lua-Unlocked Games Download** (`src/steam_bridge/interop/interop_misc.rs`, `interop_package.rs`): `is_lua_unlocked()` now checks both owned depots AND `additional_apps` (games added via `addappid`), fixing games showing as unowned and refusing to download.
- **Licenses NetPacket Hooks** (`bridge_lib/src/lib.rs`): Installed `netpacket_licenses::install_licenses_hooks()` in bridge_lib initialization.
- **ONLINE_FIX_APP_ID Consolidation**: Removed duplicate constant definition across `interop_ipc_isteamuser.rs` and `interop_misc.rs`, now single source in `interop_misc.rs`.
- **BridgeLib C Exports** (`bridge_lib/src/lib.rs`): Added `get_depot_key_c()` and `is_additional_app_c()` C-compatible exports for FFI.
- **Steamless Integration**: `SteamlessService` already wraps Steamless CLI tool (native Rust port "steamflatline" pending).

---

## [3.2.2] - 2026-11-25

### Legacy C++ Cleanup & Final Audit Closure
- **Pruned Legacy C++ Tree**:
  - Removed obsolete `usefulsteamtools/` directory (151 files, 21,669 lines) completely superseded by native Linux Rust implementations in `src/steam_bridge/` and primary workspace services.
  - Retained 100% test pass rate across all 466 workspace unit, integration, and UI tests.

---

## [3.2.0] - 2026-11-25

### BetterSteamTools Linux Port & Steam Bridge Networking
- **Native Steam Client Netpacket Protocol Handling (`src/steam_bridge/netpacket/`)**:
  - Implemented client license list and online-fix protobuf netpacket decoders with wire-type varint parsing safeguards to prevent infinite-loop hangs during packet offset traversal.
  - Added robust serialization and boundary test suites for single and multi-package client license lists and 28-bit/32-bit varint encoders/decoders.

### Core Services & SLSsteam Synchronization
- **Hardened Lua Uninstaller De-sync (`src/services/lua_installer.rs`)**:
  - Ensured `<appid>.lua` content and depot keys / manifest overrides are read and parsed before file deletion, properly de-synchronizing SLSsteam configuration without leaving orphaned keys.
- **Session & Inotify Watcher Lifecycle (`src/services/auth_service.rs`, `src/services/hot_reload_service.rs`)**:
  - Removed persistent `auth.enc` alongside `auth.json` on sign-out to prevent automatic re-authentication.
  - Added thread handle / lifecycle management to `HotReloadService` to guarantee termination of existing inotify watcher threads before spawning new watchers.
- **Applied Fix Index Schema Alignment (`src/services/applied_fix_index.rs`, `src/ui/app.rs`)**:
  - Configured default JSON persistence path and matched camelCase serialization attributes with fixes service.

### Codecs, Fixes & Interop Hardening
- **UTF-8 Binary VDF Keys (`src/parsers/vdf_binary.rs`)**:
  - Ensured all property keys are encoded cleanly as UTF-8 rather than truncating Latin-1 byte slices.
- **Filesystem & Wine Compatibility (`src/services/fixes_service.rs`)**:
  - Sanitized colon characters in fix IDs when writing `.luatools-fix/<fix_id>.json` on FAT32/NTFS filesystems, and added case-insensitive, slash-tolerant matching for `[Software\Wine\DllOverrides]`.
- **Editor Sync & API Authorization (`src/ui/lua_editor.rs`, `src/services/api_client.rs`, `src/services/github_proxy.rs`)**:
  - Cleared base edit hash upon saving in the Lua editor to allow subsequent live syncs.
  - Attached Authorization Bearer headers for manifest downloads when API tokens are present.
  - Prevented duplicate host prefixes during GitHub proxy URL concatenation.

---

## [3.1.0] - 2026-11-25

### SLSsteam Configuration & Schema Compatibility
- **Complete 32-Key `AceSLS/SLSsteam` `config.yaml` Schema (`src/models/config.rs`, `src/services/slssteam_service.rs`)**:
  - Added and emitted all 32 upstream `AceSLS/SLSsteam` keys (`LogLevels: 0xff`, `API`, `DisableFamilyShareLock`, `UseWhitelist`, `MaxSchemaTries`, `SmartTickets: 0x1`, `SafeMode`, `WarnHashMissmatch`, `NotifyInit`, quoted `FakeName: ""` / `FakeEmail: ""`, `FakeWalletBalance`, `Plugins`, `DisableCloud`, `DisableUpdates`, `DumpClientInterfaces`, `ExtendedLogging`, explicit `[]` sequence defaults for `AppIds` / `FakeOffline` / `DepotBlacklist` / `AdditionalApps`, explicit `{}` map defaults for `FakeAppIds` / `ManifestIds` / `AppTokens` / `CDKeys` / `GameTitles` / `SubscriptionTimestamps` / `SteamIdOverride` / `LaunchOptions` / `DlcData` / `DenuvoGames`, and `IdleStatus`) so `SLSsteam.so` (`yaml-cpp`) starts with zero missing-key or null-type warnings.
  - Fixed `ManifestIds` parsing & serialization: validates unquoted `u64` manifest GIDs, rejects non-numeric strings, strips octal-trap leading zeros, preserves non-numeric app tokens via `AppTokenStrings`, and automatically heals legacy/incomplete `config.yaml` files (including quoted/nested `ManifestIds` and scalar `SteamIdOverride`) on startup while preserving custom `extra_fields` without duplicate keys.

### UI Discoverability & Per-Game Integration Wiring
- **Direct Access to `Integration & Process Controls` & Surfaced Actions (`src/ui/app.rs`, `src/ui/dialogs.rs`, `src/ui/views_primary.rs`, `src/ui/views_workbench.rs`, `src/ui/views_guide.rs`)**:
  - Wired the **Home** screen `Open` button (and game cover click) on Recently Added Games directly to **Integration & Process Controls** (`04_library_integration_modal.png`), with a companion `Depots & DLC` shortcut beside it, plus a new 1-click **Game Tools** bar inside the Integration modal (`Files & DLC`, `Edit .lua`, `Game Fixes`, `Launch Options`, `Open Folder`).
  - Surfaced visible 1-click `Integrate`, `Files & DLC`, and `Edit .lua` buttons with a flattened `More` menu in **My Games**; added post-install action buttons (`Open Setup / Integration`, `Files & DLC`, `Game Fixes`, `Launch Options`) directly in **Add Games**; added `Integrate / Setup` and visible `LuaVault` action buttons (`Apply Selected`, `Rename`, `Delete`) in **Depots & Vault** and **Game Fixes**; and un-hid **Cloud Save Redirect** and **Steam Store Page Plugin & URI Links** into open cards in **Settings › Steam Setup**.

### Documentation
- **Plain-English `README.md` Rewrite (`README.md`)**:
  - Rewrote `README.md` into a concise, plain-English quick-start and feature guide aligned 100% with the updated tab and button labels across all screens and guide screenshots.

---

## [3.0.1] - 2026-11-25

> **Major Stable Release (`v2.6.1` -> `v3.0.1`)**: Consolidates every milestone from `v2.7.1`, `v2.7.3-beta`, `v2.8.2-beta`, `v2.8.4-beta`, `v2.9.0-beta`, `v2.9.1-beta`, `v2.9.2-beta`, `v2.9.3-beta`, and `v2.9.4-beta` onto `main`, delivering 100% forensic feature parity, dramatic memory optimization (**220.7 MB -> 87.5 MB VmRSS**), a simplified primary-user interface, and complete test-verified reliability (**413 passing workspace tests, 0 warnings**).

### Memory & Performance Optimizations
- **59.4% Lower Live Memory Footprint (`220.7 MB -> 87.5 MB VmRSS`, `101.2 MB -> 25.6 MB RssAnon`, `128.2 MB -> 45.4 MB PSS`)**:
  - **Streaming 64 KiB `applist.json` Scanner (`src/services/caches.rs`)**: Replaced eager full-file deserialization of the 15 MB Steam app list (~65 MB peak private heap) with `SteamAppListCache::resolve_batch()`, scanning `applist.json` in 64 KiB chunks with early exit and bounded hit/miss caches (512 hits / 1024 misses). Capped `SteamAppInfoCache` in-memory cache to 32 entries.
  - **Single-Vendor Mesa EGL Preference on Hybrid Laptops (`src/main.rs`)**: Defaults `__EGL_VENDOR_LIBRARY_FILENAMES` to `/usr/share/glvnd/egl_vendor.d/50_mesa.json` when unset and no explicit PRIME offload is requested, avoiding simultaneous loading of both Mesa/LLVM and NVIDIA shader compilers (~41 MB immediate RSS savings) and disabling unused 2D depth/stencil/MSAA framebuffers.
  - **Lazy System Tray Loading (`src/main.rs`, `src/models/config.rs`, `src/ui/app.rs`, `src/ui/views_workbench.rs`)**: Changed default `close_to_tray` to `false` and deferred `dlopen` of `libayatana-appindicator3` / `libgtk-3` (~25 MB RSS + 8 GLib threads) until tray mode is requested (`--minimized`, `--tray-locked`, or toggled on in **Settings › General**).
  - **Bounded Downscaled Textures & Eviction (`src/ui/app.rs`, `src/ui/views_guide.rs`)**: Downscales cover artwork to max `280×140` (`FilterType::Triangle`, ~6× smaller RGBA footprint) with a 24-entry LRU cache, downscales Help guide screenshots to max `760px` wide, and evicts decoded guide textures immediately when leaving Help.
  - **Lazy Remote Catalogs, Zero Per-Frame Disk I/O & Size Optimization (`src/ui/app.rs`, `src/ui/views_workbench.rs`, `src/ui/theme.rs`, `src/services/wired_service.rs`, `Cargo.toml`)**:
    - Deferred remote Steam featured catalog and Game Fixes catalog fetches until first visit to **Add Games** / **Game Fixes** (and Millennium RPC probe to explicit click).
    - Cached `WiredService::status()` SHA-256 hashes by `(path, len, mtime)` instead of hashing multi-MB `.so` libraries at 60 FPS.
    - Cached `ExternalToolsManager::inspect_tool` and `TelemetryService::recent_events` instead of polling disk on every UI frame; replaced per-frame `get_installed_games()` directory scans in Game Fixes with cached `library_rows`.
    - Removed redundant embedded `Ubuntu-Light` and `Hack` fallback font buffers once system fonts load, switched release profile to `opt-level = "s"` (shrinking binary RSS from 14.1 MB to 10.3 MB), and invoked `libc::malloc_trim(0)` after startup and library refreshes.

### Critical Bug Fixes & Security Hardening
- **Fixed Unwanted `example.com` Browser Tab Bug (`src/services/http_server.rs`)**:
  - Added strict URL validation (`validate_external_url`) on `POST /open-url` enforcing `http://` or `https://` schemes, rejecting control characters/whitespace/leading `-`, and explicitly blocking RFC 2606 example/test domains (`example.com`, `*.example.com`, `example.org`, `example.net`, `*.example`, `*.invalid`, `*.test`) with HTTP `400`.
  - Added `allow_external_spawn()` guard across `/open-url`, `/restart-steam`, RPC `"RestartSteam"`, and `/remove` global SLSsteam mutations so automated tests and headless runs (`LUATOOLS_NO_EXTERNAL_LAUNCH=1`) never spawn browsers, never launch Steam, and never modify `~/.config/SLSsteam/config.yaml`.
- **Fixed Steam Launch Button & SLSsteam Detection (`src/ui/app.rs`)**:
  - Top-bar Steam launcher and `launch_steam_with_bridge()` now recognize `~/.local/bin/steam-sls` and `~/.local/share/SLSsteam/SLSsteam.so` alongside `luatools-steam-launcher` / `libsteam_integration.so`, so SLSsteam users see **`Launch Steam`** / **`Restart Steam`** immediately instead of a broken bridge warning.
- **Fixed Blocking UI Operations & Wiring Gaps (`src/ui/dialogs.rs`, `src/ui/app.rs`, `src/ui/views_workbench.rs`, `src/services/*.rs`)**:
  - Moved `save_launch_options_coordinated` and `restore_launch_backup_coordinated` off the UI thread (`AppMessage::LaunchOptionsFinished`) and cached Steam running state on modal open instead of scanning `/proc` every frame.
  - Moved `FixesService::unfix_game` (Revert) onto a background worker with live progress bar feedback, and wired `AppliedFixIndexService` (`LT-76`) recording/rebuilding across apply & revert paths (isolated from `cfg!(test)` / temp dirs).
  - Wired `check_lua_overwrite_diff` into `AcquireInstallFinished` so cloud downloads prompt the `OverwriteDiffModal` before overwriting customized `.lua` files.
  - Fixed `PluginInstallerService::install_default_luatools_js` when given a directory path (appends `luatools.js` instead of writing a file named `LuaTools`) and wired `ensure_luatools_js_installed` in both First-Run Quick Setup and **Settings › Steam Setup › Install Store Page Plugin (LTSP)** (`LT-19`).
  - Fixed `HotReloadService::update_watch_dir` to cleanly restart the `inotify` watcher when Steam's path changes while running.
  - Wired immediate auto-save on all Settings checkboxes (`auto_update_apps`, `fast_fetch`, `donate_keys`, `check_updates_on_startup`, `cef_bridge_enabled`, `play_not_owned`, `limit_downloads`).
- **Completed Remaining Forensic Audit Closure Items (`src/steam_bridge/preload_loader.rs`, `src/steam_bridge/denuvo_auth.rs`, `src/steam_bridge/credentials/eticketclient.rs`, `src/services/wired_service.rs`, `uninstall.sh`)**:
  - Multi-user UID filtering in `preload_loader.rs` (`find_steam_pid` / `kill_all_steam_instances`) so only processes owned by the current user (`geteuid()`) are matched.
  - PE32/PE64 Denuvo section/marker scanner (`ProtectionScan`, `BST-54`) in `denuvo_auth.rs`.
  - Dynamic `seteticketurl` override wiring (`BST-07`) in `eticketclient.rs` & `lua_parser.rs`.
  - SHA-256 active unlocker verification (`LT-65`) in `wired_service.rs`, launcher auto-spawn hook (`LT-20`), and XDG MIME handler deregistration in `uninstall.sh` (`LT-05`).

### Simplified Primary-User Interface
- **Cleaner Navigation & Plain-English Labels (`src/ui/app.rs`, `src/ui/views_primary.rs`, `src/ui/views_workbench.rs`)**:
  - Grouped the left sidebar into **GAMES** (`Home`, `Add Games`, `My Games`, `Game Fixes`) and **TOOLS** (`Depots & Vault`, `Help`), with `Settings` & Account status anchored cleanly at the bottom.
  - Streamlined **Home** (`lets start crackin 😛` header, simple file drop sheet, 3 clickable plain-English status tiles jumping straight to Steam / Unlocker / Account setup, and Recent Games grid; moved technical `inotify` Hot-Reload watcher controls to **Settings › Advanced Bridge**).
  - Simplified top command bar (`Launch Steam` / `Restart Steam` / `Setup Steam`, showing red `Stop Steam` only while Steam is actually running).
  - Tidied **My Games** row actions (`Files & DLC`, `Options`, `More` with common actions up front and expert actions organized inside an **`Advanced Tools`** submenu).
  - Reorganized **Settings** into plain-English tabs (`General`, `Account`, `Steam Setup`, `Advanced Bridge`, `Updates & Tools`, `Diagnostics`), placing **1-Click Steam Unlocker (SLSsteam)** first under **Steam Setup** and folding **Cloud Redirect & Store Page Plugins** into a clean collapsible **Advanced** drawer.

---

## [2.9.4-beta] - 2026-11-25

### Added & Fixed (15 Forensic Audit Priority Features)
- **Priority 1 Critical Fixes**:
  - Silent headless protocol installation (`luatools://install/silent/...`, `?silent=1`) (`LT-13`).
  - Rootless Steam relaunch prioritizing `~/.local/bin/steam-sls` with `LD_AUDIT` (`LT-18`, `LNX-04`).
  - Default `SLSsteam` `play_not_owned = true` (`LNX-01`).
  - Colon-safe fix IDs (`online-fix:<uuid>`) in `FixesService::is_safe_fix_id` (`LT-74`, `LT-75`).
  - Canonical `text/plain` `depotId:hexKey,...` wire format in `DonateKeysService` (`LT-45`).
- **Priority 2 Core Services & Encryption**:
  - Steam UI language detection (`LT-16`), `DepotCacheMigrationService` (`LT-34`), raw manifest fetch endpoints (`LT-44`), binary Steam `.manifest` protobuf codec (`src/parsers/manifest_binary.rs`, `LT-60..62`), `LaunchModStore` with PICS `change_number` drift detection & coordinated Steam shutdown (`src/services/launch_mod_store.rs`, `LT-57..59`).
  - Central `AppliedFixIndexService` registry (`src/services/applied_fix_index.rs`, `LT-76`), `DownloadQueue` with concurrency & ETA tracking (`src/services/download_queue.rs`, `LT-38`), per-depot DLC checkbox toggles (`LT-31`), `.lua` Overwrite Diff modal (`LT-32`), and at-rest credential encryption using `age` x25519 + AES-GCM (`src/services/credential_encryption.rs`, `LT-40`).
- **Priority 3 Linux Compatibility**:
  - `StubRemover` / `Steamless` `.unpacked.exe` replacement (`LNX-06`), Goldberg Windows DLL staging for Proton titles (`LNX-08`), and `PlayUnownedService` `SLSsteam` synchronization (`LNX-03`).

---

## [2.9.3-beta] - 2026-11-24

### Added & Fixed (Phase 3 Forensic Audit Closure & Production Call-Path Wiring)
- **IPC, Single-Instance Lifecycle & Rootless Steam Launcher (`src/main.rs`, `src/services/protocol_service.rs`, `src/services/settings_service.rs`, `src/services/steam_service.rs`, `src/steam_bridge/preload_loader.rs`, `assets/luatools.desktop.in`, `install.sh`, `uninstall.sh`)**:
  - Updated `preload_loader::generate_ld_preload_launcher` (`LT-18`, `BST-65`, `LT-20`) to strip 64-bit `libsteam_integration.so` from `LD_PRELOAD` before launching 32-bit Steam, auto-launch `luatools --minimized --tray-locked` in the background if not running, prioritize `~/.local/bin/steam-sls` or set `LD_AUDIT` to `library-inject.so:SLSsteam.so`, and preserve `--gdb`/`--lldb` support.
  - Updated `SteamService::relaunch_steam_sls` (`LNX-04`) to include `library-inject.so` alongside `SLSsteam.so` in `LD_AUDIT` and detect Flatpak Steam (`~/.var/app/com.valvesoftware.Steam/.local/share/Steam/steam.sh`) when native `steam.sh` is absent.
  - Kept silent protocol URLs (`luatools://install/silent/...`, `?silent=1`) and secondary `--minimized` launches headless without stealing focus, added `"TRAYLOCK"` IPC forwarding for `--tray-locked`, and called `ProtocolService::register_desktop_handler()` on startup (`LT-02`, `LT-03`, `LT-10`, `LT-13`).
  - Enforced `0o600` permissions on `~/.config/luatools/settings.json` (`LT-40`), added `--minimized --tray-locked` to autostart desktop entries (`LT-08`), added `bst://redeem/<code>` and `bst://<appid>/<token>` extraction (`BST-60`), registered both `luatools://` and `bst://` under `$SUDO_USER` in `install.sh`, and cleaned up `~/.config/autostart/luatools.desktop` in `uninstall.sh` (`LT-05`).
- **Store Plugin, CDP Queue Drain & Full 13-Route LTSP Server (`src/services/http_server.rs`, `src/services/cef_bridge.rs`, `src/services/plugin_installer_service.rs`, `src/services/plugin_add_service.rs`, `src/services/millennium_service.rs`, `src/services/api_client.rs`, `src/services/caches.rs`, `src/services/doh_resolver.rs`, `src/services/github_proxy.rs`, `src/services/proxied_file_downloader.rs`, `src/services/analytics_service.rs`)**:
  - Added `Access-Control-Allow-Private-Network: true` CORS headers (`LT-26`), wired `PluginLog` into `HttpServerService` and `PluginAddService` (`LT-25`), and implemented all 13 extended LTSP routes (`/add-status`, `/add-source`, `/check-sources`, `/download`, `/download-status`, `/cancel`, `/open-url`, `/restart-steam`, `/check-updates`, `GET/POST /loaded-apps`, `/api-list`, `/icon`) plus real `FixesService::is_fix_applied` checks on `/check/:id` and `remove_from_slssteam = true` on `/remove` (`LT-27`, `LT-28`).
  - Added host-side CDP queue polling (`window.__lt_queue` / `window.__lt_drain()` + `window.__lt_resolve`) and `Page.addScriptToEvaluateOnNewDocument` persistence in `cef_bridge.rs` (`LT-22`, `LT-23`), added upstream `luatools.js` fetching and `config/millennium.ini` `[Plugins]` registration (`LT-19`, `LT-21`), and wired detected Steam UI language into Store metadata/search requests (`LT-16`).
  - Added `cloudflare-dns.com/dns-query` (`1.1.1.1`) DoH fallback (`LT-47`), wired `ProxiedFileDownloader` across GitHub release downloads (`LT-48`), and configured production Umami analytics defaults (`LT-46`).
- **DepotDownloader, Binary `.manifest` Protobuf Codec, VDF `LaunchModStore` Rebase & Fixes (`src/services/depot_downloader.rs`, `src/parsers/vdf_binary.rs`, `src/services/fixes_service.rs`, `src/services/steamless_service.rs`, `src/services/external_tools.rs`, `src/services/wired_service.rs`, `src/services/steam_library_service.rs`, `src/models/lua_vault.rs`)**:
  - Gated `-manifestfile` strictly on `is_mod == true`, preserved `-remember-password` when staging `.manifest` files (`LT-44`, `LT-67`, `LT-68`), implemented Steam binary `.manifest` magic (`0x71F617D0` / `0x16349781`) + protobuf varint metadata decoding (`LT-60`, `LT-61`, `LT-62`), and enforced `2x` `statvfs` free-space checks (`LT-56`).
  - Preserved raw binary VDF field casing, implemented in-place launch entry editing without deleting sibling slots (`LT-54`, `LT-55`), and added `LaunchModStore` (`launch_mods.json`) with PICS `change_number` drift detection and rebasing (`LT-58`, `LT-59`).
  - Sanitized NTFS-illegal `:` characters in `.luatools-fix/` backup IDs (`LT-74`), wired `AppliedFixIndexService` (`applied-fixes.json`) (`LT-76`), fixed Proton `user.reg` newline insertion, added `LuaVault` `index.bak` recovery (`LT-36`), removed source manifests after verified `migrate_depotcache_dir` (`LT-34`), made `SteamlessService` synchronous (`LT-69`), wired `FileHashService` SHA-256 checks (`LT-49`, `LT-65`), and added ZipSlip guards (`LNX-04`, `LNX-08`).
- **SLSsteam Schema Alignment, Procedural Lua Pre-Evaluator & Active Userdata Tickets (`src/models/config.rs`, `src/services/slssteam_service.rs`, `src/services/lua_installer.rs`, `src/services/manifest_binding_service.rs`, `src/services/game_process_injection_service.rs`, `src/parsers/lua_parser.rs`, `src/steam_bridge/ticket_store.rs`)**:
  - Aligned `SlssteamConfig` with upstream `AceSLS/SLSsteam` (`ManifestIds:`, `CDKeys:`, `DenuvoGames:`, `GameTitles:`, `SteamIdOverride:`, and unquoted numeric `uint64` `AppTokens`) (`LNX-01`, `LNX-02`, `BST-02..04`, `BST-09`, `BST-14..16`, `BST-34`, `BST-52`).
  - Added procedural Lua pre-evaluator `expand_procedural_lua` (`BST-01`) supporting `local` variable assignments, `"a" .. "b"` string concatenation, and `ipairs({...})` / numeric `for` loops alongside `setlegacycdkey`, `setAppTicket`, `setETicket`, `seteticketurl`, and `setStat` directives (`BST-05..09`).
  - Synchronized depot decryption keys to `<Steam>/config/config.vdf` on install, cleaned all SLSsteam keys on uninstall, resolved active `userdata/<account_id>/config/localconfig.vdf` in `TicketStore` (`BST-53`), and configured Proton/Wine `WINEDLLOVERRIDES` in `GameProcessInjectionService` (`BST-62`, `LNX-10`).
- **Complete GUI Wiring, Overwrite Diff Modal & 100-Item Download History Ring (`src/ui/app.rs`, `src/ui/dialogs.rs`, `src/ui/views_primary.rs`, `src/ui/views_workbench.rs`, `src/ui/lua_editor.rs`)**:
  - Wired interactive `.lua` Overwrite Diff Confirmation Modal with `LuaVault` backup (`LT-32`), 100-item persistent Download Queue & History Ring (`LT-38`), startup `LaunchModStore` PICS drift rebase toast (`LT-58`, `LT-59`), `bst://` token persistence in `handle_protocol_url` (`BST-60`), and Guided First-Time Setup in the first-run modal (`LT-09`).
  - Wired Login Autostart toggle (`LT-08`), per-depot DLC enable/disable checkboxes (`LT-31`), startup DepotCache migration (`LT-34`), in-place `LuaVault` editing & SLSsteam sync (`LT-37`), hot-reload watcher UI refresh (`LT-35`), Steam termination before `appinfo.vdf` writes (`LT-57`), Steamless DRM unpacker (`LNX-06`, `LT-69`), and Legacy Unlocker Cleaner (`LT-63`).

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
- **Platform Services, Directives & Extended Tooling**:
  - Implemented OS Login Autostart (`LT-08`) managing `~/.config/autostart/luatools.desktop` with `--minimized` and CLI `--autostart-enable`/`--autostart-disable`.
  - Implemented Steam UI Language Detection (`LT-16`) reading `registry.vdf` / `config.vdf` and mapping to canonical UI codes.
  - Implemented `bst://` URI parsing and desktop protocol registration (`BST-60`).
  - Added Raw Manifest Fetch Endpoints (`LT-44`) `/api/givemethemanifestpunk` and `/manifest/:depotid/:gid` with backend mirror failover.
  - Wired `DohResolver` (`LT-47`) and `ProxiedFileDownloader` / `GitHubProxy` (`LT-48`) for resilient DNS and tool downloads.
  - Hardened telemetry consent gating and PII path scrubbing (`LT-46`).
  - Implemented Per-Depot DLC enable/disable toggling (`LT-31`) via `LuaFileParser::toggle_dlc_depot`.
  - Implemented DepotCache migration service (`LT-34`) migrating `config/depotcache` to `depotcache/`.
  - Implemented Legacy Unlocker Mode Migration (`LT-63`) detecting and safely backing up legacy SmokeAPI/GreenLuma/BST binaries.
  - Implemented LuaVault in-place variant updates (`LT-37`) via `update_variant` and `edit_base`.
  - Implemented CloudRedirect configuration generator (`LT-71`, `BST-42`, `BST-59`) and EMsg 147 synthesis.
  - Implemented extended Lua directives (`BST-14`, `BST-15`, `BST-16`) for `addprocess()`, `forcedenuvo()`, and `pinApp()`.
  - Implemented Crockford Base32 legacy CD key generator (`BST-57`) and Game Process Injection daemon with Proton/Wine overrides (`LNX-09`).

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
