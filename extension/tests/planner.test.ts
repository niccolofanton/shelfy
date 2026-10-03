import { describe, expect, it, vi } from 'vitest';
import {
  buildSyncSteps,
  DEFAULT_SCHEDULE,
  nextReminder,
  parseSchedule,
  parseSources,
  stepErrorAction,
  type ExtensionSource,
} from '../src/sw/planner/model';
import {
  INSTAGRAM_BACKLOG_DAY_KEY,
  PLANNER_KEY,
  PlannerService,
  STEP_TIMEOUT_MS,
  type PlannerBrowser,
  type PlannerDeps,
} from '../src/sw/planner/service';
import { PlannerScheduler, registerTaskPollHandler } from '../src/sw/planner/scheduler';
import type { StartRequest } from '../src/sw/sync/service';
import type { SyncView } from '../src/sw/sync/history';
import type { SyncStartAnswer } from '../src/shared/protocol';
import { memoryStorage } from './helpers';

const FOLDER = 'https://www.instagram.com/someone/saved/recipes/123/';
const sources: ExtensionSource[] = [
  {
    platform: 'instagram',
    listing: { kind: 'ig_collection', externalId: '123', name: 'Recipes' },
    collectionId: 1,
  },
  {
    platform: 'instagram',
    listing: { kind: 'ig_collection', externalId: '456', name: 'Travel' },
    collectionId: 2,
  },
  {
    platform: 'pinterest',
    listing: { kind: 'pin_board', externalId: 'someone/recipes', name: 'Recipes' },
    collectionId: 3,
  },
];
function view(runId: string): SyncView {
  return {
    runId,
    serverId: runId,
    tabId: 1,
    platform: 'instagram',
    trigger: 'web',
    listingKey: 'test',
    listingName: null,
    state: 'ended',
    phase: null,
    stopReason: 'end_of_feed',
    errorCode: null,
    skipped: [],
    incremental: false,
    pages: 1,
    replayPages: 1,
    steps: 1,
    scanned: 1,
    inserted: 1,
    known: 0,
    queued: 0,
    startedAt: 0,
    endedAt: 1,
  };
}
async function finished(service: PlannerService) {
  for (let n = 0; n < 20000; n++) {
    const result = await service.snapshot();
    if (
      result.jobs.length &&
      result.jobs.every((job) => !['navigating', 'syncing'].includes(job.status))
    )
      return result.jobs;
    await Promise.resolve();
  }
  throw new Error('planner did not settle');
}
function fixture() {
  const storage = memoryStorage();
  let now = new Date(2026, 9, 3, 10).getTime();
  const urls = new Map<number, string>([[9, 'https://www.instagram.com/p/already-open/']]);
  let nextTab = 0;
  const browser: PlannerBrowser = {
    open: vi.fn(async () => {
      const tabId = ++nextTab;
      urls.set(tabId, 'about:blank');
      return { tabId, windowId: tabId };
    }),
    existing: vi.fn(async (tabId) => ({ tabId, windowId: 9 })),
    get: vi.fn(async (tabId) => ({ url: urls.get(tabId), status: 'complete' })),
    navigate: vi.fn(async (tabId, url) => {
      urls.set(tabId, url);
    }),
    reload: vi.fn(async () => {}),
    ping: vi.fn(async () => ({ ok: true as const, docId: 'doc', viewer: 'someone' })),
    instagramUsername: vi.fn(async () => ({ username: 'someone', login: false })),
    folderLinks: vi.fn(async () => [FOLDER, 'https://www.instagram.com/someone/saved/travel/456/']),
  };
  const ended: SyncView[] = [];
  const sync = {
    start: vi.fn(async (_request: StartRequest): Promise<SyncStartAnswer> => {
      const runId = String(ended.length + 1);
      ended.push(view(runId));
      return { ok: true, runId };
    }),
    stopPlatform: vi.fn(async () => ({ ok: true })),
    syncing: vi.fn(async () => ({ instagram: false, twitter: false, pinterest: false })),
    views: vi.fn(async () => ended),
  };
  const eligibility = vi.fn(async (): Promise<string | null> => null);
  const readSources = vi.fn(async () => sources);
  const deps: PlannerDeps = {
    storage,
    browser,
    sync,
    eligibility,
    sources: readSources,
    now: () => now,
    sleep: async (ms) => {
      now += ms;
    },
    changed: vi.fn(),
    log: vi.fn(),
  };
  const service = new PlannerService(deps);
  return {
    service,
    deps,
    browser,
    sync,
    eligibility,
    readSources,
    urls,
    storage,
    now: () => now,
    advance: (ms: number) => {
      now += ms;
    },
  };
}

describe('source planner and boundaries', () => {
  it('includes IG all plus native folders, X bookmarks, Pinterest boards and one mapped collection', () => {
    expect(
      buildSyncSteps({ platform: 'instagram' }, [...sources, sources[0]]).map((step) => step.kind),
    ).toEqual(['ig-all', 'ig-folder', 'ig-folder']);
    expect(buildSyncSteps({ platform: 'twitter' }, sources)).toMatchObject([
      { kind: 'x-bookmarks', collection: { mode: 'none' } },
    ]);
    expect(buildSyncSteps({ platform: 'pinterest' }, sources)).toMatchObject([
      { kind: 'pin-board', externalId: 'someone/recipes' },
    ]);
    expect(buildSyncSteps({ platform: 'instagram', collectionId: 1 }, sources)).toMatchObject([
      { kind: 'ig-folder', collection: { mode: 'existing', id: 1 } },
    ]);
    expect(buildSyncSteps({ platform: 'instagram', collectionId: 3 }, sources)).toEqual([]);
    expect(buildSyncSteps({ platform: 'pinterest' }, [])).toEqual([]);
  });
  it('accepts only native source ids and classifies login as platform-aborting', () => {
    expect(parseSources({ items: sources })).toEqual(sources);
    expect(
      parseSources({
        items: [
          { ...sources[0], listing: { kind: 'ig_collection', externalId: '../evil' } },
          { ...sources[2], listing: { kind: 'pin_board', externalId: 'someone/board?url=evil' } },
        ],
      }),
    ).toEqual([]);
    expect(stepErrorAction('login_required')).toBe('abort');
    expect(stepErrorAction('cancelled')).toBe('stop');
    for (const code of ['navigation', 'bridge', 'folder_missing', 'step_timeout', 'start'] as const)
      expect(stepErrorAction(code)).toBe('skip');
  });
  it('serializes starts per platform and refuses before opening windows', async () => {
    const f = fixture();
    let resolve!: (sources: ExtensionSource[]) => void;
    f.readSources.mockReturnValueOnce(
      new Promise((done) => {
        resolve = done;
      }),
    );
    const started = f.service.start({ platform: 'instagram' });
    expect(await f.service.start({ platform: 'instagram' })).toEqual({ ok: false, code: 'busy' });
    for (let i = 0; i < 10; i++) await Promise.resolve();
    resolve(sources);
    expect(await started).toEqual({ ok: true });
    await finished(f.service);
    f.eligibility.mockResolvedValueOnce('not_paired');
    expect(await f.service.start({ platform: 'twitter' })).toEqual({
      ok: false,
      code: 'not_paired',
    });
    f.eligibility.mockResolvedValueOnce('disabled');
    expect(await f.service.start({ platform: 'twitter' })).toEqual({ ok: false, code: 'disabled' });
    expect(f.browser.open).toHaveBeenCalledTimes(1);
  });
  it('walks native steps through the existing controller with web trigger and collection mapping', async () => {
    const f = fixture();
    await f.service.start({ platform: 'instagram' });
    expect(await finished(f.service)).toMatchObject([{ status: 'done', step: 3, skipped: 0 }]);
    expect(f.browser.open).toHaveBeenCalledWith('instagram', null, false);
    expect(f.sync.start.mock.calls.map(([request]) => request.collection)).toEqual([
      { mode: 'none' },
      { mode: 'existing', id: 1 },
      { mode: 'existing', id: 2 },
    ]);
    expect(f.sync.start.mock.calls.every(([request]) => request.trigger === 'web')).toBe(true);
  });
  it('aborts the platform on a login step, while another platform can finish', async () => {
    const f = fixture();
    vi.mocked(f.browser.navigate).mockImplementation(async (tabId, url) => {
      f.urls.set(
        tabId,
        url.includes('all-posts') ? 'https://www.instagram.com/accounts/login/' : url,
      );
    });
    await f.service.start({ platform: 'instagram' });
    expect(await finished(f.service)).toMatchObject([
      { status: 'error', code: 'login_required', step: 1 },
    ]);
    expect(f.sync.start).not.toHaveBeenCalled();
    await f.service.start({ platform: 'twitter' });
    expect((await finished(f.service)).find((job) => job.platform === 'twitter')?.status).toBe(
      'done',
    );
  });
  it('waits 20 seconds for a bridge, reloads once and then skips the failed step', async () => {
    const f = fixture();
    vi.mocked(f.browser.ping).mockResolvedValue(null);
    await f.service.start({ platform: 'twitter' });
    expect(await finished(f.service)).toMatchObject([
      { status: 'done', skipped: 1, code: 'bridge' },
    ]);
    expect(f.browser.reload).toHaveBeenCalledTimes(1);
    expect(f.sync.start).not.toHaveBeenCalled();
  });
  it('skips navigation after 45 seconds and continues the next native folder', async () => {
    const f = fixture();
    const startedAt = f.now();
    vi.mocked(f.browser.navigate).mockImplementation(async (tabId, url) => {
      f.urls.set(tabId, url.includes('all-posts') ? 'https://www.instagram.com/' : url);
    });
    await f.service.start({ platform: 'instagram' });
    expect(await finished(f.service)).toMatchObject([{ status: 'done', skipped: 1, step: 3 }]);
    expect(f.sync.start.mock.calls.map(([request]) => request.collection)).toEqual([
      { mode: 'existing', id: 1 },
      { mode: 'existing', id: 2 },
    ]);
    expect(f.now() - startedAt).toBe(45_000);
  });
  it('does not resume an unattended window after a worker restart', async () => {
    const f = fixture();
    await f.storage.set({
      [PLANNER_KEY]: {
        instagram: { platform: 'instagram', status: 'syncing', windowId: 9, tabId: 9 },
      },
    });
    await f.service.recover();
    expect((await f.service.snapshot()).jobs).toMatchObject([
      { status: 'stopped', code: 'worker_restarted' },
    ]);
    expect(f.sync.stopPlatform).toHaveBeenCalledWith('instagram', undefined);
    expect(f.browser.open).not.toHaveBeenCalled();
  });
  it('caps a running step at 35 minutes, stops it and skips it', async () => {
    const f = fixture();
    f.sync.views.mockImplementation(async () => [{ ...view('1'), state: 'open' }]);
    await f.service.start({ platform: 'twitter' });
    expect(await finished(f.service)).toMatchObject([
      { status: 'done', skipped: 1, code: 'step_timeout' },
    ]);
    expect(f.sync.stopPlatform).toHaveBeenCalledWith('twitter', undefined);
    expect(STEP_TIMEOUT_MS).toBe(35 * 60_000);
  });
  it('runs a full IG backlog at most once a day using only the supplied existing tab', async () => {
    const f = fixture();
    expect(await f.service.requestInstagramBacklogSync({ backlog: 199, tabId: 9 })).toEqual({
      ok: false,
      code: 'below_threshold',
    });
    expect(await f.service.requestInstagramBacklogSync({ backlog: 200, tabId: 8 })).toEqual({
      ok: false,
      code: 'no_instagram_tab',
    });
    expect(await f.service.requestInstagramBacklogSync({ backlog: 200, tabId: 9 })).toEqual({
      ok: true,
    });
    await finished(f.service);
    expect(f.browser.open).not.toHaveBeenCalled();
    expect(f.browser.existing).toHaveBeenCalledWith(9);
    expect(
      f.sync.start.mock.calls.every(([request]) => request.full && request.trigger === 'scheduled'),
    ).toBe(true);
    expect(await f.service.requestInstagramBacklogSync({ backlog: 200, tabId: 9 })).toEqual({
      ok: false,
      code: 'daily_limit',
    });
    expect(
      (await f.storage.get(INSTAGRAM_BACKLOG_DAY_KEY))[INSTAGRAM_BACKLOG_DAY_KEY],
    ).toBeTruthy();
    f.advance(24 * 60 * 60_000);
    expect(await f.service.requestInstagramBacklogSync({ backlog: 200, tabId: 9 })).toEqual({
      ok: true,
    });
    await finished(f.service);
  });
  it('stops navigation on cancellation and does not create a new task tab', async () => {
    const f = fixture();
    let release!: () => void;
    vi.mocked(f.browser.navigate).mockImplementation(
      async () =>
        new Promise<void>((resolve) => {
          release = resolve;
        }),
    );
    await f.service.start({ platform: 'twitter' });
    for (let i = 0; i < 30; i++) await Promise.resolve();
    await f.service.stop('twitter');
    release();
    expect(await finished(f.service)).toMatchObject([{ status: 'stopped', code: 'cancelled' }]);
    expect(f.sync.start).not.toHaveBeenCalled();
  });
  it('discards sources A after re-pair B, including a coincident collection id', async () => {
    const f = fixture();
    const a = { token: 'token-A', tokenId: 'A', scopes: [], pairedAt: 0 };
    const b = { ...a, token: 'token-B', tokenId: 'B' };
    let current = a;
    f.deps.pairing = async () => current;
    let release!: (value: ExtensionSource[]) => void;
    f.readSources.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          release = resolve;
        }),
    );
    const pending = f.service.start({ platform: 'instagram', collectionId: 1 });
    for (let i = 0; i < 100 && !release; i++) await Promise.resolve();
    current = b;
    release(sources);
    expect(await pending).toEqual({ ok: false, code: 'cancelled' });
    expect(f.sync.start).not.toHaveBeenCalled();
    expect(f.browser.open).not.toHaveBeenCalled();
    f.readSources.mockResolvedValue([
      { ...sources[0], listing: { kind: 'ig_collection', externalId: '999', name: 'Account B' } },
    ]);
    vi.mocked(f.browser.folderLinks).mockResolvedValue([
      'https://www.instagram.com/someone/saved/b/999/',
    ]);
    expect(await f.service.start({ platform: 'instagram', collectionId: 1 })).toEqual({ ok: true });
    await finished(f.service);
    expect(f.sync.start).toHaveBeenCalledTimes(1);
    expect(f.sync.start).toHaveBeenCalledWith(
      expect.objectContaining({ expectedPairing: b, collection: { mode: 'existing', id: 1 } }),
    );
    expect(f.browser.navigate).not.toHaveBeenCalledWith(expect.anything(), FOLDER);
  });
  it('aborts account A while navigation awaits and never invokes the controller under B', async () => {
    const f = fixture();
    const a = { token: 'token-A', tokenId: 'A', scopes: [], pairedAt: 0 };
    let current = a;
    f.deps.pairing = async () => current;
    let release!: () => void;
    vi.mocked(f.browser.navigate).mockImplementationOnce(
      async () =>
        new Promise<void>((resolve) => {
          release = resolve;
        }),
    );
    await f.service.start({ platform: 'instagram' });
    for (let i = 0; i < 100 && !release; i++) await Promise.resolve();
    current = { ...a, token: 'token-B', tokenId: 'B' };
    release();
    for (let i = 0; i < 100; i++) await Promise.resolve();
    expect(f.sync.start).not.toHaveBeenCalled();
    expect((await f.service.snapshot()).jobs).toEqual([]);
    expect((await f.service.syncing()).instagram).toBe(false);
    expect(f.sync.stopPlatform).toHaveBeenCalledWith('instagram', 'A');
  });
  it('binds the once-daily watermark to the pairing and writes it only for an accepted start', async () => {
    const f = fixture();
    let key = 'A';
    f.deps.accountKey = async () => key;
    f.eligibility.mockResolvedValueOnce('not_paired');
    expect((await f.service.requestInstagramBacklogSync({ backlog: 200, tabId: 9 })).ok).toBe(
      false,
    );
    expect(
      (await f.storage.get(INSTAGRAM_BACKLOG_DAY_KEY))[INSTAGRAM_BACKLOG_DAY_KEY],
    ).toBeUndefined();
    expect((await f.service.requestInstagramBacklogSync({ backlog: 200, tabId: 9 })).ok).toBe(true);
    await finished(f.service);
    key = 'B';
    expect((await f.service.requestInstagramBacklogSync({ backlog: 200, tabId: 9 })).ok).toBe(true);
    await finished(f.service);
    expect(
      (await f.storage.get(INSTAGRAM_BACKLOG_DAY_KEY))[INSTAGRAM_BACKLOG_DAY_KEY],
    ).toMatchObject({ accountKey: 'B' });
  });
});

describe('daily reminders and task alarm seam', () => {
  it('defaults unattended off and schedules the next local hour, including tomorrow', () => {
    expect(DEFAULT_SCHEDULE.unattended).toBe(false);
    expect(parseSchedule({ enabled: true, unattended: true, hour: 25, minute: 0 })).toBeNull();
    const now = new Date(2026, 9, 3, 19).getTime();
    const next = new Date(nextReminder(now, DEFAULT_SCHEDULE));
    expect(next.getDate()).toBe(4);
    expect(next.getHours()).toBe(18);
  });
  it('notifies by default; clicking syncs all; only explicit opt-in starts minimized scheduled runs', async () => {
    let schedule = { ...DEFAULT_SCHEDULE, enabled: true };
    const planner = {
      schedule: async () => schedule,
      setSchedule: vi.fn(async (next) => {
        schedule = next;
      }),
      startAll: vi.fn(async () => []),
    };
    const alarms = {
      get: vi.fn(async () => undefined),
      create: vi.fn(async () => {}),
      clear: vi.fn(async () => true),
    };
    const notify = vi.fn(async () => {});
    const clearNotification = vi.fn(async () => {});
    const scheduler = new PlannerScheduler({
      planner,
      alarms,
      notify,
      clearNotification,
      now: () => new Date(2026, 9, 3, 10).getTime(),
      languages: ['it'],
    });
    await scheduler.ensure();
    expect(alarms.create).toHaveBeenCalledWith('shelfy.tasks.poll', { periodInMinutes: 5 });
    await scheduler.alarm('shelfy.sync.reminder');
    expect(notify).toHaveBeenCalled();
    expect(planner.startAll).not.toHaveBeenCalled();
    await scheduler.clicked('shelfy.sync.now');
    expect(planner.startAll).toHaveBeenCalledWith('web');
    await scheduler.setSchedule({ ...schedule, unattended: true });
    await scheduler.alarm('shelfy.sync.reminder');
    expect(planner.startAll).toHaveBeenCalledWith('scheduled', true);
    const poll = vi.fn(async () => {});
    const unregister = registerTaskPollHandler(poll);
    await scheduler.alarm('shelfy.tasks.poll');
    expect(poll).toHaveBeenCalledTimes(1);
    unregister();
    await scheduler.setSchedule({ ...schedule, enabled: false });
    expect(alarms.clear).toHaveBeenCalledWith('shelfy.sync.reminder');
  });
});

describe('C9 account identity binding', () => {
  const a = {
    accountId: 'account-A',
    tokenId: 'install-A',
    token: 'secret-A',
    scopes: [],
    pairedAt: 0,
  };
  const b = { ...a, accountId: 'account-B', tokenId: 'install-B', token: 'secret-B' };
  const binding = { expectedAccountId: a.accountId, expectedTokenId: a.tokenId };
  it('rejects page B against extension A with the same numeric collection id, including stop and legacy pairing', async () => {
    const f = fixture();
    f.deps.pairing = async () => a;
    const { createWebSyncControls } = await import('../src/sw/web-sync');
    const controls = createWebSyncControls(() => f.deps.pairing!(), f.service);
    const wrong = { expectedAccountId: b.accountId, expectedTokenId: a.tokenId };
    expect(await controls.start({ platform: 'instagram', collectionId: 1 }, wrong)).toEqual({
      ok: false,
      code: 'account_mismatch',
    });
    expect(await controls.stop('instagram', wrong)).toEqual({
      ok: false,
      code: 'account_mismatch',
    });
    f.deps.pairing = async () => ({ ...a, accountId: undefined });
    expect(await controls.start({ platform: 'instagram', collectionId: 1 }, binding)).toEqual({
      ok: false,
      code: 'account_mismatch',
    });
    expect((await controls.connection()).paired).toBe(false);
    expect(f.readSources).not.toHaveBeenCalled();
    expect(f.sync.start).not.toHaveBeenCalled();
    expect(f.sync.stopPlatform).not.toHaveBeenCalled();
  });
  it('rejects re-pair after ping and between worker credential capture and planner start/stop', async () => {
    const f = fixture();
    let current = a;
    f.deps.pairing = async () => current;
    const { createWebSyncControls } = await import('../src/sw/web-sync');
    const controls = createWebSyncControls(() => f.deps.pairing!(), f.service);
    const ping = await controls.connection();
    expect(ping).toMatchObject({ accountId: a.accountId, tokenId: a.tokenId });
    expect(JSON.stringify(ping)).not.toContain(a.token);
    current = b;
    expect(await controls.start({ platform: 'instagram', collectionId: 1 }, binding)).toEqual({
      ok: false,
      code: 'account_mismatch',
    });
    // Freeze A in C9, then re-pair in the planner's next credentials await.
    f.deps.pairing = vi.fn().mockResolvedValueOnce(a).mockResolvedValue(b);
    expect(await controls.start({ platform: 'instagram', collectionId: 1 }, binding)).toEqual({
      ok: false,
      code: 'account_mismatch',
    });
    f.deps.pairing = vi.fn().mockResolvedValueOnce(a).mockResolvedValue(b);
    expect(await controls.stop('instagram', binding)).toEqual({
      ok: false,
      code: 'account_mismatch',
    });
    expect(f.readSources).not.toHaveBeenCalled();
    expect(f.sync.start).not.toHaveBeenCalled();
    expect(f.sync.stopPlatform).not.toHaveBeenCalled();
  });
  it('keeps pre-C4 login errors in the account-bound snapshot and drops mixed-account snapshots', async () => {
    const f = fixture();
    let current = a;
    f.deps.pairing = async () => current;
    const { createWebSyncControls } = await import('../src/sw/web-sync');
    const controls = createWebSyncControls(() => f.deps.pairing!(), f.service);
    vi.mocked(f.browser.instagramUsername).mockResolvedValue({ username: null, login: true });
    expect(await controls.start({ platform: 'instagram' }, binding)).toEqual({ ok: true });
    await finished(f.service);
    expect(f.sync.start).not.toHaveBeenCalled();
    const ping = await controls.connection();
    expect(ping.planner).toMatchObject([
      { platform: 'instagram', status: 'error', code: 'login_required', total: 3 },
    ]);
    expect(JSON.stringify(ping)).not.toContain(a.token);
    vi.spyOn(f.service, 'snapshot').mockImplementationOnce(async () => {
      current = b;
      return { jobs: [], schedule: { ...DEFAULT_SCHEDULE } };
    });
    expect(await controls.connection()).toMatchObject({
      paired: false,
      accountId: null,
      tokenId: null,
      planner: [],
    });
  });
});
