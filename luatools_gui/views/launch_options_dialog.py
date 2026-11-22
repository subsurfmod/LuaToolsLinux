"""Launch Options Dialog - edit per-game Steam appinfo.vdf launch entries."""

from __future__ import annotations

from PyQt6.QtWidgets import (
    QComboBox,
    QDialog,
    QDialogButtonBox,
    QFormLayout,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QListWidget,
    QMessageBox,
    QPushButton,
    QVBoxLayout,
)

from ..services.launch_options import LaunchEntry, LaunchOptionsService


class LaunchOptionsDialog(QDialog):
    """Edit Steam appinfo.vdf launch entries for a game."""

    def __init__(
        self,
        app_id: int,
        game_name: str,
        launch_service: LaunchOptionsService,
        parent=None,
    ) -> None:
        super().__init__(parent)
        self.app_id = app_id
        self.game_name = game_name
        self.launch_service = launch_service
        self.entries: list[LaunchEntry] = []
        self._current_idx: int = -1

        self.setWindowTitle(f"Launch Options - {game_name} ({app_id})")
        self.resize(640, 440)
        self._setup_ui()
        self._load()

    def _setup_ui(self) -> None:
        layout = QVBoxLayout(self)
        note = QLabel(
            "Note: Close Steam before saving changes to appinfo.vdf so Steam does not overwrite them."
        )
        note.setStyleSheet("color: #c62828;")
        note.setWordWrap(True)
        layout.addWidget(note)

        body = QHBoxLayout()
        left = QVBoxLayout()
        self.list_widget = QListWidget()
        self.list_widget.currentRowChanged.connect(self._on_row_changed)
        left.addWidget(self.list_widget)

        lbtns = QHBoxLayout()
        add_btn = QPushButton("Add")
        add_btn.clicked.connect(self._add_entry)
        del_btn = QPushButton("Remove")
        del_btn.clicked.connect(self._remove_entry)
        lbtns.addWidget(add_btn)
        lbtns.addWidget(del_btn)
        left.addLayout(lbtns)
        body.addLayout(left, 1)

        form = QFormLayout()
        self.exe_edit = QLineEdit()
        self.args_edit = QLineEdit()
        self.workdir_edit = QLineEdit()
        self.desc_edit = QLineEdit()
        self.type_combo = QComboBox()
        self.type_combo.setEditable(True)
        self.type_combo.addItems(["default", "option1", "option2", "config", "server", "manual"])
        self.os_edit = QLineEdit()
        self.os_edit.setPlaceholderText("e.g. linux, windows")

        form.addRow("Executable:", self.exe_edit)
        form.addRow("Arguments:", self.args_edit)
        form.addRow("Working Dir:", self.workdir_edit)
        form.addRow("Description:", self.desc_edit)
        form.addRow("Type:", self.type_combo)
        form.addRow("OS List:", self.os_edit)
        body.addLayout(form, 2)

        layout.addLayout(body)

        bottom = QHBoxLayout()
        restore_btn = QPushButton("Restore appinfo.vdf Backup")
        restore_btn.clicked.connect(self._restore_backup)
        bottom.addWidget(restore_btn)
        bottom.addStretch()

        buttons = QDialogButtonBox(
            QDialogButtonBox.StandardButton.Save | QDialogButtonBox.StandardButton.Cancel
        )
        buttons.accepted.connect(self._save)
        buttons.rejected.connect(self.reject)
        bottom.addWidget(buttons)
        layout.addLayout(bottom)

    def _load(self) -> None:
        self.entries = self.launch_service.get_launch_entries(self.app_id)
        self._refresh_list()
        if self.entries:
            self.list_widget.setCurrentRow(0)

    def _refresh_list(self) -> None:
        self.list_widget.clear()
        for i, e in enumerate(self.entries):
            label = e.description or e.executable or f"Entry {i}"
            self.list_widget.addItem(f"[{i}] {label}")

    def _flush_current(self) -> None:
        if 0 <= self._current_idx < len(self.entries):
            e = self.entries[self._current_idx]
            e.executable = self.exe_edit.text().strip()
            e.arguments = self.args_edit.text().strip()
            e.workingdir = self.workdir_edit.text().strip()
            e.description = self.desc_edit.text().strip()
            e.launch_type = self.type_combo.currentText().strip() or "default"
            e.oslist = self.os_edit.text().strip()

    def _on_row_changed(self, row: int) -> None:
        self._flush_current()
        self._current_idx = row
        if 0 <= row < len(self.entries):
            e = self.entries[row]
            self.exe_edit.setText(e.executable)
            self.args_edit.setText(e.arguments)
            self.workdir_edit.setText(e.workingdir)
            self.desc_edit.setText(e.description)
            self.type_combo.setCurrentText(e.launch_type)
            self.os_edit.setText(e.oslist)

    def _add_entry(self) -> None:
        self._flush_current()
        self.entries.append(LaunchEntry(index=str(len(self.entries))))
        self._refresh_list()
        self.list_widget.setCurrentRow(len(self.entries) - 1)

    def _remove_entry(self) -> None:
        row = self.list_widget.currentRow()
        if 0 <= row < len(self.entries):
            self.entries.pop(row)
            self._current_idx = -1
            self._refresh_list()

    def _save(self) -> None:
        self._flush_current()
        ok, msg = self.launch_service.save_launch_entries(self.app_id, self.entries)
        if ok:
            QMessageBox.information(self, "Saved", msg)
            self.accept()
        else:
            QMessageBox.critical(self, "Error", msg)

    def _restore_backup(self) -> None:
        ok, msg = self.launch_service.restore_backup()
        if ok:
            QMessageBox.information(self, "Restored", msg)
            self._load()
        else:
            QMessageBox.warning(self, "Restore Failed", msg)
