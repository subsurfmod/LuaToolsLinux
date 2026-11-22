"""Authentication service - Discord PKCE OAuth & 6-char Discord bot code sign-in."""

from __future__ import annotations

import base64
import hashlib
import html
import http.server
import json
import os
import secrets
import threading
import time
import urllib.parse
import webbrowser
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

import httpx

SUPABASE_URL = "https://db.lua.tools"
SUPABASE_ANON_KEY = (
    "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9."
    "eyJpYXQiOjE3NzYwMzkzNzYsImV4cCI6MTg5MzQ1NjAwMCwicm9sZSI6ImFub24iLCJpc3MiOiJzdXBhYmFzZSJ9."
    "f_-K38u3odjltP-g_67FVmG32Vg-_-k-lNBvIaVUVBM"
)
API_BASE_URL = "https://lua.tools"
OAUTH_CALLBACK_PORT = 53789
OAUTH_CALLBACK_URL = f"http://localhost:{OAUTH_CALLBACK_PORT}/callback"
BOT_ACCOUNT_DOMAIN = "@bot.lua.tools"


class AuthError(Exception):
    """Authentication failure."""


@dataclass
class AuthSession:
    access_token: str = ""
    refresh_token: str = ""
    expires_at: float = 0.0
    display_name: str = ""
    email: str = ""
    avatar_url: str = ""


class AuthService:
    """Manages Supabase Discord OAuth and bot-code authentication."""

    def __init__(self, auth_file: Optional[Path] = None) -> None:
        self.auth_file = auth_file or (Path.home() / ".local/share/luatools/auth.json")
        self._http = httpx.Client(timeout=30.0, follow_redirects=True)
        self._lock = threading.Lock()
        self._session: Optional[AuthSession] = None
        self._listeners: list[Callable[[], None]] = []

    def add_listener(self, cb: Callable[[], None]) -> None:
        self._listeners.append(cb)

    def _notify(self) -> None:
        for cb in list(self._listeners):
            try:
                cb()
            except Exception:
                pass

    @property
    def is_signed_in(self) -> bool:
        return self._session is not None and bool(self._session.refresh_token)

    @property
    def display_name(self) -> str:
        return self._session.display_name if self._session else ""

    @property
    def email(self) -> str:
        return self._session.email if self._session else ""

    @property
    def is_bot_provisioned(self) -> bool:
        return self.is_signed_in and self.email.lower().endswith(BOT_ACCOUNT_DOMAIN)

    def initialize(self) -> bool:
        """Restore saved session from disk and refresh if close to expiry."""
        stored = self._load_stored()
        if not stored or not stored.refresh_token:
            return False
        self._session = stored
        if stored.expires_at > time.time() + 120:
            self._notify()
            return True
        try:
            self._refresh()
            self._notify()
            return True
        except Exception:
            self.sign_out()
            return False

    def sign_in_with_browser(self, timeout_seconds: int = 180) -> AuthSession:
        """Open Discord OAuth PKCE flow in default browser and capture localhost callback."""
        verifier = base64.urlsafe_b64encode(secrets.token_bytes(48)).decode("ascii").rstrip("=")
        digest = hashlib.sha256(verifier.encode("ascii")).digest()
        challenge = base64.urlsafe_b64encode(digest).decode("ascii").rstrip("=")

        auth_url = (
            f"{SUPABASE_URL}/auth/v1/authorize?provider=discord"
            f"&redirect_to={urllib.parse.quote(OAUTH_CALLBACK_URL, safe='')}"
            f"&code_challenge={challenge}&code_challenge_method=s256"
        )

        result_box: dict[str, Optional[str]] = {"code": None, "error": None}
        done_event = threading.Event()

        class CallbackHandler(http.server.BaseHTTPRequestHandler):
            def do_GET(self) -> None:
                parsed = urllib.parse.urlparse(self.path)
                params = urllib.parse.parse_qs(parsed.query)
                code = params.get("code", [None])[0]
                error = params.get("error_description", [None])[0]
                if code is None and error is None:
                    self.send_response(404)
                    self.end_headers()
                    return

                result_box["code"] = code
                result_box["error"] = error
                ok = code is not None
                body = self._result_html(ok, error).encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                done_event.set()

            def log_message(self, format: str, *args) -> None:
                pass

            @staticmethod
            def _result_html(ok: bool, err: Optional[str]) -> str:
                title = "Signed in!" if ok else "Sign-in failed"
                color = "#a78bfa" if ok else "#f87171"
                msg = (
                    "You can close this tab and return to LuaTools."
                    if ok
                    else html.escape(err or "Please try again from the app.")
                )
                return (
                    f"<!doctype html><html><head><meta charset='utf-8'><title>LuaTools</title>"
                    f"<style>body{{background:#0b0b12;color:#e5e7eb;font-family:sans-serif;"
                    f"display:flex;align-items:center;justify-content:center;height:100vh;margin:0;}}"
                    f".card{{text-align:center;padding:2.5rem 3rem;background:#14141c;"
                    f"border:1px solid rgba(255,255,255,.08);border-radius:14px;}}"
                    f"h1{{color:{color};margin:0 0 .5rem;}}p{{color:#9ca3af;margin:0;}}</style></head>"
                    f"<body><div class='card'><h1>{title}</h1><p>{msg}</p></div></body></html>"
                )

        server = http.server.HTTPServer(("127.0.0.1", OAUTH_CALLBACK_PORT), CallbackHandler)
        server_thread = threading.Thread(target=self._serve_until_done, args=(server, done_event), daemon=True)
        server_thread.start()

        webbrowser.open(auth_url)

        if not done_event.wait(timeout=timeout_seconds):
            server.shutdown()
            server.server_close()
            raise AuthError("Sign-in timed out waiting for browser callback.")

        server.shutdown()
        server.server_close()

        if result_box["error"] or not result_box["code"]:
            raise AuthError(result_box["error"] or "Sign-in was denied.")

        resp = self._http.post(
            f"{SUPABASE_URL}/auth/v1/token?grant_type=pkce",
            headers={"apikey": SUPABASE_ANON_KEY},
            json={"auth_code": result_box["code"], "code_verifier": verifier},
        )
        if resp.status_code != 200:
            raise AuthError(f"Token exchange failed ({resp.status_code}).")

        session = self._apply_supabase_session(resp.json())
        self._notify()
        return session

    @staticmethod
    def _serve_until_done(server: http.server.HTTPServer, done_event: threading.Event) -> None:
        server.timeout = 0.5
        while not done_event.is_set():
            server.handle_request()

    def sign_in_with_code(self, code: str) -> AuthSession:
        """Sign in using a 6-character Discord bot code (/login)."""
        cleaned = code.strip().upper()
        if not cleaned:
            raise AuthError("Please enter a 6-character Discord code.")

        redeem_resp = self._http.post(
            f"{API_BASE_URL}/api/auth/code/redeem",
            json={"code": cleaned},
        )
        if redeem_resp.status_code == 410:
            raise AuthError("Code has expired. Generate a new one in Discord.")
        if redeem_resp.status_code in (400, 404):
            raise AuthError("Invalid code.")
        if redeem_resp.status_code != 200:
            raise AuthError(f"Code redemption failed ({redeem_resp.status_code}).")

        token_hash = redeem_resp.json().get("token", "")
        if not token_hash:
            raise AuthError("Server returned an empty magic-link token.")

        verify_resp = self._http.post(
            f"{SUPABASE_URL}/auth/v1/verify",
            headers={"apikey": SUPABASE_ANON_KEY},
            json={"type": "magiclink", "token_hash": token_hash},
        )
        if verify_resp.status_code != 200:
            raise AuthError(f"Token verification failed ({verify_resp.status_code}).")

        session = self._apply_supabase_session(verify_resp.json())
        self._notify()
        return session

    def get_valid_access_token(self) -> str:
        """Return a valid Bearer token, refreshing if needed."""
        with self._lock:
            if not self._session or not self._session.refresh_token:
                raise AuthError("Not signed in. Sign in from Settings first.")
            if self._session.expires_at <= time.time() + 120:
                self._refresh()
            return self._session.access_token

    def sign_out(self) -> None:
        self._session = None
        try:
            if self.auth_file.exists():
                self.auth_file.unlink()
        except Exception:
            pass
        self._notify()

    def _refresh(self) -> AuthSession:
        if not self._session or not self._session.refresh_token:
            raise AuthError("No refresh token.")
        resp = self._http.post(
            f"{SUPABASE_URL}/auth/v1/token?grant_type=refresh_token",
            headers={"apikey": SUPABASE_ANON_KEY},
            json={"refresh_token": self._session.refresh_token},
        )
        if resp.status_code != 200:
            raise AuthError(f"Session refresh failed ({resp.status_code}).")
        return self._apply_supabase_session(resp.json())

    def _apply_supabase_session(self, data: dict) -> AuthSession:
        user = data.get("user") or {}
        meta = user.get("user_metadata") or {}
        custom = meta.get("custom_claims") or {}
        display_name = (
            custom.get("global_name")
            or meta.get("full_name")
            or meta.get("name")
            or user.get("email")
            or "LuaTools User"
        )
        session = AuthSession(
            access_token=data.get("access_token", ""),
            refresh_token=data.get("refresh_token", ""),
            expires_at=time.time() + int(data.get("expires_in", 3600)),
            display_name=display_name,
            email=user.get("email") or "",
            avatar_url=meta.get("avatar_url") or "",
        )
        self._session = session
        self._save_stored(session)
        return session

    def _load_stored(self) -> Optional[AuthSession]:
        if not self.auth_file.exists():
            return None
        try:
            data = json.loads(self.auth_file.read_text(encoding="utf-8"))
            return AuthSession(**data)
        except Exception:
            return None

    def _save_stored(self, session: AuthSession) -> None:
        try:
            self.auth_file.parent.mkdir(parents=True, exist_ok=True)
            tmp = self.auth_file.with_suffix(".tmp")
            tmp.write_text(
                json.dumps(
                    {
                        "access_token": session.access_token,
                        "refresh_token": session.refresh_token,
                        "expires_at": session.expires_at,
                        "display_name": session.display_name,
                        "email": session.email,
                        "avatar_url": session.avatar_url,
                    },
                    indent=2,
                ),
                encoding="utf-8",
            )
            os.chmod(tmp, 0o600)
            os.replace(tmp, self.auth_file)
        except Exception:
            pass
