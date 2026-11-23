# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.1.0] - 2026-11-23

### Added & Improved
- **Depot Decryption Key File Support (`-depotkeys`)**: Automatically resolves per-app `.lua` and `SLSsteam` depot decryption keys into a restricted (`0600`) temporary keys file for `DepotDownloader` / `DepotDownloaderMod` and cleans up after completion, plus `-manifestfile` support when using `DepotDownloaderMod`.
- **CEF Store Bridge & DRM Fix Refactor**: Renamed `cef_injector` to `cef_bridge` (`CefBridgeService`) with backwards-compatible `settings.json` deserialization (`alias = "cef_injector_enabled"`), and unified neutral toolchain naming (`SteamAutoPatch` / `StubRemover`) with full binary & CLI compatibility for upstream GitHub releases.

### Fixed
- Restored live `lua.tools` fix catalog/download API endpoints while preserving refactored `DRMFix*` domain types.
- Wired live `SteamInstallation` and `SlssteamService` state into `DepotDownloaderService`, filtered non-depot DLC entitlement AppIDs out of depot download selections, and normalized progress percentages across tool bootstrap and depot downloads.
- Fixed short/non-ASCII decryption key preview slicing panic in Depot Details modal, hardened binary `appinfo.vdf` header parsing against unsigned/corrupt entry sizes, synced `SLSsteam` on `LuaVault` variant apply, honored `FastFetch` & `luatools://` source selection, and resolved all Rust `clippy` lints across library, binary, and test targets.

## [2.0-beta] - 2026-11-23 — Complete Native Rust Rewrite (Beta)

### Added
- **100% Native Rust Architecture**: Ground-up rewrite replacing Python 3 + PyQt6 + `httpx` + `PyYAML` + `websockets` + `Pillow` + `vdf` with a single stripped native Rust binary (`eframe`/`egui` + `ureq`/`rustls` + `tungstenite` + `tiny_http`).
- **One-Shot Background Git Update Check & Corner Self-Updater**:
  - `UpdateService` (`src/services/update_service.rs`) runs a single non-blocking Git remote check on startup against `origin` (`https://github.com/subsurfmod/LuaToolsLinux.git` default).
  - Displays a compact sharp-edged **`⬆ UPDATE`** button in the **bottom-right corner** when new commits or a newer version are available; clicking pulls from Git, compiles a stripped release binary, atomically replaces the installed executable via `rename()` (avoiding Linux `ETXTBSY`), and switches to a 1-click **`↻ RESTART`** button (also available via `luatools --update`).
- **Turnkey Installation & Native Linux Packaging**:
  - Universal `install.sh` supporting Fedora/RHEL (`dnf`), Ubuntu/Debian (`apt`), Arch/SteamOS (`pacman`), and openSUSE (`zypper`), automatic `rustup` bootstrap if needed, SVG icon + `.desktop` launcher installation, and `luatools://` `xdg-mime` registration.
  - `packaging/build-packages.sh` & `packaging/luatools-linux.spec` building portable `.tar.gz`, native `.rpm`, and native `.deb` packages.
- **Official `SteamRE/DepotDownloader` Crash Hardening (`src/services/depot_downloader.rs`)**:
  - Removed unsupported CLI flags (`-depotkey` / `-manifestfile` / forced `-username anonymous`), added `.NET` runtime stability guards (`stdin=null`, `DOTNET_EnableWriteXorExecute=0`, `DOTNET_EnableDiagnostics=0`, `DOTNET_DbgEnableMiniDump=0`, `RLIMIT_CORE=0`, `-max-downloads 8`), preferred framework-dependent `DepotDownloader.dll` when host `dotnet` is installed on bleeding-edge `glibc` / `OpenSSL` systems, and added 1-click terminal QR / Steam Guard session login (`-remember-password`).
- **Papercut Monotone-Dark UI, Top-Right Toast Notifications & Rich Typography**:
  - Moved transient notifications to non-blocking top-right dismissible toasts, moved the Git `⬆ Update` button to the top-right header corner, and added scrollable containers + bottom breathing room so low-placed text is never obstructed.
  - Tactile layered construction-paper monotone dark palette (`#0C0D10` / `#101115` / `#181A20`) with cut-edge highlights, underside paper lips, cast drop shadows, taped bone-paper section tags, and deeper sapphire-navy accents (`#1A3A6B` / `#234B88`).
  - Rich non-Inter typography pairing `STIX Two Text Bold` editorial serif headings with `Open Sans` (`Semibold` + `Regular`) body copy and `Adwaita Mono` / `JetBrains Mono` / `Hack` code surfaces.
  - Non-overlapping `StripBuilder` + `egui::Sides` layout across 6 focused workspaces (`Home`, `Discover`, `Library`, `Depots & Vault`, `Game Fixes`, and 4-tab `Settings`), modal editors for Game Overrides and binary `appinfo.vdf` (v27/v28/v29) Launch Options, global Quick Command Bar (`Ctrl+K` / `Ctrl+L`), `Ctrl+1`…`Ctrl+6` hotkeys, `.lua`/`.manifest`/`.zip` drag-and-drop, and headless software rasterizer visual QA (`tests/ui_visual_qa.rs`).
  - Automatic XWayland (`WINIT_UNIX_BACKEND=x11`) fallback when `DISPLAY` is active to avoid `winit 0.30.13` Wayland `wl_seat` v10 pointer panics and support custom window icons.
- **Full 100% On-Disk Compatibility with v1.x**:
  - Seamlessly reads/writes `~/.config/luatools/settings.json`, `~/.local/share/luatools/auth.json`, `~/.local/share/luatools/luavault/`, `~/.config/SLSsteam/config.yaml`, `~/.local/share/Steam/config/stplug-in/`, and `.luatools-fix/` SHA-256 backup records.

### Fixed & Hardened (Carried Forward from v1.1 Audit)
- Eliminated recursive mutex deadlock in `SteamDepotInfo` using deduplicated `Condvar` flight tracking.
- Fixed `SteamLinkParser` so numeric words in game titles (e.g. `"Warhammer 40000"`, `"Battlefield 2042"`) trigger Store Search instead of being misparsed as AppIDs.
- Fixed `SLSsteam` `AdditionalApps` YAML sequence serialization, `libraryfolders.vdf` primary library deduplication, ZipSlip archive traversal protection, and separation of 64-char hex depot decryption keys vs app tokens.

---

## [1.0.0] - 2024-11-23

### Added
- Initial Python/PyQt6 release of LuaTools Linux (`Games`, `Depots`, `Fixes`, `External Tools`, `LuaVault`, `LuaFileParser`, `SteamDepotInfo`, `DepotDownloaderService`, `SLSsteam` YAML sync).
