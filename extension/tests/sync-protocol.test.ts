// @vitest-environment jsdom
//
// The sync controller's messages (P2-13): the gated IG replay (continue, stop, jump, timeout,
// the cursor at the page cap), the parsers of shared/protocol.ts, the relay's observer and
// idle(), and the panel's reading of the active tab and its strings.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createRelay } from '../src/content/relay';
import { igFeedReplay, pinterestSsrRead, type ReplayOptions } from '../src/main/replay';
import { liveText, resultText, syncAvailability } from '../src/panel/sections/sync';
import { createTranslate, hasMessage } from '../src/shared/i18n';
import {
  MSG,
  REPLAY_GATE_MESSAGE,
  REPLAY_PAGE_MESSAGE,
  SCOPE_MESSAGE,
  SYNC_END_REASONS,
  SYNC_MODES,
  SYNC_PHASES,
  parseReplayPageMessage,
  parseSyncEndMessage,
  parseSyncKnownRequest,
  parseSyncMainRequest,
  parseSyncProgressMessage,
  parseSyncRunMessage,
  parseSyncStartRequest,
  parseSyncStopRequest,
  type BridgePong,
} from '../src/shared/protocol';
import type { PanelState } from '../src/sw/state';
import type { SyncView } from '../src/sw/sync/history';

declare const jsdom: { reconfigure(options: { url: string }): void };

// ── The gated replay ────────────────────────────────────────────────────────

type Posted = {
  type?: string;
  id?: string;
  page?: number;
  cursor?: string | null;
  wait?: boolean;
  phase?: string;
};

describe('igFeedReplay with a gate', () => {
  const posted: Posted[] = [];
  let requests: string[] = [];
  let answer: (page: Posted) => Record<string, unknown> | null = () => ({ action: 'continue' });

  beforeEach(() => {
    posted.length = 0;
    requests = [];
    jsdom.reconfigure({ url: 'https://www.instagram.com/someone/saved/all-posts/' });
    // Pages by max_id: '' → A, A → B, B → C, C → end; J (a jump target) → end.
    const feed: Record<string, { next: string | null }> = {
      '': { next: 'A' },
      A: { next: 'B' },
      B: { next: 'C' },
      C: { next: null },
      J: { next: null },
    };
    window.fetch = vi.fn(async (input: RequestInfo | URL) => {
      const maxId =
        new URL(String(input), 'https://www.instagram.com').searchParams.get('max_id') ?? '';
      requests.push(maxId);
      const next = feed[maxId]?.next ?? null;
      return new Response(
        JSON.stringify({
          items: [],
          more_available: next !== null,
          ...(next ? { next_max_id: next } : {}),
        }),
      );
    }) as typeof window.fetch;
    // The page side: record what the replay posts, and answer its gated boundaries.
    window.postMessage = ((data: Posted) => {
      posted.push(data);
      if (data.type === REPLAY_PAGE_MESSAGE && data.wait) {
        const reply = answer(data);
        if (reply)
          setTimeout(() =>
            window.dispatchEvent(
              new MessageEvent('message', {
                data: { type: REPLAY_GATE_MESSAGE, id: data.id, ...reply },
                source: window,
              }),
            ),
          );
      }
    }) as typeof window.postMessage;
  });

  afterEach(() => {
    answer = () => ({ action: 'continue' });
  });

  const run = (extra: Partial<ReplayOptions> = {}) =>
    igFeedReplay({ maxPages: 10, gapMs: 1, runId: 'g1', gate: true, gateTimeoutMs: 200, ...extra });

  it('waits at each boundary and continues to the end of the feed', async () => {
    expect(await run()).toMatchObject({ reason: 'end_of_feed', pages: 4, cursor: null });
    expect(requests).toEqual(['', 'A', 'B', 'C']);
    expect(posted.filter((p) => p.type === REPLAY_PAGE_MESSAGE)).toEqual([
      { type: REPLAY_PAGE_MESSAGE, id: 'g1', page: 1, cursor: 'A', wait: true },
      { type: REPLAY_PAGE_MESSAGE, id: 'g1', page: 2, cursor: 'B', wait: true },
      { type: REPLAY_PAGE_MESSAGE, id: 'g1', page: 3, cursor: 'C', wait: true },
    ]);
  });

  it('stops when told to, and reports the cursor it would have read', async () => {
    answer = (page) => ({ action: page.page === 2 ? 'stop' : 'continue' });
    expect(await run()).toMatchObject({ reason: 'stopped', pages: 2, cursor: 'B' });
    expect(requests).toEqual(['', 'A']);
  });

  it('jumps to the cursor it is given', async () => {
    answer = (page) => (page.page === 1 ? { action: 'jump', cursor: 'J' } : { action: 'continue' });
    expect(await run()).toMatchObject({ reason: 'end_of_feed', pages: 2 });
    expect(requests).toEqual(['', 'J']);
  });

  it('ends when nobody answers, and does not wait at the page cap', async () => {
    answer = () => null;
    expect(await run()).toMatchObject({ reason: 'gate_timeout', pages: 1, cursor: 'A' });
    answer = () => ({ action: 'continue' });
    posted.length = 0;
    expect(await run({ maxPages: 2 })).toMatchObject({ reason: 'page_cap', pages: 2, cursor: 'B' });
    expect(posted.filter((p) => p.type === REPLAY_PAGE_MESSAGE).map((p) => p.wait)).toEqual([
      true,
      false,
    ]);
  });

  it('ignores a gate answer for another replay', async () => {
    answer = () => null;
    const replay = run({ gateTimeoutMs: 300 });
    await vi.waitFor(() => expect(posted.some((p) => p.type === REPLAY_PAGE_MESSAGE)).toBe(true));
    window.dispatchEvent(
      new MessageEvent('message', {
        data: { type: REPLAY_GATE_MESSAGE, id: 'other', action: 'stop' },
        source: window,
      }),
    );
    expect(await replay).toMatchObject({ reason: 'gate_timeout' });
  });

  it('the toString() copy Chrome injects behaves the same', async () => {
    const injected = new Function(`return (${igFeedReplay.toString()});`)() as typeof igFeedReplay;
    expect(
      await injected({ maxPages: 10, gapMs: 1, runId: 'g1', gate: true, gateTimeoutMs: 200 }),
    ).toMatchObject({
      reason: 'end_of_feed',
      pages: 4,
    });
  });

  it("reads Pinterest's inline page inside an ssr scope, and is self-contained", () => {
    const read = new Function(
      `return (${pinterestSsrRead.toString()});`,
    )() as typeof pinterestSsrRead;
    const hook = vi.fn();
    (window as Window & { __ssReplayPinterest?: () => void }).__ssReplayPinterest = hook;
    expect(read('ssr-1')).toBe(true);
    expect(hook).toHaveBeenCalledOnce();
    expect(posted.map((p) => [p.type, p.phase])).toEqual([
      [SCOPE_MESSAGE, 'start'],
      [SCOPE_MESSAGE, 'end'],
    ]);
    delete (window as Window & { __ssReplayPinterest?: () => void }).__ssReplayPinterest;
    expect(read('ssr-2')).toBe(false);
  });
});

// ── Parsers ─────────────────────────────────────────────────────────────────

describe('sync message parsers', () => {
  const plan = {
    kind: MSG.syncRun,
    runId: '01JRUN',
    platform: 'instagram',
    listingKey: 'instagram:ig_saved',
    incremental: true,
    stopAfterKnown: 10,
    resumeCursor: null,
    replay: true,
    scroll: false,
    scrollSettleMs: 650,
    maxSteps: 16_000,
    maxRunMs: 1_800_000,
  };

  it('accept the documented shapes', () => {
    expect(parseSyncRunMessage(plan)).toEqual(plan);
    expect(parseSyncRunMessage({ ...plan, resumeCursor: 'QVFE' })?.resumeCursor).toBe('QVFE');
    expect(
      parseReplayPageMessage({
        type: REPLAY_PAGE_MESSAGE,
        id: 'r',
        page: 2,
        cursor: null,
        wait: true,
      }),
    ).toEqual({
      id: 'r',
      page: 2,
      cursor: null,
      wait: true,
    });
    expect(
      parseSyncMainRequest({ kind: MSG.syncMain, runId: 'r', op: 'replay', replayId: 'p' }),
    ).toMatchObject({ op: 'replay' });
    expect(
      parseSyncMainRequest({ kind: MSG.syncMain, runId: 'r', op: 'pinterest_ssr' }),
    ).toMatchObject({ replayId: null });
    expect(parseSyncKnownRequest({ kind: MSG.syncKnown, runId: 'r', reset: true })).toEqual({
      kind: MSG.syncKnown,
      runId: 'r',
      reset: true,
    });
    expect(
      parseSyncProgressMessage({
        kind: MSG.syncProgress,
        runId: 'r',
        phase: 'scroll',
        steps: 3,
        replayPages: 0,
      }),
    ).toBeTruthy();
    expect(
      parseSyncEndMessage({
        kind: MSG.syncEnd,
        runId: 'r',
        reason: 'stalled',
        skipped: ['scroll', 'bogus'],
        steps: 4,
      }),
    ).toEqual({
      kind: MSG.syncEnd,
      runId: 'r',
      reason: 'stalled',
      resumeCursor: null,
      errorCode: null,
      skipped: ['scroll'],
      steps: 4,
      replayPages: 0,
    });
    expect(
      parseSyncStartRequest({
        kind: MSG.syncStart,
        tabId: 3,
        collection: 'none',
        name: '  Dinners ',
      }),
    ).toEqual({
      kind: MSG.syncStart,
      tabId: 3,
      collection: 'none',
      name: 'Dinners',
    });
    expect(parseSyncStopRequest({ kind: MSG.syncStop, tabId: 3 })).toEqual({
      kind: MSG.syncStop,
      tabId: 3,
    });
  });

  it('refuse malformed or out-of-bounds values', () => {
    expect(parseSyncRunMessage({ ...plan, platform: 'myspace' })).toBeNull();
    expect(parseSyncRunMessage({ ...plan, maxSteps: 16_001 })).toBeNull();
    expect(parseSyncRunMessage({ ...plan, maxRunMs: -1 })).toBeNull();
    expect(parseSyncRunMessage({ ...plan, resumeCursor: 'x'.repeat(4097) })).toBeNull();
    expect(parseSyncRunMessage({ ...plan, kind: MSG.syncEnd })).toBeNull();
    expect(
      parseReplayPageMessage({ type: REPLAY_PAGE_MESSAGE, id: 'r', page: -1, cursor: null }),
    ).toBeNull();
    expect(
      parseReplayPageMessage({ type: REPLAY_PAGE_MESSAGE, id: 'r', page: 1, cursor: 7 }),
    ).toBeNull();
    expect(parseSyncMainRequest({ kind: MSG.syncMain, runId: 'r', op: 'eval' })).toBeNull();
    expect(parseSyncMainRequest({ kind: MSG.syncMain, runId: 'r', op: 'replay' })).toBeNull();
    expect(
      parseSyncProgressMessage({
        kind: MSG.syncProgress,
        runId: 'r',
        phase: 'dancing',
        steps: 1,
        replayPages: 0,
      }),
    ).toBeNull();
    expect(parseSyncEndMessage({ kind: MSG.syncEnd, runId: 'r', reason: 'bored' })).toBeNull();
    expect(
      parseSyncStartRequest({ kind: MSG.syncStart, tabId: 3, collection: 'existing' }),
    ).toBeNull();
    expect(parseSyncStopRequest({ kind: MSG.syncStop, tabId: '3' })).toBeNull();
  });
});

// ── The relay's observer ────────────────────────────────────────────────────

describe('createRelay and the sync controller', () => {
  it('shows the controller every hook batch, scope and replay page, and waits for deliveries', async () => {
    const observer = { intercept: vi.fn(), scope: vi.fn(), replayPage: vi.fn() };
    let deliver!: () => void;
    const relay = createRelay(window, {
      send: () => new Promise((resolve) => (deliver = () => resolve(undefined))),
      pageUrl: () => 'https://www.instagram.com/someone/saved/all-posts/',
      now: () => 1,
      warn: vi.fn(),
      docId: 'doc',
      viewer: () => null,
      observer,
    });
    const event = (data: unknown) => new MessageEvent('message', { data, source: window });
    relay(event({ type: SCOPE_MESSAGE, phase: 'start', source: 'replay', id: 'r', detail: {} }));
    relay(
      event({
        type: 'SOCIAL_SAVED_INTERCEPT',
        platform: 'instagram',
        items: [{ id: '1' }],
        hasNextPage: true,
      }),
    );
    relay(event({ type: REPLAY_PAGE_MESSAGE, id: 'r', page: 1, cursor: 'A', wait: true }));
    expect(observer.scope).toHaveBeenCalledWith(
      expect.objectContaining({ phase: 'start', id: 'r' }),
    );
    expect(observer.intercept).toHaveBeenCalledWith(
      expect.objectContaining({ platform: 'instagram' }),
      'replay',
    );
    expect(observer.replayPage).toHaveBeenCalledWith({ id: 'r', page: 1, cursor: 'A', wait: true });
    let idle = false;
    void relay.idle().then(() => (idle = true));
    await Promise.resolve();
    expect(idle).toBe(false);
    deliver();
    await vi.waitFor(() => expect(idle).toBe(true));
  });
});

// ── The panel ───────────────────────────────────────────────────────────────

describe('the "Sync now" section', () => {
  const pong: BridgePong = { ok: true, docId: 'd', viewer: 'someone' };
  const state = (extra: Partial<PanelState> = {}): PanelState =>
    ({
      paired: true,
      outdated: false,
      serverModes: {
        instagram: { replay: true, scroll: true },
        twitter: { replay: false, scroll: true },
        pinterest: { replay: false, scroll: true },
      },
      syncs: [],
      ...extra,
    }) as PanelState;
  const tab = (url: string, p: BridgePong | null = pong) => ({ id: 1, url, pong: p });

  it('tells why the active tab cannot be synced', () => {
    const folder = 'https://www.instagram.com/someone/saved/recipes/17890000000000001/';
    expect(syncAvailability(tab(folder), state())).toMatchObject({ ok: true });
    expect(syncAvailability(tab(folder), state({ paired: false }))).toMatchObject({
      reason: 'not_paired',
    });
    expect(syncAvailability(tab('https://x.com/home'), state())).toMatchObject({
      reason: 'not_a_listing',
    });
    expect(syncAvailability(tab(folder, null), state())).toMatchObject({ reason: 'reload_tab' });
    expect(syncAvailability(tab('https://www.pinterest.com/other/cakes/'), state())).toMatchObject({
      reason: 'not_own_board',
    });
    expect(
      syncAvailability(
        tab(folder),
        state({
          serverModes: { ...state().serverModes, instagram: { replay: false, scroll: false } },
        }),
      ),
    ).toMatchObject({ reason: 'disabled' });
    expect(
      syncAvailability(
        tab(folder),
        state({ syncs: [{ state: 'open', platform: 'instagram', tabId: 2 } as SyncView] }),
      ),
    ).toMatchObject({ reason: 'busy' });
  });

  it('has a string for every reason, phase, mode and refusal, in both languages', () => {
    const keys = [
      ...SYNC_END_REASONS.map((r) => `sync.reason.${r}`),
      ...SYNC_PHASES.map((p) => `sync.phase.${p}`),
      ...SYNC_MODES.map((m) => `sync.mode.${m}`),
      ...[
        'not_paired',
        'outdated',
        'not_a_listing',
        'reload_tab',
        'not_own_board',
        'viewer_unknown',
        'disabled',
        'busy',
      ].map((code) => `sync.refused.${code}`),
    ];
    for (const key of keys) expect(hasMessage(key), key).toBe(true);
    const t = createTranslate('en');
    const view = {
      state: 'open',
      phase: 'replay',
      replayPages: 3,
      scanned: 42,
      inserted: 5,
      known: 37,
    } as SyncView;
    expect(liveText(t, view)).toBe('Reading, page 3 · 42 read, 5 new, 37 already saved');
    expect(liveText(t, { ...view, state: 'ended', queued: 1 })).toMatch(/^Sending the last item/);
    expect(resultText(t, { ...view, stopReason: 'known_run' })).toBe(
      'Done: reached the posts already saved · 42 read, 5 new, 37 already saved',
    );
  });
});
