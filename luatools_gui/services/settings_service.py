"""Application settings service - persists user preferences in ~/.config/luatools/settings.json."""

from __future__ import annotations

import json
import os
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Optional


@dataclass
class AppSettings:
    """User-configurable settings for LuaTools Linux."""
    steam_path_override: str = ""
    auto_update_apps: bool = True
    fast_fetch: bool = True
    donate_keys: bool = True
    hubcap_api_key: str = ""
    cef_injector_enabled: bool = False
    http_server_enabled: bool = True
    http_server_port: int = 6767
    language: str = "en"
    loaded_app_ids: list[int] = field(default_factory=list)


class SettingsService:
    """Loads and saves AppSettings to ~/.config/luatools/settings.json."""

    def __init__(self, config_path: Optional[Path] = None) -> None:
        self.config_path = config_path or (Path.home() / ".config/luatools/settings.json")
        self._settings: Optional[AppSettings] = None

    @property
    def settings(self) -> AppSettings:
        if self._settings is None:
            self._settings = self._load()
        return self._settings

    def _load(self) -> AppSettings:
        if not self.config_path.exists():
            return AppSettings()
        try:
            data = json.loads(self.config_path.read_text(encoding="utf-8"))
            known = {f.name for f in AppSettings.__dataclass_fields__.values()}
            filtered = {k: v for k, v in data.items() if k in known}
            return AppSettings(**filtered)
        except Exception:
            return AppSettings()

    def save(self) -> bool:
        try:
            self.config_path.parent.mkdir(parents=True, exist_ok=True)
            tmp = self.config_path.with_suffix(".tmp")
            tmp.write_text(json.dumps(asdict(self.settings), indent=2), encoding="utf-8")
            os.replace(tmp, self.config_path)
            return True
        except Exception:
            return False

    def record_loaded_app(self, app_id: int) -> None:
        ids = [i for i in self.settings.loaded_app_ids if i != app_id]
        ids.insert(0, app_id)
        self.settings.loaded_app_ids = ids[:20]
        self.save()
