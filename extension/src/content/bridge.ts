// ISOLATED-world content script (manifest content_scripts[1], document_start, top frame).
// Receives the MAIN-world hook's window messages and forwards them to the service worker. It
// holds no secret and reads no extension storage: the pairing token stays in the worker.
// It also hosts the sync controller (content/sync/controller.ts, P2-13), which the worker starts
// with MSG.syncRun: the walk lives in the tab, where MV3 does not stop it.

import { platformForUrl } from '../shared/hosts';
import { MSG, isRecord, parseSyncRunMessage, type BridgePong } from '../shared/protocol';
import { createRelay } from './relay';
import { createViewerReader } from './scoping';
import { SyncController } from './sync/controller';

function randomHex(bytes: number): string {
  return Array.from(crypto.getRandomValues(new Uint8Array(bytes)), (byte) =>
    byte.toString(16).padStart(2, '0'),
  ).join('');
}

const docId = randomHex(12);
const initialHref = window.location.href;
const readViewer = createViewerReader(document);
const warn = (message: string): void => console.warn('[shelfy]', message);

/** The page's heading, which names a new folder or board collection (IMP-10). */
function readHeading(): string | null {
  const heading = document.querySelector('main h1') ?? document.querySelector('h1');
  const text = heading?.textContent?.replace(/\s+/g, ' ').trim() ?? '';
  return text ? text.slice(0, 120) : null;
}

const controller = new SyncController({
  send: async (message) => chrome.runtime.sendMessage(message),
  postToPage: (message) => window.postMessage(message, window.location.origin),
  href: () => window.location.href,
  initialHref,
  relayIdle: () => relay.idle(),
  scroll: {
    scrollY: () => window.scrollY,
    innerHeight: () => window.innerHeight,
    scrollBy: (dy) => window.scrollBy(0, dy),
    scrollTo: (y) => window.scrollTo(0, y),
    revealLast: (selector) => {
      const tiles = document.querySelectorAll(selector);
      tiles[tiles.length - 1]?.scrollIntoView({ behavior: 'instant', block: 'end' });
    },
  },
  sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
  now: () => Date.now(),
  randomId: () => randomHex(8),
  warn,
});

const relay = createRelay(window, {
  // async: chrome.runtime.sendMessage throws synchronously in an orphaned content script.
  send: async (message) => chrome.runtime.sendMessage(message),
  pageUrl: () => window.location.href,
  now: () => Date.now(),
  warn,
  docId,
  viewer: (platform) => (platform === 'pinterest' ? readViewer() : null),
  observer: controller,
});

window.addEventListener('message', relay);

// Messages from the extension itself (the worker, the side panel); never from a page.
chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (sender.id !== chrome.runtime.id || !isRecord(message)) return false;
  switch (message.kind) {
    case MSG.bridgePing: {
      // The side panel pings the active tab to tell a live bridge from a tab that was loaded
      // before the extension was installed or reloaded (content scripts are only injected on
      // page load).
      const viewer = platformForUrl(window.location.href) === 'pinterest' ? readViewer() : null;
      const pong: BridgePong = {
        ok: true,
        docId,
        viewer,
        heading: readHeading(),
        syncing: controller.runId,
      };
      sendResponse(pong);
      return false;
    }
    case MSG.syncRun: {
      const plan = parseSyncRunMessage(message);
      if (!plan) sendResponse({ ok: false, code: 'bad_request' });
      else sendResponse(controller.start(plan) ? { ok: true } : { ok: false, code: 'busy' });
      return false;
    }
    case MSG.syncAbort:
      controller.abort('user');
      sendResponse({ ok: true, running: controller.runId !== null });
      return false;
    default:
      return false;
  }
});
