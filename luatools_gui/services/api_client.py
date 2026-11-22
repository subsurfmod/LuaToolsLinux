"""LuaTools API Client, HubcapService, and GitHub proxy helpers."""

from __future__ import annotations

import re
import urllib.parse
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Optional

import httpx

from .auth_service import API_BASE_URL, SUPABASE_ANON_KEY, SUPABASE_URL, AuthService

MANIFEST_BACKEND_URL = "http://167.235.229.108"
MANIFEST_BACKEND_UA = "secretgoonpoon"
HUBCAP_BASE_URL = "https://hubcapmanifest.com"
DAILY_DOWNLOAD_LIMIT = 25


class ApiError(Exception):
    """API call error."""

    def __init__(self, message: str, status_code: Optional[int] = None) -> None:
        super().__init__(message)
        self.status_code = status_code


@dataclass
class SteamSearchItem:
    app_id: int
    name: str
    icon: str = ""


@dataclass
class SteamFeaturedItem:
    app_id: int
    name: str
    large_capsule_image: str = ""


@dataclass
class DlcDepotInfo:
    depot_id: str
    language: Optional[str] = None
    oslist: Optional[str] = None
    included: bool = False


@dataclass
class DlcInfo:
    appid: str
    name: str
    app_type: str
    depot_count: int = 0
    have_count: int = 0
    missing_count: int = 0
    depots: list[DlcDepotInfo] = field(default_factory=list)


@dataclass
class HubcapStats:
    user_id: str = ""
    daily_usage: int = 0
    daily_limit: int = 0
    can_make_requests: bool = False
    api_key_expires_at: Optional[str] = None


@dataclass
class DenuvoTag:
    id: str
    name: str
    slug: str = ""
    color: Optional[str] = None


@dataclass
class DenuvoGameListing:
    app_id: str
    name: str
    header_image: Optional[str] = None
    fix_count: int = 0
    tags: list[DenuvoTag] = field(default_factory=list)


@dataclass
class DenuvoFixItem:
    id: str
    title: str
    description: str = ""
    tags: list[DenuvoTag] = field(default_factory=list)
    has_manifest: bool = False
    has_fix: bool = False
    manifest_filename: Optional[str] = None
    fix_filename: Optional[str] = None
    created_at: Optional[str] = None


class HubcapService:
    """Direct client to hubcapmanifest.com using user's API key."""

    KEY_RE = re.compile(r"^smm_[0-9a-f]{96}$")

    def __init__(self) -> None:
        self._http = httpx.Client(base_url=HUBCAP_BASE_URL, timeout=120.0, follow_redirects=True)

    @classmethod
    def is_valid_key_format(cls, key: Optional[str]) -> bool:
        return bool(key and cls.KEY_RE.match(key.strip()))

    def get_stats(self, key: str) -> Optional[HubcapStats]:
        try:
            resp = self._http.get(f"/api/v1/user/stats?api_key={urllib.parse.quote(key)}")
            if resp.status_code != 200:
                return None
            d = resp.json()
            return HubcapStats(
                user_id=str(d.get("user_id", "")),
                daily_usage=int(d.get("daily_usage", 0)),
                daily_limit=int(d.get("daily_limit", 0)),
                can_make_requests=bool(d.get("can_make_requests", False)),
                api_key_expires_at=d.get("api_key_expires_at"),
            )
        except Exception:
            return None

    def check_status(self, key: str, app_id: int) -> Optional[bool]:
        try:
            resp = self._http.get(
                f"/api/v1/status/{app_id}",
                headers={"Authorization": f"Bearer {key}"},
            )
            if resp.status_code != 200:
                return None
            return bool(resp.json().get("manifest_file_exists", False))
        except Exception:
            return None

    def download_manifest(
        self,
        app_id: int,
        key: str,
        dest_dir: Path,
        progress_cb: Optional[Callable[[float, str], None]] = None,
    ) -> Path:
        url = f"/api/v1/manifest/{app_id}?api_key={urllib.parse.quote(key)}"
        dest_dir.mkdir(parents=True, exist_ok=True)
        out = dest_dir / f"{app_id}.zip"
        with self._http.stream("GET", url) as resp:
            if resp.status_code == 401:
                raise ApiError("Invalid Hubcap API key.", 401)
            if resp.status_code == 429:
                raise ApiError("Hubcap daily limit reached.", 429)
            if resp.status_code == 404:
                raise ApiError("No manifest available on Hubcap for this game.", 404)
            if resp.status_code != 200:
                raise ApiError(f"Hubcap download failed ({resp.status_code}).", resp.status_code)
            _stream_to_file(resp, out, progress_cb)
        return out


class LuaToolsApiClient:
    """Typed HTTP client for lua.tools, Steam Store search/featured, and manifest sources."""

    def __init__(self, auth: AuthService) -> None:
        self.auth = auth
        self._http = httpx.Client(timeout=120.0, follow_redirects=True)

    def search_store(self, query: str) -> list[SteamSearchItem]:
        url = (
            f"https://store.steampowered.com/api/storesearch/"
            f"?term={urllib.parse.quote(query)}&l=english&cc=US"
        )
        resp = self._http.get(url)
        if resp.status_code != 200:
            return []
        items = resp.json().get("items") or []
        results = []
        for item in items[:12]:
            aid = int(item.get("id", 0))
            if aid <= 0:
                continue
            results.append(
                SteamSearchItem(
                    app_id=aid,
                    name=item.get("name", f"App {aid}"),
                    icon=item.get("tiny_image")
                    or f"https://cdn.cloudflare.steamstatic.com/steam/apps/{aid}/capsule_sm_120.jpg",
                )
            )
        return results

    def get_featured(self) -> tuple[list[SteamFeaturedItem], list[SteamFeaturedItem]]:
        try:
            resp = self._http.get("https://store.steampowered.com/api/featuredcategories?cc=us&l=english")
            if resp.status_code != 200:
                return [], []
            data = resp.json()

            def clean(cat_key: str) -> list[SteamFeaturedItem]:
                seen = set()
                out = []
                for item in (data.get(cat_key) or {}).get("items") or []:
                    aid = int(item.get("id", 0))
                    if item.get("type") != 0 or aid <= 0 or aid in seen:
                        continue
                    img = item.get("large_capsule_image") or ""
                    if not img:
                        continue
                    seen.add(aid)
                    out.append(SteamFeaturedItem(app_id=aid, name=item.get("name", ""), large_capsule_image=img))
                    if len(out) >= 20:
                        break
                return out

            return clean("top_sellers"), clean("new_releases")
        except Exception:
            return [], []

    def check_sources(self, app_id: int) -> dict[str, str]:
        try:
            resp = self._http.get(
                f"{MANIFEST_BACKEND_URL}/check_apis?appid={app_id}",
                headers={"User-Agent": MANIFEST_BACKEND_UA},
            )
            if resp.status_code != 200:
                return {}
            data = resp.json()
            return data if isinstance(data, dict) else {}
        except Exception:
            return {}

    def get_standard_usage(self) -> Optional[tuple[int, int]]:
        try:
            token = self.auth.get_valid_access_token()
            from datetime import datetime, timezone
            today = datetime.now(timezone.utc).strftime("%Y-%m-%dT00:00:00Z")
            url = f"{SUPABASE_URL}/rest/v1/user_downloads?select=appid&downloaded_at=gte.{urllib.parse.quote(today)}"
            resp = self._http.head(
                url,
                headers={
                    "apikey": SUPABASE_ANON_KEY,
                    "Authorization": f"Bearer {token}",
                    "Prefer": "count=exact",
                },
            )
            if resp.status_code not in (200, 206):
                return None
            cr = resp.headers.get("Content-Range", "")
            if "/" in cr:
                used = int(cr.split("/")[-1])
                return used, DAILY_DOWNLOAD_LIMIT
        except Exception:
            pass
        return None

    def get_supporter_status(self) -> Optional[bool]:
        try:
            resp = self._auth_request("GET", "/api/me/supporter-status")
            return bool(resp.json().get("isSupporter", False))
        except Exception:
            return None

    def get_dlc_info(self, app_id: int, base_app_id: int) -> Optional[DlcInfo]:
        resp = self._auth_request("GET", f"/api/dlc/info?appid={app_id}&base={base_app_id}")
        d = resp.json()
        depots = [
            DlcDepotInfo(
                depot_id=str(x.get("depotId", "")),
                language=x.get("language"),
                oslist=x.get("oslist"),
                included=bool(x.get("included", False)),
            )
            for x in (d.get("depots") or [])
        ]
        return DlcInfo(
            appid=str(d.get("appid", app_id)),
            name=d.get("name", ""),
            app_type=d.get("type", ""),
            depot_count=int(d.get("depotCount", 0)),
            have_count=int(d.get("haveCount", 0)),
            missing_count=int(d.get("missingCount", 0)),
            depots=depots,
        )

    def download_manifest(
        self,
        app_id: int,
        source: str,
        game_name: Optional[str],
        dest_dir: Path,
        progress_cb: Optional[Callable[[float, str], None]] = None,
    ) -> Path:
        path = f"/api/manifest/download?appid={app_id}&source={urllib.parse.quote(source)}"
        if game_name:
            path += f"&game_name={urllib.parse.quote(game_name)}"
        return self._auth_download(path, dest_dir / f"{app_id}.zip", progress_cb)

    def download_depot_manifest(
        self,
        depot_id: int,
        manifest_id: str,
        dest_dir: Path,
        progress_cb: Optional[Callable[[float, str], None]] = None,
    ) -> Path:
        path = f"/api/givemethemanifestpunk/{depot_id}/{manifest_id}"
        return self._auth_download(path, dest_dir / f"{depot_id}_{manifest_id}.manifest", progress_cb)

    def generate_dlc(
        self,
        app_id: int,
        base_app_id: int,
        game_name: Optional[str],
        dest_dir: Path,
        progress_cb: Optional[Callable[[float, str], None]] = None,
    ) -> Path:
        path = f"/api/dlc/generate?appid={app_id}&base={base_app_id}"
        if game_name:
            path += f"&game_name={urllib.parse.quote(game_name)}"
        return self._auth_download(path, dest_dir / f"{app_id}.lua", progress_cb)

    def get_denuvo_listings(self) -> tuple[list[DenuvoGameListing], list[DenuvoTag]]:
        resp = self._http.get(f"{API_BASE_URL}/api/denuvo/listings")
        if resp.status_code != 200:
            return [], []
        d = resp.json()
        tags = [
            DenuvoTag(
                id=str(t.get("id", "")),
                name=t.get("name", ""),
                slug=t.get("slug", ""),
                color=t.get("color"),
            )
            for t in (d.get("tags") or [])
        ]
        games = []
        for g in d.get("games") or []:
            g_tags = [
                DenuvoTag(
                    id=str(t.get("id", "")),
                    name=t.get("name", ""),
                    slug=t.get("slug", ""),
                    color=t.get("color"),
                )
                for t in (g.get("tags") or [])
            ]
            games.append(
                DenuvoGameListing(
                    app_id=str(g.get("appid", "")),
                    name=g.get("name", ""),
                    header_image=g.get("header_image"),
                    fix_count=int(g.get("fixCount", 0)),
                    tags=g_tags,
                )
            )
        return games, tags

    def get_denuvo_fixes(self, app_id: int | str) -> list[DenuvoFixItem]:
        resp = self._http.get(f"{API_BASE_URL}/api/denuvo/fixes?appid={app_id}")
        if resp.status_code != 200:
            return []
        d = resp.json()
        out = []
        for f in d.get("fixes") or []:
            f_tags = [
                DenuvoTag(
                    id=str(t.get("id", "")),
                    name=t.get("name", ""),
                    slug=t.get("slug", ""),
                    color=t.get("color"),
                )
                for t in (f.get("tags") or [])
            ]
            out.append(
                DenuvoFixItem(
                    id=str(f.get("id", "")),
                    title=f.get("title", ""),
                    description=f.get("description") or "",
                    tags=f_tags,
                    has_manifest=bool(f.get("hasManifest", False)),
                    has_fix=bool(f.get("hasFix", False)),
                    manifest_filename=f.get("manifestFilename"),
                    fix_filename=f.get("fixFilename"),
                    created_at=f.get("createdAt"),
                )
            )
        return out

    def download_denuvo_slot(
        self,
        fix_id: str,
        slot: str,
        dest_dir: Path,
        fallback_name: str,
        progress_cb: Optional[Callable[[float, str], None]] = None,
    ) -> Path:
        resp = self._auth_request(
            "GET",
            f"/api/denuvo/download?fix={urllib.parse.quote(fix_id)}&slot={urllib.parse.quote(slot)}",
        )
        signed_url = resp.json().get("url", "")
        if not signed_url:
            raise ApiError("Server returned an empty download link.")

        dest_dir.mkdir(parents=True, exist_ok=True)
        out = dest_dir / fallback_name
        with self._http.stream("GET", signed_url) as dl_resp:
            if dl_resp.status_code != 200:
                raise ApiError(f"Signed download failed ({dl_resp.status_code}).", dl_resp.status_code)
            cd = dl_resp.headers.get("content-disposition", "")
            fname = _filename_from_cd(cd)
            if fname:
                out = dest_dir / fname
            _stream_to_file(dl_resp, out, progress_cb)
        return out

    def _auth_request(self, method: str, path: str) -> httpx.Response:
        token = self.auth.get_valid_access_token()
        resp = self._http.request(
            method,
            f"{API_BASE_URL}{path}",
            headers={"Authorization": f"Bearer {token}"},
        )
        if resp.status_code == 401:
            raise ApiError("Session expired. Please sign in again.", 401)
        if resp.status_code >= 400:
            msg = f"Request failed ({resp.status_code})."
            try:
                err = resp.json().get("error")
                if err:
                    msg = str(err)
            except Exception:
                pass
            raise ApiError(msg, resp.status_code)
        return resp

    def _auth_download(
        self,
        path: str,
        fallback_path: Path,
        progress_cb: Optional[Callable[[float, str], None]],
    ) -> Path:
        token = self.auth.get_valid_access_token()
        fallback_path.parent.mkdir(parents=True, exist_ok=True)
        out = fallback_path
        with self._http.stream(
            "GET",
            f"{API_BASE_URL}{path}",
            headers={"Authorization": f"Bearer {token}"},
        ) as resp:
            if resp.status_code == 401:
                raise ApiError("Session expired. Please sign in again.", 401)
            if resp.status_code >= 400:
                body = resp.read().decode("utf-8", errors="replace")
                msg = f"Download failed ({resp.status_code})."
                try:
                    import json
                    err = json.loads(body).get("error")
                    if err:
                        msg = str(err)
                except Exception:
                    pass
                raise ApiError(msg, resp.status_code)
            cd = resp.headers.get("content-disposition", "")
            fname = _filename_from_cd(cd)
            if fname:
                out = fallback_path.parent / fname
            _stream_to_file(resp, out, progress_cb)
        return out


def _filename_from_cd(cd: str) -> Optional[str]:
    if not cd:
        return None
    m = re.search(r'filename\*?=(?:UTF-8\'\')?"?([^";\r\n]+)"?', cd, re.IGNORECASE)
    if m:
        return Path(urllib.parse.unquote(m.group(1))).name
    return None


def _stream_to_file(
    resp: httpx.Response,
    dest: Path,
    progress_cb: Optional[Callable[[float, str], None]],
) -> None:
    total = int(resp.headers.get("content-length", 0))
    downloaded = 0
    with dest.open("wb") as f:
        for chunk in resp.iter_bytes(8192):
            f.write(chunk)
            downloaded += len(chunk)
            if progress_cb and total > 0:
                progress_cb(downloaded / total, f"Downloading... {downloaded // 1024} KB / {total // 1024} KB")
