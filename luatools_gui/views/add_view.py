"""Add / Browse Games View - Steam store search, featured games, manifest sources, DLC gen."""

from __future__ import annotations

import tempfile
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
    QLineEdit,
    QListWidget,
    QListWidgetItem,
    QMessageBox,
    QProgressBar,
    QPushButton,
    QSplitter,
    QVBoxLayout,
    QWidget,
)

from ..services.api_client import DlcInfo, LuaToolsApiClient, HubcapService, SteamFeaturedItem, SteamSearchItem
from ..services.auth_service import AuthService
from ..services.caches import CoverCache, GameDetails, HardwareAppIdService, SteamAppInfoCache
from ..services.lua_installer import LuaInstaller, SteamLinkParser
from ..services.settings_service import SettingsService
from .widgets.drop_zone import DropZoneWidget


class AddGamesView(QWidget):
    """Search Steam Store, browse featured games, check manifest sources, and install."""

    _search_done = pyqtSignal(list)
    _featured_done = pyqtSignal(list, list)
    _details_done = pyqtSignal(int, object, str)
    _sources_done = pyqtSignal(int, dict, object)
    _install_done = pyqtSignal(bool, str, int)
    _progress_update = pyqtSignal(float, str)

    def __init__(
        self,
        api_client: LuaToolsApiClient,
        hubcap_service: HubcapService,
        auth_service: AuthService,
        settings_service: SettingsService,
        app_info_cache: SteamAppInfoCache,
        cover_cache: CoverCache,
        hardware_service: HardwareAppIdService,
        on_game_installed: Optional[Callable[[int], None]] = None,
        parent=None,
    ) -> None:
        super().__init__(parent)
        self.api_client = api_client
        self.hubcap_service = hubcap_service
        self.auth_service = auth_service
        self.settings_service = settings_service
        self.app_info_cache = app_info_cache
        self.cover_cache = cover_cache
        self.hardware_service = hardware_service
        self.on_game_installed = on_game_installed
        self.lua_installer: Optional[LuaInstaller] = None

        self._selected_app_id: Optional[int] = None
        self._selected_details: Optional[GameDetails] = None
        self._dlc_info: Optional[DlcInfo] = None

        self._search_done.connect(self._on_search_done)
        self._featured_done.connect(self._on_featured_done)
        self._details_done.connect(self._on_details_done)
        self._sources_done.connect(self._on_sources_done)
        self._install_done.connect(self._on_install_done)
        self._progress_update.connect(self._on_progress_update)

        self._setup_ui()
        self._load_featured()

    def set_lua_installer(self, installer: Optional[LuaInstaller]) -> None:
        self.lua_installer = installer

    def _setup_ui(self) -> None:
        layout = QVBoxLayout(self)

        # Search bar
        search_row = QHBoxLayout()
        self.search_edit = QLineEdit()
        self.search_edit.setPlaceholderText("Search Steam Store, or paste AppID / Steam / SteamDB URL...")
        self.search_edit.returnPressed.connect(self._on_search)
        search_row.addWidget(self.search_edit)

        search_btn = QPushButton("Search")
        search_btn.clicked.connect(self._on_search)
        search_row.addWidget(search_btn)
        layout.addLayout(search_row)

        # Splitter: Left (Results + Featured) | Right (Game Detail + Sources)
        splitter = QSplitter(Qt.Orientation.Horizontal)
        layout.addWidget(splitter, stretch=1)

        left_w = QWidget()
        left_l = QVBoxLayout(left_w)
        left_l.setContentsMargins(0, 0, 0, 0)

        self.results_label = QLabel("Featured: Top Sellers & New Releases")
        self.results_label.setStyleSheet("font-weight: bold;")
        left_l.addWidget(self.results_label)

        self.results_list = QListWidget()
        self.results_list.itemClicked.connect(self._on_item_clicked)
        left_l.addWidget(self.results_list)
        splitter.addWidget(left_w)

        # Right panel
        right_w = QWidget()
        right_l = QVBoxLayout(right_w)
        right_l.setContentsMargins(8, 0, 0, 0)

        self.cover_lbl = QLabel("Select a game to inspect")
        self.cover_lbl.setFixedHeight(160)
        self.cover_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self.cover_lbl.setStyleSheet("background: #141424; color: #aaa; border-radius: 6px;")
        right_l.addWidget(self.cover_lbl)

        self.detail_title = QLabel("No game selected")
        self.detail_title.setStyleSheet("font-size: 16px; font-weight: bold;")
        self.detail_title.setWordWrap(True)
        right_l.addWidget(self.detail_title)

        self.detail_meta = QLabel("")
        self.detail_meta.setStyleSheet("color: #666;")
        self.detail_meta.setWordWrap(True)
        right_l.addWidget(self.detail_meta)

        sources_box = QGroupBox("Available Manifest Sources")
        sources_l = QVBoxLayout(sources_box)
        self.sources_list = QListWidget()
        self.sources_list.setMaximumHeight(160)
        sources_l.addWidget(self.sources_list)

        btn_row = QHBoxLayout()
        self.fast_fetch_btn = QPushButton("FastFetch (Auto Install)")
        self.fast_fetch_btn.setEnabled(False)
        self.fast_fetch_btn.clicked.connect(self._fast_fetch)
        btn_row.addWidget(self.fast_fetch_btn)

        self.dl_source_btn = QPushButton("Install Selected Source")
        self.dl_source_btn.setEnabled(False)
        self.dl_source_btn.clicked.connect(self._download_selected_source)
        btn_row.addWidget(self.dl_source_btn)

        self.gen_dlc_btn = QPushButton("Generate DLC Lua")
        self.gen_dlc_btn.setVisible(False)
        self.gen_dlc_btn.clicked.connect(self._generate_dlc)
        btn_row.addWidget(self.gen_dlc_btn)

        sources_l.addLayout(btn_row)
        right_l.addWidget(sources_box)

        self.progress_bar = QProgressBar()
        self.progress_bar.setVisible(False)
        right_l.addWidget(self.progress_bar)

        self.status_lbl = QLabel("Ready")
        self.status_lbl.setStyleSheet("color: #666;")
        right_l.addWidget(self.status_lbl)

        right_l.addStretch()
        splitter.addWidget(right_w)
        splitter.setSizes([420, 620])

        # DropZone
        self.drop_zone = DropZoneWidget()
        self.drop_zone.app_link_dropped.connect(self.open_app)
        self.drop_zone.files_dropped.connect(self._on_files_dropped)
        layout.addWidget(self.drop_zone)

    def _load_featured(self) -> None:
        def worker():
            top, new = self.api_client.get_featured()
            top = [x for x in top if not self.hardware_service.is_hardware(x.app_id)]
            new = [x for x in new if not self.hardware_service.is_hardware(x.app_id)]
            self._featured_done.emit(top, new)

        threading.Thread(target=worker, daemon=True).start()

    def _on_featured_done(self, top: list[SteamFeaturedItem], new: list[SteamFeaturedItem]) -> None:
        if self.search_edit.text().strip():
            return
        self.results_list.clear()
        for item in top:
            li = QListWidgetItem(f"[Top Seller] {item.name} ({item.app_id})")
            li.setData(Qt.ItemDataRole.UserRole, item.app_id)
            self.results_list.addItem(li)
        for item in new:
            li = QListWidgetItem(f"[New Release] {item.name} ({item.app_id})")
            li.setData(Qt.ItemDataRole.UserRole, item.app_id)
            self.results_list.addItem(li)

    def _on_search(self) -> None:
        q = self.search_edit.text().strip()
        if not q:
            self.results_label.setText("Featured: Top Sellers & New Releases")
            self._load_featured()
            return

        if q.isdigit():
            self.open_app(int(q))
            return
        aid = SteamLinkParser.app_id_from(q)
        if aid:
            self.open_app(aid)
            return

        self.results_label.setText(f"Searching Steam Store for '{q}'...")
        threading.Thread(
            target=lambda: self._search_done.emit(self.api_client.search_store(q)),
            daemon=True,
        ).start()

    def _on_search_done(self, items: list[SteamSearchItem]) -> None:
        self.results_list.clear()
        self.results_label.setText(f"Search Results ({len(items)})")
        for item in items:
            if self.hardware_service.is_hardware(item.app_id):
                continue
            li = QListWidgetItem(f"{item.name} (AppID {item.app_id})")
            li.setData(Qt.ItemDataRole.UserRole, item.app_id)
            self.results_list.addItem(li)

    def _on_item_clicked(self, item: QListWidgetItem) -> None:
        aid = item.data(Qt.ItemDataRole.UserRole)
        if aid:
            self.open_app(int(aid))

    def open_app(self, app_id: int) -> None:
        self._selected_app_id = app_id
        self._dlc_info = None
        self.detail_title.setText(f"Loading AppID {app_id}...")
        self.detail_meta.setText("")
        self.sources_list.clear()
        self.fast_fetch_btn.setEnabled(False)
        self.dl_source_btn.setEnabled(False)
        self.gen_dlc_btn.setVisible(False)
        self.status_lbl.setText("Fetching game details & checking manifest sources...")

        def worker():
            details = self.app_info_cache.get(app_id)
            cover_path = self.cover_cache.ensure(app_id, details.header_image if details else None)
            self._details_done.emit(app_id, details, str(cover_path) if cover_path else "")

            sources = self.api_client.check_sources(app_id)
            hk = self.settings_service.settings.hubcap_api_key.strip()
            if self.hubcap_service.is_valid_key_format(hk):
                has_hc = self.hubcap_service.check_status(hk, app_id)
                sources["hubcap"] = "available" if has_hc else "unavailable"

            dlc_info = None
            if details and details.is_dlc and details.base_app_id and self.auth_service.is_signed_in:
                try:
                    dlc_info = self.api_client.get_dlc_info(app_id, int(details.base_app_id))
                except Exception:
                    pass

            self._sources_done.emit(app_id, sources, dlc_info)

        threading.Thread(target=worker, daemon=True).start()

    def _on_details_done(self, app_id: int, details: Optional[GameDetails], cover_path: str) -> None:
        if app_id != self._selected_app_id:
            return
        self._selected_details = details
        if details:
            self.detail_title.setText(f"{details.name} (AppID {app_id})")
            meta = [details.app_type.upper() or "GAME"]
            if details.release_date:
                meta.append(details.release_date)
            if details.genres:
                meta.append(", ".join(details.genres[:3]))
            self.detail_meta.setText(" · ".join(meta))
        else:
            self.detail_title.setText(f"AppID {app_id}")

        if cover_path:
            pix = QPixmap(cover_path)
            if not pix.isNull():
                self.cover_lbl.setPixmap(
                    pix.scaled(360, 160, Qt.AspectRatioMode.KeepAspectRatio, Qt.TransformationMode.SmoothTransformation)
                )

    def _on_sources_done(self, app_id: int, sources: dict, dlc_info: Optional[DlcInfo]) -> None:
        if app_id != self._selected_app_id:
            return
        self.sources_list.clear()
        available_count = 0
        for name, status in sources.items():
            is_avail = str(status).lower() == "available"
            if is_avail:
                available_count += 1
            li = QListWidgetItem(f"{name}: {status}")
            li.setData(Qt.ItemDataRole.UserRole, (name, is_avail))
            self.sources_list.addItem(li)

        self.fast_fetch_btn.setEnabled(available_count > 0)
        self.dl_source_btn.setEnabled(available_count > 0)

        self._dlc_info = dlc_info
        if dlc_info and dlc_info.missing_count > 0:
            self.gen_dlc_btn.setVisible(True)
            self.gen_dlc_btn.setText(f"Generate DLC Lua ({dlc_info.missing_count} missing)")
        else:
            self.gen_dlc_btn.setVisible(False)

        self.status_lbl.setText(f"Found {available_count} available source(s) for AppID {app_id}")

    def _fast_fetch(self) -> None:
        for i in range(self.sources_list.count()):
            item = self.sources_list.item(i)
            name, is_avail = item.data(Qt.ItemDataRole.UserRole)
            if is_avail:
                self._download_from_source(name)
                return
        QMessageBox.information(self, "No Sources", "No available manifest sources for this game.")

    def _download_selected_source(self) -> None:
        item = self.sources_list.currentItem()
        if not item:
            self._fast_fetch()
            return
        name, is_avail = item.data(Qt.ItemDataRole.UserRole)
        if not is_avail:
            QMessageBox.warning(self, "Unavailable", f"Source '{name}' is not available.")
            return
        self._download_from_source(name)

    def _download_from_source(self, source: str) -> None:
        if not self._selected_app_id or not self.lua_installer:
            return
        app_id = self._selected_app_id
        if source != "hubcap" and not self.auth_service.is_signed_in:
            QMessageBox.warning(self, "Sign In Required", "Please sign in with Discord in Settings first.")
            return

        if self.lua_installer.read_installed_lua(app_id) is not None:
            ans = QMessageBox.question(
                self,
                "Overwrite Existing Lua?",
                f"AppID {app_id} already has a .lua file in stplug-in.\nOverwrite it from '{source}'?",
                QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            )
            if ans != QMessageBox.StandardButton.Yes:
                return

        self.progress_bar.setVisible(True)
        self.progress_bar.setValue(0)
        self.status_lbl.setText(f"Downloading from {source}...")
        game_name = self._selected_details.name if self._selected_details else None

        def worker():
            try:
                with tempfile.TemporaryDirectory() as tmp:
                    tmp_dir = Path(tmp)
                    if source == "hubcap":
                        dl = self.hubcap_service.download_manifest(
                            app_id,
                            self.settings_service.settings.hubcap_api_key.strip(),
                            tmp_dir,
                            progress_cb=lambda p, m: self._progress_update.emit(p, m),
                        )
                    else:
                        dl = self.api_client.download_manifest(
                            app_id,
                            source,
                            game_name,
                            tmp_dir,
                            progress_cb=lambda p, m: self._progress_update.emit(p, m),
                        )
                    res = self.lua_installer.install_any(dl, expected_app_id=app_id, source=source)
                    if res.ok:
                        self._install_done.emit(True, f"Installed {dl.name} ({res.manifest_count} manifest(s))", app_id)
                    else:
                        self._install_done.emit(False, res.error or "Install failed", app_id)
            except Exception as exc:
                self._install_done.emit(False, str(exc), app_id)

        threading.Thread(target=worker, daemon=True).start()

    def _generate_dlc(self) -> None:
        if not self._selected_app_id or not self._selected_details or not self._selected_details.base_app_id:
            return
        if not self.auth_service.is_signed_in or not self.lua_installer:
            QMessageBox.warning(self, "Sign In Required", "Please sign in with Discord in Settings first.")
            return
        app_id = self._selected_app_id
        base_id = int(self._selected_details.base_app_id)
        name = self._selected_details.name

        self.progress_bar.setVisible(True)
        self.status_lbl.setText("Generating DLC lua...")

        def worker():
            try:
                with tempfile.TemporaryDirectory() as tmp:
                    dl = self.api_client.generate_dlc(app_id, base_id, name, Path(tmp))
                    res = self.lua_installer.install_lua_file(dl, app_id, source="dlc-gen")
                    if res.ok:
                        self._install_done.emit(True, f"Generated and installed DLC lua for {app_id}", app_id)
                    else:
                        self._install_done.emit(False, res.error or "Failed", app_id)
            except Exception as exc:
                self._install_done.emit(False, str(exc), app_id)

        threading.Thread(target=worker, daemon=True).start()

    def _on_progress_update(self, pct: float, msg: str) -> None:
        self.progress_bar.setValue(int(pct * 100))
        self.status_lbl.setText(msg)

    def _on_install_done(self, ok: bool, msg: str, app_id: int) -> None:
        self.progress_bar.setVisible(False)
        self.status_lbl.setText(msg)
        if ok:
            if self.on_game_installed:
                self.on_game_installed(app_id)
            QMessageBox.information(self, "Installed", msg)
        else:
            QMessageBox.critical(self, "Error", msg)

    def _on_files_dropped(self, paths: list[Path]) -> None:
        if not self.lua_installer:
            return
        for p in paths:
            res = self.lua_installer.install_any(p)
            if res.ok and res.app_id and self.on_game_installed:
                self.on_game_installed(res.app_id)
        QMessageBox.information(self, "Drop Install", f"Processed {len(paths)} file(s).")
