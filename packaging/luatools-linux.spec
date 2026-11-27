%global debug_package %{nil}

Name:           luatools-linux
Version:        4.1.3
Release:        1%{?dist}
Summary:        Native Rust workstation for Steam stplug-in lua manifests, depots, and fixes
License:        GPL-3.0-or-later
URL:            https://lua.tools

# Runtime shared libraries (dlopened by winit/glutin + XDG integration)
Requires:       mesa-libEGL
Requires:       mesa-libGL
Requires:       libwayland-client
Requires:       libxkbcommon
Requires:       libX11
Requires:       libXcursor
Requires:       libXi
Requires:       libXrandr
Requires:       fontconfig
Requires:       xdg-utils
Recommends:     zenity
Recommends:     zstd
Recommends:     bzip2
Recommends:     p7zip
Recommends:     libnotify

%description
LuaTools Linux is a lightweight, zero-Python native Rust rewrite of LuaTools
featuring a sharp-edged monotone + cobalt blue UI, Steam Store & Hubcap manifest
acquisition, content-addressed LuaVault variant switching, DepotDownloaderMod
integration, SHA-256 backed game fixes, SLSsteam YAML sync, and binary appinfo.vdf editing.

%install
mkdir -p "%{buildroot}%{_bindir}"
mkdir -p "%{buildroot}/usr/lib/luatools"
mkdir -p "%{buildroot}%{_datadir}/luatools"
mkdir -p "%{buildroot}%{_datadir}/applications"
mkdir -p "%{buildroot}%{_datadir}/icons/hicolor/scalable/apps"
mkdir -p "%{buildroot}%{_datadir}/licenses/%{name}"

install -m 0755 "%{_sourcedir}/luatools" "%{buildroot}%{_bindir}/luatools"
if [ -f "%{_sourcedir}/luatools-steam-launcher" ]; then
    install -m 0755 "%{_sourcedir}/luatools-steam-launcher" "%{buildroot}%{_bindir}/luatools-steam-launcher"
fi
if [ -f "%{_sourcedir}/libsteam_integration.so" ]; then
    install -m 0755 "%{_sourcedir}/libsteam_integration.so" "%{buildroot}/usr/lib/luatools/libsteam_integration.so"
fi
if [ -f "%{_sourcedir}/libsteam_integration32.so" ]; then
    mkdir -p "%{buildroot}/usr/lib/luatools/32" "%{buildroot}/usr/lib/luatools/lib"
    install -m 0755 "%{_sourcedir}/libsteam_integration32.so" "%{buildroot}/usr/lib/luatools/libsteam_integration32.so"
    install -m 0755 "%{_sourcedir}/libsteam_integration32.so" "%{buildroot}/usr/lib/luatools/32/libsteam_integration.so"
    install -m 0755 "%{_sourcedir}/libsteam_integration32.so" "%{buildroot}/usr/lib/luatools/lib/libsteam_integration.so"
    ln -sfn lib "%{buildroot}/usr/lib/luatools/lib32"
    ln -sfn lib "%{buildroot}/usr/lib/luatools/i386-linux-gnu"
    ln -sfn ../32 "%{buildroot}/usr/lib/luatools/lib/i386-linux-gnu"
fi
if [ -f "%{_sourcedir}/libsteam_integration64.so" ]; then
    mkdir -p "%{buildroot}/usr/lib/luatools/64" "%{buildroot}/usr/lib/luatools/lib64" "%{buildroot}/usr/lib/luatools/lib"
    install -m 0755 "%{_sourcedir}/libsteam_integration64.so" "%{buildroot}/usr/lib/luatools/libsteam_integration64.so"
    install -m 0755 "%{_sourcedir}/libsteam_integration64.so" "%{buildroot}/usr/lib/luatools/64/libsteam_integration.so"
    install -m 0755 "%{_sourcedir}/libsteam_integration64.so" "%{buildroot}/usr/lib/luatools/lib64/libsteam_integration.so"
    ln -sfn lib64 "%{buildroot}/usr/lib/luatools/x86_64-linux-gnu"
    ln -sfn ../lib64 "%{buildroot}/usr/lib/luatools/lib/x86_64-linux-gnu"
fi
if [ -f "%{_sourcedir}/libsteam_auto_patch_linux.so" ]; then
    install -m 0755 "%{_sourcedir}/libsteam_auto_patch_linux.so" "%{buildroot}/usr/lib/luatools/libsteam_auto_patch_linux.so"
fi
if [ -d "%{_sourcedir}/millennium_plugin" ]; then
    mkdir -p "%{buildroot}%{_datadir}/luatools/millennium_plugin"
    cp -a "%{_sourcedir}/millennium_plugin/." "%{buildroot}%{_datadir}/luatools/millennium_plugin/"
fi
if [ -f "%{_sourcedir}/LICENSE" ]; then
    install -m 0644 "%{_sourcedir}/LICENSE" "%{buildroot}%{_datadir}/licenses/%{name}/LICENSE"
fi
install -m 0644 "%{_sourcedir}/luatools.svg" "%{buildroot}%{_datadir}/icons/hicolor/scalable/apps/luatools.svg"
install -m 0644 "%{_sourcedir}/luatools.desktop" "%{buildroot}%{_datadir}/applications/luatools.desktop"

%files
%{_bindir}/luatools
%{_bindir}/luatools-steam-launcher
/usr/lib/luatools
%{_datadir}/luatools
%{_datadir}/applications/luatools.desktop
%{_datadir}/icons/hicolor/scalable/apps/luatools.svg
%{_datadir}/licenses/%{name}/LICENSE

%changelog
* Fri Nov 27 2026 LuaTools Linux Contributors <noreply@lua.tools> - 4.1.3-1
- Release v4.1.3: Millennium v3.6.3 native plugin runtime compatibility (require("json"), logger:info, positional/table RPC args, table returns, native .millennium/Dist/webkit.js), HTTP/CEF/bridge hardening, self-updater semver/multilib fixes, Home Hub & ? tooltip fixes, and cyclic symlink / packaging fixes
* Fri Nov 27 2026 LuaTools Linux Contributors <noreply@lua.tools> - 4.1.2-1
- Release v4.1.2: Native Millennium v3.6+ LuaToolsLinux plugin (Lua backend + Steam Store/SharedJSContext UI), multi-arch $LIB preload routing, multi-user /tmp/millennium-mep-{uid}.sock isolation, V_strncpy/Plat_IsInDebugSession bridge guards, and extended companion RPC handlers
* Fri Nov 27 2026 LuaTools Linux Contributors <noreply@lua.tools> - 4.0.0-1
- Major Release v4.0.0: Full audit closure, 32-bit & 64-bit dual-arch integration, LT-50 prebuilt release updater, stream compiler feed, unified ActionProgressModal, ELFCLASS32/SIGTRAP/BST-46 KV1/NetPacket/UserStats/CloudRedirect/LegacyCdKey/PersonaStateMsg LEB128 fixes, robust Steam PID detection & kill reliability, and full GPL-3.0 LICENSE
* Thu Nov 26 2026 LuaTools Linux Contributors <noreply@lua.tools> - 3.3.11-1
- Release v3.3.11: Fix memory patch safety & 32-bit JMP UB, sandbox mlua VM, fix Valve binary KV1 parser, fix headless keyring/LTSP/--silent/localization/timeout/bst:// URI bugs, and clean up repository
* Wed Nov 25 2026 LuaTools Linux Contributors <noreply@lua.tools> - 3.2.3-1
- Release v3.2.3: Fixed 7z path traversal false positives (docs/LICENSE), DepotDownloader -remember-password flag logic, DRM decryption keys via bridge_lib depot_keys, is_lua_unlocked checks additional_apps for Lua-unlocked games, licenses NetPacket hooks installed, ONLINE_FIX_APP_ID duplicate consolidated
* Wed Nov 25 2026 LuaTools Linux Contributors <noreply@lua.tools> - 3.2.2-1
- Release v3.2.2: Complete removal of legacy C++ usefulsteamtools reference tree, full audit closure across all 155 units from Luatools complete.txt, and end-to-end workspace verification
* Wed Nov 25 2026 LuaTools Linux Contributors <noreply@lua.tools> - 3.2.0-1
- Release v3.2.0: Complete BetterSteamTools Linux port, native Steam client netpacket protocol handling & online-fix licensing, hardened SLSsteam de-sync on uninstall, case-insensitive DLL overrides, and full forensic audit fixes
* Wed Nov 25 2026 LuaTools Linux Contributors <noreply@lua.tools> - 3.1.0-1
- Release v3.1.0: Complete 32-key AceSLS/SLSsteam config.yaml schema & ManifestIds u64 healing, Home Open button wired to Integration & Process Controls modal with 1-click Game Tools bar, surfaced action buttons across My Games / Add Games / Depots & Vault / Game Fixes / Settings, and concise plain-English README rewrite
* Wed Nov 25 2026 LuaTools Linux Contributors <noreply@lua.tools> - 3.0.1-1
- Release v3.0.1: Aggressive RAM optimization (-59% VmRSS from 220.7 MB to 87.5 MB, -74% RssAnon to 25.6 MB, -64% PSS to 45.4 MB), fixed example.com browser-open bug and hardened /open-url validation, simplified primary-user UI across all screens, async Launch Options & Fix Revert, full forensic audit parity since v2.6.1 (413 passing workspace tests)
* Wed Nov 25 2026 LuaTools Linux Contributors <noreply@lua.tools> - 2.9.4-0.1.beta
- Beta v2.9.4-beta: Binary manifest codec, LaunchModStore PICS rebase, AppliedFixIndexService, DownloadQueue, age x25519/AES-GCM credential encryption, silent protocol install, steam-sls LD_AUDIT relaunch, and Proton Goldberg DLL support
* Tue Nov 24 2026 LuaTools Linux Contributors <noreply@lua.tools> - 2.9.3-0.1.beta
- Beta v2.9.3-beta: Complete GUI wiring, Overwrite Diff modal, 100-item download history ring, 13 extended LTSP HTTP routes, CDP queue drain, and AceSLS schema alignment
* Tue Nov 24 2026 LuaTools Linux Contributors <noreply@lua.tools> - 2.9.2-0.1.beta
- Beta v2.9.2-beta: Single-instance flock, Unix socket IPC, CLI flags, OS login autostart, Steam UI language detection, bst:// URI handler, per-depot DLC toggling, CloudRedirect, and Legacy Unlocker cleaner
* Tue Nov 24 2026 LuaTools Linux Contributors <noreply@lua.tools> - 2.9.1-0.1.beta
- Beta v2.9.1-beta: Individual file-by-file audit & improvements across all 78 files changed since v2.6.1 (339/339 tests passing, 0 clippy warnings), installer/uninstaller shared-library staging fixes, and emoji-free Leonida-formatted README.md
* Tue Nov 24 2026 LuaTools Linux Contributors <noreply@lua.tools> - 2.9.0-0.1.beta
- Beta v2.9.0-beta: Merge & audit beta/v2.7.3-beta, beta/v2.8.2-beta, and 2.8.4-beta; remove duplicate nested repo copy and 32 stray/redundant files; restore hardened bridge_lib/src/lib.rs; fix all compile/runtime/UB bugs across interop, netpacket, credentials, token_bridge, and OSTPlatform Linux modules
* Tue Nov 24 2026 LuaTools Linux Contributors <noreply@lua.tools> - 2.7.3-0.1.beta
- Beta v2.7.3-beta: Fix Steam game cover image loading (hashed modern Steam CDN URLs via appdetails + local librarycache + in-flight retry queue), add Kill Steam button to terminate all Steam instances, and resolve all compilation and runtime bugs from v2.7.2-alpha
* Mon Nov 23 2026 LuaTools Linux Contributors <noreply@lua.tools> - 2.6.1-1
- Release v2.6.1: Fix Steam SIGABRT crash in libsteam_integration.so (ELF64 header offset parsing, LD_PRELOAD process guard, ASLR runtime address validation, panic hook & catch_unwind, TicketStore VDF parser/upsert fixes, --gdb/--lldb launcher support)
* Mon Nov 23 2026 LuaTools Linux Contributors <noreply@lua.tools> - 2.6.0-1
- Release v2.6.0: Native Linux SteamAutoPatch (Goldberg .so/.dll deployment & ELF/PE interface generator), complete Steam Inlibrary Fix, one-click recommended fixes, and illustrated How to Use manual
* Mon Nov 23 2026 LuaTools Linux Contributors <noreply@lua.tools> - 2.0-0.1.beta
- Complete native Rust rewrite (v2.0-beta) with sharp monotone + cobalt blue UI and Git self-updater
