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

# Helper function to remove registered MIME scheme handlers for luatools.desktop
clean_mime_handlers() {
    local target_home="$1"
    local mimeapps_paths=(
        "${target_home}/.config/mimeapps.list"
        "${target_home}/.local/share/applications/mimeapps.list"
    )
    for mime_file in "${mimeapps_paths[@]}"; do
        if [ -f "$mime_file" ]; then
            sed -i \
                -e '/^x-scheme-handler\/luatools=.*luatools\.desktop/d' \
                -e '/^x-scheme-handler\/bst=.*luatools\.desktop/d' \
                -e 's/luatools\.desktop;//g' \
                -e 's/;luatools\.desktop//g' \
                -e 's/luatools\.desktop//g' \
                "$mime_file" 2>/dev/null || true
        fi
    done
}

if [ "$SYSTEM_WIDE" -eq 1 ]; then
    if [ "$(id -u)" -eq 0 ]; then
        rm -f /usr/local/bin/luatools \
              /usr/local/lib/luatools/libsteam_integration.so \
              /usr/local/lib/luatools/libsteam_auto_patch_linux.so \
              /usr/local/share/applications/luatools.desktop \
              /usr/local/share/icons/hicolor/scalable/apps/luatools.svg \
              "${HOME}/.config/autostart/luatools.desktop"
        clean_mime_handlers "${HOME}"
        if [ -n "${SUDO_USER:-}" ]; then
            rm -f "/home/${SUDO_USER}/.config/autostart/luatools.desktop"
            clean_mime_handlers "/home/${SUDO_USER}"
            if command -v update-desktop-database >/dev/null 2>&1; then
                sudo -u "$SUDO_USER" update-desktop-database "/home/${SUDO_USER}/.local/share/applications" 2>/dev/null || true
            fi
        fi
        rmdir /usr/local/lib/luatools 2>/dev/null || true
        if command -v update-desktop-database >/dev/null 2>&1; then
            update-desktop-database /usr/local/share/applications 2>/dev/null || true
        fi
    else
        sudo rm -f /usr/local/bin/luatools \
                   /usr/local/lib/luatools/libsteam_integration.so \
                   /usr/local/lib/luatools/libsteam_auto_patch_linux.so \
                   /usr/local/share/applications/luatools.desktop \
                   /usr/local/share/icons/hicolor/scalable/apps/luatools.svg
        rm -f "${HOME}/.config/autostart/luatools.desktop"
        clean_mime_handlers "${HOME}"
        sudo rmdir /usr/local/lib/luatools 2>/dev/null || true
        if command -v update-desktop-database >/dev/null 2>&1; then
            sudo update-desktop-database /usr/local/share/applications 2>/dev/null || true
            update-desktop-database "${HOME}/.local/share/applications" 2>/dev/null || true
        fi
    fi
    echo "[OK] Removed system-wide LuaTools Linux files, autostart entries, and xdg-mime associations."
else
    rm -f "${HOME}/.local/bin/luatools" \
          "${HOME}/.local/bin/luatools-steam-launcher" \
          "${HOME}/.local/lib/luatools/libsteam_integration.so" \
          "${HOME}/.local/lib/luatools/libsteam_auto_patch_linux.so" \
          "${HOME}/.local/share/applications/luatools.desktop" \
          "${HOME}/.local/share/icons/hicolor/scalable/apps/luatools.svg" \
          "${HOME}/.config/autostart/luatools.desktop"
    clean_mime_handlers "${HOME}"
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "${HOME}/.local/share/applications" 2>/dev/null || true
    fi
    echo "[OK] Removed user-local LuaTools Linux binary, integration libraries, launcher, autostart entry, xdg-mime associations, and icon."
fi

if [ "$PURGE_DATA" -eq 1 ]; then
    rm -rf "${HOME}/.config/luatools" \
           "${HOME}/.local/share/luatools" \
           "${HOME}/.cache/luatools"
    echo "[OK] Purged ~/.config/luatools, ~/.local/share/luatools, and ~/.cache/luatools."
else
    echo "[INFO] Preserved user vaults & settings (pass --purge to remove them)."
fi
