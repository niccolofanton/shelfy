import { describe, it, expect, vi } from 'vitest';
import type { EventStream } from '../src/api/events';
import type { Http } from '../src/api/http';
import { ApiError } from '../src/api/http';
import { WEB_CAPABILITIES, createHttpClient, openExternalUrl } from '../src/api/httpClient';
import { apiPost } from './fixtures';

// An Http whose GETs and sends answer from `routes` (path → answers, in
// order); a `send` resolves to a Response-ish whose `.json()` gives the answer.
function fakeHttp(routes: Record<string, unknown[]>) {
  const calls: { path: string; query: Record<string, string[]>; body?: unknown }[] = [];
  const nextAnswer = (path: string): unknown => {
    const answers = routes[path];
    if (!answers?.length) throw new ApiError(404, 'not_found');
    const answer = answers.shift();
    if (answer instanceof Error) throw answer;
    return answer;
  };
  const get = vi.fn(async (path: string, query?: URLSearchParams) => {
    const q: Record<string, string[]> = {};
    query?.forEach((value, key) => (q[key] = [...(q[key] ?? []), value]));
    calls.push({ path, query: q });
    return nextAnswer(path);
  });
  const send = vi.fn(async (_method: string, path: string, body?: unknown) => {
    calls.push({ path, query: {}, body });
    const answer = nextAnswer(path);
    return { json: async () => answer } as Response;
  });
  const http: Http = {
    get: get as Http['get'],
    send: send as Http['send'],
    onUnauthorized: () => () => {},
    sessionEnded: () => {},
    onReauthRequired: () => () => {},
  };
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
  it('reads posts by id in one batch-get call, in the ids order, and skips the ones gone', async () => {
    const { http, calls } = fakeHttp({
      '/api/v1/posts/batch-get': [
        { items: [apiPost({ key: 'x_2', platform: 'twitter' }), apiPost({ key: 'x_1' })] },
      ],
    });
    const found = await createHttpClient(http).getPostsByIds(['x_1', 'x_9', 'x_2']);
    expect(found.map((p) => p.id)).toEqual(['x_1', 'x_2']);
    expect(calls).toEqual([
      { path: '/api/v1/posts/batch-get', query: {}, body: { keys: ['x_1', 'x_9', 'x_2'] } },
    ]);
  });

  it('chunks more than 200 ids into several batch-get calls (P1-15: the per-user burst)', async () => {
    const ids = Array.from({ length: 250 }, (_, i) => `ig_${i}`);
    const { http, calls } = fakeHttp({
      '/api/v1/posts/batch-get': [
        { items: ids.slice(0, 200).map((key) => apiPost({ key })) },
        { items: ids.slice(200).map((key) => apiPost({ key })) },
      ],
    });
    const found = await createHttpClient(http).getPostsByIds(ids);
    expect(found.map((p) => p.id)).toEqual(ids);
    expect(calls).toHaveLength(2);
    expect((calls[0].body as { keys: string[] }).keys).toHaveLength(200);
    expect((calls[1].body as { keys: string[] }).keys).toHaveLength(50);
  });

  it("honours a batch-get 429's Retry-After: waits, then retries the same chunk once", async () => {
    const { http, calls } = fakeHttp({
      '/api/v1/posts/batch-get': [
        new ApiError(429, 'rate_limited', undefined, 2),
        { items: [apiPost({ key: 'x_1', platform: 'twitter' })] },
      ],
    });
    const wait = vi.fn().mockResolvedValue(undefined);
    const found = await createHttpClient(http, { wait }).getPostsByIds(['x_1']);
    expect(found.map((p) => p.id)).toEqual(['x_1']);
    expect(wait).toHaveBeenCalledWith(2000);
    expect(calls).toHaveLength(2);
    expect(calls[0].body).toEqual(calls[1].body);
  });

  it('lets other errors through', async () => {
    const { http } = fakeHttp({ '/api/v1/posts/batch-get': [new ApiError(500, 'internal')] });
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

describe('httpClient — writes (P1-06)', () => {
  it('saves a manual edit through PATCH /posts/{key} and maps the post back', async () => {
    const { http, calls } = fakeHttp({
      '/api/v1/posts/ig_1': [apiPost({ key: 'ig_1', userNote: 'Updated' })],
    });
    const post = await createHttpClient(http).updatePost('ig_1', { userNote: 'Updated' });
    expect(post.id).toBe('ig_1');
    expect(post.userNote).toBe('Updated');
    expect(calls).toEqual([
      { path: '/api/v1/posts/ig_1', query: {}, body: { userNote: 'Updated' } },
    ]);
  });

  it('creates a folder and maps it back', async () => {
    const { http, calls } = fakeHttp({
      '/api/v1/collections': [
        {
          id: 3,
          name: 'Ricette',
          color: '#3d5afe',
          count: 0,
          createdAt: 1_000,
          externalId: null,
          platform: null,
          position: null,
          sourceName: null,
        },
      ],
    });
    const created = await createHttpClient(http).createCollection('Ricette', '#3d5afe');
    expect(created).toMatchObject({ id: 3, name: 'Ricette' });
    expect(calls).toEqual([
      { path: '/api/v1/collections', query: {}, body: { name: 'Ricette', color: '#3d5afe' } },
    ]);
  });

  it('renames a folder through PATCH', async () => {
    const { http, calls } = fakeHttp({ '/api/v1/collections/3': [{}] });
    await createHttpClient(http).updateCollection(3, { name: 'Idee' });
    expect(calls).toEqual([{ path: '/api/v1/collections/3', query: {}, body: { name: 'Idee' } }]);
  });

  it('deletes a folder (label only by default) and maps the trashed count', async () => {
    const { http, calls } = fakeHttp({ '/api/v1/collections/3': [{ trashed: 0 }] });
    const res = await createHttpClient(http).deleteCollection(3);
    expect(res).toEqual({ ok: true, deletedPosts: 0, errors: [] });
    expect(calls).toEqual([{ path: '/api/v1/collections/3', query: {}, body: undefined }]);
  });

  it('deletes a folder with its posts when asked (P1-11 mode)', async () => {
    const { http, calls } = fakeHttp({
      '/api/v1/collections/3?mode=withPosts': [{ trashed: 5 }],
    });
    const res = await createHttpClient(http).deleteCollection(3, { deletePosts: true });
    expect(res).toEqual({ ok: true, deletedPosts: 5, errors: [] });
    expect(calls[0].path).toBe('/api/v1/collections/3?mode=withPosts');
  });

  it('adds posts to a folder by selector', async () => {
    const { http, calls } = fakeHttp({
      '/api/v1/collections/3/posts': [{ added: 2, collection: {} }],
    });
    await createHttpClient(http).addPostsToCollections(['ig_1', 'ig_2'], [3]);
    expect(calls).toEqual([
      {
        path: '/api/v1/collections/3/posts',
        query: {},
        body: { selector: { keys: ['ig_1', 'ig_2'] } },
      },
    ]);
  });

  it('adds posts to every collection given, one request each', async () => {
    const { http, calls } = fakeHttp({
      '/api/v1/collections/1/posts': [{ added: 1, collection: {} }],
      '/api/v1/collections/2/posts': [{ added: 1, collection: {} }],
    });
    await createHttpClient(http).addPostsToCollections(['ig_1'], [1, 2]);
    expect(calls.map((c) => c.path)).toEqual([
      '/api/v1/collections/1/posts',
      '/api/v1/collections/2/posts',
    ]);
  });

  it('removes one post from one folder (§1.2 #12)', async () => {
    const { http, calls } = fakeHttp({
      '/api/v1/collections/3/posts/ig_1': [{ removed: true, collection: {} }],
    });
    await createHttpClient(http).removePostFromCollection('ig_1', 3);
    expect(calls).toEqual([
      { path: '/api/v1/collections/3/posts/ig_1', query: {}, body: undefined },
    ]);
  });
});

describe('httpClient — capabilities, links and events', () => {
  it('can browse, read and edit the library; nothing else yet', () => {
    const { http } = fakeHttp({});
    const client = createHttpClient(http);
    expect(client.capabilities).toBe(WEB_CAPABILITIES);
    const { libraryEdit, ...rest } = client.capabilities;
    expect(libraryEdit).toBe(true);
    expect(Object.values(rest).every((on) => on === false)).toBe(true);
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
