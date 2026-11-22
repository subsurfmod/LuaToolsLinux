"""Local HTTP server on 127.0.0.1:6767 for Steam store-page plugin RPC."""

from __future__ import annotations

import http.server
import json
import threading
import urllib.parse
from typing import Callable, Optional

from .lua_installer import LuaInstaller


class HttpServerService:
    """Serves local RPC endpoints on 127.0.0.1:6767 for injected Steam store-page JS."""

    def __init__(
        self,
        port: int = 6767,
        lua_installer: Optional[LuaInstaller] = None,
        on_open_fix: Optional[Callable[[int], None]] = None,
        on_open_settings: Optional[Callable[[], None]] = None,
        on_add_app: Optional[Callable[[int], None]] = None,
    ) -> None:
        self.port = port
        self.lua_installer = lua_installer
        self.on_open_fix = on_open_fix
        self.on_open_settings = on_open_settings
        self.on_add_app = on_add_app
        self._server: Optional[http.server.ThreadingHTTPServer] = None
        self._thread: Optional[threading.Thread] = None

    @property
    def is_running(self) -> bool:
        return self._server is not None

    def start(self) -> bool:
        if self._server is not None:
            return True
        service = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_OPTIONS(self) -> None:
                self.send_response(204)
                self._cors()
                self.end_headers()

            def do_GET(self) -> None:
                self._handle("GET")

            def do_POST(self) -> None:
                self._handle("POST")

            def _cors(self) -> None:
                self.send_header("Access-Control-Allow-Origin", "*")
                self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
                self.send_header("Access-Control-Allow-Headers", "Content-Type")

            def _json(self, code: int, data: dict) -> None:
                body = json.dumps(data).encode("utf-8")
                self.send_response(code)
                self._cors()
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def _handle(self, method: str) -> None:
                parsed = urllib.parse.urlparse(self.path)
                parts = [p for p in parsed.path.split("/") if p]

                if parts == ["ping"]:
                    self._json(200, {"ok": True, "platform": "linux"})
                    return

                if len(parts) == 2 and parts[1].isdigit():
                    action, aid = parts[0], int(parts[1])
                    if action == "has":
                        exists = bool(
                            service.lua_installer and service.lua_installer.read_installed_lua(aid) is not None
                        )
                        self._json(200, {"has": exists, "appid": aid})
                        return
                    if action == "add":
                        if service.on_add_app:
                            service.on_add_app(aid)
                        self._json(200, {"queued": True, "appid": aid})
                        return
                    if action == "remove":
                        if service.lua_installer:
                            p = service.lua_installer.read_installed_lua(aid)
                            if p and p.exists():
                                p.unlink(missing_ok=True)
                        self._json(200, {"removed": True, "appid": aid})
                        return

                if len(parts) == 3 and parts[0] == "open" and parts[1] == "fix" and parts[2].isdigit():
                    if service.on_open_fix:
                        service.on_open_fix(int(parts[2]))
                    self._json(200, {"ok": True})
                    return

                if parts == ["open", "settings"]:
                    if service.on_open_settings:
                        service.on_open_settings()
                    self._json(200, {"ok": True})
                    return

                self._json(404, {"error": "Not found"})

            def log_message(self, format: str, *args) -> None:
                pass

        try:
            self._server = http.server.ThreadingHTTPServer(("127.0.0.1", self.port), Handler)
            self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
            self._thread.start()
            return True
        except Exception:
            self._server = None
            return False

    def stop(self) -> None:
        if self._server:
            try:
                self._server.shutdown()
                self._server.server_close()
            except Exception:
                pass
            self._server = None
            self._thread = None
