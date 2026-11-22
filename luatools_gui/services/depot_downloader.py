"""Depot Downloader Service - integrates with DepotDownloaderMod."""

from __future__ import annotations

import os
import re
import json
import time
import shutil
import subprocess
import threading
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

import httpx

from ..models.steam import SteamInstallation
from ..services.slssteam_service import SlssteamService
from .depot_info import SteamDepotInfo, ContentDepot, AppDepotInfo


@dataclass
class DepotSelection:
    """One depot to download."""
    depot_id: int
    manifest_id: Optional[str] = None
    manifest_path: Optional[str] = None
    size: int = 0
    from_app_id: Optional[int] = None
    language: Optional[str] = None
    os: Optional[str] = None
    is_shared: bool = False
    is_dlc: bool = False


@dataclass
class DepotRunResult:
    ok: bool
    error: Optional[str] = None


class DepotPhase:
    MANIFEST = "manifest"
    PRE_ALLOCATING = "pre_allocating"
    VALIDATING = "validating"
    DOWNLOADING = "downloading"


class DepotDownloaderService:
    """Runs DepotDownloaderMod to pull depot content from Steam CDN."""

    GITHUB_REPO = "SteamAutoCracks/DepotDownloaderMod"
    TOOL_DIR = Path.home() / ".local/share/luatools/depotdownloader"
    EXE_NAME = "DepotDownloaderMod"
    TOOL_CHECK_INTERVAL = 6 * 3600  # 6 hours

    def __init__(
        self,
        steam_installation: SteamInstallation,
        slssteam_service: SlssteamService,
        depot_info: SteamDepotInfo,
        depot_tool_path: Optional[Path] = None,
        cache_dir: Optional[Path] = None
    ):
        self.steam_installation = steam_installation
        self.slssteam_service = slssteam_service
        self.depot_info = depot_info
        self.depot_tool_path = depot_tool_path or self._find_depot_downloader()
        self.cache_dir = cache_dir or (Path.home() / ".cache/luatools/depotdownloader")
        self.cache_dir.mkdir(parents=True, exist_ok=True)

        self._run_lock = threading.Lock()
        self._http = httpx.Client(timeout=30.0, follow_redirects=True)

    def _find_depot_downloader(self) -> Optional[Path]:
        """Find DepotDownloaderMod executable."""
        # Check cached location first
        cached = self.TOOL_DIR / self.EXE_NAME
        if cached.exists():
            return cached

        # Check plugin directory
        plugin_dir = Path.home() / ".local/share/millennium/plugins/luatools/backend"
        for exe_name in ["DepotDownloaderMod", "DepotDownloaderMod.exe", "DepotDownloader"]:
            candidate = plugin_dir / exe_name
            if candidate.exists():
                return candidate

        # Check common locations
        for base in [Path.home() / ".local/share/LuaTools/depotdownloader",
                     Path.home() / "Applications"]:
            for exe_name in ["DepotDownloaderMod", "DepotDownloaderMod.exe"]:
                candidate = base / exe_name
                if candidate.exists():
                    return candidate

        return None

    @property
    def is_available(self) -> bool:
        return self.depot_tool_path is not None and self.depot_tool_path.exists()

    def make_executable(self) -> None:
        """Ensure depot downloader is executable."""
        if self.depot_tool_path and self.depot_tool_path.exists():
            try:
                self.depot_tool_path.chmod(self.depot_tool_path.stat().st_mode | 0o111)
            except Exception:
                pass

    # ===== Tool Auto-Update =====

    def ensure_tool(self, progress_callback: Optional[Callable[[float, str], None]] = None) -> Optional[Path]:
        """Ensure DepotDownloaderMod is available and up to date."""
        # Check cached version timestamp
        meta_file = self.TOOL_DIR / "meta.json"
        if self.depot_tool_path and self.depot_tool_path.exists():
            if meta_file.exists():
                try:
                    with meta_file.open("r") as f:
                        meta = json.load(f)
                    last_check = meta.get("last_check", 0)
                    if time.time() - last_check < self.TOOL_CHECK_INTERVAL:
                        return self.depot_tool_path
                except Exception:
                    pass

        # Check for updates
        return self._update_tool(progress_callback)

    def _update_tool(self, progress_callback: Optional[Callable[[float, str], None]] = None) -> Optional[Path]:
        """Download/update DepotDownloaderMod from GitHub."""
        self.TOOL_DIR.mkdir(parents=True, exist_ok=True)

        if progress_callback:
            progress_callback(0, "Checking for DepotDownloaderMod updates...")

        try:
            # Get latest release
            url = f"https://api.github.com/repos/{self.GITHUB_REPO}/releases/latest"
            resp = self._http.get(url)
            if resp.status_code != 200:
                return self.depot_tool_path

            release = resp.json()
            tag = release.get("tag_name", "")

            # Find zip asset
            asset = None
            for a in release.get("assets", []):
                if a["name"].endswith(".zip"):
                    asset = a
                    break

            if not asset:
                return self.depot_tool_path

            download_url = asset["browser_download_url"]
            zip_path = self.TOOL_DIR / "DepotDownloaderMod.zip"

            if progress_callback:
                progress_callback(0.1, f"Downloading DepotDownloaderMod {tag}...")

            # Download zip
            with self._http.stream("GET", download_url) as resp:
                resp.raise_for_status()
                total = int(resp.headers.get("content-length", 0))
                downloaded = 0
                with open(zip_path, "wb") as f:
                    for chunk in resp.iter_bytes(8192):
                        f.write(chunk)
                        downloaded += len(chunk)
                        if total > 0 and progress_callback:
                            progress_callback(0.1 + 0.8 * downloaded / total, f"Downloading... {downloaded}/{total}")

            if progress_callback:
                progress_callback(0.9, "Extracting...")

            # Extract
            import zipfile
            with zipfile.ZipFile(zip_path, "r") as zf:
                zf.extractall(self.TOOL_DIR)

            # Find executable
            for root, dirs, files in os.walk(self.TOOL_DIR):
                for f in files:
                    if f in (self.EXE_NAME, f"{self.EXE_NAME}.exe"):
                        self.depot_tool_path = Path(root) / f
                        break

            # Update meta
            meta = {
                "version": tag,
                "last_check": time.time(),
                "exe_path": str(self.depot_tool_path) if self.depot_tool_path else ""
            }
            with (self.TOOL_DIR / "meta.json").open("w") as f:
                json.dump(meta, f)

            zip_path.unlink(missing_ok=True)

            if self.depot_tool_path:
                self.make_executable()
                if progress_callback:
                    progress_callback(1.0, f"DepotDownloaderMod {tag} ready")
                return self.depot_tool_path

        except Exception as e:
            print(f"DepotDownloaderMod update failed: {e}")

        return self.depot_tool_path

    @property
    def is_available(self) -> bool:
        return self.depot_tool_path is not None and self.depot_tool_path.exists()

    def make_executable(self) -> None:
        """Ensure depot downloader is executable."""
        if self.depot_tool_path and self.depot_tool_path.exists():
            try:
                self.depot_tool_path.chmod(self.depot_tool_path.stat().st_mode | 0o111)
            except Exception:
                pass

    # ===== Key Resolution =====

    def resolve_keys(self, app_id: int) -> dict[int, str]:
        """Get depot decryption keys from lua and SLSsteam config."""
        keys = {}

        # From lua file
        stplug_dir = self.steam_installation.stplug_in_path
        if stplug_dir:
            lua_file = stplug_dir / f"{app_id}.lua"
            if lua_file.exists():
                from ..parsers.lua_parser import parse as parse_lua
                lua = parse_lua(lua_file, app_id)
                if lua:
                    for entry in lua.entries:
                        if entry.key:
                            keys[entry.id] = entry.key

        # From SLSsteam config
        config_path = Path.home() / ".config/SLSsteam/config.yaml"
        if config_path.exists():
            try:
                content = config_path.read_text()
                # Parse DepotKeys section
                in_keys = False
                for line in content.splitlines():
                    stripped = line.strip()
                    if stripped == "DepotKeys:":
                        in_keys = True
                        continue
                    if in_keys and stripped and not stripped.startswith(" "):
                        break
                    if in_keys and ":" in stripped:
                        parts = stripped.split(":", 1)
                        if len(parts) == 2:
                            depot_str = parts[0].strip()
                            key = parts[1].strip()
                            if depot_str.isdigit():
                                keys[int(depot_str)] = key
            except Exception:
                pass

        return keys

    def resolve_manifest_path(self, depot_id: int, manifest_id: str) -> Optional[str]:
        """Find manifest file in Steam's depotcache."""
        if not self.steam_installation.libraries:
            return None
        depotcache = self.steam_installation.libraries[0].depotcache
        manifest_file = depotcache / f"{depot_id}_{manifest_id}.manifest"
        if manifest_file.exists():
            return str(manifest_file)
        return None

    def can_fetch_manifests(self) -> bool:
        """Check if we can fetch missing manifests (need Steam login)."""
        return False  # Would need Steam credentials

    def fetch_manifest(self, depot_id: int, manifest_id: str, app_id: int) -> Optional[str]:
        """Fetch manifest from Steam API (requires login)."""
        # This would need authenticated Steam API access
        # For now, return None (anonymous can't fetch manifests)
        return None

    # ===== Depot Info Integration =====

    def get_app_depot_info(self, app_id: int) -> Optional[AppDepotInfo]:
        """Get full depot info for an app."""
        return self.depot_info.get(app_id)

    def get_depot_selections(self, app_id: int, lua) -> list[DepotSelection]:
        """Build depot selections from lua + depot info."""
        if not lua:
            return []

        depot_info = self.depot_info.get(app_id)
        if not depot_info:
            return []

        # Build depot lookup
        depot_map = {d.id: d for d in depot_info.depots}

        # Get keys for the game's app_id (not depot_id)
        keys = self.resolve_keys(app_id)

        # Build selections from lua entries
        selections = []
        for entry in lua.entries:
            if not entry.has_key:
                continue  # Only content depots have keys

            depot = depot_map.get(entry.id)
            if not depot:
                continue

            sel = DepotSelection(
                depot_id=entry.id,
                manifest_id=entry.manifest_id,
                size=depot.size,
                from_app_id=depot.from_app_id,
                language=depot.language,
                os=depot.os,
                is_shared=depot.is_shared,
                is_dlc=depot.is_dlc
            )

            # Resolve manifest path
            if entry.manifest_id:
                sel.manifest_path = self.resolve_manifest_path(entry.id, entry.manifest_id)

            selections.append(sel)

        return selections

    def fetch_manifest(self, depot_id: int, manifest_id: str, app_id: int) -> Optional[str]:
        """Fetch manifest from the API endpoint /api/givemethemanifestpunk/{depotId}/{manifestId}."""
        # This requires authentication - for now return None
        # In the future, this could use the Steam API or the LuaTools backend
        # Example: https://luatools-backend/api/givemethemanifestpunk/{depotId}/{manifestId}
        return None

    def resolve_manifest_path(self, depot_id: int, manifest_id: str, app_id: Optional[int] = None) -> Optional[str]:
        """Find manifest file in Steam's depotcache."""
        if not self.steam_installation.libraries:
            return None
        depotcache = self.steam_installation.libraries[0].depotcache
        manifest_file = depotcache / f"{depot_id}_{manifest_id}.manifest"
        if manifest_file.exists():
            return str(manifest_file)
        return None

    def can_fetch_manifests(self) -> bool:
        """Check if we can fetch missing manifests (need Steam login)."""
        return False  # Would need Steam credentials

    def fetch_manifest(self, depot_id: int, manifest_id: str, app_id: int) -> Optional[str]:
        """Fetch manifest from Steam API (requires login)."""
        # This would need authenticated Steam API access
        # For now, return None (anonymous can't fetch manifests)
        return None

    # ===== Depot Info Integration =====

    def get_app_depot_info(self, app_id: int) -> Optional[AppDepotInfo]:
        """Get full depot info for an app."""
        return self.depot_info.get(app_id)

    def get_depot_selections(self, app_id: int, lua) -> list[DepotSelection]:
        """Build depot selections from lua + depot info."""
        if not lua:
            return []

        depot_info = self.depot_info.get(app_id)
        if not depot_info:
            return []

        # Build depot lookup
        depot_map = {d.id: d for d in depot_info.depots}

        # Get keys
        keys = self.resolve_keys(app_id)

        # Build selections from lua entries
        selections = []
        for entry in lua.entries:
            if not entry.has_key:
                continue  # Only content depots have keys

            depot = depot_map.get(entry.id)
            if not depot:
                continue

            sel = DepotSelection(
                depot_id=entry.id,
                manifest_id=entry.manifest_id,
                size=depot.size,
                from_app_id=depot.from_app_id,
                language=depot.language,
                os=depot.os,
                is_shared=depot.is_shared,
                is_dlc=depot.is_dlc
            )

            # Resolve manifest path
            if entry.manifest_id:
                sel.manifest_path = self.resolve_manifest_path(entry.id, entry.manifest_id)

            selections.append(sel)

        return selections

    # ===== Download =====

    def download_depot(
        self,
        app_id: int,
        selection: DepotSelection,
        download_dir: Path,
        progress_callback: Optional[Callable[[float, str], None]] = None
    ) -> DepotRunResult:
        """Download a single depot using DepotDownloaderMod."""
        if not self.is_available:
            # Try to update tool
            self.ensure_tool()
            if not self.is_available:
                return DepotRunResult(False, "DepotDownloaderMod not found")

        self.make_executable()
        download_dir.mkdir(parents=True, exist_ok=True)

        # Build command - FIXED: -app should be the game's app_id, not depot_id
        cmd = [
            str(self.depot_tool_path),
            "-app", str(app_id),  # FIXED: -app is the game's app_id
            "-depot", str(selection.depot_id),
            "-dir", str(download_dir),
            "-username", "anonymous",
            "-max-downloads", "32",
            "-validate",  # Essential for resume
        ]

        # Add manifest
        if selection.manifest_path and Path(selection.manifest_path).exists():
            cmd.extend(["-manifestfile", selection.manifest_path])

        # Add depot key - FIXED: resolve_keys should use app_id, not depot_id
        keys = self.resolve_keys(app_id)
        if selection.depot_id in keys:
            cmd.extend(["-depotkey", keys[selection.depot_id]])

        if selection.manifest_id:
            cmd.extend(["-manifest", selection.manifest_id])

        # For shared depots, use owning app
        if selection.from_app_id:
            cmd[2] = str(selection.from_app_id)  # -app

        try:
            self._run_lock.acquire()
            process = subprocess.Popen(
                cmd,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                encoding="utf-8",
                errors="replace"
            )

            percent_regex = re.compile(r"(\d{1,3}\.\d{2})%")
            phase = None

            while True:
                line = process.stdout.readline()
                if not line and process.poll() is not None:
                    break

                if line:
                    clean = line.strip()
                    if progress_callback:
                        match = percent_regex.search(clean)
                        if match:
                            try:
                                p = float(match.group(1))
                                progress_callback(p, clean)
                            except ValueError:
                                pass
                        else:
                            progress_callback(0, clean)

            rc = process.wait()
            self._run_lock.release()

            if rc == 0:
                # Verify files downloaded
                has_files = False
                total_size = 0
                for root, dirs, files in os.walk(download_dir):
                    for f in files:
                        fp = Path(root) / f
                        try:
                            total_size += fp.stat().st_size
                            has_files = True
                        except Exception:
                            pass

                if has_files and total_size > 0:
                    return DepotRunResult(True)
                else:
                    return DepotRunResult(False, "Download resulted in empty folder (anonymous restriction)")
            else:
                return DepotRunResult(False, f"DepotDownloaderMod exited with code {rc}")

        except Exception as exc:
            self._run_lock.release()
            return DepotRunResult(False, str(exc))

    def download_multiple(
        self,
        app_id: int,
        selections: list[DepotSelection],
        download_dir: Path,
        progress_callback: Optional[Callable[[str, float, str], None]] = None
    ) -> list[DepotRunResult]:
        """Download multiple depots sequentially."""
        results = []
        for sel in selections:
            depot_dir = download_dir / f"depot_{sel.depot_id}"

            def cb(pct: float, msg: str):
                if progress_callback:
                    progress_callback(f"depot_{sel.depot_id}", pct, msg)

            result = self.download_depot(app_id, sel, depot_dir, cb)
            results.append(result)

        return results

    def install_game_to_steam(
        self,
        app_id: int,
        selections: list[DepotSelection],
        library_path: Path,
        progress_callback: Optional[Callable[[str, float, str], None]] = None
    ) -> list[DepotRunResult]:
        """
        Download depots directly into Steam library and generate appmanifest.acf.
        This is the "One-Click Install Game to Steam" feature.
        """
        results = []
        
        for sel in selections:
            # Download to Steam's common folder structure
            game_dir = library_path / "steamapps" / "common"
            # We need the game's install directory name
            # This would ideally come from the app manifest
            
            # For now, download to depot-specific folder
            depot_dir = library_path / "steamapps" / "depotcache" / f"depot_{sel.depot_id}"
            
            def cb(pct: float, msg: str):
                if progress_callback:
                    progress_callback(f"depot_{sel.depot_id}", pct, msg)

            result = self.download_depot(app_id, sel, depot_dir, cb)
            results.append(result)
            
            # TODO: After all depots downloaded, move files to steamapps/common/<game>/
            # and generate appmanifest_<appid>.acf
            
        return results

    def download_multiple(
        self,
        app_id: int,
        selections: list[DepotSelection],
        download_dir: Path,
        progress_callback: Optional[Callable[[str, float, str], None]] = None
    ) -> list[DepotRunResult]:
        """Download multiple depots sequentially."""
        results = []
        for sel in selections:
            depot_dir = download_dir / f"depot_{sel.depot_id}"

            def cb(pct: float, msg: str):
                if progress_callback:
                    progress_callback(f"depot_{sel.depot_id}", pct, msg)

            result = self.download_depot(app_id, sel, depot_dir, cb)
            results.append(result)

        return results

    def _update_tool(self, progress_callback: Optional[Callable[[float, str], None]] = None) -> Optional[Path]:
        """Download/update DepotDownloaderMod from GitHub."""
        self.TOOL_DIR.mkdir(parents=True, exist_ok=True)

        if progress_callback:
            progress_callback(0, "Checking for DepotDownloaderMod updates...")

        try:
            # Get latest release
            url = f"https://api.github.com/repos/{self.GITHUB_REPO}/releases/latest"
            resp = self._http.get(url)
            if resp.status_code != 200:
                return self.depot_tool_path

            release = resp.json()
            tag = release.get("tag_name", "")

            # Find zip asset - prefer Linux build
            asset = None
            for a in release.get("assets", []):
                name = a["name"].lower()
                if name.endswith(".zip") and ("linux" in name or "linux" in tag.lower()):
                    asset = a
                    break
            # Fallback to any zip
            if not asset:
                for a in release.get("assets", []):
                    if a["name"].endswith(".zip"):
                        asset = a
                        break

            if not asset:
                return self.depot_tool_path

            download_url = asset["browser_download_url"]
            zip_path = self.TOOL_DIR / "DepotDownloaderMod.zip"

            if progress_callback:
                progress_callback(0.1, f"Downloading DepotDownloaderMod {tag}...")

            # Download zip
            with self._http.stream("GET", download_url) as resp:
                resp.raise_for_status()
                total = int(resp.headers.get("content-length", 0))
                downloaded = 0
                with open(zip_path, "wb") as f:
                    for chunk in resp.iter_bytes(8192):
                        f.write(chunk)
                        downloaded += len(chunk)
                        if total > 0 and progress_callback:
                            progress_callback(0.1 + 0.8 * downloaded / total, f"Downloading... {downloaded}/{total}")

            if progress_callback:
                progress_callback(0.9, "Extracting...")

            # Extract
            import zipfile
            with zipfile.ZipFile(zip_path, "r") as zf:
                zf.extractall(self.TOOL_DIR)

            # Find executable
            for root, dirs, files in os.walk(self.TOOL_DIR):
                for f in files:
                    if f in (self.EXE_NAME, f"{self.EXE_NAME}.exe", "DepotDownloaderMod.dll"):
                        self.depot_tool_path = Path(root) / f
                        break

            # Update meta
            meta = {
                "version": tag,
                "last_check": time.time(),
                "exe_path": str(self.depot_tool_path) if self.depot_tool_path else ""
            }
            with (self.TOOL_DIR / "meta.json").open("w") as f:
                json.dump(meta, f)

            zip_path.unlink(missing_ok=True)

            if self.depot_tool_path:
                self.make_executable()
                if progress_callback:
                    progress_callback(1.0, f"DepotDownloaderMod {tag} ready")
                return self.depot_tool_path

        except Exception as e:
            print(f"DepotDownloaderMod update failed: {e}")

        return self.depot_tool_path

    def close(self) -> None:
        self._http.close()