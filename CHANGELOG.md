# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.0-beta] - 2026-11-23 — Complete Native Rust Rewrite (Beta)

### Added
- **100% Native Rust Architecture**: Ground-up rewrite replacing Python 3 + PyQt6 + `httpx` + `PyYAML` + `websockets` + `Pillow` + `vdf` with a single stripped native Rust binary (`eframe`/`egui` + `ureq`/`rustls` + `tungstenite` + `tiny_http`).
- **One-Shot Background Git Update Check & Corner Self-Updater**:
  - `UpdateService` (`src/services/update_service.rs`) runs a single non-blocking Git remote check on startup against `origin` (`https://github.com/subsurfmod/LuaToolsLinux.git` default).
  - Displays a compact sharp-edged **`⬆ UPDATE`** button in the **bottom-right corner** when new commits or a newer version are available; clicking pulls from Git, compiles a stripped release binary, atomically replaces the installed executable via `rename()` (avoiding Linux `ETXTBSY`), and switches to a 1-click **`↻ RESTART`** button (also available via `luatools --update`).
- **Turnkey Installation & Native Linux Packaging**:
  - Universal `install.sh` supporting Fedora/RHEL (`dnf`), Ubuntu/Debian (`apt`), Arch/SteamOS (`pacman`), and openSUSE (`zypper`), automatic `rustup` bootstrap if needed, SVG icon + `.desktop` launcher installation, and `luatools://` `xdg-mime` registration.
  - `packaging/build-packages.sh` & `packaging/luatools-linux.spec` building portable `.tar.gz`, native `.rpm`, and native `.deb` packages.
- **Sharp 0px Monotone + Cobalt Blue Workstation UI**:
  - Hardware-accelerated `CornerRadius::ZERO` interface across all 7 views (`01 // Overview`, `02 // Acquire`, `03 // Library`, `04 // Depots & Vault`, `05 // Fix Catalog`, `06 // Toolchain`, `07 // System`) plus modal editors for Game Overrides and binary `appinfo.vdf` (v27/v28/v29) Launch Options.
  - Global Quick Command Bar (`Ctrl+K` / `Ctrl+L`), `Ctrl+1`…`Ctrl+7` view hotkeys, window-wide `.lua`/`.manifest`/`.zip` drag-and-drop, and Lua syntax highlighting.
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
