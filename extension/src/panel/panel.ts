// Side panel: live counters and log, "Export JSON", "Clear data" and the Instagram replay.
// Live state arrives from the service worker (RUNTIME.state broadcasts); the export reads
// chrome.storage.local directly. All page-derived strings are rendered as text, never as HTML.

import { buildExport, exportFileName } from '../export-format';
import { platformForUrl } from '../hosts';
import { classifyListing, listingLabel, parseListingKey, type Listing } from '../listing';
import { PLATFORMS, RUNTIME, SOURCES, isRecord, type Platform } from '../protocol';
import {
  IG_REPLAY_GAP_MS,
  IG_REPLAY_MAX_PAGES,
  igFeedReplay,
  isCaptureHookInstalled,
  stopIgFeedReplay,
  type ReplayResult,
} from '../replay';
import {
  ITEM_KEY_PREFIX,
  LOG_LIMIT,
  META_KEY,
  isStoredItem,
  readLog,
  readMeta,
  type LogEntry,
  type Meta,
} from '../store';

const PLATFORM_LABEL: Record<Platform, string> = {
  instagram: 'Instagram',
  twitter: 'X',
  pinterest: 'Pinterest',
};
const LOG_SHOWN = 200;
const CLEAR_CONFIRM_MS = 4000;

/** Whether this extension instance captures in the tab (both content scripts are live). */
type CaptureStatus = 'checking' | 'active' | 'reload' | 'unknown';

interface ActiveTab {
  id: number;
  platform: Platform | null;
  listing: Listing | null;
  capture: CaptureStatus;
}

interface ReplayState {
  tabId: number;
  running: boolean;
  result: ReplayResult | null;
  error: string | null;
}

const state: {
  meta: Meta | null;
  log: LogEntry[];
  tab: ActiveTab | null;
  replay: ReplayState | null;
  clearArmedUntil: number;
} = { meta: null, log: [], tab: null, replay: null, clearArmedUntil: 0 };

// ── DOM helpers ─────────────────────────────────────────────────────────────

function byId<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`panel.html is missing #${id}`);
  return node as T;
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  text?: string | number,
  className?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = String(text);
  if (className) node.className = className;
  return node;
}

function table(headers: string[], rows: Array<Array<string | number>>): HTMLElement {
  if (!rows.length) return el('p', 'Nothing captured yet.', 'muted');
  const root = el('table');
  const head = root.createTHead().insertRow();
  for (const header of headers) head.append(el('th', header));
  const body = root.createTBody();
  for (const row of rows) {
    const tr = body.insertRow();
    for (const cell of row) tr.append(el('td', cell));
  }
  return root;
}

function clock(ms: number | null): string {
  return ms ? new Date(ms).toLocaleTimeString() : '–';
}

function setStatus(id: string, text: string, tone: 'ok' | 'warn' | '' = ''): void {
  const node = byId(id);
  node.textContent = text;
  node.className = `status ${tone}`.trim();
}

// ── Rendering ───────────────────────────────────────────────────────────────

function renderPlatforms(meta: Meta): void {
  const rows = PLATFORMS.map((platform) => {
    const s = meta.platforms[platform];
    return [
      PLATFORM_LABEL[platform],
      s.uniqueItems,
      s.batches,
      s.itemsReceived,
      s.discardedItems,
      s.rejectedItems,
      clock(s.lastCaptureAt),
    ];
  }).filter((row) => row.slice(1, 6).some((n) => n !== 0));
  byId('platforms').replaceChildren(
    table(['Platform', 'Items', 'Batches', 'Received', 'Discarded', 'Rejected', 'Last'], rows),
  );
  const refused = Object.entries(meta.diagnostics.refusedBatches)
    .map(([reason, n]) => `${reason} ${n}`)
    .join(', ');
  byId('diagnostics').textContent =
    `IG shortcode/pk mismatches: ${meta.diagnostics.igShortcodeMismatch}` +
    (refused ? ` · refused messages: ${refused}` : '');
}

function renderListings(meta: Meta): void {
  const rows = Object.values(meta.listings)
    .sort((a, b) => a.key.localeCompare(b.key))
    .map((l) => [
      listingLabel(l),
      l.uniqueItems,
      ...SOURCES.map((source) => l.bySource[source]),
      l.batches,
      l.endOfFeedSeen ? 'yes' : 'no',
      clock(l.lastSeenAt),
    ]);
  byId('listings').replaceChildren(
    table(['Listing', 'Items', ...SOURCES, 'Batches', 'End', 'Last'], rows),
  );
}

function renderCensus(meta: Meta): void {
  const rows: Array<Array<string | number>> = [];
  for (const platform of PLATFORMS)
    for (const [label, entry] of Object.entries(meta.platforms[platform].census))
      rows.push([`${PLATFORM_LABEL[platform]} · ${label}`, entry.requests, entry.inScope]);
  rows.sort((a, b) => String(a[0]).localeCompare(String(b[0])));
  byId('census').replaceChildren(table(['Endpoint', 'Requests', 'In scope'], rows));
}

function describeListingKey(key: string): string {
  const known = state.meta?.listings[key];
  if (known) return listingLabel(known);
  const parsed = parseListingKey(key);
  return parsed ? listingLabel({ ...parsed, name: null }) : key;
}

function describeLogEntry(entry: LogEntry): string {
  const head = `${clock(entry.at)} ${entry.platform ?? '?'}`;
  switch (entry.kind) {
    case 'batch':
      return (
        `${head} · ${describeListingKey(entry.listingKey)} · ${entry.source} · ` +
        `+${entry.newInListing} in listing, +${entry.newItems} new ` +
        `(${entry.accepted}/${entry.received} accepted)` +
        (entry.hasNextPage === false ? ' · end of feed' : '')
      );
    case 'discard':
      return `${head} · discarded ${entry.received} items (${entry.reason}) on ${entry.path}`;
    case 'marker': {
      const detail = Object.entries(entry.detail)
        .map(([k, v]) => `${k}=${String(v)}`)
        .join(' ');
      return `${head} · ${entry.source} ${entry.phase} ${detail}`;
    }
  }
}

function renderLog(): void {
  const items = state.log
    .slice(-LOG_SHOWN)
    .reverse()
    .map((entry) => el('li', describeLogEntry(entry)));
  byId('log').replaceChildren(...items);
}

function isReplayableListing(listing: Listing | null): boolean {
  return listing?.kind === 'ig_saved' || listing?.kind === 'ig_collection';
}

function renderActiveTab(): void {
  const card = byId('active-tab');
  const tab = state.tab;
  const lines: HTMLElement[] = [];
  if (!tab || !tab.platform) {
    lines.push(el('p', 'Open Instagram, X or Pinterest in this window.', 'muted'));
  } else {
    lines.push(el('p', PLATFORM_LABEL[tab.platform]));
    lines.push(
      tab.listing
        ? el('p', `Listing: ${listingLabel(tab.listing)}`, 'ok')
        : el('p', 'Not a saved listing: batches captured on this page are discarded.', 'warn'),
    );
    lines.push(
      tab.capture === 'active'
        ? el('p', 'Capture: active (hook and bridge loaded)', 'ok')
        : tab.capture === 'reload'
          ? el('p', 'Capture: not loaded in this tab. Reload the tab.', 'warn')
          : tab.capture === 'unknown'
            ? el(
                'p',
                'Capture: could not check this tab. Reload it if nothing is captured.',
                'warn',
              )
            : el('p', 'Capture: checking…', 'muted'),
    );
  }
  card.replaceChildren(...lines);

  const replayBox = byId('replay');
  replayBox.hidden = !tab || !isReplayableListing(tab.listing);
  const running = !!state.replay?.running;
  byId<HTMLButtonElement>('replay-run').disabled = running || tab?.capture !== 'active';
  byId<HTMLButtonElement>('replay-stop').disabled = !running;
  const replay = state.replay;
  if (!replay) setStatus('replay-status', '');
  else if (replay.running) setStatus('replay-status', 'Replay running… watch the log.');
  else if (replay.error) setStatus('replay-status', `Replay failed: ${replay.error}`, 'warn');
  else if (replay.result) {
    const r = replay.result;
    setStatus(
      'replay-status',
      r.started
        ? `Replay finished: ${r.pages} pages, ${r.reason}` +
            (r.status !== null ? `, last HTTP ${r.status}` : '') +
            `, app id from ${r.appIdSource ?? '?'}.`
        : `Replay not started: ${r.reason}.`,
      r.started && (r.reason === 'end_of_feed' || r.reason === 'page_cap') ? 'ok' : 'warn',
    );
  }
}

let renderQueued = false;
function scheduleRender(): void {
  if (renderQueued) return;
  renderQueued = true;
  requestAnimationFrame(() => {
    renderQueued = false;
    if (state.meta) {
      renderPlatforms(state.meta);
      renderListings(state.meta);
      renderCensus(state.meta);
    }
    renderLog();
    renderActiveTab();
  });
}

// ── Actions ─────────────────────────────────────────────────────────────────

/** Union of two log slices by sequence number (a broadcast may race the initial load). */
function mergeLog(a: readonly LogEntry[], b: readonly LogEntry[]): LogEntry[] {
  const bySeq = new Map<number, LogEntry>();
  for (const entry of [...a, ...b]) bySeq.set(entry.seq, entry);
  return [...bySeq.values()].sort((x, y) => x.seq - y.seq).slice(-LOG_LIMIT);
}

async function loadState(): Promise<void> {
  const response: unknown = await chrome.runtime.sendMessage({ kind: RUNTIME.getState });
  if (isRecord(response) && response.ok === true) {
    state.meta = readMeta(response.meta, Date.now());
    state.log = mergeLog(readLog(response.log), state.log);
  }
  scheduleRender();
}

async function exportJson(): Promise<void> {
  setStatus('action-status', 'Building the export…');
  const all = await chrome.storage.local.get(null);
  const now = Date.now();
  const items = Object.entries(all)
    .filter(([key]) => key.startsWith(ITEM_KEY_PREFIX))
    .map(([, value]) => value)
    .filter(isStoredItem);
  const file = buildExport(readMeta(all[META_KEY], now), items, {
    extensionVersion: chrome.runtime.getManifest().version,
    userAgent: navigator.userAgent,
    now,
  });
  const blob = new Blob([JSON.stringify(file, null, 2)], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = exportFileName(new Date(now));
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
  setStatus(
    'action-status',
    `Exported ${items.length} items and ${file.listings.length} listings as ${link.download}.`,
    'ok',
  );
}

async function clearData(): Promise<void> {
  const now = Date.now();
  const button = byId<HTMLButtonElement>('clear');
  if (now > state.clearArmedUntil) {
    state.clearArmedUntil = now + CLEAR_CONFIRM_MS;
    button.textContent = 'Click again to clear';
    setTimeout(() => {
      if (Date.now() >= state.clearArmedUntil) button.textContent = 'Clear data';
    }, CLEAR_CONFIRM_MS);
    return;
  }
  state.clearArmedUntil = 0;
  button.textContent = 'Clear data';
  const response: unknown = await chrome.runtime.sendMessage({ kind: RUNTIME.clear });
  if (isRecord(response) && response.ok === true) {
    state.log = [];
    await loadState();
    setStatus('action-status', 'All captured data cleared.', 'ok');
  } else setStatus('action-status', 'Could not clear the data.', 'warn');
}

/**
 * A tab captures only when this extension instance's bridge answers a ping (content scripts
 * are injected on page load, so tabs opened before an install or reload have none) and the
 * MAIN-world hook is installed.
 */
async function probeCapture(tabId: number): Promise<CaptureStatus> {
  const bridgeAlive = await chrome.tabs
    .sendMessage(tabId, { kind: RUNTIME.ping })
    .then((response) => isRecord(response) && response.ok === true)
    .catch(() => false);
  if (!bridgeAlive) return 'reload';
  try {
    const [injection] = await chrome.scripting.executeScript({
      target: { tabId },
      world: 'MAIN',
      func: isCaptureHookInstalled,
    });
    return injection?.result === true ? 'active' : 'reload';
  } catch {
    return 'unknown';
  }
}

async function refreshActiveTab(): Promise<void> {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab || typeof tab.id !== 'number') {
    state.tab = null;
    scheduleRender();
    return;
  }
  // tab.url is only visible on hosts the extension has permission for: the social hosts.
  const url = tab.url ?? '';
  const platform = url ? platformForUrl(url) : null;
  const listing = platform ? classifyListing(platform, url) : null;
  state.tab = { id: tab.id, platform, listing, capture: 'checking' };
  scheduleRender();
  if (!platform) return;
  const capture = await probeCapture(tab.id);
  if (state.tab?.id === tab.id) {
    state.tab.capture = capture;
    scheduleRender();
  }
}

function replayPages(): number {
  const value = Number(byId<HTMLInputElement>('replay-pages').value);
  return Number.isInteger(value)
    ? Math.min(Math.max(value, 1), IG_REPLAY_MAX_PAGES)
    : IG_REPLAY_MAX_PAGES;
}

async function runReplay(): Promise<void> {
  const tab = state.tab;
  if (!tab || !isReplayableListing(tab.listing) || state.replay?.running) return;
  if ((await probeCapture(tab.id)) !== 'active') {
    setStatus('replay-status', 'Capture is not loaded in this tab: reload it first.', 'warn');
    return;
  }
  const replay: ReplayState = { tabId: tab.id, running: true, result: null, error: null };
  state.replay = replay;
  scheduleRender();
  try {
    const [injection] = await chrome.scripting.executeScript({
      target: { tabId: tab.id },
      world: 'MAIN',
      func: igFeedReplay,
      args: [
        {
          maxPages: replayPages(),
          gapMs: IG_REPLAY_GAP_MS,
          runId: `replay-${Date.now().toString(36)}`,
        },
      ],
    });
    replay.result = injection?.result ?? null;
    if (!replay.result) replay.error = 'the tab returned no result (navigated away?)';
  } catch (err) {
    replay.error = err instanceof Error ? err.message : String(err);
  } finally {
    replay.running = false;
    scheduleRender();
  }
}

async function stopReplay(): Promise<void> {
  const replay = state.replay;
  if (!replay?.running) return;
  try {
    await chrome.scripting.executeScript({
      target: { tabId: replay.tabId },
      world: 'MAIN',
      func: stopIgFeedReplay,
    });
    setStatus('replay-status', 'Stop requested: the replay ends before its next page.');
  } catch (err) {
    setStatus('replay-status', `Could not stop the replay: ${String(err)}`, 'warn');
  }
}

// ── Wiring ──────────────────────────────────────────────────────────────────

chrome.runtime.onMessage.addListener((message) => {
  if (!isRecord(message) || message.kind !== RUNTIME.state) return false;
  state.meta = readMeta(message.meta, Date.now());
  state.log = message.reset === true ? [] : mergeLog(state.log, readLog(message.entries));
  scheduleRender();
  return false;
});

chrome.tabs.onActivated.addListener(() => void refreshActiveTab());
chrome.tabs.onUpdated.addListener((tabId, changeInfo) => {
  if (tabId === state.tab?.id && (changeInfo.url || changeInfo.status === 'complete'))
    void refreshActiveTab();
});

const manifest = chrome.runtime.getManifest();
byId('version').textContent = `v${manifest.version_name ?? manifest.version}`;
byId('export').addEventListener('click', () => {
  exportJson().catch((err: unknown) =>
    setStatus('action-status', `Export failed: ${String(err)}`, 'warn'),
  );
});
byId('clear').addEventListener('click', () => {
  clearData().catch((err: unknown) =>
    setStatus('action-status', `Clear failed: ${String(err)}`, 'warn'),
  );
});
byId('replay-run').addEventListener('click', () => void runReplay());
byId('replay-stop').addEventListener('click', () => void stopReplay());

void loadState();
void refreshActiveTab();
