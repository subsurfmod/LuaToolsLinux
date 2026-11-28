// LuaToolsLinux - Millennium Client UI Bundle
// Plugin: LuaToolsLinux
// Author: subsurfmod (https://github.com/subsurfmod/LuaToolsLinux)
// Loaded in Steam Client SharedJSContext (steamloopback.host)
(function () {
  'use strict';
  console.log('[LuaToolsLinux] Initializing Steam Client UI bundle (v4.1.4 by subsurfmod)');

  const PLUGIN_NAME = 'LuaToolsLinux';

  function parseRpcResponse(res) {
    let parsed = res;
    for (let i = 0; i < 2 && typeof parsed === 'string'; i++) {
      try {
        parsed = JSON.parse(parsed);
      } catch (e) {
        return i === 0 ? { raw: parsed } : parsed;
      }
    }
    return parsed;
  }

  async function callBackend(method, args) {
    if (window.Millennium && window.Millennium.callServerMethod) {
      try {
        const res = await window.Millennium.callServerMethod(PLUGIN_NAME, method, [args || {}]);
        return parseRpcResponse(res);
      } catch (e) {
        console.error('[LuaToolsLinux] Backend call error:', method, e);
        return null;
      }
    }
    return null;
  }

  window.LuaToolsLinux = {
    name: 'LuaToolsLinux',
    author: 'subsurfmod',
    version: '4.1.4',
    callBackend: callBackend,
    openWorkbench: () => callBackend('OpenSettings', {}),
    syncSlsSteam: () => callBackend('SyncSlsSteamConfig', {}),
    restartSteam: () => callBackend('RestartSteam', {}),
    getStatus: () => callBackend('GetCompanionStatus', {})
  };

  // Alias for backward compatibility
  window.LuaToolsClient = window.LuaToolsLinux;

  console.log('[LuaToolsLinux] Exposed window.LuaToolsLinux to SharedJSContext');
})();
