// A mock of the Shelfy Web API for the web app's Playwright specs: route
// handlers answer `/api/v1/**` from an in-memory library of synthetic posts,
// and record every request. Until the P1-05 synthetic libraries exist the
// specs run without a server (P1 lane rule 6).
import { test as base, type Page, type Route } from '@playwright/test';
import { DISCLAIMER_VERSION, PRIVACY_VERSION } from '../../src/disclaimer';
import type { components } from '../src/api/schema';

type Schemas = components['schemas'];

export interface RecordedRequest {
  method: string;
  path: string;
  query: URLSearchParams;
  body: unknown;
}

export interface MockApi {
  signedIn: boolean;
  posts: Schemas['Post'][];
  // `GET /posts/{key}` answers; a key missing here is 404.
  details: Record<string, unknown>;
  collections: Schemas['Collection'][];
  // P4-09: the user's jobs and which kinds are paused, for `/jobs*` and
  // `/queues/*` (jobs.spec.ts). Mutated in place by the route handlers below,
  // so a spec can read `api.jobs` afterwards to assert on the result.
  jobs: Schemas['Job'][];
  pausedKinds: Set<string>;
  // P2-12: the account's API tokens, for `/me/tokens*` (connections.spec.ts).
  tokens: Schemas['ApiToken'][];
  // The body of each `/events` connection in turn (SSE text); then `hello`.
  streams: string[];
  requests: RecordedRequest[];
  // Requests that left the app's origin.
  thirdParty: string[];
  // Artificial `GET /posts` latency, ms (default 0: instant, like every other
  // route). P1-14: the mock answers synchronously, so a large synthetic
  // library's infinite scroll would otherwise race ahead and load every page
  // before a test can observe "more matches than are loaded" (select-all-
  // matching) — a real server's network latency naturally paces it instead.
  postsDelayMs: number;
  requestsTo(path: string, method?: string): RecordedRequest[];
}

const T0 = Date.UTC(2026, 8, 1);

export function apiPost(overrides: Partial<Schemas['Post']> = {}): Schemas['Post'] {
  return {
    key: 'ig_1',
    platform: 'instagram',
    shortcode: 'C0ffee',
    postUrl: 'https://www.instagram.com/p/C0ffee/',
    profileUrl: null,
    authorUsername: 'studio.example',
    authorName: 'Studio',
    caption: 'Blown-glass lamp',
    mediaType: 'text',
    mediaCount: 0,
    postedAt: T0,
    importedAt: T0,
    sortTs: T0,
    cover: null,
    // No remote pictures: the specs make no third-party request.
    coverUrl: null,
    thumbhash: null,
    archiveState: 'pending',
    aiStatus: null,
    aiDescription: null,
    aiCategory: null,
    aiContentType: null,
    aiLanguage: null,
    aiSaveReason: null,
    aiTags: [],
    aiAnalyzedAt: null,
    userNote: null,
    userTags: [],
    webUrl: null,
    webDomain: null,
    webFinalUrl: null,
    updatedAt: T0,
    deletedAt: null,
    media: [],
    collectionIds: [],
    webCapture: null,
    ...overrides,
  };
}

export function apiDetail(post: Schemas['Post'], overrides: Record<string, unknown> = {}) {
  return {
    ...post,
    aiAttempts: 0,
    aiEntities: [],
    aiError: null,
    aiKeywords: [],
    aiModel: null,
    aiNextAt: null,
    aiProvider: null,
    aiSchemaVersion: null,
    aiWeb: null,
    coverUrlExpiresAt: null,
    entities: [],
    nativeId: post.key.replace(/^[a-z]+_/, ''),
    tags: [],
    ...overrides,
  };
}

export function apiJob(overrides: Partial<Schemas['Job']> = {}): Schemas['Job'] {
  return {
    id: 1,
    kind: 'capture.site',
    state: 'running',
    progress: 0.3,
    stage: null,
    postKey: null,
    errorCode: null,
    attempts: 0,
    maxAttempts: 2,
    runAt: T0,
    createdAt: T0,
    updatedAt: T0,
    finishedAt: null,
    ...overrides,
  };
}

// `GET /jobs`'s cursor: the id before which the next page continues.
const JOB_CURSOR_PREFIX = 'jobs.';

function jobMatches(job: Schemas['Job'], query: URLSearchParams): boolean {
  const kinds = query.getAll('kind');
  const states = query.getAll('state');
  return (
    (!kinds.length || kinds.includes(job.kind)) && (!states.length || states.includes(job.state))
  );
}

const FINISHED_STATES: Schemas['JobState'][] = ['succeeded', 'failed', 'cancelled'];

function queueSummaryOf(
  jobs: Schemas['Job'][],
  pausedKinds: Set<string>,
): Schemas['QueueSummary'][] {
  const byKind = new Map<string, Schemas['QueueSummary']>();
  for (const job of jobs) {
    const queue =
      byKind.get(job.kind) ??
      ({
        kind: job.kind,
        paused: pausedKinds.has(job.kind),
        queued: 0,
        running: 0,
        succeeded: 0,
        failed: 0,
        cancelled: 0,
      } satisfies Schemas['QueueSummary']);
    queue[job.state] += 1;
    byKind.set(job.kind, queue);
  }
  return Array.from(byKind.values());
}

function queueOf(
  kind: string,
  jobs: Schemas['Job'][],
  pausedKinds: Set<string>,
): Schemas['QueueSummary'] {
  return (
    queueSummaryOf(jobs, pausedKinds).find((q) => q.kind === kind) ?? {
      kind,
      paused: pausedKinds.has(kind),
      queued: 0,
      running: 0,
      succeeded: 0,
      failed: 0,
      cancelled: 0,
    }
  );
}

function collection(overrides: Partial<Schemas['Collection']>): Schemas['Collection'] {
  return {
    id: 1,
    name: 'Folder',
    color: '#3d5afe',
    platform: null,
    externalId: null,
    sourceName: null,
    count: 0,
    createdAt: T0,
    position: null,
    ...overrides,
  };
}

export const OWNER: Schemas['Me'] = {
  id: '01J0OWNER000000000000000000',
  email: 'owner@example.test',
  role: 'owner',
  createdAt: T0,
  capabilities: {
    admin: true,
    passkeys: true,
    emailLink: false,
    extension: false,
    'ai.tasks': false,
    capture: false,
    'video.onDemand': false,
  },
  // A returning user: the current notices are accepted (the consent gate stays
  // closed).
  consent: {
    disclaimerVersion: DISCLAIMER_VERSION,
    disclaimerAcceptedAt: T0,
    privacyVersion: PRIVACY_VERSION,
    privacyAcceptedAt: T0,
  },
};

// The account's own routes (`/me/*`), answered with an empty account.
const ACCOUNT_ROUTES: Record<string, unknown> = {
  '/api/v1/me/settings': {
    language: null,
    archiveAssetTypes: { thumbnail: true, image: true, video: true },
  },
  '/api/v1/me/passkeys': { items: [] },
  '/api/v1/me/sessions': {
    items: [
      {
        id: '0123456789abcdef0123456789abcdef',
        current: true,
        createdAt: T0,
        lastSeenAt: T0,
        expiresAt: T0 + 30 * 86_400_000,
        userAgent: null,
      },
    ],
  },
  '/api/v1/extension/status': { connected: false, lastSeenAt: null, version: null },
  '/api/v1/me/usage': { usedBytes: 0, mediaBytes: 0, dbBytes: 0, quotaBytes: 0, updatedAt: T0 },
  '/api/v1/version': { version: 'e2e', apiVersion: '1' },
};

function library(): Pick<MockApi, 'posts' | 'details' | 'collections'> {
  const posts = [
    apiPost({ key: 'ig_1', caption: 'Blown-glass lamp', collectionIds: [1] }),
    apiPost({
      key: 'x_2',
      platform: 'twitter',
      shortcode: null,
      postUrl: 'https://x.com/studio/status/2',
      caption: 'A thread about chairs',
      postedAt: T0 - 1000,
      sortTs: T0 - 1000,
    }),
    apiPost({
      key: 'pin_3',
      platform: 'pinterest',
      shortcode: null,
      postUrl: 'https://www.pinterest.com/pin/3/',
      caption: 'Terracotta tiles',
      collectionIds: [2],
      postedAt: T0 - 2000,
      sortTs: T0 - 2000,
    }),
  ];
  const details: Record<string, unknown> = {};
  for (const post of posts) details[post.key] = apiDetail(post);
  return {
    posts,
    details,
    collections: [
      collection({ id: 1, name: 'Lighting', platform: 'instagram', externalId: '179', count: 1 }),
      collection({ id: 2, name: 'Inspiration', color: '#ffaa00', count: 1 }),
    ],
  };
}

export const HELLO = sse(
  'hello',
  { version: 'e2e', heartbeatMs: 20_000, lastEventId: 'e-0' },
  'e-0',
);

// One server-sent event, as the server frames it.
export function sse(event: string, data: unknown, id?: string): string {
  return `event: ${event}\n${id ? `id: ${id}\n` : ''}data: ${JSON.stringify(data)}\n\n`;
}

function problem(route: Route, status: number, code: string): Promise<void> {
  return route.fulfill({
    status,
    contentType: 'application/problem+json',
    body: JSON.stringify({ type: 'about:blank', title: code, status, code }),
  });
}

// A minimal `FilterParams`-shaped match (P1-11/P1-14): only the fields the
// bulk/trash/count specs actually exercise. `trash` defaults to "library
// only" (false/undefined), mirroring the real server.
function matchesFilter(
  p: Schemas['Post'],
  filter: {
    platform?: string | null;
    trash?: boolean | null;
    mediaType?: string[] | null;
    collection?: number | null;
  } = {},
): boolean {
  const inTrash = p.deletedAt != null;
  if (filter.trash ? !inTrash : inTrash) return false;
  if (filter.platform && p.platform !== filter.platform) return false;
  if (filter.mediaType?.length && !filter.mediaType.includes(p.mediaType)) return false;
  if (filter.collection != null && !p.collectionIds.includes(filter.collection)) return false;
  return true;
}

// A `PostSelector` (P1-03/P1-11): explicit keys, or every post `filter`
// matches minus `exceptKeys` ("select all matching").
function resolveSelector(
  api: MockApi,
  selector: {
    keys?: string[];
    filter?: Parameters<typeof matchesFilter>[1];
    exceptKeys?: string[];
  },
): Schemas['Post'][] {
  if (selector.keys) {
    const want = new Set(selector.keys);
    return api.posts.filter((p) => want.has(p.key));
  }
  const matched = api.posts.filter((p) => matchesFilter(p, selector.filter ?? {}));
  if (!selector.exceptKeys?.length) return matched;
  const except = new Set(selector.exceptKeys);
  return matched.filter((p) => !except.has(p.key));
}

// P1-11/P1-14: trashed posts count separately and drop out of the library's
// own totals (mirroring the server, which never counts them in `GET /stats`
// or a plain `GET /posts` without `trash=1`).
function stats(api: MockApi): Schemas['Stats'] {
  const byPlatform = { instagram: 0, twitter: 0, pinterest: 0, web: 0, manual: 0 };
  const byMediaType: Record<string, number> = {};
  let total = 0;
  let trashed = 0;
  for (const post of api.posts) {
    if (post.deletedAt != null) {
      trashed += 1;
      continue;
    }
    total += 1;
    byPlatform[post.platform] += 1;
    byMediaType[post.mediaType] = (byMediaType[post.mediaType] ?? 0) + 1;
  }
  return {
    total,
    byPlatform,
    byMediaType,
    stored: 0,
    storedByKind: { covers: 0, images: 0, videos: 0 },
    trashed,
  };
}

async function answer(api: MockApi, route: Route): Promise<void> {
  const request = route.request();
  const url = new URL(request.url());
  const { pathname: path, searchParams: query } = url;
  const method = request.method();
  let body: unknown = null;
  try {
    body = request.postDataJSON();
  } catch {
    body = request.postData();
  }
  api.requests.push({ method, path, query, body });

  if (path === '/api/v1/posts' && api.postsDelayMs > 0) {
    await new Promise((resolve) => setTimeout(resolve, api.postsDelayMs));
  }

  if (path === '/api/v1/auth/methods') {
    return route.fulfill({ json: { emailLink: true, passkeys: false } });
  }
  if (!api.signedIn) return problem(route, 401, 'unauthorized');
  if (path === '/api/v1/events') {
    return route.fulfill({
      status: 200,
      headers: { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-store' },
      body: api.streams.shift() ?? HELLO,
    });
  }
  if (path === '/api/v1/me') return route.fulfill({ json: OWNER });
  if (path === '/api/v1/me/tokens') {
    if (method === 'POST') {
      const { kind, label } = (body ?? {}) as { kind: Schemas['TokenKind']; label?: string };
      const apiToken: Schemas['ApiToken'] = {
        id: `tok_${api.tokens.length + 1}`,
        kind,
        label: label ?? null,
        scopes: kind === 'shortcut' ? ['links:create'] : ['ingest'],
        createdAt: T0,
        lastUsedAt: null,
        expiresAt: null,
      };
      api.tokens.push(apiToken);
      return route.fulfill({ status: 201, json: { apiToken, token: 'shx_e2e_secret' } });
    }
    return route.fulfill({ json: { items: api.tokens } });
  }
  if (path === '/api/v1/me/tokens/pairing-code' && method === 'POST') {
    return route.fulfill({ status: 201, json: { code: 'p'.repeat(43), expiresAt: T0 + 60_000 } });
  }
  if (path.startsWith('/api/v1/me/tokens/') && method === 'DELETE') {
    const id = decodeURIComponent(path.slice('/api/v1/me/tokens/'.length));
    api.tokens = api.tokens.filter((t) => t.id !== id);
    return route.fulfill({ status: 204 });
  }
  if (method === 'GET' && path in ACCOUNT_ROUTES) {
    return route.fulfill({ json: ACCOUNT_ROUTES[path] });
  }
  if (path === '/api/v1/stats') return route.fulfill({ json: stats(api) });
  if (path === '/api/v1/collections') {
    if (method === 'POST') {
      const { name, color } = (body ?? {}) as { name: string; color?: string };
      const id = Math.max(0, ...api.collections.map((c) => c.id)) + 1;
      const created = collection({ id, name, color: color ?? '#3d5afe', count: 0 });
      api.collections.push(created);
      return route.fulfill({ status: 201, json: created });
    }
    return route.fulfill({ json: { items: api.collections } });
  }
  if (path === '/api/v1/posts/batch-get' && method === 'POST') {
    const keys = Array.isArray((body as { keys?: unknown })?.keys)
      ? (body as { keys: unknown[] }).keys.filter((k): k is string => typeof k === 'string')
      : [];
    const items = keys
      .map((key) => api.posts.find((p) => p.key === key))
      .filter((p): p is Schemas['Post'] => !!p);
    return route.fulfill({ json: { items } });
  }
  // P1-14: /posts/count and /posts/bulk are explicit checks, like batch-get
  // above — both would otherwise match the generic `/posts/{key}` regex below
  // (its key becoming the literal string "count" or "bulk").
  if (path === '/api/v1/posts/count') {
    const filter = {
      platform: query.get('platform') || undefined,
      trash: query.get('trash') === 'true' || query.get('trash') === '1',
      mediaType: query.getAll('mediaType'),
      collection: query.get('collection') ? Number(query.get('collection')) : undefined,
    };
    const total = api.posts.filter((p) => matchesFilter(p, filter)).length;
    return route.fulfill({ json: { total } satisfies Schemas['PostCount'] });
  }
  if (path === '/api/v1/posts/bulk' && method === 'POST') {
    const { selector, action, params } = (body ?? {}) as {
      selector?: Parameters<typeof resolveSelector>[1];
      action: Schemas['BulkAction'];
      params?: { collectionId?: number; collectionIds?: number[] };
    };
    const matched = resolveSelector(api, selector ?? {});
    // A fresh stamp per call (not a fixed constant): two deletes in the same
    // test must get distinct deletedAt handles, so an undo-by-deletedAt
    // after the SECOND one never also restores the first.
    const now = Date.now();
    let changed = 0;
    for (const p of matched) {
      switch (action) {
        case 'delete':
          if (p.deletedAt == null) {
            p.deletedAt = now;
            changed += 1;
          }
          break;
        case 'addToCollections':
          for (const cid of params?.collectionIds ?? []) {
            if (!p.collectionIds.includes(cid)) p.collectionIds = [...p.collectionIds, cid];
          }
          changed += 1;
          break;
        case 'removeFromCollection':
          if (params?.collectionId != null && p.collectionIds.includes(params.collectionId)) {
            p.collectionIds = p.collectionIds.filter((c) => c !== params.collectionId);
            changed += 1;
          }
          break;
        case 'clearAiDescription':
          p.aiDescription = null;
          p.aiStatus = null;
          changed += 1;
          break;
        case 'clearAiTags':
          p.aiTags = [];
          p.aiStatus = null;
          changed += 1;
          break;
        default:
          break;
      }
    }
    // F11: a null stamp means nothing moved — no undo handle then.
    const deletedAt = action === 'delete' && changed > 0 ? now : null;
    return route.fulfill({
      json: {
        action,
        changed,
        selected: matched.length,
        deletedAt,
        job: null,
      } satisfies Schemas['BulkResult'],
    });
  }
  if (path === '/api/v1/trash') {
    const trashed = api.posts
      .filter((p) => p.deletedAt != null)
      .sort((a, b) => (b.deletedAt ?? 0) - (a.deletedAt ?? 0));
    return route.fulfill({
      json: {
        items: trashed,
        nextCursor: null,
        total: trashed.length,
        retentionDays: 30,
      } satisfies Schemas['TrashPage'],
    });
  }
  if (path === '/api/v1/trash/restore' && method === 'POST') {
    const req = (body ?? {}) as {
      deletedAt?: number;
      selector?: Parameters<typeof resolveSelector>[1];
    };
    const matched =
      typeof req.deletedAt === 'number'
        ? api.posts.filter((p) => p.deletedAt === req.deletedAt)
        : resolveSelector(api, req.selector ?? {});
    let changed = 0;
    for (const p of matched) {
      if (p.deletedAt != null) {
        p.deletedAt = null;
        changed += 1;
      }
    }
    return route.fulfill({
      json: {
        action: 'restore',
        changed,
        selected: matched.length,
        deletedAt: null,
        job: null,
      } satisfies Schemas['BulkResult'],
    });
  }
  if (path === '/api/v1/trash/empty' && method === 'POST') {
    const trashed = api.posts.filter((p) => p.deletedAt != null);
    api.posts = api.posts.filter((p) => p.deletedAt == null);
    const now = T0 + 30_000;
    return route.fulfill({
      status: 202,
      json: {
        selected: trashed.length,
        // `POST /trash/empty` always starts a job (plan: "starts the purge
        // job"); the mock purges synchronously above, so it reports one
        // that already finished — nothing more for a test to wait on.
        job: {
          id: 1,
          kind: 'purge',
          state: 'succeeded',
          progress: 1,
          stage: null,
          postKey: null,
          errorCode: null,
          attempts: 0,
          maxAttempts: 1,
          runAt: now,
          createdAt: now,
          updatedAt: now,
          finishedAt: now,
        },
      } satisfies Schemas['TrashEmptying'],
    });
  }
  if (path === '/api/v1/posts') {
    // The same predicate `/posts/count` and the bulk/trash selector use
    // (platform, trash, mediaType, collection) — `GET /posts` must agree with
    // them, or a platform-filtered gallery view and its "select all
    // matching" would disagree on what matches.
    const filter = {
      platform: query.get('platform') || undefined,
      trash: query.get('trash') === 'true' || query.get('trash') === '1',
      mediaType: query.getAll('mediaType'),
      collection: query.get('collection') ? Number(query.get('collection')) : undefined,
    };
    const matching = api.posts.filter((p) => matchesFilter(p, filter));
    // Real paging (P1-14: large/synthetic-library specs need `total` to
    // legitimately exceed one loaded page, to exercise "select all
    // matching"): `cursor` is the offset of the next page, as a string.
    const limit = Math.max(1, Math.min(200, Number(query.get('limit')) || 60));
    const offset = Number(query.get('cursor')) || 0;
    const items = matching.slice(offset, offset + limit);
    const nextCursor =
      offset + items.length < matching.length ? String(offset + items.length) : null;
    const json: { items: typeof items; nextCursor: string | null; total?: number } = {
      items,
      nextCursor,
    };
    if (query.get('includeTotal') === 'true') json.total = matching.length;
    return route.fulfill({ json });
  }
  const post = /^\/api\/v1\/posts\/([^/]+)$/.exec(path);
  if (post) {
    const key = decodeURIComponent(post[1]);
    if (method === 'PATCH') {
      const idx = api.posts.findIndex((p) => p.key === key);
      if (idx === -1) return problem(route, 404, 'not_found');
      const patch = (body ?? {}) as Partial<Schemas['Post']>;
      api.posts[idx] = { ...api.posts[idx], ...patch };
      const detail = { ...(api.details[key] ?? apiDetail(api.posts[idx])), ...patch };
      api.details[key] = detail;
      return route.fulfill({ json: detail });
    }
    const detail = api.details[key];
    return detail ? route.fulfill({ json: detail }) : problem(route, 404, 'not_found');
  }
  const collectionId = /^\/api\/v1\/collections\/(\d+)$/.exec(path);
  if (collectionId && (method === 'PATCH' || method === 'DELETE')) {
    const id = Number(collectionId[1]);
    const idx = api.collections.findIndex((c) => c.id === id);
    if (idx === -1) return problem(route, 404, 'not_found');
    if (method === 'PATCH') {
      const patch = (body ?? {}) as Partial<Schemas['Collection']>;
      api.collections[idx] = { ...api.collections[idx], ...patch };
      return route.fulfill({ json: api.collections[idx] });
    }
    // DELETE: `mode=withPosts` moves every linked post to the trash (soft:
    // `deletedAt` only — this mock never actually hides trashed posts from
    // `GET /posts`, which no P1-06 spec reads from the trash anyway).
    const mode = query.get('mode');
    api.collections.splice(idx, 1);
    const now = T0 + 10_000;
    let trashed = 0;
    for (const p of api.posts) {
      if (!p.collectionIds.includes(id)) continue;
      p.collectionIds = p.collectionIds.filter((c) => c !== id);
      if (mode === 'withPosts') {
        p.deletedAt = now;
        trashed += 1;
      }
    }
    return route.fulfill({ json: { trashed, deletedAt: trashed > 0 ? now : null } });
  }
  const addPosts = /^\/api\/v1\/collections\/(\d+)\/posts$/.exec(path);
  if (addPosts && method === 'POST') {
    const id = Number(addPosts[1]);
    const col = api.collections.find((c) => c.id === id);
    if (!col) return problem(route, 404, 'not_found');
    const keys: string[] =
      (body as { selector?: { keys?: string[] } } | null)?.selector?.keys ?? [];
    let added = 0;
    for (const key of keys) {
      const p = api.posts.find((p) => p.key === key);
      if (p && !p.collectionIds.includes(id)) {
        p.collectionIds = [...p.collectionIds, id];
        added += 1;
      }
    }
    col.count += added;
    return route.fulfill({ json: { added, collection: col } });
  }
  const removePost = /^\/api\/v1\/collections\/(\d+)\/posts\/([^/]+)$/.exec(path);
  if (removePost && method === 'DELETE') {
    const id = Number(removePost[1]);
    const key = decodeURIComponent(removePost[2]);
    const col = api.collections.find((c) => c.id === id);
    if (!col) return problem(route, 404, 'not_found');
    const p = api.posts.find((p) => p.key === key);
    let removed = false;
    if (p && p.collectionIds.includes(id)) {
      p.collectionIds = p.collectionIds.filter((c) => c !== id);
      col.count = Math.max(0, col.count - 1);
      removed = true;
    }
    return route.fulfill({ json: { removed, collection: col } });
  }
  if (path === '/api/v1/client-errors' && method === 'POST') {
    return route.fulfill({ status: 204 });
  }

  // ── Jobs (P4-09) ─────────────────────────────────────────────────────────
  if (path === '/api/v1/jobs' && method === 'GET') {
    const matching = api.jobs.filter((j) => jobMatches(j, query)).sort((a, b) => b.id - a.id);
    const cursor = query.get('cursor');
    const before = cursor?.startsWith(JOB_CURSOR_PREFIX)
      ? Number(cursor.slice(JOB_CURSOR_PREFIX.length))
      : null;
    const windowed = before == null ? matching : matching.filter((j) => j.id < before);
    const limit = Number(query.get('limit')) || 60;
    const items = windowed.slice(0, limit);
    const last = items.at(-1);
    const nextCursor = windowed.length > limit && last ? `${JOB_CURSOR_PREFIX}${last.id}` : null;
    return route.fulfill({ json: { items, nextCursor } });
  }
  if (path === '/api/v1/jobs/summary' && method === 'GET') {
    return route.fulfill({ json: { queues: queueSummaryOf(api.jobs, api.pausedKinds) } });
  }
  const cancelJob = /^\/api\/v1\/jobs\/(\d+)\/cancel$/.exec(path);
  if (cancelJob && method === 'POST') {
    const job = api.jobs.find((j) => j.id === Number(cancelJob[1]));
    if (!job) return problem(route, 404, 'not_found');
    job.state = 'cancelled';
    return route.fulfill({ json: job });
  }
  const retryJob = /^\/api\/v1\/jobs\/(\d+)\/retry$/.exec(path);
  if (retryJob && method === 'POST') {
    const job = api.jobs.find((j) => j.id === Number(retryJob[1]));
    if (!job) return problem(route, 404, 'not_found');
    job.state = 'queued';
    job.errorCode = null;
    return route.fulfill({ json: job });
  }
  const queueAction = /^\/api\/v1\/queues\/([^/]+)\/(pause|resume|cancel-all|clear-finished)$/.exec(
    path,
  );
  if (queueAction && method === 'POST') {
    const kind = decodeURIComponent(queueAction[1]);
    let affected = 0;
    if (queueAction[2] === 'pause') {
      affected = api.pausedKinds.has(kind) ? 0 : 1;
      api.pausedKinds.add(kind);
    } else if (queueAction[2] === 'resume') {
      affected = api.pausedKinds.has(kind) ? 1 : 0;
      api.pausedKinds.delete(kind);
    } else if (queueAction[2] === 'cancel-all') {
      // The real scheduler publishes `job.updated` for every job it cancels
      // this way (crates/server/src/jobs/scheduler.rs `cancel_all`): queue it
      // on the next reconnect, same as any other live event here.
      const updated: string[] = [];
      for (const job of api.jobs) {
        if (job.kind === kind && (job.state === 'queued' || job.state === 'running')) {
          job.state = 'cancelled';
          affected += 1;
          updated.push(
            sse(
              'job.updated',
              {
                id: job.id,
                kind: job.kind,
                state: job.state,
                progress: job.progress,
                stage: job.stage,
                postKey: job.postKey,
                errorCode: job.errorCode,
              },
              `e-cancel-${job.id}`,
            ),
          );
        }
      }
      if (updated.length) api.streams.push(HELLO + updated.join(''));
    } else {
      const before = api.jobs.length;
      api.jobs = api.jobs.filter((j) => !(j.kind === kind && FINISHED_STATES.includes(j.state)));
      affected = before - api.jobs.length;
    }
    return route.fulfill({ json: { queue: queueOf(kind, api.jobs, api.pausedKinds), affected } });
  }

  return problem(route, 404, 'not_found');
}

export async function mockApi(page: Page, origin: string): Promise<MockApi> {
  const api: MockApi = {
    signedIn: true,
    ...library(),
    jobs: [],
    pausedKinds: new Set(),
    tokens: [],
    streams: [],
    requests: [],
    thirdParty: [],
    postsDelayMs: 0,
    requestsTo(path, method = 'GET') {
      return this.requests.filter((r) => r.path === path && r.method === method);
    },
  };
  page.on('request', (request) => {
    if (!request.url().startsWith(origin) && !request.url().startsWith('data:')) {
      api.thirdParty.push(request.url());
    }
  });
  await page.route('**/api/v1/**', (route) => answer(api, route));
  await page.route('**/media/**', (route) => problem(route, 404, 'not_found'));
  // A returning user: the disclaimer is accepted and the language is English.
  await page.addInitScript((version) => {
    localStorage.setItem(
      'app:disclaimerAcceptance',
      JSON.stringify({ version, acceptedAt: new Date().toISOString(), dontShowAgain: true }),
    );
    localStorage.setItem('app:language', 'en');
  }, DISCLAIMER_VERSION);
  return api;
}

// `api` is automatic: every page is mocked, even in a test that does not read it.
export const test = base.extend<{ api: MockApi }>({
  api: [
    async ({ page, baseURL }, use) => {
      await use(await mockApi(page, baseURL ?? ''));
    },
    { auto: true },
  ],
});

export { expect } from '@playwright/test';
