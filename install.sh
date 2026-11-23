#!/usr/bin/env bash
# ==============================================================================
# LuaTools Linux v2.0.0 — One-Command Dependency & Native App Installer
# Supports: Fedora/RHEL (dnf), Debian/Ubuntu (apt), Arch/SteamOS (pacman),
#           openSUSE (zypper), and portable user-space installations.
# ==============================================================================
set -euo pipefail

APP_NAME="luatools"
APP_VERSION="2.3.1"
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

install_system_dependencies() {
    if [ "$SKIP_SYS_DEPS" -eq 1 ]; then
        info "Skipping system package manager check (--skip-deps)."
        return 0
    fi

    local need_install
    need_install="$(check_missing_runtime_libs)"
    if [ "$need_install" -eq 0 ] && [ "$WITH_WINE" -eq 0 ]; then
        ok "All required Linux graphics, linker, and XDG runtime libraries are already present."
        return 0
    fi

    info "Installing missing system dependencies via native package manager..."
    if command -v dnf >/dev/null 2>&1; then
        local pkgs=(gcc git mesa-libEGL mesa-libGL libwayland-client libxkbcommon libX11 libXcursor libXi libXrandr fontconfig xdg-utils zenity curl)
        [ "$WITH_WINE" -eq 1 ] && pkgs+=(wine dotnet-runtime-8.0)
        run_privileged dnf install -y "${pkgs[@]}"
    elif command -v apt-get >/dev/null 2>&1; then
        local pkgs=(build-essential git libegl1 libgl1 libwayland-client0 libxkbcommon0 libx11-6 libxcursor1 libxi6 libxrandr2 libfontconfig1 xdg-utils zenity curl ca-certificates)
        [ "$WITH_WINE" -eq 1 ] && pkgs+=(wine)
        run_privileged apt-get update
        run_privileged apt-get install -y "${pkgs[@]}"
    elif command -v pacman >/dev/null 2>&1; then
        local pkgs=(base-devel git mesa wayland libxkbcommon libx11 libxcursor libxi libxrandr fontconfig xdg-utils zenity curl)
        [ "$WITH_WINE" -eq 1 ] && pkgs+=(wine dotnet-runtime)
        run_privileged pacman -Sy --needed --noconfirm "${pkgs[@]}"
    elif command -v zypper >/dev/null 2>&1; then
        local pkgs=(gcc git Mesa-libEGL1 Mesa-libGL1 libwayland-client0 libxkbcommon0 libX11-6 fontconfig xdg-utils zenity curl)
        [ "$WITH_WINE" -eq 1 ] && pkgs+=(wine)
        run_privileged zypper --non-interactive install "${pkgs[@]}"
    else
        warn "Unrecognized package manager. Ensure git, libEGL, libGL, libwayland-client/libX11, libxkbcommon, and gcc are installed."
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
        if [ -n "$(find "${SCRIPT_DIR}/Cargo.toml" "${SCRIPT_DIR}/src" -newer "${SCRIPT_DIR}/dist/${APP_NAME}" 2>/dev/null | head -n 1)" ]; then
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
    info "Compiling optimized native binary (LuaTools Linux v${APP_VERSION})..."
    (cd "${SCRIPT_DIR}" && cargo build --release -j "${CARGO_BUILD_JOBS:-2}")
    BIN_SOURCE="${SCRIPT_DIR}/target/release/${APP_NAME}"
    mkdir -p "${SCRIPT_DIR}/dist"
    install -m 0755 "${BIN_SOURCE}" "${SCRIPT_DIR}/dist/${APP_NAME}"
    ok "Built stripped native binary ($(du -h "${BIN_SOURCE}" | awk '{print $1}')) -> ${BIN_SOURCE}"
}

# ------------------------------------------------------------------------------
# 3. Install Binary, Vector Icon, Desktop Launcher & luatools:// URI Handler
# ------------------------------------------------------------------------------
install_files() {
    if [ "$SYSTEM_WIDE" -eq 1 ]; then
        local bin_dir="/usr/local/bin"
        local apps_dir="/usr/local/share/applications"
        local icon_dir="/usr/local/share/icons/hicolor/scalable/apps"
        info "Installing system-wide into /usr/local..."
        run_privileged mkdir -p "$bin_dir" "$apps_dir" "$icon_dir"
        run_privileged install -m 0755 "$BIN_SOURCE" "${bin_dir}/${APP_NAME}"
        run_privileged install -m 0644 "${SCRIPT_DIR}/assets/luatools.svg" "${icon_dir}/luatools.svg"

        local tmp_desktop
        tmp_desktop="$(mktemp)"
        sed "s|@EXEC_PATH@|${bin_dir}/${APP_NAME}|g" "${SCRIPT_DIR}/assets/luatools.desktop.in" > "$tmp_desktop"
        run_privileged install -m 0644 "$tmp_desktop" "${apps_dir}/luatools.desktop"
        rm -f "$tmp_desktop"
        command -v update-desktop-database >/dev/null 2>&1 && run_privileged update-desktop-database "$apps_dir" || true
        if command -v xdg-mime >/dev/null 2>&1 && [ "$(id -u)" -ne 0 ]; then
            xdg-mime default luatools.desktop x-scheme-handler/luatools || true
        fi
        INSTALLED_BIN="${bin_dir}/${APP_NAME}"
    else
        local bin_dir="${HOME}/.local/bin"
        local apps_dir="${HOME}/.local/share/applications"
        local icon_dir="${HOME}/.local/share/icons/hicolor/scalable/apps"
        info "Installing for current user into ~/.local..."
        mkdir -p "$bin_dir" "$apps_dir" "$icon_dir" \
                 "${HOME}/.config/luatools" \
                 "${HOME}/.local/share/luatools/luavault" \
                 "${HOME}/.local/share/luatools/tools" \
                 "${HOME}/.cache/luatools"

        install -m 0755 "$BIN_SOURCE" "${bin_dir}/${APP_NAME}"
        install -m 0644 "${SCRIPT_DIR}/assets/luatools.svg" "${icon_dir}/luatools.svg"
        sed "s|@EXEC_PATH@|${bin_dir}/${APP_NAME}|g" "${SCRIPT_DIR}/assets/luatools.desktop.in" > "${apps_dir}/luatools.desktop"
        chmod 0644 "${apps_dir}/luatools.desktop"

        if command -v xdg-mime >/dev/null 2>&1; then
            xdg-mime default luatools.desktop x-scheme-handler/luatools || true
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
