"""SLSsteam configuration model."""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

SLSSTEAM_CONFIG_PATH = Path.home() / ".config" / "SLSsteam" / "config.yaml"


@dataclass
class SlssteamConfig:
    """SLSsteam configuration data."""

    path: Path = SLSSTEAM_CONFIG_PATH
    fake_app_ids: dict[int, int] = field(default_factory=dict)
    app_tokens: dict[int, str] = field(default_factory=dict)
    dlc_data: dict[int, dict[int, str]] = field(default_factory=dict)
    additional_apps: list[int] = field(default_factory=list)  # SLSsteam's AdditionalApps list
    depot_keys: dict[int, str] = field(default_factory=dict)  # DepotKeys section
    play_not_owned: bool = False
    limit_downloads: bool = False
    workshop_path: str = ""

    @property
    def exists(self) -> bool:
        return self.path.exists()