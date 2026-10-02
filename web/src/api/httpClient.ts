// The web ShelfyClient: the HTTP API of shelfy-server (`/api/v1`), typed by
// the generated OpenAPI types (./schema.d.ts), and its realtime stream.
import type {
  PageRequest,
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

// What the web app can do without an account: browse and read. Each
// capability turns on with the task that brings its API (libraryEdit: P1-06,
// bulkActions: P1-14, ai: P3…); the desktop-only ones (window chrome, local
// files, in-app browsers, live page fallback, updates, local models) stay off.
export const WEB_CAPABILITIES: ShelfyCapabilities = Object.freeze({
  windowControls: false,
  trafficLights: false,
  localFiles: false,
  browser: false,
  webviewFallback: false,
  ai: false,
  websites: false,
  bookmarks: false,
  libraryEdit: false,
  bulkActions: false,
  settings: false,
  activity: false,
  feedback: false,
  account: false,
  updates: false,
  localModels: false,
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
  return Object.freeze({ ...WEB_CAPABILITIES, account: true, settings: true });
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
}

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

  return {
    capabilities: options.capabilities ?? webCapabilities(me),
    media: webMedia,
    ...(me ? { account: createAccountApi(http, me, { events }) } : {}),

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

    // One request per post until the API has a batch read (P1-03); a post that
    // no longer exists is skipped.
    async getPostsByIds(ids: string[]): Promise<Shelfy.Post[]> {
      const found = await Promise.all(
        ids.map(async (id) => {
          try {
            return toPost(
              await http.get<Schemas['PostDetail']>(`/api/v1/posts/${encodeURIComponent(id)}`),
            );
          } catch (err) {
            if (isApiError(err, 'not_found')) return null;
            throw err;
          }
        }),
      );
      return found.filter((p): p is Shelfy.Post => p !== null);
    },

    async getStats(): Promise<Shelfy.Stats> {
      return toStats(await http.get<Schemas['Stats']>('/api/v1/stats'));
    },

    async listCollections(): Promise<Shelfy.Collection[]> {
      const { items } = await http.get<Schemas['CollectionList']>('/api/v1/collections');
      return items.map(toCollection);
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
