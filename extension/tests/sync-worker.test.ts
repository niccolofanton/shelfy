// The worker's side of explicit syncs (P2-13) against the fake Shelfy API: explicit runs in the
// queue (appends, eager sealing, the known streak from the ingest results, the accepted keys),
// the closing PATCH, the capture routing of a syncing tab, and the sync service (start
// refusals, the server's answer in the plan, `known`, the end, the tab's navigations).

import { describe, expect, it, vi } from 'vitest';
import { MSG, type BridgePong, type CaptureMessage } from '../src/shared/protocol';
import { handleCapture } from '../src/sw/capture';
import { API } from '../src/sw/contracts';
import { prefilterBatch } from '../src/sw/prefilter';
import type { RunSpec } from '../src/sw/queue/queue';
import { RUN_KEYS_TTL_MS, normalizeRun, type Run } from '../src/sw/queue/types';
import { SyncService, type ExecuteScript, type TabsApi } from '../src/sw/sync/service';
import { closingPatch } from '../src/sw/uploader';
import { T0, harness, igItem, type Harness } from './helpers';

const IG_FOLDER = 'https://www.instagram.com/someone/saved/recipes/17890000000000001/';
const IG_KEY = 'instagram:ig_collection:17890000000000001';
const TAB = 7;

const spec = (extra: Partial<RunSpec> = {}): RunSpec => ({
  platform: 'instagram',
  trigger: 'manual',
  listing: { kind: 'ig_collection', externalId: '17890000000000001', name: 'Recipes' },
  collection: { mode: 'auto' },
  tabId: TAB,
  docId: 'doc-1',
  listingKey: IG_KEY,
  ...extra,
});

const wire = (from: number, count: number) =>
  prefilterBatch(
    Array.from({ length: count }, (_, i) => igItem(from + i)),
    'instagram',
  ).items;

describe('account binding of planner controller and queued captures', () => {
  it('never posts account A collection to B when pairing changes inside the API credential await', async () => {
    const h = harness();
    await h.pairNow();
    const pairingA = (await h.store.pairing())!;
    const s = service(h);
    const actualPost = h.client.post.bind(h.client);
    let release!: () => void;
    vi.spyOn(h.client, 'post').mockImplementation(async (path, body, options) => {
      if (path === API.syncRuns)
        await new Promise<void>((resolve) => {
          release = resolve;
        });
      return actualPost(path, body, options);
    });
    const pending = s.sync.start({
      tabId: TAB,
      trigger: 'web',
      collection: { mode: 'existing', id: 7 },
      expectedPairing: pairingA,
    });
    for (let i = 0; i < 200 && !release; i++) await Promise.resolve();
    const tokenB = h.api.mintToken();
    await h.store.setPairing({ ...pairingA, token: tokenB, tokenId: 'account-B' });
    release();
    expect(await pending).toEqual({ ok: false, code: 'not_paired' });
    expect(h.api.log.filter((request) => request.path === API.syncRuns)).toEqual([]);
    expect(await h.queue.runs()).toEqual([]);
    expect((await h.store.pairing())?.token).toBe(tokenB);
    expect(s.toTab.some((message) => (message as { kind: string }).kind === MSG.syncRun)).toBe(
      false,
    );
  });
  it('drops bound captures from A instead of recreating their run on B', async () => {
    const h = harness();
    await h.pairNow();
    const pairingA = (await h.store.pairing())!;
    const run = await h.queue.openRun(
      spec({ accountTokenId: pairingA.tokenId, collection: { mode: 'existing', id: 7 } }),
      T0,
    );
    await h.queue.captureToRun(run.id, {
      source: 'replay',
      hasNextPage: null,
      items: wire(1, 1),
      at: T0,
      messageId: null,
    });
    await h.store.setPairing({ ...pairingA, token: h.api.mintToken(), tokenId: 'account-B' });
    await h.uploader.flush();
    expect(h.api.log).toEqual([]);
    expect(await h.queue.getRun(run.id)).toBeNull();
    expect(h.api.ingests).toHaveLength(0);
  });
});

describe('explicit runs in the queue', () => {
  it('appends to an open explicit run, seals its group at once, and refuses an ended one', async () => {
    const h = harness();
    const run = await h.queue.openRun(spec(), T0);
    expect(run).toMatchObject({ trigger: 'manual', state: 'open', knownStreak: 0, skipped: [] });
    const input = {
      source: 'replay' as const,
      hasNextPage: true,
      items: wire(1, 3),
      at: T0,
      messageId: 'doc-1:0',
    };
    expect(await h.queue.captureToRun(run.id, input)).toMatchObject({
      accepted: 3,
      duplicate: false,
    });
    expect(await h.queue.captureToRun(run.id, input)).toMatchObject({
      accepted: 0,
      duplicate: true,
    });
    // Eager: due without waiting for the 2 s window.
    expect(await h.queue.sealDue(T0)).toEqual({ sealed: 1, nextDueAt: null });
    await h.queue.endRuns((r) => r.id === run.id, 'user', T0);
    expect(await h.queue.captureToRun(run.id, { ...input, messageId: 'doc-1:1' })).toMatchObject({
      run: null,
      accepted: 0,
      duplicate: false,
    });
  });

  it('follows the trailing run of known items in item order, and keeps the accepted keys', async () => {
    const h = harness();
    await h.pairNow();
    const run = await h.queue.openRun(spec(), T0);
    await h.queue.captureToRun(run.id, {
      source: 'replay',
      hasNextPage: true,
      items: wire(1, 3),
      at: T0,
      messageId: null,
    });
    await h.uploader.flush();
    let stored = await h.queue.getRun(run.id);
    expect(stored).toMatchObject({ inserted: 3, known: 0, knownStreak: 0, queued: 0 });
    // Items 2 and 3 again, then a new one, then item 1: known, known, new, known.
    const mixed = [...wire(2, 2), ...wire(10, 1), ...wire(1, 1)];
    await h.queue.captureToRun(run.id, {
      source: 'replay',
      hasNextPage: true,
      items: mixed,
      at: T0,
      messageId: null,
    });
    await h.uploader.flush();
    stored = await h.queue.getRun(run.id);
    expect(stored).toMatchObject({ inserted: 4, known: 3, knownStreak: 1 });
    await h.queue.captureToRun(run.id, {
      source: 'replay',
      hasNextPage: true,
      items: wire(2, 2),
      at: T0,
      messageId: null,
    });
    await h.uploader.flush();
    expect((await h.queue.getRun(run.id))?.knownStreak).toBe(3);

    const keys = await h.queue.runKeys(run.id);
    expect(keys).toHaveLength(3);
    expect(keys[0]).toMatchObject({ runId: run.id, serverRunId: 'run-1' });
    expect(keys.flatMap((k) => k.keys)).toContain('ig_3400000000000000010');
    h.clock.now += RUN_KEYS_TTL_MS + 1;
    expect(await h.queue.pruneRunKeys(h.clock.now - RUN_KEYS_TTL_MS)).toBe(3);
  });

  it('completes runs stored by an older build with the new fields', () => {
    const old = { id: 'r', trigger: 'passive', pages: 2 } as unknown as Run;
    expect(normalizeRun(old)).toMatchObject({
      id: 'r',
      pages: 2,
      knownStreak: 0,
      skipped: [],
      errorCode: null,
    });
  });
});

describe('closingPatch', () => {
  const ended = (extra: Partial<Run>): Run =>
    normalizeRun({
      id: 'r',
      serverId: 's',
      ...spec(),
      state: 'ended',
      stopReason: null,
      pages: 3,
      scanned: 30,
      queued: 0,
      createdAt: T0,
      lastAt: T0,
      endedAt: T0,
      ...extra,
    } as Run);

  it('maps every end of an explicit walk', () => {
    expect(closingPatch(ended({ stopReason: 'end_of_feed', resumeCursor: 'ignored' }))).toEqual({
      state: 'done',
      pages: 3,
      scanned: 30,
      stopReason: 'end_of_feed',
      resumeCursor: null,
      errorCode: null,
    });
    expect(closingPatch(ended({ stopReason: 'page_cap', resumeCursor: 'S2' }))).toMatchObject({
      state: 'done',
      stopReason: 'page_cap',
      resumeCursor: 'S2',
    });
    expect(closingPatch(ended({ stopReason: 'known_run' }))).toMatchObject({
      state: 'done',
      stopReason: 'known_run',
    });
    expect(closingPatch(ended({ stopReason: 'stalled' }))).toMatchObject({
      state: 'done',
      stopReason: null,
    });
    expect(closingPatch(ended({ stopReason: 'user' }))).toMatchObject({ state: 'stopped' });
    expect(closingPatch(ended({ stopReason: 'login_required' }))).toMatchObject({
      state: 'failed',
    });
    expect(
      closingPatch(ended({ stopReason: 'error', errorCode: 'replay_http_500' })),
    ).toMatchObject({
      state: 'failed',
      errorCode: 'replay_http_500',
    });
    expect(closingPatch(ended({ stopReason: 'user', trigger: 'passive' }))).toMatchObject({
      state: 'done',
    });
  });
});

describe('captures of a syncing tab', () => {
  const capture = (extra: Partial<CaptureMessage> = {}): CaptureMessage => ({
    kind: MSG.capture,
    platform: 'instagram',
    items: [igItem(1), igItem(2)],
    hasNextPage: true,
    pageUrl: IG_FOLDER,
    docId: 'doc-1',
    seq: 0,
    capture: 'replay',
    viewer: null,
    sentAt: T0,
    ...extra,
  });
  const send = (h: Harness, m: CaptureMessage, tabId = TAB) =>
    handleCapture(
      m,
      { tabId, frameId: 0, url: m.pageUrl, tabUrl: m.pageUrl },
      {
        queue: h.queue,
        store: h.store,
        config: h.config,
        now: () => h.clock.now,
      },
    );

  it('go to the sync run, tagged replay or scroll, whatever the passive toggle', async () => {
    const h = harness();
    await h.pairNow();
    await h.store.patchSettings({ passive: { instagram: false } });
    const run = await h.queue.openRun(spec(), T0);
    expect(await send(h, capture())).toMatchObject({ queued: 2, runId: run.id, full: true });
    expect(await send(h, capture({ seq: 1, capture: 'passive' }))).toMatchObject({
      queued: 2,
      runId: run.id,
    });
    await h.uploader.flush();
    expect(h.api.ingests.map((i) => i.source)).toEqual(['replay', 'scroll']);
    expect((await h.queue.runs()).filter((r) => r.trigger === 'passive')).toEqual([]);
  });

  it('are discarded off the listing or when the mode is killed; another document goes passive', async () => {
    const h = harness();
    await h.pairNow();
    await h.queue.openRun(spec(), T0);
    expect(
      await send(h, capture({ pageUrl: 'https://www.instagram.com/someone/saved/all-posts/' })),
    ).toMatchObject({ queued: 0, discarded: 'out_of_scope' });
    h.api.platformConfig.instagram = { replay: false };
    await h.config.refresh(true);
    expect(await send(h, capture({ seq: 2 }))).toMatchObject({ discarded: 'killed' });
    // A reload (another document) is not the sync's: the passive rules apply.
    const outcome = await send(h, capture({ docId: 'doc-2', capture: 'passive' }));
    expect(outcome).toMatchObject({ queued: 2 });
    expect((await h.queue.runs()).some((r) => r.trigger === 'passive' && r.docId === 'doc-2')).toBe(
      true,
    );
  });
});

// ── The sync service ────────────────────────────────────────────────────────

interface FakeTab {
  url: string;
  pong: BridgePong | null;
  /** What the bridge answers to MSG.syncRun and MSG.syncAbort. */
  run: { ok: boolean; code?: string } | null;
  abort: { ok: boolean; running: boolean } | null;
}

function service(h: Harness, tab: Partial<FakeTab> = {}) {
  const state: FakeTab = {
    url: IG_FOLDER,
    pong: { ok: true, docId: 'doc-1', viewer: null, heading: 'Recipes Folder', syncing: null },
    run: { ok: true },
    abort: { ok: true, running: true },
    ...tab,
  };
  const toTab: unknown[] = [];
  const tabs: TabsApi = {
    get: async (tabId) => {
      if (tabId !== TAB) throw new Error('No tab with id');
      return { id: TAB, url: state.url };
    },
    sendMessage: async <T>(tabId: number, message: unknown): Promise<T> => {
      if (tabId !== TAB) throw new Error('no receiver');
      toTab.push(message);
      const kind = (message as { kind: string }).kind;
      const answer =
        kind === MSG.bridgePing ? state.pong : kind === MSG.syncRun ? state.run : state.abort;
      if (!answer) throw new Error('Could not establish connection. Receiving end does not exist.');
      return answer as T;
    },
  };
  const injected: Parameters<ExecuteScript>[0][] = [];
  const flushes: boolean[] = [];
  const sync = new SyncService({
    queue: h.queue,
    store: h.store,
    config: h.config,
    api: h.client,
    tabs,
    executeScript: async (injection) => void injected.push(injection),
    storage: h.storage,
    now: () => h.clock.now,
    sleep: async (ms) => void (h.clock.now += ms),
    flush: (force) => {
      flushes.push(!!force);
      void h.uploader.flush({ force });
    },
    block: (failure, action) => h.uploader.block(failure, action),
    changed: () => undefined,
    log: () => undefined,
  });
  const sender = { tab: { id: TAB }, frameId: 0, documentId: 'chrome-doc-1' };
  return { sync, state, toTab, injected, flushes, sender };
}

describe('SyncService.start', () => {
  it('forces a full planner walk without an incremental stop or resume cursor', async () => {
    const h = harness();
    await h.pairNow();
    h.api.nextRunAnswer = { incremental: true, stopAfterKnown: 3, resumeCursor: 'old-cursor' };
    const s = service(h);
    expect(
      await s.sync.start({ tabId: TAB, collection: 'auto', trigger: 'scheduled', full: true }),
    ).toMatchObject({ ok: true });
    expect(s.toTab.find((m) => (m as { kind: string }).kind === MSG.syncRun)).toMatchObject({
      incremental: false,
      resumeCursor: null,
    });
  });
  it('refuses what it cannot sync', async () => {
    const h = harness();
    expect(await service(h).sync.start({ tabId: TAB, collection: 'auto' })).toEqual({
      ok: false,
      code: 'not_paired',
    });
    await h.pairNow();
    const cases: Array<[Partial<FakeTab>, string]> = [
      [{ url: 'https://www.instagram.com/explore/' }, 'not_a_listing'],
      [{ url: 'https://www.instagram.com/someone/saved/' }, 'not_a_listing'],
      [{ url: 'https://x.com/home' }, 'not_a_listing'],
      [{ pong: null }, 'reload_tab'],
      [{ pong: { ok: true, docId: 'doc-1', viewer: null, syncing: 'other' } }, 'busy'],
      [
        {
          url: 'https://www.pinterest.com/someone_else/cakes/',
          pong: { ok: true, docId: 'doc-1', viewer: 'someone' },
        },
        'not_own_board',
      ],
    ];
    for (const [tab, code] of cases)
      expect(await service(h, tab).sync.start({ tabId: TAB, collection: 'auto' }), code).toEqual({
        ok: false,
        code,
      });
    expect(await service(h).sync.start({ tabId: 99, collection: 'auto' })).toEqual({
      ok: false,
      code: 'not_a_listing',
    });
    h.api.platformConfig.instagram = { replay: false, scroll: false };
    await h.config.refresh(true);
    expect(await service(h).sync.start({ tabId: TAB, collection: 'auto' })).toEqual({
      ok: false,
      code: 'disabled',
    });
  });

  it("opens the run on the server and hands the tab a plan with the server's answer", async () => {
    const h = harness();
    await h.pairNow();
    // The tab's passive run ends: its captures belong to the sync from now on.
    const passive = await h.queue.capturePassive(
      { ...spec(), trigger: 'passive' },
      { source: 'passive', hasNextPage: true, items: wire(1, 1), at: T0, messageId: null },
    );
    h.api.nextRunAnswer = {
      incremental: true,
      stopAfterKnown: 4,
      resumeCursor: 'S9',
      collectionId: 12,
    };
    const s = service(h);
    const answer = await s.sync.start({ tabId: TAB, collection: 'auto' });
    expect(answer).toMatchObject({ ok: true });
    const created = [...h.api.runs.values()].find((r) => r.trigger === 'manual');
    expect(created).toMatchObject({
      listing: { kind: 'ig_collection', externalId: '17890000000000001', name: 'Recipes Folder' },
      collection: { mode: 'auto' },
    });
    expect(s.toTab.at(-1)).toEqual({
      kind: MSG.syncRun,
      runId: answer.ok ? answer.runId : '',
      platform: 'instagram',
      listingKey: IG_KEY,
      incremental: true,
      stopAfterKnown: 4,
      resumeCursor: 'S9',
      replay: true,
      scroll: true,
      scrollSettleMs: 650,
      maxSteps: 16_000,
      maxRunMs: 1_800_000,
    });
    const runs = await h.queue.runs();
    expect(runs.find((r) => r.id === passive.run?.id)).toMatchObject({
      state: 'ended',
      stopReason: 'user',
    });
    expect(runs.find((r) => r.trigger === 'manual')).toMatchObject({
      serverId: created?.id,
      incremental: true,
      collectionId: 12,
      docId: 'doc-1',
    });
    expect(await s.sync.syncing()).toEqual({ instagram: true, twitter: false, pinterest: false });
    expect(await s.sync.start({ tabId: TAB, collection: 'auto' })).toEqual({
      ok: false,
      code: 'busy',
    });
  });

  it('starts one sync when two starts race', async () => {
    const h = harness();
    await h.pairNow();
    const s = service(h);
    const answers = await Promise.all([
      s.sync.start({ tabId: TAB, collection: 'auto' }),
      s.sync.start({ tabId: TAB, collection: 'auto' }),
    ]);
    expect(answers.map((a) => a.ok)).toEqual([true, false]);
    expect(h.api.runs.size).toBe(1);
  });

  it('files a whole feed or a "none" choice into no collection, and a chosen name', async () => {
    const h = harness();
    await h.pairNow();
    const s = service(h, { url: 'https://www.instagram.com/someone/saved/all-posts/' });
    await s.sync.start({ tabId: TAB, collection: 'auto' });
    const h2 = harness();
    await h2.pairNow();
    await service(h2).sync.start({ tabId: TAB, collection: 'auto', name: 'Dinners' });
    const h3 = harness();
    await h3.pairNow();
    await service(h3).sync.start({ tabId: TAB, collection: 'none' });
    const created = (x: Harness) => [...x.api.runs.values()][0];
    expect(created(h)).toMatchObject({
      listing: { kind: 'ig_saved', name: null },
      collection: { mode: 'none' },
    });
    expect(created(h2)).toMatchObject({
      listing: { name: 'Dinners' },
      collection: { mode: 'auto' },
    });
    expect(created(h3)).toMatchObject({ collection: { mode: 'none' } });
  });

  it('walks offline without the server (not incremental), and stops on a refusal', async () => {
    const h = harness();
    await h.pairNow();
    h.network.down = true;
    const s = service(h);
    expect(await s.sync.start({ tabId: TAB, collection: 'auto' })).toMatchObject({ ok: true });
    expect(s.toTab.at(-1)).toMatchObject({
      incremental: false,
      stopAfterKnown: 10,
      resumeCursor: null,
    });
    expect((await h.queue.runs())[0]).toMatchObject({ serverId: null, state: 'open' });

    const old = harness();
    await old.pairNow();
    old.api.minVersion = '9.0.0';
    expect(await service(old).sync.start({ tabId: TAB, collection: 'auto' })).toEqual({
      ok: false,
      code: 'outdated',
    });
    expect(await old.queue.runs()).toEqual([]);
    expect((await old.store.status()).outdated).toBe(true);
  });

  it('ends the run when the bridge does not take the plan', async () => {
    const h = harness();
    await h.pairNow();
    const s = service(h, { run: null });
    expect(await s.sync.start({ tabId: TAB, collection: 'auto' })).toEqual({
      ok: false,
      code: 'reload_tab',
    });
    await vi.waitFor(async () =>
      expect(h.api.patches.at(-1)?.body).toMatchObject({
        state: 'failed',
        errorCode: 'controller_unreachable',
      }),
    );
  });
});

describe("SyncService and the controller's requests", () => {
  async function started(extra: Partial<FakeTab> = {}) {
    const h = harness();
    await h.pairNow();
    const s = service(h, extra);
    const answer = await s.sync.start({ tabId: TAB, collection: 'auto' });
    if (!answer.ok) throw new Error(answer.code);
    return { h, s, runId: answer.runId };
  }

  it('injects the gated replay into the controller document, with the config pacing', async () => {
    const { h, s, runId } = await started();
    expect(
      await s.sync.main(
        { kind: MSG.syncMain, runId, op: 'replay', replayId: 'replay-1' },
        s.sender,
      ),
    ).toEqual({ ok: true });
    expect(s.injected[0]).toMatchObject({
      target: { tabId: TAB, documentIds: ['chrome-doc-1'] },
      world: 'MAIN',
      args: [{ maxPages: 100, gapMs: 700, runId: 'replay-1', gate: true, gateTimeoutMs: 30_000 }],
    });
    expect(s.injected[0].func.name).toBe('igFeedReplay');
    // From another tab or frame, or for a Pinterest helper on Instagram: refused.
    expect(
      await s.sync.main(
        { kind: MSG.syncMain, runId, op: 'replay', replayId: 'r' },
        { ...s.sender, tab: { id: 8 } },
      ),
    ).toEqual({ ok: false });
    expect(
      await s.sync.main(
        { kind: MSG.syncMain, runId, op: 'replay', replayId: 'r' },
        { ...s.sender, frameId: 3 },
      ),
    ).toEqual({ ok: false });
    expect(
      await s.sync.main(
        { kind: MSG.syncMain, runId, op: 'pinterest_ssr', replayId: null },
        s.sender,
      ),
    ).toEqual({ ok: false });
    expect(h.api.runs.size).toBe(1);
  });

  it('answers the known run once the captures are ingested, or unsettled when blocked', async () => {
    const { h, s, runId } = await started();
    await h.queue.captureToRun(runId, {
      source: 'replay',
      hasNextPage: true,
      items: wire(1, 2),
      at: T0,
      messageId: null,
    });
    // Already ingested once: both are known now.
    h.api.posts.set('ig_3400000000000000001', 1);
    h.api.posts.set('ig_3400000000000000002', 1);
    expect(await s.sync.known({ kind: MSG.syncKnown, runId, reset: false }, s.sender)).toEqual({
      ok: true,
      streak: 2,
      settled: true,
    });
    expect(await s.sync.known({ kind: MSG.syncKnown, runId, reset: true }, s.sender)).toEqual({
      ok: true,
      streak: 0,
      settled: true,
    });
    expect((await h.queue.getRun(runId))?.knownStreak).toBe(0);

    h.network.down = true;
    await h.queue.captureToRun(runId, {
      source: 'replay',
      hasNextPage: true,
      items: wire(3, 1),
      at: T0,
      messageId: null,
    });
    const answer = await s.sync.known({ kind: MSG.syncKnown, runId, reset: false }, s.sender);
    expect(answer).toEqual({ ok: true, streak: 0, settled: false });
  });

  it("records progress, ends with the controller's report, and closes the run on the server", async () => {
    const { h, s, runId } = await started();
    await s.sync.progress(
      { kind: MSG.syncProgress, runId, phase: 'replay', steps: 0, replayPages: 2 },
      s.sender,
    );
    expect((await s.sync.views())[0]).toMatchObject({
      runId,
      state: 'open',
      phase: 'replay',
      replayPages: 2,
    });
    await s.sync.end(
      {
        kind: MSG.syncEnd,
        runId,
        reason: 'page_cap',
        resumeCursor: 'S2',
        errorCode: null,
        skipped: ['scroll'],
        steps: 0,
        replayPages: 2,
      },
      s.sender,
    );
    await vi.waitFor(() =>
      expect(h.api.patches.at(-1)?.body).toMatchObject({
        state: 'done',
        stopReason: 'page_cap',
        resumeCursor: 'S2',
      }),
    );
    await vi.waitFor(async () => expect(await h.queue.runs()).toEqual([]));
  });

  it('closes a run in history once the uploader forgot it', async () => {
    const h = harness();
    await h.pairNow();
    const s = service(h);
    const answer = await s.sync.start({ tabId: TAB, collection: 'auto' });
    if (!answer.ok) throw new Error(answer.code);
    const run = (await h.queue.getRun(answer.runId)) as Run;
    await s.sync.closed({ ...run, state: 'ended', stopReason: 'end_of_feed', endedAt: T0 });
    await s.sync.closed({ ...run, id: 'passive', trigger: 'passive' });
    const history = await s.sync.history.list();
    expect(history).toHaveLength(1);
    expect(history[0]).toMatchObject({
      runId: answer.runId,
      stopReason: 'end_of_feed',
      state: 'ended',
    });
  });

  it('a login URL, a reload, or a closed tab ends the run; an in-page navigation does not', async () => {
    const login = await started();
    await login.s.sync.tabUpdated(TAB, { url: 'https://www.instagram.com/accounts/login/' });
    expect((await login.h.queue.getRun(login.runId))?.stopReason).toBe('login_required');

    const spa = await started();
    spa.s.state.pong = { ok: true, docId: 'doc-1', viewer: null, syncing: spa.runId };
    await spa.s.sync.tabUpdated(TAB, { url: 'https://www.instagram.com/p/ABCdef123/' });
    expect((await spa.h.queue.getRun(spa.runId))?.state).toBe('open');

    const reload = await started();
    reload.s.state.pong = { ok: true, docId: 'doc-2', viewer: null, syncing: null };
    await reload.s.sync.tabUpdated(TAB, { status: 'loading' });
    expect((await reload.h.queue.getRun(reload.runId))?.stopReason).toBe('user');

    const closed = await started();
    await closed.s.sync.endStale(async () => false);
    expect((await closed.h.queue.getRun(closed.runId))?.stopReason).toBe('user');

    const silent = await started();
    silent.h.clock.now += 4 * 60_000;
    await silent.s.sync.endStale(async () => true);
    expect((await silent.h.queue.getRun(silent.runId))?.state).toBe('open');
    silent.h.clock.now += 2 * 60_000;
    await silent.s.sync.endStale(async () => true);
    expect(await silent.h.queue.getRun(silent.runId)).toMatchObject({
      stopReason: 'error',
      errorCode: 'controller_lost',
    });
  });

  it('stops through the controller, or by itself when the controller is gone', async () => {
    const live = await started();
    expect(await live.s.sync.stop(TAB)).toEqual({ ok: true });
    expect(live.s.toTab.at(-1)).toEqual({ kind: MSG.syncAbort });
    expect((await live.h.queue.getRun(live.runId))?.state).toBe('open'); // the controller ends it

    const gone = await started();
    gone.s.state.abort = null;
    await gone.s.sync.stopPlatform('instagram');
    expect((await gone.h.queue.getRun(gone.runId))?.stopReason).toBe('user');
    expect(await gone.s.sync.stop(TAB)).toEqual({ ok: false, code: 'not_syncing' });
  });
});
