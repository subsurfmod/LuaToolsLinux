# LuaTools Linux

A native Linux port of [LuaTools](https://github.com/madoiscool/LuaTools) for managing Steam manifest/lua configurations. Built with Python and PyQt6 for Fedora Linux (and other distributions).

## Features

- 🎮 **Games View** - Browse installed Steam games with LuaTools integration
- 📦 **Depots View** - Variant switcher, depot table, live Lua editor, download picker
- 🔧 **Fixes View** - Game fixes database with progress tracking
- 🛠 **External Tools** - SteamAutoCrack, Steamless, DepotDownloaderMod management
- 📂 **Drag & Drop** - Drop `.lua` files to auto-install and sync with SLSsteam
- 🔄 **Auto-sync** - `.lua` → SLSsteam config.yaml (AdditionalApps, FakeAppIds, AppTokens, DepotKeys)
- 🌐 **Millennium RPC** - Communicates with LuaTools plugin on port 38495

## Screenshots

*Coming soon*

## Requirements

- Fedora Linux (or similar - tested on Fedora 45+)
- Python 3.10+
- PyQt6 (`sudo dnf install python3-pyqt6`)
- Native Steam installation
- Millennium v3.6+ (for plugin integration)
- ACCELA + SLSsteam (for game downloading)

## Installation

```bash
# Clone the repository
git clone https://github.com/yourusername/LuaToolsLinux.git
cd LuaToolsLinux

# Install in development mode
pip install --user -e .

# Run the GUI
luatools
```

### Dependencies

```bash
# Fedora
sudo dnf install python3-pyqt6 python3-httpx python3-beautifulsoup4 python3-ruamel-yaml

# Arch
sudo pacman -S python-pyqt6 python-httpx python-beautifulsoup4 python-ruamel-yaml

# Ubuntu/Debian
sudo apt install python3-pyqt6 python3-httpx python3-bs4 python3-ruamel-yaml
```

## Usage

### Adding Games (Drag & Drop)
1. Run `luatools`
2. Go to **Games View** (Ctrl+1)
3. **Drag** a `.lua` file onto the Games View table
3. The app will:
   - Extract AppID from `addappid(AppID)` in the lua file
   - Create `~/.local/share/Steam/config/stplug-in/<AppID>.lua`
   - Sync with SLSsteam config.yaml (AdditionalApps, FakeAppIds, AppTokens, DepotKeys)
   - Refresh the game list automatically

### Adding Games (Manual)
1. **Games View (Ctrl+1)** → Click **"Add Game (by AppID)"**
2. Enter the Steam AppID (e.g., `1245620` for Elden Ring)
3. Choose a template (Empty, With DLCs, With depot pinning)
4. Click OK → lua file created + SLSsteam synced

### Managing Depots
1. **Games View** → Click **"Depots"** for a game
2. **Depots View (Ctrl+2)** shows:
   - **Variant Switcher** - Apply/save/delete variants
   - **Depot Table** - Pins, enable/disable, details
   - **Live Editor** - Edit lua directly
   - **Download Picker** - Download depots with progress

### External Tools
1. **Tools View (Ctrl+4)** - Manage:
   - DepotDownloaderMod (auto-updates from GitHub)
   - SteamAutoCrack (launch for game)
   - Steamless (remove SteamStub DRM)

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                      LuaTools Linux GUI                         │
│  (PyQt6 App - luatools command)                                 │
│  - Games View: installed games table                            │
│  - Depots View: variant switcher, depot table, editor          │
│  - Fixes View: game fixes database                              │
│  - Tools View: external tools management                        │
└──────────────────────────┬──────────────────────────────────────┘
                           │ RPC (port 38495)
                           ▼
┌─────────────────────────────────────────────────────────────────┐
│              LuaTools Millennium Plugin                         │
│  ~/.local/share/millennium/plugins/luatools/                    │
│  - main.lua: plugin entry point                                 │
│  - web_bridge_server.py: HTTP/RPC server                        │
│  - public/luatools.js: injected into Steam                      │
│  - .venv/: Python deps (httpx, bs4, ruamel.yaml)               │
└──────────────────────────┬──────────────────────────────────────┘
                           │ LD_PRELOAD hooks
                           ▼
┌─────────────────────────────────────────────────────────────────┐
│                    Steam + Millennium                           │
│  - Steam: ~/.local/share/Steam                                  │
│  - Millennium: /usr/lib/millennium/                             │
│  - stplug-in lua files: ~/.local/share/Steam/config/stplug-in/ │
└─────────────────────────────────────────────────────────────────┘
```

## Configuration Files

| File | Purpose |
|------|---------|
| `~/.local/share/Steam/config/stplug-in/<appid>.lua` | Game depot configuration |
| `~/.config/SLSsteam/config.yaml` | SLSsteam settings (AdditionalApps, FakeAppIds, etc.) |
| `~/.local/share/ACCELA/ACCELA.AppImage` | ACCELA launcher |
| `~/.local/share/SLSsteam/SLSsteam.so` | SLSsteam runtime |

## Known Limitations

1. **Steam CSP blocks JS injection** - The LuaTools button in Steam's top nav is blocked by Content Security Policy on Linux. Use the native GUI app instead.
2. **DepotDownloaderMod** - Requires Linux-native build (currently uses .NET)
3. **Anonymous downloads** - Some depots require Steam login for manifest fetch
4. **Fixes database** - Currently empty, needs population

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Submit a pull request

## License

MIT License - see [LICENSE](LICENSE) for details.

## Credits

- [LuaTools](https://github.com/madoiscool/LuaTools) - Original Windows version by madoiscool
- [Millennium](https://steambrew.app) - Steam plugin framework
- [SLSsteam](https://github.com/AceSLS/SLSsteam) - Steam Linux Runtime
- [ACCELA](https://github.com/ciscosweater/enter-the-wired) - Game downloader
- [DepotDownloaderMod](https://github.com/SteamAutoCracks/DepotDownloaderMod) - Depot downloader
- [Millennium](https://steambrew.app) - Steam plugin framework

## Support

- [Discord](https://discord.gg/luatools) - LuaTools community
- [Issues](https://github.com/yourusername/LuaToolsLinux/issues) - Bug reports & feature requests

---

**Note**: This is a Linux port of the Windows LuaTools application. The core Steam plugin (Millennium) and bridge server are adapted from the original LuaTools project.