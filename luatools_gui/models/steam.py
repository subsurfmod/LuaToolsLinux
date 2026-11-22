"""Steam installation and library discovery for Linux."""

from __future__ import annotations

import os
import re
import struct
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional


STEAM_ROOT_CANDIDATES = [
    Path.home() / ".local/share/Steam",
    Path.home() / ".steam" / "steam",
    Path("/var/lib/steam"),
]


@dataclass
class SteamLibrary:
    """Represents a Steam library folder."""

    path: Path
    app_ids: list[int] = field(default_factory=list)

    @property
    def steamapps(self) -> Path:
        return self.path / "steamapps"

    @property
    def common(self) -> Path:
        return self.steamapps / "common"

    @property
    def depotcache(self) -> Path:
        return self.path / "depotcache"


@dataclass
class SteamInstallation:
    """Represents a Steam installation."""

    root: Path
    libraries: list[SteamLibrary] = field(default_factory=list)
    config_path: Optional[Path] = None
    stplug_in_path: Optional[Path] = None

    @property
    def is_valid(self) -> bool:
        return self.root.exists() and (self.root / "steam.sh").exists()


def detect_steam_install() -> Optional[SteamInstallation]:
    """Detect the Steam installation on this Linux system."""
    for candidate in STEAM_ROOT_CANDIDATES:
        if candidate.exists() and (candidate / "steam.sh").exists():
            inst = SteamInstallation(root=candidate)

            # Config paths
            inst.config_path = candidate / "config"
            inst.stplug_in_path = candidate / "config" / "stplug-in"

            # Discover libraries from libraryfolders.vdf
            libs = _discover_libraries(candidate)
            inst.libraries = libs

            return inst

    return None


def _discover_libraries(steam_root: Path) -> list[SteamLibrary]:
    """Discover Steam library folders from libraryfolders.vdf."""
    libraries = []

    # Primary library
    primary = SteamLibrary(path=steam_root)
    libraries.append(primary)

    # Read libraryfolders.vdf for additional libraries
    vdf_path = steam_root / "steamapps" / "libraryfolders.vdf"
    if vdf_path.exists():
        try:
            content = vdf_path.read_text(encoding="utf-8", errors="replace")
            # Match paths like "path" "/mnt/games"
            for m in re.finditer(r'"path"\s+"([^"]+)"', content):
                lib_path = Path(m.group(1))
                if lib_path.exists():
                    libraries.append(SteamLibrary(path=lib_path))
        except Exception:
            pass

    return libraries


def scan_installed_games(library: SteamLibrary) -> list[int]:
    """Scan a Steam library for installed game ACF files and return app IDs."""
    app_ids = []
    steamapps = library.steamapps

    if not steamapps.exists():
        return app_ids

    for acf in steamapps.glob("appmanifest_*.acf"):
        try:
            app_id = int(acf.stem.replace("appmanifest_", ""))
            app_ids.append(app_id)
        except ValueError:
            continue

    return sorted(app_ids)


def get_game_name(library: SteamLibrary, app_id: int) -> str:
    """Read the game name from an appmanifest ACF file."""
    acf_path = library.steamapps / f"appmanifest_{app_id}.acf"
    if not acf_path.exists():
        return f"App {app_id}"

    try:
        content = acf_path.read_text(encoding="utf-8", errors="replace")
        m = re.search(r'"name"\s+"([^"]+)"', content)
        if m:
            return m.group(1)
    except Exception:
        pass

    return f"App {app_id}"


def get_stplug_in_dir(steam_install: SteamInstallation) -> Path:
    """Get the stplug-in directory path."""
    if steam_install.stplug_in_path:
        return steam_install.stplug_in_path
    return steam_install.root / "config" / "stplug-in"


def get_lua_file_path(stplug_in_dir: Path, app_id: int) -> Path:
    """Get the path to a game's LuaTools lua file in stplug-in."""
    return stplug_in_dir / f"{app_id}.lua"


def read_lua_file(path: Path) -> str:
    """Read a lua file content."""
    if path.exists():
        return path.read_text(encoding="utf-8", errors="replace")
    return ""


def write_lua_file(path: Path, content: str) -> None:
    """Write content to a lua file."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
