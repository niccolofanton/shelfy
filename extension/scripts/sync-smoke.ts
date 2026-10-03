// Real-browser smoke test of the sync controller (P2-13). It builds the extension for
// http://localhost:<port>, serves the fake Shelfy API there (scripts/fake-api.ts), and answers
// Instagram, X and Pinterest with synthetic pages through Playwright route interception; any
// other request is aborted. It starts syncs from the side panel's "Sync now" and checks the
// walks end to end:
//
// - IG folder: the gated REST replay from the first page to the end of the feed, filed into the
//   folder's collection under the page heading; the scroll the config killed is skipped and
//   reported; the run's accepted keys are kept in IndexedDB;
// - IG incremental: stops at the first page boundary once the known run is reached;
// - IG page cap and resume (P2-G2): a capped walk reports its cursor; the next walks read the
//   known head, jump to the cursor and continue, until the end of the feed;
// - X bookmarks and a Pinterest board: the two-pass scroll fetches pages until
//   `hasNextPage === false` (Pinterest after its inline first page, with "no collection");
// - the panel's Stop, a login wall, and a tab closed mid-sync end the run on the server.
//
//   pnpm exec tsx extension/scripts/sync-smoke.ts [--chrome <binary>] [--headed] [--port 18293]

import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { parseArgs } from 'node:util';
import type { BrowserContext, Page, Route } from 'playwright-core';
import { buildExtension } from '../build';
import { igPkToShortcode } from '../src/shared/identity';
import type { FakeRun } from './fake-api';
import {
  FakeServer,
  check,
  externalMessage,
  extensionWorker,
  htmlPage,
  jsonBody,
  launch,
  openPanel,
  runSmoke,
  stateOf,
  waitFor,
} from './smoke-lib';

const { values } = parseArgs({
  options: {
    chrome: { type: 'string' },
    headed: { type: 'boolean' },
    port: { type: 'string' },
  },
  strict: true,
});
const PORT = Number(values.port ?? process.env.SHELFY_SYNC_SMOKE_PORT ?? 18293);
const ORIGIN = `http://localhost:${PORT}`;

const IG_FOLDER_ID = '17890000000000002';
const IG_FOLDER = `https://www.instagram.com/someone/saved/recipes/${IG_FOLDER_ID}/`;
const IG_SAVED = 'https://www.instagram.com/someone/saved/all-posts/';
const X_BOOKMARKS = 'https://x.com/i/bookmarks';
const PIN_BOARD = 'https://www.pinterest.com/someone/weekend-board/';

// ── Synthetic Instagram ─────────────────────────────────────────────────────

const igPk = (n: number): string => `35000000000${String(n).padStart(8, '0')}`;

function igMedia(n: number) {
  const pk = igPk(n);
  return {
    media: {
      id: `${pk}_9000000002`,
      pk,
      code: igPkToShortcode(pk),
      media_type: 1,
      taken_at: 1758100000 + n,
      user: { username: 'synthetic_sync_author' },
      caption: { text: `Synthetic sync caption ${n}` },
      image_versions2: {
        candidates: [{ url: `https://scontent-synth1-1.cdninstagram.com/v/sync_${n}.jpg` }],
      },
    },
  };
}

/** A feed: pages keyed by the max_id that requests them ('' = the first page). */
type Feed = Record<string, { posts: number[]; next: string | null }>;

const FOLDER_FEED: Feed = {
  '': { posts: [1, 2, 3, 4], next: 'C1' },
  C1: { posts: [5, 6, 7], next: null },
};
const SAVED_FEED: Feed = {
  '': { posts: [21, 22, 23, 24], next: 'S1' },
  S1: { posts: [25, 26, 27, 28], next: 'S2' },
  S2: { posts: [29, 30, 31], next: 'S3' },
  S3: { posts: [32, 33], next: null },
};

function feedPage(feed: Feed, maxId: string): string {
  const page = feed[maxId] ?? { posts: [], next: null };
  return JSON.stringify({
    items: page.posts.map(igMedia),
    more_available: page.next !== null,
    ...(page.next ? { next_max_id: page.next } : {}),
  });
}

const tiles = (count: number, tag: string, attrs: string) =>
  Array.from(
    { length: count },
    () => `<${tag} ${attrs} style="display:block;height:300px"></${tag}>`,
  ).join('');

function igListingPage(heading: string): string {
  return `<main><h1>${heading}</h1>${tiles(12, 'a', 'href="/p/synthetic/"')}</main><script>document.title = 'loaded';</script>`;
}

// ── Synthetic X ─────────────────────────────────────────────────────────────

/** `finite`: three pages then the end; `infinite`: pages forever; `login`: a login redirect. */
let xMode: 'finite' | 'infinite' | 'login' = 'finite';

function xTweet(id: string) {
  return {
    entryId: `tweet-${id}`,
    content: {
      itemContent: {
        tweet_results: {
          result: {
            rest_id: id,
            core: {
              user_results: {
                result: {
                  core: { screen_name: 'synthetic_sync_x', name: 'Synthetic' },
                  legacy: {},
                },
              },
            },
            legacy: {
              full_text: `Synthetic bookmark ${id}`,
              created_at: 'Wed Sep 17 10:00:00 +0000 2025',
              extended_entities: {
                media: [
                  {
                    type: 'photo',
                    media_url_https: `https://pbs.twimg.com/media/SyntheticSync${id.slice(-4)}.jpg`,
                  },
                ],
              },
            },
          },
        },
      },
    },
  };
}

const xId = (page: number, n: number): string =>
  `18100000000${String(page).padStart(4, '0')}${String(n).padStart(4, '0')}`;

function xPage(cursor: string): string {
  const page = cursor === '' ? 1 : Number(cursor.replace('X', ''));
  const last = xMode !== 'infinite' && page >= 3;
  const entries: unknown[] = Array.from({ length: 5 }, (_, n) => xTweet(xId(page, n + 1)));
  entries.push({
    entryId: `cursor-bottom-${page}`,
    content: { cursorType: 'Bottom', value: last ? '' : `X${page + 1}` },
  });
  return JSON.stringify({
    data: {
      bookmark_timeline_v2: {
        timeline: { instructions: [{ type: 'TimelineAddEntries', entries }] },
      },
    },
  });
}

/** Loads a page of bookmarks at start and whenever the scroll nears the bottom. */
const X_SCRIPT = `
  let cursor = '', loading = false, done = false, redirected = false;
  const feed = document.getElementById('feed');
  async function load() {
    if (loading || done) return;
    loading = true;
    const r = await fetch('/i/api/graphql/SYNTH/Bookmarks?variables=' + encodeURIComponent(JSON.stringify({ cursor })));
    const j = await r.json();
    const entries = j.data.bookmark_timeline_v2.timeline.instructions[0].entries;
    for (const entry of entries) {
      if (entry.content.cursorType === 'Bottom') { cursor = entry.content.value; done = !cursor; continue; }
      const card = document.createElement('article');
      card.setAttribute('data-testid', 'tweet');
      card.style.cssText = 'display:block;height:400px';
      feed.append(card);
    }
    loading = false;
    document.title = 'loaded';
  }
  window.addEventListener('scroll', () => {
    if (window.__loginOnScroll && !redirected) {
      redirected = true;
      history.pushState({}, '', '/i/flow/login');
      return;
    }
    if (window.innerHeight + window.scrollY > document.body.scrollHeight - 600) load();
  });
  load();
`;

function xBookmarksPage(): string {
  const login = xMode === 'login' ? '<script>window.__loginOnScroll = true;</script>' : '';
  return `<div id="feed"></div>${login}<script>${X_SCRIPT}</script>`;
}

// ── Synthetic Pinterest ─────────────────────────────────────────────────────

const pin = (n: number) => ({
  id: `90000000000000${String(n).padStart(4, '0')}`,
  type: 'pin',
  title: `Synthetic sync pin ${n}`,
  images: { '736x': { url: `https://i.pinimg.com/736x/aa/bb/cc/sync${n}.jpg` } },
  pinner: { username: 'synthetic_pinner', full_name: 'Synthetic Pinner' },
});

function pinterestBoardPage(): string {
  const data = {
    props: {
      context: { user: { username: 'someone', is_auth: true } },
      initialReduxState: {
        resources: { BoardFeedResource: { args: { data: [pin(1), pin(2)] } } },
      },
    },
  };
  const script = `
    let fetched = false;
    window.addEventListener('scroll', () => {
      if (fetched || window.innerHeight + window.scrollY < document.body.scrollHeight - 600) return;
      fetched = true;
      fetch('/resource/BoardFeedResource/get/?data=%7B%7D').then((r) => r.text());
    });
    document.title = 'loaded';`;
  return (
    `<script type="application/json" id="__PWS_DATA__">${JSON.stringify(data)}</script>` +
    `<main><h1>Weekend Board</h1>${tiles(10, 'div', 'data-test-id="pin"')}</main><script>${script}</script>`
  );
}

const PIN_FEED_PAGE = JSON.stringify({
  resource_response: { data: [pin(3), pin(4)], bookmark: '-end-' },
  resource: { options: { bookmarks: ['-end-'] } },
});

// ── Routing ─────────────────────────────────────────────────────────────────

const igRequests: string[] = [];

async function servePlatforms(context: BrowserContext, unexpected: string[]): Promise<void> {
  await context.route(/^https?:\/\//, (route: Route) => {
    const url = route.request().url();
    if (url.startsWith(`${ORIGIN}/`)) return route.continue();
    unexpected.push(url);
    return route.abort();
  });
  await context.route('https://www.instagram.com/**', (route: Route) => {
    const url = new URL(route.request().url());
    if (url.href === IG_FOLDER) return route.fulfill(htmlPage(igListingPage('Recipes Folder')));
    if (url.href === IG_SAVED) return route.fulfill(htmlPage(igListingPage('Saved')));
    const maxId = url.searchParams.get('max_id') ?? '';
    if (url.pathname === `/api/v1/feed/collection/${IG_FOLDER_ID}/posts/`) {
      igRequests.push(`folder:${maxId}`);
      return route.fulfill(jsonBody(feedPage(FOLDER_FEED, maxId)));
    }
    if (url.pathname === '/api/v1/feed/saved/posts/') {
      igRequests.push(`saved:${maxId}`);
      return route.fulfill(jsonBody(feedPage(SAVED_FEED, maxId)));
    }
    return route.fulfill({ status: 404, body: '' });
  });
  await context.route('https://x.com/**', (route: Route) => {
    const url = new URL(route.request().url());
    if (url.pathname === '/i/bookmarks') return route.fulfill(htmlPage(xBookmarksPage()));
    if (url.pathname.startsWith('/i/api/graphql/')) {
      const variables = JSON.parse(url.searchParams.get('variables') ?? '{}') as {
        cursor?: string;
      };
      return route.fulfill(jsonBody(xPage(variables.cursor ?? '')));
    }
    return route.fulfill({ status: 404, body: '' });
  });
  await context.route('https://www.pinterest.com/**', (route: Route) => {
    const url = new URL(route.request().url());
    if (url.href === PIN_BOARD) return route.fulfill(htmlPage(pinterestBoardPage()));
    if (url.pathname === '/resource/BoardFeedResource/get/')
      return route.fulfill(jsonBody(PIN_FEED_PAGE));
    return route.fulfill({ status: 404, body: '' });
  });
}

// ── Helpers ─────────────────────────────────────────────────────────────────

async function openTab(context: BrowserContext, url: string, errors: string[]): Promise<Page> {
  const tab = await context.newPage();
  tab.on('console', (m) => {
    if (m.type() === 'error' || m.type() === 'warning') errors.push(`${url}: ${m.text()}`);
  });
  tab.on('pageerror', (e) => errors.push(`${url}: ${e.message}`));
  await tab.goto(url);
  await tab.waitForFunction(() => document.title === 'loaded');
  return tab;
}

const text = (panel: Page, testId: string): Promise<string> =>
  panel.evaluate((id) => document.querySelector(`[data-testid=${id}]`)?.textContent ?? '', testId);

/** Brings `tab` to the front, waits for the panel to offer it, and clicks "Sync now". */
async function syncFromPanel(
  panel: Page,
  tab: Page,
  listing: string,
  before?: (panel: Page) => Promise<void>,
): Promise<boolean> {
  await tab.bringToFront();
  const ready = await waitFor(
    () =>
      panel.evaluate(
        (expected) =>
          (document.querySelector('[data-testid=sync-tab]')?.textContent ?? '').includes(
            expected,
          ) &&
          !(document.querySelector('[data-testid=sync-start]') as HTMLButtonElement | null)
            ?.disabled,
        listing,
      ),
    `the panel to offer ${listing}`,
  );
  if (!ready) return false;
  await before?.(panel);
  // A click by script: the panel is a background tab while the platform tab is in front.
  await panel.evaluate(() =>
    (document.querySelector('[data-testid=sync-start]') as HTMLButtonElement).click(),
  );
  return true;
}

/** The newest run with a PATCH that ended it, after `count` runs existed. */
async function endedRun(server: FakeServer, after: number, what: string): Promise<FakeRun | null> {
  let found: FakeRun | null = null;
  await waitFor(
    () => {
      const runs = [...server.api.runs.values()].slice(after);
      found = runs.find((run) => run.trigger === 'manual' && run.state !== 'running') ?? null;
      return found !== null;
    },
    what,
    60_000,
  );
  return found;
}

const patchOf = (server: FakeServer, run: FakeRun | null): Record<string, unknown> | null =>
  (server.api.patches.filter((p) => p.id === run?.id).pop()?.body as Record<string, unknown>) ??
  null;

const ingestsOf = (server: FakeServer, run: FakeRun | null) =>
  server.api.ingests.filter((ingest) => ingest.runId === run?.id && !ingest.replayed);

// ── The run ─────────────────────────────────────────────────────────────────

async function main(): Promise<void> {
  const work = mkdtempSync(join(tmpdir(), 'shelfy-sync-smoke-'));
  const dist = join(work, 'dist');
  const build = await buildExtension({ origin: ORIGIN, outDir: dist });
  check(
    build.problems.length === 0,
    'the build passes its sanity check',
    build.problems.join('; '),
  );

  const server = new FakeServer(PORT);
  const api = server.api;
  // The IG scroll is killed (skipped and reported); a page cap of 2 makes the resume testable.
  api.platformConfig.instagram = { scroll: false, replayMaxPages: 2 };
  await server.start();
  const errors: string[] = [];
  const unexpected: string[] = [];
  const context = await launch(join(work, 'profile'), dist, {
    headed: values.headed,
    chrome: values.chrome,
  });
  try {
    await servePlatforms(context, unexpected);
    const worker = await extensionWorker(context);
    const spa = await context.newPage();
    await spa.goto(`${ORIGIN}/settings/connections`);
    const paired = await externalMessage(spa, {
      type: 'shelfy.pair',
      code: api.issuePairingCode(),
    });
    check(JSON.stringify(paired) === '{"ok":true}', 'the extension pairs', JSON.stringify(paired));
    const panel = await openPanel(context, errors);

    // ── IG folder: the gated replay to the end of the feed ──
    const folderTab = await openTab(context, IG_FOLDER, errors);
    let runs = api.runs.size;
    check(
      await syncFromPanel(panel, folderTab, 'folder', async (p) => {
        const name = await p.inputValue('[data-testid=sync-name]');
        check(
          name === 'Recipes Folder',
          'the chooser names a new folder from the page heading',
          name,
        );
      }),
      'the panel offers "Sync now" on an IG folder',
    );
    check(
      await waitFor(async () => {
        const ping = (await externalMessage(spa, { type: 'shelfy.ping' })) as {
          syncing?: { instagram?: boolean };
        };
        return ping.syncing?.instagram === true;
      }, 'ping to report the sync'),
      'shelfy.ping reports Instagram syncing while the walk runs',
    );
    let run = await endedRun(server, runs, 'the folder sync to end');
    let patch = patchOf(server, run);
    check(
      run?.listing.kind === 'ig_collection' &&
        run.listing.externalId === IG_FOLDER_ID &&
        run.listing.name === 'Recipes Folder' &&
        JSON.stringify(run.collection) === '{"mode":"auto"}',
      'the run walks the folder into its collection (auto, named from the heading)',
      JSON.stringify({ listing: run?.listing, collection: run?.collection }),
    );
    check(
      JSON.stringify(igRequests) === JSON.stringify(['folder:', 'folder:C1']),
      'the replay reads the first page and follows the cursor',
      igRequests.join(' '),
    );
    check(
      [1, 2, 3, 4, 5, 6, 7].every((n) => api.posts.has(`ig_${igPk(n)}`)) &&
        ingestsOf(server, run).every((ingest) => ingest.source === 'replay'),
      'every post of the folder arrives in batches tagged replay',
    );
    check(
      patch?.state === 'done' &&
        patch.stopReason === 'end_of_feed' &&
        patch.resumeCursor === null &&
        patch.scanned === 7,
      'the run closes done at the end of the feed',
      JSON.stringify(patch),
    );
    check(
      await waitFor(
        async () => (await text(panel, 'sync-last')).includes('reached the end of the list'),
        'the panel result',
      ),
      'the panel shows the result',
      await text(panel, 'sync-last'),
    );
    check(
      (await text(panel, 'sync-skipped')).includes('scrolling'),
      'the killed scroll is skipped and reported',
      await text(panel, 'sync-skipped'),
    );
    const keys = await worker.evaluate(
      () =>
        new Promise<number>((resolve) => {
          const open = indexedDB.open('shelfy');
          open.onsuccess = () => {
            const request = open.result.transaction('runKeys').objectStore('runKeys').getAll();
            request.onsuccess = () =>
              resolve((request.result as Array<{ keys: string[] }>).flatMap((r) => r.keys).length);
          };
        }),
    );
    check(keys >= 7, "the run's accepted keys are kept in IndexedDB", String(keys));

    // ── IG incremental: the known run stops the replay at the first boundary ──
    igRequests.length = 0;
    runs = api.runs.size;
    api.nextRunAnswer = { incremental: true, stopAfterKnown: 3 };
    await syncFromPanel(panel, folderTab, 'folder');
    run = await endedRun(server, runs, 'the incremental sync to end');
    patch = patchOf(server, run);
    check(
      patch?.stopReason === 'known_run' &&
        patch.state === 'done' &&
        JSON.stringify(igRequests) === JSON.stringify(['folder:']),
      'an incremental run stops after its first page of known posts',
      `${JSON.stringify(patch)} ${igRequests.join(' ')}`,
    );

    // ── IG page cap and resume (P2-G2) ──
    const savedTab = await openTab(context, IG_SAVED, errors);
    igRequests.length = 0;
    runs = api.runs.size;
    await syncFromPanel(panel, savedTab, 'saved');
    run = await endedRun(server, runs, 'the capped sync to end');
    patch = patchOf(server, run);
    check(
      patch?.stopReason === 'page_cap' &&
        patch.resumeCursor === 'S2' &&
        JSON.stringify(igRequests) === JSON.stringify(['saved:', 'saved:S1']),
      'a capped replay reports the cursor where it stopped',
      `${JSON.stringify(patch)} ${igRequests.join(' ')}`,
    );
    for (const [cursor, next, expected] of [
      ['S2', 'S3', 'page_cap'],
      ['S3', null, 'end_of_feed'],
    ] as const) {
      igRequests.length = 0;
      runs = api.runs.size;
      api.nextRunAnswer = { incremental: false, resumeCursor: cursor, stopAfterKnown: 3 };
      await syncFromPanel(panel, savedTab, 'saved');
      run = await endedRun(server, runs, `the resumed sync from ${cursor} to end`);
      patch = patchOf(server, run);
      check(
        patch?.stopReason === expected &&
          patch.resumeCursor === next &&
          JSON.stringify(igRequests) === JSON.stringify(['saved:', `saved:${cursor}`]),
        `a resumed walk reads the known head, then continues from ${cursor} (${expected})`,
        `${JSON.stringify(patch)} ${igRequests.join(' ')}`,
      );
    }
    check(
      [21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33].every((n) =>
        api.posts.has(`ig_${igPk(n)}`),
      ),
      'the walk spanning three runs saved the whole feed',
    );

    // ── X bookmarks: the scroll to the end of the feed ──
    xMode = 'finite';
    const xTab = await openTab(context, X_BOOKMARKS, errors);
    runs = api.runs.size;
    await syncFromPanel(panel, xTab, 'bookmarks');
    run = await endedRun(server, runs, 'the X sync to end');
    patch = patchOf(server, run);
    check(
      patch?.stopReason === 'end_of_feed' &&
        patch.state === 'done' &&
        [2, 3].every((page) => api.posts.has(`x_${xId(page, 1)}`)) &&
        ingestsOf(server, run).length > 0 &&
        ingestsOf(server, run).every((ingest) => ingest.source === 'scroll'),
      'the X scroll loads pages until hasNextPage is false',
      JSON.stringify(patch),
    );

    // ── Pinterest: the inline first page, then the scroll, into no collection ──
    const pinTab = await openTab(context, PIN_BOARD, errors);
    runs = api.runs.size;
    await syncFromPanel(panel, pinTab, 'board', async (p) => {
      await p.evaluate(() =>
        (document.querySelector('[data-testid=sync-into-none]') as HTMLInputElement).click(),
      );
    });
    run = await endedRun(server, runs, 'the Pinterest sync to end');
    patch = patchOf(server, run);
    const pinIngests = ingestsOf(server, run);
    check(
      patch?.stopReason === 'end_of_feed' &&
        JSON.stringify(run?.collection) === '{"mode":"none"}' &&
        api.posts.has('pin_900000000000000003') &&
        pinIngests.reduce((sum, ingest) => sum + ingest.count, 0) >= 4,
      'the Pinterest sync reads the inline page and scrolls to the end, into no collection',
      JSON.stringify({
        patch,
        collection: run?.collection,
        counts: pinIngests.map((i) => i.count),
      }),
    );

    // ── Stop from the panel ──
    xMode = 'infinite';
    const xInfinite = await openTab(context, X_BOOKMARKS, errors);
    runs = api.runs.size;
    await syncFromPanel(panel, xInfinite, 'bookmarks');
    await waitFor(
      () =>
        [...api.runs.values()].slice(runs).some((r) => r.trigger === 'manual' && r.batches >= 1),
      'a scrolled page',
      30_000,
    );
    await waitFor(
      async () =>
        !(await panel.evaluate(
          () => (document.querySelector('[data-testid=sync-stop]') as HTMLElement).hidden,
        )),
      'the stop button',
    );
    await panel.evaluate(() =>
      (document.querySelector('[data-testid=sync-stop]') as HTMLButtonElement).click(),
    );
    run = await endedRun(server, runs, 'the stopped sync to end');
    patch = patchOf(server, run);
    check(
      patch?.state === 'stopped' && patch.stopReason === 'user',
      'Stop in the panel ends the run as stopped',
      JSON.stringify(patch),
    );

    // ── A tab closed mid-sync ──
    runs = api.runs.size;
    await syncFromPanel(panel, xInfinite, 'bookmarks');
    await waitFor(
      () =>
        [...api.runs.values()].slice(runs).some((r) => r.trigger === 'manual' && r.batches >= 1),
      'a scrolled page',
      30_000,
    );
    await xInfinite.close();
    run = await endedRun(server, runs, 'the closed tab to end its sync');
    patch = patchOf(server, run);
    check(
      patch?.state === 'stopped' && patch.stopReason === 'user',
      'closing the tab ends the run from the worker',
      JSON.stringify(patch),
    );

    // ── A login wall ──
    xMode = 'login';
    const xLogin = await openTab(context, X_BOOKMARKS, errors);
    runs = api.runs.size;
    await syncFromPanel(panel, xLogin, 'bookmarks');
    run = await endedRun(server, runs, 'the login wall to end the sync');
    patch = patchOf(server, run);
    check(
      patch?.state === 'failed' && patch.stopReason === 'login_required',
      'a login wall ends the run as login_required',
      JSON.stringify(patch),
    );

    const state = await stateOf(panel);
    check(
      !state.syncs.some((view) => view.state === 'open'),
      'no sync is left open',
      JSON.stringify(state.syncs.map((v) => [v.platform, v.state, v.stopReason])),
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
