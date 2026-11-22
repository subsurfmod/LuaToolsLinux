"""DonateKeysService - scans Steam config.vdf for DecryptionKeys and donates new ones."""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Optional

import httpx

from ..models.steam import SteamInstallation

DONATE_ENDPOINT = "http://167.235.229.108/donatekeys/send"
DONATE_USER_AGENT = "discord(dot)gg/luatools"
HEX64_RE = re.compile(r"^[0-9a-fA-F]{64}$")


class DonateKeysService:
    """Scans Steam config/config.vdf for depot decryption keys and submits unseen ones."""

    def __init__(
        self,
        steam_installation: SteamInstallation,
        cache_path: Optional[Path] = None,
    ) -> None:
        self.steam_installation = steam_installation
        self.cache_path = cache_path or (Path.home() / ".cache/luatools/donated_keys.json")
        self.cache_path.parent.mkdir(parents=True, exist_ok=True)
        self._http = httpx.Client(timeout=15.0)

    def extract_keys_from_vdf(self) -> dict[str, str]:
        """Return {depot_id: decryption_key} from Steam config/config.vdf."""
        vdf_path = self.steam_installation.root / "config" / "config.vdf"
        if not vdf_path.exists():
            return {}
        try:
            import vdf
            data = vdf.loads(vdf_path.read_text(encoding="utf-8", errors="replace"))
        except Exception:
            return {}

        keys: dict[str, str] = {}
        self._walk_depots(data, keys)
        return keys

    def _walk_depots(self, node: object, out: dict[str, str]) -> None:
        if not isinstance(node, dict):
            return
        for k, v in node.items():
            if str(k).lower() == "depots" and isinstance(v, dict):
                for depot_id, depot_data in v.items():
                    if str(depot_id).isdigit() and isinstance(depot_data, dict):
                        dk = depot_data.get("DecryptionKey") or depot_data.get("decryptionkey")
                        if isinstance(dk, str) and HEX64_RE.match(dk.strip()):
                            out[str(depot_id)] = dk.strip().lower()
            elif isinstance(v, dict):
                self._walk_depots(v, out)

    def donate_new_keys(self) -> int:
        """Send any newly discovered depot keys. Returns count of keys submitted."""
        found = self.extract_keys_from_vdf()
        if not found:
            return 0

        sent = self._load_sent()
        new_keys = {d: k for d, k in found.items() if sent.get(d) != k}
        if not new_keys:
            return 0

        try:
            resp = self._http.post(
                DONATE_ENDPOINT,
                headers={"User-Agent": DONATE_USER_AGENT},
                json={"keys": new_keys},
            )
            if resp.status_code in (200, 201, 204):
                sent.update(new_keys)
                self._save_sent(sent)
                return len(new_keys)
        except Exception:
            pass
        return 0

    def _load_sent(self) -> dict[str, str]:
        if not self.cache_path.exists():
            return {}
        try:
            return json.loads(self.cache_path.read_text(encoding="utf-8"))
        except Exception:
            return {}

    def _save_sent(self, data: dict[str, str]) -> None:
        try:
            self.cache_path.write_text(json.dumps(data), encoding="utf-8")
        except Exception:
            pass
