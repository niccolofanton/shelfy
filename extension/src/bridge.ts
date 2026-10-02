// ISOLATED-world content script (manifest content_scripts[1], document_start, top frame).
// Receives the MAIN-world hook's window messages and forwards them to the service worker.

import { RUNTIME, isRecord } from './protocol';
import { createRelay } from './relay';

const relay = createRelay(window, {
  // async: chrome.runtime.sendMessage throws synchronously in an orphaned content script.
  send: async (message) => chrome.runtime.sendMessage(message),
  pageUrl: () => window.location.href,
  now: () => Date.now(),
  warn: (message) => console.warn('[shelfy-spike]', message),
});

window.addEventListener('message', relay);

// The side panel pings the active tab to tell a live bridge from a tab that was loaded before
// the extension was installed or reloaded (content scripts are only injected on page load).
chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (!isRecord(message) || message.kind !== RUNTIME.ping) return false;
  sendResponse({ ok: true });
  return false;
});
