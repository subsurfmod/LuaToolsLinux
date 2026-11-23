# Changelog

All notable changes to **LuaTools Linux** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [2.4.0-beta] - 2026-11-23

### Added
- **Steam Client Interop Layer (`src/steam_bridge/`)**:
  - **`binary_reader.rs`**: ELF64 parser for Steam Linux binaries (`steamclient.so`, `steamui.so`); reads section headers, symbol tables, dynamic symbols, and computes SHA-256 hashes for build-compatibility pattern lookup.
  - **`memory_patch.rs`**: x86-64 inline memory patching via trampolines (`mprotect` + relative jump + jump-back trampoline) for safe runtime code redirection.
  - **`preload_loader.rs`**: `LD_PRELOAD`-based injection of the integration shared library into the Steam client process; generates launcher scripts, manages `/etc/ld.so.preload` entries, and detects running Steam PIDs.
  - **`ticket_store.rs`**: Steam ticket store backed by `localconfig.vdf`; reads/writes `AppTicket`, `ETicket`, and `SteamID` per app ID.
  - **`pattern_db.rs`**: Steam build compatibility lookup from the upstream pattern tracker (GitHub raw → jsDelivr CDN fallback → local cache); matches SHA-256 hashes to pattern TOML files.
  - **`ipc.rs`**: Unix domain socket IPC between the GUI and the integration library running inside Steam; supports typed messages for ping, tickets, play-unowned apps, stats, legacy CD keys, depot keys, manifest codes, app ID redirects, and manifest pins.
- **`bridge_lib/`** (`bridge_lib/Cargo.toml`, `bridge_lib/src/lib.rs`):
  - New `cdylib` crate built as `libsteam_integration.so` for `LD_PRELOAD` injection into Steam.
  - Constructor initializes at Steam load, scans Steam binaries, resolves byte patterns via `pattern_db`, installs memory patches, and starts the IPC server.
  - **`BIsSubscribedApp` redirect**: Installs a memory patch on the Steam client ownership-check function; apps in the redirect set report as owned.
  - **`GetAppId` redirect**: Installs a memory patch on the Steam client app-ID query function; returns configured redirect IDs (e.g. 480 for online-play compatibility).
  - IPC message handlers for tickets, play-unowned apps, stats, legacy CD keys, depot keys, manifest codes, app ID redirects, and manifest pins.
- **`SteamBridgeService` (`src/services/steam_bridge_service.rs`)**:
  - High-level service connecting the bridge layer to the GUI; detects Steam, scans binaries, loads patterns, installs the integration library, manages tickets, and sends IPC messages.
  - Exposes `install_integration()`, `uninstall_integration()`, `scan_and_load_patterns()`, `get/set/remove_tickets()`, `send_ipc_message()`, and `status()`.
- **Manifest Binding Service (`src/services/manifest_binding_service.rs`)**:
  - Tracks manifest pins per app+depot; `set_pin()`, `remove_pin()`, `get_pin()`, `get_pins_for_app()`, `list_pinned_depots()`.
  - IPC messages: `SetManifestPin`, `RemoveManifestPin`, `ManifestBound`.
- **Hot-Reload Service (`src/services/hot_reload_service.rs`)**:
  - `inotify`-based watcher on the Steam `stplug-in/` directory; detects CREATE/MODIFY/DELETE/MOVE events on `.lua` files; runs in a background thread and notifies the bridge via IPC.
- **Cloud Redirect Service (`src/services/cloud_redirect_service.rs`)**:
  - Detects CloudRedirect installation (`~/.local/share/Steam/config/cloudredirect`); per-app cloud enable/disable; `sync_to_cloudredirect()` writes `cloudredirect.conf`; status tracking (`NotInstalled` → `Installed` → `Active`).
- **Game Process Integration Service (`src/services/game_process_injection_service.rs`)**:
  - Per-app integration configuration (library path, env vars, launch args); `build_launch_command()` constructs a `Command` with `LD_PRELOAD` set for game-process integration; ptrace-based running-process injection prototype; status tracking (`NotInjected` → `Injected` → `Failed`).

### Changed
- **Cargo workspace**: Root `Cargo.toml` converted to a workspace with members `.` and `bridge_lib`; version bumped to `2.4.0`.
- **`src/lib.rs`**: Registered `pub mod steam_bridge;`
- **`src/services/mod.rs`**: Registered `steam_bridge_service`, `manifest_binding_service`, `hot_reload_service`, `cloud_redirect_service`, `game_process_injection_service`.
- **`src/ui/app.rs`**: Added `steam_bridge_service: SteamBridgeService` field to `LuaToolsApp` in both `new()` and `new_headless_demo()` constructors.
- **`Cargo.toml`**: Added dependencies `thiserror`, `toml`, `libc`, `inotify`.

### Known Limitations / Prototype Notes
- **`bridge_lib`** is built as a separate `cdylib` and must be placed at `~/.local/lib/luatools/libsteam_integration.so` before `install_integration()` can register it.
- **Memory patches** are installed at Steam startup via `LD_PRELOAD` constructor; pattern addresses depend on the upstream pattern tracker publishing matching TOML files for the running Steam build.
- **ptrace running-process injection** is prototype-level and requires `CAP_SYS_PTRACE` and a permissive Yama ptrace scope; `LD_PRELOAD` launch is the recommended path.
- **Manifest binding enforcement** stores pins and signals via IPC; Steam-side manifest interception requires additional pattern patches for the current Steam build.

### GUI Changes Needed (not yet implemented)
The following GUI integration work remains for the 2.4.0 release:

- **Settings › Steam Integration** tab:
  - Steam detection status indicator
  - Integration library install/uninstall buttons
  - Pattern load status (steamclient / steamui SHA-256 + pattern matches)
  - LD_PRELOAD toggle (system-wide vs per-launch)
- **Library › More › Integration Actions** per game:
  - Add/Remove from redirect set (play unowned)
  - Set/Remove app ID redirect (480 spoof)
  - Set/Remove legacy CD key
  - Set/Remove depot decryption key
  - Set/Remove stat value
- **Depots › Manifest Binding** sub-tab:
  - List pinned manifests per depot
  - Add/Remove manifest pins
  - Show bind status (active / pending)
- **Hot-Reload status** in Home or Settings:
  - Watcher running/stopped indicator
  - Last file change event log
- **Cloud Redirect** status in Settings › SLSsteam & Store Bridge:
  - Installed/Not installed indicator
  - Per-app cloud enable/disable toggles
  - Sync button
- **Game Process Integration** in Library › More:
  - Per-app integration enable/disable
  - Library path selector
  - Launch with integration button

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
