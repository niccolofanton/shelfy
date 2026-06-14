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

// The injected patch lives in the page's MAIN world and is untyped JS; the data
// it hands back (captured items, the select-overlay payload) is dynamic, so it
// crosses this boundary as `unknown` — forwarded verbatim, never inspected here.
interface SocialSavedMessage {
  type?: unknown;
  items?: unknown;
  hasNextPage?: unknown;
  platform?: unknown;
  payload?: unknown;
}

try {
  // Kept as require() (not top-level import) so a failed load lands in the catch
  // below; typeof import(...) gives the real Electron types without changing the
  // runtime load path.
  const { ipcRenderer, contextBridge } = require('electron') as typeof import('electron');

  function relay(items: unknown, hasNextPage: unknown, platform: unknown): void {
    ipcRenderer.sendToHost('intercepted', { items, hasNextPage, platform });
  }

  // Selection-overlay relay (webview-select.js → host): selection count changes
  // and "is this post already saved?" queries. Tagged on its own channel so it
  // never mixes with the capture path above. Payloads use { type:'check', keys }
  // (DOM-derived shortcodes/ids) and host→overlay markSaved([{key,id}]) pairs —
  // see the protocol comment in webview-select.js.
  function relaySelect(payload: unknown): void {
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
  window.addEventListener('message', (event: MessageEvent) => {
    if (event.source !== window || !event.data) return;
    const data = event.data as SocialSavedMessage;
    if (data.type === 'SOCIAL_SAVED_INTERCEPT') {
      (window as Window & { __lastInterceptAt?: number }).__lastInterceptAt = Date.now();
      relay(data.items, data.hasNextPage, data.platform);
    } else if (data.type === 'SOCIAL_SAVED_SELECT') {
      relaySelect(data.payload);
    }
  });
} catch (err) {
  console.error('[SHELFY] preload error:', (err as Error).message);
}
