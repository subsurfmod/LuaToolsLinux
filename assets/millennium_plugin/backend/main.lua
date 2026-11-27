--[[
  LuaToolsLinux — Millennium v3.6+ Native Lua Backend
  Plugin: LuaToolsLinux
  Author: subsurfmod (https://github.com/subsurfmod/LuaToolsLinux)
  Executed inside /usr/lib/millennium/libmillennium_luavm_x64
  Handles RPC dispatch to local LuaTools Linux companion daemon (127.0.0.1:<port>),
  automatic companion wake, Webkit browser registration, and offline fallback.
--]]

local millennium = require("millennium")
local http = require("http")
local logger = require("logger")

-- Millennium v3.6+ preloads lua-cjson under require("json") (not "cjson.safe").
-- Wrap encode/decode with pcall so malformed JSON never crashes the Lua host.
local raw_json = nil
for _, mod_name in ipairs({ "json", "cjson.safe", "cjson" }) do
    local ok, mod = pcall(require, mod_name)
    if ok and type(mod) == "table" then
        raw_json = mod
        break
    end
end

local json = {
    decode = function(str)
        if not raw_json or type(str) ~= "string" or str == "" then
            return nil
        end
        local ok, res = pcall(raw_json.decode, str)
        if ok then
            return res
        end
        return nil
    end,
    encode = function(val)
        if not raw_json then
            return "{}"
        end
        local ok, res = pcall(raw_json.encode, val)
        if ok then
            return res
        end
        return "{}"
    end
}

local luatools_backend = {
    name = "LuaToolsLinux",
    author = "subsurfmod",
    version = "4.1.2",
    default_port = 6767,
    port = 6767,
    token = "",
    connected = false,
    endpoint = "http://127.0.0.1:6767",
    browser_js_hook = nil,
    browser_css_hook = nil
}

-- Resolve home directory without hardcoding any user path
local function get_home_dir()
    local h = os.getenv("HOME")
    if h and h ~= "" then return h end
    local u = os.getenv("USER")
    if u and u ~= "" then return "/home/" .. u end
    local p = io.popen("echo -n ~ 2>/dev/null")
    if p then
        local out = p:read("*a")
        p:close()
        if out and out ~= "" and out ~= "~" then return out end
    end
    return "."
end

-- Read real UID from Linux /proc/self/status (pure filesystem, independent of shell vars)
local function get_current_uid()
    local f = io.open("/proc/self/status", "r")
    if f then
        for line in f:lines() do
            local uid_match = line:match("^Uid:%s+(%d+)")
            if uid_match then
                f:close()
                return uid_match
            end
        end
        f:close()
    end
    -- Fallback via id -u
    local p = io.popen("id -u 2>/dev/null")
    if p then
        local out = p:read("*l")
        p:close()
        if out and out:match("^%d+$") then return out end
    end
    return nil
end

-- Discover per-UID companion endpoint from $XDG_RUNTIME_DIR/luatools_rpc.json
local function discover_endpoint()
    local uid = get_current_uid()
    local xdg_runtime = os.getenv("XDG_RUNTIME_DIR")
    local paths = {}
    if xdg_runtime and xdg_runtime ~= "" then
        table.insert(paths, xdg_runtime .. "/luatools_rpc.json")
    end
    if uid and uid ~= "" then
        table.insert(paths, "/run/user/" .. uid .. "/luatools_rpc.json")
    end
    table.insert(paths, "/tmp/luatools_rpc.json")

    for _, path in ipairs(paths) do
        local f = io.open(path, "r")
        if f then
            local content = f:read("*a")
            f:close()
            if content and content ~= "" then
                local data = json.decode(content)
                if data and data.port then
                    luatools_backend.port = data.port
                    luatools_backend.token = data.token or ""
                    luatools_backend.endpoint = "http://127.0.0.1:" .. tostring(data.port)
                    logger.info("[LuaToolsLinux by subsurfmod] Discovered companion endpoint on " .. luatools_backend.endpoint)
                    return true
                end
            end
        end
    end

    luatools_backend.port = luatools_backend.default_port
    luatools_backend.token = nil
    luatools_backend.endpoint = "http://127.0.0.1:" .. tostring(luatools_backend.default_port)
    return false
end

-- Forward RPC call to LuaTools HTTP server
local function rpc_call(method, params)
    discover_endpoint()
    local payload = json.encode({
        method = method,
        args = params or {},
        params = params or {}
    })

    local headers = {
        ["Content-Type"] = "application/json",
        ["Accept"] = "application/json"
    }
    if luatools_backend.token and luatools_backend.token ~= "" then
        headers["X-LuaTools-Token"] = luatools_backend.token
    end

    local ok, response, err = pcall(http.post, luatools_backend.endpoint .. "/rpc", {
        data = payload,
        body = payload,
        headers = headers,
        timeout = 10
    })

    if not ok then
        err = response
        response = nil
    end

    if err or not response then
        logger.warn("[LuaToolsLinux] RPC call " .. method .. " failed: " .. tostring(err))
        return json.encode({
            success = false,
            error = "Companion daemon unreachable: " .. tostring(err)
        })
    end

    if response.status >= 200 and response.status < 300 then
        return response.body
    else
        return json.encode({
            success = false,
            error = "HTTP error " .. tostring(response.status) .. ": " .. tostring(response.body)
        })
    end
end

-- Zero-latency offline fallback checking local stplug-in files
local function local_has_lua_fallback(appid)
    if not appid or appid == 0 then return false end
    local home = get_home_dir()
    local paths = {
        home .. "/.local/share/Steam/config/stplug-in/" .. tostring(appid) .. ".lua",
        home .. "/.local/share/Steam/stplug-in/" .. tostring(appid) .. ".lua"
    }
    for _, path in ipairs(paths) do
        local f = io.open(path, "r")
        if f then
            f:close()
            return true
        end
    end
    return false
end

-- Millennium Lifecycle Callbacks
function on_load()
    logger.info("[LuaToolsLinux] Native Millennium plugin loading (v" .. luatools_backend.version .. " by subsurfmod)")
    discover_endpoint()

    -- Inject Store scripts via Millennium Webkit API
    if millennium.add_browser_js then
        local ok_js, hook_js = pcall(millennium.add_browser_js, "public/luatools.js")
        if ok_js then
            luatools_backend.browser_js_hook = hook_js
        else
            logger.warn("[LuaToolsLinux] add_browser_js warning: " .. tostring(hook_js))
        end
    end
    if millennium.add_browser_css then
        local ok_css, hook_css = pcall(millennium.add_browser_css, "public/luatools.css")
        if ok_css then
            luatools_backend.browser_css_hook = hook_css
        else
            logger.warn("[LuaToolsLinux] add_browser_css warning: " .. tostring(hook_css))
        end
    end

    if millennium.ready then
        millennium.ready()
    end
    logger.info("[LuaToolsLinux] Plugin ready and registered for Millennium")
end

function on_frontend_loaded()
    discover_endpoint()
    logger.info("[LuaToolsLinux] Frontend loaded, checking companion status...")
end

function on_unload()
    logger.info("[LuaToolsLinux] Unloading plugin")
    if luatools_backend.browser_js_hook and millennium.remove_browser_module then
        millennium.remove_browser_module(luatools_backend.browser_js_hook)
    end
    if luatools_backend.browser_css_hook and millennium.remove_browser_module then
        millennium.remove_browser_module(luatools_backend.browser_css_hook)
    end
end

-- ============================================================================
-- Exported Methods for Millennium.callServerMethod("LuaToolsLinux", method, args)
-- ============================================================================

local function parse_args(args)
    if type(args) == "table" then
        return args
    elseif type(args) == "string" and args ~= "" then
        local decoded = json.decode(args)
        if type(decoded) == "table" then return decoded end
    end
    return {}
end

function HasLuaToolsForApp(args)
    local params = parse_args(args)
    local appid = tonumber(params.appid or params.appId or 0)
    
    local rpc_res = rpc_call("HasLuaToolsForApp", { appid = appid })
    local parsed = json.decode(rpc_res)
    if parsed and parsed.success then
        return rpc_res
    end

    -- Fallback to local check
    local fallback_exists = local_has_lua_fallback(appid)
    return json.encode({
        success = true,
        exists = fallback_exists,
        has = fallback_exists,
        disabled = false,
        depotCount = 0,
        dlcCount = 0,
        source = "local_fs_fallback"
    })
end

function CheckSources(args)
    return rpc_call("CheckSources", parse_args(args))
end

function StartAddViaLuaTools(args)
    return rpc_call("StartAddViaLuaTools", parse_args(args))
end

function GetAddViaLuaToolsStatus(args)
    return rpc_call("GetAddViaLuaToolsStatus", parse_args(args))
end

function CancelAddViaLuaTools(args)
    return rpc_call("CancelAddViaLuaTools", parse_args(args))
end

function DeleteLuaToolsForApp(args)
    return rpc_call("DeleteLuaToolsForApp", parse_args(args))
end

function ToggleAppEnabled(args)
    return rpc_call("ToggleAppEnabled", parse_args(args))
end

function CheckForFixes(args)
    return rpc_call("CheckForFixes", parse_args(args))
end

function ApplyRecommendedFixes(args)
    return rpc_call("ApplyRecommendedFixes", parse_args(args))
end

function GetApplyFixStatus(args)
    return rpc_call("GetApplyFixStatus", parse_args(args))
end

function GetInstalledFixes(args)
    return rpc_call("GetInstalledFixes", parse_args(args))
end

function RemoveAppliedFixes(args)
    return rpc_call("RemoveAppliedFixes", parse_args(args))
end

function OpenFixInDesktopApp(args)
    return rpc_call("OpenFix", parse_args(args))
end

function SyncDlcForApp(args)
    return rpc_call("SyncDlcForApp", parse_args(args))
end

function RestartSteam(args)
    return rpc_call("RestartSteam", parse_args(args))
end

function GetCompanionStatus(args)
    return rpc_call("GetCompanionStatus", parse_args(args))
end

local function shell_quote(s)
    return "'" .. tostring(s):gsub("'", "'\\''") .. "'"
end

function WakeCompanion(args)
    local home = get_home_dir()
    local candidates = {
        home .. "/.local/bin/luatools",
        "/usr/local/bin/luatools",
        "/usr/bin/luatools"
    }
    local bin = "luatools"
    for _, c in ipairs(candidates) do
        local f = io.open(c, "r")
        if f then
            f:close()
            bin = c
            break
        end
    end
    os.execute(shell_quote(bin) .. " --minimized --tray-locked >/dev/null 2>&1 &")
    return json.encode({ success = true, waking = true })
end

function GetInstalledLuaScripts(args)
    return rpc_call("GetInstalledLuaScripts", parse_args(args))
end

function SetLoadedApps(args)
    return rpc_call("SetLoadedApps", parse_args(args))
end

function SyncSlsSteamConfig(args)
    return rpc_call("SyncSlsSteamConfig", parse_args(args))
end

function GetSettingsConfig(args)
    return rpc_call("GetSettingsConfig", parse_args(args))
end

function UpdateSettingValue(args)
    return rpc_call("UpdateSettingValue", parse_args(args))
end

function GetApiList(args)
    return rpc_call("GetApiList", parse_args(args))
end

function CheckUpdates(args)
    return rpc_call("CheckUpdates", parse_args(args))
end

function OpenSettings(args)
    return rpc_call("OpenSettings", parse_args(args))
end

function GetTranslations(args)
    return rpc_call("GetTranslations", parse_args(args))
end

function GetBridgeDiagnostics(args)
    return rpc_call("GetBridgeDiagnostics", parse_args(args))
end

function GetVersion(args)
    return json.encode({
        pluginName = luatools_backend.name,
        pluginAuthor = luatools_backend.author,
        pluginVersion = luatools_backend.version,
        backendType = "lua",
        millenniumVersion = "3.6.3"
    })
end

-- Millennium v3.6+ lua-host requires main.lua to return a lifecycle/export table
return {
    on_load = on_load,
    on_unload = on_unload,
    on_frontend_loaded = on_frontend_loaded,
    HasLuaToolsForApp = HasLuaToolsForApp,
    CheckSources = CheckSources,
    StartAddViaLuaTools = StartAddViaLuaTools,
    GetAddViaLuaToolsStatus = GetAddViaLuaToolsStatus,
    CancelAddViaLuaTools = CancelAddViaLuaTools,
    DeleteLuaToolsForApp = DeleteLuaToolsForApp,
    ToggleAppEnabled = ToggleAppEnabled,
    CheckForFixes = CheckForFixes,
    ApplyRecommendedFixes = ApplyRecommendedFixes,
    GetApplyFixStatus = GetApplyFixStatus,
    GetInstalledFixes = GetInstalledFixes,
    RemoveAppliedFixes = RemoveAppliedFixes,
    OpenFixInDesktopApp = OpenFixInDesktopApp,
    SyncDlcForApp = SyncDlcForApp,
    RestartSteam = RestartSteam,
    GetCompanionStatus = GetCompanionStatus,
    WakeCompanion = WakeCompanion,
    GetInstalledLuaScripts = GetInstalledLuaScripts,
    SetLoadedApps = SetLoadedApps,
    SyncSlsSteamConfig = SyncSlsSteamConfig,
    GetSettingsConfig = GetSettingsConfig,
    UpdateSettingValue = UpdateSettingValue,
    GetApiList = GetApiList,
    CheckUpdates = CheckUpdates,
    OpenSettings = OpenSettings,
    GetTranslations = GetTranslations,
    GetBridgeDiagnostics = GetBridgeDiagnostics,
    GetVersion = GetVersion
}
