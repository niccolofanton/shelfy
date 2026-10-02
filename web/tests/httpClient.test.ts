import { describe, it, expect, vi } from 'vitest';
import type { EventStream } from '../src/api/events';
import type { Http } from '../src/api/http';
import { ApiError } from '../src/api/http';
import { WEB_CAPABILITIES, createHttpClient, openExternalUrl } from '../src/api/httpClient';
import { apiPost } from './fixtures';

// An Http whose GETs answer from `routes` (path → answers, in order).
function fakeHttp(routes: Record<string, unknown[]>) {
  const calls: { path: string; query: Record<string, string[]> }[] = [];
  const get = vi.fn(async (path: string, query?: URLSearchParams) => {
    const q: Record<string, string[]> = {};
    query?.forEach((value, key) => (q[key] = [...(q[key] ?? []), value]));
    calls.push({ path, query: q });
    const answers = routes[path];
    if (!answers?.length) throw new ApiError(404, 'not_found');
    const answer = answers.shift();
    if (answer instanceof Error) throw answer;
    return answer;
  });
  const http: Http = { get: get as Http['get'], send: vi.fn(), onUnauthorized: () => () => {} };
  return { http, calls };
}

const posts = (prefix: string, n: number) =>
  Array.from({ length: n }, (_, i) => apiPost({ key: `${prefix}_${i}` }));

describe('httpClient.listPosts', () => {
  it('asks for one page and the total', async () => {
    const { http, calls } = fakeHttp({
      '/api/v1/posts': [{ items: posts('ig', 50), nextCursor: 'c1', total: 90 }],
    });
    const page = await createHttpClient(http).listPosts(
      { platform: 'instagram', search: 'lamp' },
      { limit: 50 },
    );
    expect(calls).toEqual([
      {
        path: '/api/v1/posts',
        query: {
          platform: ['instagram'],
          q: ['lamp'],
          limit: ['50'],
          includeTotal: ['true'],
        },
      },
    ]);
    expect(page.total).toBe(90);
    expect(page.nextCursor).toBe('c1');
    expect(page.posts.map((p) => p.id).slice(0, 2)).toEqual(['ig_0', 'ig_1']);
  });

  it('stops at the last page even when the window is not full', async () => {
    const { http, calls } = fakeHttp({
      '/api/v1/posts': [{ items: posts('ig', 2), nextCursor: null, total: 2 }],
    });
    const page = await createHttpClient(http).listPosts({}, { limit: 50 });
    expect(calls).toHaveLength(1);
    expect(page).toMatchObject({ total: 2, nextCursor: null });
  });

  it('fills a window larger than a page with several requests', async () => {
    const { http, calls } = fakeHttp({
      '/api/v1/posts': [
        { items: posts('a', 200), nextCursor: 'c1', total: 1000 },
        { items: posts('b', 200), nextCursor: 'c2' },
        { items: posts('c', 50), nextCursor: 'c3' },
      ],
    });
    const page = await createHttpClient(http).listPosts({}, { limit: 450 });
    expect(calls.map((c) => [c.query.limit, c.query.cursor, c.query.includeTotal])).toEqual([
      [['200'], undefined, ['true']],
      [['200'], ['c1'], undefined],
      [['50'], ['c2'], undefined],
    ]);
    expect(page.posts).toHaveLength(450);
    expect(page.total).toBe(1000);
    expect(page.nextCursor).toBe('c3');
  });

  it('continues from a cursor without asking for the total again', async () => {
    const { http, calls } = fakeHttp({
      '/api/v1/posts': [{ items: posts('b', 3), nextCursor: null }],
    });
    const page = await createHttpClient(http).listPosts({}, { limit: 250, cursor: 'c9' });
    expect(calls[0].query).toEqual({ limit: ['200'], cursor: ['c9'] });
    expect(page).toMatchObject({ total: undefined, nextCursor: null });
    expect(page.posts).toHaveLength(3);
  });
});

describe('httpClient — other reads', () => {
  it('reads posts by id and skips the ones that are gone', async () => {
    const { http, calls } = fakeHttp({
      '/api/v1/posts/x_1': [apiPost({ key: 'x_1', platform: 'twitter' })],
    });
    const found = await createHttpClient(http).getPostsByIds(['x_1', 'x_2']);
    expect(found.map((p) => p.id)).toEqual(['x_1']);
    expect(calls.map((c) => c.path)).toEqual(['/api/v1/posts/x_1', '/api/v1/posts/x_2']);
  });

  it('lets other errors through', async () => {
    const { http } = fakeHttp({ '/api/v1/posts/x_1': [new ApiError(500, 'internal')] });
    await expect(createHttpClient(http).getPostsByIds(['x_1'])).rejects.toBeInstanceOf(ApiError);
  });

  it('maps stats and folders', async () => {
    const { http } = fakeHttp({
      '/api/v1/stats': [
        {
          total: 3,
          byPlatform: { instagram: 1, twitter: 1, pinterest: 0, web: 1, manual: 0 },
          byMediaType: {},
          stored: 1,
          storedByKind: { covers: 1, images: 0, videos: 0 },
          trashed: 0,
        },
      ],
      '/api/v1/collections': [
        {
          items: [
            {
              id: 2,
              name: 'Inspiration',
              color: '#ffaa00',
              count: 1,
              createdAt: 1_000,
              externalId: null,
              platform: null,
              position: null,
              sourceName: null,
            },
          ],
        },
      ],
    });
    const client = createHttpClient(http);
    expect(await client.getStats()).toMatchObject({ total: 3, downloaded: 1 });
    expect(await client.listCollections()).toEqual([
      {
        id: 2,
        name: 'Inspiration',
        color: '#ffaa00',
        count: 1,
        createdAt: 1,
        externalId: null,
        platform: null,
        igName: null,
      },
    ]);
  });
});

describe('httpClient — capabilities, links and events', () => {
  it('can browse and read, nothing else yet', () => {
    const { http } = fakeHttp({});
    const client = createHttpClient(http);
    expect(client.capabilities).toBe(WEB_CAPABILITIES);
    expect(Object.values(client.capabilities).every((on) => on === false)).toBe(true);
    const off = client.on('posts.changed', () => {});
    expect(typeof off).toBe('function');
  });

  it('opens only http(s) links, in a new tab without an opener', () => {
    const open = vi.fn() as unknown as typeof window.open;
    openExternalUrl('https://www.instagram.com/p/C0ffee/', open);
    openExternalUrl('javascript:alert(1)', open);
    openExternalUrl('data:text/html,hi', open);
    openExternalUrl('file:///etc/passwd', open);
    openExternalUrl('not a url', open);
    expect(vi.mocked(open).mock.calls).toEqual([
      ['https://www.instagram.com/p/C0ffee/', '_blank', 'noopener,noreferrer'],
    ]);
  });
});

describe('httpClient — live events and error reports', () => {
  // An EventStream the test drives by event name.
  function fakeStream() {
    const listeners = new Map<string, ((data: unknown) => void)[]>();
    const events: EventStream = {
      state: 'open',
      lastEventId: null,
      on(name, listener) {
        const entry = listener as (data: unknown) => void;
        listeners.set(name, [...(listeners.get(name) ?? []), entry]);
        return () =>
          listeners.set(
            name,
            (listeners.get(name) ?? []).filter((l) => l !== entry),
          );
      },
    };
    const push = (name: string, data: unknown) =>
      (listeners.get(name) ?? []).forEach((listener) => listener(data));
    return { events, push, count: (name: string) => (listeners.get(name) ?? []).length };
  }

  it('hands the stream events to the UI in the seam terms', () => {
    const { http } = fakeHttp({});
    const { events, push, count } = fakeStream();
    const client = createHttpClient(http, { events });
    const changed = vi.fn();
    const stats = vi.fn();
    const resync = vi.fn();
    const offChanged = client.on('posts.changed', changed);
    client.on('stats.changed', stats);
    client.on('resync', resync);

    push('posts.changed', { keys: ['ig_1'], reason: 'edit' });
    push('posts.changed', { keys: null, reason: 'ingest' });
    push('stats.changed', {});
    push('resync', { reason: 'expired' });

    expect(changed.mock.calls).toEqual([
      [{ type: 'posts.changed', keys: ['ig_1'], reason: 'edit' }],
      [{ type: 'posts.changed', keys: null, reason: 'ingest' }],
    ]);
    expect(stats.mock.calls).toEqual([[{ type: 'stats.changed' }]]);
    expect(resync.mock.calls).toEqual([[{ type: 'resync' }]]);
    offChanged();
    expect(count('posts.changed')).toBe(0);
  });

  it('subscribes to nothing for events the server does not send yet', () => {
    const { http } = fakeHttp({});
    const { events, count } = fakeStream();
    const client = createHttpClient(http, { events });
    expect(typeof client.on('post.stored', vi.fn())).toBe('function');
    expect(typeof client.on('post.analyzed', vi.fn())).toBe('function');
    expect(['job.updated', 'posts.changed', 'notification'].map(count)).toEqual([0, 0, 0]);
  });

  it('checks the session when the stream cannot connect', async () => {
    const refuse: (() => void)[] = [];
    class Refused {
      onopen: ((event: Event) => void) | null = null;
      onerror: ((event: Event) => void) | null = null;
      constructor() {
        refuse.push(() => this.onerror?.(new Event('error')));
      }
      addEventListener(): void {}
      close(): void {}
    }
    vi.stubGlobal('EventSource', Refused);
    try {
      const { http, calls } = fakeHttp({});
      const off = createHttpClient(http).on('posts.changed', vi.fn());
      refuse[0]();
      off();
      await Promise.resolve();
      expect(calls.map((c) => c.path)).toEqual(['/api/v1/me']);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('passes error reports on, and takes its capabilities from the caller', () => {
    const { http } = fakeHttp({});
    const reportError = vi.fn();
    const capabilities = { ...WEB_CAPABILITIES, libraryEdit: true };
    const client = createHttpClient(http, { reportError, capabilities });
    const report = { view: 'gallery', error: new Error('boom') };
    client.reportError(report);
    expect(reportError).toHaveBeenCalledWith(report);
    expect(client.capabilities.libraryEdit).toBe(true);
    expect(() => createHttpClient(http).reportError(report)).not.toThrow();
  });
});
