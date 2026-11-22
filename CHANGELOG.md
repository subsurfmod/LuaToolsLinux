# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] - 2024-11-23

### Added
- Initial release of LuaTools Linux
- **Games View** - Installed games table with Edit, Depots, Add Game buttons
- **Depots View** - Variant switcher, depot table, live editor, download picker
- **Fixes View** - Game fixes database with progress tracking
- **External Tools View** - SteamAutoCrack, Steamless, DepotDownloaderMod management
- **Drag & Drop** - Drop `.lua` files to auto-install and sync with SLSsteam
- **Lua Vault** - Content-addressed storage for all lua variants
- **Lua Parser** - Full addappid/setManifestid parsing with diff support
- **SteamDepotInfo** - Fetches depot metadata from api.steamcmd.net
- **DepotDownloaderService** - Auto-updates DepotDownloaderMod, downloads depots
- **ExternalToolsManager** - Auto-updates SteamAutoCrack, Steamless, DepotDownloaderMod
- **SLSsteam Sync** - Automatic sync of .lua → SLSsteam config.yaml

### Fixed
- **DepotDownloaderService** - Fixed `-app` argument (uses game's app_id)
- **DepotDownloaderService** - Fixed key resolution (uses game's app_id)
- **SlssteamService** - Fixed key casing (PlayNotOwnedGames, LimitDownloadsToSteamLibrary)
- **SlssteamService** - Added AdditionalApps and DepotKeys support

### Known Issues
- Steam CSP blocks JS injection (button in Steam UI)
- DepotDownloaderMod requires Linux-native build
- Anonymous downloads may fail for some depots
- Fixes database needs population

## [Unreleased]

### Planned
- SteamAutoCrack/Steamless full UI integration
- Plugin Installer for Millennium coexistence
- Settings View completion
- Millennium Plugin Auto-Update
- Fixes database population