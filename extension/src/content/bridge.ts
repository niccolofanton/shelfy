// ISOLATED-world content script (manifest content_scripts[1], document_start, top frame).
// Receives the MAIN-world hook's window messages and forwards them to the service worker. It
// holds no secret and reads no extension storage: the pairing token stays in the worker.

import { platformForUrl } from '../shared/hosts';
import { MSG, isRecord, type BridgePong } from '../shared/protocol';
import { createRelay } from './relay';
import { createViewerReader } from './scoping';

function randomDocId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(12));
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');
}

const docId = randomDocId();
const readViewer = createViewerReader(document);

const relay = createRelay(window, {
  // async: chrome.runtime.sendMessage throws synchronously in an orphaned content script.
  send: async (message) => chrome.runtime.sendMessage(message),
  pageUrl: () => window.location.href,
  now: () => Date.now(),
  warn: (message) => console.warn('[shelfy]', message),
  docId,
  viewer: (platform) => (platform === 'pinterest' ? readViewer() : null),
});

window.addEventListener('message', relay);

// The side panel pings the active tab to tell a live bridge from a tab that was loaded before
// the extension was installed or reloaded (content scripts are only injected on page load).
chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (sender.id !== chrome.runtime.id || !isRecord(message) || message.kind !== MSG.bridgePing)
    return false;
  const viewer = platformForUrl(window.location.href) === 'pinterest' ? readViewer() : null;
  const pong: BridgePong = { ok: true, docId, viewer };
  sendResponse(pong);
  return false;
});
