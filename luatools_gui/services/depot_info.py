"""Steam Depot Info Service - fetches depot metadata from api.steamcmd.net."""

from __future__ import annotations

import json
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional
from concurrent.futures import ThreadPoolExecutor
import threading

import httpx

from ..models.steam import SteamInstallation


@dataclass
class ContentDepot:
    """One depot of an app."""
    id: int
    size: int
    dlc_app_id: Optional[int] = None
    is_shared: bool = False
    os: Optional[str] = None
    language: Optional[str] = None
    public_manifest_id: Optional[str] = None
    from_app_id: Optional[int] = None

    @property
    def is_dlc(self) -> bool:
        return self.dlc_app_id is not None


@dataclass
class AppDepotInfo:
    """Authoritative depot list for an app."""
    app_id: int
    depots: list[ContentDepot] = field(default_factory=list)
    dlc_ids: list[int] = field(default_factory=list)
    launch_exes: list[str] = field(default_factory=list)
    public_build_id: Optional[str] = None


class SteamDepotInfo:
    """Fetches and caches depot information from api.steamcmd.net."""

    FAILURE_TTL = 60  # seconds
    REQUEST_TIMEOUT = 15.0

    def __init__(self, cache_dir: Optional[Path] = None):
        self._cache_dir = cache_dir or (Path.home() / ".cache/luatools/depotinfo")
        self._cache_dir.mkdir(parents=True, exist_ok=True)

        self._http = httpx.Client(timeout=self.REQUEST_TIMEOUT, follow_redirects=True)
        self._memory_cache: dict[int, tuple[Optional[AppDepotInfo], float]] = {}
        self._in_flight: dict[int, threading.Event] = {}
        self._lock = threading.Lock()

    def get(self, app_id: int) -> Optional[AppDepotInfo]:
        """Get depot info (uses memory + disk cache)."""
        # Check memory cache
        with self._lock:
            if app_id in self._memory_cache:
                info, cached_at = self._memory_cache[app_id]
                if info is not None or time.time() - cached_at < self.FAILURE_TTL:
                    return info

        # Check disk cache
        disk_info = self._load_disk_cache(app_id)
        if disk_info:
            with self._lock:
                self._memory_cache[app_id] = (disk_info, time.time())
            return disk_info

        # Fetch from API
        return self._fetch_and_cache(app_id)

    def _fetch_and_cache(self, app_id: int) -> Optional[AppDepotInfo]:
        """Fetch from API and cache."""
        # Deduplicate concurrent fetches
        event = None
        with self._lock:
            if app_id in self._in_flight:
                event = self._in_flight[app_id]
            else:
                event = threading.Event()
                self._in_flight[app_id] = event

        if event is not None and event != threading.Event():
            event.wait()
            with self._lock:
                return self._memory_cache.get(app_id, (None, 0))[0]

        try:
            info = self._fetch_from_api(app_id)
            with self._lock:
                self._memory_cache[app_id] = (info, time.time())
                self._in_flight.pop(app_id, None)
            if info:
                self._save_disk_cache(app_id, info)
            return info
        except Exception:
            with self._lock:
                self._memory_cache[app_id] = (None, time.time())
                self._in_flight.pop(app_id, None)
            return None
        finally:
            event.set()

    def _fetch_from_api(self, app_id: int) -> Optional[AppDepotInfo]:
        """Fetch depot info from api.steamcmd.net."""
        try:
            url = f"https://api.steamcmd.net/v1/info/{app_id}"
            resp = self._http.get(url)
            if resp.status_code != 200:
                return None

            data = resp.json()
            if "data" not in data or str(app_id) not in data["data"]:
                return None

            app = data["data"][str(app_id)]

            # Parse depots
            depots = []
            if "depots" in app and isinstance(app["depots"], dict):
                for dep_key, dep_val in app["depots"].items():
                    if not dep_key.isdigit():
                        continue  # Skip "branches", "baselanguages", etc.
                    if not isinstance(dep_val, dict):
                        continue

                    depot_id = int(dep_key)

                    is_shared = "depotfromapp" in dep_val
                    from_app_id = None
                    if is_shared:
                        try:
                            from_app_id = int(dep_val.get("depotfromapp", "0"))
                        except (ValueError, TypeError):
                            pass

                    dlc_app_id = None
                    if "dlcappid" in dep_val:
                        try:
                            dlc_app_id = int(dep_val["dlcappid"])
                        except (ValueError, TypeError):
                            pass

                    os_list = None
                    language = None
                    if "config" in dep_val and isinstance(dep_val["config"], dict):
                        cfg = dep_val["config"]
                        os_list = cfg.get("oslist")
                        language = cfg.get("dlclanguage") or cfg.get("language")

                    public_manifest_id = None
                    size = 0
                    if "manifests" in dep_val and isinstance(dep_val["manifests"], dict):
                        manifests = dep_val["manifests"]
                        if "public" in manifests:
                            pub = manifests["public"]
                            if isinstance(pub, dict):
                                public_manifest_id = pub.get("gid")
                                try:
                                    size = int(pub.get("size", "0"))
                                except (ValueError, TypeError):
                                    pass

                    depots.append(ContentDepot(
                        id=depot_id,
                        size=size,
                        dlc_app_id=dlc_app_id,
                        is_shared=is_shared,
                        os=os_list,
                        language=language,
                        public_manifest_id=public_manifest_id,
                        from_app_id=from_app_id
                    ))

            # Parse public build ID
            public_build_id = None
            if "depots" in app and isinstance(app["depots"], dict):
                depots_obj = app["depots"]
                if "branches" in depots_obj and isinstance(depots_obj["branches"], dict):
                    branches = depots_obj["branches"]
                    if "public" in branches and isinstance(branches["public"], dict):
                        public_build_id = str(branches["public"].get("buildid", ""))

            # Parse DLC list
            dlc_ids = []
            if "extended" in app and isinstance(app["extended"], dict):
                ext = app["extended"]
                if "listofdlc" in ext and ext["listofdlc"]:
                    for part in ext["listofdlc"].split(","):
                        part = part.strip()
                        if part:
                            try:
                                dlc_ids.append(int(part))
                            except ValueError:
                                pass

            # Parse launch executables
            launch_exes = self._parse_launch_exes(app)

            return AppDepotInfo(
                app_id=app_id,
                depots=depots,
                dlc_ids=dlc_ids,
                launch_exes=launch_exes,
                public_build_id=public_build_id
            )

        except Exception:
            return None

    def _parse_launch_exes(self, app: dict) -> list[str]:
        """Parse launch executables from app info."""
        launch_exes = []
        if "config" not in app or not isinstance(app["config"], dict):
            return launch_exes
        if "launch" not in app["config"] or not isinstance(app["config"]["launch"], dict):
            return launch_exes

        default_exes = []
        other_exes = []

        for entry_name, entry_val in app["config"]["launch"].items():
            if not isinstance(entry_val, dict):
                continue
            exe = entry_val.get("executable")
            if not exe:
                continue

            is_windows = True
            has_beta = False
            has_owns_dlc = False

            if "config" in entry_val and isinstance(entry_val["config"], dict):
                cfg = entry_val["config"]
                os_list = cfg.get("oslist", "")
                if os_list and "windows" not in os_list.lower():
                    is_windows = False
                has_beta = bool(cfg.get("betakey"))
                has_owns_dlc = "ownsdlc" in cfg

            if not is_windows or has_beta or has_owns_dlc:
                continue

            norm = exe.replace("/", "\\")
            is_default = entry_val.get("type", "").lower() == "default"
            (default_exes if is_default else other_exes).append(norm)

        # Deduplicate preserving order
        seen = set()
        for exe in default_exes + other_exes:
            if exe.lower() not in seen:
                seen.add(exe.lower())
                launch_exes.append(exe)

        return launch_exes

    def invalidate(self, app_id: int) -> None:
        """Invalidate cache for an app."""
        with self._lock:
            self._memory_cache.pop(app_id, None)
            self._in_flight.pop(app_id, None)
        # Also remove disk cache
        cache_file = self._cache_dir / f"{app_id}.json"
        if cache_file.exists():
            cache_file.unlink()

    def _load_disk_cache(self, app_id: int) -> Optional[AppDepotInfo]:
        """Load from disk cache."""
        cache_file = self._cache_dir / f"{app_id}.json"
        if not cache_file.exists():
            return None
        try:
            with cache_file.open("r") as f:
                data = json.load(f)
            return AppDepotInfo(
                app_id=data["app_id"],
                depots=[ContentDepot(**d) for d in data.get("depots", [])],
                dlc_ids=data.get("dlc_ids", []),
                launch_exes=data.get("launch_exes", []),
                public_build_id=data.get("public_build_id")
            )
        except Exception:
            return None

    def _save_disk_cache(self, app_id: int, info: AppDepotInfo) -> None:
        """Save to disk cache."""
        cache_file = self._cache_dir / f"{app_id}.json"
        try:
            data = {
                "app_id": info.app_id,
                "depots": [
                    {
                        "id": d.id, "size": d.size, "dlc_app_id": d.dlc_app_id,
                        "is_shared": d.is_shared, "os": d.os, "language": d.language,
                        "public_manifest_id": d.public_manifest_id, "from_app_id": d.from_app_id
                    } for d in info.depots
                ],
                "dlc_ids": info.dlc_ids,
                "launch_exes": info.launch_exes,
                "public_build_id": info.public_build_id
            }
            with cache_file.open("w") as f:
                json.dump(data, f)
        except Exception:
            pass

    def close(self) -> None:
        self._http.close()