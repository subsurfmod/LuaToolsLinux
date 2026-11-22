"""Game detail dialog - edit lua file, fake app id, token for a specific game."""

from __future__ import annotations

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import (
    QCheckBox,
    QDialog,
    QDialogButtonBox,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QMessageBox,
    QPushButton,
    QTextEdit,
    QVBoxLayout,
)

from ..services.depot_service import DepotService
from ..services.luaservice import LuaToolsService
from ..services.slssteam_service import SlssteamService
from ..services.steam_service import SteamService


class GameDetailDialog(QDialog):
    """Dialog to edit LuaTools settings for a single game."""

    def __init__(
        self,
        app_id: int,
        game_name: str,
        steam_service: SteamService,
        slssteam_service: SlssteamService,
        depot_service: DepotService,
        luaservice: LuaToolsService | None = None,
        parent=None,
    ) -> None:
        super().__init__(parent)

        self.app_id = app_id
        self.game_name = game_name
        self.steam_service = steam_service
        self.slssteam_service = slssteam_service
        self.depot_service = depot_service
        self.luaservice = luaservice

        self.setWindowTitle(f"Game Details - {game_name} (AppID {app_id})")
        self.setMinimumSize(600, 500)
        self._setup_ui()
        self._load_current_values()

    def _setup_ui(self) -> None:
        layout = QVBoxLayout(self)

        # Title
        title = QLabel(f"<b>{self.game_name}</b> (AppID: {self.app_id})")
        title.setStyleSheet("font-size: 16px; padding: 8px;")
        layout.addWidget(title)

        # --- Lua File Section ---
        lua_group = QGroupBox("LuaTools stplug-in File")
        lua_layout = QVBoxLayout()

        self.lua_edit = QTextEdit()
        self.lua_edit.setPlaceholderText("-- Lua configuration for this game...")
        self.lua_edit.setMinimumHeight(150)
        lua_layout.addWidget(self.lua_edit)

        lua_btn_layout = QHBoxLayout()
        self.lua_write_btn = QPushButton("Save Lua File")
        self.lua_write_btn.clicked.connect(self._save_lua)
        self.lua_delete_btn = QPushButton("Delete Lua File")
        self.lua_delete_btn.clicked.connect(self._delete_lua)
        self.lua_delete_btn.setStyleSheet("color: #cc0000;")

        lua_btn_layout.addWidget(self.lua_write_btn)
        lua_btn_layout.addWidget(self.lua_delete_btn)
        lua_btn_layout.addStretch()
        lua_layout.addLayout(lua_btn_layout)

        lua_group.setLayout(lua_layout)
        layout.addWidget(lua_group)

        # --- FakeAppId Section ---
        fake_group = QGroupBox("FakeAppId (SLSsteam)")
        fake_layout = QHBoxLayout()

        self.fake_edit = QLineEdit()
        self.fake_edit.setPlaceholderText("480")
        self.fake_edit.setMaximumWidth(100)

        self.fake_set_btn = QPushButton("Set")
        self.fake_set_btn.clicked.connect(self._set_fake_app_id)
        self.fake_remove_btn = QPushButton("Remove")
        self.fake_remove_btn.clicked.connect(self._remove_fake_app_id)
        self.fake_remove_btn.setStyleSheet("color: #cc0000;")

        fake_layout.addWidget(QLabel("Depot ID:"))
        fake_layout.addWidget(self.fake_edit)
        fake_layout.addWidget(self.fake_set_btn)
        fake_layout.addWidget(self.fake_remove_btn)
        fake_layout.addStretch()

        fake_group.setLayout(fake_layout)
        layout.addWidget(fake_group)

        # --- Token Section ---
        token_group = QGroupBox("Access Token (SLSsteam)")
        token_layout = QHBoxLayout()

        self.token_edit = QLineEdit()
        self.token_edit.setPlaceholderText("Enter token...")
        self.token_edit.setEchoMode(QLineEdit.EchoMode.Password)

        self.token_set_btn = QPushButton("Set")
        self.token_set_btn.clicked.connect(self._set_token)
        self.token_remove_btn = QPushButton("Remove")
        self.token_remove_btn.clicked.connect(self._remove_token)
        self.token_remove_btn.setStyleSheet("color: #cc0000;")

        token_layout.addWidget(self.token_edit)
        token_layout.addWidget(self.token_set_btn)
        token_layout.addWidget(self.token_remove_btn)

        token_group.setLayout(token_layout)
        layout.addWidget(token_group)

        # --- DepotDownloader Section ---
        depot_group = QGroupBox("Depot Downloader")
        depot_layout = QHBoxLayout()

        self.depot_edit = QLineEdit()
        self.depot_edit.setPlaceholderText("Depot ID (e.g. 480)")

        self.download_btn = QPushButton("Download Depot")
        self.download_btn.clicked.connect(self._download_depot)

        depot_layout.addWidget(QLabel("Depot ID:"))
        depot_layout.addWidget(self.depot_edit)
        depot_layout.addWidget(self.download_btn)
        depot_layout.addStretch()

        depot_group.setLayout(depot_layout)
        layout.addWidget(depot_group)

        # --- Buttons ---
        button_box = QDialogButtonBox(QDialogButtonBox.StandardButton.Close)
        button_box.rejected.connect(self.reject)
        layout.addWidget(button_box)

    def _load_current_values(self) -> None:
        # Lua file
        if self.luaservice and self.luaservice.lua_exists(self.app_id):
            content = self.luaservice.read_lua(self.app_id)
            self.lua_edit.setPlainText(content)
        else:
            self.lua_edit.setPlainText(
                LuaToolsService.default_lua_content(self.app_id) if self.luaservice else ""
            )

        # FakeAppId
        fake_id = self.slssteam_service.get_fake_app_id(self.app_id)
        if fake_id:
            self.fake_edit.setText(str(fake_id))

        # Token
        token = self.slssteam_service.get_token(self.app_id)
        if token:
            self.token_edit.setText(token)

    def _save_lua(self) -> None:
        if not self.luaservice:
            QMessageBox.warning(self, "Error", "LuaTools service not available")
            return

        content = self.lua_edit.toPlainText()
        if self.luaservice.write_lua(self.app_id, content):
            QMessageBox.information(self, "Saved", "Lua file saved successfully.")
        else:
            QMessageBox.critical(self, "Error", "Failed to save lua file.")

    def _delete_lua(self) -> None:
        if not self.luaservice:
            QMessageBox.warning(self, "Error", "LuaTools service not available")
            return

        if self.luaservice.delete_lua(self.app_id):
            self.lua_edit.clear()
            QMessageBox.information(self, "Deleted", "Lua file deleted.")
        else:
            QMessageBox.critical(self, "Error", "Failed to delete lua file.")

    def _set_fake_app_id(self) -> None:
        text = self.fake_edit.text().strip()
        if not text:
            QMessageBox.warning(self, "Error", "Please enter a Depot ID")
            return

        try:
            depot_id = int(text)
        except ValueError:
            QMessageBox.warning(self, "Error", "Invalid Depot ID")
            return

        self.slssteam_service.set_fake_app_id(self.app_id, depot_id)
        if self.slssteam_service.save():
            QMessageBox.information(self, "Saved", f"FakeAppId {depot_id} set for AppID {self.app_id}")
        else:
            QMessageBox.critical(self, "Error", "Failed to save SLSsteam config")

    def _remove_fake_app_id(self) -> None:
        self.slssteam_service.remove_fake_app_id(self.app_id)
        self.fake_edit.clear()
        if self.slssteam_service.save():
            QMessageBox.information(self, "Removed", "FakeAppId removed.")
        else:
            QMessageBox.critical(self, "Error", "Failed to save SLSsteam config")

    def _set_token(self) -> None:
        token = self.token_edit.text().strip()
        if not token:
            QMessageBox.warning(self, "Error", "Please enter a token")
            return

        self.slssteam_service.set_token(self.app_id, token)
        if self.slssteam_service.save():
            QMessageBox.information(self, "Saved", "Token saved.")
        else:
            QMessageBox.critical(self, "Error", "Failed to save SLSsteam config")

    def _remove_token(self) -> None:
        self.slssteam_service.remove_token(self.app_id)
        self.token_edit.clear()
        if self.slssteam_service.save():
            QMessageBox.information(self, "Removed", "Token removed.")
        else:
            QMessageBox.critical(self, "Error", "Failed to save SLSsteam config")

    def _download_depot(self) -> None:
        text = self.depot_edit.text().strip()
        if not text:
            QMessageBox.warning(self, "Error", "Please enter a Depot ID")
            return

        try:
            depot_id = int(text)
        except ValueError:
            QMessageBox.warning(self, "Error", "Invalid Depot ID")
            return

        from pathlib import Path
        download_dir = str(Path.home() / ".local/share/Steam/steamapps/depotcache" / f"depot_{depot_id}")

        success = self.depot_service.download_depot(
            appid=self.app_id,
            depot_id=depot_id,
            download_dir=download_dir,
        )

        if success:
            QMessageBox.information(
                self,
                "Download Started",
                f"Depot {depot_id} download started for AppID {self.app_id}",
            )
        else:
            QMessageBox.critical(self, "Error", self.depot_service.get_state().message)
