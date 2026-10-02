// A mock of the Shelfy Web API for the web app's Playwright specs: route
// handlers answer `/api/v1/**` from an in-memory library of synthetic posts,
// and record every request. Until the P1-05 synthetic libraries exist the
// specs run without a server (P1 lane rule 6).
import { test as base, type Page, type Route } from '@playwright/test';
import { DISCLAIMER_VERSION } from '../../src/disclaimer';
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
  // The body of each `/events` connection in turn (SSE text); then `hello`.
  streams: string[];
  requests: RecordedRequest[];
  // Requests that left the app's origin.
  thirdParty: string[];
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

function stats(api: MockApi): Schemas['Stats'] {
  const byPlatform = { instagram: 0, twitter: 0, pinterest: 0, web: 0, manual: 0 };
  const byMediaType: Record<string, number> = {};
  for (const post of api.posts) {
    byPlatform[post.platform] += 1;
    byMediaType[post.mediaType] = (byMediaType[post.mediaType] ?? 0) + 1;
  }
  return {
    total: api.posts.length,
    byPlatform,
    byMediaType,
    stored: 0,
    storedByKind: { covers: 0, images: 0, videos: 0 },
    trashed: 0,
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
  if (path === '/api/v1/stats') return route.fulfill({ json: stats(api) });
  if (path === '/api/v1/collections') return route.fulfill({ json: { items: api.collections } });
  if (path === '/api/v1/posts') {
    const folder = Number(query.get('collection')) || null;
    const items = api.posts.filter((p) => folder == null || p.collectionIds.includes(folder));
    return route.fulfill({ json: { items, nextCursor: null, total: items.length } });
  }
  const post = /^\/api\/v1\/posts\/([^/]+)$/.exec(path);
  if (post) {
    const detail = api.details[decodeURIComponent(post[1])];
    return detail ? route.fulfill({ json: detail }) : problem(route, 404, 'not_found');
  }
  if (path === '/api/v1/client-errors' && method === 'POST') {
    return route.fulfill({ status: 204 });
  }
  return problem(route, 404, 'not_found');
}

export async function mockApi(page: Page, origin: string): Promise<MockApi> {
  const api: MockApi = {
    signedIn: true,
    ...library(),
    streams: [],
    requests: [],
    thirdParty: [],
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
