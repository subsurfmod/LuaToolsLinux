"""Lua Installer - installs lua files and syncs with SLSsteam config.yaml."""

from __future__ import annotations

import re
import shutil
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

from ..models.steam import SteamInstallation
from ..parsers.lua_parser import parse as parse_lua, LuaContents, LuaEntry
from .slssteam_service import SlssteamService


@dataclass
class InstallResult:
    """Result of a lua installation."""
    ok: bool
    error: Optional[str] = None
    app_id: Optional[int] = None
    lua_path: Optional[str] = None


class LuaInstaller:
    """Installs lua files and syncs with SLSsteam config.yaml."""

    def __init__(
        self,
        installation,
        settings_service=None,
        lua_vault=None,
    ) -> None:
        # Support both calling conventions:
        # - (steam_installation, slssteam_service) - from games_view
        # - (installation, settings_service, lua_vault) - from main_window
        if isinstance(installation, SteamInstallation):
            # Called with (steam_installation, slssteam_service) or (installation, settings_service, lua_vault)
            if isinstance(settings_service, SlssteamService):
                # (steam_installation, slssteam_service)
                self.steam_installation = installation
                self.slssteam_service = settings_service
            else:
                # (installation, settings_service, lua_vault)
                self.steam_installation = installation
                self.settings_service = settings_service
                self.lua_vault = lua_vault
                # Get slssteam_service from settings_service if available
                self.slssteam_service = getattr(settings_service, 'slssteam_service', None)
        else:
            # Fallback
            self.steam_installation = installation
            self.slssteam_service = settings_service

    @property
    def stplug_in_dir(self) -> Path:
        """Get the stplug-in directory path."""
        return self.steam_installation.stplug_in_path

    def install_lua(
        self,
        app_id: int,
        lua_content: str,
        add_to_slssteam: bool = True,
        add_fake_app_id: bool = True,
        add_token: bool = False,
        add_additional_app: bool = True,
        fake_app_depot_id: int = 480,
    ) -> InstallResult:
        """
        Install a lua file and optionally sync with SLSsteam config.yaml.

        Args:
            app_id: The Steam AppID
            lua_content: The lua file content
            add_to_slssteam: Whether to sync with SLSsteam config.yaml
            add_fake_app_id: Whether to add FakeAppId entry (default depot 480)
            add_token: Whether to add token from lua (if present)
            add_additional_app: Whether to add to SLSsteam's AdditionalApps list
            fake_app_depot_id: Depot ID for FakeAppId (default 480)

        Returns:
            InstallResult with success status and details
        """
        stplug_dir = self.stplug_in_dir
        stplug_dir.mkdir(parents=True, exist_ok=True)

        lua_path = stplug_dir / f"{app_id}.lua"

        # Write the lua file
        try:
            lua_path.write_text(lua_content, encoding="utf-8")
        except Exception as e:
            return InstallResult(ok=False, error=f"Failed to write lua file: {e}", app_id=app_id)

        # Parse lua to extract tokens, keys, DLCs, etc.
        lua = parse_lua(lua_content, app_id)

        # Sync with SLSsteam config.yaml if requested
        if add_to_slssteam:
            self._sync_with_slssteam(app_id, lua, add_fake_app_id, add_token, add_additional_app, 480)

        # Notify listeners of successful installation
            self._notify_installed(app_id)
            return InstallResult(ok=True, app_id=app_id, lua_path=str(lua_path))

    # --- Event listeners for MainWindow compatibility ---
    def add_installed_listener(self, callback) -> None:
        """Add a callback to be called when a game is installed."""
        if not hasattr(self, '_installed_listeners'):
            self._installed_listeners = []
        self._installed_listeners.append(callback)

    def _notify_installed(self, app_id: int) -> None:
        """Notify all listeners that a game was installed."""
        if hasattr(self, '_installed_listeners'):
            for callback in self._installed_listeners:
                try:
                    callback(app_id)
                except Exception:
                    pass

    def _sync_with_slssteam(
        self,
        app_id: int,
        lua: Optional[object],
        add_fake_app_id: bool,
        add_token: bool,
        add_additional_app: bool,
        fake_app_depot_id: int,
    ) -> None:
        """Sync lua data with SLSsteam config.yaml."""
        # Parse lua if not already parsed
        if lua is None:
            stplug_dir = self.stplug_in_dir
            lua_file = stplug_dir / f"{app_id}.lua"
            if lua_file.exists():
                from ..parsers.lua_parser import parse as parse_lua
                lua = parse_lua(lua_file, app_id)

        if lua is None:
            return

        # 1. Add to AdditionalApps list
        self.slssteam_service.add_additional_app(app_id)

        # 2. Add FakeAppId (default depot 480)
        if not self.slssteam_service.has_fake_app_id(app_id):
            self.slssteam_service.set_fake_app_id(app_id, 480)

        # 3. Add FakeAppId for each depot with a key in the lua
        for entry in lua.entries:
            if entry.has_key:
                self.slssteam_service.set_fake_app_id(entry.id, 480)

        # 4. Add tokens from lua
        for entry in lua.entries:
            if entry.key:
                self.slssteam_service.set_depot_key(entry.id, entry.key)

        # 5. Add tokens from lua entries (AppTokens)
        # Note: AppTokens in SLSsteam are per-app, not per-depot
        # We'll add the first token found as the app token
        first_key = None
        for entry in lua.entries:
            if entry.key:
                first_key = entry.key
                break
        if first_key:
            self.slssteam_service.set_token(app_id, first_key)

        # 6. Add to AdditionalApps
        self.slssteam_service.add_additional_app(app_id)

        # 6. Add DLC data if present
        if hasattr(lua, 'entries'):
            for entry in lua.entries:
                if not entry.has_key and entry.id != app_id:
                    # This is a DLC entry
                    self.slssteam_service.config.dlc_data.setdefault(app_id, {})[entry.id] = entry.comment or f"DLC {entry.id}"

        # 7. Save DepotKeys from lua
        for entry in lua.entries:
            if entry.key:
                # Save depot key in DepotKeys section
                from ..services.slssteam_service import SlssteamService
                # Use internal method to set depot key
                self.slssteam_service.set_depot_key(entry.id, entry.key)

        # Save the config
        self.slssteam_service.save()

    def install_from_file(
        self,
        source_path: Path,
        app_id: int,
        **kwargs
    ) -> InstallResult:
        """Install lua from an existing file."""
        if not source_path.exists():
            return InstallResult(ok=False, error=f"Source file not found: {source_path}")

        content = source_path.read_text(encoding="utf-8", errors="replace")
        return self.install_lua(app_id, content, **kwargs)

    def uninstall_lua(self, app_id: int, remove_from_slssteam: bool = False) -> InstallResult:
        """Remove a lua file and optionally clean up SLSsteam config."""
        lua_path = self.stplug_in_dir / f"{app_id}.lua"
        if not lua_path.exists():
            return InstallResult(ok=False, error=f"Lua file not found for AppID {app_id}")

        try:
            lua_path.unlink()
        except Exception as e:
            return InstallResult(ok=False, error=f"Failed to delete lua file: {e}")

        if remove_from_slssteam:
            # Remove from SLSsteam config
            # Note: We keep the config entries by default to preserve user settings
            pass

        return InstallResult(ok=True, app_id=app_id, lua_path=None)

    def get_installed_games(self) -> list[int]:
        """Get list of AppIDs with installed lua files."""
        if not self.stplug_in_dir.exists():
            return []
        return sorted(
            int(f.stem) for f in self.stplug_in_dir.glob("*.lua")
            if f.stem.isdigit()
        )

    def enumerate_installed(self) -> list[int]:
        """Alias for get_installed_games for compatibility."""
        return self.get_installed_games()

    def read_lua(self, app_id: int) -> Optional[str]:
        """Read the lua file content for an app."""
        lua_path = self.stplug_in_dir / f"{app_id}.lua"
        if lua_path.exists():
            return lua_path.read_text(encoding="utf-8", errors="replace")
        return None


class SteamLinkParser:
    """Parses Steam/SteamDB URLs and text to extract AppIDs."""

    # URL patterns for various Steam/SteamDB URLs
    STEAM_STORE_PATTERN = r'store\.steampowered\.com/(?:app|sub)/(\d+)'
    STEAMDB_PATTERN = r'steamdb\.info/(?:app|sub)/(\d+)'
    STEAM_COMMUNITY_PATTERN = r'steamcommunity\.com/(?:app|sub)/(\d+)'
    STEAM_STORE_OLD_PATTERN = r'steampowered\.com/(?:app|sub)/(\d+)'
    # Generic numeric AppID
    APPID_PATTERN = r'\b(\d{5,7})\b'

    @staticmethod
    def app_id_from(text: str) -> Optional[int]:
        """
        Extract AppID from various Steam/SteamDB URLs or plain text.
        
        Args:
            text: Text that may contain an AppID, Steam URL, or SteamDB URL
            
        Returns:
            AppID as int if found, None otherwise
        """
        if not text:
            return None
            
        text = text.strip()
        
        # If it's just a number, return it directly
        if text.isdigit():
            return int(text)
        
        # Try various Steam/SteamDB URL patterns
        patterns = [
            SteamLinkParser.STEAM_STORE_PATTERN,
            SteamLinkParser.STEAMDB_PATTERN,
            SteamLinkParser.STEAM_COMMUNITY_PATTERN,
            SteamLinkParser.STEAM_STORE_OLD_PATTERN,
        ]
        
        for pattern in patterns:
            match = re.search(pattern, text)
            if match:
                try:
                    return int(match.group(1))
                except (ValueError, IndexError):
                    continue
        
        # Try to find any 5-7 digit number that could be an AppID
        match = re.search(SteamLinkParser.APPID_PATTERN, text)
        if match:
            try:
                return int(match.group(1))
            except ValueError:
                pass
                
        return None