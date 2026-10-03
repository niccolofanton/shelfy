import React from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import { ShelfyProvider } from '@ui/api/ShelfyProvider';
import type { ShelfyClient } from '@ui/api/ShelfyClient';
import type { SyncApi, SyncConnection, SyncProgress, SyncRun } from '@ui/api/sync';
import { useWebSync, WebSyncProvider } from '@ui/hooks/useWebSync';
import { createSyncExtension } from '../src/extension/bridge';
import { createSyncApi } from '../src/api/sync';
import { createHttp } from '../src/api/http';
import { createEventStream, type EventSourceLike } from '../src/api/events';
import { EXTENSION_ID } from '../../extension/src/id';

const run = (id = 'run-ig', overrides: Partial<SyncRun> = {}): SyncRun => ({
  id,
  platform: 'instagram',
  trigger: 'web',
  listing: { kind: 'ig_saved', externalId: null, name: null },
  state: 'running',
  scanned: 5,
  inserted: 2,
  known: 3,
  updated: 0,
  pages: 1,
  startedAt: 1000,
  finishedAt: null,
  errorCode: null,
  collectionId: null,
  ...overrides,
});
const event = (item: SyncRun): SyncProgress => ({ ...item, runId: item.id });
const ready: SyncConnection = {
  extension: { state: 'ready', paired: true, outdated: false, version: '1' },
  syncing: {},
};
class Source implements EventSourceLike {
  static instances: Source[] = [];
  onopen: ((event: Event) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  listeners = new Map<string, (event: MessageEvent) => void>();
  closed = false;
  constructor(readonly url: string) {
    Source.instances.push(this);
  }
  addEventListener(name: string, listener: (event: MessageEvent) => void) {
    this.listeners.set(name, listener);
  }
  close() {
    this.closed = true;
  }
  emit(name: string, data: unknown, id = '') {
    this.listeners.get(name)?.(
      new MessageEvent(name, { data: JSON.stringify(data), lastEventId: id }),
    );
  }
}
afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
  Source.instances = [];
});

function harness(items: SyncRun[] = [], customList?: SyncApi['list']) {
  const progress = new Set<(event: SyncProgress) => void>();
  const refresh = new Set<() => void>();
  const stopped = vi.fn();
  const api: SyncApi = {
    connection: vi.fn().mockResolvedValue(ready),
    start: vi.fn().mockResolvedValue({ ok: true }),
    stop: vi.fn().mockResolvedValue({ ok: true }),
    list: vi.fn(async (query = {}) => ({
      items: items.filter(
        (item) =>
          (!query.platform || item.platform === query.platform) &&
          (!query.state || item.state === query.state),
      ),
      nextCursor: null,
    })),
    onProgress: (listener) => {
      progress.add(listener);
      return () => {
        progress.delete(listener);
        stopped();
      };
    },
    onRefresh: (listener) => {
      refresh.add(listener);
      return () => {
        refresh.delete(listener);
        stopped();
      };
    },
  };
  if (customList) api.list = customList;
  let client = {
    capabilities: { sync: true },
    sync: api,
    listCollections: vi
      .fn()
      .mockResolvedValue([{ platform: 'instagram', externalId: '123', id: 7 }]),
  } as unknown as ShelfyClient;
  const wrapper = ({ children }: { children: React.ReactNode }) => (
    <ShelfyProvider client={client}>
      <WebSyncProvider>{children}</WebSyncProvider>
    </ShelfyProvider>
  );
  const hook = renderHook(() => useWebSync()!, { wrapper });
  return {
    ...hook,
    api,
    items,
    progress,
    refresh,
    stopped,
    setClient: (next: ShelfyClient) => {
      client = next;
      hook.rerender();
    },
    emit: (item: SyncRun) =>
      act(() => {
        for (const listener of progress) listener(event(item));
      }),
  };
}

describe('sync seam and fixed-ID extension controls', () => {
  it('reads planner state and sends C9 collection start and platform stop', async () => {
    const sent: unknown[] = [];
    const extension = createSyncExtension({
      chrome: {
        runtime: {
          sendMessage(id, message, callback) {
            expect(id).toBe(EXTENSION_ID);
            sent.push(message);
            callback(
              sent.length === 1
                ? {
                    ok: true,
                    version: '1',
                    paired: true,
                    outdated: false,
                    syncing: { instagram: true, twitter: false },
                  }
                : { ok: true },
            );
          },
        },
      },
    });
    expect((await extension.connection()).syncing).toEqual({ instagram: true, twitter: false });
    expect(await extension.start({ platform: 'instagram', collectionId: 7 })).toEqual({ ok: true });
    expect(await extension.stop('instagram')).toEqual({ ok: true });
    expect(sent).toEqual([
      { type: 'shelfy.ping' },
      { type: 'shelfy.sync.start', target: { platform: 'instagram', collectionId: 7 } },
      { type: 'shelfy.sync.stop', platform: 'instagram' },
    ]);
  });
  it('returns unsupported/missing and bounds a silent extension timeout', async () => {
    expect((await createSyncExtension({ chrome: null }).connection()).extension.state).toBe(
      'unsupported',
    );
    expect((await createSyncExtension({ chrome: {} }).connection()).extension.state).toBe(
      'missing',
    );
    vi.useFakeTimers();
    const pending = createSyncExtension({ chrome: { runtime: { sendMessage() {} } } }).start({
      platform: 'twitter',
    });
    await vi.advanceTimersByTimeAsync(30_000);
    expect(await pending).toEqual({ ok: false, code: 'unreachable' });
  });
  it('lists account runs by platform/state and shares replay/reconnect/cleanup with SSE', async () => {
    vi.useFakeTimers();
    const fetch = vi.fn(
      async (_url: RequestInfo | URL, _init?: RequestInit) =>
        new Response(JSON.stringify({ items: [], nextCursor: null })),
    );
    const stream = createEventStream({ EventSource: Source, target: null, random: () => 1 });
    const api = createSyncApi(createHttp({ fetch }), stream);
    await api.list({ platform: 'instagram', state: 'running', limit: 999, cursor: 'a+b' });
    expect(String(fetch.mock.calls[0][0])).toBe(
      '/api/v1/sync-runs?limit=100&cursor=a%2Bb&state=running&platform=instagram',
    );
    const progress = vi.fn(),
      refresh = vi.fn();
    const offProgress = api.onProgress(progress),
      offRefresh = api.onRefresh(refresh);
    expect(Source.instances).toHaveLength(1);
    Source.instances[0].onopen?.(new Event('open'));
    Source.instances[0].emit('hello', {});
    Source.instances[0].emit('sync.progress', event(run()), 'sync-1');
    Source.instances[0].emit('sync.progress', { ...event(run()), platform: 'future' });
    expect(progress).toHaveBeenCalledTimes(1);
    Source.instances[0].onerror?.(new Event('error'));
    await vi.advanceTimersByTimeAsync(1000);
    expect(Source.instances[1].url).toContain('lastEventId=sync-1');
    Source.instances[1].emit('resync', {});
    expect(refresh).toHaveBeenCalledTimes(2);
    offProgress();
    offRefresh();
    expect(stream.state).toBe('closed');
    expect(Source.instances[1].closed).toBe(true);
  });
});

describe('web sync account lifecycle and parallel plans', () => {
  it('keeps progress received during the initial REST request and aborts its account requests on cleanup', async () => {
    const requests: {
      query: Parameters<SyncApi['list']>[0];
      resolve: (value: { items: SyncRun[]; nextCursor: null }) => void;
    }[] = [];
    const list: SyncApi['list'] = vi.fn(
      (query) => new Promise((resolve) => requests.push({ query, resolve })),
    );
    const h = harness([], list);
    await waitFor(() => expect(requests).toHaveLength(4));
    h.emit(run('run-ig', { scanned: 100 }));
    await act(async () => {
      for (const request of requests)
        request.resolve({
          items:
            request.query?.platform === 'instagram' || request.query?.state === 'running'
              ? [run()]
              : [],
          nextCursor: null,
        });
    });
    expect(h.result.current.latest.instagram?.scanned).toBe(100);
    expect(h.result.current.latest.instagram?.startedAt).toBe(1000);
    h.unmount();
    expect(requests.every((request) => request.query?.signal?.aborted)).toBe(true);
  });

  it('finds the last source sync behind media refresh history without treating covers as a user sync', async () => {
    const list: SyncApi['list'] = vi.fn(async (query = {}) => {
      if (query.platform !== 'instagram') return { items: [], nextCursor: null };
      return query.cursor
        ? { items: [run('source-sync', { state: 'done' })], nextCursor: null }
        : {
            items: [run('cover-refresh', { trigger: 'refresh', state: 'done' })],
            nextCursor: 'older',
          };
    });
    const h = harness([], list);
    await waitFor(() => expect(h.result.current.latest.instagram?.id).toBe('source-sync'));
    expect(h.result.current.active.instagram).toBe(false);
    expect(list).toHaveBeenCalledWith(
      expect.objectContaining({ platform: 'instagram', cursor: 'older' }),
    );
  });

  it('keeps IG and X progress independent and hydrates terminal login errors', async () => {
    const h = harness([
      run(),
      run('run-x', {
        platform: 'twitter',
        listing: { kind: 'x_bookmarks', externalId: null, name: null },
      }),
    ]);
    await waitFor(() => expect(h.result.current.runs).toHaveLength(2));
    h.emit(run('run-ig', { scanned: 100, inserted: 80, known: 20 }));
    h.emit(run('run-x', { platform: 'twitter', scanned: 50 }));
    expect(h.result.current.runs.find((r) => r.id === 'run-ig')?.scanned).toBe(100);
    expect(h.result.current.runs.find((r) => r.id === 'run-x')?.scanned).toBe(50);
    h.items[0] = run('run-ig', {
      state: 'failed',
      errorCode: 'login_required',
      finishedAt: 9000,
      scanned: 100,
    });
    h.emit(h.items[0]);
    await waitFor(() =>
      expect(h.result.current.latest.instagram?.errorCode).toBe('login_required'),
    );
    expect(h.result.current.active.twitter).toBe(true);
    h.unmount();
    expect(h.stopped).toHaveBeenCalledTimes(2);
  });
  it('does not resurrect a running item lost during reconnect, or regress newer SSE counters', async () => {
    const h = harness([run()]);
    await waitFor(() => expect(h.result.current.active.instagram).toBe(true));
    h.emit(run('run-ig', { scanned: 100 }));
    h.emit(run('run-ig', { scanned: 1 }));
    expect(h.result.current.latest.instagram?.scanned).toBe(100);
    h.items[0] = run('run-ig', { state: 'done', finishedAt: 9000, scanned: 101 });
    act(() => {
      for (const refresh of h.refresh) refresh();
    });
    await waitFor(() => expect(h.result.current.active.instagram).toBe(false));
    expect(h.result.current.latest.instagram?.state).toBe('done');
  });
  it('tracks both IG steps and prevents double-start before the extension replies', async () => {
    const h = harness();
    await waitFor(() => expect(h.api.connection).toHaveBeenCalled());
    let resolve!: (value: { ok: true }) => void;
    vi.mocked(h.api.start).mockImplementation(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    let first!: Promise<void>;
    await act(async () => {
      first = h.result.current.start({ platform: 'instagram' });
      await Promise.resolve();
      await Promise.resolve();
    });
    await waitFor(() => expect(h.api.start).toHaveBeenCalledTimes(1));
    await act(() => h.result.current.start({ platform: 'instagram' }));
    expect(h.api.start).toHaveBeenCalledTimes(1);
    await act(async () => {
      resolve({ ok: true });
      await first;
    });
    h.emit(run('first'));
    h.emit(run('first', { state: 'done' }));
    h.emit(
      run('folder', { listing: { kind: 'ig_collection', externalId: '123', name: 'Recipes' } }),
    );
    expect(h.result.current.step(h.result.current.runs.find((r) => r.id === 'folder')!)).toEqual({
      index: 2,
      total: 2,
    });
    await act(() => h.result.current.stop('instagram'));
    expect(h.api.stop).toHaveBeenCalledWith('instagram');
  });
  it('does not apply a late extension response or old callback after an account switch', async () => {
    const h = harness();
    await waitFor(() => expect(h.api.connection).toHaveBeenCalled());
    let resolve!: (value: { ok: false; code: string }) => void;
    vi.mocked(h.api.start).mockImplementation(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    const oldProgress = [...h.progress][0];
    let pending!: Promise<void>;
    await act(async () => {
      pending = h.result.current.start({ platform: 'instagram' }, 1);
      await Promise.resolve();
      await Promise.resolve();
    });
    await waitFor(() => expect(h.api.start).toHaveBeenCalledTimes(1));
    act(() => h.setClient({ capabilities: { sync: false } } as unknown as ShelfyClient));
    await act(async () => {
      resolve({ ok: false, code: 'old-account' });
      await pending;
      oldProgress(event(run('A')));
    });
    expect(h.result.current.runs).toEqual([]);
    expect(h.result.current.error).toBeNull();
    expect(h.result.current.help).toBeNull();
    expect(h.result.current.enabled).toBe(false);
    expect(h.stopped).toHaveBeenCalledTimes(2);
  });
  it('shows desktop-Chrome help on phones before sending any sync command', async () => {
    vi.spyOn(navigator, 'userAgent', 'get').mockReturnValue('iPhone');
    const h = harness();
    await act(() => h.result.current.start({ platform: 'instagram' }));
    expect(h.result.current.help).toBe('mobile');
    expect(h.api.start).not.toHaveBeenCalled();
  });
});
