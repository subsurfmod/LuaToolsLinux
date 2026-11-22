"""Fixes View - Denuvo/Game fixes catalog, tag filtering, apply with backup & revert."""

from __future__ import annotations

import re
import threading
from pathlib import Path
from typing import Optional

from PyQt6.QtCore import Qt, pyqtSignal
from PyQt6.QtWidgets import (
    QCheckBox,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QListWidget,
    QListWidgetItem,
    QMessageBox,
    QProgressBar,
    QPushButton,
    QSplitter,
    QTableWidget,
    QTableWidgetItem,
    QHeaderView,
    QVBoxLayout,
    QWidget,
)

from ..models.steam import SteamInstallation
from ..services.api_client import DenuvoFixItem, DenuvoGameListing, DenuvoTag
from ..services.auth_service import AuthService
from ..services.fixes import FixesService
from ..services.luaservice import LuaToolsService
from ..services.steam_service import SteamService


class FixesView(QWidget):
    """Browse /api/denuvo/listings, filter by tags/my games, apply & revert fixes."""

    _listings_loaded = pyqtSignal(list, list)
    _fixes_loaded = pyqtSignal(int, list)
    _action_done = pyqtSignal(bool, str)
    _progress_update = pyqtSignal(float, str)

    def __init__(
        self,
        steam_service: SteamService,
        fixes_service: FixesService,
        steam_installation: SteamInstallation,
        auth_service: Optional[AuthService] = None,
        parent=None,
    ) -> None:
        super().__init__(parent)
        self.steam_service = steam_service
        self.fixes_service = fixes_service
        self.steam_installation = steam_installation
        self.auth_service = auth_service

        self._all_games: list[DenuvoGameListing] = []
        self._all_tags: list[DenuvoTag] = []
        self._selected_tag_id: Optional[str] = None
        self._current_app_id: Optional[int] = None
        self._current_fixes: list[DenuvoFixItem] = []

        self._listings_loaded.connect(self._on_listings_loaded)
        self._fixes_loaded.connect(self._on_fixes_loaded)
        self._action_done.connect(self._on_action_done)
        self._progress_update.connect(self._on_progress_update)

        self._setup_ui()
        self.refresh_listings()

    def _setup_ui(self) -> None:
        layout = QVBoxLayout(self)

        # Header
        header = QHBoxLayout()
        title = QLabel("Game & Denuvo Fixes")
        title.setStyleSheet("font-weight: bold; font-size: 16px;")
        header.addWidget(title)
        header.addStretch()

        self.search_edit = QLineEdit()
        self.search_edit.setPlaceholderText("Search fixes by game or AppID...")
        self.search_edit.setMaximumWidth(250)
        self.search_edit.textChanged.connect(self._apply_filter)
        header.addWidget(self.search_edit)

        self.my_games_cb = QCheckBox("My Games Only")
        self.my_games_cb.stateChanged.connect(self._apply_filter)
        header.addWidget(self.my_games_cb)

        refresh_btn = QPushButton("Refresh Catalog")
        refresh_btn.clicked.connect(self.refresh_listings)
        header.addWidget(refresh_btn)
        layout.addLayout(header)

        # Tag filter row
        self.tags_layout = QHBoxLayout()
        self.tags_layout.addWidget(QLabel("Tags:"))
        self.tags_layout.addStretch()
        layout.addLayout(self.tags_layout)

        # Splitter: Left (Games) | Right (Fixes for selected game)
        splitter = QSplitter(Qt.Orientation.Horizontal)
        layout.addWidget(splitter, stretch=1)

        self.games_list = QListWidget()
        self.games_list.itemClicked.connect(self._on_game_clicked)
        splitter.addWidget(self.games_list)

        right_w = QWidget()
        right_l = QVBoxLayout(right_w)
        right_l.setContentsMargins(6, 0, 0, 0)

        self.game_title_lbl = QLabel("Select a game from the catalog")
        self.game_title_lbl.setStyleSheet("font-size: 15px; font-weight: bold;")
        right_l.addWidget(self.game_title_lbl)

        self.table = QTableWidget(0, 4)
        self.table.setHorizontalHeaderLabels(["Fix Title", "Tags", "Status", "Actions"])
        self.table.horizontalHeader().setSectionResizeMode(0, QHeaderView.ResizeMode.Stretch)
        self.table.horizontalHeader().setSectionResizeMode(3, QHeaderView.ResizeMode.ResizeToContents)
        self.table.setAlternatingRowColors(True)
        right_l.addWidget(self.table)

        splitter.addWidget(right_w)
        splitter.setSizes([340, 720])

        self.progress_bar = QProgressBar()
        self.progress_bar.setVisible(False)
        layout.addWidget(self.progress_bar)

        self.status_label = QLabel("Loading fixes catalog...")
        self.status_label.setStyleSheet("padding: 4px; color: #666;")
        layout.addWidget(self.status_label)

    def set_games(self, app_ids: list[int]) -> None:
        # Kept for compatibility with MainWindow
        pass

    def refresh_listings(self) -> None:
        self.status_label.setText("Fetching fixes catalog from lua.tools...")
        threading.Thread(
            target=lambda: self._listings_loaded.emit(*self.fixes_service.get_listings()),
            daemon=True,
        ).start()

    def _on_listings_loaded(self, games: list[DenuvoGameListing], tags: list[DenuvoTag]) -> None:
        self._all_games = games
        self._all_tags = tags

        # Rebuild tag pills
        while self.tags_layout.count() > 1:
            item = self.tags_layout.takeAt(1)
            if item.widget():
                item.widget().deleteLater()

        all_btn = QPushButton("All")
        all_btn.clicked.connect(lambda: self._select_tag(None))
        self.tags_layout.addWidget(all_btn)

        for t in tags:
            btn = QPushButton(t.name)
            btn.clicked.connect(lambda _, tid=t.id: self._select_tag(tid))
            self.tags_layout.addWidget(btn)
        self.tags_layout.addStretch()

        self._apply_filter()

    def _select_tag(self, tag_id: Optional[str]) -> None:
        self._selected_tag_id = tag_id if self._selected_tag_id != tag_id else None
        self._apply_filter()

    def _apply_filter(self) -> None:
        q = self.search_edit.text().strip().lower()
        my_only = self.my_games_cb.isChecked()
        my_ids: set[str] = set()
        if my_only and self.steam_installation.stplug_in_path:
            ls = LuaToolsService(self.steam_installation.stplug_in_path)
            my_ids = {str(x) for x in ls.list_lua_files()}

        self.games_list.clear()
        shown = 0
        for g in self._all_games:
            if self._selected_tag_id and not any(t.id == self._selected_tag_id for t in g.tags):
                continue
            if my_only and g.app_id not in my_ids:
                continue
            if q and q not in g.name.lower() and q not in g.app_id:
                continue
            item = QListWidgetItem(f"{g.name} ({g.app_id}) [{g.fix_count}]")
            item.setData(Qt.ItemDataRole.UserRole, g)
            self.games_list.addItem(item)
            shown += 1

        self.status_label.setText(f"Showing {shown} of {len(self._all_games)} games with fixes")

    def _on_game_clicked(self, item: QListWidgetItem) -> None:
        g: DenuvoGameListing = item.data(Qt.ItemDataRole.UserRole)
        if g and g.app_id.isdigit():
            self.open_app(int(g.app_id), g.name)

    def open_app(self, app_id: int, name: Optional[str] = None) -> None:
        self._current_app_id = app_id
        self.game_title_lbl.setText(f"{name or f'App {app_id}'} (AppID {app_id})")
        self.status_label.setText(f"Loading fixes for AppID {app_id}...")
        threading.Thread(
            target=lambda: self._fixes_loaded.emit(app_id, self.fixes_service.get_fixes(app_id)),
            daemon=True,
        ).start()

    def _find_install_dir(self, app_id: int) -> Optional[Path]:
        for lib in self.steam_installation.libraries:
            acf = lib.steamapps / f"appmanifest_{app_id}.acf"
            if acf.exists():
                m = re.search(r'"installdir"\s+"([^"]+)"', acf.read_text(encoding="utf-8", errors="replace"))
                if m:
                    p = lib.common / m.group(1)
                    if p.exists():
                        return p
        return None

    def _on_fixes_loaded(self, app_id: int, fixes: list[DenuvoFixItem]) -> None:
        if app_id != self._current_app_id:
            return
        self._current_fixes = fixes
        install_dir = self._find_install_dir(app_id)
        self.table.setRowCount(len(fixes))

        for row, fix in enumerate(fixes):
            title_item = QTableWidgetItem(fix.title)
            title_item.setToolTip(fix.description)
            title_item.setFlags(title_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.table.setItem(row, 0, title_item)

            tags_item = QTableWidgetItem(", ".join(t.name for t in fix.tags))
            tags_item.setFlags(tags_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.table.setItem(row, 1, tags_item)

            applied = bool(install_dir and self.fixes_service.is_fix_applied(str(install_dir), fix.id))
            status_txt = "Applied" if applied else ("Ready" if install_dir else "Game not installed")
            st_item = QTableWidgetItem(status_txt)
            st_item.setFlags(st_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.table.setItem(row, 2, st_item)

            aw = QWidget()
            al = QHBoxLayout(aw)
            al.setContentsMargins(2, 2, 2, 2)

            if fix.has_manifest:
                m_btn = QPushButton("Manifest")
                m_btn.clicked.connect(lambda _, f=fix: self._apply_manifest(f))
                al.addWidget(m_btn)

            if fix.has_fix:
                f_btn = QPushButton("Apply Fix")
                f_btn.setEnabled(install_dir is not None)
                f_btn.clicked.connect(lambda _, f=fix, d=install_dir: self._apply_fix(f, d))
                al.addWidget(f_btn)

            if applied and install_dir:
                u_btn = QPushButton("Revert")
                u_btn.setStyleSheet("color: #cc0000;")
                u_btn.clicked.connect(lambda _, f=fix, d=install_dir: self._revert_fix(f, d))
                al.addWidget(u_btn)

            self.table.setCellWidget(row, 3, aw)

        self.status_label.setText(f"Found {len(fixes)} fix(es) for AppID {app_id}")

    def _apply_manifest(self, fix: DenuvoFixItem) -> None:
        if not self._current_app_id:
            return
        if self.auth_service and not self.auth_service.is_signed_in:
            QMessageBox.warning(self, "Sign In Required", "Please sign in with Discord in Settings first.")
            return
        aid = self._current_app_id
        self.progress_bar.setVisible(True)
        self.progress_bar.setValue(0)

        def worker():
            res = self.fixes_service.apply_denuvo_manifest(
                aid, fix, progress_cb=lambda p, m: self._progress_update.emit(p, m)
            )
            self._action_done.emit(res.ok, f"Installed manifest for {fix.title}" if res.ok else (res.error or "Failed"))

        threading.Thread(target=worker, daemon=True).start()

    def _apply_fix(self, fix: DenuvoFixItem, install_dir: Optional[Path]) -> None:
        if not self._current_app_id or not install_dir:
            return
        if self.auth_service and not self.auth_service.is_signed_in:
            QMessageBox.warning(self, "Sign In Required", "Please sign in with Discord in Settings first.")
            return
        aid = self._current_app_id
        self.progress_bar.setVisible(True)
        self.progress_bar.setValue(0)

        def worker():
            res = self.fixes_service.apply_denuvo_fix(
                aid, fix, str(install_dir), progress_cb=lambda p, m: self._progress_update.emit(p, m)
            )
            self._action_done.emit(
                res.ok,
                f"Applied fix ({len(res.applied_files)} file(s))" if res.ok else (res.error or "Failed"),
            )

        threading.Thread(target=worker, daemon=True).start()

    def _revert_fix(self, fix: DenuvoFixItem, install_dir: Path) -> None:
        if not self._current_app_id:
            return
        res = self.fixes_service.unfix_game(self._current_app_id, str(install_dir), fix.id)
        if res.ok:
            QMessageBox.information(self, "Reverted", f"Reverted {len(res.applied_files)} file(s).")
            self.open_app(self._current_app_id)
        else:
            QMessageBox.critical(self, "Revert Failed", res.error or "Unknown error")

    def _on_progress_update(self, pct: float, msg: str) -> None:
        self.progress_bar.setValue(int(pct * 100))
        self.status_label.setText(msg)

    def _on_action_done(self, ok: bool, msg: str) -> None:
        self.progress_bar.setVisible(False)
        self.status_label.setText(msg)
        if ok:
            QMessageBox.information(self, "Success", msg)
            if self._current_app_id:
                self.open_app(self._current_app_id)
        else:
            QMessageBox.critical(self, "Error", msg)
