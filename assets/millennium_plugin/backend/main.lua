--[[
  LuaToolsLinux — Millennium v3.6.3 Native Lua Backend
  Plugin: LuaToolsLinux
  Author: subsurfmod (https://github.com/subsurfmod/LuaToolsLinux)
  Executed inside /usr/lib/millennium/libmillennium_luavm_x64
  Handles RPC dispatch to local LuaTools Linux companion daemon (127.0.0.1:<port>),
  automatic companion wake, and offline fallback.
--]]

local millennium = require("millennium")
local http = require("http")
local logger = require("logger")

-- Millennium v3.6.3 logger uses method syntax (`logger:info(msg)` -> self at stack #1, msg at stack #2).
-- Always invoke via pcall-guarded colon helpers so logger mismatches can never abort execution.
local function log_info(msg)
    if logger then
        pcall(function() logger:info(tostring(msg)) end)
    end
end

local function log_warn(msg)
    if logger then
        pcall(function() logger:warn(tostring(msg)) end)
    end
end

local function log_error(msg)
    if logger then
        pcall(function() logger:error(tostring(msg)) end)
    end
end

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
    version = "4.1.6",
    default_port = 6767,
    port = 6767,
    token = nil,
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

-- Discover per-UID companion endpoint from $XDG_RUNTIME_DIR or ~/.config/luatools/luatools_rpc.json
-- (Steam Runtime 3 pressure-vessel isolates /run/user/<uid>, while ~/.config/luatools is shared.)
local function discover_endpoint()
    local uid = get_current_uid()
    local xdg_runtime = os.getenv("XDG_RUNTIME_DIR")
    local home = get_home_dir()
    local paths = {}
    if xdg_runtime and xdg_runtime ~= "" then
        table.insert(paths, xdg_runtime .. "/luatools_rpc.json")
    end
    if uid and uid ~= "" then
        table.insert(paths, "/run/user/" .. uid .. "/luatools_rpc.json")
    end
    if home and home ~= "" and home ~= "." then
        table.insert(paths, home .. "/.config/luatools/luatools_rpc.json")
    end
    table.insert(paths, "/tmp/luatools_rpc.json")

    for _, path in ipairs(paths) do
        local f = io.open(path, "r")
        if f then
            local content = f:read("*a")
            f:close()
            if content and content ~= "" then
                local data = json.decode(content)
                if type(data) == "table" and data.port then
                    local new_endpoint = "http://127.0.0.1:" .. tostring(data.port)
                    local new_token = data.token or ""
                    local changed = (luatools_backend.endpoint ~= new_endpoint)
                        or (luatools_backend.token ~= new_token)
                        or (not luatools_backend.connected)
                    luatools_backend.port = data.port
                    luatools_backend.token = new_token
                    luatools_backend.endpoint = new_endpoint
                    luatools_backend.connected = true
                    if changed then
                        log_info("[LuaToolsLinux by subsurfmod] Discovered companion endpoint on " .. luatools_backend.endpoint .. " via " .. path)
                    end
                    return true
                end
            end
        end
    end

    if luatools_backend.connected or luatools_backend.token ~= "" then
        log_warn("[LuaToolsLinux] Could not find luatools_rpc.json in runtime or ~/.config/luatools; falling back to default port " .. tostring(luatools_backend.default_port))
    end
    luatools_backend.port = luatools_backend.default_port
    luatools_backend.token = ""
    luatools_backend.connected = false
    luatools_backend.endpoint = "http://127.0.0.1:" .. tostring(luatools_backend.default_port)
    return false
end

-- Forward RPC call to LuaTools HTTP server and return a Lua table
-- (libmillennium_luavm_x64 serializes returned Lua tables directly to JSON objects).
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

    -- Millennium v3.6.3 Lua_HttpPost pushes a synthetic table without replacing stack #2,
    -- causing Lua_HttpRequest (which reads stack #1 & #2) to default to HTTP GET if method
    -- is omitted from arg #2. Always call http.request (or pass method = "POST" in arg #2).
    local http_fn = http.request or http.post
    local ok, response, err = pcall(http_fn, luatools_backend.endpoint .. "/rpc", {
        method = "POST",
        data = payload,
        headers = headers,
        timeout = 10
    })

    if not ok then
        err = response
        response = nil
    end

    if err or type(response) ~= "table" or type(response.status) ~= "number" then
        local reason = tostring(err or "invalid HTTP response")
        log_warn("[LuaToolsLinux] RPC call " .. tostring(method) .. " failed: " .. reason)
        return {
            success = false,
            error = "Companion daemon unreachable: " .. reason
        }
    end

    if response.status >= 200 and response.status < 300 then
        local decoded = json.decode(response.body)
        if type(decoded) == "table" then
            return decoded
        end
        log_error("[LuaToolsLinux] RPC call " .. tostring(method) .. " returned non-JSON body")
        return {
            success = false,
            error = "Invalid JSON response from companion daemon",
            raw = response.body
        }
    else
        log_warn("[LuaToolsLinux] RPC call " .. tostring(method) .. " returned HTTP " .. tostring(response.status) .. ": " .. tostring(response.body))
        local decoded = json.decode(response.body)
        if type(decoded) == "table" then
            if decoded.success == nil then
                decoded.success = false
            end
            if not decoded.error then
                decoded.error = "HTTP error " .. tostring(response.status)
            end
            return decoded
        end
        return {
            success = false,
            error = "HTTP error " .. tostring(response.status) .. ": " .. tostring(response.body)
        }
    end
end

-- Zero-latency offline fallback checking local stplug-in files (native + Flatpak Steam)
local function local_has_lua_fallback(appid)
    if not appid or appid == 0 then return false, false end
    local home = get_home_dir()
    local id_str = tostring(appid)
    local dirs = {
        home .. "/.local/share/Steam/config/stplug-in/",
        home .. "/.local/share/Steam/stplug-in/",
        home .. "/.var/app/com.valvesoftware.Steam/.local/share/Steam/config/stplug-in/",
        home .. "/.var/app/com.valvesoftware.Steam/.local/share/Steam/stplug-in/"
    }
    for _, dir in ipairs(dirs) do
        local f = io.open(dir .. id_str .. ".lua", "r")
        if f then
            f:close()
            return true, false
        end
    end
    for _, dir in ipairs(dirs) do
        local f = io.open(dir .. id_str .. ".lua.disabled", "r")
        if f then
            f:close()
            return false, true
        end
    end
    return false, false
end

-- Millennium Lifecycle Callbacks
-- Millennium v3.6.3 natively loads `.millennium/Dist/webkit.js`, so we do NOT call
-- `add_browser_js("public/luatools.js")` or `add_browser_css("public/luatools.css")`
-- (which resolve under `themes/public/...` and 404).
function on_load()
    log_info("[LuaToolsLinux] Native Millennium plugin loading (v" .. luatools_backend.version .. " by subsurfmod)")
    discover_endpoint()

    if millennium and millennium.ready then
        pcall(millennium.ready)
    end
    log_info("[LuaToolsLinux] Plugin ready and registered for Millennium")
end

function on_frontend_loaded()
    discover_endpoint()
    log_info("[LuaToolsLinux] Frontend loaded, checking companion status...")
end

function on_unload()
    log_info("[LuaToolsLinux] Unloading plugin")
    if type(luatools_backend.browser_js_hook) == "number" and millennium and millennium.remove_browser_module then
        pcall(millennium.remove_browser_module, luatools_backend.browser_js_hook)
    end
    if type(luatools_backend.browser_css_hook) == "number" and millennium and millennium.remove_browser_module then
        pcall(millennium.remove_browser_module, luatools_backend.browser_css_hook)
    end
end

-- ============================================================================
-- Exported Methods for Millennium.callServerMethod("LuaToolsLinux", method, [args])
-- Accepts BOTH a single table/JSON-object argument AND positional arguments!
-- ============================================================================

local function parse_args(positional_keys, ...)
    local n = select("#", ...)
    if n == 0 then
        return {}
    end

    local first = select(1, ...)
    if n == 1 then
        if type(first) == "table" then
            -- Unwrap single-element wrapper array `[ { ... } ]` when called as callServerMethod(plugin, method, [args])
            if type(first[1]) == "table" and next(first, 1) == nil then
                local has_named = false
                for k, _ in pairs(first) do
                    if type(k) == "string" then
                        has_named = true
                        break
                    end
                end
                if not has_named then
                    return first[1]
                end
            end
            return first
        elseif type(first) == "string" and first ~= "" then
            local trimmed = first:match("^%s*(.-)%s*$")
            local lead = trimmed:sub(1, 1)
            if lead == "{" or lead == "[" then
                local decoded = json.decode(trimmed)
                if type(decoded) == "table" then
                    return decoded
                end
            end
        end
    end

    local out = {}
    if type(positional_keys) == "table" then
        for i, key in ipairs(positional_keys) do
            if i <= n then
                local v = select(i, ...)
                if v ~= nil then
                    out[key] = v
                end
            end
        end
    elseif type(first) == "table" then
        return first
    end
    return out
end

function HasLuaToolsForApp(...)
    local params = parse_args({ "appid" }, ...)
    local appid = tonumber(params.appid or params.appId or 0) or 0

    local rpc_res = rpc_call("HasLuaToolsForApp", { appid = appid })
    if type(rpc_res) == "table" and rpc_res.success then
        return rpc_res
    end

    -- Fallback to local check
    local fallback_exists, fallback_disabled = local_has_lua_fallback(appid)
    return {
        success = true,
        exists = fallback_exists,
        has = fallback_exists,
        disabled = fallback_disabled,
        depotCount = 0,
        dlcCount = 0,
        source = "local_fs_fallback"
    }
end

function CheckSources(...)
    return rpc_call("CheckSources", parse_args({ "appid" }, ...))
end

function StartAddViaLuaTools(...)
    return rpc_call("StartAddViaLuaTools", parse_args({ "appid", "name", "source" }, ...))
end

function GetAddViaLuaToolsStatus(...)
    return rpc_call("GetAddViaLuaToolsStatus", parse_args({ "appid" }, ...))
end

function CancelAddViaLuaTools(...)
    return rpc_call("CancelAddViaLuaTools", parse_args({ "appid" }, ...))
end

function DeleteLuaToolsForApp(...)
    return rpc_call("DeleteLuaToolsForApp", parse_args({ "appid" }, ...))
end

function ToggleAppEnabled(...)
    return rpc_call("ToggleAppEnabled", parse_args({ "appid", "enabled" }, ...))
end

function CheckForFixes(...)
    return rpc_call("CheckForFixes", parse_args({ "appid" }, ...))
end

function ApplyRecommendedFixes(...)
    return rpc_call("ApplyRecommendedFixes", parse_args({ "appid", "fixId" }, ...))
end

function GetApplyFixStatus(...)
    return rpc_call("GetApplyFixStatus", parse_args({ "appid" }, ...))
end

function GetInstalledFixes(...)
    return rpc_call("GetInstalledFixes", parse_args({ "appid" }, ...))
end

function RemoveAppliedFixes(...)
    return rpc_call("RemoveAppliedFixes", parse_args({ "appid" }, ...))
end

function OpenFixInDesktopApp(...)
    return rpc_call("OpenFix", parse_args({ "appid" }, ...))
end

function SyncDlcForApp(...)
    return rpc_call("SyncDlcForApp", parse_args({ "appid" }, ...))
end

function RestartSteam(...)
    return rpc_call("RestartSteam", parse_args(nil, ...))
end

function GetCompanionStatus(...)
    return rpc_call("GetCompanionStatus", parse_args(nil, ...))
end

local function shell_quote(s)
    return "'" .. tostring(s):gsub("'", "'\\''") .. "'"
end

function WakeCompanion(...)
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
    return { success = true, waking = true }
end

function GetInstalledLuaScripts(...)
    return rpc_call("GetInstalledLuaScripts", parse_args(nil, ...))
end

function SetLoadedApps(...)
    local params = parse_args({ "apps" }, ...)
    if type(params) == "table" and params[1] ~= nil and params.apps == nil and params.appIds == nil then
        params = { apps = params }
    end
    return rpc_call("SetLoadedApps", params)
end

function SyncSlsSteamConfig(...)
    return rpc_call("SyncSlsSteamConfig", parse_args(nil, ...))
end

function GetSettingsConfig(...)
    return rpc_call("GetSettingsConfig", parse_args(nil, ...))
end

function UpdateSettingValue(...)
    return rpc_call("UpdateSettingValue", parse_args({ "key", "value" }, ...))
end

function GetApiList(...)
    return rpc_call("GetApiList", parse_args(nil, ...))
end

function CheckUpdates(...)
    return rpc_call("CheckUpdates", parse_args(nil, ...))
end

function OpenSettings(...)
    return rpc_call("OpenSettings", parse_args(nil, ...))
end

function GetTranslations(...)
    return rpc_call("GetTranslations", parse_args({ "lang" }, ...))
end

function GetBridgeDiagnostics(...)
    return rpc_call("GetBridgeDiagnostics", parse_args(nil, ...))
end

function GetVersion(...)
    return {
        pluginName = luatools_backend.name,
        pluginAuthor = luatools_backend.author,
        pluginVersion = luatools_backend.version,
        backendType = "lua",
        millenniumVersion = "3.6.3"
    }
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
