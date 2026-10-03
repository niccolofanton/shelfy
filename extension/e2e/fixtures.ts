import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import type { BrowserContext } from '@playwright/test';
import { igPkToShortcode } from '../src/shared/identity';
import { CDN_HOST, ORIGIN, PNG } from './server';
export const FOLDER_ID = '17890000000000001';
export const SAVED = 'https://www.instagram.com/someone/saved/all-posts/';
export const FOLDER = `https://www.instagram.com/someone/saved/recipes/${FOLDER_ID}/`;
export const PK = (n: number) => String(3500000000000000000n + BigInt(n));
export const KEY = (n: number) => `ig_${PK(n)}`;
export const CDN = (n: number) => `https://${CDN_HOST}/v/fixture-${n}.png`;
const fixture = (name: string) =>
  readFileSync(fileURLToPath(new URL(`../tests/fixtures/${name}`, import.meta.url)), 'utf8');
export function media(n: number) {
  return {
    pk: PK(n),
    id: `${PK(n)}_9000000001`,
    code: igPkToShortcode(PK(n)),
    media_type: 1,
    taken_at: 1758000000 + n,
    user: { username: 'synthetic_ci_author' },
    caption: { text: `Fixture ${n}` },
    image_versions2: { candidates: [{ url: CDN(n) }] },
  };
}
export const tiles = (n: number) =>
  `<main><h1>Recipes fixture</h1><a href="/p/${igPkToShortcode(PK(n))}/" style="display:block;width:120px;height:120px"><img src="${CDN(n)}" />Fixture</a></main>`;
export interface FixtureState {
  mode: 'normal' | 'offline' | 'blocked' | 'killed';
  replayPages: number;
  unexpected: string[];
  uploadRequests: number;
}
export async function routePlatforms(context: BrowserContext, state: FixtureState) {
  context.on('request', (request) => {
    if (request.url().startsWith(`${ORIGIN}/api/v1/uploads`)) state.uploadRequests++;
  });
  await context.route(/^https?:\/\//, (route) => {
    if (route.request().url().startsWith(`${ORIGIN}/`)) return route.continue();
    state.unexpected.push(route.request().url());
    return route.abort();
  });
  await context.route(
    /https:\/\/(?:[^/]+\.cdninstagram\.com|pbs\.twimg\.com|i\.pinimg\.com)\//,
    (route) => route.fulfill({ contentType: 'image/png', body: PNG }),
  );
  await context.route('https://www.instagram.com/**', (route) => {
    const url = new URL(route.request().url());
    if (url.pathname === '/api/v1/accounts/current_user/')
      return route.fulfill({ json: { user: { username: 'someone' } } });
    const info = /^\/api\/v1\/media\/(\d+)\/info\/$/.exec(url.pathname);
    if (info)
      return route.fulfill({
        json: { items: [media(Number(BigInt(info[1]) - 3500000000000000000n))] },
      });
    if (url.pathname === '/api/v1/feed/saved/posts/') {
      const cursor = Number(url.searchParams.get('max_id') || '1');
      state.replayPages++;
      const ids =
        state.mode === 'blocked'
          ? Array.from({ length: 15 }, (_, n) => 600 + n)
          : state.mode === 'offline'
            ? [801]
            : state.mode === 'killed'
              ? [901]
              : [cursor * 2 - 1, cursor * 2];
      return route.fulfill({
        json: {
          items: ids.map((n) => ({ media: media(n) })),
          more_available: state.mode === 'normal' && cursor < 3,
          next_max_id: cursor < 3 ? String(cursor + 1) : null,
        },
      });
    }
    if (url.pathname === `/api/v1/feed/collection/${FOLDER_ID}/posts/`)
      return route.fulfill({
        json: { items: [201, 202].map((n) => ({ media: media(n) })), more_available: false },
      });
    if (url.pathname === '/someone/saved/')
      return route.fulfill({
        contentType: 'text/html',
        body: `<main><a href="${FOLDER}">Recipes fixture</a></main>`,
      });
    if (url.href === SAVED || url.href === FOLDER)
      return route.fulfill({
        contentType: 'text/html',
        body: `${tiles(url.href === FOLDER ? 201 : 321)}<script>fetch('${url.href === FOLDER ? `/api/v1/feed/collection/${FOLDER_ID}/posts/` : '/api/v1/feed/saved/posts/'}').then(()=>document.title='loaded')</script>`,
      });
    if (url.pathname.startsWith('/p/'))
      return route.fulfill({ contentType: 'text/html', body: tiles(321) });
    return route.fulfill({ contentType: 'text/html', body: '<main>Fixture account</main>' });
  });
  await context.route('https://x.com/**', (route) => {
    const url = new URL(route.request().url());
    if (url.pathname.startsWith('/i/api/graphql/'))
      return route.fulfill({ contentType: 'application/json', body: fixture('x-bookmarks.json') });
    return route.fulfill({
      contentType: 'text/html',
      body: '<main><h1>Bookmarks</h1></main><script>fetch("/i/api/graphql/SYNTH/Bookmarks?variables=%7B%7D").then(()=>document.title="loaded")</script>',
    });
  });
  await context.route('https://www.pinterest.com/**', (route) => {
    const pins = JSON.parse(fixture('pinterest-board-feed.json')).resource_response.data;
    return route.fulfill({
      contentType: 'text/html',
      body: `<main><h1>Recipes fixture</h1></main><script type="application/json" id="__PWS_DATA__">${JSON.stringify({ props: { context: { user: { username: 'someone', is_auth: true } }, initialReduxState: { resources: { BoardFeedResource: { args: { data: pins } } } } } })}</script>`,
    });
  });
}
