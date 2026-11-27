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

# Helper function to cleanly remove Millennium plugin and unregister from config.json
clean_millennium_plugin() {
    local target_home="$1"
    local plugin_dir="${target_home}/.local/share/millennium/plugins/LuaToolsLinux"
    rm -rf "$plugin_dir"
    local millennium_cfg="${target_home}/.config/millennium/config.json"
    if [ -f "$millennium_cfg" ]; then
        local millennium_cfg_tmp="${millennium_cfg}.tmp.$$"
        if python3 -c "
import json, os, sys
src, tmp = sys.argv[1], sys.argv[2]
try:
    st = os.stat(src)
    with open(src, 'r', encoding='utf-8') as f:
        d = json.load(f)
    enabled = d.get('plugins', {}).get('enabledPlugins', [])
    if isinstance(enabled, list) and 'LuaToolsLinux' in enabled:
        enabled.remove('LuaToolsLinux')
        with open(tmp, 'w', encoding='utf-8') as f:
            json.dump(d, f, indent=2)
            f.flush()
            os.fsync(f.fileno())
        try:
            os.chmod(tmp, st.st_mode & 0o7777)
            if os.geteuid() == 0:
                os.chown(tmp, st.st_uid, st.st_gid)
        except OSError:
            pass
except Exception:
    try:
        if os.path.exists(tmp):
            os.remove(tmp)
    except OSError:
        pass
    sys.exit(1)
" "$millennium_cfg" "$millennium_cfg_tmp" 2>/dev/null; then
            if [ -f "$millennium_cfg_tmp" ]; then
                mv -f "$millennium_cfg_tmp" "$millennium_cfg"
            fi
        else
            rm -f "$millennium_cfg_tmp" 2>/dev/null || true
        fi
    fi
}

if [ "$SYSTEM_WIDE" -eq 1 ]; then
    if [ "$(id -u)" -eq 0 ]; then
        rm -f /usr/local/bin/luatools \
              /usr/local/bin/luatools-steam-launcher \
              /usr/local/lib/luatools/libsteam_integration.so \
              /usr/local/lib/luatools/libsteam_integration32.so \
              /usr/local/lib/luatools/libsteam_integration64.so \
              /usr/local/lib/luatools/libsteam_auto_patch_linux.so \
              /usr/local/share/applications/luatools.desktop \
              /usr/local/share/icons/hicolor/scalable/apps/luatools.svg \
              "${HOME}/.config/autostart/luatools.desktop"
        clean_mime_handlers "${HOME}"
        clean_millennium_plugin "${HOME}"
        if [ -n "${SUDO_USER:-}" ]; then
            sudo_home="$(getent passwd "$SUDO_USER" 2>/dev/null | cut -d: -f6 || true)"
            if [ -z "$sudo_home" ]; then
                sudo_home="/home/${SUDO_USER}"
            fi
            rm -f "${sudo_home}/.config/autostart/luatools.desktop"
            clean_mime_handlers "${sudo_home}"
            clean_millennium_plugin "${sudo_home}"
            if command -v update-desktop-database >/dev/null 2>&1; then
                sudo -u "$SUDO_USER" update-desktop-database "${sudo_home}/.local/share/applications" 2>/dev/null || true
            fi
        fi
        rm -rf /usr/local/lib/luatools/32 \
               /usr/local/lib/luatools/64 \
               /usr/local/lib/luatools/lib \
               /usr/local/lib/luatools/lib64 \
               /usr/local/lib/luatools/lib32 \
               /usr/local/lib/luatools/i386-linux-gnu \
               /usr/local/lib/luatools/x86_64-linux-gnu
        rmdir /usr/local/lib/luatools 2>/dev/null || true
        if command -v update-desktop-database >/dev/null 2>&1; then
            update-desktop-database /usr/local/share/applications 2>/dev/null || true
        fi
    else
        sudo rm -f /usr/local/bin/luatools \
                   /usr/local/bin/luatools-steam-launcher \
                   /usr/local/lib/luatools/libsteam_integration.so \
                   /usr/local/lib/luatools/libsteam_integration32.so \
                   /usr/local/lib/luatools/libsteam_integration64.so \
                   /usr/local/lib/luatools/libsteam_auto_patch_linux.so \
                   /usr/local/share/applications/luatools.desktop \
                   /usr/local/share/icons/hicolor/scalable/apps/luatools.svg
        rm -f "${HOME}/.config/autostart/luatools.desktop"
        clean_mime_handlers "${HOME}"
        clean_millennium_plugin "${HOME}"
        sudo rm -rf /usr/local/lib/luatools/32 \
                    /usr/local/lib/luatools/64 \
                    /usr/local/lib/luatools/lib \
                    /usr/local/lib/luatools/lib64 \
                    /usr/local/lib/luatools/lib32 \
                    /usr/local/lib/luatools/i386-linux-gnu \
                    /usr/local/lib/luatools/x86_64-linux-gnu
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
          "${HOME}/.local/lib/luatools/libsteam_integration32.so" \
          "${HOME}/.local/lib/luatools/libsteam_integration64.so" \
          "${HOME}/.local/lib/luatools/libsteam_auto_patch_linux.so" \
          "${HOME}/.local/share/applications/luatools.desktop" \
          "${HOME}/.local/share/icons/hicolor/scalable/apps/luatools.svg" \
          "${HOME}/.config/autostart/luatools.desktop"
    clean_mime_handlers "${HOME}"
    clean_millennium_plugin "${HOME}"
    rm -rf "${HOME}/.local/lib/luatools/32" \
           "${HOME}/.local/lib/luatools/64" \
           "${HOME}/.local/lib/luatools/lib" \
           "${HOME}/.local/lib/luatools/lib64" \
           "${HOME}/.local/lib/luatools/lib32" \
           "${HOME}/.local/lib/luatools/i386-linux-gnu" \
           "${HOME}/.local/lib/luatools/x86_64-linux-gnu"
    rmdir "${HOME}/.local/lib/luatools" 2>/dev/null || true
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
