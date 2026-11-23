# LuaTools Linux v2.6.0 (Native Rust Edition)

A complete native Rust rewrite of LuaTools Linux (`eframe` / `egui`) featuring rootless Steam Client Interop (`libsteam_integration.so`), one-click game setup, native Linux Goldberg `SteamAutoPatch`, `LuaVault` build switching, `DepotDownloader` QR & depot-key automation, and an illustrated in-app **How to Use** manual.

---

## Installation (Includes All Dependencies)

Run the universal `curl` one-liner — it verifies/installs required Linux graphics, XDG, and archive runtime packages (`dnf` / `apt` / `pacman` / `zypper`), clones the repository into `~/.local/share/luatools/repo`, bootstraps `rustup` if needed to compile stripped native binaries (`luatools` + `libsteam_integration.so` + `libsteam_auto_patch_linux.so`), installs the desktop launcher & icon, and registers the `luatools://` URI scheme:

```bash
curl -fsSL https://raw.githubusercontent.com/subsurfmod/LuaToolsLinux/main/install.sh | bash -s -- --yes
```

Or install from a local `git clone`:

```bash
git clone https://github.com/subsurfmod/LuaToolsLinux.git
cd LuaToolsLinux
./install.sh --yes
```

- **System-wide install (`/usr/local/bin/luatools`)**: `curl -fsSL https://raw.githubusercontent.com/subsurfmod/LuaToolsLinux/main/install.sh | bash -s -- --yes --system`
- **Include optional Wine + .NET runtimes** (for Windows tools like Steamless / SteamAutoCrack): `curl -fsSL https://raw.githubusercontent.com/subsurfmod/LuaToolsLinux/main/install.sh | bash -s -- --yes --with-wine`
- **Uninstall cleanly**: `curl -fsSL https://raw.githubusercontent.com/subsurfmod/LuaToolsLinux/main/uninstall.sh | bash` (or pass `--purge` to also remove cached data)

---

## Visual Walkthrough & Screenshots

### 1. Home Workspace & Top-Right Steam Bridge Pill
![Home Workspace](assets/guide/01_home_overview.png)

### 2. Discover Catalog & Cloud Manifest Providers
![Discover Workspace](assets/guide/02_discover_store.png)

### 3. Depots, LuaVault Presets & Manifest Binding
![Depots & LuaVault](assets/guide/03_depots_vault_binding.png)

### 4. Library & One-Click Recommended Fixes Modal
![Integration & Quick Setup Modal](assets/guide/04_library_integration_modal.png)

### 5. Rootless Steam Client Interop & ELF64 Pattern Scanner
![Steam Integration](assets/guide/05_settings_steam_integration.png)

### 6. SLSsteam, Native Wired Suite & Cloud Redirect
![Wired & Cloud Redirect](assets/guide/06_settings_wired_cloud.png)

---

## Building Native `.rpm`, `.deb`, and Portable `.tar.gz` Packages

```bash
./packaging/build-packages.sh
```

Outputs self-contained distributables into `dist/`:
- **Fedora / RHEL / openSUSE**: `sudo dnf install ./dist/luatools-linux-2.6.0-1.*.rpm`
- **Ubuntu / Debian / Pop!_OS**: `sudo apt install ./dist/luatools-linux_2.6.0_amd64.deb`
- **Any Linux Distro (No root required)**: extract `dist/luatools-linux-2.6.0-x86_64.tar.gz` and run `./install.sh --yes --skip-deps`

---

## Full v2.6.0 Feature Matrix

| Workspace | Capabilities |
|-----------|--------------|
| **Home** | Taped overview header, Rem Drop Sheet drag-and-drop ingest (`lua`/`manifest`/`zip`), 3-column live status summary, live **`stplug-in/` Hot-Reload Watcher & Steam Bridge** status bar, top-right **Launch / Restart Steam w/ Bridge** pill |
| **Discover** | Unified Steam Store search, Featured (`Top Sellers` / `New Releases`), multi-provider manifest checks, Hubcap integration, FastFetch auto-install, DLC `.lua` generator, 1-click **Add to Steam Library** (`appmanifest_<appid>.acf`) |
| **Library** | Multi-library ACF + `stplug-in` scanner, live filter chips, bulk `.lua` cleanup, **One-Click Recommended Fixes** (`Play-Unowned` + `480` redirect + native `SteamAutoPatch`), **Integration Actions** (Redirect Set, CD Keys, `DepotKeyIntegration`, Steam Stats, per-game `LD_PRELOAD`), binary `appinfo.vdf` Launch Options, **ACF Registration** |
| **Depots & Vault** | 4-tab workspace (`Manifests & Vault`, `Lua Editor`, `Download Depots`, **`Manifest Binding`**) with content-addressed `LuaVault`, syntax-highlighted Lua editor, `Pin` / `Unpin` manifest toggling, IPC **Manifest Binding** enforcement, and crash-hardened `DepotDownloader` with `-depotkeys` & QR sign-in |
| **Game Fixes** | `lua.tools` game & DRM fixes catalog, category filters, SHA-256 `.luatools-fix/` backup & verified revert |
| **How to Use** | Illustrated 6-tab workstation manual (`Ctrl+6` / `F1`) with embedded high-DPI screenshots, click-to-enlarge preview lightbox, Quick Reference task table, annotated `.lua` reference, and 1-click jump buttons |
| **Settings** | 6 sub-tabs (`General & Steam`, `Account & Providers`, `SLSsteam & Store Bridge`, **`Steam Integration`**, `Updates & Toolchain`, `Telemetry & Logs`) covering Discord OAuth & Bot Code login, Hubcap API key validator, **Wired Suite (`ACCELA` + `SLSsteam`)**, **Cloud Redirect**, **`libsteam_integration.so` Interop + `inotify` Hot-Reload**, **Goldberg Emulator (Native Linux)**, **Git Self-Updater**, and **Privacy-First Opt-Out Bug Telemetry** |

---

## Effortless Git Self-Updates

- On startup, `luatools` runs **one lightweight non-blocking background check** against your Git remote (`origin` or a custom Git URL configured in `Settings -> Updates & Toolchain`).
- When new commits or a newer version exist upstream, an **`Update`** button appears in the **top-right corner** of the window.
- Clicking **`Update`** pulls the latest changes (`git pull`), compiles the workspace release binaries, atomically replaces the installed binary without `ETXTBSY` locks, and turns into **`Restart`** for a 1-click relaunch (also available via CLI as `luatools --update`).

---

## Keyboard Shortcuts

- **`Ctrl+K` / `Ctrl+L`**: Focus Global Quick Command Bar (AppID, Steam/SteamDB URL, `luatools://`, or Store Search)
- **`Ctrl+1` … `Ctrl+5`**: Switch between `Home`, `Discover`, `Library`, `Depots & Vault`, and `Game Fixes`
- **`Ctrl+6` / `F1`**: Open the illustrated `How to Use` manual (`Esc` closes enlarged screenshot previews)
- **`Ctrl+7` / `Ctrl+,`**: Open `Settings`
- **`Ctrl+Q`**: Quit application
