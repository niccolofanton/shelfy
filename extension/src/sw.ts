// Service worker: validates relayed batches, merges them into chrome.storage.local and pushes
// live state to the side panel. It holds no state in memory: MV3 stops idle workers after ~30 s,
// so every message is processed against storage, one at a time (a promise chain), and the next
// message simply wakes the worker again.

import { platformForUrl } from './hosts';
import {
  RUNTIME,
  isRecord,
  parseBatchMessage,
  parseCensusRuntimeMessage,
  parseMarkerMessage,
  type Platform,
} from './protocol';
import {
  LOG_KEY,
  META_KEY,
  appendLog,
  applyBatch,
  applyCensus,
  applyMarker,
  emptyMeta,
  isStoredItem,
  itemStorageKey,
  prepareBatch,
  readLog,
  readMeta,
  recordRefusal,
  type LogEntry,
  type Meta,
  type StoredItem,
} from './store';

async function openPanelOnActionClick(): Promise<void> {
  try {
    await chrome.sidePanel.setPanelBehavior({ openPanelOnActionClick: true });
  } catch (err) {
    console.warn('[shelfy-spike] could not enable the side panel action', err);
  }
}

chrome.runtime.onInstalled.addListener(() => void openPanelOnActionClick());
void openPanelOnActionClick();

let tail: Promise<unknown> = Promise.resolve();
function serial<T>(task: () => Promise<T>): Promise<T> {
  const run = tail.then(task);
  tail = run.catch(() => undefined);
  return run;
}

function broadcast(meta: Meta, entries: LogEntry[], reset = false): void {
  // Rejects when no side panel is open: nothing to update then.
  chrome.runtime.sendMessage({ kind: RUNTIME.state, meta, entries, reset }).catch(() => undefined);
}

type SenderCheck = { ok: true; platform: Platform } | { ok: false; reason: string };

/**
 * A batch must come from the top frame of a tab on a supported host, and the platform the page
 * declared must match both the sender's host and the relayed page URL.
 */
function checkSender(
  sender: chrome.runtime.MessageSender,
  declared: Platform,
  pageUrl: string,
): SenderCheck {
  if (typeof sender.tab?.id !== 'number' || sender.frameId !== 0)
    return { ok: false, reason: 'not_a_top_frame' };
  const senderPlatform = platformForUrl(sender.url ?? sender.tab.url ?? '');
  if (!senderPlatform) return { ok: false, reason: 'unsupported_host' };
  if (senderPlatform !== declared || platformForUrl(pageUrl) !== declared)
    return { ok: false, reason: 'platform_host_mismatch' };
  return { ok: true, platform: declared };
}

async function refuse(reason: string): Promise<{ ok: false; error: string }> {
  const now = Date.now();
  const stored = await chrome.storage.local.get(META_KEY);
  const meta = recordRefusal(readMeta(stored[META_KEY], now), reason, now);
  await chrome.storage.local.set({ [META_KEY]: meta });
  broadcast(meta, []);
  return { ok: false, error: reason };
}

async function handleBatch(message: unknown, sender: chrome.runtime.MessageSender) {
  const batchMessage = parseBatchMessage(message);
  if (!batchMessage) return refuse('malformed_batch');
  const check = checkSender(sender, batchMessage.platform, batchMessage.pageUrl);
  if (!check.ok) return refuse(check.reason);

  const now = Date.now();
  const batch = prepareBatch(batchMessage, now);
  const itemKeys = batch.items.map((item) => itemStorageKey(item.identity.key));
  const stored = await chrome.storage.local.get([META_KEY, LOG_KEY, ...itemKeys]);
  const existing = new Map<string, StoredItem>();
  for (const key of itemKeys) {
    const value = stored[key];
    if (isStoredItem(value)) existing.set(value.key, value);
  }
  const result = applyBatch(readMeta(stored[META_KEY], now), existing, batch, now);
  const writes: Record<string, unknown> = {
    [META_KEY]: result.meta,
    [LOG_KEY]: appendLog(stored[LOG_KEY], [result.entry]),
  };
  for (const item of result.changed) writes[itemStorageKey(item.key)] = item;
  await chrome.storage.local.set(writes);
  broadcast(result.meta, [result.entry]);
  return { ok: true, accepted: batch.items.length };
}

async function handleMarker(message: unknown, sender: chrome.runtime.MessageSender) {
  const marker = parseMarkerMessage(message);
  if (!marker) return refuse('malformed_marker');
  const platform = platformForUrl(sender.url ?? sender.tab?.url ?? '');
  if (!platform || typeof sender.tab?.id !== 'number') return refuse('unsupported_host');
  const now = Date.now();
  const stored = await chrome.storage.local.get([META_KEY, LOG_KEY]);
  const { meta, entry } = applyMarker(readMeta(stored[META_KEY], now), marker, platform, now);
  await chrome.storage.local.set({
    [META_KEY]: meta,
    [LOG_KEY]: appendLog(stored[LOG_KEY], [entry]),
  });
  broadcast(meta, [entry]);
  return { ok: true };
}

async function handleCensus(message: unknown, sender: chrome.runtime.MessageSender) {
  const census = parseCensusRuntimeMessage(message);
  if (!census) return refuse('malformed_census');
  const senderPlatform = platformForUrl(sender.url ?? sender.tab?.url ?? '');
  if (!senderPlatform || platformForUrl(census.pageUrl) !== senderPlatform)
    return refuse('platform_host_mismatch');
  const now = Date.now();
  const stored = await chrome.storage.local.get(META_KEY);
  const meta = applyCensus(readMeta(stored[META_KEY], now), census.counts, census.pageUrl, now);
  await chrome.storage.local.set({ [META_KEY]: meta });
  broadcast(meta, []);
  return { ok: true };
}

async function readState() {
  const now = Date.now();
  const stored = await chrome.storage.local.get([META_KEY, LOG_KEY]);
  return { ok: true, meta: readMeta(stored[META_KEY], now), log: readLog(stored[LOG_KEY]) };
}

async function clearAll() {
  await chrome.storage.local.clear();
  const meta = emptyMeta(Date.now());
  await chrome.storage.local.set({ [META_KEY]: meta, [LOG_KEY]: [] });
  broadcast(meta, [], true);
  return { ok: true };
}

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (sender.id !== chrome.runtime.id || !isRecord(message)) return false;
  let task: (() => Promise<unknown>) | null = null;
  switch (message.kind) {
    case RUNTIME.batch:
      task = () => handleBatch(message, sender);
      break;
    case RUNTIME.marker:
      task = () => handleMarker(message, sender);
      break;
    case RUNTIME.census:
      task = () => handleCensus(message, sender);
      break;
    case RUNTIME.getState:
      task = readState;
      break;
    case RUNTIME.clear:
      // Only extension pages (the side panel) may clear the store, never a content script.
      if (sender.tab) return false;
      task = clearAll;
      break;
    default:
      return false;
  }
  serial(task).then(sendResponse, (err: unknown) =>
    sendResponse({ ok: false, error: err instanceof Error ? err.message : String(err) }),
  );
  return true;
});
