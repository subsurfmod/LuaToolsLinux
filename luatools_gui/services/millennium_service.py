"""Millennium service - integrates with Millennium's RPC API on port 38495."""

from __future__ import annotations

import json
import urllib.error
import urllib.request
from typing import Any, Optional

MILLENNIUM_RPC_URL = "http://127.0.0.1:38495/rpc"


class MillenniumService:
    """Client for Millennium's RPC API."""

    def __init__(self, base_url: str = MILLENNIUM_RPC_URL) -> None:
        self.base_url = base_url

    def is_available(self) -> bool:
        """Check if Millennium RPC is reachable."""
        try:
            req = urllib.request.Request(
                self.base_url,
                data=json.dumps({"method": "Ping", "args": {}}).encode("utf-8"),
                headers={"Content-Type": "application/json"},
                method="POST",
            )
            with urllib.request.urlopen(req, timeout=2) as resp:
                return resp.status == 200
        except Exception:
            return False

    def call(self, method: str, args: Optional[dict[str, Any]] = None) -> dict[str, Any]:
        """Call a Millennium RPC method."""
        if args is None:
            args = {}

        payload = {"method": method, "args": args}
        data = json.dumps(payload).encode("utf-8")

        req = urllib.request.Request(
            self.base_url,
            data=data,
            headers={"Content-Type": "application/json"},
            method="POST",
        )

        try:
            with urllib.request.urlopen(req, timeout=10) as resp:
                body = resp.read().decode("utf-8")
                return json.loads(body)
        except urllib.error.HTTPError as exc:
            return {"success": False, "error": f"HTTP {exc.code}: {exc.reason}"}
        except urllib.error.URLError as exc:
            return {"success": False, "error": f"Connection error: {exc}"}
        except Exception as exc:
            return {"success": False, "error": str(exc)}

    def get_version(self) -> Optional[str]:
        result = self.call("version")
        if result.get("success"):
            return result.get("result")
        return None

    def get_steam_path(self) -> Optional[str]:
        result = self.call("steam_path")
        if result.get("success"):
            return result.get("result")
        return None

    def add_browser_js(self, content: str, pattern: str = ".*") -> bool:
        result = self.call("add_browser_js", {"content": content, "pattern": pattern})
        return result.get("success", False)

    def call_frontend_method(self, method: str, params: list) -> dict[str, Any]:
        return self.call("call_frontend_method", {"method": method, "params": params})
