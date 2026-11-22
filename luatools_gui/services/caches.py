"""Metadata and cover caches: SteamAppInfoCache, SteamAppListCache, CoverCache, HardwareAppIdService."""

from __future__ import annotations

import hashlib
import json
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

import httpx

STEAM_PLACEHOLDER_SHA256 = "732ec27ff347292488e3341f00d0750a31f42c44f55da4e04bc8f61d7eb1b6f3"
HARDWARE_APPID_URL = "https://raw.githubusercontent.com/jsnli/steamappidlist/master/data/hardware_appid.json"


@dataclass
class GameDetails:
    app_id: int
    name: str = ""
    app_type: str = ""
    base_app_id: Optional[str] = None
    genres: list[str] = field(default_factory=list)
    header_image: str = ""
    release_date: str = ""
    is_free: bool = False

    @property
    def is_dlc(self) -> bool:
        return self.app_type.lower() == "dlc"


class SteamAppInfoCache:
    """Fetches and caches Steam Store appdetails JSON."""

    def __init__(self, cache_dir: Optional[Path] = None) -> None:
        self.cache_dir = cache_dir or (Path.home() / ".cache/luatools/details")
        self.cache_dir.mkdir(parents=True, exist_ok=True)
        self._http = httpx.Client(timeout=15.0, follow_redirects=True)
        self._mem: dict[int, GameDetails] = {}
        self._lock = threading.Lock()
        self._last_req = 0.0

    def get(self, app_id: int) -> Optional[GameDetails]:
        with self._lock:
            if app_id in self._mem:
                return self._mem[app_id]

        cache_file = self.cache_dir / f"{app_id}.json"
        if cache_file.exists():
            try:
                raw = json.loads(cache_file.read_text(encoding="utf-8"))
                details = self._parse_details(app_id, raw)
                if details:
                    with self._lock:
                        self._mem[app_id] = details
                    return details
            except Exception:
                pass

        # Rate-limit slightly to avoid Steam 429
        with self._lock:
            elapsed = time.time() - self._last_req
            if elapsed < 0.35:
                time.sleep(0.35 - elapsed)
            self._last_req = time.time()

        try:
            resp = self._http.get(
                f"https://store.steampowered.com/api/appdetails?appids={app_id}&l=english"
            )
            if resp.status_code != 200:
                return None
            payload = resp.json()
            entry = payload.get(str(app_id))
            if not isinstance(entry, dict) or not entry.get("success"):
                return None
            data = entry.get("data")
            if not isinstance(data, dict):
                return None

            cache_file.write_text(json.dumps(data), encoding="utf-8")
            details = self._parse_details(app_id, data)
            if details:
                with self._lock:
                    self._mem[app_id] = details
            return details
        except Exception:
            return None

    @staticmethod
    def _parse_details(app_id: int, data: dict) -> Optional[GameDetails]:
        if not isinstance(data, dict):
            return None
        genres = [g.get("description", "") for g in (data.get("genres") or []) if g.get("description")]
        fullgame = data.get("fullgame") or {}
        rel = data.get("release_date") or {}
        return GameDetails(
            app_id=int(data.get("steam_appid") or app_id),
            name=data.get("name") or f"App {app_id}",
            app_type=data.get("type") or "",
            base_app_id=str(fullgame["appid"]) if fullgame.get("appid") else None,
            genres=genres,
            header_image=data.get("header_image") or "",
            release_date=rel.get("date") or "",
            is_free=bool(data.get("is_free", False)),
        )


class SteamAppListCache:
    """Bulk appid -> name cache from applist.morrenus.xyz (14-day TTL)."""

    TTL_SECONDS = 14 * 24 * 3600

    def __init__(self, cache_path: Optional[Path] = None) -> None:
        self.cache_path = cache_path or (Path.home() / ".cache/luatools/applist.json")
        self.cache_path.parent.mkdir(parents=True, exist_ok=True)
        self._http = httpx.Client(timeout=30.0, follow_redirects=True)
        self._map: dict[int, str] = {}
        self._loaded = False

    def ensure_loaded(self, force: bool = False) -> None:
        if self._loaded and not force:
            return
        if not force and self.cache_path.exists():
            try:
                raw = json.loads(self.cache_path.read_text(encoding="utf-8"))
                self._map = {int(k): v for k, v in raw.items()}
                self._loaded = True
                age = time.time() - self.cache_path.stat().st_mtime
                if age < self.TTL_SECONDS:
                    return
            except Exception:
                pass
        self._loaded = True
        threading.Thread(target=self._refresh, daemon=True).start()

    def _refresh(self) -> None:
        try:
            resp = self._http.get("https://applist.morrenus.xyz/")
            if resp.status_code == 200:
                data = resp.json()
                apps = data.get("applist", {}).get("apps", []) if isinstance(data, dict) else data
                mapping: dict[int, str] = {}
                if isinstance(apps, list):
                    for item in apps:
                        aid = item.get("appid")
                        name = item.get("name")
                        if aid and name:
                            mapping[int(aid)] = str(name)
                if mapping:
                    self._map = mapping
                    self._loaded = True
                    self.cache_path.write_text(
                        json.dumps({str(k): v for k, v in mapping.items()}),
                        encoding="utf-8",
                    )
        except Exception:
            self._loaded = True

    def get_name(self, app_id: int) -> Optional[str]:
        if not self._loaded:
            self.ensure_loaded()
        return self._map.get(app_id)


class CoverCache:
    """Downloads and caches Steam header cover JPEGs locally."""

    def __init__(self, cache_dir: Optional[Path] = None) -> None:
        self.cache_dir = cache_dir or (Path.home() / ".cache/luatools/covers")
        self.cache_dir.mkdir(parents=True, exist_ok=True)
        self._http = httpx.Client(timeout=15.0, follow_redirects=True)

    def get_path(self, app_id: int) -> Optional[Path]:
        p = self.cache_dir / f"{app_id}.jpg"
        return p if p.exists() and p.stat().st_size > 100 else None

    def ensure(self, app_id: int, url: Optional[str] = None) -> Optional[Path]:
        existing = self.get_path(app_id)
        if existing:
            return existing

        candidates = []
        if url:
            candidates.append(url)
        candidates.append(f"https://shared.fastly.steamstatic.com/store_item_assets/steam/apps/{app_id}/header.jpg")
        candidates.append(f"https://cdn.cloudflare.steamstatic.com/steam/apps/{app_id}/header.jpg")

        for candidate in candidates:
            try:
                resp = self._http.get(candidate)
                if resp.status_code != 200:
                    continue
                data = resp.content
                if len(data) < 3 or data[:3] != b"\xff\xd8\xff":
                    continue
                if hashlib.sha256(data).hexdigest() == STEAM_PLACEHOLDER_SHA256:
                    continue
                dest = self.cache_dir / f"{app_id}.jpg"
                dest.write_bytes(data)
                return dest
            except Exception:
                continue
        return None


class HardwareAppIdService:
    """Filters Steam hardware app IDs (Steam Deck, Index, etc.) from featured/search."""

    TTL_SECONDS = 14 * 24 * 3600

    def __init__(self, cache_path: Optional[Path] = None) -> None:
        self.cache_path = cache_path or (Path.home() / ".cache/luatools/hardware_appid.json")
        self.cache_path.parent.mkdir(parents=True, exist_ok=True)
        self._http = httpx.Client(timeout=15.0, follow_redirects=True)
        self._ids: set[int] = set()
        self._loaded = False

    def is_hardware(self, app_id: int) -> bool:
        if not self._loaded:
            self._load()
        return app_id in self._ids

    def _load(self) -> None:
        self._loaded = True
        if self.cache_path.exists():
            age = time.time() - self.cache_path.stat().st_mtime
            if age < self.TTL_SECONDS:
                try:
                    items = json.loads(self.cache_path.read_text(encoding="utf-8"))
                    self._ids = {int(x["appid"]) for x in items if "appid" in x}
                    return
                except Exception:
                    pass
        try:
            resp = self._http.get(HARDWARE_APPID_URL)
            if resp.status_code == 200:
                self.cache_path.write_text(resp.text, encoding="utf-8")
                items = resp.json()
                self._ids = {int(x["appid"]) for x in items if "appid" in x}
        except Exception:
            pass
