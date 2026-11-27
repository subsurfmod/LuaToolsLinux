// LuaToolsLinux - Millennium Client UI Bundle
// Plugin: LuaToolsLinux
// Author: subsurfmod (https://github.com/subsurfmod/LuaToolsLinux)
// Loaded in Steam Client SharedJSContext (steamloopback.host)
(function () {
  'use strict';
  console.log('[LuaToolsLinux] Initializing Steam Client UI bundle (v4.1.2 by subsurfmod)');

  const PLUGIN_NAME = 'LuaToolsLinux';

  async function callBackend(method, args) {
    if (window.Millennium && window.Millennium.callServerMethod) {
      try {
        let res;
        try {
          res = await window.Millennium.callServerMethod(PLUGIN_NAME, method, args || {});
        } catch (e) {
          res = await window.Millennium.callServerMethod('luatools', method, args || {});
        }
        return typeof res === 'string' ? JSON.parse(res) : res;
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
    version: '4.1.2',
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
