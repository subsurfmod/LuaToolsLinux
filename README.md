<div align="center">
  <img width="140" height="140" alt="LuaTools Linux" src="assets/luatools.svg" />
  <h1>LuaTools Linux</h1>
  <p><strong>Native Rust &amp; egui desktop suite for Linux &amp; Steam Deck — add Steam games, set up SLSsteam in one click, manage DLC &amp; depots, and apply game fixes.</strong></p>
  <p>
    <a href="https://github.com/subsurfmod/LuaToolsLinux/releases/latest">
      <img src="https://img.shields.io/badge/RELEASE-v4.1.3-1A3A6B?style=for-the-badge&logo=github&logoColor=white" alt="Release v4.1.3">
    </a>
    <img src="https://img.shields.io/badge/RUST-100%25%20NATIVE-DEA584?style=for-the-badge&logo=rust&logoColor=111111" alt="Rust">
    <img src="https://img.shields.io/badge/LINUX-x86__64%20%7C%20i686-FCC624?style=for-the-badge&logo=linux&logoColor=111111" alt="Linux x86_64 and i686">
    <img src="https://img.shields.io/badge/TESTS-631%20PASSING-2EA043?style=for-the-badge&logo=checkmarx&logoColor=white" alt="631 Passing Tests">
    <a href="LICENSE">
      <img src="https://img.shields.io/badge/LICENSE-GPL--3.0-blue?style=for-the-badge" alt="GPL-3.0 License">
    </a>
  </p>
  <p>
    <sub>Supports <strong>Steam Deck (SteamOS)</strong> · <strong>Fedora</strong> · <strong>Ubuntu / Debian</strong> · <strong>Arch Linux</strong> · <strong>Wayland &amp; X11</strong> · Native &amp; Flatpak Steam</sub>
  </p>
</div>

---

## Why LuaTools Linux v4.1.3?

**LuaTools Linux** is a fast, lightweight, native **Rust + egui** desktop application built specifically for Linux gaming and the Steam Deck. It brings the full LuaTools workflow to Linux without Wine overhead, combining a dual-architecture (`x86_64` + `i686`) Steam bridge, one-click rootless **SLSsteam** setup, a sandboxed Lua 5.4 runtime, depot/DLC management, and one-click game fixes in a single binary.

### Key v4.1.3 Highlights

- **Native `egui` Desktop App** — Crisp, responsive dark UI on both **Wayland** and **X11** with a streamlined English interface, instant progress/completion feedback on every button (`ActionProgressModal`), and live topbar Steam controls (`Launch Steam`, `Restart Steam`, `Kill Steam`).
- **Prebuilt Binary Auto-Updater & Live Compiler Feed** — Update in one click from the top-right **Update** badge or `luatools --update`, complete with SHA-256 release verification, monotonic progress tracking, and a live streaming monospace Rust compiler log when building from source.
- **Dual-Arch 32-bit & 64-bit Steam Bridge** — Ships both `libsteam_integration32.so` (`i686`) and `libsteam_integration64.so` (`x86_64`) with `ELFCLASS32`/`ELFCLASS64` pattern scanning, NetPacket protobuf codecs, Cloud Save Redirect, achievement/stat sync, and authoritative PID-lockfile Steam detection (native & Flatpak).
- **One-Click Rootless SLSsteam Setup** — Install, sync, and verify **SLSsteam** with zero root password required, plus integrated **Store Page Plugin (LTSP)**, **Millennium**, and **CEF** support.
- **Find Games, Manage Depots & Apply Fixes** — Search Steam games in **Add Games**, drag-and-drop `.lua` / `.manifest` / `.zip` files onto **Home**, toggle DLCs and saved `.lua` builds in **Depots & LuaVault**, unpack SteamStub DRM natively (`steamflatline`), and apply or revert **Game Fixes** with automatic backups.
- **First-Class Linux Packaging** — Ready-to-install `.deb`, `.rpm`, and portable `.tar.gz` bundles plus a one-line `install.sh` for **Steam Deck (SteamOS)**, **Fedora**, **Ubuntu / Debian / Pop!_OS**, and **Arch Linux**.

---

## Quick Start

1. **Setup Steam** — Click **Setup Steam** in the top-right bar (or open **Settings › Steam Setup**) and click **Install Steam Unlocker (SLSsteam)**.
2. **Add a Game** — Drag and drop a `.lua`, `.zip`, or `.manifest` file onto **Home**, or search any title/AppID in **Add Games** and click **Install Game**.
3. **Configure & Play** — Click **Open** on any added game on **Home** (or **Integrate** in **My Games**) for per-game fixes, AppID redirects, and launch options, then click **Launch Steam** or **Restart Steam** in the top-right bar.

---

## Installation

### One-Line Install (Fedora, Ubuntu, Arch Linux, Steam Deck / SteamOS)

```bash
curl -fsSL https://raw.githubusercontent.com/subsurfmod/LuaToolsLinux/main/install.sh | bash -s -- --yes
```

### Native Packages (`.deb` / `.rpm` / `.tar.gz`)

Download the latest v4.1.3 package from the [GitHub Releases](https://github.com/subsurfmod/LuaToolsLinux/releases/latest) page, or build packages locally with `./packaging/build-packages.sh`:

```bash
# Ubuntu / Debian / Pop!_OS
sudo apt install ./luatools-linux_4.1.3_amd64.deb

# Fedora / RHEL / openSUSE
sudo dnf install ./luatools-linux-4.1.3-1.*.rpm

# Portable Tarball (Arch Linux / Steam Deck / any distro)
tar -xzf luatools-linux-4.1.3-x86_64.tar.gz
cd luatools-linux-4.1.3-x86_64
./install.sh --yes
```

### Install from Source (Git)

```bash
git clone https://github.com/subsurfmod/LuaToolsLinux.git
cd LuaToolsLinux
./install.sh --yes
```

### Update & Uninstall

- **In-App Update**: Click **Update** in the top-right corner when a new release is detected (or open **Settings › Updates & Tools**).
- **CLI Update**:
  ```bash
  luatools --update
  ```
- **Uninstall**:
  ```bash
  curl -fsSL https://raw.githubusercontent.com/subsurfmod/LuaToolsLinux/main/uninstall.sh | bash
  ```

---

## Screens at a Glance

- **Home** — Drag and drop `.lua`, `.manifest`, or `.zip` files, inspect Steam / Unlocker / Account status tiles, and open recently added games directly into Integration or **Depots & DLC**.
- **Add Games** — Browse Top Sellers and New Releases or search by game name, AppID, or Steam URL to install manifests from available providers, with post-install shortcuts to Integration, Files & DLC, Game Fixes, and Launch Options.
- **My Games** — Filter and manage your library with 1-click **Integrate**, **Files & DLC**, and **Edit .lua** actions plus a **More** menu for Game Fixes, Launch Options, native SteamStub unpacking, and folder access.
- **Depots & Vault** — Toggle individual DLCs, save and switch `.lua` build presets in **LuaVault**, edit `.lua` files with live validation, pin manifest versions, and download depots.
- **Game Fixes** — Browse available fixes, apply them with automatic backups, or revert back to original files at any time.
- **Help** — Built-in visual guide, `.lua` reference templates, and troubleshooting tips.
- **Settings** — Configure Steam paths, cloud manifest account, 1-click **SLSsteam** setup, Cloud Save Redirect, Store Page Plugin (LTSP / Millennium), Advanced Steam Bridge (`i686`/`x86_64` pattern scanner & `inotify` hot-reload), live auto-updater, and diagnostics.

---

## Screenshots

### 1. Home
Drop `.lua`, `.manifest`, or `.zip` files, check Steam & unlocker status at a glance, and click **Open** on any added game.
![Home](assets/guide/01_home_overview.png)

### 2. Add Games
Search the Steam catalog, pick an available manifest provider, and click **Install Game**.
![Add Games](assets/guide/02_discover_store.png)

### 3. Depots & LuaVault
Switch saved `.lua` presets, toggle DLCs on or off, edit `.lua` files, and download depots.
![Depots & Vault](assets/guide/03_depots_vault_binding.png)

### 4. Per-Game Integration & Quick Setup Window
Click **Open** on an added game on **Home** (or **Integrate** in **My Games**) to apply recommended fixes, AppID redirects, depot keys, and launch settings in one click.
![Integration & Quick Setup Window](assets/guide/04_library_integration_modal.png)

### 5. Advanced Steam Bridge Settings
Per-launch 32-bit/64-bit Steam client integration, binary pattern scanner status, and live `.lua` folder hot-reload controls in **Settings › Advanced Bridge**.
![Advanced Steam Bridge](assets/guide/05_settings_steam_integration.png)

### 6. One-Click SLSsteam Setup
Install **SLSsteam**, sync unowned-game settings, and configure Cloud Save Redirect & Store Page Plugins in **Settings › Steam Setup**.
![Steam Setup](assets/guide/06_settings_wired_cloud.png)

---

## Keyboard Shortcuts

| Shortcut | Action |
| :--- | :--- |
| `Ctrl+K` / `Ctrl+L` | Focus top search bar (game name, AppID, Steam URL, or file path) |
| `Ctrl+1` … `Ctrl+5` | Switch between `Home`, `Add Games`, `My Games`, `Depots & Vault`, and `Game Fixes` |
| `Ctrl+6` / `F1` | Open `Help` (`Esc` closes open modals and previews) |
| `Ctrl+7` / `Ctrl+,` | Open `Settings` |
| `Ctrl+Q` | Quit LuaTools |

---

## License

Licensed under the **GNU General Public License v3.0 or later (`GPL-3.0-or-later`)** — see [LICENSE](LICENSE) for full terms.

## Credits & Acknowledgments

LuaTools Linux is built on the shoulders of giants. This project incorporates ideas, protocols, and inspiration from the following open-source projects:

- **BetterSteamTools (BST)** — https://github.com/madoiscool/BetterSteamTools *(Lua format, `addappid`/`setManifestid` semantics, protocol definitions)*
- **LuaToolsSteamPlugin (LTSP)** — https://github.com/madoiscool/LuaToolsSteamPlugin *(Store page plugin & browser protocol handlers)*
- **SLSsteam / AceSLS** — https://github.com/acedio/slssteam *(Linux Steam client hooking, `app_id` spoofing, plugin loading)*
- **Steamless / steamflatline** — https://github.com/atom0s/Steamless *(SteamStub DRM unpacker v1.0–v3.1 reference)*
- **DepotDownloaderMod** — https://github.com/SteamAutoCracks/DepotDownloaderMod *(Depot downloading with `manifestfile`/`depotkey` support)*
- **SteamAutoPatch** — https://github.com/SteamAutoCracks/Steam-auto-crack *(Automated Goldberg emulator configuration)*
- **Goldberg Emulator** — https://github.com/Detanup01/gbe_fork *(Native Linux Steamworks API replacement)*
- **SteamRE/DepotDownloader** — https://github.com/SteamRE/DepotDownloader *(Binary manifest format reference)*

### Rust Ecosystem

Built with `eframe`, `egui`, `mlua`, `ureq`, `tiny_http`, `tungstenite`, `serde`, `serde_json`, `aes`, `keyring`, `image`, and the crates listed in [Cargo.toml](Cargo.toml). For the complete list of third-party attributions, see [CREDITS.md](CREDITS.md).
