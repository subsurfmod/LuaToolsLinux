"""Launch Options Service - reads and edits Steam appcache/appinfo.vdf launch entries."""

from __future__ import annotations

import os
import shutil
import struct
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional

from ..models.steam import SteamInstallation

MAGIC_27 = 0x07564427
MAGIC_28 = 0x07564428
MAGIC_29 = 0x07564429

TYPE_TABLE = 0x00
TYPE_STRING = 0x01
TYPE_INT32 = 0x02
TYPE_FLOAT = 0x03
TYPE_POINTER = 0x04
TYPE_WSTRING = 0x05
TYPE_COLOR = 0x06
TYPE_UINT64 = 0x07
TYPE_END = 0x08
TYPE_INT64 = 0x0A
TYPE_END_ALT = 0x0B


class AppInfoFormatError(Exception):
    """Unsupported appinfo.vdf version."""


@dataclass
class VdfProperty:
    name: str
    vtype: int
    value: Any


class VdfTable:
    """Ordered list of binary VDF properties (preserves duplicate keys & order)."""

    def __init__(self) -> None:
        self.items: list[VdfProperty] = []

    def find(self, name: str) -> Optional[VdfProperty]:
        low = name.lower()
        for p in self.items:
            if p.name.lower() == low:
                return p
        return None

    def get_table(self, name: str) -> Optional[VdfTable]:
        p = self.find(name)
        return p.value if p and isinstance(p.value, VdfTable) else None

    def get_text(self, name: str) -> str:
        p = self.find(name)
        if not p:
            return ""
        if isinstance(p.value, (bytes, bytearray)):
            return p.value.decode("utf-8", errors="replace")
        return str(p.value)

    def ensure_table(self, name: str) -> VdfTable:
        existing = self.get_table(name)
        if existing is not None:
            return existing
        t = VdfTable()
        self.items.append(VdfProperty(name, TYPE_TABLE, t))
        return t

    def set_text(self, name: str, value: Optional[str]) -> None:
        if not value:
            self.remove(name)
            return
        raw = value.encode("utf-8")
        existing = self.find(name)
        if existing:
            existing.value = raw
        else:
            self.items.append(VdfProperty(name, TYPE_STRING, raw))

    def remove(self, name: str) -> None:
        low = name.lower()
        self.items = [p for p in self.items if p.name.lower() != low]


@dataclass
class LaunchEntry:
    index: str
    executable: str = ""
    arguments: str = ""
    workingdir: str = ""
    description: str = ""
    launch_type: str = "default"
    oslist: str = ""
    osarch: str = ""


def _read_cstring_bytes(f) -> bytes:
    buf = bytearray()
    while True:
        b = f.read(1)
        if not b or b == b"\x00":
            break
        buf.extend(b)
    return bytes(buf)


def _read_vdf(f, strings: list[str], indexed_keys: bool) -> VdfTable:
    table = VdfTable()
    while True:
        marker_b = f.read(1)
        if not marker_b:
            return table
        marker = marker_b[0]
        if marker in (TYPE_END, TYPE_END_ALT):
            return table

        if indexed_keys:
            idx = struct.unpack("<I", f.read(4))[0]
            name = strings[idx]
        else:
            name = _read_cstring_bytes(f).decode("utf-8", errors="replace")

        if marker == TYPE_TABLE:
            val = _read_vdf(f, strings, indexed_keys)
        elif marker == TYPE_STRING:
            val = _read_cstring_bytes(f)
        elif marker in (TYPE_INT32, TYPE_POINTER, TYPE_COLOR):
            val = struct.unpack("<i", f.read(4))[0]
        elif marker == TYPE_UINT64:
            val = struct.unpack("<Q", f.read(8))[0]
        elif marker == TYPE_INT64:
            val = struct.unpack("<q", f.read(8))[0]
        elif marker == TYPE_FLOAT:
            val = struct.unpack("<f", f.read(4))[0]
        else:
            raise AppInfoFormatError(f"Unsupported VDF type 0x{marker:02X}")

        table.items.append(VdfProperty(name, marker, val))


def _write_vdf(buf: bytearray, table: VdfTable, intern_fn, indexed_keys: bool) -> None:
    for p in table.items:
        buf.append(p.vtype)
        if indexed_keys:
            buf.extend(struct.pack("<I", intern_fn(p.name)))
        else:
            buf.extend(p.name.encode("utf-8") + b"\x00")

        if p.vtype == TYPE_TABLE:
            _write_vdf(buf, p.value, intern_fn, indexed_keys)
        elif p.vtype == TYPE_STRING:
            buf.extend(bytes(p.value) + b"\x00")
        elif p.vtype in (TYPE_INT32, TYPE_POINTER, TYPE_COLOR):
            buf.extend(struct.pack("<i", int(p.value)))
        elif p.vtype == TYPE_UINT64:
            buf.extend(struct.pack("<Q", int(p.value)))
        elif p.vtype == TYPE_INT64:
            buf.extend(struct.pack("<q", int(p.value)))
        elif p.vtype == TYPE_FLOAT:
            buf.extend(struct.pack("<f", float(p.value)))
    buf.append(TYPE_END)


class AppInfoFile:
    """Streaming reader/writer for Steam's appcache/appinfo.vdf."""

    def __init__(self, path: Path) -> None:
        self.path = path
        self.magic: int = 0
        self.universe: int = 0
        self.strings: list[str] = []
        self._string_bytes: list[bytes] = []
        self._index: dict[int, tuple[int, int]] = {}  # app_id -> (offset, size)
        self._apps_start: int = 0
        self._parse_header_and_index()

    @property
    def indexed_keys(self) -> bool:
        return self.magic >= MAGIC_29

    @property
    def meta_length(self) -> int:
        return 60 if self.magic >= MAGIC_28 else 40

    def _parse_header_and_index(self) -> None:
        with self.path.open("rb") as f:
            self.magic, self.universe = struct.unpack("<II", f.read(8))
            if self.magic not in (MAGIC_27, MAGIC_28, MAGIC_29):
                raise AppInfoFormatError(f"Unknown appinfo version 0x{self.magic:08X}")

            if self.indexed_keys:
                table_offset = struct.unpack("<q", f.read(8))[0]
                resume = f.tell()
                f.seek(table_offset)
                count = struct.unpack("<I", f.read(4))[0]
                for _ in range(count):
                    raw = _read_cstring_bytes(f)
                    self._string_bytes.append(raw)
                    self.strings.append(raw.decode("utf-8", errors="replace"))
                f.seek(resume)

            self._apps_start = f.tell()
            while True:
                offset = f.tell()
                hdr = f.read(8)
                if len(hdr) < 8:
                    break
                app_id, size = struct.unpack("<Ii", hdr)
                if app_id == 0:
                    break
                self._index[app_id] = (offset, size)
                f.seek(offset + 8 + size)

    def read_app(self, app_id: int) -> Optional[VdfTable]:
        if app_id not in self._index:
            return None
        offset, _ = self._index[app_id]
        with self.path.open("rb") as f:
            f.seek(offset + 8 + self.meta_length)
            return _read_vdf(f, self.strings, self.indexed_keys)

    def save_with_edits(self, dest: Path, edits: dict[int, VdfTable]) -> None:
        import hashlib

        strings_bytes = list(self._string_bytes)
        lookup = {s: i for i, s in enumerate(self.strings)}

        def intern(key: str) -> int:
            if key in lookup:
                return lookup[key]
            idx = len(strings_bytes)
            lookup[key] = idx
            strings_bytes.append(key.encode("utf-8"))
            return idx

        with self.path.open("rb") as src, dest.open("wb") as out:
            out.write(struct.pack("<II", self.magic, self.universe))
            table_ptr_pos = out.tell()
            if self.indexed_keys:
                out.write(struct.pack("<q", 0))

            for app_id, (offset, size) in self._index.items():
                if app_id in edits:
                    src.seek(offset + 8)
                    meta = bytearray(src.read(self.meta_length))
                    blob = bytearray()
                    _write_vdf(blob, edits[app_id], intern, self.indexed_keys)
                    if len(meta) >= 60:
                        sha_bin = hashlib.sha1(blob).digest()
                        meta[40:60] = sha_bin
                    new_size = len(meta) + len(blob)
                    out.write(struct.pack("<Ii", app_id, new_size))
                    out.write(meta)
                    out.write(blob)
                else:
                    src.seek(offset)
                    out.write(src.read(8 + size))

            out.write(struct.pack("<I", 0))

            if self.indexed_keys:
                table_pos = out.tell()
                out.write(struct.pack("<I", len(strings_bytes)))
                for raw in strings_bytes:
                    out.write(raw + b"\x00")
                out.seek(table_ptr_pos)
                out.write(struct.pack("<q", table_pos))


class LaunchOptionsService:
    """Reads and writes per-game Steam launch options in appcache/appinfo.vdf."""

    def __init__(self, steam_installation: SteamInstallation) -> None:
        self.steam_installation = steam_installation

    @property
    def appinfo_path(self) -> Path:
        return self.steam_installation.root / "appcache" / "appinfo.vdf"

    @property
    def backup_path(self) -> Path:
        return self.steam_installation.root / "appcache" / "appinfo.vdf.luatools.bak"

    def get_launch_entries(self, app_id: int) -> list[LaunchEntry]:
        if not self.appinfo_path.exists():
            return []
        af = AppInfoFile(self.appinfo_path)
        root = af.read_app(app_id)
        if not root:
            return []
        body = root.get_table("appinfo") or root
        config = body.get_table("config")
        if not config:
            return []
        launch = config.get_table("launch")
        if not launch:
            return []

        entries = []
        for prop in launch.items:
            if isinstance(prop.value, VdfTable):
                t = prop.value
                cfg = t.get_table("config")
                entries.append(
                    LaunchEntry(
                        index=prop.name,
                        executable=t.get_text("executable"),
                        arguments=t.get_text("arguments"),
                        workingdir=t.get_text("workingdir"),
                        description=t.get_text("description"),
                        launch_type=t.get_text("type") or "default",
                        oslist=cfg.get_text("oslist") if cfg else "",
                        osarch=cfg.get_text("osarch") if cfg else "",
                    )
                )
        return entries

    def save_launch_entries(self, app_id: int, entries: list[LaunchEntry]) -> tuple[bool, str]:
        if not self.appinfo_path.exists():
            return False, "appinfo.vdf not found."
        try:
            af = AppInfoFile(self.appinfo_path)
            root = af.read_app(app_id)
            if not root:
                return False, f"AppID {app_id} not found in appinfo.vdf."

            body = root.get_table("appinfo") or root
            config = body.ensure_table("config")
            launch = config.ensure_table("launch")
            launch.items.clear()

            for idx, entry in enumerate(entries):
                t = VdfTable()
                t.set_text("executable", entry.executable)
                t.set_text("arguments", entry.arguments)
                t.set_text("workingdir", entry.workingdir)
                t.set_text("description", entry.description)
                t.set_text("type", entry.launch_type or "default")
                if entry.oslist or entry.osarch:
                    cfg = t.ensure_table("config")
                    cfg.set_text("oslist", entry.oslist)
                    cfg.set_text("osarch", entry.osarch)
                launch.items.append(VdfProperty(str(idx), TYPE_TABLE, t))

            if not self.backup_path.exists():
                shutil.copy2(self.appinfo_path, self.backup_path)

            tmp = self.appinfo_path.with_suffix(".vdf.tmp")
            af.save_with_edits(tmp, {app_id: root})
            os.replace(tmp, self.appinfo_path)
            return True, "Launch options saved to appinfo.vdf."
        except Exception as exc:
            return False, str(exc)

    def restore_backup(self) -> tuple[bool, str]:
        if not self.backup_path.exists():
            return False, "No backup found."
        try:
            shutil.copy2(self.backup_path, self.appinfo_path)
            return True, "Restored appinfo.vdf from backup."
        except Exception as exc:
            return False, str(exc)
