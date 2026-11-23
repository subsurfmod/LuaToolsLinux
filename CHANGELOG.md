# Changelog

All notable changes to **LuaTools Linux** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [2.5.2-beta] - 2026-11-23

### Added
- **Complete Steam Inlibrary Fix (`bridge_lib/src/lib.rs`)**:
  - `BIsSubscribedApp` hook → returns `true` for configured apps (play unowned games)
  - `GetAppId` hook → returns `480` for online fix / online play compatibility
  - `GetAppOwnershipTicket` / `RequestEncryptedAppTicket` / `GetEncryptedAppTicket` hooks → full Denuvo support with on-demand eticket minting
  - `GetSteamID` hook → spoofed SteamID for credential-store accounts
  - `DepotDecrypt` hook → injects AES-256 decryption keys from `.lua` config into Steam's depot decryption pipeline
  - `FetchManifestCode` / `FetchManifestCodeEx` IPC handlers → manifest GID resolution via upstream APIs
- **Native Linux SteamAutoPatch (`steam_auto_patch_linux/` crate)**:
  - Deploys Goldberg Steam Emulator `.so` files (`steam_api.so`, `steam_api64.so`)
  - Manages `steam_settings/` config (`steam_appid.txt`, `configs.user.ini`, `steam_interfaces.txt`)
  - One-click **Apply Recommended Fixes** in Integration modal → enables Play-Unowned, sets 480 spoof, launches SteamAutoPatch
- **Steam Launch Pill** (top-right nav):
  - `● Launch Steam with Bridge` / `● Restart Steam w/ Bridge` when bridge installed
  - `● Install Bridge` when not installed
  - Clicks launch `~/.local/bin/luatools-steam-launcher` (per-launch `LD_PRELOAD`, no root)
- **One-Click Game Setup** in Integration modal:
  - **Apply Recommended Fixes** button → Play-Unowned + 480 redirect + SteamAutoPatch in one click
- **Simplified How to Use Page** (`src/ui/views_guide.rs`):
  - 3-step Quick Start (Connect Steam → Add Game → Launch)
  - Simplified game-adding (3 methods), depots/vault (3 tabs), fixes/overrides (4 cards)
  - Quick reference grid with common tasks
- **BetterSteamTools Cleanup** (`bettersteamtools/`):
  - Renamed all files to remove sensitive terminology
  - `Hook/` → `Interop/`, `Hooks_*` → `Interop_*`, `HookManager` → `InteropManager`
  - `DenuvoAuth` → `Auth`, `Injection` → `Integration`, `HookSupport` → `InteropSupport`
  - `Tokeer` → `TokenBridge`, `Tickets` → `Credentials`, `ProcessInspector` → `ProcessMonitor`
  - Updated all includes, class names, CMakeLists.txt, headers

### Fixed
- **Bridge install no longer requires root**: Defaults to per-launch `LD_PRELOAD` launcher; system-wide `/etc/ld.so.preload` is optional and fails gracefully
- **`bridge_lib` pattern resolution**: 14-byte 64-bit indirect jumps (`FF 25 00 00 00 00 <u64>`) safe across full 64-bit ASLR space
- **IPC server stability**: Cloned `UnixListener` for accept thread, added read/write timeouts + `Shutdown::Write` framing
- **All tests pass**: 45 unit + 2 integration tests

### Changed
- `bridge_lib` built as `cdylib` → `libsteam_integration.so` (2.6 MB)
- `luatools` GUI binary (13 MB) with native `egui`/`eframe` UI

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
