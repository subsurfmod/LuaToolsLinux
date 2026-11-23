# LuaTools Linux v2.0-beta (Native Rust Edition)

A complete **100% Pure-Rust** ground-up rewrite of LuaTools Linux — engineered for minimal memory footprint (`~25–40 MB` RAM), instant `< 40 ms` startup, zero Python/Qt runtime dependencies, and a tactile **Papercut Monotone-Dark + Deep Sapphire** hardware-accelerated native GUI (`eframe` / `egui`).

---

## One-Command Installation (Includes All Dependencies)

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
- **Fedora / RHEL / openSUSE**: `sudo dnf install ./dist/luatools-linux-2.0-0.1.beta.*.rpm`
- **Ubuntu / Debian / Pop!_OS**: `sudo apt install ./dist/luatools-linux_2.0-beta_amd64.deb`
- **Any Linux Distro (No root required)**: extract `dist/luatools-linux-2.0-beta-x86_64.tar.gz` and run `./install.sh --yes --skip-deps`

---

## Full v2.0-beta Feature Parity

| Workspace | Capabilities |
|-----------|--------------|
| **Home** | Taped overview header, Rem Drop Sheet drag-and-drop ingest (`lua`/`manifest`/`zip`), 3-column live status summary (`Steam Engine`, `Manifest Providers`, `SLSsteam & Bridge`), recent configured titles with Steam cover art |
| **Discover** | Unified Steam Store search, Featured (`Top Sellers` / `New Releases`), multi-provider manifest checks, Hubcap integration, FastFetch auto-install, DLC `.lua` generator |
| **Library** | Multi-library ACF + `stplug-in` scanner, live filter chips (`All`, `Lua Ready`, `Installed`, `Overrides`), bulk `.lua` cleanup, per-game Configure & binary `appinfo.vdf` Launch Options |
| **Depots & Vault** | Sub-tabbed workspace (`Manifests & Vault`, `Lua Editor`, `Download Depots`) with content-addressed `LuaVault` (`~/.local/share/luatools/luavault` — 100% compatible with v1.x), syntax-highlighted Lua editor, `Pin` / `Unpin` manifest toggling, and `DepotDownloaderMod` / `DepotDownloader` multi-depot picker |
| **Game Fixes** | `lua.tools` game & Denuvo fixes catalog, category filters, SHA-256 `.luatools-fix/` backup & verified revert |
| **Settings** | 4 focused sub-tabs (`General & Steam`, `Account & Providers`, `SLSsteam & Store Bridge`, `Updates & Toolchain`) covering Discord PKCE OAuth (`:53789`) & Bot Code login, Hubcap API key validator, Steam path override, lossless `SLSsteam` `config.yaml` editor, Millennium `:38495` RPC, Steam CEF `:8080` WebSocket injector, local `:6767` RPC bridge, `luatools://` handler, external tools (`DepotDownloaderMod`, `SteamAutoCrack`, `Steamless`), and **Git Self-Updater** |

---

## Effortless Git Self-Updates

- On startup, `luatools` runs **one lightweight non-blocking background check** against your Git remote (`origin` or a custom Git URL configured in `Settings → Updates & Toolchain`).
- When new commits or a newer version exist upstream, a compact **`⬆ Update`** button appears in the **bottom-right corner** of the window.
- Clicking **`⬆ Update`** pulls the latest changes (`git pull`), compiles an optimized native release binary, atomically replaces the installed binary without `ETXTBSY` locks, and turns into **`↻ Restart`** for a 1-click relaunch (also available via CLI as `luatools --update`).

---

## Keyboard Shortcuts

- **`Ctrl+K` / `Ctrl+L`**: Focus Global Quick Command Bar (AppID, Steam/SteamDB URL, `luatools://`, or Store Search)
- **`Ctrl+1` … `Ctrl+6`**: Switch directly between `Home`, `Discover`, `Library`, `Depots & Vault`, `Game Fixes`, and `Settings`
- **`Ctrl+Q`**: Quit application
