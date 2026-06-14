'use strict';

// Runs as the webview's preload script in the ISOLATED world
// (nodeIntegration=false, contextIsolation=true). It wires up the relay path;
// the actual fetch/XHR patch (webview-injected.js) is injected into the MAIN
// world by the host via webContents.executeJavaScript(), which — unlike a DOM
// <script> tag — bypasses the page's Content Security Policy.
//
//  contextBridge → exposes window.__socialSavedBridge to the MAIN world so the
//                  injected patch can hand captured data back; we forward it to
//                  the host with ipcRenderer.sendToHost. This is the primary
//                  (and, post-hardening, only privileged) capture path.
//  postMessage   → same-origin fallback the injected script may use.

try {
  const { ipcRenderer, contextBridge } = require('electron');

  function relay(items, hasNextPage, platform) {
    ipcRenderer.sendToHost('intercepted', { items, hasNextPage, platform });
  }

  // Selection-overlay relay (webview-select.js → host): selection count changes
  // and "is this post already saved?" queries. Tagged on its own channel so it
  // never mixes with the capture path above. Payloads use { type:'check', keys }
  // (DOM-derived shortcodes/ids) and host→overlay markSaved([{key,id}]) pairs —
  // see the protocol comment in webview-select.js.
  function relaySelect(payload) {
    ipcRenderer.sendToHost('ss-select', payload);
  }

  // Bridge available to the MAIN world (set up synchronously, before page scripts).
  try {
    contextBridge.exposeInMainWorld('__socialSavedBridge', {
      send: relay,
      sendSelect: relaySelect,
    });
  } catch (_) {
    // Should not happen under contextIsolation=true; relay still works via the
    // same-origin postMessage listener below.
  }

  // Same-origin postMessage fallback used by the injected script if the bridge
  // is unavailable. event.source === window enforces same-origin provenance.
  window.addEventListener('message', (event) => {
    if (event.source !== window || !event.data) return;
    if (event.data.type === 'SOCIAL_SAVED_INTERCEPT') {
      window.__lastInterceptAt = Date.now();
      relay(event.data.items, event.data.hasNextPage, event.data.platform);
    } else if (event.data.type === 'SOCIAL_SAVED_SELECT') {
      relaySelect(event.data.payload);
    }
  });
} catch (err) {
  console.error('[SHELFY] preload error:', err.message);
}
