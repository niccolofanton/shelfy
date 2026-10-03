// Service worker entry (manifest background.service_worker, module). It wires the parts and
// registers every listener synchronously at the top level, as MV3 requires. It keeps no state
// that matters in memory: MV3 stops an idle worker after ~30 s, so the queue is in IndexedDB,
// the rest in chrome.storage, and every event simply wakes the worker again.

import { createPlannerBrowser } from './planner/browser';
import { parseSchedule, parseSources } from './planner/model';
import { PlannerService, PLANNER_ALARM } from './planner/service';
import { PlannerScheduler, pollTasks } from './planner/scheduler';
import { SelectionService } from '../content/select/service';
import { BUILD } from '../shared/build-info';
import {
  EXTERNAL,
  MSG,
  parseCaptureMessage,
  isPlatform,
  parseCensusRuntimeMessage,
  parseSettingsPatch,
  parseSyncEndMessage,
  parseSyncKnownRequest,
  parseSyncMainRequest,
  parseSyncProgressMessage,
  parseSyncStartRequest,
  parseSyncStopRequest,
  type CensusCounts,
  type PingAnswer,
} from '../shared/protocol';
import { EXTENSION_VERSION } from '../shared/version';
import { ApiClient } from './api';
import { handleCapture } from './capture';
import { ConfigService } from './config';
import { checkConnection } from './connection';
import { classifyFailure } from './errors';
import { forgetPairing, installLabel, pair } from './pairing';
import { IdbQueueStore } from './queue/idb';
import { Queue } from './queue/queue';
import { Router } from './router';
import { SettingsStore } from './settings';
import { panelState } from './state';
import { SyncService } from './sync/service';
import { Uploader } from './uploader';

const ALARM = {
  /** Every 5 minutes while the browser runs: config refresh, stale runs, queue flush. */
  maintenance: 'shelfy.maintenance',
  /** One-shot wake-up for a retry or a seal the worker may not live to see. */
  flush: 'shelfy.flush',
} as const;
const MAINTENANCE_MINUTES = 5;
/** Chrome fires alarms no sooner than 30 s from now. */
const MIN_ALARM_DELAY_MS = 30_000;
/** A passive run with no capture for this long has ended (the user moved on). */
const PASSIVE_IDLE_MS = 30 * 60_000;
const STATE_THROTTLE_MS = 250;
const CENSUS_KEY = 'shelfy.census';

const origin = BUILD.origin;
const now = (): number => Date.now();
const log = (where: string, err: unknown): void =>
  console.warn(`[shelfy] ${where}:`, err instanceof Error ? err.message : err);

const store = new SettingsStore(chrome.storage.local);
// The extension token lives in storage.local. Content scripts never need it, so they get no
// access to the area at all (by default storage.local is exposed to them).
chrome.storage.local
  .setAccessLevel({ accessLevel: 'TRUSTED_CONTEXTS' })
  .catch((err: unknown) => log('storage access level', err));

const api = new ApiClient({
  origin,
  version: EXTENSION_VERSION,
  fetch: (input, init) => fetch(input, init),
  credentials: async () => {
    const [pairing, settings] = await Promise.all([store.pairing(), store.settings()]);
    return { token: pairing?.token ?? null, access: settings.access };
  },
  now,
});
const queue = new Queue(new IdbQueueStore(), {
  client: { ext: EXTENSION_VERSION, parser: BUILD.parser },
});
const config = new ConfigService({ api, store, version: EXTENSION_VERSION, now });

// ── State broadcast and wake-ups ─────────────────────────────────────────────

let stateTimer: ReturnType<typeof setTimeout> | null = null;
/** Tells open extension pages to pull the state again (throttled, trailing). */
function changed(): void {
  if (stateTimer) return;
  stateTimer = setTimeout(() => {
    stateTimer = null;
    // Rejects when no extension page is open: nothing to update then.
    chrome.runtime.sendMessage({ kind: MSG.stateChanged }).catch(() => undefined);
  }, STATE_THROTTLE_MS);
}

let flushTimer: ReturnType<typeof setTimeout> | null = null;
let flushTimerAt = Infinity;

async function ensureFlushAlarm(when: number): Promise<void> {
  const existing = await chrome.alarms.get(ALARM.flush);
  if (existing && existing.scheduledTime <= when) return;
  await chrome.alarms.create(ALARM.flush, { when });
}

/** Flushes again at `at`: a timer while the worker lives, and an alarm in case it does not. */
function wakeAt(at: number): void {
  if (at < flushTimerAt) {
    if (flushTimer) clearTimeout(flushTimer);
    flushTimerAt = at;
    flushTimer = setTimeout(
      () => {
        flushTimer = null;
        flushTimerAt = Infinity;
        void uploader.flush().catch((err: unknown) => log('flush', err));
      },
      Math.max(0, at - now()),
    );
  }
  ensureFlushAlarm(Math.max(at, now() + MIN_ALARM_DELAY_MS)).catch((err: unknown) =>
    log('alarm', err),
  );
}

const uploader = new Uploader({
  queue,
  api,
  store,
  config,
  now,
  random: Math.random,
  wakeAt,
  changed,
  closed: async (run): Promise<void> => {
    await sync.closed(run);
    if (run.trigger !== 'passive') await pollTasks();
  },
});

function flush(force = false): void {
  uploader.flush({ force }).catch((err: unknown) => log('flush', err));
}

// The worker's side of explicit syncs (P2-13): the walk itself runs in the tab.
const sync = new SyncService({
  queue,
  store,
  config,
  api,
  tabs: {
    get: (tabId) => chrome.tabs.get(tabId),
    sendMessage: (tabId, message, options) => chrome.tabs.sendMessage(tabId, message, options),
  },
  executeScript: (injection) => chrome.scripting.executeScript(injection),
  storage: chrome.storage.local,
  now,
  sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
  flush,
  block: (failure, action): Promise<void> => uploader.block(failure, action),
  changed,
  log,
});

// P2-15 source planner: a window per platform, the existing P2-13 controller.
const planner = new PlannerService({
  storage: chrome.storage.local,
  browser: createPlannerBrowser(),
  sync,
  now,
  sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
  changed,
  log,
  async eligibility(platform) {
    const [pairing, status, current] = await Promise.all([
      store.pairing(),
      store.status(),
      config.current(),
    ]);
    if (!pairing) return 'not_paired';
    if (status.outdated) return 'outdated';
    const modes = current.platforms[platform];
    return !modes.scroll && !(platform === 'instagram' && modes.replay) ? 'disabled' : null;
  },
  async sources() {
    const response = await api.get('/api/v1/extension/sources', { auth: 'token' });
    if (!response.ok) throw new Error('sources_unavailable');
    return parseSources(response.data);
  },
});
const plannerScheduler = new PlannerScheduler({
  planner,
  now,
  languages: navigator.languages,
  alarms: chrome.alarms,
  async notify(id, options) {
    await chrome.notifications.create(id, {
      type: 'basic',
      iconUrl: 'icon.png',
      title: options.title,
      message: options.message,
      buttons: [{ title: options.button }],
    });
  },
  async clearNotification(id) {
    await chrome.notifications.clear(id);
  },
});

const selection = new SelectionService({
  api,
  store,
  storage: chrome.storage.local,
  tabs: chrome.tabs,
  execute: (injection) => chrome.scripting.executeScript(injection),
  client: { ext: EXTENSION_VERSION, parser: BUILD.parser },
  changed,
});

// ── Runs, config and start-up ───────────────────────────────────────────────

async function tabExists(tabId: number): Promise<boolean> {
  return chrome.tabs.get(tabId).then(
    () => true,
    () => false,
  );
}

/** Ends passive runs whose tab is gone (closed, or a browser restart) or that sat idle. */
async function endStaleRuns(): Promise<void> {
  const at = now();
  const gone = new Set<number>();
  for (const run of await queue.runs())
    if (run.state === 'open' && run.tabId !== null && !(await tabExists(run.tabId)))
      gone.add(run.tabId);
  const ended = await queue.endRuns(
    (run) =>
      run.trigger === 'passive' &&
      ((run.tabId !== null && gone.has(run.tabId)) || at - run.lastAt > PASSIVE_IDLE_MS),
    'user',
    at,
  );
  if (ended) changed();
}

async function refreshConfig(force = false): Promise<void> {
  const result = await config.refresh(force);
  if (result.outcome === 'failed') {
    const action = classifyFailure(result.failure);
    // A revoked token or a too-old build shows on the config too; anything else waits for the
    // next refresh with the cached config.
    if (action.action === 'unpair' || action.action === 'outdated')
      await uploader.block(result.failure, action);
  } else if (result.outcome === 'updated' || result.outcome === 'unchanged') changed();
}

async function maintenance(): Promise<void> {
  await refreshConfig().catch((err: unknown) => log('config', err));
  await endStaleRuns().catch((err: unknown) => log('runs', err));
  await sync.endStale(tabExists).catch((err: unknown) => log('syncs', err));
  await uploader.flush();
}

async function ensureMaintenanceAlarm(): Promise<void> {
  if (!(await chrome.alarms.get(ALARM.maintenance)))
    await chrome.alarms.create(ALARM.maintenance, { periodInMinutes: MAINTENANCE_MINUTES });
}

async function enablePanelOnActionClick(): Promise<void> {
  await chrome.sidePanel.setPanelBehavior({ openPanelOnActionClick: true });
}

// ── Census (debug builds) ───────────────────────────────────────────────────

let census: CensusCounts = {};
let censusLoaded: Promise<void> | null = null;

function loadCensus(): Promise<void> {
  censusLoaded ??= chrome.storage.session.get(CENSUS_KEY).then((stored) => {
    const value = stored[CENSUS_KEY];
    if (value && typeof value === 'object') census = { ...(value as CensusCounts), ...census };
  });
  return censusLoaded;
}

async function addCensus(counts: CensusCounts): Promise<void> {
  await loadCensus();
  for (const [key, count] of Object.entries(counts)) census[key] = (census[key] ?? 0) + count;
  await chrome.storage.session.set({ [CENSUS_KEY]: census });
  changed();
}

// ── Routes ──────────────────────────────────────────────────────────────────

const router = new Router(chrome.runtime.id, origin, log)
  .internal(MSG.selectCommand, 'page', async (message) => {
    if (
      !Number.isSafeInteger(message.tabId) ||
      typeof message.action !== 'string' ||
      !['status', 'enable', 'disable', 'import'].includes(message.action) ||
      !['auto', 'none'].includes(String(message.collection)) ||
      !['en', 'it'].includes(String(message.lang))
    )
      return { ok: false, code: 'bad_request' };
    return selection.command(
      message.tabId as number,
      message.action,
      message.lang as 'en' | 'it',
      message.collection as 'auto' | 'none',
      typeof message.name === 'string' ? message.name.trim().slice(0, 120) : null,
    );
  })
  .internal(MSG.selectLookup, 'content', async (message, sender) => {
    if (!Array.isArray(message.keys) || message.keys.some((key) => typeof key !== 'string'))
      return { ok: false };
    return selection.lookup(message.keys as string[], sender);
  })
  .internal(MSG.selectOpen, 'content', async (message, sender) =>
    typeof message.key === 'string' ? selection.open(message.key, sender) : { ok: false },
  )
  .internal(MSG.capture, 'content', async (message, sender) => {
    const capture = parseCaptureMessage(message);
    if (!capture) return { ok: false, code: 'bad_request' };
    const outcome = await handleCapture(
      capture,
      { tabId: sender.tab?.id, frameId: sender.frameId, url: sender.url, tabUrl: sender.tab?.url },
      { queue, store, config, now },
    );
    if (outcome.queued) {
      if ('full' in outcome && outcome.full) flush();
      else wakeAt(now() + queue.options.windowMs);
    }
    changed();
    return outcome;
  })
  .internal(MSG.census, 'content', async (message) => {
    const parsed = parseCensusRuntimeMessage(message);
    if (!BUILD.debug || !parsed) return { ok: false };
    await addCensus(parsed.counts);
    return { ok: true };
  })
  .internal(MSG.stateGet, 'page', async () => {
    if (BUILD.debug) await loadCensus();
    return panelState({
      version: EXTENSION_VERSION,
      origin,
      debug: BUILD.debug,
      store,
      queue,
      config,
      census: () => (Object.keys(census).length ? census : null),
      syncs: () => sync.views(),
    });
  })
  .internal(MSG.settingsSet, 'page', async (message) => {
    const patch = parseSettingsPatch(message.patch);
    if (!patch) return { ok: false, code: 'bad_request' };
    await store.patchSettings(patch);
    changed();
    if (patch.access !== undefined) flush(true);
    return { ok: true };
  })
  .internal(MSG.connectionCheck, 'page', async () => {
    const check = await checkConnection({
      origin,
      version: EXTENSION_VERSION,
      api,
      store,
      fetch: (input, init) => fetch(input, init),
      now,
    });
    changed();
    flush();
    return { ok: true, check };
  })
  .internal(MSG.queueFlush, 'page', async () => {
    await uploader.flush({ force: true });
    return { ok: true };
  })
  .internal(MSG.pairingForget, 'page', async () => {
    await forgetPairing(store);
    changed();
    return { ok: true };
  })
  // P2-13: explicit syncs. The panel starts and stops them; the tab's controller reports.
  .internal(MSG.syncStart, 'page', async (message) => {
    const request = parseSyncStartRequest(message);
    if (!request) return { ok: false, code: 'bad_request' };
    return sync.start({
      tabId: request.tabId,
      trigger: 'manual',
      collection: request.collection,
      name: request.name,
    });
  })
  .internal(MSG.syncStop, 'page', async (message) => {
    const request = parseSyncStopRequest(message);
    return request ? sync.stop(request.tabId) : { ok: false, code: 'bad_request' };
  })
  .internal(MSG.syncMain, 'content', async (message, sender) => {
    const request = parseSyncMainRequest(message);
    return request ? sync.main(request, sender) : { ok: false };
  })
  .internal(MSG.syncKnown, 'content', async (message, sender) => {
    const request = parseSyncKnownRequest(message);
    return request ? sync.known(request, sender) : { ok: false };
  })
  .internal(MSG.syncProgress, 'content', async (message, sender) => {
    const progress = parseSyncProgressMessage(message);
    return progress ? sync.progress(progress, sender) : { ok: false };
  })
  .internal(MSG.syncEnd, 'content', async (message, sender) => {
    const end = parseSyncEndMessage(message);
    return end ? sync.end(end, sender) : { ok: false };
  })
  .internal(MSG.plannerGet, 'page', async () => planner.snapshot())
  .internal(MSG.plannerStartAll, 'page', async () => ({
    ok: true,
    results: await planner.startAll(),
  }))
  .internal(MSG.plannerStop, 'page', async (message) =>
    isPlatform(message.platform)
      ? planner.stop(message.platform)
      : { ok: false, code: 'bad_request' },
  )
  .internal(MSG.plannerSchedule, 'page', async (message) => {
    const schedule = parseSchedule(message.schedule);
    if (!schedule) return { ok: false, code: 'bad_request' };
    await plannerScheduler.setSchedule(schedule);
    return { ok: true };
  })
  .external(EXTERNAL.syncStart, async (request) =>
    request.type === EXTERNAL.syncStart
      ? planner.start(request.target)
      : { ok: false, code: 'bad_request' },
  )
  .external(EXTERNAL.syncStop, async (request) =>
    request.type === EXTERNAL.syncStop
      ? planner.stop(request.platform)
      : { ok: false, code: 'bad_request' },
  )
  .external(EXTERNAL.ping, async () => {
    const [pairing, status] = await Promise.all([store.pairing(), store.status()]);
    const answer: PingAnswer = {
      ok: true,
      version: EXTENSION_VERSION,
      paired: pairing !== null,
      outdated: status.outdated,
      syncing: await planner.syncing(),
    };
    return answer;
  })
  .external(EXTERNAL.pair, async (request) => {
    if (request.type !== EXTERNAL.pair) return { ok: false, code: 'bad_request' };
    const answer = await pair(request.code, {
      api,
      store,
      version: EXTENSION_VERSION,
      label: installLabel(navigator.userAgent, navigator.platform),
      now,
      paired: () => {
        changed();
        refreshConfig(true)
          .catch((err: unknown) => log('config', err))
          .finally(() => flush(true));
      },
    });
    return answer;
  });

chrome.runtime.onMessage.addListener(router.onMessage);
chrome.runtime.onMessageExternal.addListener(router.onMessageExternal);

chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === ALARM.maintenance) void maintenance().catch((err) => log('maintenance', err));
  else if (alarm.name === ALARM.flush) flush();
  else if (alarm.name === PLANNER_ALARM.reminder || alarm.name === PLANNER_ALARM.taskPoll)
    void plannerScheduler.alarm(alarm.name).catch((error: unknown) => log('planner alarm', error));
});

chrome.notifications.onClicked.addListener((id) => {
  void plannerScheduler.clicked(id).catch((error: unknown) => log('reminder click', error));
});
chrome.notifications.onButtonClicked.addListener((id, index) => {
  if (index === 0)
    void plannerScheduler.clicked(id).catch((error: unknown) => log('reminder click', error));
});

chrome.tabs.onRemoved.addListener((tabId) => {
  queue
    .endRuns((run) => run.tabId === tabId, 'user', now())
    .then((ended) => {
      if (ended) flush();
    })
    .catch((err: unknown) => log('tab closed', err));
});

// A syncing tab navigated: a login wall, a reload or a new page ends its run (P2-13).
chrome.tabs.onUpdated.addListener((tabId, change) => {
  if (change.url || change.status === 'loading')
    sync.tabUpdated(tabId, change).catch((err: unknown) => log('sync tab', err));
});

chrome.runtime.onInstalled.addListener(() => {
  enablePanelOnActionClick().catch((err: unknown) => log('side panel', err));
});

// Registered so that the browser starts the worker with the profile: what the queue held when
// the browser closed goes out without waiting for the next alarm or capture.
chrome.runtime.onStartup.addListener(() => undefined);

// Back online after a network failure: try now instead of waiting out the backoff.
self.addEventListener('online', () => {
  store
    .patchStatus((status) => (status.lastError?.code === 'network' ? { blockedUntil: 0 } : {}))
    .then(() => flush())
    .catch((err: unknown) => log('online', err));
});

// Every start of the worker (install, browser start, any wake-up after suspension).
enablePanelOnActionClick().catch((err: unknown) => log('side panel', err));
void (async () => {
  await planner.recover().catch((error: unknown) => log('planner recovery', error));
  await plannerScheduler.ensure().catch((error: unknown) => log('planner schedule', error));
  await ensureMaintenanceAlarm().catch((err: unknown) => log('alarm', err));
  await endStaleRuns().catch((err: unknown) => log('runs', err));
  await sync.endStale(tabExists).catch((err: unknown) => log('syncs', err));
  await refreshConfig().catch((err: unknown) => log('config', err));
  await uploader.flush();
})().catch((err: unknown) => log('start-up', err));
