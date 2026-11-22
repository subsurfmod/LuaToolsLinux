"""ProtocolService - handles luatools:// URIs and xdg-mime desktop registration."""

from __future__ import annotations

import shutil
import subprocess
import sys
import urllib.parse
from dataclasses import dataclass
from pathlib import Path
from typing import Optional


@dataclass
class ProtocolAction:
    kind: str  # "game", "install", "manage", "fix"
    app_id: int
    source: Optional[str] = None


class ProtocolService:
    """Parses luatools:// URLs and registers the Linux x-scheme-handler/luatools."""

    @staticmethod
    def parse(url: str) -> Optional[ProtocolAction]:
        if not url or not url.lower().startswith("luatools://"):
            return None
        parsed = urllib.parse.urlparse(url)
        kind = (parsed.netloc or "").lower()
        path_parts = [p for p in parsed.path.split("/") if p]
        if not path_parts or not path_parts[0].isdigit():
            return None
        app_id = int(path_parts[0])
        if kind not in ("game", "install", "manage", "fix"):
            return None
        qs = urllib.parse.parse_qs(parsed.query)
        source = qs.get("source", [None])[0]
        return ProtocolAction(kind=kind, app_id=app_id, source=source)

    @staticmethod
    def register_desktop_handler() -> tuple[bool, str]:
        """Install ~/.local/share/applications/luatools.desktop and register luatools://."""
        apps_dir = Path.home() / ".local/share/applications"
        apps_dir.mkdir(parents=True, exist_ok=True)
        desktop_file = apps_dir / "luatools.desktop"

        exe = shutil.which("luatools")
        exec_cmd = f"{exe} %u" if exe else f"{sys.executable} -m luatools_gui.main %u"

        content = (
            "[Desktop Entry]\n"
            "Name=LuaTools Linux\n"
            "Comment=Steam manifest and lua configuration manager\n"
            f"Exec={exec_cmd}\n"
            "Terminal=false\n"
            "Type=Application\n"
            "Categories=Game;Utility;\n"
            "MimeType=x-scheme-handler/luatools;\n"
        )
        try:
            desktop_file.write_text(content, encoding="utf-8")
            subprocess.run(
                ["xdg-mime", "default", "luatools.desktop", "x-scheme-handler/luatools"],
                check=False,
                capture_output=True,
            )
            return True, f"Registered handler at {desktop_file}"
        except Exception as exc:
            return False, str(exc)
