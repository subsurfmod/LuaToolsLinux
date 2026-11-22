"""Entry point for LuaTools Linux GUI application."""

from __future__ import annotations

import sys

from PyQt6.QtWidgets import QApplication

from .views.main_window import MainWindow


def main() -> int:
    """Run the LuaTools Linux GUI application."""
    app = QApplication(sys.argv)
    app.setApplicationName("LuaTools Linux")
    app.setApplicationVersion("1.1.0")

    window = MainWindow()
    window.show()

    for arg in sys.argv[1:]:
        if arg.lower().startswith("luatools://"):
            window.handle_protocol_url(arg)
            break

    return app.exec()


if __name__ == "__main__":
    sys.exit(main())
