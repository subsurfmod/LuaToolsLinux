%global debug_package %{nil}

Name:           luatools-linux
Version:        2.1.0
Release:        1%{?dist}
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

%description
LuaTools Linux v2.0.0 is a lightweight, zero-Python native Rust rewrite of LuaTools
featuring a sharp-edged monotone + cobalt blue UI, Steam Store & Hubcap manifest
acquisition, content-addressed LuaVault variant switching, DepotDownloaderMod
integration, SHA-256 backed game fixes, SLSsteam YAML sync, and binary appinfo.vdf editing.

%install
mkdir -p %{buildroot}%{_bindir}
mkdir -p %{buildroot}%{_datadir}/applications
mkdir -p %{buildroot}%{_datadir}/icons/hicolor/scalable/apps

install -m 0755 %{_sourcedir}/luatools %{buildroot}%{_bindir}/luatools
install -m 0644 %{_sourcedir}/luatools.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/luatools.svg
install -m 0644 %{_sourcedir}/luatools.desktop %{buildroot}%{_datadir}/applications/luatools.desktop

%files
%{_bindir}/luatools
%{_datadir}/applications/luatools.desktop
%{_datadir}/icons/hicolor/scalable/apps/luatools.svg

%changelog
* Mon Nov 23 2026 LuaTools Linux Contributors <noreply@lua.tools> - 2.0-0.1.beta
- Complete native Rust rewrite (v2.0-beta) with sharp monotone + cobalt blue UI and Git self-updater
