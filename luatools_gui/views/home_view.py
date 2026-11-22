"""Home / Dashboard view."""

from __future__ import annotations

import threading
from pathlib import Path
from typing import Callable, Optional

from PyQt6.QtCore import Qt, pyqtSignal
from PyQt6.QtGui import QPixmap
from PyQt6.QtWidgets import (
    QFrame,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QMessageBox,
    QPushButton,
    QVBoxLayout,
    QWidget,
)

from ..models.steam import SteamInstallation
from ..services.auth_service import AuthService
from ..services.caches import CoverCache, SteamAppListCache
from ..services.cef_injector import CefInjectorService
from ..services.donate_keys import DonateKeysService
from ..services.lua_installer import LuaInstaller
from ..services.millennium_service import MillenniumService
from ..services.settings_service import SettingsService
from ..services.slssteam_service import SlssteamService
from ..services.steam_service import SteamService
from .widgets.drop_zone import DropZoneWidget


class HomeView(QWidget):
    """Dashboard showing Steam/Auth/Plugin status, recently added games, and DropZone."""

    _cover_ready = pyqtSignal(int, str)
    _donate_done = pyqtSignal(int)

    def __init__(
        self,
        steam_service: SteamService,
        slssteam_service: SlssteamService,
        millennium_service: MillenniumService,
        auth_service: AuthService,
        settings_service: SettingsService,
        cover_cache: CoverCache,
        applist_cache: SteamAppListCache,
        on_navigate: Optional[Callable[[str], None]] = None,
        on_open_app: Optional[Callable[[int], None]] = None,
        on_manage_depots: Optional[Callable[[int], None]] = None,
        parent=None,
    ) -> None:
        super().__init__(parent)
        self.steam_service = steam_service
        self.slssteam_service = slssteam_service
        self.millennium_service = millennium_service
        self.auth_service = auth_service
        self.settings_service = settings_service
        self.cover_cache = cover_cache
        self.applist_cache = applist_cache
        self.on_navigate = on_navigate
        self.on_open_app = on_open_app
        self.on_manage_depots = on_manage_depots

        self.steam_installation: Optional[SteamInstallation] = None
        self.lua_installer: Optional[LuaInstaller] = None
        self.cef_injector: Optional[CefInjectorService] = None
        self.donate_keys: Optional[DonateKeysService] = None

        self._recent_labels: dict[int, QLabel] = {}
        self._cover_ready.connect(self._on_cover_ready)
        self._donate_done.connect(self._on_donate_done)

        self._setup_ui()

    def _setup_ui(self) -> None:
        layout = QVBoxLayout(self)
        layout.setSpacing(12)

        # Header
        header = QHBoxLayout()
        title = QLabel("LuaTools Linux Dashboard")
        title.setStyleSheet("font-size: 20px; font-weight: bold;")
        header.addWidget(title)
        header.addStretch()

        add_btn = QPushButton("Browse & Add Games")
        add_btn.clicked.connect(lambda: self.on_navigate and self.on_navigate("add"))
        header.addWidget(add_btn)

        donate_btn = QPushButton("Donate Keys")
        donate_btn.clicked.connect(self._donate_keys_now)
        header.addWidget(donate_btn)
        layout.addLayout(header)

        # Status Cards Row
        cards_row = QHBoxLayout()

        # Steam Card
        steam_box = QGroupBox("Steam & Library")
        steam_l = QVBoxLayout(steam_box)
        self.steam_status_lbl = QLabel("Detecting Steam...")
        self.lua_count_lbl = QLabel("Configured games: 0")
        steam_l.addWidget(self.steam_status_lbl)
        steam_l.addWidget(self.lua_count_lbl)
        cards_row.addWidget(steam_box)

        # Account Card
        auth_box = QGroupBox("LuaTools Account")
        auth_l = QVBoxLayout(auth_box)
        self.auth_status_lbl = QLabel("Guest (not signed in)")
        auth_btn = QPushButton("Account Settings")
        auth_btn.clicked.connect(lambda: self.on_navigate and self.on_navigate("settings"))
        auth_l.addWidget(self.auth_status_lbl)
        auth_l.addWidget(auth_btn)
        cards_row.addWidget(auth_box)

        # Unlocker & Plugin Card
        plugin_box = QGroupBox("SLSsteam & Plugin")
        plugin_l = QVBoxLayout(plugin_box)
        self.sls_status_lbl = QLabel("SLSsteam: Checking...")
        self.plugin_status_lbl = QLabel("Millennium / CEF: Checking...")
        plugin_l.addWidget(self.sls_status_lbl)
        plugin_l.addWidget(self.plugin_status_lbl)
        cards_row.addWidget(plugin_box)

        layout.addLayout(cards_row)

        # Recently Added Strip
        recent_box = QGroupBox("Recently Added Games")
        self.recent_layout = QHBoxLayout(recent_box)
        self.recent_empty_lbl = QLabel("No games added yet. Drop a .lua/.zip below or browse in Add Games.")
        self.recent_empty_lbl.setStyleSheet("color: #888;")
        self.recent_layout.addWidget(self.recent_empty_lbl)
        layout.addWidget(recent_box)

        # DropZone
        self.drop_zone = DropZoneWidget()
        self.drop_zone.files_dropped.connect(self._handle_dropped_files)
        self.drop_zone.app_link_dropped.connect(self._handle_dropped_app)
        layout.addWidget(self.drop_zone)

        layout.addStretch()

    def set_context(
        self,
        steam_installation: Optional[SteamInstallation],
        lua_installer: Optional[LuaInstaller],
        cef_injector: Optional[CefInjectorService],
        donate_keys: Optional[DonateKeysService],
    ) -> None:
        self.steam_installation = steam_installation
        self.lua_installer = lua_installer
        self.cef_injector = cef_injector
        self.donate_keys = donate_keys
        self.refresh()

    def refresh(self) -> None:
        # Steam
        if self.steam_installation:
            self.steam_status_lbl.setText(
                f"Connected: {self.steam_installation.root}\n"
                f"Libraries: {len(self.steam_installation.libraries)}"
            )
            self.steam_status_lbl.setStyleSheet("color: #2e7d32;")
        else:
            self.steam_status_lbl.setText("Steam not detected")
            self.steam_status_lbl.setStyleSheet("color: #c62828;")

        installed_luas = self.lua_installer.enumerate_installed() if self.lua_installer else []
        self.lua_count_lbl.setText(f"Configured .lua games: {len(installed_luas)}")

        # Auth
        if self.auth_service.is_signed_in:
            self.auth_status_lbl.setText(f"Signed in as: {self.auth_service.display_name}")
            self.auth_status_lbl.setStyleSheet("color: #2e7d32;")
        else:
            self.auth_status_lbl.setText("Guest mode (sign in for API downloads)")
            self.auth_status_lbl.setStyleSheet("color: #666;")

        # SLSsteam & Millennium
        sls_ok = self.slssteam_service.config.exists
        self.sls_status_lbl.setText(
            f"SLSsteam config: {'Found' if sls_ok else 'Not found'}"
            f" (playNotOwned={self.slssteam_service.config.play_not_owned})"
        )
        if self.millennium_service.is_available():
            self.plugin_status_lbl.setText("Millennium RPC: Connected (:38495)")
        elif self.cef_injector and self.cef_injector.is_running:
            self.plugin_status_lbl.setText(f"CEF CDP: {self.cef_injector.last_status}")
        else:
            self.plugin_status_lbl.setText("Millennium / CEF: Inactive")

        # Recently added
        self._refresh_recent(installed_luas[:4])

    def _refresh_recent(self, recent: list[tuple[int, Path]]) -> None:
        while self.recent_layout.count():
            item = self.recent_layout.takeAt(0)
            if item.widget():
                item.widget().deleteLater()
        self._recent_labels.clear()

        if not recent:
            lbl = QLabel("No games added yet. Drop a .lua/.zip below or browse in Add Games.")
            lbl.setStyleSheet("color: #888;")
            self.recent_layout.addWidget(lbl)
            return

        for app_id, _ in recent:
            card = QFrame()
            card.setFrameShape(QFrame.Shape.StyledPanel)
            card.setMaximumWidth(260)
            cl = QVBoxLayout(card)

            img_lbl = QLabel("Loading...")
            img_lbl.setFixedSize(230, 108)
            img_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
            img_lbl.setStyleSheet("background: #1a1a2e; color: #aaa; border-radius: 4px;")
            cl.addWidget(img_lbl)
            self._recent_labels[app_id] = img_lbl

            name = self.applist_cache.get_name(app_id) or f"App {app_id}"
            name_lbl = QLabel(f"<b>{name}</b><br><span style='color:#666'>AppID {app_id}</span>")
            name_lbl.setWordWrap(True)
            cl.addWidget(name_lbl)

            btn = QPushButton("Manage Depots")
            btn.clicked.connect(lambda _, aid=app_id: self.on_manage_depots and self.on_manage_depots(aid))
            cl.addWidget(btn)

            self.recent_layout.addWidget(card)
            threading.Thread(target=self._fetch_cover, args=(app_id,), daemon=True).start()

        self.recent_layout.addStretch()

    def _fetch_cover(self, app_id: int) -> None:
        p = self.cover_cache.ensure(app_id)
        if p:
            self._cover_ready.emit(app_id, str(p))

    def _on_cover_ready(self, app_id: int, path: str) -> None:
        lbl = self._recent_labels.get(app_id)
        if lbl:
            pix = QPixmap(path)
            if not pix.isNull():
                lbl.setPixmap(pix.scaled(230, 108, Qt.AspectRatioMode.KeepAspectRatio, Qt.TransformationMode.SmoothTransformation))

    def _handle_dropped_files(self, paths: list[Path]) -> None:
        if not self.lua_installer:
            QMessageBox.warning(self, "Not Ready", "Steam installation not detected.")
            return
        installed = []
        errors = []
        for p in paths:
            res = self.lua_installer.install_any(p)
            if res.ok:
                installed.append(f"{p.name} (AppID {res.app_id or '?'})")
            else:
                errors.append(f"{p.name}: {res.error}")
        self.refresh()
        if installed:
            QMessageBox.information(self, "Installed", "Installed:\n" + "\n".join(installed))
        if errors:
            QMessageBox.warning(self, "Install Errors", "\n".join(errors))

    def _handle_dropped_app(self, app_id: int) -> None:
        if self.on_open_app:
            self.on_open_app(app_id)

    def _donate_keys_now(self) -> None:
        if not self.donate_keys:
            return
        threading.Thread(
            target=lambda: self._donate_done.emit(self.donate_keys.donate_new_keys()),
            daemon=True,
        ).start()

    def _on_donate_done(self, count: int) -> None:
        QMessageBox.information(
            self,
            "Donate Keys",
            f"Submitted {count} new decryption key(s) from Steam config.vdf."
            if count > 0
            else "No new decryption keys to submit.",
        )
