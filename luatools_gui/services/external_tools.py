"""External Tools Integration - SteamAutoCrack, Steamless, DepotDownloaderMod."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

import httpx

from ..models.steam import SteamInstallation


@dataclass
class ExternalTool:
    """An external tool managed by LuaTools."""
    name: str
    github_repo: str
    exe_name: str
    asset_pattern: str  # e.g. "*.zip", "*.tar.gz"
    version: str = ""
    path: Optional[Path] = None


class ExternalToolsManager:
    """Manages external tools (SteamAutoCrack, Steamless, DepotDownloaderMod)."""

    TOOLS = {
        "depotdownloader": ExternalTool(
            name="DepotDownloaderMod",
            github_repo="SteamAutoCracks/DepotDownloaderMod",
            exe_name="DepotDownloaderMod",
            asset_pattern="*.zip"
        ),
        "steamautocrack": ExternalTool(
            name="SteamAutoCrack",
            github_repo="SteamAutoCracks/Steam-auto-crack",
            exe_name="SteamAutoCrack",
            asset_pattern="*.zip"
        ),
        "steamless": ExternalTool(
            name="Steamless",
            github_repo="atom0s/Steamless",
            exe_name="Steamless",
            asset_pattern="*.zip"
        ),
    }

    CHECK_INTERVAL = 6 * 3600  # 6 hours

    def __init__(
        self,
        tools_dir: Optional[Path] = None,
        progress_callback: Optional[Callable[[str, float, str], None]] = None
    ):
        self.tools_dir = tools_dir or (Path.home() / ".local/share/luatools/tools")
        self.tools_dir.mkdir(parents=True, exist_ok=True)
        self.progress_callback = progress_callback
        self._http = httpx.Client(timeout=60.0, follow_redirects=True)
        self._tool_paths: dict[str, Path] = {}

    def _report(self, tool: str, progress: float, message: str) -> None:
        if self.progress_callback:
            self.progress_callback(tool, progress, message)

    def get_tool_path(self, tool_name: str) -> Optional[Path]:
        """Get path to a tool, downloading if necessary."""
        if tool_name in self._tool_paths:
            return self._tool_paths[tool_name]

        tool = self.TOOLS.get(tool_name)
        if not tool:
            return None

        # Check cached location
        tool_dir = self.tools_dir / tool_name
        meta_file = tool_dir / "meta.json"

        if tool_dir.exists():
            # Check version
            if meta_file.exists():
                try:
                    with meta_file.open("r") as f:
                        meta = json.load(f)
                    if time.time() - meta.get("last_check", 0) < self.CHECK_INTERVAL:
                        for exe in tool_dir.rglob(tool.exe_name):
                            self._tool_paths[tool_name] = exe
                            return exe
                except Exception:
                    pass

        # Download/update
        return self._update_tool(tool_name)

    def _update_tool(self, tool_name: str) -> Optional[Path]:
        """Download/update a tool from GitHub."""
        tool = self.TOOLS.get(tool_name)
        if not tool:
            return None

        tool_dir = self.tools_dir / tool_name
        tool_dir.mkdir(parents=True, exist_ok=True)

        self._report(tool_name, 0, f"Checking for {tool.name} updates...")

        try:
            url = f"https://api.github.com/repos/{tool.github_repo}/releases/latest"
            resp = self._http.get(url)
            if resp.status_code != 200:
                return self._tool_paths.get(tool_name)

            release = resp.json()
            tag = release.get("tag_name", "")

            # Find matching asset
            asset = None
            for a in release.get("assets", []):
                if a["name"].endswith(".zip") or a["name"].endswith(".tar.gz"):
                    asset = a
                    break

            if not asset:
                return self._tool_paths.get(tool_name)

            download_url = asset["browser_download_url"]
            archive_path = tool_dir / asset["name"]

            self._report(tool_name, 0.1, f"Downloading {tool.name} {tag}...")

            with self._http.stream("GET", download_url) as resp:
                resp.raise_for_status()
                total = int(resp.headers.get("content-length", 0))
                downloaded = 0
                with archive_path.open("wb") as f:
                    for chunk in resp.iter_bytes(8192):
                        f.write(chunk)
                        downloaded += len(chunk)
                        if total > 0:
                            self._report(tool_name, 0.1 + 0.7 * downloaded / total,
                                f"Downloading... {downloaded}/{total}")

            self._report(tool_name, 0.8, "Extracting...")

            # Extract
            if archive_path.suffix == ".zip":
                import zipfile
                with zipfile.ZipFile(archive_path, "r") as zf:
                    zf.extractall(tool_dir)
            else:
                import tarfile
                with tarfile.open(archive_path, "r:*") as tf:
                    tf.extractall(tool_dir)

            # Find executable
            for root, dirs, files in os.walk(tool_dir):
                for f in files:
                    if f == tool.exe_name or f == f"{tool.exe_name}.exe":
                        exe_path = Path(root) / f
                        self._tool_paths[tool_name] = exe_path
                        break

            # Update meta
            meta = {
                "version": tag,
                "last_check": time.time(),
                "exe_path": str(self._tool_paths.get(tool_name, ""))
            }
            with (tool_dir / "meta.json").open("w") as f:
                json.dump(meta, f)

            archive_path.unlink(missing_ok=True)

            if tool_name in self._tool_paths:
                self._make_executable(self._tool_paths[tool_name])
                self._report(tool_name, 1.0, f"{tool.name} {tag} ready")
                return self._tool_paths[tool_name]

        except Exception as e:
            self._report(tool_name, 0, f"Error: {e}")

        return self._tool_paths.get(tool_name)

    def _make_executable(self, path: Path) -> None:
        try:
            path.chmod(path.stat().st_mode | 0o111)
        except Exception:
            pass

    def launch_steamautocrack(
        self,
        app_id: int,
        install_path: str,
        args: Optional[list[str]] = None
    ) -> bool:
        """Launch SteamAutoCrack for a game."""
        exe = self.get_tool_path("steamautocrack")
        if not exe:
            return False

        cmd = [str(exe)]
        if args:
            cmd.extend(args)
        else:
            cmd.extend(["--appid", str(app_id), "--path", install_path])

        try:
            subprocess.Popen(cmd)
            return True
        except Exception:
            return False

    def launch_steamless(
        self,
        target_file: str,
        output_dir: Optional[str] = None,
        args: Optional[list[str]] = None
    ) -> bool:
        """Launch Steamless to remove SteamStub."""
        exe = self.get_tool_path("steamless")
        if not exe:
            return False

        cmd = [str(exe)]
        if args:
            cmd.extend(args)
        else:
            cmd.extend(["-f", target_file])
            if output_dir:
                cmd.extend(["-o", output_dir])

        try:
            subprocess.Popen(cmd)
            return True
        except Exception:
            return False

    def close(self) -> None:
        self._http.close()


def create_external_tools_manager(
    tools_dir: Optional[Path] = None,
    progress_callback: Optional[Callable[[str, float, str], None]] = None
) -> ExternalToolsManager:
    return ExternalToolsManager(tools_dir, progress_callback)