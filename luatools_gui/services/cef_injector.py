"""CEF CDP Injector Service - injects luatools.js into Steam's store tabs via port 8080."""

from __future__ import annotations

import asyncio
import json
import threading
import time
from pathlib import Path
from typing import Optional

import httpx

from ..models.steam import SteamInstallation

CEF_DEBUG_URL = "http://127.0.0.1:8080/json"


class CefInjectorService:
    """Polls Steam CEF CDP on 127.0.0.1:8080 and injects luatools.js into store tabs."""

    def __init__(
        self,
        steam_installation: Optional[SteamInstallation] = None,
        http_port: int = 6767,
    ) -> None:
        self.steam_installation = steam_installation
        self.http_port = http_port
        self._stop_event = threading.Event()
        self._thread: Optional[threading.Thread] = None
        self._injected_tabs: set[str] = set()
        self.last_status: str = "Stopped"

    @property
    def is_running(self) -> bool:
        return self._thread is not None and self._thread.is_alive()

    def ensure_cef_debugging_enabled(self) -> tuple[bool, str]:
        """Create .cef-enable-remote-debugging in Steam root so Steam opens CDP on :8080."""
        if not self.steam_installation:
            return False, "Steam installation not detected."
        marker = self.steam_installation.root / ".cef-enable-remote-debugging"
        try:
            marker.touch(exist_ok=True)
            return True, f"Enabled at {marker} (restart Steam to take effect)"
        except Exception as exc:
            return False, str(exc)

    def _find_luatools_js(self) -> Optional[Path]:
        candidates = [
            Path.home() / ".local/share/millennium/plugins/luatools/public/luatools.js",
            Path.home() / ".local/share/Steam/steamui/LuaTools/luatools.js",
        ]
        for p in candidates:
            if p.exists():
                return p
        return None

    def _build_polyfill_js(self) -> str:
        base = f"http://127.0.0.1:{self.http_port}"
        return f"""
(function() {{
  if (window.__luatoolsLinuxPolyfill) return;
  window.__luatoolsLinuxPolyfill = true;
  const BASE = "{base}";
  if (!window.Millennium) {{
    window.Millennium = {{
      callServerMethod: async function(method, args) {{
        const r = await fetch(BASE + "/rpc", {{
          method: "POST",
          headers: {{"Content-Type": "application/json"}},
          body: JSON.stringify({{method, args: args || {{}}}})
        }});
        return await r.text();
      }}
    }};
  }}
}})();
"""

    def start(self) -> None:
        if self.is_running:
            return
        self._stop_event.clear()
        self._thread = threading.Thread(target=self._run_loop, daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._stop_event.set()
        self.last_status = "Stopped"

    def _run_loop(self) -> None:
        self.last_status = "Polling CDP :8080..."
        client = httpx.Client(timeout=2.0)
        while not self._stop_event.is_set():
            try:
                resp = client.get(CEF_DEBUG_URL)
                if resp.status_code == 200:
                    tabs = resp.json()
                    store_tabs = [
                        t for t in tabs
                        if "store.steampowered.com" in (t.get("url") or "")
                        and t.get("webSocketDebuggerUrl")
                    ]
                    self.last_status = f"CDP connected ({len(store_tabs)} store tab(s))"
                    for tab in store_tabs:
                        tid = tab.get("id", "")
                        if tid and tid not in self._injected_tabs:
                            ws_url = tab["webSocketDebuggerUrl"]
                            if self._inject_tab(ws_url):
                                self._injected_tabs.add(tid)
                else:
                    self.last_status = "CDP :8080 not responding"
            except Exception:
                self.last_status = "Waiting for Steam CDP :8080..."
            self._stop_event.wait(2.0)
        client.close()

    def _inject_tab(self, ws_url: str) -> bool:
        js_path = self._find_luatools_js()
        js_code = self._build_polyfill_js()
        if js_path:
            js_code += "\n" + js_path.read_text(encoding="utf-8", errors="replace")

        async def _send() -> None:
            import websockets
            async with websockets.connect(ws_url, open_timeout=3) as ws:
                payload = {
                    "id": 1,
                    "method": "Runtime.evaluate",
                    "params": {"expression": js_code},
                }
                await ws.send(json.dumps(payload))
                await asyncio.wait_for(ws.recv(), timeout=3)

        try:
            asyncio.run(_send())
            return True
        except Exception:
            return False
