// LuaToolsLinux - Millennium v3.6.3 Canonical WebKit Injection Bundle
// Plugin: LuaToolsLinux (v4.1.5)
// Author: subsurfmod (https://github.com/subsurfmod/LuaToolsLinux)
// Loaded natively by Millennium v3.6.3 from .millennium/Dist/webkit.js
(function () {
  'use strict';

  if (
    typeof window === 'undefined' ||
    !window.location ||
    window.location.hostname !== 'store.steampowered.com'
  ) {
    return;
  }

  if (window.__luatoolsStorePluginInjected) return;
  window.__luatoolsStorePluginInjected = true;
  console.log('[LuaToolsLinux] Store WebKit bundle active on', window.location.href);

  const LUATOOLS_STORE_CSS = `/* LuaTools Linux - Steam Store Webkit Stylesheet */
#luatools-store-container {
  margin: 16px 0;
  padding: 16px;
  background: linear-gradient(135deg, rgba(27, 40, 56, 0.95) 0%, rgba(16, 24, 34, 0.95) 100%);
  border: 1px solid #2a475e;
  border-left: 4px solid #5c7e10;
  border-radius: 4px;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.4);
  font-family: "Motiva Sans", -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
  color: #c6d4df;
  position: relative;
  z-index: 100;
}

#luatools-store-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 12px;
}

.luatools-title {
  font-size: 15px;
  font-weight: 700;
  color: #ffffff;
  display: flex;
  align-items: center;
  gap: 8px;
}

.luatools-title svg {
  width: 18px;
  height: 18px;
  fill: #a4d007;
}

.luatools-badge {
  font-size: 11px;
  padding: 2px 8px;
  border-radius: 3px;
  text-transform: uppercase;
  font-weight: 600;
  letter-spacing: 0.5px;
}

.luatools-badge-installed {
  background: rgba(92, 126, 16, 0.3);
  color: #a4d007;
  border: 1px solid #5c7e10;
}

.luatools-badge-disabled {
  background: rgba(184, 134, 11, 0.25);
  color: #f0c05a;
  border: 1px solid #9e7319;
}

.luatools-badge-available {
  background: rgba(42, 71, 94, 0.4);
  color: #66c0f4;
  border: 1px solid #2a475e;
}

.luatools-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
  align-items: center;
}

.luatools-btn-group {
  display: inline-flex;
  position: relative;
  overflow: visible;
  box-shadow: 0 2px 6px rgba(0,0,0,0.3);
}

.luatools-btn-group .luatools-btn-primary {
  border-radius: 3px 0 0 3px;
}

.luatools-btn-group .luatools-dropdown-btn {
  border-radius: 0 3px 3px 0;
}

.luatools-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 8px 16px;
  font-size: 13px;
  font-weight: 600;
  border: none;
  border-radius: 3px;
  cursor: pointer;
  transition: all 0.2s ease;
  user-select: none;
  text-decoration: none;
}

.luatools-btn:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

.luatools-btn-primary {
  background: linear-gradient(90deg, #799905 0%, #536f06 100%);
  color: #ffffff;
  text-shadow: 0 1px 2px rgba(0,0,0,0.5);
}

.luatools-btn-primary:hover:not(:disabled) {
  background: linear-gradient(90deg, #8cb006 0%, #5e7f07 100%);
  box-shadow: 0 0 10px rgba(164, 208, 7, 0.4);
}

.luatools-btn-secondary {
  background: #2a475e;
  color: #c6d4df;
}

.luatools-btn-secondary:hover:not(:disabled) {
  background: #3a6282;
  color: #ffffff;
}

.luatools-btn-danger {
  background: #80222b;
  color: #ffccd0;
}

.luatools-btn-danger:hover:not(:disabled) {
  background: #a32b37;
  color: #ffffff;
}

.luatools-btn-restart {
  background: linear-gradient(90deg, #2d73b0 0%, #1a4d7c 100%);
  color: #ffffff;
  animation: luatools-pulse 2s infinite ease-in-out;
}

@keyframes luatools-pulse {
  0% { box-shadow: 0 0 0 0 rgba(102, 192, 244, 0.5); }
  70% { box-shadow: 0 0 0 8px rgba(102, 192, 244, 0); }
  100% { box-shadow: 0 0 0 0 rgba(102, 192, 244, 0); }
}

.luatools-dropdown-btn {
  padding: 8px 10px;
  background: #476005;
  color: #ffffff;
  border: none;
  border-left: 1px solid rgba(0,0,0,0.2);
  cursor: pointer;
}

.luatools-dropdown-btn:hover {
  background: #536f06;
}

.luatools-dropdown-menu {
  position: absolute;
  top: 100%;
  left: 0;
  background: #1b2838;
  border: 1px solid #2a475e;
  border-radius: 4px;
  box-shadow: 0 6px 18px rgba(0,0,0,0.7);
  min-width: 180px;
  display: none;
  flex-direction: column;
  z-index: 1000;
}

.luatools-dropdown-menu.active {
  display: flex;
}

.luatools-dropdown-item {
  padding: 8px 14px;
  font-size: 12px;
  color: #c6d4df;
  cursor: pointer;
  transition: background 0.15s ease;
}

.luatools-dropdown-item:hover:not(.disabled) {
  background: #2a475e;
  color: #ffffff;
}

.luatools-dropdown-item.disabled {
  opacity: 0.45;
  color: #7c8b99;
  cursor: not-allowed;
}

/* Progress bar */
.luatools-progress-container {
  margin-top: 12px;
  background: #121a24;
  border-radius: 4px;
  padding: 10px 14px;
  border: 1px solid #233547;
}

.luatools-progress-meta {
  display: flex;
  justify-content: space-between;
  font-size: 12px;
  margin-bottom: 6px;
  color: #8f98a0;
}

.luatools-progress-track {
  height: 8px;
  background: #1e2837;
  border-radius: 4px;
  overflow: hidden;
  position: relative;
}

.luatools-progress-bar {
  height: 100%;
  background: linear-gradient(90deg, #5c7e10 0%, #a4d007 100%);
  width: 0%;
  transition: width 0.3s ease;
  border-radius: 4px;
}

.luatools-detail-text {
  font-size: 12px;
  color: #8f98a0;
  margin-top: 8px;
}
`;

  function injectStoreStyles() {
    if (document.getElementById('luatools-store-css')) return;
    const parent = document.head || document.documentElement;
    if (!parent) return;
    const style = document.createElement('style');
    style.id = 'luatools-store-css';
    style.textContent = LUATOOLS_STORE_CSS;
    parent.appendChild(style);
  }

  const PLUGIN_NAME = 'LuaToolsLinux';
  const DEFAULT_RPC_PORT = 6767;
  let currentAppId = null;
  let statusPollTimer = null;

  function clearStatusPoll() {
    if (statusPollTimer) {
      clearInterval(statusPollTimer);
      statusPollTimer = null;
    }
  }

  function getAppIdFromUrl() {
    const match = window.location.pathname.match(/\/app\/(\d+)/);
    return match ? parseInt(match[1], 10) : null;
  }

  function getAppName() {
    const el = document.querySelector('.apphub_AppName');
    return el ? el.textContent.trim() : 'Game';
  }

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

  async function callHttpFallback(method, args) {
    const rawPort =
      window.__LUATOOLS_RPC_PORT__ !== undefined && window.__LUATOOLS_RPC_PORT__ !== null
        ? window.__LUATOOLS_RPC_PORT__
        : (window.PluginConfig && window.PluginConfig.backendPort);
    const parsedPort = Number(rawPort);
    const port =
      Number.isInteger(parsedPort) && parsedPort > 0 && parsedPort <= 65535
        ? parsedPort
        : DEFAULT_RPC_PORT;

    const headers = {
      'Content-Type': 'application/json',
      'Accept': 'application/json'
    };
    if (typeof window.__LUATOOLS_RPC_TOKEN__ === 'string' && window.__LUATOOLS_RPC_TOKEN__.length > 0) {
      headers['X-LuaTools-Token'] = window.__LUATOOLS_RPC_TOKEN__;
    }

    try {
      const resp = await fetch(`http://127.0.0.1:${port}/rpc`, {
        method: 'POST',
        headers: headers,
        body: JSON.stringify({
          method: method,
          args: args || {},
          params: args || {}
        })
      });
      const text = await resp.text();
      try {
        return parseRpcResponse(text);
      } catch (e) {
        return { success: resp.ok, raw: text };
      }
    } catch (err) {
      console.error('[LuaToolsLinux] Direct HTTP RPC fallback failed:', method, err);
      return null;
    }
  }

  async function callPlugin(method, args) {
    const mill =
      (window.Millennium && typeof window.Millennium.callServerMethod === 'function' && window.Millennium) ||
      (window.MILLENNIUM_API && window.MILLENNIUM_API.Millennium && typeof window.MILLENNIUM_API.Millennium.callServerMethod === 'function' && window.MILLENNIUM_API.Millennium) ||
      null;
    if (!mill) {
      return callHttpFallback(method, args);
    }
    try {
      const res = await mill.callServerMethod(PLUGIN_NAME, method, [args || {}]);
      return parseRpcResponse(res);
    } catch (err) {
      console.error('[LuaToolsLinux] Call failed:', method, err);
      return null;
    }
  }

  let mountObserver = null;
  let mountTimeout = null;

  function clearMountObserver() {
    if (mountObserver) {
      mountObserver.disconnect();
      mountObserver = null;
    }
    if (mountTimeout) {
      clearTimeout(mountTimeout);
      mountTimeout = null;
    }
  }

  function findPurchaseTarget() {
    return (
      document.querySelector('.game_area_purchase_game_wrapper') ||
      document.querySelector('#game_area_purchase') ||
      document.querySelector('.game_area_purchase_game') ||
      document.querySelector('.queue_actions_ctn') ||
      document.querySelector('.game_area_purchase')
    );
  }

  function createWidget(appId, state) {
    injectStoreStyles();

    const existing = document.getElementById('luatools-store-container');
    if (existing) existing.remove();

    const target = findPurchaseTarget();
    if (!target || !target.parentNode) {
      clearMountObserver();
      const observeRoot = document.body || document.documentElement;
      if (observeRoot) {
        mountObserver = new MutationObserver(() => {
          if (currentAppId !== appId) {
            clearMountObserver();
            return;
          }
          const t = findPurchaseTarget();
          if (t && t.parentNode) {
            clearMountObserver();
            createWidget(appId, state);
          }
        });
        mountObserver.observe(observeRoot, { childList: true, subtree: true });
        mountTimeout = setTimeout(clearMountObserver, 15000);
      } else {
        mountTimeout = setTimeout(() => {
          if (currentAppId === appId) createWidget(appId, state);
        }, 250);
      }
      return;
    }

    clearMountObserver();
    console.log('[LuaToolsLinux] Mounted store widget for app', appId, 'installed=', Boolean(state && (state.exists || state.has)), 'ffi=', typeof window.__private_millennium_ffi_do_not_use__);

    const container = document.createElement('div');
    container.id = 'luatools-store-container';

    const isInstalled = Boolean(state && (state.exists || state.has));
    const isDisabled = Boolean(!isInstalled && state && state.disabled);
    const badgeClass = isInstalled
      ? 'luatools-badge-installed'
      : (isDisabled ? 'luatools-badge-disabled' : 'luatools-badge-available');
    const badgeText = isInstalled
      ? '✓ In LuaToolsLinux'
      : (isDisabled ? '⏸ Disabled in LuaToolsLinux' : 'Ready to Unlock');

    container.innerHTML = `
      <div id="luatools-store-header">
        <div class="luatools-title">
          <svg viewBox="0 0 24 24"><path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-1 14.5v-9l6 4.5-6 4.5z"/></svg>
          LuaToolsLinux <span style="font-size: 11px; opacity: 0.65; font-weight: normal; margin-left: 4px;">by subsurfmod</span>
        </div>
        <div class="luatools-badge ${badgeClass}">${badgeText}</div>
      </div>
      <div class="luatools-actions" id="luatools-actions-root"></div>
      <div id="luatools-progress-area" style="display: none;"></div>
      <div id="luatools-detail-area" class="luatools-detail-text"></div>
    `;

    target.parentNode.insertBefore(container, target);
    renderActions(appId, state);
  }

  function renderActions(appId, state) {
    const root = document.getElementById('luatools-actions-root');
    const detail = document.getElementById('luatools-detail-area');
    if (!root) return;

    root.innerHTML = '';
    const isInstalled = Boolean(state && (state.exists || state.has));
    const isDisabled = Boolean(!isInstalled && state && state.disabled);

    if (isInstalled) {
      if (detail) {
        detail.textContent = `Unlocked via LuaToolsLinux (Depots: ${state.depotCount || 0}, DLCs: ${state.dlcCount || 0})`;
      }

      // Restart Steam Button
      const btnRestart = document.createElement('button');
      btnRestart.className = 'luatools-btn luatools-btn-restart';
      btnRestart.innerHTML = '⟳ Restart Steam to Play';
      btnRestart.onclick = async () => {
        btnRestart.disabled = true;
        btnRestart.textContent = 'Restarting Steam...';
        const res = await callPlugin('RestartSteam', {});
        if (!res || res.success === false) {
          btnRestart.textContent = '⚠ Restart Failed';
          setTimeout(() => {
            btnRestart.disabled = false;
            btnRestart.innerHTML = '⟳ Restart Steam to Play';
          }, 2500);
        }
      };
      root.appendChild(btnRestart);

      // Fixes & Online-Fix Button
      const btnFix = document.createElement('button');
      btnFix.className = 'luatools-btn luatools-btn-secondary';
      btnFix.innerHTML = '🛠 Fixes & Online-Fix';
      btnFix.onclick = async () => {
        await callPlugin('OpenFixInDesktopApp', { appid: appId });
      };
      root.appendChild(btnFix);

      // Sync DLC Button
      const btnDlc = document.createElement('button');
      btnDlc.className = 'luatools-btn luatools-btn-secondary';
      btnDlc.innerHTML = '⤓ Sync DLC';
      btnDlc.onclick = async () => {
        btnDlc.disabled = true;
        btnDlc.textContent = 'Syncing...';
        const res = await callPlugin('SyncDlcForApp', { appid: appId });
        if (res && res.success) {
          btnDlc.textContent = '✓ DLC Synced';
        } else {
          btnDlc.textContent = '⚠ Sync Failed';
        }
        setTimeout(() => {
          btnDlc.disabled = false;
          btnDlc.innerHTML = '⤓ Sync DLC';
        }, 2000);
      };
      root.appendChild(btnDlc);

      // Remove Button
      const btnRemove = document.createElement('button');
      btnRemove.className = 'luatools-btn luatools-btn-danger';
      btnRemove.innerHTML = '✕ Remove';
      btnRemove.onclick = async () => {
        if (!confirm('Remove this game from LuaToolsLinux stplug-in and SLSsteam?')) return;
        btnRemove.disabled = true;
        btnRemove.textContent = 'Removing...';
        await callPlugin('DeleteLuaToolsForApp', { appid: appId });
        initAppWidget(appId);
      };
      root.appendChild(btnRemove);

    } else if (isDisabled) {
      if (detail) {
        detail.textContent = `Currently disabled in LuaToolsLinux (Depots: ${state.depotCount || 0}, DLCs: ${state.dlcCount || 0}). Re-enable to activate.`;
      }

      const btnEnable = document.createElement('button');
      btnEnable.className = 'luatools-btn luatools-btn-primary';
      btnEnable.innerHTML = '▶ Enable in LuaToolsLinux';
      btnEnable.onclick = async () => {
        btnEnable.disabled = true;
        btnEnable.textContent = 'Enabling...';
        const res = await callPlugin('ToggleAppEnabled', { appid: appId, enabled: true });
        if (res && res.success) {
          initAppWidget(appId);
        } else {
          btnEnable.textContent = '⚠ Enable Failed';
          setTimeout(() => {
            btnEnable.disabled = false;
            btnEnable.innerHTML = '▶ Enable in LuaToolsLinux';
          }, 2500);
        }
      };
      root.appendChild(btnEnable);

      const btnRemove = document.createElement('button');
      btnRemove.className = 'luatools-btn luatools-btn-danger';
      btnRemove.innerHTML = '✕ Remove';
      btnRemove.onclick = async () => {
        if (!confirm('Remove this disabled game from LuaToolsLinux stplug-in and SLSsteam?')) return;
        btnRemove.disabled = true;
        btnRemove.textContent = 'Removing...';
        await callPlugin('DeleteLuaToolsForApp', { appid: appId });
        initAppWidget(appId);
      };
      root.appendChild(btnRemove);

    } else {
      if (detail) {
        detail.textContent = 'Download manifest, depot decryption keys, and sync SLSsteam with one click.';
      }

      // Main Add Button Group
      const btnGroup = document.createElement('div');
      btnGroup.className = 'luatools-btn-group';

      const btnAdd = document.createElement('button');
      btnAdd.className = 'luatools-btn luatools-btn-primary';
      btnAdd.innerHTML = '+ Add via LuaToolsLinux';
      btnAdd.onclick = () => startAdd(appId, null);
      btnGroup.appendChild(btnAdd);

      // Dropdown Arrow
      const btnDrop = document.createElement('button');
      btnDrop.className = 'luatools-dropdown-btn';
      btnDrop.innerHTML = '▾';
      btnDrop.onclick = (e) => {
        e.stopPropagation();
        toggleMirrorMenu(appId, btnGroup);
      };
      btnGroup.appendChild(btnDrop);
      root.appendChild(btnGroup);
    }
  }

  async function toggleMirrorMenu(appId, parentGroup) {
    let menu = parentGroup.querySelector('.luatools-dropdown-menu');
    if (!menu) {
      menu = document.createElement('div');
      menu.className = 'luatools-dropdown-menu';
      parentGroup.style.position = 'relative';
      parentGroup.appendChild(menu);

      menu.innerHTML = '<div class="luatools-dropdown-item">Querying mirrors...</div>';
      menu.classList.add('active');

      const res = await callPlugin('CheckSources', { appid: appId });
      menu.innerHTML = '';
      if (res && res.sources && res.sources.length > 0) {
        res.sources.forEach((src) => {
          const item = document.createElement('div');
          item.className = 'luatools-dropdown-item';
          const label = src.displayName || src.name || 'Mirror';
          const isAvailable = src.available !== false;
          item.textContent = `${label} (${isAvailable ? 'Available' : 'Unavailable'})`;
          if (!isAvailable) {
            item.classList.add('disabled');
            item.setAttribute('aria-disabled', 'true');
            item.onclick = (e) => {
              e.stopPropagation();
            };
          } else {
            item.onclick = (e) => {
              e.stopPropagation();
              menu.classList.remove('active');
              startAdd(appId, src.name);
            };
          }
          menu.appendChild(item);
        });
      } else {
        menu.innerHTML = '<div class="luatools-dropdown-item">Auto (Default Mirror)</div>';
        menu.firstChild.onclick = () => {
          menu.classList.remove('active');
          startAdd(appId, null);
        };
      }
    } else {
      menu.classList.toggle('active');
    }
  }

  async function startAdd(appId, source) {
    const root = document.getElementById('luatools-actions-root');
    const progArea = document.getElementById('luatools-progress-area');
    if (!root || !progArea) return;

    clearStatusPoll();
    root.style.display = 'none';
    progArea.style.display = 'block';
    progArea.innerHTML = `
      <div class="luatools-progress-container">
        <div class="luatools-progress-meta">
          <span id="luatools-step-desc">Starting download...</span>
          <span id="luatools-step-pct">0%</span>
        </div>
        <div class="luatools-progress-track">
          <div class="luatools-progress-bar" id="luatools-bar"></div>
        </div>
      </div>
    `;

    const appName = getAppName();
    const cleanSource = (source && source !== 'auto' && source !== 'Auto (Default Mirror)') ? source : null;
    const res = await callPlugin('StartAddViaLuaTools', {
      appid: appId,
      name: appName,
      source: cleanSource
    });

    if (!res || !res.success) {
      const desc = document.getElementById('luatools-step-desc');
      const pct = document.getElementById('luatools-step-pct');
      const bar = document.getElementById('luatools-bar');
      const errMsg = (res && res.error) ? res.error : 'Companion daemon unreachable';
      if (desc) desc.textContent = 'Failed: ' + errMsg;
      if (pct) pct.textContent = 'Error';
      if (bar) {
        bar.style.width = '100%';
        bar.style.background = '#80222b';
      }
      setTimeout(() => {
        if (currentAppId === appId) {
          initAppWidget(appId);
        }
      }, 2500);
      return;
    }

    clearStatusPoll();
    statusPollTimer = setInterval(() => pollAddStatus(appId), 600);
  }

  async function pollAddStatus(appId) {
    if (currentAppId !== appId) {
      clearStatusPoll();
      return;
    }
    const res = await callPlugin('GetAddViaLuaToolsStatus', { appid: appId });
    if (!res) return;
    const state = res.state || res.result || res;
    if (!state) return;

    const desc = document.getElementById('luatools-step-desc');
    const pct = document.getElementById('luatools-step-pct');
    const bar = document.getElementById('luatools-bar');

    const rawProg = typeof state.progress === 'number' ? state.progress : 0;
    const pctVal = rawProg <= 1.0 && rawProg > 0 ? Math.round(rawProg * 100) : Math.round(rawProg);
    const clampedPct = Math.min(100, Math.max(0, pctVal));

    const stepText = state.installStatus || state.step || (state.status ? 'Status: ' + state.status : 'Processing...');
    if (desc) desc.textContent = stepText;
    if (pct) pct.textContent = clampedPct + '%';
    if (bar) bar.style.width = clampedPct + '%';

    const statusLower = (state.status || '').toLowerCase();
    const isDone = statusLower === 'done' || statusLower === 'completed' || state.installed === true;
    const isCancelled = statusLower === 'cancelled' || state.cancelled === true;
    const isFailed = statusLower === 'failed' || state.installFailed === true || (state.error && !isDone);

    if (isDone) {
      clearStatusPoll();
      if (desc) desc.textContent = '✓ Unlocked successfully!';
      if (bar) bar.style.width = '100%';
      setTimeout(() => {
        if (currentAppId === appId) initAppWidget(appId);
      }, 800);
    } else if (isCancelled) {
      clearStatusPoll();
      if (desc) desc.textContent = 'Add cancelled';
      setTimeout(() => {
        if (currentAppId === appId) initAppWidget(appId);
      }, 1000);
    } else if (isFailed) {
      clearStatusPoll();
      if (desc) desc.textContent = 'Failed: ' + (state.error || 'Download failed');
      if (bar) bar.style.background = '#80222b';
      setTimeout(() => {
        if (currentAppId === appId) initAppWidget(appId);
      }, 3000);
    }
  }

  async function initAppWidget(appId) {
    if (!appId) return;
    currentAppId = appId;
    injectStoreStyles();
    const state = await callPlugin('HasLuaToolsForApp', { appid: appId });
    if (currentAppId !== appId) return;
    createWidget(appId, state);
  }

  function onRouteChange() {
    const newAppId = getAppIdFromUrl();
    if (newAppId !== currentAppId) {
      clearStatusPoll();
      clearMountObserver();
      currentAppId = newAppId;
      if (newAppId) {
        initAppWidget(newAppId);
      }
    }
  }

  const origPushState = history.pushState;
  history.pushState = function () {
    origPushState.apply(this, arguments);
    setTimeout(onRouteChange, 100);
  };
  const origReplaceState = history.replaceState;
  history.replaceState = function () {
    origReplaceState.apply(this, arguments);
    setTimeout(onRouteChange, 100);
  };
  window.addEventListener('popstate', onRouteChange);
  setInterval(onRouteChange, 1000);

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', () => {
      injectStoreStyles();
      const appId = getAppIdFromUrl();
      if (appId) initAppWidget(appId);
    });
  } else {
    injectStoreStyles();
    const appId = getAppIdFromUrl();
    if (appId) initAppWidget(appId);
  }
})();
