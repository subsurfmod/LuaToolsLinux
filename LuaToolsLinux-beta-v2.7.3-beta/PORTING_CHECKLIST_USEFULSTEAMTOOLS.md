# Porting Checklist — usefulsteamtools features NOT yet implemented in LuatoolsLinux

**Source project:** `/home/tejas/usefulsteamtools/` (151 files, Windows C++ DLL — OpenSteamTool / BetterSteamTools)
**Target project:** `/home/tejas/LuaToolsLinux-GH/` (LuatoolsLinux v2.6.1, native Rust desktop app)

This file lists every usefulsteamtools function / capability that has **no equivalent** in LuatoolsLinux v2.6.1 and must be ported for the Linux port to fully replace the Windows unlocker.

> usefulsteamtools is an **injected Windows DLL** that hooks inside the Steam client process and manipulates Steam's memory/IPC at runtime. LuatoolsLinux is a **Rust GUI app** running outside Steam. Most items below require Linux-specific primitives (`dlopen`/`dlsym`, `ptrace`/`LD_PRELOAD`, `/proc`, `mprotect`, inotify, Unix domain sockets, ELF parsing) rather than a direct API translation.

---

## 1. Windows OS Abstraction Layer (OSTPlatform) — zero Linux equivalents

| usefulsteamtools function | Header | Purpose |
|---|---|---|
| `OSTPlatform::Detour::BeginTransaction()` | `include/Detour.h` | Detours transaction begin |
| `OSTPlatform::Detour::CommitTransaction()` | `include/Detour.h` | Detours transaction commit |
| `OSTPlatform::Detour::Attach(target, detour)` | `include/Detour.h` | Attach a detour hook |
| `OSTPlatform::Detour::Detach(target, detour)` | `include/Detour.h` | Detach a detour hook |
| `OSTPlatform::DynamicLibrary::Load(path)` | `include/DynamicLibrary.h` | Load a DLL (Win32 `LoadLibrary`) → Linux: `dlopen` |
| `OSTPlatform::DynamicLibrary::GetLoaded(name)` | `include/DynamicLibrary.h` | Get already-loaded module handle |
| `OSTPlatform::DynamicLibrary::GetSymbol(module, name)` | `include/DynamicLibrary.h` | Resolve exported symbol → Linux: `dlsym` |
| `OSTPlatform::DynamicLibrary::GetModuleDirectory(module)` | `include/DynamicLibrary.h` | Get a loaded module's directory |
| `OSTPlatform::DynamicLibrary::GetMainExecutablePath()` | `include/DynamicLibrary.h` | Get `steam.exe` path |
| `OSTPlatform::Http::Execute(method, url, ...)` | `include/Http.h` | WinHTTP request (LuatoolsLinux has `ureq`-based HTTP in services, but not this exact WinHTTP wrapper) |
| `OSTPlatform::Hash::Sha256OfFile(path)` | `include/Hash.h` | SHA-256 of a file (Win32 BCrypt) |
| `OSTPlatform::Hash::Sha256OfBuffer(data, size)` | `include/Hash.h` | SHA-256 of in-memory buffer |
| `OSTPlatform::Process::GetCreationTime(pid)` | `include/Process.h` | Process creation time |
| `OSTPlatform::Process::GetImagePath(pid)` | `include/Process.h` | Process executable path |
| `OSTPlatform::Process::GetEnvironmentVariableValue(pid, name)` | `include/Process.h` | Read another process's environment |
| `OSTPlatform::Process::GetProcessCommandLine(pid)` | `include/Process.h` | Process command line |
| `OSTPlatform::Process::EnumerateModules(pid)` | `include/Process.h` | Enumerate process modules |
| `OSTPlatform::RemoteProcess::InjectLibrary(pid, dllPath)` | `include/RemoteProcess.h` | DLL injection into remote process → Linux: `ptrace` / `LD_PRELOAD` |
| `OSTPlatform::RemoteProcess::GetArchitecture(pid)` | `include/RemoteProcess.h` | Get remote process architecture |
| `OSTPlatform::SteamCredentialStore::GetAppTicket(appId, &ticket)` | `include/SteamCredentialStore.h` | Read app ticket from Windows registry → Linux: `localconfig.vdf` |
| `OSTPlatform::SteamCredentialStore::WriteAppTicket(appId, data)` | `include/SteamCredentialStore.h` | Write app ticket to Windows registry |
| `OSTPlatform::SteamCredentialStore::GetETicket(appId, &ticket)` | `include/SteamCredentialStore.h` | Read encrypted ticket from registry |
| `OSTPlatform::SteamCredentialStore::WriteETicket(appId, data)` | `include/SteamCredentialStore.h` | Write encrypted ticket to registry |
| `OSTPlatform::SteamCredentialStore::GetSteamId(appId, &steamId)` | `include/SteamCredentialStore.h` | Read spoof SteamID from registry |
| `OSTPlatform::SteamCredentialStore::WriteSteamId(appId, steamId)` | `include/SteamCredentialStore.h` | Write spoof SteamID to registry |
| `OSTPlatform::Thread::StartDetached(entry)` | `include/Thread.h` | Start a detached thread (LuatoolsLinux has `std::thread`, but not this exact wrapper) |
| `OSTPlatform::Dialog::ShowWarning(title, msg)` | `include/Dialog.h` | Warning message box |
| `OSTPlatform::Dialog::ShowInfo(title, msg)` | `include/Dialog.h` | Info message box |
| `OSTPlatform::Dialog::ShowConfirm(title, msg)` | `include/Dialog.h` | Yes/No modal dialog |
| `OSTPlatform::Trap::Context::InstructionPointer()` | `include/Trap.h` | VEH trap context: instruction pointer (RIP) |
| `OSTPlatform::Trap::Context::Argument(index)` | `include/Trap.h` | Read Nth x64 calling-convention argument |
| `OSTPlatform::Trap::Context::EnableSingleStep()` | `include/Trap.h` | Enable single-step after a trap |
| `OSTPlatform::Memory::GetModuleImage(module)` | `include/Memory.h` | Get module base address + size |
| `OSTPlatform::Memory::WriteExecutableByte(target, value)` | `include/Memory.h` | Write a byte to executable memory |
| `OSTPlatform::ByteSearch::Find(bytes, pattern)` | `include/ByteSearch.h` | Byte pattern search in memory |
| `OSTPlatform::ByteSearch::FindInFile(path, pattern)` | `include/ByteSearch.h` | Byte pattern search in a file |
| `OSTPlatform::DirectoryWatch::*` (all functions) | `include/DirectoryWatch.h` | Windows `ReadDirectoryChangesW` → Linux: `inotify` (LuatoolsLinux `hot_reload_service.rs` uses inotify but is not integrated with Steam's `stplug-in` the same way) |
| `OSTPlatform::Encoding::WideToUtf8(value)` | `include/Encoding.h` | UTF-16 → UTF-8 |
| `OSTPlatform::Encoding::Utf8ToWide(value)` | `include/Encoding.h` | UTF-8 → UTF-16 |
| `OSTPlatform::Numbers::ParseUInt64/32/Int64/32(...)` | `include/Numbers.h` | Strict numeric parsing |

---

## 2. DLL Injection & Hooking — the core DRM-bypass / unlock layer

**None of these exist in LuatoolsLinux.** These hooks run inside the Steam client process and manipulate Steam's memory/IPC at runtime.

| usefulsteamtools function | File | Purpose |
|---|---|---|
| `InteropManager::SteamUI::CoreHook()` / `CoreUnhook()` | `Interop/InteropManager.cpp` | Hook / unhook SteamUI |
| `InteropManager::SteamClient::CoreHook()` / `CoreUnhook()` | `Interop/InteropManager.cpp` | Hook / unhook SteamClient |
| `InteropIPC::Install()` / `Uninstall()` | `Interop/Interop_IPC.cpp` | Hook `IPCProcessMessage` (named-pipe IPC interception) |
| `InteropIPC::RegisterHandlers(entries)` | `Interop/Interop_IPC.cpp` | Register pre/post IPC handlers by interface ID + func hash |
| `InteropIPC_ISteamUser::Register()` | `Interop/Interop_IPC_ISteamUser.cpp` | Register IClientUser IPC handlers |
| `HandlerPost_IClientUser_GetSteamID(...)` | `Interop/Interop_IPC_ISteamUser.cpp` | Spoof `GetSteamID` response (pool SteamID) |
| `HandlerPost_IClientUser_GetAppOwnershipTicketExtendedData(...)` | `Interop/Interop_IPC_ISteamUser.cpp` | Spoof ownership ticket response |
| `HandlerPost_IClientUser_RequestEncryptedAppTicket(...)` | `Interop/Interop_IPC_ISteamUser.cpp` | Cache `hAsyncCall` for on-demand eticket minting |
| `HandlerPost_IClientUser_GetEncryptedAppTicket(...)` | `Interop/Interop_IPC_ISteamUser.cpp` | Serve fresh / cached encrypted app ticket |
| `InteropIPC_ISteamUtils::Register()` | `Interop/Interop_IPC_ISteamUtils.cpp` | Register IClientUtils IPC handlers |
| `HandlerPost_IClientUtils_GetAppID(...)` | `Interop/Interop_IPC_ISteamUtils.cpp` | Spoof `GetAppID` response (restore real appid from 480) |
| `HandlerPost_IClientUtils_GetAPICallResult(...)` | `Interop/Interop_IPC_ISteamUtils.cpp` | Handle `EncryptedAppTicketResponse` callback → set `k_EResultOK` |
| `InteropMisc::Install()` / `Uninstall()` | `Interop/Interop_Misc.cpp` | Install SpawnProcess VEH trap + misc hooks |
| `InteropMisc::ResolveAppId()` | `Interop/Interop_Misc.cpp` | Resolve real AppId (OnlineFix or pipe binding) |
| `InteropMisc::IsOnlineFixActive()` | `Interop/Interop_Misc.cpp` | Is `-onlinefix` game active |
| `InteropMisc::NotifyNetworkingSocketsUsed()` | `Interop/Interop_Misc.cpp` | Mark SteamNetworkingSockets P2P as active |
| `InteropMisc::ShouldReportOnlineFixAppId()` | `Interop/Interop_Misc.cpp` | Should `GetAppID` report 480 for session-cert match |
| `InteropMisc::EnsureBufferCapacity(pWrite, newCapacity, updatePut)` | `Interop/Interop_Misc.cpp` | Grow a `CUtlBuffer` via steamclient |
| `InteropMisc::GetGameNameByAppID(appId)` | `Interop/Interop_Misc.cpp` | Get localized game name via `GetAppDataFromAppInfo` |
| VEH trap: `SpawnProcess` (`ARM_INT3_C`) | `Interop/Interop_Misc.cpp` | Intercept `CUser_SpawnProcess` to detect `-onlinefix` and rewrite AppId → 480 |
| VEH trap: `GetAppIDForCurrentPipe` | `Interop/Interop_Misc.cpp` | Capture the `SteamEngine` pointer |
| VEH trap: `GetAppDataFromAppInfo` | `Interop/Interop_Misc.cpp` | Capture the `CAppInfoCache` pointer |
| HOOK: `OptedInMask(pThis, appId)` | `Interop/Interop_Misc.cpp` | Rewrite Steam Input opt-in mask from 480 → real appid |
| HOOK: `BuildSpawnEnvBlock(...)` | `Interop/Interop_Misc.cpp` | Set overlay `CGameID` to real appid |
| HOOK: `GetOrAddAppData(pCache, appId, bCreate)` | `Interop/Interop_Misc.cpp` | Mark unresolved appinfo as `skip_flag` to bypass license-update blocking *(commented out in source)* |
| `InteropCallbacks::Install()` / `Uninstall()` | `Interop/Interop_Callbacks.cpp` | Hook `SendCallbackToPipe` (callback modifier — currently pass-through) |
| `InteropDecryption::Install()` / `Uninstall()` | `Interop/Interop_Decryption.cpp` | Hook `ConfigStoreGetBinary` to inject decryption keys |
| `InteropDecryption::ConfigStoreGetBinary(...)` | `Interop/Interop_Decryption.cpp` | Provide decryption key for a depot from `LuaConfig` |
| `InteropDecryption::GetCacheAppOwnershipTicket(appId)` | `Interop/Interop_Decryption.cpp` | Read cached app ownership ticket from local ConfigStore |
| `InteropDecryption::GetSeenAppTicketIds()` | `Interop/Interop_Decryption.cpp` | Get set of appids seen in ConfigStore |
| `InteropKeyValues::Install()` / `Uninstall()` | `Interop/Interop_KeyValues.cpp` | Hook `KeyValues::ReadAsBinary` *(not active — commented out)* |
| `InteropManifest::Install()` / `Uninstall()` | `Interop/Interop_Manifest.cpp` | Hook `BuildDepotDependency` + `YldLoadDepotManifest` |
| HOOK: `BuildDepotDependency(...)` | `Interop/Interop_Manifest.cpp` | Patch depot entry `gid`/`size` in place; fail on transient 0-depot window |
| HOOK: `YldLoadDepotManifest(a1, a2, appId, depotId, manifestGid, branch)` | `Interop/Interop_Manifest.cpp` | Pre-seed manifest cache before Steam's disk check |
| `InteropManifest::LookupDepot(depotId, &appId, &gid)` | `Interop/Interop_Manifest.cpp` | Look up depot → (appId, gid) from seen records |
| `InteropManifest::ReportMissingManifest(depotId, gid)` | `Interop/Interop_Manifest.cpp` | Show "manifests not ready" dialog |
| `InteropNetPacket::Install()` / `Uninstall()` | `Interop/Interop_NetPacket.cpp` | Hook `RecvPkt` / `BBuildAndAsyncSendFrame` |
| NetPacket layout probing (`TryResolveLayout` / `ProbeLayout`) | `Interop/Interop_NetPacket.cpp` | Auto-detect `CNetPacket` `m_pubData`/`m_cubData` offsets |
| `Hooks_NetPacket_AccessToken::HandleSend(pBody, cbBody)` | `Interop/Interop_NetPacket.cpp` | Inject access tokens into `CMsgClientPICSProductInfoRequest` |
| `Hooks_NetPacket_UserStats::HandleSend_GetUserStats(...)` | `Interop/Interop_NetPacket.cpp` | Spoof SteamID in `Player.GetUserStats` request |
| `Hooks_NetPacket_UserStats::HandleRecv_GetUserStatsResponse(...)` | `Interop/Interop_NetPacket.cpp` | Force `eresult=OK`, strip stats in `GetUserStats` response |
| `Hooks_NetPacket_UserStats::HandleSend_ClientGetUserStats(...)` | `Interop/Interop_NetPacket.cpp` | Force `schema_local_version=-1`, spoof `steam_id` |
| `Hooks_NetPacket_UserStats::HandleRecv_ClientGetUserStatsResponse(...)` | `Interop/Interop_NetPacket.cpp` | Clear stats/achievements, inject CloudRedirect blocks |
| `Hooks_NetPacket_UserStats::FillUserAchievements(appId, &resp, &nUnlocked)` | `Interop/Interop_NetPacket.cpp` | Rebuild `GetUserAchievements` from schema + local `.bin` |
| `Hooks_NetPacket_UserStats::QueueHeldUserAch(h)` | `Interop/Interop_NetPacket.cpp` | Queue held achievement reply |
| `Hooks_NetPacket_UserStats::DrainAchievements(pThis, pCarrier, invokeOriginal)` | `Interop/Interop_NetPacket.cpp` | Deliver queued rebuilt achievement replies |
| `Hooks_NetPacket_ETicket::HandleEncryptedAppTicketResponse(pBody, cbBody)` | `Interop/Interop_NetPacket.cpp` | Spoof `ClientRequestEncryptedAppTicketResponse` with owned ticket |
| `Hooks_NetPacket_OwnershipTicket::HandleRecv(pBody, cbBody)` | `Interop/Interop_NetPacket.cpp` | Spoof `MsgClientGetAppOwnershipTicketResponse` (eMsg 858) |
| `Hooks_NetPacket_Licenses::HandleRecv(pBody, cbBody)` | `Interop/Interop_NetPacket.cpp` | Parse `CMsgClientLicenseList`, hand to `InteropPackage` |
| `Hooks_NetPacket_ManifestProbe::RequestManifestCode(appId, depotId, gid)` | `Interop/Interop_NetPacket.cpp` | Request manifest request code via WebSocket |
| `InteropPackage::Install()` / `Uninstall()` | `Interop/Interop_Package.cpp` | Hook `CheckAppOwnership` |
| HOOK: `CheckAppOwnership(pObj, appId, pOwn)` | `Interop/Interop_Package.cpp` | Fake ownership: set `PackageId=0`, `bOwnsLicense=true`, `bFreeLicense=false` |
| `InteropPackage::NotifyLicenseChanged()` | `Interop/Interop_Package.cpp` | Add/remove depots from injected package, mark license changed |
| `InteropPackage::OnLicenseList(licenses)` | `Interop/Interop_Package.cpp` | Store license list from `CMsgClientLicenseList` |
| `InteropPackage::TryDumpOwnedDepots()` | `Interop/Interop_Package.cpp` | Resolve licenses to owned depot IDs |
| `InteropPackage::GetOwnedDepots()` | `Interop/Interop_Package.cpp` | Get owned depots (ready + list) |
| `InteropSteamUI::Install()` / `Uninstall()` | `Interop/Interop_SteamUI.cpp` | Hook SteamUI (`FillInAppOverview`, `BuildCompleteAppOverviewChange`, `CSteamUIAppControllerRunFrame`) |
| HOOK: `FillInAppOverview(pThis, pAppOverview, pApp)` | `Interop/Interop_SteamUI.cpp` | Track active downloads, set `PurchasedTime` from `LuaConfig` |
| HOOK: `BuildCompleteAppOverviewChange(pController, pChange, ...)` | `Interop/Interop_SteamUI.cpp` | Append removed appids to library delta |
| HOOK: `CSteamUIAppControllerRunFrame(pController)` | `Interop/Interop_SteamUI.cpp` | Clear ownership flags, mark app change (remove from library UI) |
| `InteropSteamUI::QueueRemoval(appId)` | `Interop/Interop_SteamUI.cpp` | Queue app removal from library |
| `InteropSteamUI::CancelRemoval(appId)` | `Interop/Interop_SteamUI.cpp` | Cancel queued removal |
| `InteropSteamUI::ActiveDownloadCount()` | `Interop/Interop_SteamUI.cpp` | Count active downloads |
| `PendingAPI::RecordEncryptedTicket(call, appID)` | `Interop/PendingAPI.cpp` | Record `hAsyncCall` → appId mapping |
| `PendingAPI::TakeEncryptedTicket(call)` | `Interop/PendingAPI.cpp` | Take and remove `hAsyncCall` → appId mapping |

---

## 3. Pipe / Process Management

| usefulsteamtools function | File | Purpose |
|---|---|---|
| `PipeManager::OnHandshake(pipe)` | `Pipe/PipeManager.cpp` | Handle new Steam named-pipe handshake: resolve process, appId, track game |
| `PipeManager::DenuvoAuth::Apply(ctx)` | `Pipe/Features/Auth/DenuvoAuth.cpp` | Run Denuvo authorization window for tracked pipe |
| `PipeManager::Integration::Apply(ctx)` | `Pipe/Features/Integration/Injection.cpp` | Inject DLLs into game process per config |

---

## 4. Credentials / Tickets

| usefulsteamtools function | File | Purpose |
|---|---|---|
| `AppTicket::GetSpoofSteamID(appId)` | `Utils/Credentials/AppTicket.cpp` | Get spoofed SteamID for app (from credential store) |
| `AppTicket::GetAppOwnershipTicketFromCredentialStore(appId)` | `Utils/Credentials/AppTicket.cpp` | Read cached ownership ticket |
| `AppTicket::GetEncryptedTicketFromCredentialStore(appId)` | `Utils/Credentials/AppTicket.cpp` | Read cached encrypted ticket |
| `AppTicket::ExtractSteamIdFromTicketBytes(ticket)` | `Utils/Credentials/AppTicket.cpp` | Extract SteamID from ticket bytes |
| `EticketClient::FetchFreshEticket(appId, nonce, existingSteamId)` | `Utils/Credentials/EticketClient.cpp` | Mint fresh nonce-bound eticket via backend |
| `EticketClient::FetchOwnershipTicket(appId, nonce, existingSteamId)` | `Utils/Credentials/EticketClient.cpp` | Mint ownership ticket via backend |
| `LegacyCDKey::Resolve(appId, accountId)` | `Utils/Credentials/LegacyCDKey.cpp` | Resolve / synthesize legacy CD key (`XXXX-XXXX-XXXX-XXXX`) |

---

## 5. CloudRedirect (Windows host library)

| usefulsteamtools function | File | Purpose |
|---|---|---|
| `CloudRedirectHost::Initialize(steamInstallPath)` | `Utils/CloudRedirect/CloudRedirectHost.cpp` | Load `cloud_redirect.dll`, resolve all CR exports, call `CR_InitCloudSave` |
| `CloudRedirectHost::SyncAppSet()` | `Utils/CloudRedirect/CloudRedirectHost.cpp` | Push current app set to CloudRedirect |
| `CloudRedirectHost::IsActive()` | `Utils/CloudRedirect/CloudRedirectHost.cpp` | Is CloudRedirect active |
| `CloudRedirectHost::IsApp(appId)` | `Utils/CloudRedirect/CloudRedirectHost.cpp` | Is app redirected |
| `CloudRedirectHost::HandleCloudRpc(method, appId, accountId, reqBody, reqLen, respBuf, respMaxLen, respLen, eresult)` | `Utils/CloudRedirect/CloudRedirectHost.cpp` | Handle Cloud RPC synchronously |
| `CloudRedirectHost::SetAccountId(accountId)` | `Utils/CloudRedirect/CloudRedirectHost.cpp` | Set account ID |
| `CloudRedirectHost::NotifyAppRunning(appId, running)` | `Utils/CloudRedirect/CloudRedirectHost.cpp` | Notify app running state |
| `CloudRedirectHost::NotifyStatsStored(appId)` | `Utils/CloudRedirect/CloudRedirectHost.cpp` | Notify stats stored |
| `CloudRedirectHost::GetAchievements(appId, out, maxBlocks)` | `Utils/CloudRedirect/CloudRedirectHost.cpp` | Get achievement blocks |
| `CloudRedirectHost::Shutdown()` | `Utils/CloudRedirect/CloudRedirectHost.cpp` | Shut down CloudRedirect |

> LuatoolsLinux has a separate `cloud_redirect_service.rs` but it is a higher-level service; it does not implement the `cloud_redirect.dll` host ABI (`CR_InitCloudSave`, `CR_HandleCloudRpc`, etc.).

---

## 6. TokenBridge (`bst://` URI scheme)

| usefulsteamtools function | File | Purpose |
|---|---|---|
| `TokenBridge::RegisterUriScheme(dllPath)` | `Utils/TokenBridge/TokenBridge.cpp` | Register `bst://` URI scheme in `HKCU` |
| `TokenBridge::HandleUri(rawUrl)` | `Utils/TokenBridge/TokenBridge.cpp` | Handle `bst://redeem/<code>` or `bst://refreshachievements/<appid>` |
| `TokenBridge::Redeem(code)` | `Utils/TokenBridge/TokenBridge.cpp` | Redeem code via backend, write app ticket + eticket to Steam |
| `TokenBridge::RefreshAchievements(arg)` | `Utils/TokenBridge/TokenBridge.cpp` | Delete `UserGameStatsSchema_*.bin` files |

---

## 7. Config / Lua file parsing & watching

| usefulsteamtools function | File | Purpose |
|---|---|---|
| `LuaConfig::ParseDirectory(dir)` | `Utils/Config/LuaConfig.cpp` | Parse all `.lua` files in a directory |
| `LuaConfig::ParseFile(path)` | `Utils/Config/LuaConfig.cpp` | Parse a single `.lua` file via Lua C API |
| `LuaConfig::HasDepot(appId)` | `Utils/Config/LuaConfig.cpp` | Check if depot is in config |
| `LuaConfig::GetDecryptionKey(depotId)` | `Utils/Config/LuaConfig.cpp` | Get decryption key for depot |
| `LuaConfig::GetAccessToken(appId)` | `Utils/Config/LuaConfig.cpp` | Get access token for app |
| `LuaConfig::GetStatSteamId(appId)` | `Utils/Config/LuaConfig.cpp` | Get stat SteamID for app |
| `LuaConfig::GetManifestOverrides()` | `Utils/Config/LuaConfig.cpp` | Get manifest gid/size overrides |
| `LuaConfig::GetAllDepotIds()` | `Utils/Config/LuaConfig.cpp` | Get all depot IDs |
| `LuaConfig::IsOwned(appId)` | `Utils/Config/LuaConfig.cpp` | Check if app is owned |
| `LuaConfig::GetPurchaseTime(appId)` | `Utils/Config/LuaConfig.cpp` | Get purchase time |
| `LuaConfig::GetAppIdForProcess(imageName)` | `Utils/Config/LuaConfig.cpp` | Map process name → appid |
| `LuaConfig::GetEticketUrl()` | `Utils/Config/LuaConfig.cpp` | Get eticket backend URL |
| `LuaConfig::MarkOwned(appId)` | `Utils/Config/LuaConfig.cpp` | Mark app as owned |
| `LuaConfig::TakePendingRemovals()` / `TakePendingAdditions()` | `Utils/Config/LuaConfig.cpp` | Get pending depot add/remove list |
| `LuaFileWatcher::Start(watchDirs)` / `Stop()` | `Utils/Config/LuaFileWatcher.cpp` | Watch `.lua` files for changes |
| `ConfigFileWatcher::Start(ConfigPath, LuaDir)` / `Stop()` | `Utils/Config/ConfigFileWatcher.cpp` | Watch `opensteamtool.toml` for changes |

> LuatoolsLinux has `src/parsers/lua_parser.rs` and `src/services/hot_reload_service.rs` but they do not implement the full `LuaConfig` registry + file-watching integration used by usefulsteamtools.

---

## 8. Steam Metadata

| usefulsteamtools function | File | Purpose |
|---|---|---|
| `IPCLoader::Load(steamclientPath)` | `Utils/SteamMetadata/IPCLoader.cpp` | Load IPC method metadata (interface ID, funcHash, fencepost, argc) from TOML |
| `IPCLoader::Find(interfaceID, funcHash)` | `Utils/SteamMetadata/IPCLoader.cpp` | Find IPC method by ID + hash |
| `IPCLoader::Find(interfaceName, methodName)` | `Utils/SteamMetadata/IPCLoader.cpp` | Find IPC method by name |
| `PatternLoader::Load(module, path, component)` | `Utils/SteamMetadata/PatternLoader.cpp` | Load patterns (SHA-256 of DLL, check cache, download from GitHub) |
| `PatternLoader::ReportMissingFunctions()` | `Utils/SteamMetadata/PatternLoader.cpp` | Report functions not found by pattern scan |
| `PatternLoader::FindPattern(module, funcHash)` | `Utils/SteamMetadata/PatternLoader.cpp` | Find function address via byte pattern |
| `ManifestClient::SetProvider(name)` | `Utils/SteamMetadata/ManifestClient.cpp` | Set manifest code provider |
| `ManifestClient::RequestCode(gid)` / `RequestCodeEx(app, depot, gid)` | `Utils/SteamMetadata/ManifestClient.cpp` | Request manifest code from provider |
| `ManifestDonor::Start()` | `Utils/SteamMetadata/ManifestDonor.cpp` | Start manifest donor service (fetch wanted list, mint codes) |
| `StatsClient::FetchStatSteamId(appId, &steamId)` | `Utils/SteamMetadata/StatsClient.cpp` | Fetch stat SteamID from `https://stats.luastools.xyz/<appid>` |
| `SteamDiagnostics::Initialize(steamclientPath, steamUIPath)` | `Utils/SteamMetadata/SteamDiagnostics.cpp` | Capture Steam build ID + DLL SHA-256 for diagnostics |
| `RemoteToml::Fetch(channel, component, sha256)` | `Utils/SteamMetadata/RemoteToml.cpp` | Fetch TOML from mirror chain (jsDelivr → git.lua.tools → GitHub raw) |

---

## 9. Achievements (BKV binary parsing)

| usefulsteamtools function | File | Purpose |
|---|---|---|
| `LocalAchievements::Read(appId, accountId)` | `Utils/Achievements/LocalAchievements.cpp` | Read `UserGameStats_<acct>_<app>.bin` (BKV format) |
| `LocalAchievements::BkvRead(data, len, pos, out, depth, totalNodes)` | `Utils/Achievements/LocalAchievements.cpp` | Parse Steam's binary KeyValues tree |

---

## 10. Self-Updater (Windows DLL replacement)

| usefulsteamtools function | File | Purpose |
|---|---|---|
| `AppUpdater::Check()` | `Utils/Update/AppUpdater.cpp` | Check for update via `latest.toml` (version + sha256) |
| `AppUpdater::DownloadAndStage(result, selfDllPath)` | `Utils/Update/AppUpdater.cpp` | Download new DLL, verify SHA-256 + MZ header, rename old DLL, write new bytes |
| `AppUpdater::CleanupStagedBackup(selfDllPath)` | `Utils/Update/AppUpdater.cpp` | Remove stale `.old` backup |
| `AppUpdater::RestartSteam()` | `Utils/Update/AppUpdater.cpp` | Gracefully restart Steam via PowerShell |

> LuatoolsLinux has `src/services/update_service.rs` but it uses `git pull` + `cargo build --release` + atomic `rename()` of the **executable binary**, not DLL replacement. This item is DLL-specific and has no direct Linux equivalent.

---

## Additional: Build-level breakage in usefulsteamtools

The usefulsteamtools project **does not compile as-is**. Its `src/CMakeLists.txt` and `src/Interop/InteropManager.cpp` reference ~23 files/headers that do not exist (naming mismatch):

- `Pipe/Features/Auth/Auth.cpp/.h` → actual: `DenuvoAuth.cpp/.h`, `ProtectionScan.cpp/.h`
- `Pipe/Features/Integration/Integration.cpp/.h` → actual: `Injection.cpp/.h`
- `Interop/InteropCallbacks.cpp/.h` → actual: `Interop_Callbacks.cpp/.h`
- `Interop/InteropDecryption.cpp/.h` → actual: `Interop_Decryption.cpp/.h`
- `Interop/InteropIPC.cpp/.h` → actual: `Interop_IPC.cpp/.h`
- `Interop/InteropKeyValues.cpp/.h` → actual: `Interop_KeyValues.cpp/.h`
- `Interop/InteropManifest.cpp/.h` → actual: `Interop_Manifest.cpp/.h`
- `Interop/InteropMisc.cpp/.h` → actual: `Interop_Misc.cpp/.h`
- `Interop/InteropNetPacket.cpp/.h` → actual: `Interop_NetPacket.cpp/.h`
- `Interop/InteropPackage.cpp/.h` → actual: `Interop_Package.cpp/.h`
- `Interop/InteropSteamUI.cpp/.h` → actual: `Interop_SteamUI.cpp/.h`

Plus `InteropManager.cpp` includes `Interops_CallBack.h` etc. (`Interops_` prefix, `CallBack` spelling) but actual headers are `Interop_Callbacks.h` etc.

And `OSTPlatform/Linux/` is empty (only `.gitkeep`) — no Linux platform backend exists.

---

## Suggested porting order

1. **OSTPlatform Linux backend** — `dlopen`/`dlsym`, `/proc` process introspection, `ptrace`/`LD_PRELOAD` injection, `mprotect` memory writes, ELF pattern scanning.
2. **Interop hook layer** — replace Windows Detours/VEH traps with Linux equivalents (`ptrace` breakpoints, `LD_PRELOAD` constructor, `dlopen` of `libsteam_client.so`).
3. **Credentials / tickets** — replace Windows registry `SteamCredentialStore` with `localconfig.vdf` read/write (Linux equivalent already exists in LuatoolsLinux `steam_bridge/ticket_store.rs`).
4. **IPC / NetPacket hooks** — replace Windows named pipes with Unix domain sockets (LuatoolsLinux `steam_bridge/ipc.rs` exists but needs the Steam client integration).
5. **Manifest / depot overrides** — `BuildDepotDependency`, `YldLoadDepotManifest`, `CheckAppOwnership`.
6. **TokenBridge / CloudRedirect / Updater** — high-level features built on top of the above.
