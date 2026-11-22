"""Depot downloader service - integrates with DepotDownloaderMod for depot downloads."""

from __future__ import annotations

import os
import re
import subprocess
import threading
import urllib.request
import json
import zipfile
import shutil
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Optional

from ..models.steam import SteamLibrary


@dataclass
class DownloadState:
    """State of an ongoing depot download."""

    status: str = "idle"  # idle, downloading, done, failed, cancelled
    progress: float = 0.0
    message: str = ""
    download_path: str = ""
    phase: str = ""  # PreAllocating, Validating, Downloading
    process: Optional[subprocess.Popen] = None


# GitHub release info for DepotDownloader (SteamRE)
DEPOT_DOWNLOADER_REPO = "SteamRE/DepotDownloader"
DEPOT_DOWNLOADER_API_URL = f"https://api.github.com/repos/{DEPOT_DOWNLOADER_REPO}/releases/latest"


class DepotService:
    """Service for downloading depot content using DepotDownloaderMod."""

    def __init__(self) -> None:
        self.state = DownloadState()
        self._lock = threading.Lock()
        self._tool_dir = Path.home() / ".local/share/luatools/depotdownloader"
        self._tool_dir.mkdir(parents=True, exist_ok=True)

    def find_depot_downloader(self, custom_path: str = "") -> Optional[str]:
        """Find the DepotDownloaderMod executable."""
        if custom_path:
            p = Path(custom_path)
            if p.is_dir():
                for name in ["DepotDownloaderMod", "DepotDownloaderMod.exe", "DepotDownloader", "DepotDownloader.exe"]:
                    candidate = p / name
                    if candidate.exists():
                        return str(candidate)
            elif p.is_file():
                return str(p)

        # Check plugin directory
        plugin_dir = Path.home() / ".local/share/millennium/plugins/luatools/backend"
        for exe_name in ["DepotDownloaderMod", "DepotDownloaderMod.exe", "DepotDownloader"]:
            candidate = plugin_dir / exe_name
            if candidate.exists():
                return str(candidate)

        # Check our tool directory
        for exe_name in ["DepotDownloader", "DepotDownloaderMod"]:
            candidate = self._tool_dir / exe_name
            if candidate.exists():
                return str(candidate)

        return None

    def is_available(self, custom_path: str = "") -> bool:
        return self.find_depot_downloader(custom_path) is not None

    def get_tool_path(self) -> Path:
        """Get the path where the tool should be installed."""
        return self._tool_dir / "DepotDownloader"

    def get_tool_version(self) -> Optional[str]:
        """Get the currently installed tool version."""
        version_file = self._tool_dir / "version.txt"
        if version_file.exists():
            return version_file.read_text().strip()
        return None

    def check_for_tool_update(self) -> tuple[bool, Optional[str], Optional[str]]:
        """Check if a newer version of DepotDownloader is available.
        Returns: (update_available, latest_version, download_url)"""
        try:
            req = urllib.request.Request(
                DEPOT_DOWNLOADER_API_URL,
                headers={"Accept": "application/vnd.github.v3+json", "User-Agent": "LuaToolsLinux"}
            )
            with urllib.request.urlopen(req, timeout=10) as resp:
                data = json.loads(resp.read().decode())
        except Exception:
            return False, None, None

        latest_version = data.get("tag_name", "").replace("DepotDownloader_", "")
        current_version = self.get_tool_version()

        # Find linux-x64 asset
        download_url = None
        for asset in data.get("assets", []):
            if "linux-x64" in asset.get("name", "").lower():
                download_url = asset.get("browser_download_url")
                break

        if not download_url:
            return False, latest_version, None

        update_available = latest_version != current_version
        return update_available, latest_version, download_url

    def download_and_install_tool(self, download_url: str, version: str) -> bool:
        """Download and install DepotDownloader from GitHub."""
        zip_path = self._tool_dir / "DepotDownloader-linux-x64.zip"
        try:
            # Download
            urllib.request.urlretrieve(download_url, zip_path)

            # Extract
            with zipfile.ZipFile(zip_path, 'r') as zf:
                zf.extractall(self._tool_dir)

            # Find the executable
            exe_path = None
            for root, dirs, files in os.walk(self._tool_dir):
                for f in files:
                    if f == "DepotDownloader" or f.endswith(".exe"):
                        exe_path = Path(root) / f
                        break
                if exe_path:
                    break

            if not exe_path:
                return False

            # Move to standard location
            target = self.get_tool_path()
            if exe_path != target:
                shutil.move(str(exe_path), str(target))

            # Make executable
            target.chmod(target.stat().st_mode | 0o111)

            # Save version
            (self._tool_dir / "version.txt").write_text(version)

            # Cleanup
            zip_path.unlink(missing_ok=True)
            return True

        except Exception:
            return False

    def ensure_tool(self) -> Optional[str]:
        """Ensure DepotDownloader is available, auto-update if needed."""
        exe = self.find_depot_downloader()
        if exe:
            return exe

        # Try to download latest
        update_available, version, url = self.check_for_tool_update()
        if update_available and url and self.download_and_install_tool(url, version):
            return str(self.get_tool_path())

        return None

    def download_depot(
        self,
        appid: int,
        depot_id: int,
        download_dir: str,
        depot_key: str = "",
        manifest_id: str = "",
        custom_path: str = "",
        username: str = "anonymous",
        password: str = "",
        progress_callback: Optional[Callable[[float, str, str], None]] = None,
    ) -> bool:
        """Start a depot download using DepotDownloaderMod.
        progress_callback: called with (progress_percent, phase, message)"""
        exe = self.find_depot_downloader(custom_path) or self.ensure_tool()
        if not exe:
            self.state.status = "failed"
            self.state.message = "DepotDownloaderMod not found"
            return False

        download_path = Path(download_dir)
        download_path.mkdir(parents=True, exist_ok=True)

        cmd = [
            exe,
            "-app", str(appid),
            "-depot", str(depot_id),
            "-dir", str(download_path),
            "-username", username,
        ]

        if password:
            cmd.extend(["-password", password])

        if depot_key:
            cmd.extend(["-depotkey", depot_key])

        if manifest_id:
            cmd.extend(["-manifest", manifest_id])

        # Make executable
        try:
            st = os.stat(exe)
            os.chmod(exe, st.st_mode | 0o111)
        except Exception:
            pass

        self.state = DownloadState(
            status="downloading",
            progress=0.0,
            message="Starting download...",
            download_path=str(download_path),
            phase="",
        )

        def _run():
            try:
                process = subprocess.Popen(
                    cmd,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.STDOUT,
                    text=True,
                    encoding="utf-8",
                    errors="replace",
                )
                with self._lock:
                    self.state.process = process

                percent_regex = re.compile(r"(\d{1,3}\.\d{2})%")
                phase_regex = re.compile(r"(PreAllocating|Validating|Downloading)", re.IGNORECASE)
                last_line = ""

                while True:
                    line = process.stdout.readline()
                    if not line and process.poll() is not None:
                        break
                    if line:
                        clean = line.strip()
                        if clean:
                            last_line = clean
                            
                            # Detect phase
                            phase_match = phase_regex.search(clean)
                            if phase_match:
                                with self._lock:
                                    self.state.phase = phase_match.group(1)

                            match = percent_regex.search(clean)
                            if match:
                                try:
                                    p = float(match.group(1))
                                    with self._lock:
                                        self.state.progress = p
                                        self.state.message = f"{self.state.phase}: {p}%" if self.state.phase else f"Downloading: {p}%"
                                    if progress_callback:
                                        progress_callback(p, self.state.phase, clean)
                                except ValueError:
                                    pass
                            else:
                                with self._lock:
                                    self.state.message = clean
                                if progress_callback:
                                    progress_callback(0, self.state.phase, clean)

                rc = process.wait()
                with self._lock:
                    self.state.process = None

                if rc == 0:
                    self.state.status = "done"
                    self.state.progress = 100.0
                    self.state.message = f"Download complete: {download_path}"
                    if progress_callback:
                        progress_callback(100.0, "Done", self.state.message)
                else:
                    self.state.status = "failed"
                    self.state.message = f"Error (code {rc}): {last_line}"
                    if progress_callback:
                        progress_callback(0, "Error", self.state.message)

            except Exception as exc:
                with self._lock:
                    self.state.status = "failed"
                    self.state.message = str(exc)
                    self.state.process = None
                if progress_callback:
                    progress_callback(0, "Error", str(exc))

        thread = threading.Thread(target=_run, daemon=True)
        thread.start()
        return True

    def download_workshop(
        self,
        appid: int,
        pubfile_id: int,
        download_dir: str,
        custom_path: str = "",
        library_path: str = "",
        progress_callback: Optional[Callable[[float, str, str], None]] = None,
    ) -> bool:
        """Download a workshop item using DepotDownloaderMod."""
        exe = self.find_depot_downloader(custom_path) or self.ensure_tool()
        if not exe:
            self.state.status = "failed"
            self.state.message = "DepotDownloaderMod not found"
            return False

        download_path = Path(download_dir)
        download_path.mkdir(parents=True, exist_ok=True)

        cmd = [
            exe,
            "-app", str(appid),
            "-pubfile", str(pubfile_id),
            "-dir", str(download_path),
        ]

        try:
            st = os.stat(exe)
            os.chmod(exe, st.st_mode | 0o111)
        except Exception:
            pass

        self.state = DownloadState(
            status="downloading",
            progress=0.0,
            message="Starting workshop download...",
            download_path=str(download_path),
            phase="",
        )

        def _run():
            try:
                process = subprocess.Popen(
                    cmd,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.STDOUT,
                    text=True,
                    encoding="utf-8",
                    errors="replace",
                )
                with self._lock:
                    self.state.process = process

                percent_regex = re.compile(r"(\d{1,3}\.\d{2})%")
                phase_regex = re.compile(r"(PreAllocating|Validating|Downloading)", re.IGNORECASE)

                while True:
                    line = process.stdout.readline()
                    if not line and process.poll() is not None:
                        break
                    if line:
                        clean = line.strip()
                        if clean:
                            phase_match = phase_regex.search(clean)
                            if phase_match:
                                with self._lock:
                                    self.state.phase = phase_match.group(1)

                            match = percent_regex.search(clean)
                            if match:
                                try:
                                    p = float(match.group(1))
                                    with self._lock:
                                        self.state.progress = p
                                        self.state.message = f"{self.state.phase}: {p}%" if self.state.phase else f"Downloading: {p}%"
                                    if progress_callback:
                                        progress_callback(p, self.state.phase, clean)
                                except ValueError:
                                    pass
                            else:
                                with self._lock:
                                    self.state.message = clean
                                if progress_callback:
                                    progress_callback(0, self.state.phase, clean)

                rc = process.wait()
                with self._lock:
                    self.state.process = None

                if rc == 0:
                    self.state.status = "done"
                    self.state.progress = 100.0
                    self.state.message = f"Workshop download complete: {download_path}"
                    if progress_callback:
                        progress_callback(100.0, "Done", self.state.message)
                else:
                    self.state.status = "failed"
                    self.state.message = f"Download failed (code {rc})"
                    if progress_callback:
                        progress_callback(0, "Error", self.state.message)

            except Exception as exc:
                with self._lock:
                    self.state.status = "failed"
                    self.state.message = str(exc)
                    self.state.process = None
                if progress_callback:
                    progress_callback(0, "Error", str(exc))

        thread = threading.Thread(target=_run, daemon=True)
        thread.start()
        return True

    def cancel(self) -> None:
        """Cancel the current download."""
        with self._lock:
            if self.state.process:
                try:
                    self.state.process.kill()
                except Exception:
                    pass
                self.state.process = None
            self.state.status = "cancelled"
            self.state.message = "Cancelled by user"

    def get_state(self) -> DownloadState:
        with self._lock:
            state = self.state
            if state.process is None and state.status == "downloading":
                state.status = "failed"
                state.message = "Download process ended unexpectedly"
            return state
