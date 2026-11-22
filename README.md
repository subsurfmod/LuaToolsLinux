# LuaTools Linux v2.0-beta (Native Rust Edition)

A complete **100% Pure-Rust** ground-up rewrite of LuaTools Linux — engineered for minimal memory footprint (`~25–40 MB` RAM), instant `< 40 ms` startup, zero Python/Qt runtime dependencies, and a razor-sharp **0px Monotone + Cobalt Blue** hardware-accelerated native GUI (`eframe` / `egui`).

---

## One-Command Installation (Includes All Dependencies)

Run the bundled universal installer — it automatically verifies/installs required Linux graphics & XDG runtime packages (`dnf` / `apt` / `pacman` / `zypper`), uses a prebuilt binary or bootstraps `rustup` to compile an LTO-stripped native executable, installs the app icon & desktop launcher, and registers the `luatools://` URI scheme:

```bash
cd /home/slave/Documents/luatools-linux-v2
./install.sh --yes
```

- **System-wide install (`/usr/local/bin/luatools`)**: `./install.sh --yes --system`
- **Include optional Wine + .NET runtimes** (for Windows tools like Steamless / SteamAutoCrack): `./install.sh --yes --with-wine`
- **Uninstall cleanly**: `./uninstall.sh` (or `./uninstall.sh --purge` to also remove cached data)

---

## Building Native `.rpm`, `.deb`, and Portable `.tar.gz` Packages

```bash
./packaging/build-packages.sh
```

Outputs self-contained distributables into `dist/`:
- **Fedora / RHEL / openSUSE**: `sudo dnf install ./dist/luatools-linux-2.0-0.1.beta.*.rpm`
- **Ubuntu / Debian / Pop!_OS**: `sudo apt install ./dist/luatools-linux_2.0-beta_amd64.deb`
- **Any Linux Distro (No root required)**: extract `dist/luatools-linux-2.0-beta-x86_64.tar.gz` and run `./install.sh --yes --skip-deps`

---

## Full v2.0-beta Feature Parity

| Module | Capabilities |
|--------|--------------|
| **01 // Overview** | Live telemetry cards (`STEAM ENGINE`, `LUATOOLS ACCOUNT`, `SLSSTEAM UNLOCKER`, `BRIDGE & INJECTOR`), drag-and-drop ingest zone, recent configured titles with Steam cover art |
| **02 // Acquire** | Steam Store search, Featured (`Top Sellers` / `New Releases`), multi-provider manifest checks, Hubcap integration, FastFetch auto-install, DLC `.lua` generator |
| **03 // Library** | Multi-library ACF + `stplug-in` scanner, live filter chips (`ALL`, `LUA ACTIVE`, `INSTALLED`, `OVERRIDES`), bulk `.lua` delete, per-game Config & binary `appinfo.vdf` Launch Options |
| **04 // Depots & Vault** | Content-addressed `LuaVault` (`~/.local/share/luatools/luavault` — 100% compatible with v1.x), inline Active Title switcher, syntax-highlighted Live Lua Workbench, `PIN` / `UNPIN` manifest toggling, and `DepotDownloaderMod` / `DepotDownloader` multi-depot picker |
| **05 // Fix Catalog** | `lua.tools` game & Denuvo fixes catalog, tag chips, SHA-256 `.luatools-fix/` backup & verified revert |
| **06 // Toolchain** | Local disk inspector & GitHub release updater for `DepotDownloaderMod`, `SteamAutoCrack`, and `Steamless` (`Native Linux`, `Wine`, `dotnet` wrappers) |
| **07 // System** | Discord PKCE OAuth (`:53789`) & 6-char Bot Code login, Hubcap API key validator, Steam path override, lossless `SLSsteam` `config.yaml` editor, Millennium `:38495` RPC, Steam CEF `:8080` WebSocket injector, local `:6767` RPC bridge, `luatools://` handler, and **Git Self-Updater** |

---

## Effortless Git Self-Updates

- On startup, `luatools` runs **one lightweight non-blocking background check** against your Git remote (`origin` or a custom Git URL configured in `07 // SYSTEM`).
- When new commits or a newer version exist upstream, a compact **`⬆ UPDATE`** button appears in the **bottom-right corner** of the window.
- Clicking **`⬆ UPDATE`** pulls the latest changes (`git pull`), compiles an optimized native release binary, atomically replaces the installed binary without `ETXTBSY` locks, and turns into **`↻ RESTART`** for a 1-click relaunch (also available via CLI as `luatools --update`).

---

## Keyboard Shortcuts

- **`Ctrl+K` / `Ctrl+L`**: Focus Global Quick Command Bar (AppID, Steam/SteamDB URL, `luatools://`, or Store Search)
- **`Ctrl+1` … `Ctrl+7`**: Switch directly between views `01` through `07`
- **`Ctrl+Q`**: Quit application
