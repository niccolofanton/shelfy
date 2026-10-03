import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiClient } from '../src/sw/api';
import { DEFAULT_CONFIG } from '../src/sw/contracts';
import { createUlid } from '../src/shared/ulid';
import { EXTENSION_VERSION } from '../src/shared/version';
import { RefreshCollector } from '../src/content/tasks/collector';
import { parseTasks, type ExtensionTask } from '../src/sw/tasks/contracts';
import { TasksService, TASK_SESSION_KEY } from '../src/sw/tasks/service';
import { allowedCdn, readImage, uploadImage, MAX_IMAGE_BYTES } from '../src/sw/tasks/upload';
import type { TaskInstagram } from '../src/sw/tasks/instagram';
import { INSTAGRAM_BACKLOG_DAY_KEY } from '../src/sw/planner/service';
import { harness, igItem, memoryStorage, ORIGIN, T0 } from './helpers';

const PNG = Uint8Array.from(
  Buffer.from(
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aAAEAAAAASUVORK5CYII=',
    'base64',
  ),
);
const CDN = 'https://scontent-synth1-1.cdninstagram.com/v/poster.jpg';
function task(n: number, kind: ExtensionTask['kind'] = 'refresh_media'): ExtensionTask {
  const nativeId = String(igItem(n).id).split('_')[0];
  return {
    id: `${kind}.ig_${nativeId}.post`,
    kind,
    nativeId,
    platform: 'instagram',
    postKey: `ig_${nativeId}`,
    postUrl: 'https://www.instagram.com/p/fixture/',
    shortcode: null,
    position: null,
    url: kind === 'upload_media' ? CDN : null,
    expiresAt: null,
    leaseId: `lease-${n}`,
    leaseUntil: T0 + 300_000,
  };
}
async function fixture() {
  const h = harness();
  await h.pairNow();
  const session = memoryStorage();
  const config = structuredClone(DEFAULT_CONFIG);
  const now = () => h.clock.now;
  const requestLog: Array<{ url: string; init: RequestInit }> = [];
  const fetchApi = h.api.fetchHandler(ORIGIN);
  const fetcher = vi.fn(async (url: string, init: RequestInit) => {
    requestLog.push({ url, init });
    if (url === CDN) return new Response(PNG, { headers: { 'content-type': 'image/png' } });
    return fetchApi(url, init);
  });
  const credentials = async () => ({
    token: (await h.store.pairing())?.token ?? null,
    access: null,
  });
  const refresh = vi.fn<Parameters<TaskInstagram['refresh']>, ReturnType<TaskInstagram['refresh']>>(
    async (_tab, item) => ({
      outcome: 'refreshed',
      items: [igItem(Number(item.nativeId.slice(-8)))],
    }),
  );
  const find = vi.fn<Parameters<TaskInstagram['find']>, ReturnType<TaskInstagram['find']>>(
    async (excluded) => (excluded?.includes(9) ? null : { id: 9, docId: 'doc' }),
  );
  const planner = {
    syncing: vi.fn(async () => ({ instagram: false, twitter: false, pinterest: false })),
    requestInstagramBacklogSync: vi.fn(async () => ({ ok: false as const, code: 'daily_limit' })),
  };
  const deps = {
    store: h.store,
    session,
    config: async () => config,
    apiFor: () =>
      new ApiClient({
        origin: ORIGIN,
        version: EXTENSION_VERSION,
        fetch: fetcher,
        credentials,
        now,
      }),
    uploadDeps: () => ({ origin: ORIGIN, version: EXTENSION_VERSION, fetch: fetcher, credentials }),
    instagram: { find, refresh },
    planner,
    now,
    sleep: async (ms: number) => {
      h.clock.now += ms;
    },
    id: createUlid(now),
    changed: vi.fn(),
    failure: vi.fn(async () => {}),
    client: { ext: EXTENSION_VERSION, parser: 'test-parser' },
  };
  const service = new TasksService(deps);
  const put = (item: ExtensionTask) => h.api.tasks.set(item.id, item);
  return { ...h, service, deps, put, config, session, requestLog, fetcher, refresh, find, planner };
}
afterEach(() => {
  vi.useRealTimers();
});

describe('leased media task worker', () => {
  it('uploads only an image, hashes tus metadata and echoes the polled lease', async () => {
    const f = await fixture();
    const item = task(1, 'upload_media');
    f.put(item);
    await f.service.poll();
    const upload = [...f.api.uploads.values()][0];
    expect(upload.bytes).toEqual(PNG);
    expect(upload.metadata).toMatchObject({ purpose: 'archive-object', ext: 'png' });
    expect(upload.metadata.sha256).toMatch(/^[a-f0-9]{64}$/);
    expect(f.api.taskCompletions).toEqual([
      {
        id: item.id,
        status: 204,
        body: { leaseId: item.leaseId, outcome: 'uploaded', uploadId: 'upload-1', errorCode: null },
      },
    ]);
    const cdn = f.requestLog.find((request) => request.url === CDN)!;
    expect(cdn.init).toMatchObject({ credentials: 'omit', redirect: 'manual' });
    expect(cdn.init.headers).toBeUndefined();
  });
  it('repolls after stale generation 409, never replaying the obsolete completion', async () => {
    const f = await fixture();
    const item = task(1);
    f.put(item);
    f.api.staleLeaseOnce = true;
    await f.service.poll();
    expect(f.api.taskCompletions.map((completion) => completion.status)).toEqual([409, 204]);
    expect(
      f.api.taskCompletions.map((completion) => (completion.body as { leaseId: string }).leaseId),
    ).toEqual(['lease-1', 'lease-1-new']);
    const paths = f.api.log.map((entry) => entry.path);
    expect(paths.filter((path) => path.startsWith('/api/v1/ingest/tasks?'))).toHaveLength(3);
    expect(f.refresh).toHaveBeenCalledTimes(2);
  });
  it('ingests refresh/hydration before completing, without marking a saved-feed full walk', async () => {
    const f = await fixture();
    f.put(task(1));
    f.put(task(2, 'hydrate_link'));
    await f.service.poll();
    expect(f.api.ingests.map((ingest) => ingest.source)).toEqual(['refresh', 'refresh']);
    expect([...f.api.runs.values()].every((run) => run.trigger === 'refresh')).toBe(true);
    expect(
      f.api.patches.every((patch) => (patch.body as { stopReason: null }).stopReason === null),
    ).toBe(true);
    expect(
      f.api.taskCompletions.map((entry) => (entry.body as { outcome: string }).outcome),
    ).toEqual(['refreshed', 'refreshed']);
    expect(f.clock.now).toBe(T0 + 700);
  });
  it('paces MAIN reads 700ms apart with fake timers', async () => {
    const f = await fixture();
    f.put(task(1));
    f.put(task(2));
    vi.useFakeTimers();
    vi.setSystemTime(T0);
    f.deps.now = () => Date.now();
    f.deps.sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    const pending = f.service.poll();
    await vi.advanceTimersByTimeAsync(0);
    expect(f.refresh).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(699);
    expect(f.refresh).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    await pending;
    expect(f.refresh).toHaveBeenCalledTimes(2);
  });
  it.each(['rate_limited', 'checkpoint', 'login_required'] as const)(
    'stops at first %s, including after a worker restart',
    async (code) => {
      const f = await fixture();
      f.put(task(1));
      f.put(task(2));
      f.refresh.mockResolvedValue({ outcome: 'blocked', code });
      await f.service.poll();
      expect(f.refresh).toHaveBeenCalledTimes(1);
      expect(f.api.taskCompletions).toHaveLength(0);
      await new TasksService(f.deps).poll();
      expect(f.refresh).toHaveBeenCalledTimes(1);
    },
  );
  it('does not read IG without an existing tab, while waiting counts stay visible', async () => {
    const f = await fixture();
    f.put(task(1));
    f.find.mockResolvedValue(null);
    await f.service.poll();
    expect(f.refresh).not.toHaveBeenCalled();
    expect(await f.service.snapshot()).toMatchObject({
      waiting: { instagram: 1 },
      code: 'no_instagram_tab',
    });
    expect(f.api.taskCompletions).toHaveLength(0);
  });
  it('honors the cap across navigation and worker restart; closing the tab starts a new session', async () => {
    const f = await fixture();
    await f.session.set({
      [TASK_SESSION_KEY]: {
        accountKey: 'tok-1',
        sessions: { 9: { count: 199, lastAt: 0, blocked: null } },
      },
    });
    f.put(task(1));
    f.put(task(2));
    await f.service.poll();
    expect(f.refresh).toHaveBeenCalledTimes(1);
    const restarted = new TasksService(f.deps);
    await restarted.poll();
    expect(f.refresh).toHaveBeenCalledTimes(1);
    await restarted.tabRemoved(9);
    await restarted.poll();
    expect(f.refresh).toHaveBeenCalledTimes(2);
  });
  it('starts a large backlog full sync on the existing tab and defers per-post reads', async () => {
    const f = await fixture();
    f.put(task(1));
    f.api.waitingOverride = { instagram: 200, twitter: 0, pinterest: 0 };
    f.planner.requestInstagramBacklogSync.mockResolvedValue({ ok: true } as never);
    await f.service.poll();
    expect(f.planner.requestInstagramBacklogSync).toHaveBeenCalledWith({ backlog: 200, tabId: 9 });
    expect(f.refresh).not.toHaveBeenCalled();
  });
  it('completes gone posts, refuses malformed task leases and rejects unrelated refresh items', async () => {
    expect(
      parseTasks({
        tasks: [{ ...task(1), leaseId: '' }],
        waiting: { instagram: 1, twitter: 0, pinterest: 0 },
      })?.tasks,
    ).toEqual([]);
    const f = await fixture();
    f.put(task(1));
    f.put(task(2));
    f.refresh
      .mockResolvedValueOnce({ outcome: 'gone' })
      .mockResolvedValueOnce({ outcome: 'refreshed', items: [igItem(99)] });
    await f.service.poll();
    expect(
      f.api.taskCompletions.map((entry) => (entry.body as { outcome: string }).outcome),
    ).toEqual(['gone', 'failed']);
    expect(f.api.ingests).toHaveLength(0);
  });
  it('runs no requests while unpaired, globally killed, refresh-disabled or outdated', async () => {
    const f = await fixture();
    f.put(task(1));
    await f.store.setPairing(null);
    await f.service.poll();
    expect(f.fetcher).not.toHaveBeenCalled();
    await f.pairNow();
    for (const modes of Object.values(f.config.platforms))
      Object.assign(modes, { passive: false, replay: false, scroll: false });
    await f.service.poll();
    expect(f.fetcher).not.toHaveBeenCalled();
    f.config.platforms.instagram.passive = true;
    f.config.refreshPerSession = 0;
    await f.service.poll();
    expect(f.refresh).not.toHaveBeenCalled();
    await f.store.patchStatus({ outdated: true });
    f.fetcher.mockClear();
    await f.service.poll();
    expect(f.fetcher).not.toHaveBeenCalled();
  });
  it('cancels an in-flight refresh across account changes and resets the backlog watermark on unpair', async () => {
    const f = await fixture();
    f.put(task(1));
    let release!: () => void;
    f.refresh.mockImplementationOnce(async () => {
      await new Promise<void>((resolve) => {
        release = resolve;
      });
      return { outcome: 'refreshed', items: [igItem(1)] };
    });
    const pending = f.service.poll();
    for (let i = 0; i < 100 && !release; i++) await Promise.resolve();
    await f.storage.set({ [INSTAGRAM_BACKLOG_DAY_KEY]: { accountKey: 'tok-1', day: 'today' } });
    await f.store.setPairing(null);
    await f.service.reset();
    release();
    await pending;
    expect(
      (await f.storage.get(INSTAGRAM_BACKLOG_DAY_KEY))[INSTAGRAM_BACKLOG_DAY_KEY],
    ).toBeUndefined();
    expect(f.api.ingests).toHaveLength(0);
    expect(f.api.taskCompletions).toHaveLength(0);
    expect((await f.service.snapshot()).waiting.instagram).toBe(0);
  });
});

describe('image limits and isolated refresh capture', () => {
  it('never fetches video URLs or hosts outside the platform CDN allowlist', async () => {
    const fetcher = vi.fn();
    for (const url of [
      'https://evil.test/photo.jpg',
      'https://scontent.cdninstagram.com/video.mp4',
      'https://cdninstagram.com.evil.test/a.jpg',
    ]) {
      const item = { ...task(1, 'upload_media'), url };
      expect(allowedCdn(item)).toBeNull();
      await expect(
        uploadImage(item, {
          origin: ORIGIN,
          version: 'test',
          fetch: fetcher,
          credentials: async () => ({ token: 'token', access: null }),
          guard: async () => {},
        }),
      ).rejects.toThrow('cdn_not_allowed');
    }
    expect(fetcher).not.toHaveBeenCalled();
  });
  it('rejects image type spoofing and enforces 15MiB even without content-length', async () => {
    await expect(
      readImage(new Response('not a PNG', { headers: { 'content-type': 'image/png' } })),
    ).rejects.toThrow('image_type');
    await expect(
      readImage(
        new Response(PNG, {
          headers: { 'content-type': 'image/png', 'content-length': String(MAX_IMAGE_BYTES + 1) },
        }),
      ),
    ).rejects.toThrow('image_too_large');
    const cancelled = vi.fn();
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new Uint8Array(MAX_IMAGE_BYTES + 1));
      },
      cancel: cancelled,
    });
    await expect(
      readImage(new Response(stream, { headers: { 'content-type': 'image/png' } })),
    ).rejects.toThrow('image_too_large');
    expect(cancelled).toHaveBeenCalled();
  });
  it('collects only the requested post inside an authorized refresh, without passive forwarding', async () => {
    const collector = new RefreshCollector();
    const item = task(1);
    expect(
      collector.consume(
        { platform: 'instagram', items: [igItem(1)], hasNextPage: null },
        'refresh',
      ),
    ).toBe(true);
    expect(collector.prepare('request', item.nativeId)).toBe(true);
    expect(collector.prepare('other', item.nativeId)).toBe(false);
    expect(
      collector.consume(
        { platform: 'instagram', items: [igItem(1)], hasNextPage: null },
        'passive',
      ),
    ).toBe(false);
    collector.consume(
      { platform: 'instagram', items: [igItem(99), igItem(1)], hasNextPage: null },
      'refresh',
    );
    expect(await collector.take('request', false)).toEqual([igItem(1)]);
    expect(await collector.take('request', false)).toEqual([]);
  });
});
