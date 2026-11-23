%global debug_package %{nil}

Name:           luatools-linux
Version:        2.7.3
Release:        0.1.beta%{?dist}
Summary:        Native Rust workstation for Steam stplug-in lua manifests, depots, and fixes
License:        MIT
URL:            https://lua.tools

# Runtime shared libraries (dlopened by winit/glutin + XDG integration)
Requires:       mesa-libEGL
Requires:       mesa-libGL
Requires:       libwayland-client
Requires:       libxkbcommon
Requires:       libX11
Requires:       xdg-utils
Recommends:     zenity
Recommends:     zstd
Recommends:     bzip2
Recommends:     p7zip

%description
LuaTools Linux v2.0.0 is a lightweight, zero-Python native Rust rewrite of LuaTools
featuring a sharp-edged monotone + cobalt blue UI, Steam Store & Hubcap manifest
acquisition, content-addressed LuaVault variant switching, DepotDownloaderMod
integration, SHA-256 backed game fixes, SLSsteam YAML sync, and binary appinfo.vdf editing.

%install
mkdir -p %{buildroot}%{_bindir}
mkdir -p %{buildroot}/usr/lib/luatools
mkdir -p %{buildroot}%{_datadir}/applications
mkdir -p %{buildroot}%{_datadir}/icons/hicolor/scalable/apps

install -m 0755 %{_sourcedir}/luatools %{buildroot}%{_bindir}/luatools
if [ -f %{_sourcedir}/libsteam_integration.so ]; then
    install -m 0755 %{_sourcedir}/libsteam_integration.so %{buildroot}/usr/lib/luatools/libsteam_integration.so
fi
if [ -f %{_sourcedir}/libsteam_auto_patch_linux.so ]; then
    install -m 0755 %{_sourcedir}/libsteam_auto_patch_linux.so %{buildroot}/usr/lib/luatools/libsteam_auto_patch_linux.so
fi
install -m 0644 %{_sourcedir}/luatools.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/luatools.svg
install -m 0644 %{_sourcedir}/luatools.desktop %{buildroot}%{_datadir}/applications/luatools.desktop

%files
%{_bindir}/luatools
/usr/lib/luatools
%{_datadir}/applications/luatools.desktop
%{_datadir}/icons/hicolor/scalable/apps/luatools.svg

%changelog
* Tue Nov 24 2026 LuaTools Linux Contributors <noreply@lua.tools> - 2.7.3-0.1.beta
- Beta v2.7.3-beta: Fix Steam game cover image loading (hashed modern Steam CDN URLs via appdetails + local librarycache + in-flight retry queue), add Kill Steam button to terminate all Steam instances, and resolve all compilation and runtime bugs from v2.7.2-alpha
* Mon Nov 23 2026 LuaTools Linux Contributors <noreply@lua.tools> - 2.6.1-1
- Release v2.6.1: Fix Steam SIGABRT crash in libsteam_integration.so (ELF64 header offset parsing, LD_PRELOAD process guard, ASLR runtime address validation, panic hook & catch_unwind, TicketStore VDF parser/upsert fixes, --gdb/--lldb launcher support)
* Mon Nov 23 2026 LuaTools Linux Contributors <noreply@lua.tools> - 2.6.0-1
- Release v2.6.0: Native Linux SteamAutoPatch (Goldberg .so/.dll deployment & ELF/PE interface generator), complete Steam Inlibrary Fix, one-click recommended fixes, and illustrated How to Use manual
* Mon Nov 23 2026 LuaTools Linux Contributors <noreply@lua.tools> - 2.0-0.1.beta
- Complete native Rust rewrite (v2.0-beta) with sharp monotone + cobalt blue UI and Git self-updater
