# LuaTools Linux v2.5.1 (Native Rust Edition)

A complete rewrite of LuaTools Linux.  (`eframe` / `egui`).

---

## Installation (Includes All Dependencies)

Run the universal `curl` one-liner — it automatically verifies/installs required Linux graphics & XDG runtime packages (`dnf` / `apt` / `pacman` / `zypper`), clones the repository into `~/.local/share/luatools/repo`, bootstraps `rustup` if needed to compile an LTO-stripped native executable, installs the app icon & desktop launcher, and registers the `luatools://` URI scheme:

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

## Building Native `.rpm`, `.deb`, and Portable `.tar.gz` Packages

```bash
./packaging/build-packages.sh
```

Outputs self-contained distributables into `dist/`:
- **Fedora / RHEL / openSUSE**: `sudo dnf install ./dist/luatools-linux-2.5.1-1.*.rpm`
- **Ubuntu / Debian / Pop!_OS**: `sudo apt install ./dist/luatools-linux_2.5.1_amd64.deb`
- **Any Linux Distro (No root required)**: extract `dist/luatools-linux-2.5.1-x86_64.tar.gz` and run `./install.sh --yes --skip-deps`

---

## Full v2.5.1 Feature Parity

| Workspace | Capabilities |
|-----------|--------------|
| **Home** | Taped overview header, Rem Drop Sheet drag-and-drop ingest (`lua`/`manifest`/`zip`), 3-column live status summary, live **`stplug-in/` Hot-Reload Watcher & Steam Bridge** status bar, recent configured titles with Steam cover art |
| **Discover** | Unified Steam Store search, Featured (`Top Sellers` / `New Releases`), multi-provider manifest checks, Hubcap integration, FastFetch auto-install, DLC `.lua` generator, 1-click **Add to Steam Library** (`appmanifest_<appid>.acf`) |
| **Library** | Multi-library ACF + `stplug-in` scanner, live filter chips, bulk `.lua` cleanup, per-game Configure, **Integration Actions** (Redirect Set, `480` AppID Spoof, Legacy CD Keys, Depot Keys, Steam Stats, and per-game `LD_PRELOAD` Process Integration), binary `appinfo.vdf` Launch Options, **Steam Library ACF Registration**, **Play Unowned**, and **SteamAutoPatch** |
| **Depots & Vault** | 4-tab workspace (`Manifests & Vault`, `Lua Editor`, `Download Depots`, **`Manifest Binding`**) with content-addressed `LuaVault`, syntax-highlighted Lua editor, `Pin` / `Unpin` manifest toggling, IPC **Manifest Binding** enforcement, and crash-hardened `DepotDownloader` with `-depotkeys` & QR sign-in |
| **Game Fixes** | `lua.tools` game & DRM fixes catalog, category filters, SHA-256 `.luatools-fix/` backup & verified revert |
| **How to Use** | Interactive 5-part workstation manual covering Quick Start, `.lua`/`.manifest`/`.zip` ingest, annotated `.lua` syntax, `LuaVault` & Manifest Binding, `DepotDownloader` QR sessions, Wired/Steam Integration setup, `appinfo.vdf` Launch Options, and troubleshooting |
| **Settings** | 6 focused sub-tabs (`General & Steam`, `Account & Providers`, `SLSsteam & Store Bridge`, **`Steam Integration`**, `Updates & Toolchain`, `Telemetry & Logs`) covering Discord OAuth & Bot Code login, Hubcap API key validator, **Wired Suite (`ACCELA` + `SLSsteam`)**, **Cloud Redirect**, **`libsteam_integration.so` ELF64/Pattern/IPC Interop + `inotify` Hot-Reload**, external tools, **Git Self-Updater**, and **Privacy-First Opt-Out Bug Telemetry** |

---

## Effortless Git Self-Updates

- On startup, `luatools` runs **one lightweight non-blocking background check** against your Git remote (`origin` or a custom Git URL configured in `Settings → Updates & Toolchain`).
- When new commits or a newer version exist upstream, a compact **`⬆ Update`** button appears in the **top-right corner** of the window.
- Clicking **`⬆ Update`** pulls the latest changes (`git pull`), compiles an optimized native release binary, atomically replaces the installed binary without `ETXTBSY` locks, and turns into **`↻ Restart`** for a 1-click relaunch (also available via CLI as `luatools --update`).

---

## Keyboard Shortcuts

- **`Ctrl+K` / `Ctrl+L`**: Focus Global Quick Command Bar (AppID, Steam/SteamDB URL, `luatools://`, or Store Search)
- **`Ctrl+1` … `Ctrl+5`**: Switch between `Home`, `Discover`, `Library`, `Depots & Vault`, and `Game Fixes`
- **`Ctrl+6` / `F1`**: Open the `How to Use` interactive manual
- **`Ctrl+7` / `Ctrl+,`**: Open `Settings`
- **`Ctrl+Q`**: Quit application
