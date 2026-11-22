"""Fixes System - Denuvo/Game fixes catalog, SHA-256 backup/apply, and revert."""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import tempfile
import zipfile
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable, Optional

import httpx

from ..models.steam import SteamInstallation
from .api_client import DenuvoFixItem, DenuvoGameListing, DenuvoTag, LuaToolsApiClient
from .lua_installer import LuaInstaller


@dataclass
class GameFix:
    """Legacy/generic game fix model."""
    id: str
    name: str
    description: str
    app_id: int
    download_url: str
    fix_type: str
    size: int = 0
    sha256: str = ""
    source: str = ""
    tags: list[str] = field(default_factory=list)


@dataclass
class FixApplicationResult:
    ok: bool
    error: Optional[str] = None
    applied_files: list[str] = field(default_factory=list)


@dataclass
class AppliedFixRecord:
    app_id: int
    fix_id: str
    applied_at: str
    files: list[dict] = field(default_factory=list)


class FixesService:
    """Manages Denuvo/game fixes from lua.tools API and .luatools-fix/ backup & revert."""

    FIX_DIR_NAME = ".luatools-fix"

    def __init__(
        self,
        steam_installation: Optional[SteamInstallation],
        api_client: Optional[LuaToolsApiClient] = None,
        lua_installer: Optional[LuaInstaller] = None,
        cache_dir: Optional[Path] = None,
        progress_callback: Optional[Callable[[float, str], None]] = None,
    ) -> None:
        self.steam_installation = steam_installation
        self.api_client = api_client
        self.lua_installer = lua_installer
        self.cache_dir = cache_dir or (Path.home() / ".cache/luatools/fixes")
        self.cache_dir.mkdir(parents=True, exist_ok=True)
        self.progress_callback = progress_callback
        self._http = httpx.Client(timeout=60.0, follow_redirects=True)

    def _report(self, progress: float, message: str) -> None:
        if self.progress_callback:
            self.progress_callback(progress, message)

    # ===== Remote Denuvo / Game Fixes API =====

    def get_listings(self) -> tuple[list[DenuvoGameListing], list[DenuvoTag]]:
        if not self.api_client:
            return [], []
        return self.api_client.get_denuvo_listings()

    def get_fixes(self, app_id: int | str) -> list[DenuvoFixItem]:
        if not self.api_client:
            return []
        return self.api_client.get_denuvo_fixes(app_id)

    def apply_denuvo_manifest(
        self,
        app_id: int,
        fix: DenuvoFixItem,
        progress_cb: Optional[Callable[[float, str], None]] = None,
    ) -> FixApplicationResult:
        if not self.api_client or not self.lua_installer:
            return FixApplicationResult(False, "API client or LuaInstaller not configured.")
        try:
            with tempfile.TemporaryDirectory() as tmpdir:
                tmp = Path(tmpdir)
                fallback = fix.manifest_filename or f"{app_id}.zip"
                downloaded = self.api_client.download_denuvo_slot(
                    fix.id, "manifest", tmp, fallback, progress_cb=progress_cb
                )
                res = self.lua_installer.install_any(
                    downloaded, expected_app_id=app_id, force_locked=True, source=f"fix:{fix.id}"
                )
                if not res.ok:
                    return FixApplicationResult(False, res.error or "Failed to install manifest.")
                return FixApplicationResult(True, applied_files=[downloaded.name])
        except Exception as exc:
            return FixApplicationResult(False, str(exc))

    def apply_denuvo_fix(
        self,
        app_id: int,
        fix: DenuvoFixItem,
        install_path: str,
        progress_cb: Optional[Callable[[float, str], None]] = None,
    ) -> FixApplicationResult:
        if not self.api_client:
            return FixApplicationResult(False, "API client not configured.")
        game_dir = Path(install_path)
        if not game_dir.exists():
            return FixApplicationResult(False, f"Game install directory does not exist: {install_path}")

        try:
            with tempfile.TemporaryDirectory() as tmpdir:
                tmp = Path(tmpdir)
                fallback = fix.fix_filename or f"{fix.id}.zip"
                dl_path = self.api_client.download_denuvo_slot(
                    fix.id, "fix", tmp, fallback, progress_cb=progress_cb
                )

                extract_dir = tmp / "extract"
                extract_dir.mkdir()
                if dl_path.suffix.lower() == ".zip":
                    with zipfile.ZipFile(dl_path, "r") as zf:
                        zf.extractall(extract_dir)
                else:
                    shutil.copy2(dl_path, extract_dir / dl_path.name)

                return self._apply_extracted_with_backup(app_id, fix.id, extract_dir, game_dir)
        except Exception as exc:
            return FixApplicationResult(False, str(exc))

    def _apply_extracted_with_backup(
        self,
        app_id: int,
        fix_id: str,
        extract_dir: Path,
        game_dir: Path,
    ) -> FixApplicationResult:
        fix_meta_dir = game_dir / self.FIX_DIR_NAME
        backup_dir = fix_meta_dir / "backups" / fix_id
        backup_dir.mkdir(parents=True, exist_ok=True)

        records = []
        applied = []

        for src in extract_dir.rglob("*"):
            if not src.is_file():
                continue
            rel = src.relative_to(extract_dir)
            rel_str = rel.as_posix()
            dst = game_dir / rel
            dst.parent.mkdir(parents=True, exist_ok=True)

            if dst.exists():
                hash_before = _sha256(dst)
                bkp_file = backup_dir / rel
                bkp_file.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(dst, bkp_file)
                shutil.copy2(src, dst)
                hash_after = _sha256(dst)
                records.append({
                    "relativePath": rel_str,
                    "action": "modified",
                    "backupPath": f"backups/{fix_id}/{rel_str}",
                    "hashBefore": hash_before,
                    "hashAfter": hash_after,
                })
            else:
                shutil.copy2(src, dst)
                hash_after = _sha256(dst)
                records.append({
                    "relativePath": rel_str,
                    "action": "added",
                    "backupPath": None,
                    "hashBefore": None,
                    "hashAfter": hash_after,
                })
            applied.append(rel_str)

        record_file = fix_meta_dir / f"{fix_id}.json"
        record_payload = {
            "appId": app_id,
            "fixId": fix_id,
            "appliedAt": datetime.now(timezone.utc).isoformat(),
            "files": records,
        }
        record_file.write_text(json.dumps(record_payload, indent=2), encoding="utf-8")
        return FixApplicationResult(True, applied_files=applied)

    def is_fix_applied(self, install_path: str, fix_id: str) -> bool:
        return (Path(install_path) / self.FIX_DIR_NAME / f"{fix_id}.json").exists()

    def list_applied_fixes(self, install_path: str) -> list[AppliedFixRecord]:
        fix_meta_dir = Path(install_path) / self.FIX_DIR_NAME
        if not fix_meta_dir.exists():
            return []
        out = []
        for f in fix_meta_dir.glob("*.json"):
            try:
                d = json.loads(f.read_text(encoding="utf-8"))
                out.append(
                    AppliedFixRecord(
                        app_id=int(d.get("appId", 0)),
                        fix_id=str(d.get("fixId", f.stem)),
                        applied_at=str(d.get("appliedAt", "")),
                        files=d.get("files") or [],
                    )
                )
            except Exception:
                pass
        return sorted(out, key=lambda r: r.applied_at, reverse=True)

    def unfix_game(self, app_id: int, install_path: str, fix_id: str = "") -> FixApplicationResult:
        """Revert an applied fix using .luatools-fix/<fix_id>.json SHA-256 verification."""
        game_dir = Path(install_path)
        fix_meta_dir = game_dir / self.FIX_DIR_NAME
        if not fix_meta_dir.exists():
            return FixApplicationResult(False, "No applied fix records found in game directory.")

        if not fix_id:
            applied = self.list_applied_fixes(install_path)
            if not applied:
                return FixApplicationResult(False, "No applied fixes found.")
            fix_id = applied[0].fix_id

        record_file = fix_meta_dir / f"{fix_id}.json"
        if not record_file.exists():
            return FixApplicationResult(False, f"Fix record '{fix_id}' not found.")

        try:
            record = json.loads(record_file.read_text(encoding="utf-8"))
            entries = record.get("files") or []

            # Verify no external modification before reverting
            for entry in entries:
                rel = entry["relativePath"]
                dst = game_dir / rel
                expected_after = entry.get("hashAfter")
                if expected_after and dst.exists():
                    if _sha256(dst) != expected_after:
                        return FixApplicationResult(
                            False,
                            f"File '{rel}' was modified after this fix was applied. "
                            "Revert newer fixes first or verify game files in Steam.",
                        )

            reverted = []
            for entry in entries:
                rel = entry["relativePath"]
                dst = game_dir / rel
                action = entry.get("action")
                if action == "added":
                    if dst.exists():
                        dst.unlink()
                    reverted.append(rel)
                elif action == "modified":
                    bkp_rel = entry.get("backupPath")
                    if bkp_rel:
                        bkp = fix_meta_dir / bkp_rel
                        if bkp.exists():
                            dst.parent.mkdir(parents=True, exist_ok=True)
                            shutil.copy2(bkp, dst)
                            reverted.append(rel)

            record_file.unlink(missing_ok=True)
            bkp_dir = fix_meta_dir / "backups" / fix_id
            if bkp_dir.exists():
                shutil.rmtree(bkp_dir, ignore_errors=True)

            return FixApplicationResult(True, applied_files=reverted)
        except Exception as exc:
            return FixApplicationResult(False, str(exc))

    def close(self) -> None:
        self._http.close()


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest().lower()


def create_fixes_service(
    steam_installation: SteamInstallation,
    cache_dir: Optional[Path] = None,
    progress_callback: Optional[Callable[[float, str], None]] = None,
) -> FixesService:
    return FixesService(steam_installation, cache_dir=cache_dir, progress_callback=progress_callback)
