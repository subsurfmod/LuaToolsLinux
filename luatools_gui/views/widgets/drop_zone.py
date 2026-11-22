"""Reusable DropZone widget for .lua, .manifest, .zip files and Steam/SteamDB URLs."""

from __future__ import annotations

from pathlib import Path
from typing import Optional

from PyQt6.QtCore import Qt, pyqtSignal
from PyQt6.QtGui import QDragEnterEvent, QDropEvent
from PyQt6.QtWidgets import (
    QFileDialog,
    QFrame,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPushButton,
    QVBoxLayout,
)

from ...services.lua_installer import SteamLinkParser


class DropZoneWidget(QFrame):
    """Drag-and-drop target for .lua, .manifest, .zip files or Steam/SteamDB URLs."""

    files_dropped = pyqtSignal(list)  # list[Path]
    app_link_dropped = pyqtSignal(int)  # app_id

    SUPPORTED_EXTS = {".lua", ".manifest", ".zip"}

    def __init__(self, parent=None) -> None:
        super().__init__(parent)
        self.setAcceptDrops(True)
        self.setFrameShape(QFrame.Shape.StyledPanel)
        self.setStyleSheet(
            "DropZoneWidget { border: 2px dashed #6373EE; border-radius: 8px; "
            "background: rgba(99, 115, 238, 0.06); padding: 12px; }"
        )
        self._setup_ui()

    def _setup_ui(self) -> None:
        layout = QVBoxLayout(self)
        layout.setAlignment(Qt.AlignmentFlag.AlignCenter)

        self.label = QLabel(
            "Drop .lua, .manifest, .zip files or Steam / SteamDB links here"
        )
        self.label.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self.label.setStyleSheet("font-weight: bold; color: #6373EE;")
        layout.addWidget(self.label)

        row = QHBoxLayout()
        self.url_input = QLineEdit()
        self.url_input.setPlaceholderText("Paste Steam/SteamDB URL or AppID and press Enter...")
        self.url_input.returnPressed.connect(self._on_url_submit)
        row.addWidget(self.url_input)

        browse_btn = QPushButton("Browse File...")
        browse_btn.clicked.connect(self._browse_files)
        row.addWidget(browse_btn)
        layout.addLayout(row)

    def _on_url_submit(self) -> None:
        text = self.url_input.text().strip()
        if not text:
            return
        aid = int(text) if text.isdigit() else SteamLinkParser.app_id_from(text)
        if aid and aid > 0:
            self.url_input.clear()
            self.app_link_dropped.emit(aid)

    def _browse_files(self) -> None:
        paths, _ = QFileDialog.getOpenFileNames(
            self,
            "Select Lua, Manifest, or Zip Files",
            "",
            "LuaTools Files (*.lua *.manifest *.zip);;All Files (*)",
        )
        if paths:
            self.files_dropped.emit([Path(p) for p in paths])

    def dragEnterEvent(self, event: QDragEnterEvent) -> None:
        mime = event.mimeData()
        if mime.hasUrls() or mime.hasText():
            event.acceptProposedAction()
        else:
            event.ignore()

    def dropEvent(self, event: QDropEvent) -> None:
        mime = event.mimeData()
        if mime.hasUrls():
            local_files: list[Path] = []
            for url in mime.urls():
                if url.isLocalFile():
                    p = Path(url.toLocalFile())
                    if p.suffix.lower() in self.SUPPORTED_EXTS:
                        local_files.append(p)
                else:
                    aid = SteamLinkParser.app_id_from(url.toString())
                    if aid:
                        self.app_link_dropped.emit(aid)
                        event.acceptProposedAction()
                        return
            if local_files:
                self.files_dropped.emit(local_files)
                event.acceptProposedAction()
                return

        if mime.hasText():
            aid = SteamLinkParser.app_id_from(mime.text())
            if aid:
                self.app_link_dropped.emit(aid)
                event.acceptProposedAction()
                return
        event.ignore()
