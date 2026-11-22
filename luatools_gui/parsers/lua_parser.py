"""Lua file parser - parses addappid/setManifestid lines from stplug-in lua files."""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional


@dataclass
class LuaEntry:
    """One depot/app entry declared in a lua file."""
    id: int
    key: Optional[str] = None
    manifest_id: Optional[str] = None
    commented_manifest_id: Optional[str] = None
    comment: Optional[str] = None
    size_on_disk: Optional[int] = None

    @property
    def has_key(self) -> bool:
        return self.key is not None

    @property
    def is_dlc(self) -> bool:
        return not self.has_key and self.id != 0


@dataclass
class LuaContents:
    """Parsed contents of a stplug-in lua file."""
    base_app_id: int
    entries: list[LuaEntry] = field(default_factory=list)
    active_pins: dict[int, str] = field(default_factory=dict)
    commented_pins: dict[int, str] = field(default_factory=dict)
    disabled_entries: list[LuaEntry] = field(default_factory=list)

    @property
    def depot_count(self) -> int:
        return sum(1 for e in self.entries if e.has_key)

    @property
    def dlc_count(self) -> int:
        return sum(1 for e in self.entries if not e.has_key and e.id != self.base_app_id)

    @property
    def has_active_pins(self) -> bool:
        return len(self.active_pins) > 0


class LuaFileParser:
    """Parses stplug-in lua files to extract depot/app declarations."""

    # Regex patterns
    ADD_APPID_RE = re.compile(
        r'addappid\s*\(\s*(\d+)\s*(?:,\s*\d+\s*(?:,\s*"([^"]*)")?)?\s*\)[ \t]*(?:--[ \t]*(.*?)[ \t]*)?$',
        re.IGNORECASE
    )

    SET_MANIFEST_RE = re.compile(
        r'setManifestid\s*\(\s*(\d+)\s*,\s*"(\d+)"\s*(?:,\s*(\d+))?',
        re.IGNORECASE
    )

    COMMENT_TAIL_RE = re.compile(r'\s*\(\d+\)\s*\S*\s*$')

    @staticmethod
    def _clean_comment(raw: Optional[str]) -> Optional[str]:
        if not raw or not raw.strip():
            return None
        s = raw.strip()
        # Reject commented-out code
        if any(x in s.lower() for x in ("setmanifestid", "addappid", "addtoken")):
            return None
        s = LuaFileParser.COMMENT_TAIL_RE.sub("", s).strip()
        return s if s else None

    @classmethod
    def parse(cls, file_path: Path, app_id_from_name: int) -> Optional[LuaContents]:
        """Parse a lua file and return its contents."""
        try:
            text = file_path.read_text(encoding="utf-8", errors="replace")
        except Exception:
            return None

        order: list[int] = []
        key_by_id: dict[int, Optional[str]] = {}
        comment_by_id: dict[int, str] = {}
        manifests: dict[int, str] = {}
        sizes: dict[int, int] = {}
        commented_manifests: dict[int, str] = {}
        disabled_order: list[int] = []
        disabled_key_by_id: dict[int, Optional[str]] = {}

        for raw_line in text.split('\n'):
            line = raw_line.strip()
            if not line:
                continue

            commented = line.startswith("--")
            line_to_match = line[2:].strip() if commented else line

            # Parse setManifestid lines (both commented and uncommented)
            pin = cls.SET_MANIFEST_RE.match(line_to_match)
            if pin:
                try:
                    depot = int(pin.group(1))
                    manifest_id = pin.group(2)
                    if commented:
                        commented_manifests[depot] = manifest_id
                    else:
                        manifests[depot] = manifest_id

                    # Size is third argument
                    if pin.group(3):
                        sz = int(pin.group(3))
                        if sz > 0:
                            sizes[depot] = sz
                except (ValueError, IndexError):
                    pass

            # Parse addappid lines
            m = cls.ADD_APPID_RE.match(line_to_match)
            if not m:
                continue

            try:
                entry_id = int(m.group(1))
            except (ValueError, IndexError):
                continue

            key = m.group(2) if m.group(2) else None
            comment = cls._clean_comment(m.group(3) if m.group(3) else None)

            if comment:
                if entry_id not in comment_by_id or len(comment) > len(comment_by_id[entry_id]):
                    comment_by_id[entry_id] = comment

            if commented:
                if entry_id in disabled_key_by_id:
                    existing = disabled_key_by_id[entry_id]
                    disabled_key_by_id[entry_id] = existing or key
                else:
                    disabled_key_by_id[entry_id] = key
                    disabled_order.append(entry_id)
                continue

            # Merge keys - first key wins
            if entry_id in key_by_id:
                existing = key_by_id[entry_id]
                if existing is None and key is not None:
                    key_by_id[entry_id] = key
            else:
                key_by_id[entry_id] = key
                order.append(entry_id)

        # Build active entries
        entries = []
        for entry_id in order:
            entries.append(LuaEntry(
                id=entry_id,
                key=key_by_id.get(entry_id),
                manifest_id=manifests.get(entry_id),
                commented_manifest_id=commented_manifests.get(entry_id),
                comment=comment_by_id.get(entry_id),
                size_on_disk=sizes.get(entry_id)
            ))

        # Build disabled entries
        disabled_entries = []
        for entry_id in disabled_order:
            if entry_id in key_by_id:
                continue  # Already active
            disabled_entries.append(LuaEntry(
                id=entry_id,
                key=disabled_key_by_id.get(entry_id),
                manifest_id=manifests.get(entry_id),
                commented_manifest_id=commented_manifests.get(entry_id),
                comment=comment_by_id.get(entry_id),
                size_on_disk=sizes.get(entry_id)
            ))

        base_id = entries[0].id if entries else app_id_from_name

        return LuaContents(
            base_app_id=base_id,
            entries=entries,
            active_pins=manifests,
            commented_pins=commented_manifests,
            disabled_entries=disabled_entries
        )

    @classmethod
    def diff(cls, old_lua: Optional[LuaContents], new_lua: LuaContents) -> tuple[list[LuaEntry], list[LuaEntry]]:
        """Compute diff between two lua files."""
        old_ids = {e.id for e in old_lua.entries} if old_lua else set()
        new_ids = {e.id for e in new_lua.entries}

        added = [e for e in new_lua.entries if e.id not in old_ids]
        removed = [e for e in old_lua.entries if e.id not in new_ids] if old_lua else []

        return added, removed


# Export for convenience
parse = LuaFileParser.parse
diff = LuaFileParser.diff