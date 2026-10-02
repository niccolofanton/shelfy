// The web ShelfyClient: the HTTP API of shelfy-server (`/api/v1`), typed by
// the generated OpenAPI types (./schema.d.ts), and its realtime stream.
import type {
  CollectionDeleteOptions,
  CollectionDeleteResult,
  PageRequest,
  PostEdit,
  PostPage,
  PostQuery,
  ShelfyCapabilities,
  ShelfyClient,
  ShelfyEvent,
  ViewErrorReport,
} from '@ui/api/ShelfyClient';
import { createAccountApi } from './account';
import { createEventStream, type EventStream } from './events';
import { isApiError, type Http } from './http';
import { createLinksApi } from './links';
import {
  MAX_PAGE_SIZE,
  listPostsParams,
  toCollection,
  toPost,
  toSearchParams,
  toStats,
  webMedia,
} from './mapping';
import type { components } from './schema';

type Schemas = components['schemas'];

// `POST /posts/batch-get` takes at most 200 keys per call (plan §2.9).
const MAX_BATCH_GET = 200;

// What the web app can do without an account: browse, read and edit the
// library. Each capability turns on with the task that brings its API
// (libraryEdit: P1-06 — note, manual tags, manual AI edits and folders, all
// through PATCH/collections; bulkActions: P1-14, ai: P3…); the desktop-only
// ones (window chrome, local files, in-app browsers, live page fallback,
// updates, local models) stay off.
export const WEB_CAPABILITIES: ShelfyCapabilities = Object.freeze({
  windowControls: false,
  trafficLights: false,
  localFiles: false,
  browser: false,
  webviewFallback: false,
  ai: false,
  websites: false,
  bookmarks: false,
  libraryEdit: true,
  bulkActions: false,
  settings: false,
  activity: false,
  feedback: false,
  account: false,
  updates: false,
  localModels: false,
  links: false,
});

// What the web app can do for a signed-in user, from `GET /me` (plan §2.19
// Capabilities): their account and its Settings. The server's other
// capabilities drive views that still call the desktop bridge, so they stay
// off here until those views move onto this client: `extension` (P2),
// `ai.tasks` → `ai` (P3), `capture` → `websites` and `video.onDemand` (P4).
// `passkeys` and `emailLink` are the account's sign-in methods
// (AccountApi.signIn).
export function webCapabilities(me: Schemas['Me'] | null | undefined): ShelfyCapabilities {
  if (!me) return WEB_CAPABILITIES;
  return Object.freeze({ ...WEB_CAPABILITIES, account: true, settings: true, links: true });
}

// Opens only http(s) URLs, in a new tab without access to this window.
export function openExternalUrl(url: string, open: typeof window.open = window.open): void {
  let protocol: string;
  try {
    protocol = new URL(url).protocol;
  } catch {
    return;
  }
  if (protocol !== 'http:' && protocol !== 'https:') return;
  open.call(window, url, '_blank', 'noopener,noreferrer');
}

export interface HttpClientOptions {
  // The signed-in user (`GET /me`): the client gets their account
  // (./account.ts) and the capabilities that go with it. Made once per
  // session: the client owns the session's realtime stream.
  me?: Schemas['Me'] | null;
  // What this client can do. Default: webCapabilities(me).
  capabilities?: ShelfyCapabilities;
  // The realtime stream. Default: `/api/v1/events`, whose failed connections
  // check the session (`GET /me`), so an expired one signs the app out.
  events?: EventStream;
  // Where error-boundary reports go (./clientErrors.ts). Default: dropped.
  reportError?: (report: ViewErrorReport) => void;
  // How a 429's `Retry-After` is honoured (P1-15). Default: a real wait; tests
  // inject an instant one so a rate-limit spec doesn't actually sleep.
  wait?: (ms: number) => Promise<void>;
}

const defaultWait = (ms: number): Promise<void> =>
  new Promise((resolve) => setTimeout(resolve, ms));

export function createHttpClient(http: Http, options: HttpClientOptions = {}): ShelfyClient {
  const events =
    options.events ??
    createEventStream({
      // A 401 here reaches Http's unauthorized listeners; any other failure
      // is the stream's to retry.
      onConnectFailed: () => void http.get('/api/v1/me').catch(() => {}),
    });
  const reportError = options.reportError ?? (() => {});
  const me = options.me ?? null;
  const wait = options.wait ?? defaultWait;

  // POSTs one batch-get chunk; a 429 is honoured once (wait `Retry-After`,
  // then retry the same chunk) before giving up like any other failure.
  async function batchGet(keys: string[]): Promise<Schemas['PostBatch']> {
    try {
      const res = await http.send('POST', '/api/v1/posts/batch-get', { keys });
      return (await res.json()) as Schemas['PostBatch'];
    } catch (err) {
      if (!isApiError(err, 'rate_limited') || err.retryAfter == null) throw err;
      await wait(err.retryAfter * 1000);
      const res = await http.send('POST', '/api/v1/posts/batch-get', { keys });
      return (await res.json()) as Schemas['PostBatch'];
    }
  }

  return {
    capabilities: options.capabilities ?? webCapabilities(me),
    media: webMedia,
    ...(me ? { account: createAccountApi(http, me, { events }), links: createLinksApi(http) } : {}),

    // One window of `limit` posts: as many API pages as it takes (at most 200
    // each). The first page of a query also asks for the total.
    async listPosts(query: PostQuery, { limit, cursor, signal }: PageRequest): Promise<PostPage> {
      const posts: Shelfy.Post[] = [];
      const first = !cursor;
      let next: string | null = cursor || null;
      let total: number | undefined;
      do {
        const params = listPostsParams(query, {
          limit: Math.min(MAX_PAGE_SIZE, limit - posts.length),
          cursor: next,
          includeTotal: first && posts.length === 0,
        });
        const page = await http.get<Schemas['PostPage']>(
          '/api/v1/posts',
          toSearchParams(params),
          signal,
        );
        if (total === undefined && typeof page.total === 'number') total = page.total;
        for (const item of page.items) posts.push(toPost(item));
        next = page.nextCursor;
      } while (next && posts.length < limit);
      return { posts, total, nextCursor: next };
    },

    // `POST /posts/batch-get`, chunked at MAX_BATCH_GET (P1-15: a GET per post
    // put more than 60 ids over the per-user burst). The batch read answers
    // the gallery's list shape, not the single-post detail's, so a post opened
    // this way carries no AI entities/keywords (only `GET /posts/{key}` has
    // them) until it is edited, whose PATCH answer is the full detail.
    async getPostsByIds(ids: string[]): Promise<Shelfy.Post[]> {
      const byKey = new Map<string, Shelfy.Post>();
      for (let i = 0; i < ids.length; i += MAX_BATCH_GET) {
        const chunk = ids.slice(i, i + MAX_BATCH_GET);
        if (chunk.length === 0) continue;
        const { items } = await batchGet(chunk);
        for (const item of items) byKey.set(item.key, toPost(item));
      }
      const found: Shelfy.Post[] = [];
      for (const id of ids) {
        const post = byKey.get(id);
        if (post) found.push(post);
      }
      return found;
    },

    async getStats(): Promise<Shelfy.Stats> {
      return toStats(await http.get<Schemas['Stats']>('/api/v1/stats'));
    },

    async listCollections(): Promise<Shelfy.Collection[]> {
      const { items } = await http.get<Schemas['CollectionList']>('/api/v1/collections');
      return items.map(toCollection);
    },

    async updatePost(id: string, edit: PostEdit): Promise<Shelfy.Post> {
      const res = await http.send(
        'PATCH',
        `/api/v1/posts/${encodeURIComponent(id)}`,
        edit satisfies Schemas['PostPatch'],
      );
      return toPost(await res.json());
    },

    async createCollection(name, color): Promise<Shelfy.Collection> {
      const res = await http.send('POST', '/api/v1/collections', { name, color });
      return toCollection(await res.json());
    },

    async updateCollection(id, fields): Promise<void> {
      await http.send('PATCH', `/api/v1/collections/${id}`, fields);
    },

    async deleteCollection(
      id: number,
      options?: CollectionDeleteOptions,
    ): Promise<CollectionDeleteResult> {
      // `mode=withPosts` moves the posts to the trash (server-side since
      // P1-11); the UI only offers the choice once its own trash/bulk
      // surface exists (capability `bulkActions`, P1-14).
      const mode = options?.deletePosts ? 'withPosts' : undefined;
      const path = `/api/v1/collections/${id}` + (mode ? `?mode=${mode}` : '');
      const res = await http.send('DELETE', path);
      const { trashed } = (await res.json()) as Schemas['CollectionDeleted'];
      return { ok: true, deletedPosts: trashed, errors: [] };
    },

    async addPostsToCollections(postIds, collectionIds): Promise<void> {
      for (const id of collectionIds) {
        await http.send('POST', `/api/v1/collections/${id}/posts`, {
          selector: { keys: postIds },
        });
      }
    },

    async removePostFromCollection(postId, collectionId): Promise<void> {
      await http.send(
        'DELETE',
        `/api/v1/collections/${collectionId}/posts/${encodeURIComponent(postId)}`,
      );
    },

    openExternal: (url) => openExternalUrl(url),

    // The stream's events, in the seam's terms. `post.stored` and
    // `post.analyzed` have no source yet: archive and AI jobs (P2–P4) report
    // through `job.updated`, and their changes already come as `posts.changed`.
    on(type, listener) {
      const emit = listener as (event: ShelfyEvent) => void;
      switch (type) {
        case 'posts.changed':
          return events.on('posts.changed', ({ keys, reason }) =>
            emit({ type: 'posts.changed', keys, reason }),
          );
        case 'stats.changed':
          return events.on('stats.changed', () => emit({ type: 'stats.changed' }));
        case 'resync':
          return events.on('resync', () => emit({ type: 'resync' }));
        default:
          return () => {};
      }
    },

    reportError,
  };
}
