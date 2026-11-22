"""External Tools View - Manage SteamAutoCrack, Steamless, DepotDownloaderMod."""

from __future__ import annotations

import threading
from pathlib import Path
from typing import Optional

from PyQt6.QtCore import pyqtSignal
from PyQt6.QtWidgets import (
    QFileDialog,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QMessageBox,
    QProgressBar,
    QPushButton,
    QVBoxLayout,
    QWidget,
)

from ..models.steam import SteamInstallation
from ..services.external_tools import ExternalToolsManager
from ..services.steam_service import SteamService


class ExternalToolsView(QWidget):
    """External Tools view - manage SteamAutoCrack, Steamless, DepotDownloaderMod."""

    _tool_progress = pyqtSignal(str, float, str)
    _tool_finished = pyqtSignal(str, bool, str)

    def __init__(
        self,
        steam_service: SteamService,
        tools_manager: ExternalToolsManager,
        steam_installation: SteamInstallation,
        parent=None,
    ) -> None:
        super().__init__(parent)
        self.steam_service = steam_service
        self.tools_manager = tools_manager
        self.steam_installation = steam_installation
        self.current_app_id: Optional[int] = None

        self._tool_progress.connect(self._on_tool_progress)
        self._tool_finished.connect(self._on_tool_finished)
        self._setup_ui()

    def _setup_ui(self) -> None:
        layout = QVBoxLayout(self)

        header = QHBoxLayout()
        title = QLabel("External Tools")
        title.setStyleSheet("font-weight: bold; font-size: 16px;")
        header.addWidget(title)
        header.addStretch()
        layout.addLayout(header)

        for tool_name, tool in self.tools_manager.TOOLS.items():
            group = QGroupBox(tool.name)
            group_layout = QVBoxLayout(group)

            status_layout = QHBoxLayout()
            status_lbl = QLabel("Not checked")
            status_lbl.setStyleSheet("color: #666;")
            status_layout.addWidget(QLabel("Status:"))
            status_layout.addWidget(status_lbl)
            status_layout.addStretch()

            version_lbl = QLabel("Unknown")
            status_layout.addWidget(QLabel("Path / Info:"))
            status_layout.addWidget(version_lbl)
            status_layout.addStretch()
            group_layout.addLayout(status_layout)

            btn_layout = QHBoxLayout()
            check_btn = QPushButton("Check / Install / Update")
            check_btn.clicked.connect(lambda _, t=tool_name: self._install_tool(t))
            btn_layout.addWidget(check_btn)

            if tool_name == "steamautocrack":
                launch_btn = QPushButton("Launch SteamAutoCrack")
                launch_btn.clicked.connect(self._launch_steamautocrack)
                btn_layout.addWidget(launch_btn)
            elif tool_name == "steamless":
                launch_btn = QPushButton("Launch Steamless")
                launch_btn.clicked.connect(self._launch_steamless)
                btn_layout.addWidget(launch_btn)

            btn_layout.addStretch()
            group_layout.addLayout(btn_layout)

            progress = QProgressBar()
            progress.setVisible(False)
            group_layout.addWidget(progress)

            setattr(self, f"{tool_name}_status", status_lbl)
            setattr(self, f"{tool_name}_version", version_lbl)
            setattr(self, f"{tool_name}_progress", progress)
            setattr(self, f"{tool_name}_check_btn", check_btn)

            layout.addWidget(group)

        self.footer_status = QLabel("Ready")
        self.footer_status.setStyleSheet("padding: 4px; color: #666;")
        layout.addWidget(self.footer_status)
        layout.addStretch()

    def _install_tool(self, tool_name: str) -> None:
        progress: QProgressBar = getattr(self, f"{tool_name}_progress")
        btn: QPushButton = getattr(self, f"{tool_name}_check_btn")
        progress.setVisible(True)
        progress.setValue(0)
        btn.setEnabled(False)
        self.footer_status.setText(f"Updating {tool_name}...")

        def worker():
            self.tools_manager.progress_callback = lambda t, pct, msg: self._tool_progress.emit(t, pct, msg)
            exe = self.tools_manager.get_tool_path(tool_name)
            if exe and exe.exists():
                self._tool_finished.emit(tool_name, True, str(exe))
            else:
                self._tool_finished.emit(tool_name, False, "Failed to download/find executable")

        threading.Thread(target=worker, daemon=True).start()

    def _on_tool_progress(self, tool_name: str, pct: float, msg: str) -> None:
        progress: Optional[QProgressBar] = getattr(self, f"{tool_name}_progress", None)
        if progress:
            progress.setValue(int(pct * 100))
        self.footer_status.setText(msg)

    def _on_tool_finished(self, tool_name: str, ok: bool, info: str) -> None:
        progress: QProgressBar = getattr(self, f"{tool_name}_progress")
        btn: QPushButton = getattr(self, f"{tool_name}_check_btn")
        status: QLabel = getattr(self, f"{tool_name}_status")
        version: QLabel = getattr(self, f"{tool_name}_version")
        progress.setVisible(False)
        btn.setEnabled(True)
        if ok:
            status.setText("Installed")
            status.setStyleSheet("color: #00aa00;")
            version.setText(info)
            self.footer_status.setText(f"{tool_name} ready")
        else:
            status.setText("Not installed")
            status.setStyleSheet("color: #cc0000;")
            version.setText(info)
            self.footer_status.setText(f"{tool_name}: {info}")

    def _launch_steamautocrack(self) -> None:
        folder = QFileDialog.getExistingDirectory(self, "Select Game Folder for SteamAutoCrack")
        if folder:
            if not self.tools_manager.launch_steamautocrack(self.current_app_id or 0, folder):
                QMessageBox.critical(self, "Error", "Failed to launch SteamAutoCrack")

    def _launch_steamless(self) -> None:
        file_path, _ = QFileDialog.getOpenFileName(
            self, "Select file to unpack", "", "Executables (*.exe);;All files (*)"
        )
        if file_path:
            output_dir = str(Path(file_path).parent / (Path(file_path).stem + "_unpacked"))
            if self.tools_manager.launch_steamless(file_path, output_dir):
                QMessageBox.information(self, "Steamless", f"Launched Steamless on {file_path}")
            else:
                QMessageBox.critical(self, "Error", "Failed to launch Steamless")

    def set_current_game(self, app_id: int) -> None:
        self.current_app_id = app_id
