// The web ShelfyClient: the HTTP API of shelfy-server (`/api/v1`), typed by
// the generated OpenAPI types (./schema.d.ts).
import type {
  PageRequest,
  PostPage,
  PostQuery,
  ShelfyCapabilities,
  ShelfyClient,
} from '@ui/api/ShelfyClient';
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

// What the web app can do so far: browse and read. Each capability turns on
// with the task that brings its API (libraryEdit: P1-06, bulkActions: P1-14,
// settings: P1-20, ai: P3…); the desktop-only ones (window chrome, local
// files, in-app browsers, live page fallback) stay off. P1-20 reads them from
// `GET /me` instead.
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
});

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

export function createHttpClient(http: Http): ShelfyClient {
  return {
    capabilities: WEB_CAPABILITIES,
    media: webMedia,

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

    // Live updates arrive with the SSE client (P1-04); until then nothing is pushed.
    on: () => () => {},
  };
}
