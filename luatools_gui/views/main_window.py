"""Main application window."""

from __future__ import annotations

import threading
from typing import Optional

from PyQt6.QtCore import pyqtSignal
from PyQt6.QtGui import QAction
from PyQt6.QtWidgets import (
    QApplication,
    QHBoxLayout,
    QLabel,
    QMainWindow,
    QMessageBox,
    QPushButton,
    QStackedWidget,
    QVBoxLayout,
    QWidget,
)

from ..models.steam import SteamInstallation
from ..models.vault.lua_vault import LuaVault
from ..services.api_client import HubcapService, LuaToolsApiClient
from ..services.auth_service import AuthService
from ..services.caches import (
    CoverCache,
    HardwareAppIdService,
    SteamAppInfoCache,
    SteamAppListCache,
)
from ..services.cef_injector import CefInjectorService
from ..services.depot_downloader import DepotDownloaderService
from ..services.depot_info import SteamDepotInfo
from ..services.depot_service import DepotService
from ..services.donate_keys import DonateKeysService
from ..services.external_tools import ExternalToolsManager
from ..services.fixes import FixesService
from ..services.http_server import HttpServerService
from ..services.lua_installer import LuaInstaller
from ..services.millennium_service import MillenniumService
from ..services.protocol_service import ProtocolService
from ..services.settings_service import SettingsService
from ..services.slssteam_service import SlssteamService
from ..services.steam_service import SteamService
from .add_view import AddGamesView
from .depots_view import create_depots_view
from .external_tools_view import ExternalToolsView
from .fixes_view import FixesView
from .games_view import GamesView
from .home_view import HomeView
from .settings_view import SettingsView


class MainWindow(QMainWindow):
    """Main window of LuaTools Linux."""

    _rpc_open_fix = pyqtSignal(int)
    _rpc_open_settings = pyqtSignal()
    _rpc_add_app = pyqtSignal(int)

    def __init__(self) -> None:
        super().__init__()

        self.setWindowTitle("LuaTools Linux")
        self.resize(1400, 900)

        # Core services
        self.settings_service = SettingsService()
        self.auth_service = AuthService()
        self.auth_service.initialize()

        self.steam_service = SteamService()
        self.slssteam_service = SlssteamService()
        self.depot_service = DepotService()
        self.millennium_service = MillenniumService()

        # API & Caches
        self.api_client = LuaToolsApiClient(self.auth_service)
        self.hubcap_service = HubcapService()
        self.app_info_cache = SteamAppInfoCache()
        self.applist_cache = SteamAppListCache()
        self.cover_cache = CoverCache()
        self.hardware_service = HardwareAppIdService()

        # Steam-dependent services (initialized after detection)
        self.depot_info = SteamDepotInfo()
        self.depot_downloader = DepotDownloaderService(
            steam_installation=None,
            slssteam_service=self.slssteam_service,
            depot_info=self.depot_info,
        )
        self.lua_vault: Optional[LuaVault] = None
        self.lua_installer: Optional[LuaInstaller] = None
        self.fixes_service: Optional[FixesService] = None
        self.donate_keys: Optional[DonateKeysService] = None
        self.cef_injector: Optional[CefInjectorService] = None
        self.http_server: Optional[HttpServerService] = None
        self.tools_manager = ExternalToolsManager()

        self._rpc_open_fix.connect(self._on_rpc_open_fix)
        self._rpc_open_settings.connect(lambda: self._show_view("settings"))
        self._rpc_add_app.connect(self._on_open_app_in_add)

        self._setup_menu()
        self._setup_ui()
        self._detect_steam()

    def _setup_menu(self) -> None:
        menubar = self.menuBar()

        file_menu = menubar.addMenu("&File")
        exit_action = QAction("E&xit", self)
        exit_action.setShortcut("Ctrl+Q")
        exit_action.triggered.connect(self.close)
        file_menu.addAction(exit_action)

        steam_menu = menubar.addMenu("&Steam")
        detect_action = QAction("&Detect Steam", self)
        detect_action.triggered.connect(self._detect_steam)
        steam_menu.addAction(detect_action)

        restart_action = QAction("&Restart Steam", self)
        restart_action.triggered.connect(self._restart_steam)
        steam_menu.addAction(restart_action)

        view_menu = menubar.addMenu("&View")
        nav_items = [
            ("&Home", "Ctrl+1", "home"),
            ("&Add / Browse", "Ctrl+2", "add"),
            ("&Manage Games", "Ctrl+3", "games"),
            ("&Depots", "Ctrl+4", "depots"),
            ("&Fixes", "Ctrl+5", "fixes"),
            ("External &Tools", "Ctrl+6", "tools"),
            ("&Settings", "Ctrl+7", "settings"),
        ]
        self._nav_actions: dict[str, QAction] = {}
        for label, shortcut, key in nav_items:
            act = QAction(label, self)
            act.setShortcut(shortcut)
            act.triggered.connect(lambda _, k=key: self._show_view(k))
            view_menu.addAction(act)
            self._nav_actions[key] = act

        help_menu = menubar.addMenu("&Help")
        about_action = QAction("&About", self)
        about_action.triggered.connect(self._show_about)
        help_menu.addAction(about_action)

    def _setup_ui(self) -> None:
        central = QWidget()
        self.setCentralWidget(central)
        layout = QVBoxLayout(central)

        # Navigation bar
        nav_bar = QHBoxLayout()
        self._nav_buttons: dict[str, QPushButton] = {}
        for label, key in [
            ("Home", "home"),
            ("Add Games", "add"),
            ("Manage Games", "games"),
            ("Depots", "depots"),
            ("Fixes", "fixes"),
            ("External Tools", "tools"),
            ("Settings", "settings"),
        ]:
            btn = QPushButton(label)
            btn.setCheckable(True)
            btn.clicked.connect(lambda _, k=key: self._show_view(k))
            nav_bar.addWidget(btn)
            self._nav_buttons[key] = btn
        nav_bar.addStretch()
        layout.addLayout(nav_bar)

        self.status_label = QLabel("Detecting Steam...")
        self.status_label.setStyleSheet("padding: 2px 4px; color: #666;")
        layout.addWidget(self.status_label)

        self.stack = QStackedWidget()
        layout.addWidget(self.stack)

        # Views
        self.home_view = HomeView(
            steam_service=self.steam_service,
            slssteam_service=self.slssteam_service,
            millennium_service=self.millennium_service,
            auth_service=self.auth_service,
            settings_service=self.settings_service,
            cover_cache=self.cover_cache,
            applist_cache=self.applist_cache,
            on_navigate=self._show_view,
            on_open_app=self._on_open_app_in_add,
            on_manage_depots=self._on_manage_depots,
        )
        self.add_view = AddGamesView(
            api_client=self.api_client,
            hubcap_service=self.hubcap_service,
            auth_service=self.auth_service,
            settings_service=self.settings_service,
            app_info_cache=self.app_info_cache,
            cover_cache=self.cover_cache,
            hardware_service=self.hardware_service,
            on_game_installed=self._on_game_installed,
        )
        self.games_view = GamesView(
            steam_service=self.steam_service,
            slssteam_service=self.slssteam_service,
            depot_service=self.depot_service,
            applist_cache=self.applist_cache,
            on_manage_depots=self._on_manage_depots,
        )
        self.depots_view = None
        self.fixes_view = None
        self.tools_view = None
        self.settings_view = SettingsView(
            slssteam_service=self.slssteam_service,
            millennium_service=self.millennium_service,
            auth_service=self.auth_service,
            settings_service=self.settings_service,
            api_client=self.api_client,
            hubcap_service=self.hubcap_service,
        )

        self.stack.addWidget(self.home_view)
        self.stack.addWidget(self.add_view)
        self.stack.addWidget(self.games_view)
        self.stack.addWidget(self.settings_view)

        self._show_view("home")

    def _detect_steam(self) -> None:
        self.status_label.setText("Detecting Steam...")
        QApplication.processEvents()
        inst = self.steam_service.detect()
        self._on_steam_detected(inst)

    def _on_steam_detected(self, installation: Optional[SteamInstallation]) -> None:
        if not installation:
            self.status_label.setText(
                "Steam not found. Please install Steam at ~/.local/share/Steam or ~/.steam/steam"
            )
            return

        self.status_label.setText(
            f"Steam found: {installation.root} ({len(installation.libraries)} library folder(s))"
        )

        self.depot_downloader.steam_installation = installation
        self.depot_downloader.depot_info = self.depot_info
        self.lua_vault = LuaVault(installation)
        self.lua_installer = LuaInstaller(installation, self.settings_service, self.lua_vault)
        self.lua_installer.add_installed_listener(self._on_game_installed)

        self.fixes_service = FixesService(
            installation,
            api_client=self.api_client,
            lua_installer=self.lua_installer,
        )
        self.donate_keys = DonateKeysService(installation)
        self.cef_injector = CefInjectorService(
            installation, http_port=self.settings_service.settings.http_server_port
        )
        self.settings_view.cef_injector = self.cef_injector

        if self.settings_service.settings.http_server_enabled and not self.http_server:
            self.http_server = HttpServerService(
                port=self.settings_service.settings.http_server_port,
                lua_installer=self.lua_installer,
                on_open_fix=lambda aid: self._rpc_open_fix.emit(aid),
                on_open_settings=lambda: self._rpc_open_settings.emit(),
                on_add_app=lambda aid: self._rpc_add_app.emit(aid),
            )
            self.http_server.start()

        if self.settings_service.settings.cef_injector_enabled:
            self.cef_injector.start()

        self.add_view.set_lua_installer(self.lua_installer)
        self.games_view.set_installation(installation)
        self.home_view.set_context(
            installation, self.lua_installer, self.cef_injector, self.donate_keys
        )

        if self.depots_view is None:
            self.depots_view = create_depots_view(
                steam_service=self.steam_service,
                slssteam_service=self.slssteam_service,
                depot_downloader=self.depot_downloader,
                depot_info=self.depot_info,
                lua_vault=self.lua_vault,
                millennium_service=self.millennium_service,
                steam_installation=installation,
            )
            self.stack.addWidget(self.depots_view)

        if self.fixes_view is None:
            self.fixes_view = FixesView(
                steam_service=self.steam_service,
                fixes_service=self.fixes_service,
                steam_installation=installation,
                auth_service=self.auth_service,
            )
            self.stack.addWidget(self.fixes_view)

        if self.tools_view is None:
            self.tools_view = ExternalToolsView(
                steam_service=self.steam_service,
                tools_manager=self.tools_manager,
                steam_installation=installation,
            )
            self.stack.addWidget(self.tools_view)

        if self.settings_service.settings.donate_keys:
            threading.Thread(target=self.donate_keys.donate_new_keys, daemon=True).start()

    def _on_game_installed(self, app_id: int) -> None:
        self.home_view.refresh()
        self.games_view._refresh()

    def _on_open_app_in_add(self, app_id: int) -> None:
        self.add_view.open_app(app_id)
        self._show_view("add")

    def _on_manage_depots(self, app_id: int) -> None:
        if self.depots_view:
            self.depots_view.set_game(app_id)
            self._show_view("depots")

    def _on_rpc_open_fix(self, app_id: int) -> None:
        if self.fixes_view:
            self.fixes_view.open_app(app_id)
            self._show_view("fixes")

    def handle_protocol_url(self, url: str) -> None:
        action = ProtocolService.parse(url)
        if not action:
            return
        if action.kind in ("game", "install"):
            self._on_open_app_in_add(action.app_id)
        elif action.kind == "manage":
            self._on_manage_depots(action.app_id)
        elif action.kind == "fix":
            self._on_rpc_open_fix(action.app_id)

    def _show_view(self, view_name: str) -> None:
        mapping = {
            "home": self.home_view,
            "add": self.add_view,
            "games": self.games_view,
            "depots": self.depots_view,
            "fixes": self.fixes_view,
            "tools": self.tools_view,
            "settings": self.settings_view,
        }
        widget = mapping.get(view_name)
        if widget is None:
            QMessageBox.warning(self, "Not Ready", "Steam detection has not completed yet.")
            return
        self.stack.setCurrentWidget(widget)
        for k, btn in self._nav_buttons.items():
            btn.setChecked(k == view_name)
        if view_name == "home":
            self.home_view.refresh()

    def _restart_steam(self) -> None:
        result = self.millennium_service.call("restart_steam")
        if result.get("success"):
            self.status_label.setText("Restarting Steam...")
        else:
            QMessageBox.warning(
                self,
                "Restart Failed",
                f"Could not restart Steam via Millennium: {result.get('error', 'unknown error')}",
            )

    def _show_about(self) -> None:
        QMessageBox.about(
            self,
            "About LuaTools Linux",
            "LuaTools Linux v1.1.0\n\n"
            "Native Linux port of LuaTools for managing Steam manifest/lua configurations.\n\n"
            "Features:\n"
            "• Steam Store search, Featured browse & Manifest source checks\n"
            "• Drag & drop .lua / .manifest / .zip / URL installer\n"
            "• Game & Denuvo Fixes catalog with SHA-256 backup & revert\n"
            "• DepotDownloaderMod & LuaVault variant switcher\n"
            "• Steam appinfo.vdf Launch Options editor\n"
            "• SLSsteam config, Millennium RPC & CEF CDP injector\n"
            "• Discord OAuth / Bot Code auth & Hubcap support",
        )

    def closeEvent(self, event) -> None:
        if self.cef_injector:
            self.cef_injector.stop()
        if self.http_server:
            self.http_server.stop()
        super().closeEvent(event)
