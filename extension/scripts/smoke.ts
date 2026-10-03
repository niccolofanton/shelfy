// Real-browser smoke test of the extension (P2-06). It builds the extension for
// http://localhost:<port>, serves a fake Shelfy API there (scripts/fake-api.ts), and answers every
// Instagram, X and Pinterest URL with synthetic pages through Playwright route interception; any
// other request is aborted, so nothing reaches a real platform or server. It checks what unit
// tests cannot: the manifest loads with its fixed ID, the SPA pairs over externally_connectable,
// the MAIN-world hook and the bridge capture on synthetic listings, the worker sends batches with
// the bearer token, holds them in IndexedDB while the API is down — across a browser restart —
// and then sends them once, even when an answer is lost; and the side panel works.
//
//   pnpm exec tsx extension/scripts/smoke.ts [--chrome <binary>] [--headed] [--port 18286]
//
// Branded Google Chrome ignores --load-extension (Chrome 137+): this uses Playwright's Chromium
// (`pnpm exec playwright install chromium`), any Chromium in the Playwright cache, or --chrome.

import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import type { BrowserContext, Page, Route } from 'playwright-core';
import { buildExtension } from '../build';
import { EXTENSION_ID } from '../src/id';
import { igPkToShortcode } from '../src/shared/identity';
import {
  FakeServer,
  check,
  externalMessage,
  extensionWorker,
  htmlPage,
  jsonBody,
  launch as launchChromium,
  openPanel,
  runSmoke,
  sleep,
  stateOf,
  waitFor,
} from './smoke-lib';

const here = dirname(fileURLToPath(import.meta.url));
const fixtures = join(here, '..', 'tests', 'fixtures');
const fixture = (name: string): string => readFileSync(join(fixtures, name), 'utf8');

const { values } = parseArgs({
  options: {
    chrome: { type: 'string' },
    headed: { type: 'boolean' },
    port: { type: 'string' },
  },
  strict: true,
});
const PORT = Number(values.port ?? process.env.SHELFY_SMOKE_PORT ?? 18286);
const ORIGIN = `http://localhost:${PORT}`;

const launch = (profile: string, dist: string): Promise<BrowserContext> =>
  launchChromium(profile, dist, { headed: values.headed, chrome: values.chrome });

const IG_SAVED = 'https://www.instagram.com/someone/saved/all-posts/';
const IG_FOLDER = 'https://www.instagram.com/someone/saved/recipes/17890000000000001/';
const IG_EXPLORE = 'https://www.instagram.com/explore/';
const X_BOOKMARKS = 'https://x.com/i/bookmarks';
const PIN_BOARD = 'https://www.pinterest.com/someone/recipes/';
const PIN_OTHER_BOARD = 'https://www.pinterest.com/someone_else/cakes/';
const FOLDER_KEYS = ['ig_3400000000000000005', 'ig_3400000000000000006', 'ig_3400000000000000007'];

// ── Synthetic platform pages ────────────────────────────────────────────────

const page = htmlPage;
const json = jsonBody;
const fetchThenLoaded = (url: string, init = '{}') =>
  `<script>fetch(${JSON.stringify(url)}, ${init}).then((r) => r.text()).then(() => { document.title = 'loaded'; });</script>`;

/** A synthetic IG REST page (the saved-feed shape) with posts 5, 6, 7. */
function folderFeed(): string {
  const media = (n: number) => ({
    media: {
      id: `340000000000000000${n}_9000000001`,
      pk: `340000000000000000${n}`,
      code: igPkToShortcode(`340000000000000000${n}`),
      media_type: 1,
      taken_at: 1758000000 + n,
      user: { username: 'synthetic_author_b' },
      caption: { text: `Synthetic folder caption ${n}` },
      image_versions2: {
        candidates: [
          { url: `https://scontent-synth1-1.cdninstagram.com/v/folder_${n}.jpg?oe=68F00000` },
        ],
      },
    },
  });
  return JSON.stringify({ items: [5, 6, 7].map(media), more_available: false });
}

function pinterestPage(viewer: string): string {
  const pins = (
    JSON.parse(fixture('pinterest-board-feed.json')) as { resource_response: { data: unknown } }
  ).resource_response.data;
  const data = {
    props: {
      context: { user: { username: viewer, is_auth: true } },
      initialReduxState: { resources: { BoardFeedResource: { args: { data: pins } } } },
    },
  };
  return `<script type="application/json" id="__PWS_DATA__">${JSON.stringify(data)}</script><script>document.title = 'loaded';</script>`;
}

async function servePlatforms(context: BrowserContext, unexpected: string[]): Promise<void> {
  // Registered first, so it runs last: anything not answered below never leaves the browser.
  await context.route(/^https?:\/\//, (route: Route) => {
    const url = route.request().url();
    // The fake server, also under a second origin (127.0.0.1) that is not externally connectable.
    if (url.startsWith(`${ORIGIN}/`) || url.startsWith(`http://127.0.0.1:${PORT}/`))
      return route.continue();
    unexpected.push(url);
    return route.abort();
  });
  await context.route('https://www.instagram.com/**', (route: Route) => {
    const url = new URL(route.request().url());
    if (url.href === IG_SAVED)
      return route.fulfill(page(fetchThenLoaded('/api/v1/feed/saved/posts/?max_id=')));
    if (url.href === IG_FOLDER)
      return route.fulfill(
        page(fetchThenLoaded('/api/v1/feed/collection/17890000000000001/posts/?max_id=')),
      );
    if (url.href === IG_EXPLORE)
      return route.fulfill(
        page(fetchThenLoaded('/graphql/query', "{ method: 'POST', body: 'doc_id=1' }")),
      );
    if (url.pathname === '/api/v1/feed/saved/posts/')
      return route.fulfill(json(fixture('ig-saved-rest-page1.json')));
    if (url.pathname === '/api/v1/feed/collection/17890000000000001/posts/')
      return route.fulfill(json(folderFeed()));
    if (url.pathname === '/graphql/query')
      return route.fulfill(json(fixture('ig-graphql-legacy.json')));
    return route.fulfill({ status: 404, body: '' });
  });
  await context.route('https://x.com/**', (route: Route) => {
    const url = new URL(route.request().url());
    if (url.href === X_BOOKMARKS)
      return route.fulfill(
        page(fetchThenLoaded('/i/api/graphql/SYNTH/Bookmarks?variables=%7B%7D')),
      );
    if (url.pathname.startsWith('/i/api/graphql/'))
      return route.fulfill(json(fixture('x-bookmarks.json')));
    return route.fulfill({ status: 404, body: '' });
  });
  await context.route('https://www.pinterest.com/**', (route: Route) => {
    const url = route.request().url();
    if (url === PIN_BOARD || url === PIN_OTHER_BOARD)
      return route.fulfill(page(pinterestPage('someone')));
    return route.fulfill({ status: 404, body: '' });
  });
}

// ── Helpers on the browser side ─────────────────────────────────────────────

async function visit(context: BrowserContext, url: string, errors: string[]): Promise<Page> {
  const tab = await context.newPage();
  tab.on('console', (m) => {
    if (m.type() === 'error' || m.type() === 'warning') errors.push(`${url}: ${m.text()}`);
  });
  tab.on('pageerror', (e) => errors.push(`${url}: ${e.message}`));
  await tab.goto(url);
  await tab.waitForFunction(() => document.title === 'loaded');
  return tab;
}

// ── The run ─────────────────────────────────────────────────────────────────

async function main(): Promise<void> {
  const work = mkdtempSync(join(tmpdir(), 'shelfy-ext-smoke-'));
  const dist = join(work, 'dist');
  const profile = join(work, 'profile');
  const build = await buildExtension({ origin: ORIGIN, outDir: dist });
  check(
    build.problems.length === 0,
    'the build passes its sanity check',
    build.problems.join('; '),
  );

  const server = new FakeServer(PORT);
  const api = server.api;
  await server.start();
  const errors: string[] = [];
  const unexpected: string[] = [];
  let context = await launch(profile, dist);
  try {
    await servePlatforms(context, unexpected);
    const worker = await extensionWorker(context);
    const id = new URL(worker.url()).host;
    check(id === EXTENSION_ID, 'the extension loads with its fixed ID', id);

    // ── Pairing over externally_connectable (C2, C9) ──
    const spa = await context.newPage();
    await spa.goto(`${ORIGIN}/settings/connections`);
    const ping = (await externalMessage(spa, { type: 'shelfy.ping' })) as Record<string, unknown>;
    check(
      JSON.stringify(ping) ===
        JSON.stringify({
          ok: true,
          version: '0.2.0',
          paired: false,
          outdated: false,
          syncing: { instagram: false, twitter: false, pinterest: false },
        }),
      'shelfy.ping answers the SPA, unpaired',
      JSON.stringify(ping),
    );
    const bad = await externalMessage(spa, { type: 'shelfy.pair', code: 'A'.repeat(43) });
    check(
      JSON.stringify(bad) === '{"ok":false,"code":"invalid_pairing_code"}',
      'an unknown pairing code is refused with the server code',
      JSON.stringify(bad),
    );
    const paired = await externalMessage(spa, {
      type: 'shelfy.pair',
      code: api.issuePairingCode(),
    });
    check(
      JSON.stringify(paired) === '{"ok":true}',
      'shelfy.pair exchanges a fresh code',
      JSON.stringify(paired),
    );
    const after = (await externalMessage(spa, { type: 'shelfy.ping' })) as { paired?: boolean };
    check(after.paired === true, 'shelfy.ping reports the pairing');
    const stranger = await context.newPage();
    await stranger.goto(`http://127.0.0.1:${PORT}/`);
    check(
      (await externalMessage(stranger, { type: 'shelfy.ping' })) === 'no chrome.runtime',
      'a page on another origin cannot message the extension',
    );

    const panel = await openPanel(context, errors);
    check(
      await waitFor(
        async () =>
          (await panel.textContent('[data-testid=pairing-state]'))?.startsWith('Paired') ?? false,
        'the panel to show the pairing',
      ),
      'the side panel shows the pairing',
    );

    // ── Passive capture on synthetic listings ──
    await visit(context, IG_SAVED, errors);
    check(
      await waitFor(
        () => api.posts.has('ig_3400000000000000001') && api.posts.has('ig_3400000000000000002'),
        'IG items',
      ),
      'passive batches from IG saved reach the API',
    );
    const ingest = api.log.find((r) => r.path === '/api/v1/ingest/batches' && r.status === 200);
    check(
      !!ingest &&
        /^Bearer shx_[A-Za-z0-9_-]{43}$/.test(ingest.authorization ?? '') &&
        ingest.extension === '0.2.0' &&
        /^[0-9A-HJKMNP-TV-Z]{26}$/.test(ingest.idempotencyKey ?? ''),
      'they carry the paired bearer token, X-Shelfy-Extension and a ULID Idempotency-Key',
    );
    const igRun = [...api.runs.values()].find((run) => run.listing.kind === 'ig_saved');
    check(
      igRun?.trigger === 'passive',
      'the listing visit opened a passive sync run',
      JSON.stringify(igRun?.listing),
    );
    const caption = [...api.batchItems.values()]
      .flat()
      .some((item) => (item as { text?: string }).text === 'Synthetic caption A');
    check(caption, 'captions travel with the items (P2-G15)');

    await visit(context, X_BOOKMARKS, errors);
    await visit(context, PIN_BOARD, errors);
    check(
      await waitFor(
        () =>
          [
            'x_1800000000000000001',
            'x_1800000000000000002',
            'pin_900000000000000001',
            'pin_900000000000000002',
          ].every((key) => api.posts.has(key)),
        'X and Pinterest items',
      ),
      "X bookmarks and the user's own Pinterest board reach the API",
    );
    await visit(context, IG_EXPLORE, errors);
    await visit(context, PIN_OTHER_BOARD, errors);
    await sleep(3_000);
    let state = await stateOf(panel);
    check(
      state.queue.discarded.out_of_scope >= 2 &&
        state.queue.discarded.not_own_board >= 2 &&
        ![...api.runs.values()].some((run) => run.listing.externalId === 'someone_else/cakes') &&
        !api.posts.has('ig_3400000000000000004'),
      "pages outside saved listings and another user's board are discarded",
      JSON.stringify(state.queue.discarded),
    );

    // ── The API goes down: the queue holds, across a browser restart ──
    await server.stop();
    const sentBefore = api.ingests.length;
    await visit(context, IG_FOLDER, errors);
    check(
      await waitFor(async () => {
        const s = await stateOf(panel);
        return s.queue.queuedItems >= 3 && s.lastError?.code === 'network';
      }, 'the queue to hold the folder items'),
      'with the API down the queue holds the batch and records the network error',
    );
    check(api.ingests.length === sentBefore, 'nothing was sent while the API was down');

    await context.close();
    context = await launch(profile, dist);
    await servePlatforms(context, unexpected);
    await extensionWorker(context);
    const panel2 = await openPanel(context, errors);
    state = await stateOf(panel2);
    check(
      state.queue.queuedItems >= 3,
      'the queue survives a browser restart',
      String(state.queue.queuedItems),
    );

    // ── The API is back: the first answer is lost, the retry is a replay ──
    await server.start();
    api.loseNextIngestResponse = true;
    await panel2.click('[data-testid=queue-retry]');
    check(
      await waitFor(
        () => FOLDER_KEYS.every((key) => api.posts.has(key)),
        'the folder items to arrive',
      ),
      'once the API is back, "Retry now" sends the held batch',
    );
    await waitFor(
      async () => (await stateOf(panel2)).lastError?.code === 'server',
      'the lost answer',
    );
    await panel2.click('[data-testid=queue-retry]');
    check(
      await waitFor(
        async () => (await stateOf(panel2)).queue.queuedItems === 0,
        'the queue to drain',
      ),
      'the retry drains the queue',
    );
    const folderRun = [...api.runs.values()].find(
      (run) => run.listing.externalId === '17890000000000001',
    );
    const folderIngests = api.ingests.filter((i) => i.runId === folderRun?.id);
    check(
      FOLDER_KEYS.every((key) => api.posts.get(key) === 1) &&
        folderIngests.length === 2 &&
        folderIngests[0].key === folderIngests[1].key &&
        folderIngests[1].replayed,
      'the batch whose answer was lost went again under the same key and was ingested once',
      JSON.stringify(
        folderIngests.map((i) => ({ replayed: i.replayed, same: i.key === folderIngests[0]?.key })),
      ),
    );
    check(
      await waitFor(
        () =>
          [...api.runs.values()].some(
            (run) => run.listing.externalId === '17890000000000001' && run.state === 'done',
          ),
        'the folder run to close',
      ),
      'the folder visit opened a run, which closed after its last batch',
    );

    // ── Check connection, Access headers, toggles, kill switch ──
    await panel2.click('[data-testid=check-connection]');
    await panel2.waitForFunction(
      () => document.querySelectorAll('[data-testid=connection-results] li').length >= 3,
    );
    const lines = await panel2.$$eval('[data-testid=connection-results] li', (items) =>
      items.map((li) => li.textContent ?? ''),
    );
    check(
      lines.includes('Browser cookie: Shelfy answered') &&
        lines.includes('Extension token: accepted'),
      '"Check connection" reports cookie mode and the token',
      lines.join(' | '),
    );
    await panel2.fill('[data-testid=access-id]', 'smoke-client.access');
    await panel2.fill('[data-testid=access-secret]', 'smoke-not-a-secret');
    await panel2.click('[data-testid=access-save]');
    await panel2.waitForFunction(
      () => document.querySelector('[data-testid=access-state]')?.textContent === 'Headers saved',
    );
    const logged = api.log.length;
    await panel2.click('[data-testid=check-connection]');
    check(
      await waitFor(
        () => api.log.slice(logged).some((r) => r.accessId === 'smoke-client.access'),
        'a request with the Access headers',
      ),
      'saved Access headers go out with API requests',
    );
    check(
      (await panel2.inputValue('[data-testid=access-secret]')) === '',
      'the panel never shows the saved secret again',
    );
    await panel2.click('[data-testid=access-clear]');
    await panel2.waitForFunction(
      () =>
        document.querySelector('[data-testid=access-state]')?.textContent === 'No headers saved',
    );

    await panel2.click('[data-testid=passive-twitter]');
    await waitFor(async () => !(await stateOf(panel2)).passive.twitter, 'the X toggle');
    const xRuns = [...api.runs.values()].filter((run) => run.platform === 'twitter').length;
    await visit(context, X_BOOKMARKS, errors);
    await sleep(3_000);
    state = await stateOf(panel2);
    check(
      state.queue.discarded.disabled >= 2 &&
        [...api.runs.values()].filter((run) => run.platform === 'twitter').length === xRuns,
      'a passive toggle turned off stops capture on that platform',
    );

    api.killed.add('pinterest');
    api.bumpConfig();
    await visit(context, PIN_BOARD, errors);
    check(
      await waitFor(
        async () => !(await stateOf(panel2)).serverPassive.pinterest,
        'the kill switch',
      ),
      'a killed source (409) makes the worker refresh its config and turn the platform off',
    );
    await visit(context, PIN_BOARD, errors);
    check(
      await waitFor(
        async () => (await stateOf(panel2)).queue.discarded.killed >= 2,
        'a killed capture',
      ),
      'the next capture on the killed platform is discarded',
    );

    check(
      errors.length === 0,
      'no console errors or warnings in pages and the panel',
      errors.join(' | '),
    );
    check(unexpected.length === 0, 'no request left the browser', unexpected.join(' '));
  } finally {
    await context.close().catch(() => undefined);
    await server.stop();
    rmSync(work, { recursive: true, force: true });
  }
}

runSmoke(main);
