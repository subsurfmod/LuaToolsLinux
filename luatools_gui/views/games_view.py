"""Games view - Manage all stplug-in lua games and installed Steam games."""

from __future__ import annotations

import subprocess
import webbrowser
from pathlib import Path
from typing import Callable, Optional

from PyQt6.QtCore import Qt, QMimeData
from PyQt6.QtWidgets import (
    QApplication,
    QComboBox,
    QDialog,
    QDialogButtonBox,
    QFormLayout,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLineEdit,
    QMessageBox,
    QPushButton,
    QTableWidget,
    QTableWidgetItem,
    QVBoxLayout,
    QWidget,
)

from ..models.steam import SteamInstallation, get_game_name, scan_installed_games
from ..services.caches import SteamAppListCache
from ..services.depot_service import DepotService
from ..services.launch_options import LaunchOptionsService
from ..services.lua_installer import LuaInstaller
from ..services.luaservice import LuaToolsService
from ..services.slssteam_service import SlssteamService
from ..services.steam_service import SteamService
from .game_detail_dialog import GameDetailDialog
from .launch_options_dialog import LaunchOptionsDialog


class GamesView(QWidget):
    """View showing all LuaTools-configured and installed Steam games."""

    def __init__(
        self,
        steam_service: SteamService,
        slssteam_service: SlssteamService,
        depot_service: DepotService,
        applist_cache: Optional[SteamAppListCache] = None,
        on_manage_depots: Optional[Callable[[int], None]] = None,
    ) -> None:
        super().__init__()
        self.steam_service = steam_service
        self.slssteam_service = slssteam_service
        self.depot_service = depot_service
        self.applist_cache = applist_cache
        self.luaservice: Optional[LuaToolsService] = None
        self.launch_service: Optional[LaunchOptionsService] = None
        self.on_manage_depots = on_manage_depots

        self._rows: list[dict] = []
        self._setup_ui()

    def _setup_ui(self) -> None:
        layout = QVBoxLayout(self)

        # Header
        header = QHBoxLayout()
        self.title_label = QLabel("Manage Games")
        self.title_label.setStyleSheet("font-size: 18px; font-weight: bold;")
        header.addWidget(self.title_label)
        header.addStretch()

        self.search_edit = QLineEdit()
        self.search_edit.setPlaceholderText("Filter by name or AppID...")
        self.search_edit.setMaximumWidth(260)
        self.search_edit.textChanged.connect(self._apply_filter)
        header.addWidget(self.search_edit)

        self.filter_combo = QComboBox()
        self.filter_combo.addItems(["All Games", "Has Lua", "Installed Only"])
        self.filter_combo.currentIndexChanged.connect(self._apply_filter)
        header.addWidget(self.filter_combo)

        self.add_game_btn = QPushButton("Add by AppID")
        self.add_game_btn.clicked.connect(self._add_game_dialog)
        header.addWidget(self.add_game_btn)

        self.delete_sel_btn = QPushButton("Delete Selected Lua")
        self.delete_sel_btn.setStyleSheet("color: #cc0000;")
        self.delete_sel_btn.clicked.connect(self._bulk_delete_lua)
        header.addWidget(self.delete_sel_btn)

        self.refresh_btn = QPushButton("Refresh")
        self.refresh_btn.clicked.connect(self._refresh)
        header.addWidget(self.refresh_btn)

        layout.addLayout(header)

        # Games table
        self.table = QTableWidget(0, 7)
        self.table.setHorizontalHeaderLabels(
            ["App ID", "Name", "Status", "Lua", "FakeAppId", "Token", "Actions"]
        )
        self.table.horizontalHeader().setSectionResizeMode(1, QHeaderView.ResizeMode.Stretch)
        self.table.horizontalHeader().setSectionResizeMode(6, QHeaderView.ResizeMode.ResizeToContents)
        self.table.setSelectionBehavior(QTableWidget.SelectionBehavior.SelectRows)
        self.table.setAlternatingRowColors(True)
        self.table.itemDoubleClicked.connect(self._on_game_double_clicked)
        layout.addWidget(self.table)

        # Enable drag-and-drop for .lua files
        self.setAcceptDrops(True)
        self.table.setAcceptDrops(True)

        self.status_label = QLabel("No games loaded")
        self.status_label.setStyleSheet("padding: 4px; color: #666;")
        layout.addWidget(self.status_label)

    # --- Drag and Drop support for .lua files ---
    def dragEnterEvent(self, event) -> None:
        if event.mimeData().hasUrls():
            for url in event.mimeData().urls():
                if url.isLocalFile() and url.toLocalFile().lower().endswith('.lua'):
                    event.acceptProposedAction()
                    return
        event.ignore()

    def dragMoveEvent(self, event) -> None:
        if event.mimeData().hasUrls():
            for url in event.mimeData().urls():
                if url.isLocalFile() and url.toLocalFile().lower().endswith('.lua'):
                    event.acceptProposedAction()
                    return
        event.ignore()

    def dropEvent(self, event) -> None:
        if event.mimeData().hasUrls():
            for url in event.mimeData().urls():
                if url.isLocalFile() and url.toLocalFile().lower().endswith('.lua'):
                    file_path = Path(url.toLocalFile())
                    self._handle_lua_drop(file_path)
                    event.acceptProposedAction()
                    return
        event.ignore()

    def _handle_lua_drop(self, file_path: Path) -> None:
        """Handle a dropped .lua file - parse AppID and install."""
        try:
            content = file_path.read_text(encoding="utf-8", errors="replace")
            
            # Try to extract AppID from the lua content
            import re
            # Look for addappid(AppID) pattern
            matches = re.findall(r'addappid\s*\(\s*(\d+)\s*(?:,\s*\d+\s*(?:,\s*"[^"]*")?)?\s*\)', content)
            if not matches:
                QMessageBox.warning(self, "Invalid Lua File", 
                    "Could not find an addappid(AppID) pattern in the lua file.")
                return
            
            # Use the first AppID found
            app_id = int(matches[0])
            
            # Read the full content
            content = file_path.read_text(encoding="utf-8", errors="replace")
            
            # Use LuaInstaller to install
            if hasattr(self, 'lua_installer') and self.lua_installer:
                result = self.lua_installer.install_lua(app_id, content)
                if result.ok:
                    self.status_label.setText(f"Installed lua for AppID {app_id} from {file_path.name}")
                    self._refresh()
                else:
                    QMessageBox.critical(self, "Error", f"Failed to install lua: {result.error}")
            elif self.luaservice and self.luaservice.write_lua(int(matches[0]), file_path.read_text(encoding="utf-8", errors="replace")):
                self._refresh()
            else:
                QMessageBox.critical(self, "Error", "Lua installer not available")
                
        except Exception as e:
            QMessageBox.critical(self, "Error", f"Failed to process lua file: {e}")

    def set_installation(self, installation: SteamInstallation) -> None:
        stplug_dir = installation.stplug_in_path
        if stplug_dir:
            self.luaservice = LuaToolsService(stplug_dir)
            from ..services.lua_installer import LuaInstaller
            self.lua_installer = LuaInstaller(installation, self.slssteam_service)
        self.launch_service = LaunchOptionsService(installation)
        self._refresh()

    def _refresh(self) -> None:
        inst = self.steam_service.installation
        if not inst:
            return

        self.status_label.setText("Scanning libraries and stplug-in...")
        QApplication.processEvents()

        if self.applist_cache:
            self.applist_cache.ensure_loaded()

        installed_names: dict[int, str] = {}
        install_paths: dict[int, Path] = {}
        for lib in inst.libraries:
            for aid in scan_installed_games(lib):
                installed_names[aid] = get_game_name(lib, aid)
                acf = lib.steamapps / f"appmanifest_{aid}.acf"
                if acf.exists():
                    import re
                    m = re.search(r'"installdir"\s+"([^"]+)"', acf.read_text(encoding="utf-8", errors="replace"))
                    if m:
                        install_paths[aid] = lib.common / m.group(1)

        lua_ids = set(self.luaservice.list_lua_files()) if self.luaservice else set()
        all_ids = sorted(set(installed_names.keys()) | lua_ids)

        self._rows = []
        for aid in all_ids:
            is_inst = aid in installed_names
            has_lua = aid in lua_ids
            name = installed_names.get(aid) or (
                self.applist_cache.get_name(aid) if self.applist_cache else None
            ) or f"App {aid}"
            status = (
                "Installed + Lua" if (is_inst and has_lua)
                else ("Lua Only" if has_lua else "Installed")
            )
            self._rows.append({
                "app_id": aid,
                "name": name,
                "installed": is_inst,
                "has_lua": has_lua,
                "status": status,
                "install_path": install_paths.get(aid),
            })

        self._apply_filter()

    def _apply_filter(self) -> None:
        q = self.search_edit.text().strip().lower()
        mode = self.filter_combo.currentIndex()

        filtered = []
        for r in self._rows:
            if mode == 1 and not r["has_lua"]:
                continue
            if mode == 2 and not r["installed"]:
                continue
            if q and q not in r["name"].lower() and q not in str(r["app_id"]):
                continue
            filtered.append(r)

        self.table.setRowCount(len(filtered))
        for row, r in enumerate(filtered):
            aid = r["app_id"]

            id_item = QTableWidgetItem(str(aid))
            id_item.setFlags(id_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.table.setItem(row, 0, id_item)

            name_item = QTableWidgetItem(r["name"])
            name_item.setFlags(name_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.table.setItem(row, 1, name_item)

            st_item = QTableWidgetItem(r["status"])
            st_item.setFlags(st_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.table.setItem(row, 2, st_item)

            lua_item = QTableWidgetItem("Yes" if r["has_lua"] else "No")
            lua_item.setFlags(lua_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.table.setItem(row, 3, lua_item)

            fake_id = self.slssteam_service.get_fake_app_id(aid)
            fake_item = QTableWidgetItem(str(fake_id) if fake_id else "No")
            fake_item.setFlags(fake_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.table.setItem(row, 4, fake_item)

            has_tok = self.slssteam_service.has_token(aid)
            tok_item = QTableWidgetItem("Yes" if has_tok else "No")
            tok_item.setFlags(tok_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.table.setItem(row, 5, tok_item)

            actions_w = QWidget()
            al = QHBoxLayout(actions_w)
            al.setContentsMargins(2, 2, 2, 2)

            edit_btn = QPushButton("Edit")
            edit_btn.setFixedWidth(48)
            edit_btn.clicked.connect(lambda _, a=aid, n=r["name"]: self._edit_game(a, n))
            al.addWidget(edit_btn)

            depots_btn = QPushButton("Depots")
            depots_btn.setFixedWidth(58)
            depots_btn.clicked.connect(lambda _, a=aid: self.on_manage_depots and self.on_manage_depots(a))
            al.addWidget(depots_btn)

            launch_btn = QPushButton("Launch")
            launch_btn.setFixedWidth(58)
            launch_btn.clicked.connect(lambda _, a=aid, n=r["name"]: self._edit_launch_options(a, n))
            al.addWidget(launch_btn)

            if r["install_path"]:
                folder_btn = QPushButton("Folder")
                folder_btn.setFixedWidth(54)
                folder_btn.clicked.connect(lambda _, p=r["install_path"]: self._open_folder(p))
                al.addWidget(folder_btn)

            store_btn = QPushButton("Store")
            store_btn.setFixedWidth(50)
            store_btn.clicked.connect(lambda _, a=aid: webbrowser.open(f"https://store.steampowered.com/app/{a}"))
            al.addWidget(store_btn)

            self.table.setCellWidget(row, 6, actions_w)

        self.status_label.setText(f"Showing {len(filtered)} of {len(self._rows)} games")

    def _on_game_double_clicked(self, item) -> None:
        row = item.row()
        aid = int(self.table.item(row, 0).text())
        name = self.table.item(row, 1).text()
        self._edit_game(aid, name)

    def _edit_game(self, app_id: int, name: str) -> None:
        dialog = GameDetailDialog(
            app_id=app_id,
            game_name=name,
            steam_service=self.steam_service,
            slssteam_service=self.slssteam_service,
            depot_service=self.depot_service,
            luaservice=self.luaservice,
            parent=self,
        )
        dialog.exec()
        self._refresh()

    def _edit_launch_options(self, app_id: int, name: str) -> None:
        if not self.launch_service:
            return
        dlg = LaunchOptionsDialog(app_id, name, self.launch_service, parent=self)
        dlg.exec()

    @staticmethod
    def _open_folder(path: Path) -> None:
        try:
            subprocess.Popen(["xdg-open", str(path)])
        except Exception:
            pass

    def _bulk_delete_lua(self) -> None:
        if not self.luaservice:
            return
        rows = sorted({idx.row() for idx in self.table.selectedIndexes()})
        if not rows:
            QMessageBox.information(self, "No Selection", "Select one or more rows first.")
            return
        app_ids = [int(self.table.item(r, 0).text()) for r in rows]
        reply = QMessageBox.question(
            self,
            "Delete Selected Lua Files",
            f"Delete .lua files for {len(app_ids)} selected game(s)?",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if reply != QMessageBox.StandardButton.Yes:
            return
        for aid in app_ids:
            self.luaservice.delete_lua(aid)
        self._refresh()

    def _add_game_dialog(self) -> None:
        dialog = QDialog(self)
        dialog.setWindowTitle("Add Game by AppID")
        dialog.setMinimumWidth(400)
        layout = QFormLayout(dialog)

        app_id_edit = QLineEdit()
        app_id_edit.setPlaceholderText("e.g. 1245620")
        layout.addRow("AppID:", app_id_edit)

        name_edit = QLineEdit()
        name_edit.setPlaceholderText("Optional game name")
        layout.addRow("Name:", name_edit)

        template_combo = QComboBox()
        template_combo.addItems(["Empty (just addappid)", "With DLCs", "With depot pinning"])
        layout.addRow("Template:", template_combo)

        buttons = QDialogButtonBox(QDialogButtonBox.StandardButton.Ok | QDialogButtonBox.StandardButton.Cancel)
        buttons.accepted.connect(dialog.accept)
        buttons.rejected.connect(dialog.reject)
        layout.addRow(buttons)

        if dialog.exec() == QDialog.DialogCode.Accepted:
            aid_txt = app_id_edit.text().strip()
            if not aid_txt.isdigit():
                QMessageBox.warning(self, "Invalid AppID", "Please enter a numeric AppID.")
                return
            aid = int(aid_txt)
            gname = name_edit.text().strip()
            comment = f" -- {gname}" if gname else ""
            idx = template_combo.currentIndex()
            if idx == 0:
                content = f"addappid({aid}){comment}\n"
            elif idx == 1:
                content = f"addappid({aid}){comment}\n\n-- addappid(DLC_APPID)\n"
            else:
                content = f'addappid({aid}){comment}\naddappid({aid}, 1, "KEY")\nsetManifestid({aid}, "MANIFEST", 0)\n'
            
            # Use LuaInstaller to write lua and sync with SLSsteam
            if hasattr(self, 'lua_installer') and self.lua_installer:
                result = self.lua_installer.install_lua(aid, content)
                if result.ok:
                    self.status_label.setText(f"Created lua for AppID {aid} and synced with SLSsteam")
                    self._refresh()
                else:
                    QMessageBox.critical(self, "Error", f"Failed to install lua: {result.error}")
            elif self.luaservice and self.luaservice.write_lua(aid, content):
                self._refresh()
