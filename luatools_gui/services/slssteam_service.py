"""SLSsteam config service - manages FakeAppIds, tokens, DLCs in config.yaml."""

from __future__ import annotations

import os
from pathlib import Path
from typing import Optional

from ..models.config import SlssteamConfig, SLSSTEAM_CONFIG_PATH


class SlssteamService:
    """Service for reading and writing SLSsteam config.yaml."""

    def __init__(self, config_path: Optional[Path] = None) -> None:
        self.config_path = config_path or SLSSTEAM_CONFIG_PATH
        self._config: Optional[SlssteamConfig] = None

    @property
    def config(self) -> SlssteamConfig:
        if self._config is None:
            self._config = self._load()
        return self._config

    def _load(self) -> SlssteamConfig:
        cfg = SlssteamConfig()

        if not self.config_path.exists():
            return cfg

        try:
            content = self.config_path.read_text(encoding="utf-8", errors="replace")
            self._parse_yaml_like(content, cfg)
        except Exception:
            pass

        return cfg

    def _parse_yaml_like(self, content: str, cfg: SlssteamConfig) -> None:
        """Parse SLSsteam config.yaml (uses PyYAML when available, with fallback)."""
        try:
            import yaml
            data = yaml.safe_load(content)
            if isinstance(data, dict):
                for k, v in (data.get("FakeAppIds") or {}).items():
                    try:
                        cfg.fake_app_ids[int(k)] = int(v)
                    except (ValueError, TypeError):
                        pass
                for k, v in (data.get("AppTokens") or {}).items():
                    try:
                        cfg.app_tokens[int(k)] = str(v)
                    except (ValueError, TypeError):
                        pass
                for app_k, dlcs in (data.get("DlcData") or {}).items():
                    try:
                        aid = int(app_k)
                        if isinstance(dlcs, dict):
                            cfg.dlc_data[aid] = {int(dk): str(dv) for dk, dv in dlcs.items()}
                    except (ValueError, TypeError):
                        pass
                # Handle SLSsteam's actual key casing
                if "PlayNotOwnedGames" in data:
                    cfg.play_not_owned = bool(data["PlayNotOwnedGames"])
                if "LimitDownloadsToSteamLibrary" in data:
                    cfg.limit_downloads = bool(data["LimitDownloadsToSteamLibrary"])
                if "AdditionalApps" in data and isinstance(data["AdditionalApps"], list):
                    cfg.additional_apps = [int(x) for x in data["AdditionalApps"] if str(x).isdigit()]
                if "DepotKeys" in data and isinstance(data["DepotKeys"], dict):
                    for k, v in data["DepotKeys"].items():
                        try:
                            cfg.depot_keys[int(k)] = str(v)
                        except (ValueError, TypeError):
                            pass
                if isinstance(data.get("workshop"), dict):
                    cfg.workshop_path = str(data["workshop"].get("workshopPath") or "")
                return
        except Exception:
            pass

        lines = content.splitlines()

        current_section = None
        for line in lines:
            stripped = line.strip()

            # Skip comments and empty lines
            if not stripped or stripped.startswith("#"):
                continue

            # Detect section headers
            if stripped.endswith(":") and not stripped.startswith("  ") and not stripped.startswith("\t"):
                section_name = stripped.rstrip(":").strip()
                if section_name in ("FakeAppIds", "AppTokens", "DlcData", "AdditionalApps", "DepotKeys"):
                    current_section = section_name
                elif section_name in ("PlayNotOwnedGames", "LimitDownloadsToSteamLibrary"):
                    current_section = section_name
                elif section_name == "workshop":
                    current_section = "workshop"
                else:
                    current_section = None
                continue

            # Parse key-value pairs
            if ":" in stripped:
                key, _, value = stripped.partition(":")
                key = key.strip().strip('"').strip("'")
                value = value.strip().strip('"').strip("'")

                if current_section == "FakeAppIds" and key.isdigit():
                    try:
                        cfg.fake_app_ids[int(key)] = int(value)
                    except ValueError:
                        pass

                elif current_section == "AppTokens" and key.isdigit():
                    cfg.app_tokens[int(key)] = value

                elif current_section == "DlcData" and key.isdigit():
                    # DlcData has nested structure: appid: { dlcid: "name" }
                    # We need to track the parent appid for DLC entries
                    pass

                elif current_section == "AdditionalApps" and key.isdigit():
                    try:
                        cfg.additional_apps.append(int(key))
                    except ValueError:
                        pass

                elif current_section == "DepotKeys" and key.isdigit():
                    try:
                        cfg.depot_keys[int(key)] = value
                    except ValueError:
                        pass

                elif current_section == "PlayNotOwnedGames":
                    cfg.play_not_owned = value.lower() in ("true", "1", "yes", "on")

                elif current_section == "LimitDownloadsToSteamLibrary":
                    cfg.limit_downloads = value.lower() in ("true", "1", "yes", "on")

                elif current_section == "workshop" and key == "workshopPath":
                    cfg.workshop_path = value

    def save(self) -> bool:
        """Save current config to disk."""
        try:
            self.config_path.parent.mkdir(parents=True, exist_ok=True)
            content = self._serialize()
            # Atomic write
            tmp_path = str(self.config_path) + ".tmp"
            Path(tmp_path).write_text(content, encoding="utf-8")
            os.replace(tmp_path, self.config_path)
            return True
        except Exception:
            return False

    def _serialize(self) -> str:
        """Serialize config to YAML-like format with correct SLSsteam key casing."""
        lines = []

        if cfg := self.config:
            # FakeAppIds
            if cfg.fake_app_ids:
                lines.append("FakeAppIds:")
                for appid, depotid in sorted(cfg.fake_app_ids.items()):
                    lines.append(f"  {appid}: {depotid}")
                lines.append("")

            # AppTokens
            if cfg.app_tokens:
                lines.append("AppTokens:")
                for appid, token in sorted(cfg.app_tokens.items()):
                    lines.append(f"  {appid}: {token}")
                lines.append("")

            # DlcData
            if cfg.dlc_data:
                lines.append("DlcData:")
                for appid, dlcs in sorted(cfg.dlc_data.items()):
                    lines.append(f"  {appid}:")
                    for dlcid, name in sorted(dlcs.items()):
                        safe_name = str(name).replace('"', '\\"')
                        lines.append(f'    {dlcid}: "{safe_name}"')
                lines.append("")

            # AdditionalApps
            if cfg.additional_apps:
                lines.append("AdditionalApps:")
                for appid in sorted(cfg.additional_apps):
                    lines.append(f"  {appid}")
                lines.append("")

            # DepotKeys
            if cfg.depot_keys:
                lines.append("DepotKeys:")
                for depotid, key in sorted(cfg.depot_keys.items()):
                    lines.append(f"  {depotid}: {key}")
                lines.append("")

            # Settings - use SLSsteam's actual key casing
            lines.append(f"PlayNotOwnedGames: {'true' if cfg.play_not_owned else 'false'}")
            lines.append(f"LimitDownloadsToSteamLibrary: {'true' if cfg.limit_downloads else 'false'}")

            # Workshop
            if cfg.workshop_path:
                lines.append("workshop:")
                lines.append(f'  workshopPath: "{cfg.workshop_path}"')

        return "\n".join(lines) + "\n"

    # ----- FakeAppId operations -----

    def get_fake_app_id(self, appid: int) -> Optional[int]:
        return self.config.fake_app_ids.get(appid)

    def set_fake_app_id(self, appid: int, depotid: int = 480) -> None:
        self.config.fake_app_ids[appid] = depotid

    def remove_fake_app_id(self, appid: int) -> bool:
        return self.config.fake_app_ids.pop(appid, None) is not None

    def has_fake_app_id(self, appid: int) -> bool:
        return appid in self.config.fake_app_ids

    # ----- Token operations -----

    def get_token(self, appid: int) -> Optional[str]:
        return self.config.app_tokens.get(appid)

    def set_token(self, appid: int, token: str) -> None:
        self.config.app_tokens[appid] = token

    def remove_token(self, appid: int) -> bool:
        return self.config.app_tokens.pop(appid, None) is not None

    def has_token(self, appid: int) -> bool:
        return appid in self.config.app_tokens

    # ----- AdditionalApps operations -----

    def get_additional_apps(self) -> list[int]:
        return list(self.config.additional_apps)

    def add_additional_app(self, appid: int) -> bool:
        if appid not in self.config.additional_apps:
            self.config.additional_apps.append(appid)
            return True
        return False

    def remove_additional_app(self, appid: int) -> bool:
        if appid in self.config.additional_apps:
            self.config.additional_apps.remove(appid)
            return True
        return False

    def has_additional_app(self, appid: int) -> bool:
        return appid in self.config.additional_apps

    # ----- DepotKeys operations -----

    def get_depot_key(self, depotid: int) -> Optional[str]:
        return self.config.depot_keys.get(depotid)

    def set_depot_key(self, depotid: int, key: str) -> None:
        self.config.depot_keys[depotid] = key

    def remove_depot_key(self, depotid: int) -> bool:
        return self.config.depot_keys.pop(depotid, None) is not None

    def has_depot_key(self, depotid: int) -> bool:
        return depotid in self.config.depot_keys

    # ----- Settings -----

    def set_play_not_owned(self, value: bool) -> None:
        self.config.play_not_owned = value

    def set_limit_downloads(self, value: bool) -> None:
        self.config.limit_downloads = value

    def set_workshop_path(self, path: str) -> None:
        self.config.workshop_path = path
