#!/usr/bin/env bash
# ==============================================================================
# LuaTools Linux — Clean Uninstaller
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
    if [ "$(id -u)" -eq 0 ]; then
        rm -f /usr/local/bin/luatools \
              /usr/local/lib/luatools/libsteam_integration.so \
              /usr/local/lib/luatools/libsteam_auto_patch_linux.so \
              /usr/local/share/applications/luatools.desktop \
              /usr/local/share/icons/hicolor/scalable/apps/luatools.svg \
              "${HOME}/.config/autostart/luatools.desktop"
        if [ -n "${SUDO_USER:-}" ]; then
            rm -f "/home/${SUDO_USER}/.config/autostart/luatools.desktop"
        fi
        rmdir /usr/local/lib/luatools 2>/dev/null || true
    else
        sudo rm -f /usr/local/bin/luatools \
                   /usr/local/lib/luatools/libsteam_integration.so \
                   /usr/local/lib/luatools/libsteam_auto_patch_linux.so \
                   /usr/local/share/applications/luatools.desktop \
                   /usr/local/share/icons/hicolor/scalable/apps/luatools.svg
        rm -f "${HOME}/.config/autostart/luatools.desktop"
        sudo rmdir /usr/local/lib/luatools 2>/dev/null || true
    fi
    echo "[OK] Removed system-wide LuaTools Linux files."
else
    rm -f "${HOME}/.local/bin/luatools" \
          "${HOME}/.local/bin/luatools-steam-launcher" \
          "${HOME}/.local/lib/luatools/libsteam_integration.so" \
          "${HOME}/.local/lib/luatools/libsteam_auto_patch_linux.so" \
          "${HOME}/.local/share/applications/luatools.desktop" \
          "${HOME}/.local/share/icons/hicolor/scalable/apps/luatools.svg" \
          "${HOME}/.config/autostart/luatools.desktop"
    echo "[OK] Removed user-local LuaTools Linux binary, integration libraries, launcher, autostart entry, and icon."
fi

if [ "$PURGE_DATA" -eq 1 ]; then
    rm -rf "${HOME}/.config/luatools" \
           "${HOME}/.local/share/luatools" \
           "${HOME}/.cache/luatools"
    echo "[OK] Purged ~/.config/luatools, ~/.local/share/luatools, and ~/.cache/luatools."
else
    echo "[INFO] Preserved user vaults & settings (pass --purge to remove them)."
fi
