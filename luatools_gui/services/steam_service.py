"""Steam service - discovery and management of Steam installation."""

from __future__ import annotations

from typing import Optional

from ..models.steam import (
    SteamInstallation,
    SteamLibrary,
    detect_steam_install,
    scan_installed_games,
    get_game_name,
    get_stplug_in_dir,
    get_lua_file_path,
    read_lua_file,
    write_lua_file,
)


class SteamService:
    """Service for Steam installation discovery and game scanning."""

    def __init__(self) -> None:
        self._installation: Optional[SteamInstallation] = None

    def detect(self) -> Optional[SteamInstallation]:
        """Detect Steam installation."""
        self._installation = detect_steam_install()
        return self._installation

    @property
    def installation(self) -> Optional[SteamInstallation]:
        return self._installation

    def is_available(self) -> bool:
        return self._installation is not None and self._installation.is_valid

    def get_libraries(self) -> list[SteamLibrary]:
        if self._installation:
            return self._installation.libraries
        return []

    def scan_games(self, library: Optional[SteamLibrary] = None) -> list[int]:
        """Scan for installed game app IDs."""
        if not self._installation:
            return []

        lib = library or self._installation.libraries[0]
        return scan_installed_games(lib)

    def get_game_name(self, app_id: int, library: Optional[SteamLibrary] = None) -> str:
        if not self._installation:
            return f"App {app_id}"

        lib = library or self._installation.libraries[0]
        return get_game_name(lib, app_id)

    def get_stplug_in_dir(self) -> Optional[str]:
        if self._installation:
            return str(get_stplug_in_dir(self._installation))
        return None

    def read_lua(self, app_id: int) -> str:
        if not self._installation:
            return ""

        stplug_dir = get_stplug_in_dir(self._installation)
        lua_path = get_lua_file_path(stplug_dir, app_id)
        return read_lua_file(lua_path)

    def write_lua(self, app_id: int, content: str) -> bool:
        if not self._installation:
            return False

        stplug_dir = get_stplug_in_dir(self._installation)
        lua_path = get_lua_file_path(stplug_dir, app_id)
        try:
            write_lua_file(lua_path, content)
            return True
        except Exception:
            return False

    def delete_lua(self, app_id: int) -> bool:
        if not self._installation:
            return False

        stplug_dir = get_stplug_in_dir(self._installation)
        lua_path = get_lua_file_path(stplug_dir, app_id)
        try:
            if lua_path.exists():
                lua_path.unlink()
            return True
        except Exception:
            return False
