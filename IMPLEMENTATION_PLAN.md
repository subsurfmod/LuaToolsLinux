# LuaTools Linux Implementation Plan

Based on the forensic audit in `/home/tejas/Documents/Luatools complete.txt`, this plan addresses the critical gaps.

## Priority 1: Critical Runtime Fixes (Blockers)

### 1. Fix Silent Headless Install Protocol (LT-13) - BROKEN
- **File**: `src/services/protocol_service.rs`
- **Bug**: Line 29 tries to parse "silent" as u32
- **Fix**: Handle "silent" segment before parsing appid

### 2. Fix Steam Process Relaunch Split-Brain (LT-18, LNX-04) - PARTIAL
- **File**: `src/ui/app.rs` (lines 1686-1866), `src/steam_bridge/preload_loader.rs`
- **Bug**: "Restart Steam" launches `luatools-steam-launcher` (64-bit LD_PRELOAD) instead of `steam-sls` (LD_AUDIT)
- **Fix**: Modify restart logic to invoke `steam-sls` wrapper

### 3. Fix SLSsteam Default Config (LNX-01) - PARTIAL
- **File**: `src/services/slssteam_service.rs`, `src/models/config.rs`
- **Bug**: `PlayNotOwnedGames: false` by default disables unlocking
- **Fix**: Default to `true`, fix unquoted YAML hex keys

### 4. Fix Fix ID Colon Rejection (LT-74, LT-75) - PARTIAL
- **File**: `src/services/fixes_service.rs` (lines 62-70)
- **Bug**: `is_safe_fix_id` rejects ":" characters, breaking all "online-fix:<uuid>" fixes
- **Fix**: Allow colons in fix IDs

### 5. Fix DonateKeys Wire Format (LT-45) - BROKEN
- **File**: `src/services/donate_keys.rs` (line 100)
- **Bug**: Sends JSON instead of `text/plain` format "depotId:hexKey,..."
- **Fix**: Change Content-Type and payload format

---

## Priority 2: Missing Core Features (NOT PORTED)

### 6. System Tray Support (LT-07) - NOT PORTED
- **Need**: Add `tray-icon` crate, implement minimize-to-tray, context menu
- **Files**: New `src/services/tray_service.rs`, modify `src/main.rs`, `src/ui/app.rs`

### 7. Steam UI Language Detection (LT-16) - NOT PORTED
- **File**: `src/services/steam_service.rs`
- **Need**: Parse `~/.local/share/Steam/config/config.vdf` for "Language" key

### 8. DepotCache Migration Service (LT-34) - NOT PORTED
- **File**: New `src/services/depot_cache_migration.rs`
- **Need**: Move manifests from `config/depotcache` to `depotcache`

### 9. Raw Manifest Fetch Endpoint (LT-44) - NOT PORTED
- **File**: `src/services/api_client.rs`
- **Need**: Implement `fetch_manifest(depot_id, gid)` calling `https://lua.tools/api/givemethemanifestpunk/{depot}/{gid}`

### 10. Binary .manifest Magic/Protobuf Decoder (LT-60, LT-61, LT-62) - NOT PORTED
- **File**: New `src/parsers/manifest_binary.rs`
- **Need**: Parse magic headers (0x71F617D0, 0x1F4812BE, 0x32C415AB), unwrap ZIP, decode protobuf, validate AES keys

### 11. LaunchModStore & PICS Drift Detection (LT-58, LT-59) - NOT PORTED
- **File**: New `src/services/launch_mod_store.rs`
- **Need**: Track launchmods.json, detect Steam PICS overwrites, show re-apply toast

### 12. Steam Process Coordination for appinfo.vdf (LT-57) - NOT PORTED
- **File**: `src/ui/dialogs.rs` (LaunchOptionsModal)
- **Need**: Auto-stop Steam before saving, restart after

### 13. AppliedFixIndexService (LT-76) - NOT PORTED
- **File**: New `src/services/applied_fix_index.rs`
- **Need**: Central registry of applied fixes across libraries

### 14. Download Queue with History (LT-38) - NOT PORTED
- **File**: New `src/services/download_queue.rs`
- **Need**: Centralized worker, concurrency, ETA, cancellation, 100-item ring history

### 15. Per-Depot DLC Checkbox Toggle (LT-31) - NOT PORTED
- **File**: `src/ui/lua_editor.rs`, `src/ui/views_workbench.rs`
- **Need**: Enable `disabled_entries` field in UI with checkboxes

### 16. Overwrite Diff Confirmation Modal (LT-32) - NOT PORTED
- **File**: New `src/ui/diff_modal.rs`, integrate in `lua_installer.rs`
- **Need**: Show unified diff before overwriting .lua files

### 17. At-Rest Credential Encryption (LT-40) - PARTIAL
- **File**: `src/services/auth_service.rs`, `src/services/settings_service.rs`
- **Need**: Replace plaintext JSON with encrypted storage (libsecret/keyring or age)

---

## Priority 3: Linux-Specific Fixes

### 18. Fix YAML Unquoted Keys in SlssteamService (LNX-01)
- **File**: `src/services/slssteam_service.rs` (line 245)
- **Fix**: Serialize depot keys as quoted strings

### 19. Fix SLSsteam Config Sync on Hot Reload (LT-35, LNX-02)
- **File**: `src/services/hot_reload_service.rs`, `src/services/lua_installer.rs`
- **Fix**: Re-sync config.yaml on .lua changes, handle removals

### 20. Fix ManifestBindingService Placebo (LNX-09)
- **File**: `src/services/manifest_binding_service.rs`, `src/steam_bridge/manifest_hooks.rs`
- **Fix**: Actually read MANIFEST_PINS in bridge, implement hook

### 21. Fix StubRemover Executable Swap (LNX-06)
- **File**: `src/services/external_tools.rs` (lines 403-428)
- **Fix**: Wait for process, move `.unpacked.exe` over original

### 22. Fix Goldberg Windows DLL Download (LNX-08)
- **File**: `src/services/external_tools.rs` (lines 255-286)
- **Fix**: Download `emu-win-release.7z` for Proton games

### 23. Fix PlayUnownedService Placebo (LNX-03)
- **File**: `src/services/play_unowned_service.rs`
- **Fix**: Either remove or make it actually call SLSsteam config update

### 24. Add Steam-Startup Auto-Launcher (LT-20 alternative)
- **File**: New `src/services/steam_autostart.rs`
- **Need**: systemd user service or Steam wrapper script to launch LuaTools on Steam start

---

## Priority 4: Dead Code Cleanup (from Section 5 of audit)

### 25. Remove or Wire Dead Services
- `src/services/plugin_installer_service.rs` (345 LOC) - Remove or wire to LTSP
- `src/services/plugin_add_service.rs` (265 LOC) - Remove
- `src/services/plugin_log.rs` (196 LOC) - Remove
- `src/services/doh_resolver.rs` (264 LOC) - Remove or wire to ureq
- `src/services/analytics_service.rs` (123 LOC) - Remove
- `src/services/mode_migration.rs` (189 LOC) - Wire or remove
- `src/services/steamless_service.rs` (355 LOC) - Remove or implement
- `src/services/asset_hash.rs` (214 LOC) - Wire to downloaders
- `src/services/file_hash.rs` (183 LOC) - Wire to downloaders
- `src/services/github_proxy.rs` (297 LOC) - Remove or wire
- `src/services/proxied_file_downloader.rs` (192 LOC) - Remove
- `src/steam_bridge/manifest_hooks.rs` (357 LOC) - Implement or remove
- `src/steam_bridge/denuvo_auth.rs` (442 LOC) - Remove
- `src/steam_bridge/token_bridge.rs` (247 LOC) - Remove
- `src/steam_bridge/cloud_redirect.rs` (395 LOC) - Remove
- `src/steam_bridge/interop/*.rs` (2,714 LOC) - Remove (32-bit only)
- `src/steam_bridge/netpacket/*.rs` (1,766 LOC) - Remove (32-bit only)
- `src/steam_bridge/credentials/*.rs` (1,054 LOC) - Remove
- `src/steam_bridge/ost_platform/*.rs` (6,625 LOC) - Remove

---

## Implementation Order

1. **Week 1**: Priority 1 (5 critical runtime fixes)
2. **Week 2**: Priority 2 items 6-10 (System tray, language detection, depotcache migration, manifest fetch, binary manifest codec)
3. **Week 3**: Priority 2 items 11-17 (LaunchModStore, process coordination, applied fix index, download queue, DLC checkboxes, diff modal, credential encryption)
4. **Week 4**: Priority 3 (Linux-specific fixes)
5. **Week 5**: Priority 4 (Dead code cleanup)

---

## Status Tracking

| Task | Status | Notes |
|------|--------|-------|
| LT-13 Silent install fix | ✅ | Already works - handles "silent" segment in protocol_service.rs |
| LT-18/LNX-04 Steam restart fix | ✅ | Implemented in steam_service.rs relaunch_steam_sls() |
| LNX-01 SLSsteam defaults | ✅ | play_not_owned defaults to true in config.rs |
| LT-74/LT-75 Fix ID colons | ✅ | is_safe_fix_id() allows colons in fixes_service.rs |
| LT-45 DonateKeys wire format | ✅ | Uses text/plain "depotId:hexKey,..." format in donate_keys.rs |
| LT-07 System tray | ⬜ | |
| LT-16 Steam language detection | ✅ | detect_steam_ui_language_code() in steam_service.rs |
| LT-34 DepotCache migration | ✅ | Implemented depot_cache_migration.rs |
| LT-44 Raw manifest fetch | ✅ | fetch_raw_manifest() in api_client.rs (line 191) |
| LT-60/61/62 Binary manifest codec | ✅ | manifest_binary.rs - magic headers, ZIP unwrap, protobuf, AES key validation |
| LT-58/59 LaunchModStore/Drift | ✅ | Implemented launch_mod_store.rs |
| LT-57 Steam process coordination | ✅ | Implemented in dialogs.rs |
| LT-76 AppliedFixIndex | ✅ | Implemented applied_fix_index.rs |
| LT-38 Download queue | ✅ | Implemented download_queue.rs, tests pass |
| LT-31 DLC checkboxes | ✅ | Implemented in views_workbench.rs with Checkbox for each disabled_entry |
| LT-32 Diff modal | ✅ | OverwriteDiffModal in app.rs/dialogs.rs shows unified diff before overwrite |
| LT-40 Credential encryption | ✅ | Implemented credential_encryption.rs with age (x25519/AES-GCM) |
| LNX-01 YAML fixes | ✅ | DepotKeys already serialized as quoted strings in slssteam_service.rs |
| LT-35/LNX-02 Hot reload sync | ⬜ | |
| LNX-09 ManifestBinding fix | ⬜ | |
| LNX-06 StubRemover swap | ✅ | launch_stub_remover() in external_tools.rs waits, backs up, replaces |
| LNX-08 Goldberg Windows DLLs | ✅ | Downloads emu-win-release.7z, extracts to goldberg_root for Proton |
| LNX-03 PlayUnowned fix | ✅ | play_unowned_service.rs syncs with SLSsteam config |
| LT-20 Steam autostart | ⬜ | |
| Dead code cleanup | ⬜ | |