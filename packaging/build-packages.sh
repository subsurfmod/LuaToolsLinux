#!/usr/bin/env bash
# ==============================================================================
# LuaTools Linux — Native Package Builder (.rpm, .deb, .tar.gz, AppDir)
# ==============================================================================
set -euo pipefail

VERSION="4.0.0"
ARCH="$(uname -m)"
DEB_ARCH="amd64"
[ "$ARCH" = "aarch64" ] && DEB_ARCH="arm64"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
DIST_DIR="${ROOT_DIR}/dist"

export PATH="${HOME}/.cargo/bin:${PATH}"

echo "[1/4] Building workspace release binary + 32-bit & 64-bit bridge libraries..."
CARGO_JOBS="${CARGO_BUILD_JOBS:-2}"
(cd "$ROOT_DIR" && cargo build --workspace --release -j "$CARGO_JOBS" ${CARGO_EXTRA_ARGS:-})
if command -v rustup >/dev/null 2>&1; then
    rustup target add i686-unknown-linux-gnu 2>/dev/null || true
fi
if rustup target list 2>/dev/null | grep -q "i686-unknown-linux-gnu (installed)"; then
    echo "  -> Building 32-bit i686 Steam integration library (native Steam LD_PRELOAD)..."
    (cd "$ROOT_DIR" && cargo build --manifest-path bridge_lib/Cargo.toml --target i686-unknown-linux-gnu --release -j "$CARGO_JOBS" ${CARGO_EXTRA_ARGS:-} || true)
fi
(cd "$ROOT_DIR" && cargo build --profile debug-debug -p bridge_lib -j "$CARGO_JOBS" ${CARGO_EXTRA_ARGS:-} || true)
mkdir -p "$DIST_DIR" "$DIST_DIR/32" "$DIST_DIR/64"
BIN="${ROOT_DIR}/target/release/luatools"
BRIDGE_32="${ROOT_DIR}/target/i686-unknown-linux-gnu/release/libsteam_integration.so"
BRIDGE_64="${ROOT_DIR}/target/release/libsteam_integration.so"
[ -f "$BRIDGE_64" ] || BRIDGE_64="${ROOT_DIR}/target/debug-debug/libsteam_integration.so"
PATCH_SO="${ROOT_DIR}/target/release/libsteam_auto_patch_linux.so"

cp -f "$BIN" "${DIST_DIR}/luatools"
if [ -f "$BRIDGE_32" ]; then
    cp -f "$BRIDGE_32" "${DIST_DIR}/libsteam_integration.so"
    cp -f "$BRIDGE_32" "${DIST_DIR}/libsteam_integration32.so"
    cp -f "$BRIDGE_32" "${DIST_DIR}/32/libsteam_integration.so"
fi
if [ -f "$BRIDGE_64" ]; then
    [ -f "$BRIDGE_32" ] || cp -f "$BRIDGE_64" "${DIST_DIR}/libsteam_integration.so"
    cp -f "$BRIDGE_64" "${DIST_DIR}/libsteam_integration64.so"
    cp -f "$BRIDGE_64" "${DIST_DIR}/64/libsteam_integration.so"
fi
[ -f "$PATCH_SO" ] && cp -f "$PATCH_SO" "${DIST_DIR}/libsteam_auto_patch_linux.so"

# ------------------------------------------------------------------------------
# 2. Portable Tarball Bundle (with one-click install.sh)
# ------------------------------------------------------------------------------
echo "[2/4] Creating portable tarball bundle..."
STAGE_TAR="$(mktemp -d)"
BUNDLE_NAME="luatools-linux-${VERSION}-${ARCH}"
mkdir -p "${STAGE_TAR}/${BUNDLE_NAME}/dist" "${STAGE_TAR}/${BUNDLE_NAME}/dist/32" "${STAGE_TAR}/${BUNDLE_NAME}/dist/64" "${STAGE_TAR}/${BUNDLE_NAME}/assets"
cp -f "$BIN" "${STAGE_TAR}/${BUNDLE_NAME}/dist/luatools"
if [ -f "$BRIDGE_32" ]; then
    cp -f "$BRIDGE_32" "${STAGE_TAR}/${BUNDLE_NAME}/dist/libsteam_integration.so"
    cp -f "$BRIDGE_32" "${STAGE_TAR}/${BUNDLE_NAME}/dist/libsteam_integration32.so"
    cp -f "$BRIDGE_32" "${STAGE_TAR}/${BUNDLE_NAME}/dist/32/libsteam_integration.so"
fi
if [ -f "$BRIDGE_64" ]; then
    [ -f "$BRIDGE_32" ] || cp -f "$BRIDGE_64" "${STAGE_TAR}/${BUNDLE_NAME}/dist/libsteam_integration.so"
    cp -f "$BRIDGE_64" "${STAGE_TAR}/${BUNDLE_NAME}/dist/libsteam_integration64.so"
    cp -f "$BRIDGE_64" "${STAGE_TAR}/${BUNDLE_NAME}/dist/64/libsteam_integration.so"
fi
[ -f "$PATCH_SO" ] && cp -f "$PATCH_SO" "${STAGE_TAR}/${BUNDLE_NAME}/dist/libsteam_auto_patch_linux.so"
cp -f "${ROOT_DIR}/install.sh" "${ROOT_DIR}/uninstall.sh" "${ROOT_DIR}/README.md" "${STAGE_TAR}/${BUNDLE_NAME}/"
cp -f "${ROOT_DIR}/assets/luatools.svg" "${ROOT_DIR}/assets/luatools.desktop.in" "${ROOT_DIR}/assets/luatools-steam-launcher" "${STAGE_TAR}/${BUNDLE_NAME}/assets/"
chmod +x "${STAGE_TAR}/${BUNDLE_NAME}/install.sh" "${STAGE_TAR}/${BUNDLE_NAME}/uninstall.sh" "${STAGE_TAR}/${BUNDLE_NAME}/dist/luatools" "${STAGE_TAR}/${BUNDLE_NAME}/assets/luatools-steam-launcher"
tar -C "$STAGE_TAR" -czf "${DIST_DIR}/${BUNDLE_NAME}.tar.gz" "$BUNDLE_NAME"
rm -rf "$STAGE_TAR"
echo "  -> ${DIST_DIR}/${BUNDLE_NAME}.tar.gz"

# ------------------------------------------------------------------------------
# 3. Native RPM Package (Fedora / RHEL / openSUSE)
# ------------------------------------------------------------------------------
if command -v rpmbuild >/dev/null 2>&1; then
    echo "[3/4] Building native .rpm package..."
    RPM_TOP="$(mktemp -d)"
    mkdir -p "${RPM_TOP}"/{BUILD,RPMS,SOURCES,SPECS,SRPMS,TMP}
    cp -f "$BIN" "${RPM_TOP}/SOURCES/luatools"
    cp -f "${ROOT_DIR}/assets/luatools-steam-launcher" "${RPM_TOP}/SOURCES/luatools-steam-launcher"
    if [ -f "$BRIDGE_32" ]; then
        cp -f "$BRIDGE_32" "${RPM_TOP}/SOURCES/libsteam_integration.so"
        cp -f "$BRIDGE_32" "${RPM_TOP}/SOURCES/libsteam_integration32.so"
    fi
    if [ -f "$BRIDGE_64" ]; then
        [ -f "$BRIDGE_32" ] || cp -f "$BRIDGE_64" "${RPM_TOP}/SOURCES/libsteam_integration.so"
        cp -f "$BRIDGE_64" "${RPM_TOP}/SOURCES/libsteam_integration64.so"
    fi
    [ -f "$PATCH_SO" ] && cp -f "$PATCH_SO" "${RPM_TOP}/SOURCES/libsteam_auto_patch_linux.so"
    cp -f "${ROOT_DIR}/assets/luatools.svg" "${RPM_TOP}/SOURCES/luatools.svg"
    sed "s|@EXEC_PATH@|/usr/bin/luatools|g" "${ROOT_DIR}/assets/luatools.desktop.in" > "${RPM_TOP}/SOURCES/luatools.desktop"
    cp -f "${SCRIPT_DIR}/luatools-linux.spec" "${RPM_TOP}/SPECS/luatools-linux.spec"

    rpmbuild --define "_topdir ${RPM_TOP}" --define "_tmppath ${RPM_TOP}/TMP" -bb "${RPM_TOP}/SPECS/luatools-linux.spec"
    BUILT_RPM="$(find "${RPM_TOP}/RPMS" -name "*.rpm" | head -n1)"
    if [ -n "$BUILT_RPM" ]; then
        cp -f "$BUILT_RPM" "${DIST_DIR}/"
        echo "  -> ${DIST_DIR}/$(basename "$BUILT_RPM")"
    fi
    rm -rf "$RPM_TOP"
else
    echo "[3/4] Skipping .rpm (rpmbuild not installed)."
fi

# ------------------------------------------------------------------------------
# 4. Native DEB Package (Ubuntu / Debian / Pop!_OS / SteamOS-compatible)
#    Uses dpkg-deb if present, or pure POSIX ar+tar fallback so it builds anywhere!
# ------------------------------------------------------------------------------
echo "[4/4] Building native .deb package..."
DEB_WORK="$(mktemp -d)"
DEB_ROOT="${DEB_WORK}/pkg"
mkdir -p "${DEB_ROOT}/DEBIAN" \
         "${DEB_ROOT}/usr/bin" \
         "${DEB_ROOT}/usr/lib/luatools" \
         "${DEB_ROOT}/usr/lib/luatools/32" \
         "${DEB_ROOT}/usr/lib/luatools/64" \
         "${DEB_ROOT}/usr/share/applications" \
         "${DEB_ROOT}/usr/share/icons/hicolor/scalable/apps"

install -m 0755 "$BIN" "${DEB_ROOT}/usr/bin/luatools"
install -m 0755 "${ROOT_DIR}/assets/luatools-steam-launcher" "${DEB_ROOT}/usr/bin/luatools-steam-launcher"
if [ -f "$BRIDGE_32" ]; then
    install -m 0755 "$BRIDGE_32" "${DEB_ROOT}/usr/lib/luatools/libsteam_integration.so"
    install -m 0755 "$BRIDGE_32" "${DEB_ROOT}/usr/lib/luatools/libsteam_integration32.so"
    install -m 0755 "$BRIDGE_32" "${DEB_ROOT}/usr/lib/luatools/32/libsteam_integration.so"
fi
if [ -f "$BRIDGE_64" ]; then
    [ -f "$BRIDGE_32" ] || install -m 0755 "$BRIDGE_64" "${DEB_ROOT}/usr/lib/luatools/libsteam_integration.so"
    install -m 0755 "$BRIDGE_64" "${DEB_ROOT}/usr/lib/luatools/libsteam_integration64.so"
    install -m 0755 "$BRIDGE_64" "${DEB_ROOT}/usr/lib/luatools/64/libsteam_integration.so"
fi
[ -f "$PATCH_SO" ] && install -m 0755 "$PATCH_SO" "${DEB_ROOT}/usr/lib/luatools/libsteam_auto_patch_linux.so"
install -m 0644 "${ROOT_DIR}/assets/luatools.svg" "${DEB_ROOT}/usr/share/icons/hicolor/scalable/apps/luatools.svg"
sed "s|@EXEC_PATH@|/usr/bin/luatools|g" "${ROOT_DIR}/assets/luatools.desktop.in" > "${DEB_ROOT}/usr/share/applications/luatools.desktop"
chmod 0644 "${DEB_ROOT}/usr/share/applications/luatools.desktop"

INSTALLED_KB="$(du -sk "${DEB_ROOT}/usr" | awk '{print $1}')"
cat > "${DEB_ROOT}/DEBIAN/control" <<EOF
Package: luatools-linux
Version: ${VERSION}
Section: games
Priority: optional
Architecture: ${DEB_ARCH}
Installed-Size: ${INSTALLED_KB}
Depends: libegl1, libgl1, libwayland-client0, libx11-6, libxkbcommon0, xdg-utils
Recommends: zenity
Maintainer: LuaTools Linux Contributors <noreply@lua.tools>
Description: Native Rust workstation for Steam stplug-in lua manifests and fixes
 Lightweight hardware-accelerated native Linux edition of LuaTools (${VERSION}).
EOF

DEB_OUT="${DIST_DIR}/luatools-linux_${VERSION}_${DEB_ARCH}.deb"
if command -v dpkg-deb >/dev/null 2>&1; then
    dpkg-deb --build --root-owner-group "$DEB_ROOT" "$DEB_OUT" >/dev/null
elif command -v ar >/dev/null 2>&1; then
    (
        cd "$DEB_WORK"
        printf "2.0\n" > debian-binary
        tar --owner=0 --group=0 -C "${DEB_ROOT}/DEBIAN" -czf control.tar.gz .
        tar --owner=0 --group=0 -C "${DEB_ROOT}" -czf data.tar.gz ./usr
        rm -f "$DEB_OUT"
        ar rcs "$DEB_OUT" debian-binary control.tar.gz data.tar.gz
    )
fi
rm -rf "$DEB_WORK"
[ -f "$DEB_OUT" ] && echo "  -> ${DEB_OUT}"

echo "[DONE] All artifacts ready in ${DIST_DIR}/"
