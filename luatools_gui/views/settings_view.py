"""Settings view - Account, Hubcap, LuaTools behavior, SLSsteam, Millennium & CEF CDP."""

from __future__ import annotations

import threading
from typing import Optional

from PyQt6.QtCore import pyqtSignal
from PyQt6.QtWidgets import (
    QCheckBox,
    QFileDialog,
    QGroupBox,
    QHBoxLayout,
    QInputDialog,
    QLabel,
    QLineEdit,
    QMessageBox,
    QPushButton,
    QScrollArea,
    QVBoxLayout,
    QWidget,
)

from ..services.api_client import HubcapService, LuaToolsApiClient
from ..services.auth_service import AuthService
from ..services.cef_injector import CefInjectorService
from ..services.millennium_service import MillenniumService
from ..services.protocol_service import ProtocolService
from ..services.settings_service import SettingsService
from ..services.slssteam_service import SlssteamService


class SettingsView(QWidget):
    """Comprehensive settings view for LuaTools Linux."""

    _auth_done = pyqtSignal(bool, str)
    _usage_done = pyqtSignal(str)
    _hubcap_done = pyqtSignal(str)

    def __init__(
        self,
        slssteam_service: SlssteamService,
        millennium_service: MillenniumService,
        auth_service: Optional[AuthService] = None,
        settings_service: Optional[SettingsService] = None,
        api_client: Optional[LuaToolsApiClient] = None,
        hubcap_service: Optional[HubcapService] = None,
        cef_injector: Optional[CefInjectorService] = None,
    ) -> None:
        super().__init__()
        self.slssteam_service = slssteam_service
        self.millennium_service = millennium_service
        self.auth_service = auth_service
        self.settings_service = settings_service
        self.api_client = api_client
        self.hubcap_service = hubcap_service
        self.cef_injector = cef_injector

        self._auth_done.connect(self._on_auth_done)
        self._usage_done.connect(lambda txt: self.usage_lbl.setText(txt))
        self._hubcap_done.connect(lambda txt: self.hubcap_status_lbl.setText(txt))

        self._setup_ui()
        self._load_values()

    def _setup_ui(self) -> None:
        outer = QVBoxLayout(self)
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        outer.addWidget(scroll)

        container = QWidget()
        layout = QVBoxLayout(container)
        scroll.setWidget(container)

        # 1. Account (Discord OAuth / Bot Code)
        auth_group = QGroupBox("LuaTools Account (Discord)")
        auth_l = QVBoxLayout(auth_group)
        self.auth_lbl = QLabel("Guest (not signed in)")
        self.usage_lbl = QLabel("")
        self.usage_lbl.setStyleSheet("color: #666;")
        auth_l.addWidget(self.auth_lbl)
        auth_l.addWidget(self.usage_lbl)

        auth_btns = QHBoxLayout()
        self.browser_signin_btn = QPushButton("Sign In with Discord (Browser)")
        self.browser_signin_btn.clicked.connect(self._sign_in_browser)
        auth_btns.addWidget(self.browser_signin_btn)

        self.code_signin_btn = QPushButton("Sign In with 6-Char Bot Code")
        self.code_signin_btn.clicked.connect(self._sign_in_code)
        auth_btns.addWidget(self.code_signin_btn)

        self.signout_btn = QPushButton("Sign Out")
        self.signout_btn.clicked.connect(self._sign_out)
        auth_btns.addWidget(self.signout_btn)
        auth_btns.addStretch()
        auth_l.addLayout(auth_btns)
        layout.addWidget(auth_group)

        # 2. Hubcap API Key
        hc_group = QGroupBox("Hubcap (hubcapmanifest.com)")
        hc_l = QVBoxLayout(hc_group)
        hc_row = QHBoxLayout()
        hc_row.addWidget(QLabel("API Key:"))
        self.hubcap_edit = QLineEdit()
        self.hubcap_edit.setPlaceholderText("smm_...")
        self.hubcap_edit.setEchoMode(QLineEdit.EchoMode.Password)
        hc_row.addWidget(self.hubcap_edit)
        hc_check_btn = QPushButton("Check Key & Stats")
        hc_check_btn.clicked.connect(self._check_hubcap)
        hc_row.addWidget(hc_check_btn)
        hc_l.addLayout(hc_row)
        self.hubcap_status_lbl = QLabel("")
        self.hubcap_status_lbl.setStyleSheet("color: #666;")
        hc_l.addWidget(self.hubcap_status_lbl)
        layout.addWidget(hc_group)

        # 3. General LuaTools Behavior
        beh_group = QGroupBox("LuaTools Behavior")
        beh_l = QVBoxLayout(beh_group)
        self.auto_update_cb = QCheckBox("Auto-Update Apps (comment out setManifestid pins on install)")
        self.fast_fetch_cb = QCheckBox("Enable FastFetch (auto-pick first available source)")
        self.donate_keys_cb = QCheckBox("Donate new depot decryption keys from config.vdf")
        beh_l.addWidget(self.auto_update_cb)
        beh_l.addWidget(self.fast_fetch_cb)
        beh_l.addWidget(self.donate_keys_cb)
        layout.addWidget(beh_group)

        # 4. SLSsteam settings
        sls_group = QGroupBox("SLSsteam Configuration")
        sls_layout = QVBoxLayout(sls_group)

        self.play_not_owned_cb = QCheckBox("Allow playing not-owned games (playNotOwnedGames)")
        sls_layout.addWidget(self.play_not_owned_cb)

        self.limit_downloads_cb = QCheckBox("Limit downloads to Steam Library")
        sls_layout.addWidget(self.limit_downloads_cb)

        workshop_layout = QHBoxLayout()
        workshop_layout.addWidget(QLabel("Workshop Tool Path:"))
        self.workshop_edit = QLineEdit()
        self.workshop_edit.setPlaceholderText("/path/to/DepotDownloaderMod")
        workshop_layout.addWidget(self.workshop_edit)
        browse_btn = QPushButton("Browse")
        browse_btn.clicked.connect(self._browse_workshop)
        workshop_layout.addWidget(browse_btn)
        sls_layout.addLayout(workshop_layout)
        layout.addWidget(sls_group)

        # 5. Steam Integration (Millennium + CEF CDP + Protocol)
        mill_group = QGroupBox("Steam Integration (Millennium, CEF CDP & Protocol)")
        mill_layout = QVBoxLayout(mill_group)

        self.millennium_status = QLabel("Checking Millennium...")
        mill_layout.addWidget(self.millennium_status)

        self.cef_cb = QCheckBox("Enable CEF CDP Store Injector (port 8080 -> 6767)")
        mill_layout.addWidget(self.cef_cb)

        btn_row = QHBoxLayout()
        check_btn = QPushButton("Check Millennium Status")
        check_btn.clicked.connect(self._check_millennium)
        btn_row.addWidget(check_btn)

        enable_cdp_btn = QPushButton("Enable Steam CEF Debugging")
        enable_cdp_btn.clicked.connect(self._enable_cef_debug)
        btn_row.addWidget(enable_cdp_btn)

        proto_btn = QPushButton("Register luatools:// Protocol")
        proto_btn.clicked.connect(self._register_protocol)
        btn_row.addWidget(proto_btn)

        restart_btn = QPushButton("Restart Steam")
        restart_btn.clicked.connect(self._restart_steam)
        btn_row.addWidget(restart_btn)
        mill_layout.addLayout(btn_row)
        layout.addWidget(mill_group)

        # Save button
        save_btn = QPushButton("Save All Settings")
        save_btn.clicked.connect(self._save)
        save_btn.setStyleSheet("font-weight: bold; padding: 8px;")
        layout.addWidget(save_btn)
        layout.addStretch()

    def _load_values(self) -> None:
        self.play_not_owned_cb.setChecked(self.slssteam_service.config.play_not_owned)
        self.limit_downloads_cb.setChecked(self.slssteam_service.config.limit_downloads)
        self.workshop_edit.setText(self.slssteam_service.config.workshop_path)

        if self.settings_service:
            s = self.settings_service.settings
            self.hubcap_edit.setText(s.hubcap_api_key)
            self.auto_update_cb.setChecked(s.auto_update_apps)
            self.fast_fetch_cb.setChecked(s.fast_fetch)
            self.donate_keys_cb.setChecked(s.donate_keys)
            self.cef_cb.setChecked(s.cef_injector_enabled)

        self._refresh_auth_ui()

    def _refresh_auth_ui(self) -> None:
        if not self.auth_service:
            return
        if self.auth_service.is_signed_in:
            self.auth_lbl.setText(f"Signed in as: {self.auth_service.display_name} ({self.auth_service.email})")
            self.auth_lbl.setStyleSheet("color: #2e7d32; font-weight: bold;")
            self.signout_btn.setEnabled(True)
            if self.api_client:
                def fetch_usage():
                    u = self.api_client.get_standard_usage()
                    sup = self.api_client.get_supporter_status()
                    parts = []
                    if u:
                        parts.append(f"Daily Downloads: {u[0]} / {u[1]}")
                    if sup is not None:
                        parts.append(f"Supporter: {'Yes' if sup else 'No'}")
                    self._usage_done.emit(" · ".join(parts))

                threading.Thread(target=fetch_usage, daemon=True).start()
        else:
            self.auth_lbl.setText("Guest (not signed in)")
            self.auth_lbl.setStyleSheet("color: #666;")
            self.usage_lbl.setText("")
            self.signout_btn.setEnabled(False)

    def _sign_in_browser(self) -> None:
        if not self.auth_service:
            return
        self.browser_signin_btn.setEnabled(False)
        self.auth_lbl.setText("Waiting for Discord OAuth in browser...")

        def worker():
            try:
                sess = self.auth_service.sign_in_with_browser()
                self._auth_done.emit(True, f"Signed in as {sess.display_name}")
            except Exception as exc:
                self._auth_done.emit(False, str(exc))

        threading.Thread(target=worker, daemon=True).start()

    def _sign_in_code(self) -> None:
        if not self.auth_service:
            return
        code, ok = QInputDialog.getText(
            self, "Discord Bot Code", "Enter the 6-character code from Discord /login:"
        )
        if not ok or not code.strip():
            return

        def worker():
            try:
                sess = self.auth_service.sign_in_with_code(code)
                self._auth_done.emit(True, f"Signed in as {sess.display_name}")
            except Exception as exc:
                self._auth_done.emit(False, str(exc))

        threading.Thread(target=worker, daemon=True).start()

    def _on_auth_done(self, ok: bool, msg: str) -> None:
        self.browser_signin_btn.setEnabled(True)
        self._refresh_auth_ui()
        if ok:
            QMessageBox.information(self, "Signed In", msg)
        else:
            QMessageBox.critical(self, "Sign-In Failed", msg)

    def _sign_out(self) -> None:
        if self.auth_service:
            self.auth_service.sign_out()
            self._refresh_auth_ui()

    def _check_hubcap(self) -> None:
        if not self.hubcap_service:
            return
        key = self.hubcap_edit.text().strip()
        if not self.hubcap_service.is_valid_key_format(key):
            self.hubcap_status_lbl.setText("Invalid format (expected smm_ + 96 hex chars)")
            return
        self.hubcap_status_lbl.setText("Checking Hubcap stats...")

        def worker():
            st = self.hubcap_service.get_stats(key)
            if st:
                self._hubcap_done.emit(
                    f"Valid key! Usage: {st.daily_usage}/{st.daily_limit} · Can request: {st.can_make_requests}"
                )
            else:
                self._hubcap_done.emit("Could not verify Hubcap key.")

        threading.Thread(target=worker, daemon=True).start()

    def _browse_workshop(self) -> None:
        path, _ = QFileDialog.getOpenFileName(
            self, "Select DepotDownloaderMod", "", "Executables (*)"
        )
        if path:
            self.workshop_edit.setText(path)

    def _check_millennium(self) -> None:
        if self.millennium_service.is_available():
            version = self.millennium_service.get_version()
            steam_path = self.millennium_service.get_steam_path()
            self.millennium_status.setText(
                f"Millennium: Connected (v{version or 'unknown'})\nSteam path: {steam_path or 'unknown'}"
            )
            self.millennium_status.setStyleSheet("color: #00aa00;")
        else:
            self.millennium_status.setText("Millennium: Not available (is Steam running with Millennium?)")
            self.millennium_status.setStyleSheet("color: #cc0000;")

    def _enable_cef_debug(self) -> None:
        if not self.cef_injector:
            QMessageBox.warning(self, "Not Ready", "Steam not detected yet.")
            return
        ok, msg = self.cef_injector.ensure_cef_debugging_enabled()
        QMessageBox.information(self, "CEF Debugging", msg) if ok else QMessageBox.warning(self, "Error", msg)

    def _register_protocol(self) -> None:
        ok, msg = ProtocolService.register_desktop_handler()
        QMessageBox.information(self, "Protocol Handler", msg) if ok else QMessageBox.warning(self, "Error", msg)

    def _restart_steam(self) -> None:
        result = self.millennium_service.call("restart_steam")
        if result.get("success"):
            self.millennium_status.setText("Restarting Steam...")
        else:
            QMessageBox.warning(
                self, "Error", f"Could not restart Steam: {result.get('error', 'unknown')}"
            )

    def _save(self) -> None:
        self.slssteam_service.set_play_not_owned(self.play_not_owned_cb.isChecked())
        self.slssteam_service.set_limit_downloads(self.limit_downloads_cb.isChecked())
        self.slssteam_service.set_workshop_path(self.workshop_edit.text().strip())
        sls_ok = self.slssteam_service.save()

        app_ok = True
        if self.settings_service:
            s = self.settings_service.settings
            s.hubcap_api_key = self.hubcap_edit.text().strip()
            s.auto_update_apps = self.auto_update_cb.isChecked()
            s.fast_fetch = self.fast_fetch_cb.isChecked()
            s.donate_keys = self.donate_keys_cb.isChecked()
            s.cef_injector_enabled = self.cef_cb.isChecked()
            app_ok = self.settings_service.save()

            if self.cef_injector:
                if s.cef_injector_enabled:
                    self.cef_injector.start()
                else:
                    self.cef_injector.stop()

        if sls_ok and app_ok:
            QMessageBox.information(self, "Saved", "All settings saved successfully.")
        else:
            QMessageBox.critical(self, "Error", "Failed to save one or more configuration files.")
