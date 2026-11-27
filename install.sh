#!/usr/bin/env bash
# ==============================================================================
# LuaTools Linux — One-Command Dependency & Native App Installer
# Supports: Fedora/RHEL (dnf), Debian/Ubuntu (apt), Arch/SteamOS (pacman),
#           openSUSE (zypper), and portable user-space installations.
# ==============================================================================
set -euo pipefail

APP_NAME="luatools"
APP_VERSION="4.0.0"
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

    local clone_dir="${HOME}/.local/share/luatools/repo"
    mkdir -p "$(dirname "$clone_dir")"
    if [ -d "${clone_dir}/.git" ]; then
        info "Updating existing repository checkout in ${clone_dir}..."
        git -C "$clone_dir" pull --ff-only || git -C "$clone_dir" pull --rebase --autostash
    else
        rm -rf "$clone_dir"
        info "Cloning ${REPO_URL} into ${clone_dir}..."
        git clone --depth 1 "$REPO_URL" "$clone_dir"
    fi
    SCRIPT_DIR="$clone_dir"
}

# ------------------------------------------------------------------------------
# 2. Locate Prebuilt Binary OR Bootstrap Rust & Compile Native Release Binary
# ------------------------------------------------------------------------------
ensure_binary() {
    local has_newer_src=0
    if [ -f "${SCRIPT_DIR}/Cargo.toml" ] && [ -x "${SCRIPT_DIR}/dist/${APP_NAME}" ]; then
        if [ -n "$(find "${SCRIPT_DIR}/Cargo.toml" "${SCRIPT_DIR}/src" "${SCRIPT_DIR}/bridge_lib" "${SCRIPT_DIR}/steam_auto_patch_linux" -newer "${SCRIPT_DIR}/dist/${APP_NAME}" 2>/dev/null | head -n 1)" ]; then
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

    export PATH="${HOME}/.cargo/bin:${PATH}"
    if ! command -v cargo >/dev/null 2>&1; then
        info "Rust toolchain (cargo) not found — bootstrapping user-local rustup..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
        # shellcheck disable=SC1091
        [ -f "${HOME}/.cargo/env" ] && source "${HOME}/.cargo/env"
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
    mkdir -p "${SCRIPT_DIR}/dist" "${SCRIPT_DIR}/dist/32" "${SCRIPT_DIR}/dist/64"
    install -m 0755 "${BIN_SOURCE}" "${SCRIPT_DIR}/dist/${APP_NAME}"

    local bridge_32="${SCRIPT_DIR}/target/i686-unknown-linux-gnu/release/libsteam_integration.so"
    local bridge_64="${SCRIPT_DIR}/target/release/libsteam_integration.so"
    [ -f "$bridge_64" ] || bridge_64="${SCRIPT_DIR}/target/debug-debug/libsteam_integration.so"

    if [ -f "$bridge_32" ]; then
        install -m 0755 "$bridge_32" "${SCRIPT_DIR}/dist/libsteam_integration.so"
        install -m 0755 "$bridge_32" "${SCRIPT_DIR}/dist/libsteam_integration32.so"
        install -m 0755 "$bridge_32" "${SCRIPT_DIR}/dist/32/libsteam_integration.so"
        ok "Staged 32-bit i686 Steam integration library -> dist/libsteam_integration.so"
    elif [ -f "$bridge_64" ]; then
        install -m 0755 "$bridge_64" "${SCRIPT_DIR}/dist/libsteam_integration.so"
    fi

    if [ -f "$bridge_64" ]; then
        install -m 0755 "$bridge_64" "${SCRIPT_DIR}/dist/libsteam_integration64.so"
        install -m 0755 "$bridge_64" "${SCRIPT_DIR}/dist/64/libsteam_integration.so"
        ok "Staged 64-bit x86_64 Steam integration library -> dist/libsteam_integration64.so"
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
            run_privileged mkdir -p "${lib_dir}/32"
            run_privileged install -m 0755 "$bridge_32" "${lib_dir}/libsteam_integration.so"
            run_privileged install -m 0755 "$bridge_32" "${lib_dir}/libsteam_integration32.so"
            run_privileged install -m 0755 "$bridge_32" "${lib_dir}/32/libsteam_integration.so"
        elif [ -n "$bridge_64" ]; then
            run_privileged install -m 0755 "$bridge_64" "${lib_dir}/libsteam_integration.so"
        fi
        if [ -n "$bridge_64" ]; then
            run_privileged mkdir -p "${lib_dir}/64"
            run_privileged install -m 0755 "$bridge_64" "${lib_dir}/libsteam_integration64.so"
            run_privileged install -m 0755 "$bridge_64" "${lib_dir}/64/libsteam_integration.so"
        fi

        [ -n "$patch_src" ] && run_privileged install -m 0755 "$patch_src" "${lib_dir}/libsteam_auto_patch_linux.so"
        run_privileged install -m 0644 "${SCRIPT_DIR}/assets/luatools.svg" "${icon_dir}/luatools.svg"

        local tmp_desktop
        tmp_desktop="$(mktemp)"
        sed "s|@EXEC_PATH@|${bin_dir}/${APP_NAME}|g" "${SCRIPT_DIR}/assets/luatools.desktop.in" > "$tmp_desktop"
        run_privileged install -m 0644 "$tmp_desktop" "${apps_dir}/luatools.desktop"
        rm -f "$tmp_desktop"
        command -v update-desktop-database >/dev/null 2>&1 && run_privileged update-desktop-database "$apps_dir" || true
        if command -v xdg-mime >/dev/null 2>&1; then
            if [ "$(id -u)" -eq 0 ] && [ -n "${SUDO_USER:-}" ]; then
                sudo -u "$SUDO_USER" xdg-mime default luatools.desktop x-scheme-handler/luatools || true
                sudo -u "$SUDO_USER" xdg-mime default luatools.desktop x-scheme-handler/bst || true
            elif [ "$(id -u)" -ne 0 ]; then
                xdg-mime default luatools.desktop x-scheme-handler/luatools || true
                xdg-mime default luatools.desktop x-scheme-handler/bst || true
            fi
        fi
        INSTALLED_BIN="${bin_dir}/${APP_NAME}"
    else
        local bin_dir="${HOME}/.local/bin"
        local lib_dir="${HOME}/.local/lib/luatools"
        local apps_dir="${HOME}/.local/share/applications"
        local icon_dir="${HOME}/.local/share/icons/hicolor/scalable/apps"
        info "Installing for current user into ~/.local..."
        mkdir -p "$bin_dir" "$lib_dir" "$apps_dir" "$icon_dir" \
                 "${HOME}/.config/luatools" \
                 "${HOME}/.local/share/luatools/luavault" \
                 "${HOME}/.local/share/luatools/tools" \
                 "${HOME}/.cache/luatools"

        install -m 0755 "$BIN_SOURCE" "${bin_dir}/${APP_NAME}"
        local launcher_src="${SCRIPT_DIR}/assets/luatools-steam-launcher"
        [ -f "$launcher_src" ] && install -m 0755 "$launcher_src" "${bin_dir}/luatools-steam-launcher"

        # Shared libraries (32-bit primary for Steam + 64-bit for multilib/$LIB)
        if [ -n "$bridge_32" ]; then
            mkdir -p "${lib_dir}/32"
            install -m 0755 "$bridge_32" "${lib_dir}/libsteam_integration.so"
            install -m 0755 "$bridge_32" "${lib_dir}/libsteam_integration32.so"
            install -m 0755 "$bridge_32" "${lib_dir}/32/libsteam_integration.so"
        elif [ -n "$bridge_64" ]; then
            install -m 0755 "$bridge_64" "${lib_dir}/libsteam_integration.so"
        fi
        if [ -n "$bridge_64" ]; then
            mkdir -p "${lib_dir}/64"
            install -m 0755 "$bridge_64" "${lib_dir}/libsteam_integration64.so"
            install -m 0755 "$bridge_64" "${lib_dir}/64/libsteam_integration.so"
        fi

        [ -n "$patch_src" ] && install -m 0755 "$patch_src" "${lib_dir}/libsteam_auto_patch_linux.so"
        install -m 0644 "${SCRIPT_DIR}/assets/luatools.svg" "${icon_dir}/luatools.svg"
        sed "s|@EXEC_PATH@|${bin_dir}/${APP_NAME}|g" "${SCRIPT_DIR}/assets/luatools.desktop.in" > "${apps_dir}/luatools.desktop"
        chmod 0644 "${apps_dir}/luatools.desktop"

        if command -v xdg-mime >/dev/null 2>&1; then
            if [ "$(id -u)" -eq 0 ] && [ -n "${SUDO_USER:-}" ]; then
                sudo -u "$SUDO_USER" xdg-mime default luatools.desktop x-scheme-handler/luatools || true
                sudo -u "$SUDO_USER" xdg-mime default luatools.desktop x-scheme-handler/bst || true
            else
                xdg-mime default luatools.desktop x-scheme-handler/luatools || true
                xdg-mime default luatools.desktop x-scheme-handler/bst || true
            fi
        fi
        if command -v update-desktop-database >/dev/null 2>&1; then
            update-desktop-database "$apps_dir" >/dev/null 2>&1 || true
        fi
        INSTALLED_BIN="${bin_dir}/${APP_NAME}"
    fi

    # Record source repository location for one-click in-app Git updates
    if [ "$(id -u)" -ne 0 ]; then
        local meta_dir="${HOME}/.local/share/luatools"
        mkdir -p "$meta_dir"
        local git_origin=""
        if command -v git >/dev/null 2>&1 && [ -d "${SCRIPT_DIR}/.git" ]; then
            git_origin="$(git -C "${SCRIPT_DIR}" remote get-url origin 2>/dev/null || true)"
        fi
        if [ -f "${SCRIPT_DIR}/Cargo.toml" ]; then
            cat > "${meta_dir}/source_repo.json" <<EOF
{
  "repo_dir": "${SCRIPT_DIR}",
  "remote_url": "${git_origin}"
}
EOF
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
