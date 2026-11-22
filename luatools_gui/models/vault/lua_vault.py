"""Lua Vault - content-addressed storage for all lua variants per game."""

from __future__ import annotations

import hashlib
import json
import shutil
import tempfile
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Optional

from ...parsers.lua_parser import LuaContents, LuaEntry, parse as parse_lua
from ..steam import SteamInstallation


@dataclass
class LuaVariant:
    """One stored lua variant for a game."""
    hash: str
    kind: str  # default, build, preset
    build_id: Optional[str] = None
    label: Optional[str] = None
    captured_at: datetime = field(default_factory=datetime.utcnow)
    source: Optional[str] = None
    manifest_ids: dict[str, str] = field(default_factory=dict)  # depot_id -> manifest_id
    depot_count: int = 0
    dlc_count: int = 0

    @property
    def is_pinned(self) -> bool:
        return len(self.manifest_ids) > 0

    @property
    def display_label(self) -> str:
        if self.label:
            return self.label
        if self.build_id:
            return f"Build {self.build_id}"
        if self.kind == "default":
            return "Default"
        return f"Preset ({self.captured_at.strftime('%b %d')})"


@dataclass
class VaultIndex:
    """On-disk index for a game's vault."""
    app_id: int = 0
    variants: list[LuaVariant] = field(default_factory=list)
    edit_base_hash: Optional[str] = None


class LuaVault:
    """Content-addressed storage for lua files per game."""

    KIND_DEFAULT = "default"
    KIND_BUILD = "build"
    KIND_PRESET = "preset"

    def __init__(self, steam_installation: SteamInstallation, vault_root: Optional[Path] = None):
        self.steam_installation = steam_installation
        self.vault_root = vault_root or (Path.home() / ".local/share/luatools/luavault")
        self.vault_root.mkdir(parents=True, exist_ok=True)

    def _app_dir(self, app_id: int) -> Path:
        return self.vault_root / str(app_id)

    def _index_path(self, app_id: int) -> Path:
        return self._app_dir(app_id) / "index.json"

    def _variant_path(self, app_id: int, hash_str: str) -> Path:
        return self._app_dir(app_id) / f"{hash_str}.lua"

    def _live_path(self, app_id: int) -> Optional[Path]:
        stplug_dir = self.steam_installation.stplug_in_path
        if stplug_dir:
            return stplug_dir / f"{app_id}.lua"
        return None

    # ===== Reads =====

    def get_variants(self, app_id: int) -> list[LuaVariant]:
        """Get all stored variants for a game, newest first."""
        index = self._load_index(app_id)
        return sorted(index.variants, key=lambda v: v.captured_at, reverse=True)

    def get_active_hash(self, app_id: int) -> Optional[str]:
        """Get hash of the lua Steam is currently using."""
        path = self._live_path(app_id)
        if path and path.exists():
            return self._hash_file(path)
        return None

    def has_variants(self, app_id: int) -> bool:
        """Check if game has any stored variants."""
        index = self._load_index(app_id)
        return len(index.variants) > 0

    def get_active_variant(self, app_id: int) -> Optional[LuaVariant]:
        """Get the variant Steam is currently using."""
        active_hash = self.get_active_hash(app_id)
        if not active_hash:
            return None
        return next((v for v in self.get_variants(app_id) if v.hash == active_hash), None)

    def read_text(self, app_id: int, hash_str: str) -> str:
        """Read stored variant text."""
        path = self._variant_path(app_id, hash_str)
        if path.exists():
            return path.read_text(encoding="utf-8", errors="replace")
        return ""

    def read_live_text(self, app_id: int) -> str:
        """Read live lua text."""
        path = self._live_path(app_id)
        if path and path.exists():
            return path.read_text(encoding="utf-8", errors="replace")
        return ""

    def get_edit_base(self, app_id: int) -> Optional[str]:
        """Get the edit base hash if live lua is being edited."""
        index = self._load_index(app_id)
        return index.edit_base_hash

    def set_edit_base(self, app_id: int, hash_str: Optional[str]) -> None:
        """Set the edit base hash."""
        index = self._load_index(app_id)
        if index.edit_base_hash != hash_str:
            index.edit_base_hash = hash_str
            self._save_index(app_id, index)

    # ===== Writes =====

    @staticmethod
    def _hash_file(path: Path) -> str:
        """Compute SHA256 of file."""
        h = hashlib.sha256()
        with path.open("rb") as f:
            for chunk in iter(lambda: f.read(8192), b""):
                h.update(chunk)
        return h.hexdigest()

    @staticmethod
    def _hash_text(text: str) -> str:
        """Compute SHA256 of text."""
        return hashlib.sha256(text.encode("utf-8")).hexdigest()

    def _load_index(self, app_id: int) -> VaultIndex:
        """Load game's index.json."""
        path = self._index_path(app_id)
        if path.exists():
            try:
                with path.open("r", encoding="utf-8") as f:
                    data = json.load(f)
                    variants = []
                    for v in data.get("variants", []):
                        v_copy = dict(v)
                        if isinstance(v_copy.get("captured_at"), str):
                            try:
                                v_copy["captured_at"] = datetime.fromisoformat(v_copy["captured_at"])
                            except ValueError:
                                v_copy["captured_at"] = datetime.utcnow()
                        variants.append(LuaVariant(**v_copy))
                    return VaultIndex(
                        app_id=data.get("app_id", app_id),
                        variants=variants,
                        edit_base_hash=data.get("edit_base_hash")
                    )
            except Exception:
                pass
        return VaultIndex(app_id=app_id)

    def _save_index(self, app_id: int, index: VaultIndex) -> None:
        """Save game's index.json atomically."""
        self._app_dir(app_id).mkdir(parents=True, exist_ok=True)
        path = self._index_path(app_id)
        tmp = path.with_suffix(".tmp")

        # Backup existing
        if path.exists():
            try:
                shutil.copy2(path, path.with_suffix(".bak"))
            except Exception:
                pass

        data = {
            "app_id": index.app_id,
            "variants": [
                {
                    "hash": v.hash,
                    "kind": v.kind,
                    "build_id": v.build_id,
                    "label": v.label,
                    "captured_at": v.captured_at.isoformat(),
                    "source": v.source,
                    "manifest_ids": v.manifest_ids,
                    "depot_count": v.depot_count,
                    "dlc_count": v.dlc_count
                }
                for v in index.variants
            ],
            "edit_base_hash": index.edit_base_hash
        }

        with tmp.open("w", encoding="utf-8") as f:
            json.dump(data, f, indent=2)

        tmp.replace(path)

    def _describe_variant(self, app_id: int, lua_path: Path, hash_str: str,
                          kind: str, build_id: Optional[str], source: Optional[str]) -> LuaVariant:
        """Parse lua and create variant metadata."""
        lua = parse_lua(lua_path, app_id)
        pins = {}
        if lua:
            pins = {str(k): v for k, v in lua.active_pins.items()}
            depot_count = lua.depot_count
            dlc_count = lua.dlc_count
        else:
            depot_count = 0
            dlc_count = 0

        return LuaVariant(
            hash=hash_str,
            kind=kind,
            build_id=build_id,
            label=None,
            captured_at=datetime.utcnow(),
            source=source,
            manifest_ids=pins,
            depot_count=depot_count,
            dlc_count=dlc_count
        )

    def capture(self, app_id: int, lua_path: Path, kind: str,
                build_id: Optional[str] = None, source: Optional[str] = None) -> Optional[LuaVariant]:
        """Store a lua file as a variant. Returns existing if identical bytes."""
        if not lua_path.exists():
            return None

        hash_str = self._hash_file(lua_path)

        index = self._load_index(app_id)
        existing = next((v for v in index.variants if v.hash == hash_str), None)

        if existing:
            # Backfill build_id if missing
            changed = existing.build_id is None and build_id is not None
            if changed:
                existing.build_id = build_id
                existing.kind = self.KIND_BUILD
                existing.source = existing.source or source
                self._save_index(app_id, index)
            return existing

        # New variant
        self._app_dir(app_id).mkdir(parents=True, exist_ok=True)
        variant_path = self._variant_path(app_id, hash_str)
        shutil.copy2(lua_path, variant_path)

        variant = self._describe_variant(app_id, lua_path, hash_str, kind, build_id, source)
        index.variants.append(variant)
        self._save_index(app_id, index)
        return variant

    def sync_default_from_live(self, app_id: int) -> bool:
        """Point Default slot at live lua. Returns True if index changed."""
        live_path = self._live_path(app_id)
        if not live_path or not live_path.exists():
            return False

        live_hash = self._hash_file(live_path)
        index = self._load_index(app_id)

        # Check if live is already stored
        live_is_stored = any(v.hash == live_hash for v in index.variants)
        edit_pending = index.edit_base_hash is not None

        if live_is_stored or edit_pending:
            return False

        # Collapse multiple defaults
        defaults = [v for v in index.variants if v.kind == self.KIND_DEFAULT]
        if len(defaults) > 1:
            # Keep the one matching live, else newest
            keep = next((v for v in defaults if v.hash == live_hash),
                        max(defaults, key=lambda v: v.captured_at))
            for v in defaults:
                if v != keep and v.hash != live_hash:
                    index.variants.remove(v)
                    self._variant_path(app_id, v.hash).unlink(missing_ok=True)

        # Add/replace default
        self._app_dir(app_id).mkdir(parents=True, exist_ok=True)
        variant_path = self._variant_path(app_id, live_hash)
        shutil.copy2(live_path, variant_path)

        default_variant = self._describe_variant(app_id, live_path, live_hash,
                                                  self.KIND_DEFAULT, None, None)
        index.variants.append(default_variant)
        self._save_index(app_id, index)
        return True

    def apply(self, app_id: int, variant: LuaVariant) -> bool:
        """Make a stored variant the live lua."""
        live_path = self._live_path(app_id)
        source_path = self._variant_path(app_id, variant.hash)

        if not live_path or not source_path.exists():
            return False

        # Capture outgoing lua first (in case it's an unsaved edit)
        self.sync_default_from_live(app_id)

        try:
            live_path.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source_path, live_path)
            self.set_edit_base(app_id, None)  # Live now matches stored
            return True
        except Exception:
            return False

    def write_live(self, app_id: int, text: str) -> bool:
        """Write text to live lua (editor save)."""
        live_path = self._live_path(app_id)
        if not live_path:
            return False
        try:
            live_path.parent.mkdir(parents=True, exist_ok=True)
            live_path.write_text(text, encoding="utf-8")
            return True
        except Exception:
            return False

    def save_text(self, app_id: int, text: str, label: Optional[str] = None) -> Optional[LuaVariant]:
        """Store edited text as a new preset."""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".lua", delete=False, encoding="utf-8") as f:
            f.write(text)
            tmp_path = Path(f.name)

        try:
            variant = self.capture(app_id, tmp_path, self.KIND_PRESET)
            if variant:
                self.set_edit_base(app_id, None)
                if label:
                    self.rename(app_id, variant.hash, label)
            return variant
        finally:
            tmp_path.unlink(missing_ok=True)

    def update_variant(self, app_id: int, old_hash: str, text: str) -> Optional[LuaVariant]:
        """Overwrite an existing variant with new text, keeping its identity."""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".lua", delete=False, encoding="utf-8") as f:
            f.write(text)
            tmp_path = Path(f.name)

        try:
            new_hash = self._hash_file(tmp_path)
            index = self._load_index(app_id)

            # Find old variant
            try:
                i = next(i for i, v in enumerate(index.variants) if v.hash == old_hash)
            except StopIteration:
                return None

            old = index.variants[i]

            # Check if new hash already exists
            existing = next((v for v in index.variants if v.hash == new_hash), None)
            if existing and existing.hash != old_hash:
                # Already exists - retire old entry
                index.variants.pop(i)
                self._variant_path(app_id, old_hash).unlink(missing_ok=True)
                self._save_index(app_id, index)
                return existing

            # Write new file
            self._app_dir(app_id).mkdir(parents=True, exist_ok=True)
            new_variant_path = self._variant_path(app_id, new_hash)
            shutil.copy2(tmp_path, new_variant_path)

            # Describe from new bytes, keep old identity
            described = self._describe_variant(app_id, tmp_path, new_hash, old.kind,
                                                old.build_id, old.source)
            result = LuaVariant(
                hash=described.hash, kind=described.kind, build_id=described.build_id,
                label=old.label, captured_at=old.captured_at, source=old.source,
                manifest_ids=described.manifest_ids, depot_count=described.depot_count,
                dlc_count=described.dlc_count
            )
            index.variants[i] = result
            index.edit_base_hash = None
            self._save_index(app_id, index)

            if new_hash != old_hash:
                self._variant_path(app_id, old_hash).unlink(missing_ok=True)

            return result
        finally:
            tmp_path.unlink(missing_ok=True)

    def rename(self, app_id: int, hash_str: str, label: Optional[str]) -> None:
        """Set a variant's user-chosen name."""
        index = self._load_index(app_id)
        for v in index.variants:
            if v.hash == hash_str:
                v.label = label.strip() if label and label.strip() else None
                self._save_index(app_id, index)
                break

    def delete(self, app_id: int, hash_str: str) -> bool:
        """Delete a variant (not the active one)."""
        if self.get_active_hash(app_id) == hash_str:
            return False

        index = self._load_index(app_id)
        initial_len = len(index.variants)
        index.variants = [v for v in index.variants if v.hash != hash_str]
        if len(index.variants) < initial_len:
            self._variant_path(app_id, hash_str).unlink(missing_ok=True)
            self._save_index(app_id, index)
            return True
        return False

    def adopt_loose_build_luas(self, app_id: int) -> None:
        """Store loose <appid>_<buildid>.lua files as build variants."""
        stplug_dir = self.steam_installation.stplug_in_path
        if not stplug_dir:
            return

        for lua_file in stplug_dir.glob("*_*.lua"):
            try:
                parts = lua_file.stem.split("_", 1)
                if len(parts) == 2 and parts[0].isdigit() and int(parts[0]) == app_id:
                    build_id = parts[1]
                    self.capture(app_id, lua_file, self.KIND_BUILD, build_id=build_id)
            except Exception:
                continue

    def apply_build_if_nothing_live(self, app_id: int) -> Optional[LuaVariant]:
        """Make a stored build live when no lua exists."""
        if self.get_active_hash(app_id) is not None:
            return None
        builds = [v for v in self.get_variants(app_id) if v.kind == self.KIND_BUILD]
        if builds:
            return self.apply(app_id, builds[0]) and builds[0] or None
        return None

    def apps_with_variants(self) -> list[int]:
        """List app IDs that have stored variants."""
        apps = []
        for d in self.vault_root.iterdir():
            if d.is_dir() and d.name.isdigit():
                app_id = int(d.name)
                if self.has_variants(app_id):
                    apps.append(app_id)
        return sorted(apps)