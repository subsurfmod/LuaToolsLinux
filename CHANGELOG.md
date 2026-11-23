# Changelog

All notable changes to **LuaTools Linux** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
