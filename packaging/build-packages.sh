#!/usr/bin/env bash
# ==============================================================================
# LuaTools Linux v2.0.0 — Native Package Builder (.rpm, .deb, .tar.gz, AppDir)
# ==============================================================================
set -euo pipefail

VERSION="2.0-beta"
ARCH="$(uname -m)"
DEB_ARCH="amd64"
[ "$ARCH" = "aarch64" ] && DEB_ARCH="arm64"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
DIST_DIR="${ROOT_DIR}/dist"

export PATH="${HOME}/.cargo/bin:${PATH}"

echo "[1/4] Building stripped release binary..."
CARGO_JOBS="${CARGO_BUILD_JOBS:-2}"
(cd "$ROOT_DIR" && cargo build --release -j "$CARGO_JOBS" ${CARGO_EXTRA_ARGS:-})
mkdir -p "$DIST_DIR"
BIN="${ROOT_DIR}/target/release/luatools"
cp -f "$BIN" "${DIST_DIR}/luatools"

# ------------------------------------------------------------------------------
# 2. Portable Tarball Bundle (with one-click install.sh)
# ------------------------------------------------------------------------------
echo "[2/4] Creating portable tarball bundle..."
STAGE_TAR="$(mktemp -d)"
BUNDLE_NAME="luatools-linux-${VERSION}-${ARCH}"
mkdir -p "${STAGE_TAR}/${BUNDLE_NAME}/dist" "${STAGE_TAR}/${BUNDLE_NAME}/assets"
cp -f "$BIN" "${STAGE_TAR}/${BUNDLE_NAME}/dist/luatools"
cp -f "${ROOT_DIR}/install.sh" "${ROOT_DIR}/uninstall.sh" "${ROOT_DIR}/README.md" "${STAGE_TAR}/${BUNDLE_NAME}/"
cp -f "${ROOT_DIR}/assets/luatools.svg" "${ROOT_DIR}/assets/luatools.desktop.in" "${STAGE_TAR}/${BUNDLE_NAME}/assets/"
chmod +x "${STAGE_TAR}/${BUNDLE_NAME}/install.sh" "${STAGE_TAR}/${BUNDLE_NAME}/uninstall.sh" "${STAGE_TAR}/${BUNDLE_NAME}/dist/luatools"
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
    cp -f "${ROOT_DIR}/assets/luatools.svg" "${RPM_TOP}/SOURCES/luatools.svg"
    sed "s|@EXEC_PATH@|/usr/bin/luatools|g" "${ROOT_DIR}/assets/luatools.desktop.in" > "${RPM_TOP}/SOURCES/luatools.desktop"
    cp -f "${SCRIPT_DIR}/luatools-linux.spec" "${RPM_TOP}/SPECS/luatools-linux.spec"

    rpmbuild --define "_topdir ${RPM_TOP}" --define "_tmppath ${RPM_TOP}/TMP" -bb "${RPM_TOP}/SPECS/luatools-linux.spec"
    find "${RPM_TOP}/RPMS" -name "*.rpm" -exec cp -f {} "${DIST_DIR}/" \;
    rm -rf "$RPM_TOP"
    echo "  -> $(ls -1 "${DIST_DIR}"/*.rpm | tail -n1)"
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
         "${DEB_ROOT}/usr/share/applications" \
         "${DEB_ROOT}/usr/share/icons/hicolor/scalable/apps"

install -m 0755 "$BIN" "${DEB_ROOT}/usr/bin/luatools"
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
Depends: libegl1, libgl1, libx11-6, libxkbcommon0, xdg-utils
Recommends: zenity
Maintainer: LuaTools Linux Contributors <noreply@lua.tools>
Description: Native Rust workstation for Steam stplug-in lua manifests and fixes
 Lightweight hardware-accelerated native Linux port of LuaTools v2.0.0.
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
