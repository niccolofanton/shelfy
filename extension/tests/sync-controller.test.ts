// The sync controller (content/sync/controller.ts, P2-13) with fake timers: the IG replay's
// gate (continue, the incremental stop, the resume jump), the scroll's end signals, the
// termination rules (login wall, leaving the listing, the time cap, the user's stop), the
// failure codes, and what it reports to the worker.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { SyncController, type ControllerDeps } from '../src/content/sync/controller';
import {
  MSG,
  REPLAY_GATE_MESSAGE,
  type InterceptMessage,
  type SyncEndMessage,
  type SyncPlan,
} from '../src/shared/protocol';

const IG_FOLDER = 'https://www.instagram.com/someone/saved/recipes/17890000000000001/';
const IG_KEY = 'instagram:ig_collection:17890000000000001';
const X_URL = 'https://x.com/i/bookmarks';
const PIN_BOARD = 'https://www.pinterest.com/someone/recipes/';
const PIN_KEY = 'pinterest:pin_board:someone/recipes';

type Sent = { kind: string; [key: string]: unknown };

function plan(extra: Partial<SyncPlan> = {}): SyncPlan {
  return {
    runId: 'run-1',
    platform: 'instagram',
    listingKey: IG_KEY,
    incremental: false,
    stopAfterKnown: 3,
    resumeCursor: null,
    replay: true,
    scroll: false,
    scrollSettleMs: 650,
    maxSteps: 16_000,
    maxRunMs: 1_800_000,
    ...extra,
  };
}

function setup(href: string, options: { height?: number; initialHref?: string } = {}) {
  const sent: Sent[] = [];
  const page: Array<Record<string, unknown>> = [];
  const view = { href, y: 0, height: options.height ?? 100_000 };
  const answers = {
    known: (): unknown => ({ ok: true, streak: 0, settled: true }),
    main: (): unknown => ({ ok: true }),
  };
  const deps: ControllerDeps = {
    send: async <T>(message: unknown): Promise<T> => {
      const m = message as Sent;
      sent.push(m);
      if (m.kind === MSG.syncKnown) return answers.known() as T;
      if (m.kind === MSG.syncMain) return answers.main() as T;
      return { ok: true } as T;
    },
    postToPage: (message) => void page.push(message as Record<string, unknown>),
    href: () => view.href,
    initialHref: options.initialHref ?? href,
    relayIdle: async () => undefined,
    scroll: {
      scrollY: () => view.y,
      innerHeight: () => 1000,
      scrollBy: (dy) => (view.y = Math.min(view.height - 1000, view.y + dy)),
      scrollTo: (y) => (view.y = y),
      revealLast: () => undefined,
    },
    sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
    now: () => Date.now(),
    randomId: () => 'abc',
    warn: vi.fn(),
  };
  const controller = new SyncController(deps);
  const ofKind = (kind: string) => sent.filter((m) => m.kind === kind);
  const end = () => ofKind(MSG.syncEnd)[0] as unknown as SyncEndMessage | undefined;
  return { controller, sent, page, view, answers, ofKind, end };
}

const items = (
  platform: InterceptMessage['platform'],
  ids: string[],
  hasNextPage: boolean | null = true,
): InterceptMessage => ({
  platform,
  items: ids.map((id) => ({ id })),
  hasNextPage,
});

const tick = (ms = 0) => vi.advanceTimersByTimeAsync(ms);

beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
});

describe('SyncController on Instagram (the gated replay)', () => {
  /** Starts a replay walk and answers the worker's injection with the replay's start. */
  async function started(s: ReturnType<typeof setup>, p: SyncPlan) {
    expect(s.controller.start(p)).toBe(true);
    await tick();
    const [main] = s.ofKind(MSG.syncMain);
    expect(main).toMatchObject({ runId: 'run-1', op: 'replay', replayId: 'replay-abc' });
    s.controller.scope({ phase: 'start', source: 'replay', id: 'replay-abc', detail: {} });
    return 'replay-abc';
  }

  const replayEnd = (s: ReturnType<typeof setup>, detail: Record<string, string | number | null>) =>
    s.controller.scope({ phase: 'end', source: 'replay', id: 'replay-abc', detail });

  it('walks to the end of the feed, answering every boundary, and reports the skipped scroll', async () => {
    const s = setup(IG_FOLDER);
    const id = await started(s, plan());
    s.controller.intercept(items('instagram', ['1', '2']));
    s.controller.replayPage({ id, page: 1, cursor: 'C1', wait: true });
    await tick();
    expect(s.page).toEqual([{ type: REPLAY_GATE_MESSAGE, id, action: 'continue', cursor: null }]);
    expect(s.ofKind(MSG.syncKnown)).toEqual([]); // not incremental: nothing to ask
    replayEnd(s, { pages: 2, reason: 'end_of_feed', status: 200 });
    await tick(10);
    expect(s.end()).toMatchObject({
      runId: 'run-1',
      reason: 'end_of_feed',
      resumeCursor: null,
      errorCode: null,
      skipped: ['scroll'],
      replayPages: 2,
    });
    expect(s.controller.runId).toBeNull();
  });

  it('stops at the first boundary where the known run reaches the threshold (incremental)', async () => {
    const s = setup(IG_FOLDER);
    s.answers.known = () => ({ ok: true, streak: 3, settled: true });
    const id = await started(s, plan({ incremental: true, scroll: true }));
    s.controller.replayPage({ id, page: 1, cursor: 'C1', wait: true });
    await tick();
    expect(s.page.at(-1)).toMatchObject({ action: 'stop' });
    replayEnd(s, { pages: 1, reason: 'stopped', status: 200 });
    await tick(10);
    // The known run is terminal: no scroll after it.
    expect(s.end()).toMatchObject({ reason: 'known_run', skipped: [] });
    expect(s.ofKind(MSG.syncProgress).every((m) => m.phase !== 'scroll')).toBe(true);
  });

  it('goes on while the count is short or cannot be settled (API down)', async () => {
    const s = setup(IG_FOLDER);
    s.answers.known = () => ({ ok: true, streak: 2, settled: true });
    const id = await started(s, plan({ incremental: true }));
    s.controller.replayPage({ id, page: 1, cursor: 'C1', wait: true });
    await tick();
    s.answers.known = () => ({ ok: true, streak: 40, settled: false });
    s.controller.replayPage({ id, page: 2, cursor: 'C2', wait: true });
    await tick();
    expect(s.page.map((m) => m.action)).toEqual(['continue', 'continue']);
  });

  it('resumes a capped walk: the known head, then a jump to the cursor (P2-G2)', async () => {
    const s = setup(IG_FOLDER);
    s.answers.known = () => ({ ok: true, streak: 4, settled: true });
    const id = await started(s, plan({ resumeCursor: 'S2' }));
    s.controller.replayPage({ id, page: 1, cursor: 'S1', wait: true });
    await tick();
    expect(s.page.at(-1)).toEqual({ type: REPLAY_GATE_MESSAGE, id, action: 'jump', cursor: 'S2' });
    expect(s.ofKind(MSG.syncKnown).map((m) => m.reset)).toEqual([false, true]);
    // After the jump the walk is not incremental: it continues without asking.
    s.controller.replayPage({ id, page: 2, cursor: 'S3', wait: true });
    await tick();
    expect(s.page.at(-1)).toMatchObject({ action: 'continue' });
    expect(s.ofKind(MSG.syncKnown)).toHaveLength(2);
    // The page cap: the last boundary is reported without waiting.
    s.controller.replayPage({ id, page: 3, cursor: 'S4', wait: false });
    replayEnd(s, { pages: 3, reason: 'page_cap', status: 200 });
    await tick(10);
    expect(s.page).toHaveLength(2);
    expect(s.end()).toMatchObject({ reason: 'page_cap', resumeCursor: 'S4' });
  });

  it('maps replay failures: a signed-out answer, an HTTP error, a replay that never starts', async () => {
    const signedOut = setup(IG_FOLDER);
    await started(signedOut, plan({ scroll: true }));
    replayEnd(signedOut, { pages: 0, reason: 'http_error', status: 401 });
    await tick(10);
    expect(signedOut.end()).toMatchObject({ reason: 'login_required', errorCode: null });

    const failed = setup(IG_FOLDER);
    await started(failed, plan());
    replayEnd(failed, { pages: 3, reason: 'http_error', status: 500 });
    await tick(10);
    expect(failed.end()).toMatchObject({ reason: 'error', errorCode: 'replay_http_500' });

    const never = setup(IG_FOLDER);
    never.controller.start(plan());
    await tick(15_000);
    expect(never.end()).toMatchObject({ reason: 'error', errorCode: 'replay_unavailable' });

    const refused = setup(IG_FOLDER);
    refused.answers.main = () => ({ ok: false });
    refused.controller.start(plan());
    await tick(10);
    expect(refused.end()).toMatchObject({ reason: 'error', errorCode: 'replay_unavailable' });
  });

  it('a login wall or leaving the listing stops the replay at its next boundary', async () => {
    const s = setup(IG_FOLDER);
    const id = await started(s, plan());
    s.view.href = 'https://www.instagram.com/p/ABCdef123/'; // a post detail: not leaving
    await tick(1_000);
    s.controller.replayPage({ id, page: 1, cursor: 'C1', wait: true });
    await tick();
    expect(s.page.at(-1)).toMatchObject({ action: 'continue' });
    s.view.href = 'https://www.instagram.com/accounts/login/';
    await tick(600);
    s.controller.replayPage({ id, page: 2, cursor: 'C2', wait: true });
    await tick();
    expect(s.page.at(-1)).toMatchObject({ action: 'stop' });
    replayEnd(s, { pages: 2, reason: 'stopped', status: 200 });
    await tick(10);
    expect(s.end()).toMatchObject({ reason: 'login_required' });
  });

  it('ignores page and scope messages of another replay', async () => {
    const s = setup(IG_FOLDER);
    await started(s, plan());
    s.controller.replayPage({ id: 'forged', page: 1, cursor: 'X', wait: true });
    s.controller.scope({
      phase: 'end',
      source: 'replay',
      id: 'forged',
      detail: { reason: 'end_of_feed' },
    });
    await tick(100);
    expect(s.page).toEqual([]);
    expect(s.end()).toBeUndefined();
  });
});

describe('SyncController with the scroll (X, Pinterest)', () => {
  const xPlan = (extra: Partial<SyncPlan> = {}) =>
    plan({
      platform: 'twitter',
      listingKey: 'twitter:x_bookmarks',
      replay: false,
      scroll: true,
      scrollSettleMs: 750,
      ...extra,
    });

  it('ends at hasNextPage === false, with steps and progress reported', async () => {
    const s = setup(X_URL);
    s.controller.start(xPlan());
    await tick(3_000);
    s.controller.intercept(items('twitter', ['x1', 'x2'], false));
    await tick(2_000);
    expect(s.end()).toMatchObject({ reason: 'end_of_feed', skipped: [] });
    expect(s.end()?.steps).toBeGreaterThan(2);
    expect(s.ofKind(MSG.syncProgress).some((m) => m.phase === 'scroll')).toBe(true);
    expect(s.view.y).toBeGreaterThan(0);
  });

  it('stops at the known run on a page boundary (incremental)', async () => {
    const s = setup(X_URL);
    s.answers.known = () => ({ ok: true, streak: 2, settled: true });
    s.controller.start(xPlan({ incremental: true, stopAfterKnown: 2 }));
    await tick(1_000);
    expect(s.ofKind(MSG.syncKnown)).toEqual([]); // no page yet: no boundary
    s.controller.intercept(items('twitter', ['x1', 'x2']));
    await tick(2_000);
    expect(s.ofKind(MSG.syncKnown)).toHaveLength(1);
    expect(s.end()).toMatchObject({ reason: 'known_run' });
  });

  it('ends without an end signal as stalled, after both passes', async () => {
    const s = setup(X_URL, { height: 1_000 });
    s.controller.start(xPlan());
    await tick(200_000);
    expect(s.end()).toMatchObject({ reason: 'stalled' });
  });

  it('applies the time and step caps', async () => {
    const timed = setup(X_URL);
    timed.controller.start(xPlan({ maxRunMs: 5_000 }));
    await tick(7_000);
    expect(timed.end()).toMatchObject({ reason: 'time_cap' });

    const stepped = setup(X_URL);
    stepped.controller.start(xPlan({ maxSteps: 4 }));
    await tick(10_000);
    expect(stepped.end()).toMatchObject({ reason: 'time_cap', steps: 4 });
  });

  it("stops on the user's stop, a login wall, or leaving the bookmarks", async () => {
    const user = setup(X_URL);
    user.controller.start(xPlan());
    expect(user.controller.start(xPlan())).toBe(false); // one walk per document
    await tick(1_000);
    user.controller.abort();
    await tick(1_000);
    expect(user.end()).toMatchObject({ reason: 'user' });

    const login = setup(X_URL);
    login.controller.start(xPlan());
    await tick(1_000);
    login.view.href = 'https://x.com/i/flow/login';
    await tick(2_000);
    expect(login.end()).toMatchObject({ reason: 'login_required' });

    const left = setup(X_URL);
    left.controller.start(xPlan());
    await tick(1_000);
    left.view.href = 'https://x.com/home';
    await tick(2_000);
    expect(left.end()).toMatchObject({ reason: 'user' });
  });

  it("reads Pinterest's inline first page only on the page the document was loaded at", async () => {
    const pinPlan = plan({
      platform: 'pinterest',
      listingKey: PIN_KEY,
      replay: false,
      scroll: true,
    });
    const fresh = setup(PIN_BOARD);
    fresh.controller.start(pinPlan);
    await tick(1_000);
    expect(fresh.ofKind(MSG.syncMain)).toMatchObject([{ op: 'pinterest_ssr', runId: 'run-1' }]);
    fresh.controller.intercept(items('pinterest', ['p1'], false));
    await tick(2_000);
    expect(fresh.end()).toMatchObject({ reason: 'end_of_feed' });

    const navigated = setup(PIN_BOARD, { initialHref: 'https://www.pinterest.com/someone/cakes/' });
    navigated.controller.start(pinPlan);
    await tick(1_000);
    expect(navigated.ofKind(MSG.syncMain)).toEqual([]);
  });

  it("does not end Instagram on the replay's last page signal", async () => {
    const s = setup(IG_FOLDER);
    s.controller.start(plan({ replay: false, scroll: true }));
    await tick(1_000);
    s.controller.intercept(items('instagram', ['1'], false));
    await tick(3_000);
    expect(s.end()).toBeUndefined();
    s.controller.abort();
    await tick(1_000);
    expect(s.end()).toMatchObject({ reason: 'user', skipped: ['replay'] });
  });
});
