#!/usr/bin/env bash
# ==============================================================================
# LuaTools Linux — One-Command Dependency & Native App Installer
# Supports: Fedora/RHEL (dnf), Debian/Ubuntu (apt), Arch/SteamOS (pacman),
#           openSUSE (zypper), and portable user-space installations.
# ==============================================================================
set -euo pipefail

APP_NAME="luatools"
APP_VERSION="4.1.6"
REPO_URL="https://github.com/subsurfmod/LuaToolsLinux.git"
SCRIPT_DIR=""
if [ -n "${BASH_SOURCE[0]:-}" ] && [ -f "${BASH_SOURCE[0]}" ]; then
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fi

AUTO_YES=0
SYSTEM_WIDE=0
SKIP_SYS_DEPS=0
WITH_WINE=0
FORCE_REBUILD=0

for arg in "$@"; do
    case "$arg" in
        -y|--yes) AUTO_YES=1 ;;
        --system) SYSTEM_WIDE=1 ;;
        --skip-deps) SKIP_SYS_DEPS=1 ;;
        --with-wine) WITH_WINE=1 ;;
        --rebuild) FORCE_REBUILD=1 ;;
        -h|--help)
            cat <<EOF
LuaTools Linux v${APP_VERSION} Installer

Usage: ./install.sh [OPTIONS]

Options:
  -y, --yes       Non-interactive mode (approve all prompts automatically)
  --system        Install binary to /usr/local/bin & desktop files to /usr/local/share
  --skip-deps     Skip OS package manager runtime dependency check
  --rebuild       Force compiling a fresh release binary from Rust source
  --with-wine     Also install Wine + .NET runtime for optional Windows tools (Steamless/SteamAutoCrack)
  -h, --help      Show this help message

Optional Desktop Tray & Notification Packages (checked/installed automatically):
  - libayatana-appindicator3 / libappindicator3 (system tray indicator icon)
  - libnotify / notify-send (desktop completion & tray notifications)
EOF
            exit 0
            ;;
    esac
done

info()  { printf "\033[1;34m[LUATOOLS]\033[0m %s\n" "$*"; }
ok()    { printf "\033[1;36m[   OK   ]\033[0m %s\n" "$*"; }
warn()  { printf "\033[1;33m[  WARN  ]\033[0m %s\n" "$*"; }
err()   { printf "\033[1;31m[ ERROR  ]\033[0m %s\n" "$*" >&2; }

# Resolve the target non-root user & home so `sudo ./install.sh` (without --system)
# never silently installs into /root/.local instead of the invoking user's ~/.local.
TARGET_USER=""
TARGET_HOME="${HOME}"
if [ "$(id -u)" -eq 0 ] && [ -n "${SUDO_USER:-}" ] && [ "${SUDO_USER}" != "root" ]; then
    resolved_home="$(getent passwd "${SUDO_USER}" 2>/dev/null | cut -d: -f6 || true)"
    if [ -n "$resolved_home" ] && [ -d "$resolved_home" ]; then
        TARGET_USER="${SUDO_USER}"
        TARGET_HOME="$resolved_home"
    fi
fi

run_privileged() {
    if [ "$(id -u)" -eq 0 ]; then
        "$@"
    elif command -v sudo >/dev/null 2>&1; then
        sudo "$@"
    else
        err "Root privileges required for: $*"
        return 1
    fi
}

run_as_target_user() {
    if [ "$(id -u)" -eq 0 ] && [ -n "$TARGET_USER" ]; then
        sudo -u "$TARGET_USER" -H "$@"
    else
        "$@"
    fi
}

# Remove any pre-existing directory/file at link destination before creating symlink
# so `ln -sfn` never nests a symlink inside an existing real directory.
safe_symlink() {
    local target="$1"
    local link_path="$2"
    rm -rf "$link_path"
    ln -sfn "$target" "$link_path"
}

safe_symlink_privileged() {
    local target="$1"
    local link_path="$2"
    run_privileged rm -rf "$link_path"
    run_privileged ln -sfn "$target" "$link_path"
}

# Render .desktop template safely without fragile sed delimiter/escape issues on paths.
render_desktop_file() {
    local tmpl="$1"
    local exec_path="$2"
    local out_path="$3"
    if command -v python3 >/dev/null 2>&1; then
        python3 -c "
import sys
tmpl, exec_path, out_path = sys.argv[1], sys.argv[2], sys.argv[3]
with open(tmpl, 'r', encoding='utf-8') as f:
    text = f.read()
with open(out_path, 'w', encoding='utf-8') as f:
    f.write(text.replace('@EXEC_PATH@', exec_path))
" "$tmpl" "$exec_path" "$out_path"
    else
        local content
        content="$(cat "$tmpl")"
        printf '%s\n' "${content//@EXEC_PATH@/$exec_path}" > "$out_path"
    fi
}

# ------------------------------------------------------------------------------
# 1. Check & Install Runtime System Dependencies (only if missing!)
# ------------------------------------------------------------------------------
check_missing_runtime_libs() {
    local missing=0
    for lib in libEGL.so.1 libGL.so.1 libX11.so.6 libxkbcommon.so.0; do
        if ! ldconfig -p 2>/dev/null | grep -q "$lib" && [ ! -e "/usr/lib64/$lib" ] && [ ! -e "/usr/lib/$lib" ] && [ ! -e "/usr/lib/x86_64-linux-gnu/$lib" ]; then
            missing=1
        fi
    done
    if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1; then
        missing=1
    fi
    if ! command -v xdg-open >/dev/null 2>&1; then
        missing=1
    fi
    if ! command -v git >/dev/null 2>&1; then
        missing=1
    fi
    echo "$missing"
}

check_optional_tray_deps() {
    local missing_tray=0
    local missing_notify=0
    if ! ldconfig -p 2>/dev/null | grep -Eq 'lib(ayatana-)?appindicator3\.so\.1' \
        && [ ! -e "/usr/lib64/libayatana-appindicator3.so.1" ] \
        && [ ! -e "/usr/lib64/libappindicator3.so.1" ] \
        && [ ! -e "/usr/lib/libayatana-appindicator3.so.1" ] \
        && [ ! -e "/usr/lib/libappindicator3.so.1" ] \
        && [ ! -e "/usr/lib/x86_64-linux-gnu/libayatana-appindicator3.so.1" ] \
        && [ ! -e "/usr/lib/x86_64-linux-gnu/libappindicator3.so.1" ]; then
        missing_tray=1
    fi
    if ! command -v notify-send >/dev/null 2>&1; then
        missing_notify=1
    fi
    echo "${missing_tray}:${missing_notify}"
}

install_system_dependencies() {
    local tray_status missing_tray missing_notify
    tray_status="$(check_optional_tray_deps)"
    missing_tray="${tray_status%%:*}"
    missing_notify="${tray_status##*:}"

    if [ "$SKIP_SYS_DEPS" -eq 1 ]; then
        info "Skipping system package manager check (--skip-deps)."
        if [ "$missing_tray" -eq 1 ] || [ "$missing_notify" -eq 1 ]; then
            warn "Optional tray/notification deps missing (libayatana-appindicator3/libappindicator3 or libnotify/notify-send). System tray icon or desktop toasts may be disabled."
        fi
        return 0
    fi

    local need_install
    need_install="$(check_missing_runtime_libs)"
    if [ "$need_install" -eq 0 ] && [ "$WITH_WINE" -eq 0 ] && [ "$missing_tray" -eq 0 ] && [ "$missing_notify" -eq 0 ]; then
        ok "All required Linux graphics, tray/notification, linker, and XDG runtime libraries are already present."
        return 0
    fi

    info "Installing missing system dependencies via native package manager..."
    if command -v dnf >/dev/null 2>&1; then
        local pkgs=(gcc glibc-devel.i686 git mesa-libEGL mesa-libGL libwayland-client libxkbcommon libX11 libXcursor libXi libXrandr fontconfig xdg-utils zenity curl zstd bzip2 p7zip p7zip-plugins libnotify)
        [ "$WITH_WINE" -eq 1 ] && pkgs+=(wine dotnet-runtime-8.0)
        run_privileged dnf install -y "${pkgs[@]}" 2>/dev/null || run_privileged dnf install -y gcc git mesa-libEGL mesa-libGL libwayland-client libxkbcommon libX11 libXcursor libXi libXrandr fontconfig xdg-utils zenity curl zstd bzip2 p7zip p7zip-plugins libnotify
        # Optional system tray indicator library (AppIndicator3)
        if [ "$missing_tray" -eq 1 ]; then
            run_privileged dnf install -y libayatana-appindicator-gtk3 2>/dev/null \
                || run_privileged dnf install -y libappindicator-gtk3 2>/dev/null \
                || warn "Optional AppIndicator3 tray library not found in dnf repos; tray will fall back gracefully."
        fi
    elif command -v apt-get >/dev/null 2>&1; then
        local pkgs=(build-essential gcc-multilib libc6-dev-i386 git libegl1 libgl1 libwayland-client0 libxkbcommon0 libx11-6 libxcursor1 libxi6 libxrandr2 libfontconfig1 xdg-utils zenity curl ca-certificates zstd bzip2 p7zip-full libnotify-bin)
        [ "$WITH_WINE" -eq 1 ] && pkgs+=(wine)
        run_privileged apt-get update
        run_privileged apt-get install -y "${pkgs[@]}" 2>/dev/null || run_privileged apt-get install -y build-essential git libegl1 libgl1 libwayland-client0 libxkbcommon0 libx11-6 libxcursor1 libxi6 libxrandr2 libfontconfig1 xdg-utils zenity curl ca-certificates zstd bzip2 p7zip-full libnotify-bin
        if [ "$missing_tray" -eq 1 ]; then
            run_privileged apt-get install -y libayatana-appindicator3-1 2>/dev/null \
                || run_privileged apt-get install -y libappindicator3-1 2>/dev/null \
                || warn "Optional AppIndicator3 tray library not found in apt repos; tray will fall back gracefully."
        fi
    elif command -v pacman >/dev/null 2>&1; then
        local pkgs=(base-devel lib32-glibc git mesa wayland libxkbcommon libx11 libxcursor libxi libxrandr fontconfig xdg-utils zenity curl zstd bzip2 7zip libnotify)
        [ "$WITH_WINE" -eq 1 ] && pkgs+=(wine dotnet-runtime)
        run_privileged pacman -Sy --needed --noconfirm "${pkgs[@]}" 2>/dev/null || run_privileged pacman -Sy --needed --noconfirm base-devel git mesa wayland libxkbcommon libx11 libxcursor libxi libxrandr fontconfig xdg-utils zenity curl zstd bzip2 7zip libnotify
        if [ "$missing_tray" -eq 1 ]; then
            run_privileged pacman -S --needed --noconfirm libayatana-appindicator 2>/dev/null \
                || run_privileged pacman -S --needed --noconfirm libappindicator-gtk3 2>/dev/null \
                || warn "Optional AppIndicator3 tray library not found in pacman repos; tray will fall back gracefully."
        fi
    elif command -v zypper >/dev/null 2>&1; then
        local pkgs=(gcc glibc-devel-32bit gcc-32bit git Mesa-libEGL1 Mesa-libGL1 libwayland-client0 libxkbcommon0 libX11-6 fontconfig xdg-utils zenity curl zstd bzip2 7zip libnotify-tools)
        [ "$WITH_WINE" -eq 1 ] && pkgs+=(wine)
        run_privileged zypper --non-interactive install "${pkgs[@]}" 2>/dev/null || run_privileged zypper --non-interactive install gcc git Mesa-libEGL1 Mesa-libGL1 libwayland-client0 libxkbcommon0 libX11-6 fontconfig xdg-utils zenity curl zstd bzip2 7zip libnotify-tools
        if [ "$missing_tray" -eq 1 ]; then
            run_privileged zypper --non-interactive install libayatana-appindicator3-1 2>/dev/null \
                || run_privileged zypper --non-interactive install libappindicator3-1 2>/dev/null \
                || warn "Optional AppIndicator3 tray library not found in zypper repos; tray will fall back gracefully."
        fi
    else
        warn "Unrecognized package manager. Ensure git, libEGL, libGL, libwayland-client/libX11, libxkbcommon, gcc, and optional libappindicator3 + libnotify (notify-send) are installed."
    fi
    ok "System dependencies verified."
}

ensure_source_checkout() {
    if [ -n "$SCRIPT_DIR" ] && { [ -f "${SCRIPT_DIR}/Cargo.toml" ] || [ -x "${SCRIPT_DIR}/dist/${APP_NAME}" ] || [ -x "${SCRIPT_DIR}/bin/${APP_NAME}" ]; }; then
        return 0
    fi

    local clone_dir="${TARGET_HOME}/.local/share/luatools/repo"
    run_as_target_user mkdir -p "$(dirname "$clone_dir")"
    if [ -d "${clone_dir}/.git" ]; then
        info "Updating existing repository checkout in ${clone_dir}..."
        run_as_target_user git -C "$clone_dir" pull --ff-only || run_as_target_user git -C "$clone_dir" pull --rebase --autostash
    else
        rm -rf "$clone_dir"
        info "Cloning ${REPO_URL} into ${clone_dir}..."
        run_as_target_user git clone --depth 1 "$REPO_URL" "$clone_dir"
    fi
    SCRIPT_DIR="$clone_dir"
}

# ------------------------------------------------------------------------------
# 2. Locate Prebuilt Binary OR Bootstrap Rust & Compile Native Release Binary
# ------------------------------------------------------------------------------
ensure_binary() {
    local has_newer_src=0
    if [ -f "${SCRIPT_DIR}/Cargo.toml" ] && [ -x "${SCRIPT_DIR}/dist/${APP_NAME}" ]; then
        local check_paths=(
            "${SCRIPT_DIR}/Cargo.toml"
            "${SCRIPT_DIR}/src"
            "${SCRIPT_DIR}/bridge_lib"
            "${SCRIPT_DIR}/steam_auto_patch_linux"
        )
        [ -f "${SCRIPT_DIR}/build.rs" ] && check_paths+=("${SCRIPT_DIR}/build.rs")
        [ -f "${SCRIPT_DIR}/Cargo.lock" ] && check_paths+=("${SCRIPT_DIR}/Cargo.lock")
        [ -d "${SCRIPT_DIR}/assets" ] && check_paths+=("${SCRIPT_DIR}/assets")
        [ -e "${SCRIPT_DIR}/.git/HEAD" ] && check_paths+=("${SCRIPT_DIR}/.git/HEAD")
        [ -e "${SCRIPT_DIR}/.git/logs/HEAD" ] && check_paths+=("${SCRIPT_DIR}/.git/logs/HEAD")
        [ -e "${SCRIPT_DIR}/.git/refs/heads/main" ] && check_paths+=("${SCRIPT_DIR}/.git/refs/heads/main")
        if [ -n "$(find "${check_paths[@]}" -newer "${SCRIPT_DIR}/dist/${APP_NAME}" -print -quit 2>/dev/null)" ]; then
            has_newer_src=1
        fi
    fi

    if [ "$FORCE_REBUILD" -eq 0 ] && [ "$has_newer_src" -eq 0 ]; then
        if [ -x "${SCRIPT_DIR}/dist/${APP_NAME}" ]; then
            BIN_SOURCE="${SCRIPT_DIR}/dist/${APP_NAME}"
            ok "Using prebuilt native binary: ${BIN_SOURCE}"
            return 0
        fi
        if [ -x "${SCRIPT_DIR}/bin/${APP_NAME}" ]; then
            BIN_SOURCE="${SCRIPT_DIR}/bin/${APP_NAME}"
            ok "Using bundled native binary: ${BIN_SOURCE}"
            return 0
        fi
    fi

    export PATH="${TARGET_HOME}/.cargo/bin:${HOME}/.cargo/bin:${PATH}"
    if ! command -v cargo >/dev/null 2>&1; then
        info "Rust toolchain (cargo) not found — bootstrapping user-local rustup..."
        if [ "$(id -u)" -eq 0 ] && [ -n "$TARGET_USER" ]; then
            run_as_target_user sh -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable'
        else
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
            # shellcheck disable=SC1091
            [ -f "${HOME}/.cargo/env" ] && source "${HOME}/.cargo/env"
        fi
        export PATH="${TARGET_HOME}/.cargo/bin:${HOME}/.cargo/bin:${PATH}"
    fi

    ok "Rust toolchain ready: $(rustc --version)"
    if command -v rustup >/dev/null 2>&1; then
        if ! rustup target list 2>/dev/null | grep -q "i686-unknown-linux-gnu (installed)"; then
            info "Adding Rust target i686-unknown-linux-gnu for native 32-bit Steam integration..."
            rustup target add i686-unknown-linux-gnu 2>/dev/null || warn "Could not add i686-unknown-linux-gnu target via rustup"
        fi
    fi

    info "Compiling optimized native binary + 64-bit Steam integration library (LuaTools Linux v${APP_VERSION})..."
    (cd "${SCRIPT_DIR}" && cargo build --workspace --release -j "${CARGO_BUILD_JOBS:-2}")
    (cd "${SCRIPT_DIR}" && cargo build --profile debug-debug -p bridge_lib -j "${CARGO_BUILD_JOBS:-2}" || true)

    info "Compiling native 32-bit i686 Steam integration library (ubuntu12_32/steam LD_PRELOAD)..."
    (cd "${SCRIPT_DIR}" && cargo build --manifest-path bridge_lib/Cargo.toml --target i686-unknown-linux-gnu --release -j "${CARGO_BUILD_JOBS:-2}") || warn "Could not compile 32-bit i686 bridge_lib; falling back to 64-bit"

    BIN_SOURCE="${SCRIPT_DIR}/target/release/${APP_NAME}"
    mkdir -p "${SCRIPT_DIR}/dist" "${SCRIPT_DIR}/dist/32" "${SCRIPT_DIR}/dist/64" "${SCRIPT_DIR}/dist/lib" "${SCRIPT_DIR}/dist/lib64"
    install -m 0755 "${BIN_SOURCE}" "${SCRIPT_DIR}/dist/${APP_NAME}"

    local bridge_32="${SCRIPT_DIR}/target/i686-unknown-linux-gnu/release/libsteam_integration.so"
    local bridge_64="${SCRIPT_DIR}/target/release/libsteam_integration.so"
    [ -f "$bridge_64" ] || bridge_64="${SCRIPT_DIR}/target/debug-debug/libsteam_integration.so"

    if [ -f "$bridge_32" ]; then
        install -m 0755 "$bridge_32" "${SCRIPT_DIR}/dist/libsteam_integration.so"
        install -m 0755 "$bridge_32" "${SCRIPT_DIR}/dist/libsteam_integration32.so"
        install -m 0755 "$bridge_32" "${SCRIPT_DIR}/dist/32/libsteam_integration.so"
        install -m 0755 "$bridge_32" "${SCRIPT_DIR}/dist/lib/libsteam_integration.so"
        safe_symlink lib "${SCRIPT_DIR}/dist/lib32"
        safe_symlink lib "${SCRIPT_DIR}/dist/i386-linux-gnu"
        safe_symlink ../32 "${SCRIPT_DIR}/dist/lib/i386-linux-gnu"
        ok "Staged 32-bit i686 Steam integration library -> dist/libsteam_integration.so, dist/lib/ & dist/32/"
    elif [ -f "$bridge_64" ]; then
        install -m 0755 "$bridge_64" "${SCRIPT_DIR}/dist/libsteam_integration.so"
    fi

    if [ -f "$bridge_64" ]; then
        install -m 0755 "$bridge_64" "${SCRIPT_DIR}/dist/libsteam_integration64.so"
        install -m 0755 "$bridge_64" "${SCRIPT_DIR}/dist/64/libsteam_integration.so"
        install -m 0755 "$bridge_64" "${SCRIPT_DIR}/dist/lib64/libsteam_integration.so"
        safe_symlink lib64 "${SCRIPT_DIR}/dist/x86_64-linux-gnu"
        safe_symlink ../lib64 "${SCRIPT_DIR}/dist/lib/x86_64-linux-gnu"
        ok "Staged 64-bit x86_64 Steam integration library -> dist/libsteam_integration64.so, dist/lib64/ & dist/64/"
    fi

    if [ -f "${SCRIPT_DIR}/target/release/libsteam_auto_patch_linux.so" ]; then
        install -m 0755 "${SCRIPT_DIR}/target/release/libsteam_auto_patch_linux.so" "${SCRIPT_DIR}/dist/libsteam_auto_patch_linux.so"
    fi
    ok "Built stripped native binary ($(du -h "${BIN_SOURCE}" | awk '{print $1}')) -> ${BIN_SOURCE}"
}

# ------------------------------------------------------------------------------
# 3. Install Binary, Shared Libraries, Vector Icon, Desktop Launcher & URI Handler
# ------------------------------------------------------------------------------
install_files() {
    local bridge_32=""
    for candidate in \
        "${SCRIPT_DIR}/target/i686-unknown-linux-gnu/release/libsteam_integration.so" \
        "${SCRIPT_DIR}/dist/lib/libsteam_integration.so" \
        "${SCRIPT_DIR}/dist/32/libsteam_integration.so" \
        "${SCRIPT_DIR}/dist/libsteam_integration32.so" \
        "${SCRIPT_DIR}/dist/libsteam_integration.so"; do
        if [ -f "$candidate" ]; then
            if command -v file >/dev/null 2>&1; then
                if file -L "$candidate" | grep -q "ELF 32-bit"; then
                    bridge_32="$candidate"
                    break
                fi
            else
                bridge_32="$candidate"
                break
            fi
        fi
    done

    local bridge_64=""
    for candidate in \
        "${SCRIPT_DIR}/target/release/libsteam_integration.so" \
        "${SCRIPT_DIR}/target/debug-debug/libsteam_integration.so" \
        "${SCRIPT_DIR}/dist/lib64/libsteam_integration.so" \
        "${SCRIPT_DIR}/dist/64/libsteam_integration.so" \
        "${SCRIPT_DIR}/dist/libsteam_integration64.so" \
        "${SCRIPT_DIR}/dist/libsteam_integration.so"; do
        if [ -f "$candidate" ]; then
            if command -v file >/dev/null 2>&1; then
                if file -L "$candidate" | grep -q "ELF 64-bit"; then
                    bridge_64="$candidate"
                    break
                fi
            else
                bridge_64="$candidate"
                break
            fi
        fi
    done

    local patch_src=""
    for candidate in \
        "${SCRIPT_DIR}/dist/libsteam_auto_patch_linux.so" \
        "${SCRIPT_DIR}/target/release/libsteam_auto_patch_linux.so"; do
        if [ -f "$candidate" ]; then
            patch_src="$candidate"
            break
        fi
    done

    if [ "$SYSTEM_WIDE" -eq 1 ]; then
        local bin_dir="/usr/local/bin"
        local lib_dir="/usr/local/lib/luatools"
        local apps_dir="/usr/local/share/applications"
        local icon_dir="/usr/local/share/icons/hicolor/scalable/apps"
        info "Installing system-wide into /usr/local..."
        run_privileged mkdir -p "$bin_dir" "$lib_dir" "$apps_dir" "$icon_dir"
        run_privileged install -m 0755 "$BIN_SOURCE" "${bin_dir}/${APP_NAME}"
        local launcher_src="${SCRIPT_DIR}/assets/luatools-steam-launcher"
        [ -f "$launcher_src" ] && run_privileged install -m 0755 "$launcher_src" "${bin_dir}/luatools-steam-launcher"

        # Shared libraries (32-bit primary for Steam + 64-bit for multilib/$LIB)
        if [ -n "$bridge_32" ]; then
            run_privileged mkdir -p "${lib_dir}/32" "${lib_dir}/lib"
            run_privileged install -m 0755 "$bridge_32" "${lib_dir}/libsteam_integration.so"
            run_privileged install -m 0755 "$bridge_32" "${lib_dir}/libsteam_integration32.so"
            run_privileged install -m 0755 "$bridge_32" "${lib_dir}/32/libsteam_integration.so"
            run_privileged install -m 0755 "$bridge_32" "${lib_dir}/lib/libsteam_integration.so"
            safe_symlink_privileged lib "${lib_dir}/lib32" 2>/dev/null || true
            safe_symlink_privileged lib "${lib_dir}/i386-linux-gnu" 2>/dev/null || true
            safe_symlink_privileged ../32 "${lib_dir}/lib/i386-linux-gnu" 2>/dev/null || true
        elif [ -n "$bridge_64" ]; then
            run_privileged install -m 0755 "$bridge_64" "${lib_dir}/libsteam_integration.so"
        fi
        if [ -n "$bridge_64" ]; then
            run_privileged mkdir -p "${lib_dir}/64" "${lib_dir}/lib64" "${lib_dir}/lib"
            run_privileged install -m 0755 "$bridge_64" "${lib_dir}/libsteam_integration64.so"
            run_privileged install -m 0755 "$bridge_64" "${lib_dir}/64/libsteam_integration.so"
            run_privileged install -m 0755 "$bridge_64" "${lib_dir}/lib64/libsteam_integration.so"
            safe_symlink_privileged lib64 "${lib_dir}/x86_64-linux-gnu" 2>/dev/null || true
            safe_symlink_privileged ../lib64 "${lib_dir}/lib/x86_64-linux-gnu" 2>/dev/null || true
        fi

        [ -n "$patch_src" ] && run_privileged install -m 0755 "$patch_src" "${lib_dir}/libsteam_auto_patch_linux.so"
        run_privileged install -m 0644 "${SCRIPT_DIR}/assets/luatools.svg" "${icon_dir}/luatools.svg"

        local tmp_desktop
        tmp_desktop="$(mktemp)"
        render_desktop_file "${SCRIPT_DIR}/assets/luatools.desktop.in" "${bin_dir}/${APP_NAME}" "$tmp_desktop"
        run_privileged install -m 0644 "$tmp_desktop" "${apps_dir}/luatools.desktop"
        rm -f "$tmp_desktop"
        command -v update-desktop-database >/dev/null 2>&1 && run_privileged update-desktop-database "$apps_dir" || true
        if command -v xdg-mime >/dev/null 2>&1; then
            if [ "$(id -u)" -eq 0 ] && [ -n "$TARGET_USER" ]; then
                sudo -u "$TARGET_USER" -H xdg-mime default luatools.desktop x-scheme-handler/luatools || true
                sudo -u "$TARGET_USER" -H xdg-mime default luatools.desktop x-scheme-handler/bst || true
            elif [ "$(id -u)" -ne 0 ]; then
                xdg-mime default luatools.desktop x-scheme-handler/luatools || true
                xdg-mime default luatools.desktop x-scheme-handler/bst || true
            fi
        fi
        INSTALLED_BIN="${bin_dir}/${APP_NAME}"
    else
        if [ "$(id -u)" -eq 0 ] && [ -n "$TARGET_USER" ]; then
            warn "User-local install invoked via sudo without --system; installing into ${TARGET_HOME}/.local for ${TARGET_USER} instead of /root/.local."
        fi
        local user_home="${TARGET_HOME}"
        local bin_dir="${user_home}/.local/bin"
        local lib_dir="${user_home}/.local/lib/luatools"
        local apps_dir="${user_home}/.local/share/applications"
        local icon_dir="${user_home}/.local/share/icons/hicolor/scalable/apps"
        info "Installing for user into ${user_home}/.local..."
        mkdir -p "$bin_dir" "$lib_dir" "$apps_dir" "$icon_dir" \
                 "${user_home}/.config/luatools" \
                 "${user_home}/.local/share/luatools/luavault" \
                 "${user_home}/.local/share/luatools/tools" \
                 "${user_home}/.cache/luatools"

        install -m 0755 "$BIN_SOURCE" "${bin_dir}/${APP_NAME}"
        local launcher_src="${SCRIPT_DIR}/assets/luatools-steam-launcher"
        [ -f "$launcher_src" ] && install -m 0755 "$launcher_src" "${bin_dir}/luatools-steam-launcher"

        # Shared libraries (32-bit primary for Steam + 64-bit for multilib/$LIB)
        if [ -n "$bridge_32" ]; then
            mkdir -p "${lib_dir}/32" "${lib_dir}/lib"
            install -m 0755 "$bridge_32" "${lib_dir}/libsteam_integration.so"
            install -m 0755 "$bridge_32" "${lib_dir}/libsteam_integration32.so"
            install -m 0755 "$bridge_32" "${lib_dir}/32/libsteam_integration.so"
            install -m 0755 "$bridge_32" "${lib_dir}/lib/libsteam_integration.so"
            safe_symlink lib "${lib_dir}/lib32" 2>/dev/null || true
            safe_symlink lib "${lib_dir}/i386-linux-gnu" 2>/dev/null || true
            safe_symlink ../32 "${lib_dir}/lib/i386-linux-gnu" 2>/dev/null || true
        elif [ -n "$bridge_64" ]; then
            install -m 0755 "$bridge_64" "${lib_dir}/libsteam_integration.so"
        fi
        if [ -n "$bridge_64" ]; then
            mkdir -p "${lib_dir}/64" "${lib_dir}/lib64" "${lib_dir}/lib"
            install -m 0755 "$bridge_64" "${lib_dir}/libsteam_integration64.so"
            install -m 0755 "$bridge_64" "${lib_dir}/64/libsteam_integration.so"
            install -m 0755 "$bridge_64" "${lib_dir}/lib64/libsteam_integration.so"
            safe_symlink lib64 "${lib_dir}/x86_64-linux-gnu" 2>/dev/null || true
            safe_symlink ../lib64 "${lib_dir}/lib/x86_64-linux-gnu" 2>/dev/null || true
        fi

        [ -n "$patch_src" ] && install -m 0755 "$patch_src" "${lib_dir}/libsteam_auto_patch_linux.so"
        install -m 0644 "${SCRIPT_DIR}/assets/luatools.svg" "${icon_dir}/luatools.svg"
        render_desktop_file "${SCRIPT_DIR}/assets/luatools.desktop.in" "${bin_dir}/${APP_NAME}" "${apps_dir}/luatools.desktop"
        chmod 0644 "${apps_dir}/luatools.desktop"

        if [ "$(id -u)" -eq 0 ] && [ -n "$TARGET_USER" ]; then
            local target_grp
            target_grp="$(id -gn "$TARGET_USER")"
            chown -R "${TARGET_USER}:${target_grp}" \
                "${bin_dir}/${APP_NAME}" \
                "$lib_dir" \
                "${apps_dir}/luatools.desktop" \
                "${icon_dir}/luatools.svg" \
                "${user_home}/.config/luatools" \
                "${user_home}/.local/share/luatools" \
                "${user_home}/.cache/luatools" 2>/dev/null || true
            [ -f "${bin_dir}/luatools-steam-launcher" ] && chown "${TARGET_USER}:${target_grp}" "${bin_dir}/luatools-steam-launcher" 2>/dev/null || true
        fi

        if command -v xdg-mime >/dev/null 2>&1; then
            if [ "$(id -u)" -eq 0 ] && [ -n "$TARGET_USER" ]; then
                sudo -u "$TARGET_USER" -H xdg-mime default luatools.desktop x-scheme-handler/luatools || true
                sudo -u "$TARGET_USER" -H xdg-mime default luatools.desktop x-scheme-handler/bst || true
            else
                xdg-mime default luatools.desktop x-scheme-handler/luatools || true
                xdg-mime default luatools.desktop x-scheme-handler/bst || true
            fi
        fi
        if command -v update-desktop-database >/dev/null 2>&1; then
            run_as_target_user update-desktop-database "$apps_dir" >/dev/null 2>&1 || true
        fi
        INSTALLED_BIN="${bin_dir}/${APP_NAME}"
    fi

    # Resolve non-root user home for Millennium plugin & source_repo.json metadata
    local target_home=""
    local target_user=""
    if [ "$(id -u)" -ne 0 ]; then
        target_home="${HOME}"
    elif [ -n "$TARGET_USER" ]; then
        target_home="${TARGET_HOME}"
        target_user="${TARGET_USER}"
    fi

    # Install native Millennium plugin (LuaToolsLinux by subsurfmod) only for a non-root user
    # when Millennium is actually installed on the system.
    if [ -d "${SCRIPT_DIR}/assets/millennium_plugin" ]; then
        if [ -z "$target_home" ] || [ "$target_home" = "/root" ] || [ ! -d "$target_home" ]; then
            info "Skipping user Millennium plugin deployment (no non-root target home)."
        elif [ ! -d "/usr/lib/millennium" ] \
            && [ ! -d "/usr/lib64/millennium" ] \
            && [ ! -d "/usr/share/millennium" ] \
            && [ ! -d "${target_home}/.local/share/millennium" ] \
            && [ ! -d "${target_home}/.config/millennium" ] \
            && [ ! -f "${target_home}/.local/share/Steam/ubuntu12_32/libmillennium_x86.so" ] \
            && [ ! -f "${target_home}/.steam/steam/ubuntu12_32/libmillennium_x86.so" ]; then
            info "Millennium not detected; skipping LuaToolsLinux Millennium plugin deployment."
        else
            local millennium_plugins_dir="${target_home}/.local/share/millennium/plugins/LuaToolsLinux"
            info "Installing native Millennium v3.6+ plugin: LuaToolsLinux (by subsurfmod)..."
            mkdir -p "${millennium_plugins_dir}/backend" "${millennium_plugins_dir}/public" "${millennium_plugins_dir}/dist" "${millennium_plugins_dir}/.millennium/Dist"
            install -m 0644 "${SCRIPT_DIR}/assets/millennium_plugin/plugin.json" "${millennium_plugins_dir}/plugin.json"
            install -m 0644 "${SCRIPT_DIR}/assets/millennium_plugin/backend/main.lua" "${millennium_plugins_dir}/backend/main.lua"
            install -m 0644 "${SCRIPT_DIR}/assets/millennium_plugin/public/luatools.js" "${millennium_plugins_dir}/public/luatools.js"
            install -m 0644 "${SCRIPT_DIR}/assets/millennium_plugin/public/luatools.css" "${millennium_plugins_dir}/public/luatools.css"
            install -m 0644 "${SCRIPT_DIR}/assets/millennium_plugin/dist/index.js" "${millennium_plugins_dir}/dist/index.js"
            install -m 0644 "${SCRIPT_DIR}/assets/millennium_plugin/dist/index.js" "${millennium_plugins_dir}/.millennium/Dist/index.js"
            if [ -f "${SCRIPT_DIR}/assets/millennium_plugin/.millennium/Dist/webkit.js" ]; then
                install -m 0644 "${SCRIPT_DIR}/assets/millennium_plugin/.millennium/Dist/webkit.js" "${millennium_plugins_dir}/.millennium/Dist/webkit.js"
            elif [ -f "${SCRIPT_DIR}/assets/millennium_plugin/dist/webkit.js" ]; then
                install -m 0644 "${SCRIPT_DIR}/assets/millennium_plugin/dist/webkit.js" "${millennium_plugins_dir}/.millennium/Dist/webkit.js"
            fi
            if [ -n "$target_user" ] && [ "$(id -u)" -eq 0 ]; then
                chown -R "${target_user}:$(id -gn "$target_user")" "${target_home}/.local/share/millennium" 2>/dev/null || true
            fi

            local millennium_cfg="${target_home}/.config/millennium/config.json"
            local millennium_state="${target_home}/.config/luatools/millennium_plugin_state.json"
            if [ -f "$millennium_cfg" ] && command -v python3 >/dev/null 2>&1; then
                local millennium_cfg_tmp="${millennium_cfg}.tmp.$$"
                if python3 -c "
import json, os, sys
src, tmp, state_path = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    if state_path and os.path.isfile(state_path):
        with open(state_path, 'r', encoding='utf-8') as sf:
            st = json.load(sf)
        if isinstance(st, dict) and st.get('user_opted_out') is True:
            sys.exit(0)
except Exception:
    pass
try:
    with open(src, 'r', encoding='utf-8') as f:
        d = json.load(f)
except Exception:
    d = {}
if not isinstance(d, dict):
    d = {}
plugins = d.get('plugins')
if not isinstance(plugins, dict):
    plugins = {}
    d['plugins'] = plugins
enabled = plugins.get('enabledPlugins')
if not isinstance(enabled, list):
    enabled = []
    plugins['enabledPlugins'] = enabled
if 'LuaToolsLinux' not in enabled:
    enabled.append('LuaToolsLinux')
gen = d.get('general')
if not isinstance(gen, dict):
    gen = {}
    d['general'] = gen
gen['injectJavascript'] = True
gen['injectCSS'] = True
with open(tmp, 'w', encoding='utf-8') as f:
    json.dump(d, f, indent=2)
    f.flush()
    os.fsync(f.fileno())
" "$millennium_cfg" "$millennium_cfg_tmp" "$millennium_state" 2>/dev/null; then
                    if [ -f "$millennium_cfg_tmp" ]; then
                        if [ -n "$target_user" ] && [ "$(id -u)" -eq 0 ]; then
                            chown "${target_user}:$(id -gn "$target_user")" "$millennium_cfg_tmp" 2>/dev/null || true
                        fi
                        mv -f "$millennium_cfg_tmp" "$millennium_cfg"
                    fi
                else
                    rm -f "$millennium_cfg_tmp"
                fi
            fi
            ok "Installed Millennium plugin -> ${millennium_plugins_dir}"
        fi
    fi

    # Record source repository location for one-click in-app Git updates (safely JSON-escaped,
    # and written for $SUDO_USER under --system as well).
    if [ -n "$target_home" ] && [ "$target_home" != "/root" ] && [ -d "$target_home" ] && [ -f "${SCRIPT_DIR}/Cargo.toml" ]; then
        local meta_dir="${target_home}/.local/share/luatools"
        mkdir -p "$meta_dir"
        local git_origin=""
        if command -v git >/dev/null 2>&1 && { [ -d "${SCRIPT_DIR}/.git" ] || [ -f "${SCRIPT_DIR}/.git" ]; }; then
            git_origin="$(git -C "${SCRIPT_DIR}" remote get-url origin 2>/dev/null || true)"
        fi
        local meta_file="${meta_dir}/source_repo.json"
        local meta_tmp="${meta_file}.tmp.$$"
        if command -v python3 >/dev/null 2>&1; then
            python3 -c "
import json, os, sys
tmp, repo_dir, remote_url = sys.argv[1], sys.argv[2], sys.argv[3]
with open(tmp, 'w', encoding='utf-8') as f:
    json.dump({'repo_dir': repo_dir, 'remote_url': remote_url}, f, indent=2)
    f.write('\n')
    f.flush()
    os.fsync(f.fileno())
" "$meta_tmp" "$SCRIPT_DIR" "$git_origin"
            mv -f "$meta_tmp" "$meta_file"
        else
            local esc_dir="${SCRIPT_DIR//\\/\\\\}"
            esc_dir="${esc_dir//\"/\\\"}"
            local esc_origin="${git_origin//\\/\\\\}"
            esc_origin="${esc_origin//\"/\\\"}"
            printf '{\n  "repo_dir": "%s",\n  "remote_url": "%s"\n}\n' "$esc_dir" "$esc_origin" > "$meta_file"
        fi
        if [ -n "$target_user" ] && [ "$(id -u)" -eq 0 ]; then
            chown -R "${target_user}:$(id -gn "$target_user")" "$meta_dir" 2>/dev/null || true
        fi
    fi

    ok "Installed executable   -> ${INSTALLED_BIN}"
    ok "Registered URI handler -> luatools://"
}

main() {
    info "Starting LuaTools Linux v${APP_VERSION} setup..."
    install_system_dependencies
    ensure_source_checkout
    ensure_binary
    install_files

    printf "\n\033[1;34m==============================================================\033[0m\n"
    printf " \033[1;37mLUATOOLS // LINUX v%s INSTALLED SUCCESSFULLY\033[0m\n" "$APP_VERSION"
    printf " Launch from your app menu or run: \033[1;36m%s\033[0m\n" "$INSTALLED_BIN"
    printf "\033[1;34m==============================================================\033[0m\n"
}

main "$@"
