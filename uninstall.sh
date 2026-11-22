#!/usr/bin/env bash
# ==============================================================================
# LuaTools Linux v2.0.0 — Clean Uninstaller
# Usage: ./uninstall.sh [--system] [--purge]
# ==============================================================================
set -euo pipefail

SYSTEM_WIDE=0
PURGE_DATA=0

for arg in "$@"; do
    case "$arg" in
        --system) SYSTEM_WIDE=1 ;;
        --purge) PURGE_DATA=1 ;;
    esac
done

if [ "$SYSTEM_WIDE" -eq 1 ]; then
    sudo rm -f /usr/local/bin/luatools \
               /usr/local/share/applications/luatools.desktop \
               /usr/local/share/icons/hicolor/scalable/apps/luatools.svg
    echo "[OK] Removed system-wide LuaTools Linux files."
else
    rm -f "${HOME}/.local/bin/luatools" \
          "${HOME}/.local/share/applications/luatools.desktop" \
          "${HOME}/.local/share/icons/hicolor/scalable/apps/luatools.svg"
    echo "[OK] Removed user-local LuaTools Linux binary, launcher, and icon."
fi

if [ "$PURGE_DATA" -eq 1 ]; then
    rm -rf "${HOME}/.config/luatools" \
           "${HOME}/.local/share/luatools" \
           "${HOME}/.cache/luatools"
    echo "[OK] Purged ~/.config/luatools, ~/.local/share/luatools, and ~/.cache/luatools."
else
    echo "[INFO] Preserved user vaults & settings (pass --purge to remove them)."
fi
