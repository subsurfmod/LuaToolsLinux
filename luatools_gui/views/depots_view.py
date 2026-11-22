"""Depots View - variant switcher, depot table with toggles, and download picker."""

from __future__ import annotations

import os
import re
import shutil
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional, Callable

from PyQt6.QtCore import Qt, QTimer, pyqtSignal
from PyQt6.QtGui import QAction, QColor, QFont
from PyQt6.QtWidgets import (
    QAbstractItemView,
    QCheckBox,
    QComboBox,
    QDialog,
    QDialogButtonBox,
    QFileDialog,
    QFormLayout,
    QGroupBox,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLineEdit,
    QMessageBox,
    QPushButton,
    QScrollArea,
    QSplitter,
    QTableWidget,
    QTableWidgetItem,
    QTabWidget,
    QTextEdit,
    QTreeWidget,
    QTreeWidgetItem,
    QVBoxLayout,
    QWidget,
)

from ..models.vault.lua_vault import LuaVault, LuaVariant
from ..parsers.lua_parser import LuaContents, LuaEntry, parse as parse_lua, diff as diff_lua
from ..services.depot_downloader import DepotDownloaderService, DepotSelection, DepotRunResult
from ..services.depot_info import SteamDepotInfo, ContentDepot
from ..services.steam_service import SteamService
from ..services.slssteam_service import SlssteamService
from ..services.millennium_service import MillenniumService
from ..models.steam import SteamInstallation


@dataclass
class DepotPickRow:
    """One tickable depot in the download picker."""
    depot: ContentDepot
    lua_entry: Optional[object] = None  # LuaEntry
    manifest_id: Optional[str] = None
    manifest_path: Optional[str] = None
    has_key: bool = False
    size: int = 0
    blocked_reason: Optional[str] = None
    is_selected: bool = False


class DepotDetailDialog(QDialog):
    """Dialog to view/edit depot details."""

    def __init__(self, depot: ContentDepot, lua_entry: Optional[LuaEntry], parent=None):
        super().__init__(parent)
        self.setWindowTitle(f"Depot {depot.id} Details")
        self.resize(500, 400)

        layout = QFormLayout(self)

        self.depot = depot
        self.lua_entry = lua_entry

        # Basic info
        layout.addRow("Depot ID:", QLabel(str(depot.id)))
        layout.addRow("Size:", QLabel(self._format_size(depot.size)))

        if depot.is_shared:
            layout.addRow("Type:", QLabel("Shared Redistributable"))
            if depot.from_app_id:
                layout.addRow("From App:", QLabel(str(depot.from_app_id)))
        elif depot.is_dlc:
            layout.addRow("Type:", QLabel("DLC"))
            if depot.dlc_app_id:
                layout.addRow("DLC App ID:", QLabel(str(depot.dlc_app_id)))
        else:
            layout.addRow("Type:", QLabel("Game Content"))

        if depot.os:
            layout.addRow("OS:", QLabel(depot.os))
        if depot.language:
            layout.addRow("Language:", QLabel(depot.language))

        if depot.public_manifest_id:
            layout.addRow("Public Manifest:", QLabel(depot.public_manifest_id))

        # Lua pin info
        if lua_entry:
            if lua_entry.manifest_id:
                layout.addRow("Pinned Manifest:", QLabel(lua_entry.manifest_id))
            if lua_entry.commented_manifest_id:
                layout.addRow("Commented Pin:", QLabel(lua_entry.commented_manifest_id))
            if lua_entry.key:
                layout.addRow("Decryption Key:", QLabel(lua_entry.key[:8] + "..."))
            if lua_entry.size_on_disk:
                layout.addRow("Size in Lua:", QLabel(self._format_size(lua_entry.size_on_disk)))

        # Buttons
        buttons = QDialogButtonBox(QDialogButtonBox.StandardButton.Close)
        buttons.rejected.connect(self.reject)
        layout.addRow(buttons)

    @staticmethod
    def _format_size(size: int) -> str:
        for unit in ["B", "KB", "MB", "GB", "TB"]:
            if size < 1024:
                return f"{size:.1f} {unit}"
            size /= 1024
        return f"{size:.1f} PB"


class DepotsView(QWidget):
    """Main depots view with variant switcher, depot table, and download picker."""

    # Signals
    game_selected = pyqtSignal(int)  # app_id

    def __init__(
        self,
        steam_service: SteamService,
        slssteam_service: SlssteamService,
        depot_downloader: DepotDownloaderService,
        depot_info: SteamDepotInfo,
        lua_vault: LuaVault,
        millennium_service: MillenniumService,
        steam_installation: SteamInstallation,
        parent=None
    ):
        super().__init__(parent)

        self.steam_service = steam_service
        self.slssteam_service = slssteam_service
        self.depot_downloader = depot_downloader
        self.depot_info = depot_info
        self.lua_vault = lua_vault
        self.millennium_service = millennium_service
        self.steam_installation = steam_installation

        # State
        self.current_app_id: Optional[int] = None
        self.current_lua: Optional[LuaContents] = None
        self.current_variants: list[LuaVariant] = []
        self.current_selection: Optional[LuaVariant] = None
        self.depot_picks: list[DepotPickRow] = []
        self.is_select_mode = False

        self._setup_ui()
        self._connect_signals()

    def _setup_ui(self) -> None:
        layout = QVBoxLayout(self)

        # Toolbar
        toolbar = QHBoxLayout()
        self.app_label = QLabel("Select a game from the list")
        self.app_label.setStyleSheet("font-weight: bold; font-size: 14px;")
        toolbar.addWidget(self.app_label)
        toolbar.addStretch()

        self.refresh_btn = QPushButton("Refresh")
        self.refresh_btn.clicked.connect(self._refresh)
        toolbar.addWidget(self.refresh_btn)

        self.select_depots_btn = QPushButton("Download Depots")
        self.select_depots_btn.setEnabled(False)
        self.select_depots_btn.clicked.connect(self._toggle_select_mode)
        toolbar.addWidget(self.select_depots_btn)

        layout.addLayout(toolbar)

        # Main splitter
        splitter = QSplitter(Qt.Orientation.Horizontal)
        layout.addWidget(splitter)

        # Left: Variant Switcher
        left_widget = QWidget()
        left_layout = QVBoxLayout(left_widget)
        left_layout.setContentsMargins(0, 0, 0, 0)

        var_group = QGroupBox("Stored Variants")
        var_layout = QVBoxLayout(var_group)

        self.variants_tree = QTreeWidget()
        self.variants_tree.setHeaderLabels(["Variant", "Kind", "Build ID", "Pinned", "Depots", "DLCs"])
        self.variants_tree.setSelectionMode(QAbstractItemView.SelectionMode.SingleSelection)
        self.variants_tree.itemSelectionChanged.connect(self._on_variant_selected)
        self.variants_tree.setContextMenuPolicy(Qt.ContextMenuPolicy.CustomContextMenu)
        self.variants_tree.customContextMenuRequested.connect(self._show_variant_context_menu)
        var_layout.addWidget(self.variants_tree)

        var_btn_layout = QHBoxLayout()
        self.apply_variant_btn = QPushButton("Apply")
        self.apply_variant_btn.setEnabled(False)
        self.apply_variant_btn.clicked.connect(self._apply_variant)
        var_btn_layout.addWidget(self.apply_variant_btn)

        self.save_preset_btn = QPushButton("Save as Preset")
        self.save_preset_btn.setEnabled(False)
        self.save_preset_btn.clicked.connect(self._save_as_preset)
        var_btn_layout.addWidget(self.save_preset_btn)

        self.delete_variant_btn = QPushButton("Delete")
        self.delete_variant_btn.setEnabled(False)
        self.delete_variant_btn.clicked.connect(self._delete_variant)
        var_btn_layout.addWidget(self.delete_variant_btn)

        var_layout.addLayout(var_btn_layout)
        left_layout.addWidget(var_group)

        # Editor
        editor_group = QGroupBox("Live Lua Editor")
        editor_layout = QVBoxLayout(editor_group)

        self.editor = QTextEdit()
        self.editor.setFont(QFont("Monospace", 10))
        self.editor.setPlaceholderText("Select a variant or click Edit to modify live lua...")
        editor_layout.addWidget(self.editor)

        edit_btn_layout = QHBoxLayout()
        self.edit_btn = QPushButton("Edit Live")
        self.edit_btn.setEnabled(False)
        self.edit_btn.clicked.connect(self._edit_live)
        edit_btn_layout.addWidget(self.edit_btn)

        self.save_edit_btn = QPushButton("Save Edit")
        self.save_edit_btn.setEnabled(False)
        self.save_edit_btn.setVisible(False)
        self.save_edit_btn.clicked.connect(self._save_edit)
        edit_btn_layout.addWidget(self.save_edit_btn)

        self.cancel_edit_btn = QPushButton("Cancel")
        self.cancel_edit_btn.setEnabled(False)
        self.cancel_edit_btn.setVisible(False)
        self.cancel_edit_btn.clicked.connect(self._cancel_edit)
        edit_btn_layout.addWidget(self.cancel_edit_btn)

        edit_btn_layout.addStretch()
        editor_layout.addLayout(edit_btn_layout)
        left_layout.addWidget(editor_group)

        splitter.addWidget(left_widget)

        # Right: Depot Table + Download Picker
        right_widget = QWidget()
        right_layout = QVBoxLayout(right_widget)
        right_layout.setContentsMargins(0, 0, 0, 0)

        self.tab_widget = QTabWidget()
        right_layout.addWidget(self.tab_widget)

        # Tab 1: Depot Table
        self.table_widget = QWidget()
        table_layout = QVBoxLayout(self.table_widget)

        self.depot_table = QTableWidget()
        self.depot_table.setColumnCount(8)
        self.depot_table.setHorizontalHeaderLabels([
            "Depot ID", "Title", "Size", "OS", "Language",
            "Type", "Pinned", "Actions"
        ])
        self.depot_table.horizontalHeader().setSectionResizeMode(1, QHeaderView.ResizeMode.Stretch)
        self.depot_table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        self.depot_table.setAlternatingRowColors(True)
        self.depot_table.itemDoubleClicked.connect(self._on_depot_double_clicked)
        table_layout.addWidget(self.depot_table)

        self.tab_widget.addTab(self.table_widget, "Depots")

        # Tab 2: Download Picker
        self.picker_widget = QWidget()
        picker_layout = QVBoxLayout(self.picker_widget)

        # Destination
        dest_group = QGroupBox("Download Destination")
        dest_layout = QFormLayout(dest_group)

        self.dest_edit = QLineEdit()
        default_dest = Path.home() / "LuaTools Depots"
        self.dest_edit.setText(str(default_dest))
        dest_layout.addRow("Folder:", self.dest_edit)

        self.browse_dest_btn = QPushButton("Browse")
        self.browse_dest_btn.clicked.connect(self._browse_dest)
        dest_layout.addRow("", self.browse_dest_btn)

        self.space_label = QLabel("")
        dest_layout.addRow("Free Space:", self.space_label)

        picker_layout.addWidget(dest_group)

        # Depot picks table
        self.picker_table = QTableWidget()
        self.picker_table.setColumnCount(7)
        self.picker_table.setHorizontalHeaderLabels([
            "✓", "Depot ID", "Title", "Size", "OS/Lang", "Type", "Status"
        ])
        self.picker_table.horizontalHeader().setSectionResizeMode(2, QHeaderView.ResizeMode.Stretch)
        self.picker_table.setAlternatingRowColors(True)
        picker_layout.addWidget(self.picker_table)

        # Picker buttons
        picker_btn_layout = QHBoxLayout()
        self.select_all_btn = QPushButton("Select All")
        self.select_all_btn.clicked.connect(lambda: self._set_all_picks(True))
        picker_btn_layout.addWidget(self.select_all_btn)

        self.deselect_all_btn = QPushButton("Deselect All")
        self.deselect_all_btn.clicked.connect(lambda: self._set_all_picks(False))
        picker_btn_layout.addWidget(self.deselect_all_btn)

        picker_btn_layout.addStretch()

        self.download_btn = QPushButton("Download Selected")
        self.download_btn.setStyleSheet("font-weight: bold; padding: 8px;")
        self.download_btn.clicked.connect(self._start_download)
        picker_btn_layout.addWidget(self.download_btn)

        self.cancel_picker_btn = QPushButton("Cancel")
        self.cancel_picker_btn.clicked.connect(self._cancel_picker)
        picker_btn_layout.addWidget(self.cancel_picker_btn)

        picker_layout.addLayout(picker_btn_layout)

        # Progress
        self.download_progress = QLabel("")
        self.download_progress.setWordWrap(True)
        picker_layout.addWidget(self.download_progress)

        self.tab_widget.addTab(self.picker_widget, "Download Depots")

        splitter.addWidget(right_widget)

        # Set splitter sizes (30% left, 70% right)
        splitter.setSizes([400, 800])

        # Status
        self.status_label = QLabel("Select a game to view depots")
        self.status_label.setStyleSheet("padding: 4px; color: #666;")
        layout.addWidget(self.status_label)

    def _connect_signals(self) -> None:
        pass

    def set_game(self, app_id: int) -> None:
        """Set the current game and load its data."""
        self.current_app_id = app_id
        self.current_lua = None
        self.current_variants = []
        self.current_selection = None

        # Update UI
        game_name = self.steam_service.get_game_name(app_id)
        self.app_label.setText(f"{game_name} (AppID {app_id})")
        self.select_depots_btn.setEnabled(True)
        self.status_label.setText("Loading...")

        # Load lua
        self._load_lua()

        # Load variants
        self._load_variants()

        # Load depot info
        QTimer.singleShot(100, self._load_depots)

    def _load_lua(self) -> None:
        """Load and parse the live lua file."""
        if not self.current_app_id:
            return

        stplug_dir = self.steam_installation.stplug_in_path
        if stplug_dir:
            lua_file = stplug_dir / f"{self.current_app_id}.lua"
            if lua_file.exists():
                self.current_lua = parse_lua(lua_file, self.current_app_id)
            else:
                self.current_lua = None

    def _load_variants(self) -> None:
        """Load stored variants from vault."""
        if not self.current_app_id:
            return

        self.current_variants = self.lua_vault.get_variants(self.current_app_id)
        self._populate_variants_tree()

    def _populate_variants_tree(self) -> None:
        """Populate the variants tree widget."""
        self.variants_tree.clear()

        active_hash = self.lua_vault.get_active_hash(self.current_app_id) if self.current_app_id else None

        for variant in self.current_variants:
            item = QTreeWidgetItem([
                variant.display_label,
                variant.kind.capitalize(),
                variant.build_id or "-",
                "Yes" if variant.is_pinned else "No",
                str(variant.depot_count),
                str(variant.dlc_count)
            ])
            item.setData(0, Qt.ItemDataRole.UserRole, variant)

            if variant.hash == active_hash:
                item.setBackground(0, QColor("#c8e6c9"))
                item.setFont(0, QFont("", weight=QFont.Weight.Bold))

            self.variants_tree.addTopLevelItem(item)

        self.variants_tree.resizeColumnToContents(0)

    def _on_variant_selected(self) -> None:
        """Handle variant selection change."""
        items = self.variants_tree.selectedItems()
        if not items:
            self.current_selection = None
            self.apply_variant_btn.setEnabled(False)
            self.save_preset_btn.setEnabled(False)
            self.delete_variant_btn.setEnabled(False)
            self.edit_btn.setEnabled(False)
            self.editor.setPlaceholderText("Select a variant or click Edit to modify live lua...")
            self.editor.clear()
            return

        item = items[0]
        variant = item.data(0, Qt.ItemDataRole.UserRole)
        self.current_selection = variant

        # Update button states
        self.apply_variant_btn.setEnabled(True)
        self.save_preset_btn.setEnabled(True)
        self.delete_variant_btn.setEnabled(True)
        self.edit_btn.setEnabled(True)

        # Load variant text into editor
        text = self.lua_vault.read_text(self.current_app_id, variant.hash)
        self.editor.setPlainText(text)
        self.editor.setReadOnly(True)

    def _apply_variant(self) -> None:
        """Apply selected variant as live lua."""
        if not self.current_selection or not self.current_app_id:
            return

        if self.lua_vault.apply(self.current_app_id, self.current_selection):
            self.status_label.setText(f"Applied variant: {self.current_selection.display_label}")
            self._load_variants()
            self._load_lua()
            self._load_depots()
        else:
            QMessageBox.critical(self, "Error", "Failed to apply variant")

    def _save_as_preset(self) -> None:
        """Save live lua as a new preset."""
        if not self.current_app_id:
            return

        text = self.lua_vault.read_live_text(self.current_app_id)
        if not text.strip():
            QMessageBox.warning(self, "Empty", "Live lua is empty")
            return

        variant = self.lua_vault.save_text(self.current_app_id, text)
        if variant:
            self.status_label.setText(f"Saved preset: {variant.display_label}")
            self._load_variants()
        else:
            QMessageBox.critical(self, "Error", "Failed to save preset")

    def _delete_variant(self) -> None:
        """Delete selected variant."""
        if not self.current_selection or not self.current_app_id:
            return

        reply = QMessageBox.question(
            self, "Delete Variant",
            f"Delete '{self.current_selection.display_label}'?",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No
        )
        if reply == QMessageBox.StandardButton.Yes:
            if self.lua_vault.delete(self.current_app_id, self.current_selection.hash):
                self.status_label.setText("Variant deleted")
                self._load_variants()
            else:
                QMessageBox.critical(self, "Error", "Failed to delete (may be active)")

    def _edit_live(self) -> None:
        """Start editing live lua."""
        if not self.current_app_id:
            return

        text = self.lua_vault.read_live_text(self.current_app_id)
        self.editor.setPlainText(text)
        self.editor.setReadOnly(False)

        self.edit_btn.setVisible(False)
        self.save_edit_btn.setVisible(True)
        self.cancel_edit_btn.setVisible(True)
        self.editor.setFocus()

    def _save_edit(self) -> None:
        """Save editor changes to live lua."""
        if not self.current_app_id:
            return

        text = self.editor.toPlainText()
        if self.lua_vault.write_live(self.current_app_id, text):
            self.status_label.setText("Live lua saved")
            self._cancel_edit()
            self._load_lua()
            self._load_depots()
        else:
            QMessageBox.critical(self, "Error", "Failed to save live lua")

    def _cancel_edit(self) -> None:
        """Cancel editing."""
        self.editor.setReadOnly(True)
        self.edit_btn.setVisible(True)
        self.save_edit_btn.setVisible(False)
        self.cancel_edit_btn.setVisible(False)

    def _load_depots(self) -> None:
        """Load and populate depot table."""
        if not self.current_app_id or not self.current_lua:
            self.depot_table.setRowCount(0)
            return

        self.depot_table.setRowCount(0)

        # Get depot info
        depot_info = self.depot_info.get(self.current_app_id)
        depot_map = {d.id: d for d in depot_info.depots} if depot_info else {}

        # Build rows from lua entries
        rows = []
        for entry in self.current_lua.entries:
            if not entry.has_key:
                continue

            depot = depot_map.get(entry.id)

            rows.append((entry, depot))

        self.depot_table.setRowCount(len(rows))

        for row, (entry, depot) in enumerate(rows):
            # Depot ID
            id_item = QTableWidgetItem(str(entry.id))
            id_item.setFlags(id_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.depot_table.setItem(row, 0, id_item)

            # Title
            title = entry.comment or (f"Depot {entry.id}" if not depot else
                ("Shared" if depot.is_shared else ("DLC" if depot.is_dlc else "Game Content")))
            title_item = QTableWidgetItem(title)
            title_item.setFlags(title_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.depot_table.setItem(row, 1, title_item)

            # Size
            size = entry.size_on_disk or (depot.size if depot else 0)
            size_item = QTableWidgetItem(self._format_size(size))
            size_item.setFlags(size_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            size_item.setTextAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
            self.depot_table.setItem(row, 2, size_item)

            # OS
            os_text = depot.os if depot else ""
            os_item = QTableWidgetItem(os_text)
            os_item.setFlags(os_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.depot_table.setItem(row, 3, os_item)

            # Language
            lang_text = depot.language if depot else ""
            lang_item = QTableWidgetItem(lang_text)
            lang_item.setFlags(lang_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.depot_table.setItem(row, 4, lang_item)

            # Type
            if depot:
                if depot.is_shared:
                    type_text = "Shared"
                elif depot.is_dlc:
                    type_text = "DLC"
                else:
                    type_text = "Game"
            else:
                type_text = "Unknown"
            type_item = QTableWidgetItem(type_text)
            type_item.setFlags(type_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.depot_table.setItem(row, 5, type_item)

            # Pinned
            pin_status = ""
            if entry.manifest_id:
                pin_status = f"Pinned: {entry.manifest_id}"
                if entry.manifest_id != (depot.public_manifest_id if depot else None):
                    pin_status += " ⚠ outdated"
            elif entry.commented_manifest_id:
                pin_status = f"Auto-update: {entry.commented_manifest_id}"
            pin_item = QTableWidgetItem(pin_status)
            pin_item.setFlags(pin_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.depot_table.setItem(row, 6, pin_item)

            # Actions
            actions_widget = QWidget()
            actions_layout = QHBoxLayout(actions_widget)
            actions_layout.setContentsMargins(2, 2, 2, 2)

            toggle_btn = QPushButton("Disable" if entry.manifest_id else "Enable")
            toggle_btn.setFixedWidth(80)
            toggle_btn.clicked.connect(lambda _, e=entry: self._toggle_pin(e))
            actions_layout.addWidget(toggle_btn)

            details_btn = QPushButton("Details")
            details_btn.setFixedWidth(70)
            details_btn.clicked.connect(lambda _, e=entry, d=depot: self._show_depot_details(e, d))
            actions_layout.addWidget(details_widget)

            actions_layout.addStretch()
            self.depot_table.setCellWidget(row, 7, actions_widget)

        self.depot_table.resizeColumnsToContents()

    def _format_size(self, size: int) -> str:
        for unit in ["B", "KB", "MB", "GB", "TB"]:
            if size < 1024:
                return f"{size:.1f} {unit}"
            size /= 1024
        return f"{size:.1f} PB"

    def _toggle_pin(self, entry) -> None:
        """Toggle depot pin on/off."""
        # This would modify the lua file
        # Implementation: read lua, modify, write back
        pass

    def _show_depot_details(self, entry, depot) -> None:
        """Show depot details dialog."""
        dlg = DepotDetailDialog(depot, entry, self)
        dlg.exec()

    def _on_depot_double_clicked(self, item) -> None:
        """Handle depot double-click."""
        row = item.row()
        # Could show details
        pass

    # ===== Download Picker Mode =====

    def _toggle_select_mode(self) -> None:
        """Toggle depot download picker mode."""
        if not self.current_app_id or not self.current_lua:
            return

        if not self.is_select_mode:
            self._enter_select_mode()
        else:
            self._exit_select_mode()

    def _enter_select_mode(self) -> None:
        """Enter depot download picker mode."""
        self.is_select_mode = True
        self.tab_widget.setCurrentWidget(self.picker_widget)
        self.select_depots_btn.setText("Cancel Download")

        # Build depot picks
        self._build_depot_picks()

        # Update space label
        self._update_space_label()

    def _exit_select_mode(self) -> None:
        """Exit depot download picker mode."""
        self.is_select_mode = False
        self.tab_widget.setCurrentWidget(self.table_widget)
        self.select_depots_btn.setText("Download Depots")
        self.depot_picks = []

    def _build_depot_picks(self) -> None:
        """Build depot picks from lua + depot info."""
        if not self.current_app_id or not self.current_lua:
            return

        self.depot_picks = []

        # Get depot info
        depot_info = self.depot_info.get(self.current_app_id)
        if not depot_info:
            return

        depot_map = {d.id: d for d in depot_info.depots}

        # Get keys
        keys = self.depot_downloader.resolve_keys(self.current_app_id)

        # Get Steam language
        steam_lang = self._get_steam_language()

        for entry in self.current_lua.entries:
            if not entry.has_key:
                continue

            depot = depot_map.get(entry.id)
            if not depot:
                continue

            # Determine block reason
            blocked = None
            if depot.is_shared:
                blocked = "Shared redistributable (usually system-installed)"
            elif not entry.key and entry.id not in keys:
                blocked = "Missing decryption key"
            elif entry.manifest_id is None and depot.public_manifest_id is None:
                blocked = "No manifest available"

            pick = DepotPickRow(
                depot=depot,
                lua_entry=entry,
                manifest_id=entry.manifest_id or depot.public_manifest_id,
                manifest_path=self.depot_downloader.resolve_manifest_path(entry.id, entry.manifest_id)
                    if entry.manifest_id else None,
                has_key=entry.id in keys,
                size=depot.size,
                blocked_reason=blocked,
                is_selected=not blocked and not depot.is_shared and not depot.is_dlc
            )
            self.depot_picks.append(pick)

        self._populate_picker_table()

    def _populate_picker_table(self) -> None:
        """Populate the download picker table."""
        self.picker_table.setRowCount(len(self.depot_picks))

        for row, pick in enumerate(self.depot_picks):
            # Checkbox
            checkbox = QCheckBox()
            checkbox.setChecked(pick.is_selected)
            checkbox.setEnabled(pick.blocked_reason is None)
            checkbox.stateChanged.connect(lambda state, p=pick: self._on_pick_changed(p, state))
            self.picker_table.setCellWidget(row, 0, checkbox)

            # Depot ID
            id_item = QTableWidgetItem(str(pick.depot.id))
            id_item.setFlags(id_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.picker_table.setItem(row, 1, id_item)

            # Title
            title_item = QTableWidgetItem(
                pick.depot.language or ("Shared" if pick.depot.is_shared else "Game Content"))
            title_item.setFlags(title_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.picker_table.setItem(row, 2, title_item)

            # Size
            size_item = QTableWidgetItem(self._format_size(pick.size))
            size_item.setFlags(size_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            size_item.setTextAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
            self.picker_table.setItem(row, 3, size_item)

            # OS/Lang
            os_lang = []
            if pick.depot.os:
                os_lang.append(pick.depot.os)
            if pick.depot.language:
                os_lang.append(pick.depot.language)
            oslang_item = QTableWidgetItem(", ".join(os_lang) if os_lang else "-")
            oslang_item.setFlags(oslang_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.picker_table.setItem(row, 4, oslang_item)

            # Type
            if pick.depot.is_shared:
                type_text = "Shared"
            elif pick.depot.is_dlc:
                type_text = "DLC"
            else:
                type_text = "Game"
            type_item = QTableWidgetItem(type_text)
            type_item.setFlags(type_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.picker_table.setItem(row, 5, type_item)

            # Status
            if pick.blocked_reason:
                status = f"Blocked: {pick.blocked_reason}"
            elif pick.has_key:
                status = "Ready"
            else:
                status = "No key"
            status_item = QTableWidgetItem(status)
            status_item.setFlags(status_item.flags() & ~Qt.ItemFlag.ItemIsEditable)
            if pick.blocked_reason:
                status_item.setForeground(QColor("#cc0000"))
            self.picker_table.setItem(row, 6, status_item)

        self.picker_table.resizeColumnsToContents()
        self._update_download_button()

    def _on_pick_changed(self, pick: DepotPickRow, state: int) -> None:
        """Handle pick checkbox change."""
        pick.is_selected = state == Qt.CheckState.Checked.value
        self._update_download_button()
        self._update_space_label()

    def _set_all_picks(self, selected: bool) -> None:
        """Select/deselect all available picks."""
        for pick in self.depot_picks:
            if pick.blocked_reason is None:
                pick.is_selected = selected
        self._populate_picker_table()

    def _update_download_button(self) -> None:
        """Update download button state."""
        has_selection = any(p.is_selected for p in self.depot_picks)
        self.download_btn.setEnabled(has_selection)

    def _update_space_label(self) -> None:
        """Update free space label."""
        dest = Path(self.dest_edit.text().strip())
        try:
            stat = shutil.disk_usage(dest)
            free = stat.free
            self.space_label.setText(f"{free // (1024**3)} GB free on {dest}")
        except Exception:
            self.space_label.setText("Unknown")

    def _browse_dest(self) -> None:
        """Browse for download destination."""
        dialog = QFileDialog(self)
        dialog.setFileMode(QFileDialog.FileMode.Directory)
        dialog.setOption(QFileDialog.Option.ShowDirsOnly, True)
        if dialog.exec():
            self.dest_edit.setText(dialog.selectedFiles()[0])
            self._update_space_label()

    def _start_download(self) -> None:
        """Start downloading selected depots."""
        selected = [p for p in self.depot_picks if p.is_selected]
        if not selected:
            return

        # Build selections
        selections = []
        for pick in selected:
            sel = DepotSelection(
                depot_id=pick.depot.id,
                manifest_id=pick.manifest_id,
                manifest_path=pick.manifest_path,
                size=pick.size,
                from_app_id=pick.depot.from_app_id,
                language=pick.depot.language,
                os=pick.depot.os,
                is_shared=pick.depot.is_shared,
                is_dlc=pick.depot.is_dlc
            )
            selections.append(sel)

        dest_dir = Path(self.dest_edit.text().strip())
        dest_dir.mkdir(parents=True, exist_ok=True)

        self.download_progress.setText("Starting download...")
        self.download_btn.setEnabled(False)
        self.cancel_picker_btn.setEnabled(False)

        # Run in background thread
        import threading
        self._download_thread = threading.Thread(
            target=self._run_download,
            args=(selections, dest_dir),
            daemon=True
        )
        self._download_thread.start()

    def _run_download(self, selections: list[DepotSelection], dest_dir: Path) -> None:
        """Run download in background thread."""
        results = self.depot_downloader.download_multiple(
            selections,
            dest_dir,
            progress_callback=self._download_progress_cb
        )

        # Update UI on main thread
        from PyQt6.QtCore import QMetaObject, Qt
        from PyQt6.QtCore import Q_ARG

        QMetaObject.invokeMethod(
            self, "_download_finished",
            Qt.ConnectionType.QueuedConnection,
            Q_ARG(list, results)
        )

    def _download_progress_cb(self, depot_id: str, percent: float, message: str) -> None:
        """Download progress callback."""
        from PyQt6.QtCore import QMetaObject, Qt
        QMetaObject.invokeMethod(
            self, "_update_progress",
            Qt.ConnectionType.QueuedConnection,
            Q_ARG(str, depot_id),
            Q_ARG(float, percent),
            Q_ARG(str, message)
        )

    def _update_progress(self, depot_id: str, percent: float, message: str) -> None:
        """Update progress label."""
        self.download_progress.setText(f"{depot_id}: {percent:.1f}% - {message}")

    def _download_finished(self, results: list[DepotRunResult]) -> None:
        """Handle download completion."""
        ok = sum(1 for r in results if r.ok)
        total = len(results)
        self.download_progress.setText(f"Done: {ok}/{total} succeeded")
        self.download_btn.setEnabled(True)
        self.cancel_picker_btn.setEnabled(True)

        if ok < total:
            errors = [r.error for r in results if not r.ok]
            QMessageBox.warning(self, "Partial Success",
                f"{ok}/{total} depots downloaded.\nErrors:\n" + "\n".join(errors[:5]))

    def _cancel_picker(self) -> None:
        """Cancel picker mode."""
        self._exit_select_mode()

    def _show_variant_context_menu(self, pos) -> None:
        """Show context menu for variant tree."""
        pass

    def _refresh(self) -> None:
        """Refresh all data."""
        if self.current_app_id:
            self.set_game(self.current_app_id)

    def _get_steam_language(self) -> Optional[str]:
        """Get Steam UI language from registry."""
        try:
            import subprocess
            result = subprocess.run(
                ["grep", "language", "~/.steam/steam/config/steamui.rg"],
                capture_output=True, text=True, shell=True
            )
            # Parse language from steamui.rg or steam.cfg
        except Exception:
            pass
        return "english"


def create_depots_view(
    steam_service: SteamService,
    slssteam_service: SlssteamService,
    depot_downloader: DepotDownloaderService,
    depot_info: SteamDepotInfo,
    lua_vault: LuaVault,
    millennium_service: MillenniumService,
    steam_installation: SteamInstallation
) -> DepotsView:
    """Factory function to create DepotsView."""
    return DepotsView(
        steam_service=steam_service,
        slssteam_service=slssteam_service,
        depot_downloader=depot_downloader,
        depot_info=depot_info,
        lua_vault=lua_vault,
        millennium_service=millennium_service,
        steam_installation=steam_installation
    )