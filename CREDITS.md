# Credits & Third-Party Dependencies

LuaTools Linux is built on the shoulders of giants. This project incorporates code, ideas, and inspiration from the following open-source repositories and projects. We are deeply grateful to their authors and maintainers.

## Core Upstream Projects

### Steam & SLSsteam Ecosystem
- **SLSsteam / AceSLS** — Steam client hooking framework for Linux
  - Repository: https://github.com/acedio/slssteam (or similar)
  - Used for: Steam client integration, app_id spoofing, plugin loading

- **BetterSteamTools (BST)** — Original Windows LuaTools implementation
  - Repository: https://github.com/madoiscool/BetterSteamTools
  - Used for: Core Lua file format, addappid/setManifestid semantics, protocol definitions

- **LuaToolsSteamPlugin (LTSP)** — Steam Store page integration
  - Repository: https://github.com/madoiscool/LuaToolsSteamPlugin
  - Used for: Store page plugin, browser protocol handlers

### Steam Stub / DRM Removal
- **steamflatline / Steamless** — SteamStub unpacker
  - Repository: https://github.com/atom0s/Steamless
  - Used for: SteamStub DRM removal (variants v1.0, v2.0, v3.0, v3.1)

- **SteamAutoCracks/DepotDownloaderMod** — High-performance depot downloader
  - Repository: https://github.com/SteamAutoCracks/DepotDownloaderMod
  - Used for: Depot downloading with manifestfile/depotkey support

- **SteamAutoCracks/Steam-auto-crack** — SteamAutoPatch CLI
  - Repository: https://github.com/SteamAutoCracks/Steam-auto-crack
  - Used for: Automated Goldberg emulator configuration

- **Goldberg Emulator** — Steamworks API replacement
  - Repository: https://github.com/Detanup01/gbe_fork
  - Used for: Native Linux Steam API replacement (libsteam_api.so)

### Manifest & Depot Parsing
- **SteamRE/DepotDownloader** — Original depot downloader
  - Repository: https://github.com/SteamRE/DepotDownloader
  - Used for: Manifest binary format reference

- **ValveProtobuf** — Steam protocol buffer definitions
  - Various community repositories
  - Used for: Manifest protobuf structure reference

## Rust Crates (Direct Dependencies)

### GUI & Application Framework
- **eframe** (https://github.com/emilk/egui/tree/master/crates/eframe) — Native GUI framework
- **egui** (https://github.com/emilk/egui) — Immediate mode GUI
- **egui_extras** — Extended egui widgets

### Networking & HTTP
- **ureq** (https://github.com/algesten/ureq) — Simple HTTP client
- **tiny_http** (https://github.com/sile/tinyhttp) — Lightweight HTTP server
- **tungstenite** (https://github.com/snapview/tungstenite-rs) — WebSocket client
- **url** (https://github.com/servo/rust-url) — URL parsing

### Serialization & Data
- **serde / serde_json / serde_yml** (https://github.com/serde-rs) — Serialization
- **toml** (https://github.com/toml-rs/toml) — TOML parsing

### Cryptography & Hashing
- **sha2 / sha1 / md5** (https://github.com/RustCrypto/hashes) — Hash algorithms
- **aes / cbc / cipher** (https://github.com/RustCrypto/block-ciphers) — AES encryption
- **xtea** — XTEA cipher (SteamStub)
- **base64 / hex** — Encoding

### System & Utilities
- **libc** (https://github.com/rust-lang/libc) — C library bindings
- **inotify** (https://github.com/atlas-rs/inotify-rs) — Filesystem watching
- **rand** (https://github.com/rust-random/rand) — Random number generation
- **walkdir** (https://github.com/BurntSushi/walkdir) — Directory traversal
- **regex** (https://github.com/rust-lang/regex) — Regular expressions
- **chrono** (https://github.com/chronotope/chrono) — Date/time handling
- **thiserror** (https://github.com/dtolnay/thiserror) — Error derive
- **walkdir_lite** — Lightweight walkdir alternative

### Keyring & Credentials
- **keyring** (https://github.com/hwchen/keyring-rs) — OS credential storage
  - Uses: libsecret (GNOME), KWallet (KDE), Windows Credential Manager, macOS Keychain

### Image Processing
- **image** (https://github.com/image-rs/image) — Image loading/saving

### Other
- **dirs** (https://github.com/dirs-dev/dirs-rs) — Platform-specific directories
- **tempfile** (https://github.com/Stebalien/tempfile) — Temporary files (dev)
- **zip** (https://github.com/zip-rs/zip) — ZIP archive handling

## Special Thanks & Inspiration

### Steam Community Projects
- **SteamAutoCracks** organization — For DepotDownloaderMod and Steam-auto-crack
- **SteamRE** organization — For reverse engineering documentation
- **madoiscool** — For BetterSteamTools and LuaToolsSteamPlugin
- **acedio** — For SLSsteam/AceSLS
- **atom0s** — For Steamless/SteamStub research

### Individual Contributors
- All contributors to the Rust crates listed above
- The Steam homebrew/modding community for documenting protocols
- GitHub security researchers who disclosed Steam client vulnerabilities

## License Compliance

This project is licensed under **GPL-3.0-or-later**.

### GPL Compatibility Notes:
- All direct dependencies are MIT, Apache-2.0, or BSD licensed (GPL-compatible)
- LGPL dependencies (if any) are dynamically linked
- No proprietary or non-GPL-compatible code is included

### Attribution Requirements:
- This CREDITS.md file satisfies attribution requirements for MIT/BSD/Apache dependencies
- GPL-3.0 requires source distribution — this repository is the complete corresponding source
- Keyring crate uses system keyring APIs (LGPL/GPL compatible system libraries)

## Building from Source

See `README.md` for build instructions. All dependencies are fetched via Cargo from crates.io.

---

*Generated with gratitude for the open-source ecosystem. If we missed anyone, please open an issue!*