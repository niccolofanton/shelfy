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

import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import { homedir, platform as osPlatform, tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { chromium, type BrowserContext, type Page, type Route, type Worker } from 'playwright-core';
import { buildExtension } from '../build';
import { EXTENSION_ID } from '../src/id';
import { igPkToShortcode } from '../src/shared/identity';
import { MSG } from '../src/shared/protocol';
import type { PanelState } from '../src/sw/state';
import { FakeShelfyApi } from './fake-api';

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

const IG_SAVED = 'https://www.instagram.com/someone/saved/all-posts/';
const IG_FOLDER = 'https://www.instagram.com/someone/saved/recipes/17890000000000001/';
const IG_EXPLORE = 'https://www.instagram.com/explore/';
const X_BOOKMARKS = 'https://x.com/i/bookmarks';
const PIN_BOARD = 'https://www.pinterest.com/someone/recipes/';
const PIN_OTHER_BOARD = 'https://www.pinterest.com/someone_else/cakes/';
const FOLDER_KEYS = ['ig_3400000000000000005', 'ig_3400000000000000006', 'ig_3400000000000000007'];

let failures = 0;
function check(condition: boolean, what: string, detail = ''): void {
  if (!condition) failures += 1;
  console.log(`${condition ? 'ok  ' : 'FAIL'} - ${what}${detail ? ` (${detail})` : ''}`);
}

async function waitFor(
  condition: () => boolean | Promise<boolean>,
  what: string,
  timeoutMs = 20_000,
): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await condition()) return true;
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  console.log(`     (timed out after ${timeoutMs} ms waiting for ${what})`);
  return false;
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

// ── Chromium ─────────────────────────────────────────────────────────────────

const CHROME_BINARIES = [
  'chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
  'chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
  'chrome-mac/Chromium.app/Contents/MacOS/Chromium',
  'chrome-linux64/chrome',
  'chrome-linux/chrome',
  'chrome-win64/chrome.exe',
  'chrome-win/chrome.exe',
];

/** --chrome, else Playwright's own Chromium, else the newest full Chromium in its cache. */
function findChromium(): string | undefined {
  if (values.chrome) return values.chrome;
  const bundled = chromium.executablePath();
  if (existsSync(bundled)) return bundled;
  const cache =
    process.env.PLAYWRIGHT_BROWSERS_PATH ??
    (osPlatform() === 'darwin'
      ? join(homedir(), 'Library', 'Caches', 'ms-playwright')
      : osPlatform() === 'win32'
        ? join(homedir(), 'AppData', 'Local', 'ms-playwright')
        : join(homedir(), '.cache', 'ms-playwright'));
  if (!existsSync(cache)) return undefined;
  const builds = readdirSync(cache)
    .filter((name) => /^chromium-\d+$/.test(name))
    .sort((a, b) => Number(b.slice(9)) - Number(a.slice(9)));
  for (const build of builds)
    for (const binary of CHROME_BINARIES)
      if (existsSync(join(cache, build, binary))) return join(cache, build, binary);
  return undefined;
}

async function launch(profile: string, dist: string): Promise<BrowserContext> {
  return chromium.launchPersistentContext(profile, {
    headless: !values.headed,
    executablePath: findChromium(),
    args: [
      `--disable-extensions-except=${dist}`,
      `--load-extension=${dist}`,
      '--disable-background-networking',
      '--disable-component-update',
      '--no-first-run',
    ],
  });
}

async function extensionWorker(context: BrowserContext): Promise<Worker> {
  return (
    context.serviceWorkers().find((w) => w.url().startsWith('chrome-extension://')) ??
    (await context.waitForEvent('serviceworker', {
      predicate: (w) => w.url().startsWith('chrome-extension://'),
    }))
  );
}

// ── The fake Shelfy server ──────────────────────────────────────────────────

const SPA_PAGE =
  '<!doctype html><html><head><title>Shelfy (fake)</title></head><body>fake SPA</body></html>';

class FakeServer {
  readonly api = new FakeShelfyApi();
  private server: Server | null = null;

  async start(): Promise<void> {
    this.server = createServer((req, res) => void this.serve(req, res));
    await new Promise<void>((resolve, reject) => {
      this.server?.once('error', reject);
      this.server?.listen(PORT, '127.0.0.1', () => resolve());
    });
  }

  async stop(): Promise<void> {
    const server = this.server;
    this.server = null;
    if (!server) return;
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }

  private async serve(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const chunks: Buffer[] = [];
    for await (const chunk of req) chunks.push(chunk as Buffer);
    const path = req.url ?? '/';
    if (req.method === 'GET' && (path === '/' || path.startsWith('/settings'))) {
      res.writeHead(200, { 'content-type': 'text/html' }).end(SPA_PAGE);
      return;
    }
    const headers: Record<string, string> = {};
    for (const [name, value] of Object.entries(req.headers))
      if (typeof value === 'string') headers[name.toLowerCase()] = value;
    const result = this.api.handle({
      method: req.method ?? 'GET',
      path,
      headers,
      body: chunks.length ? Buffer.concat(chunks).toString('utf8') : null,
    });
    res.writeHead(result.status, result.headers).end(result.body);
  }
}

// ── Synthetic platform pages ────────────────────────────────────────────────

const page = (body: string, head = '') => ({
  status: 200,
  contentType: 'text/html',
  body: `<!doctype html><html><head>${head}</head><body>${body}</body></html>`,
});
const json = (body: string) => ({ status: 200, contentType: 'application/json', body });
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

async function openPanel(context: BrowserContext, errors: string[]): Promise<Page> {
  const panel = await context.newPage();
  panel.on('console', (m) => {
    if (m.type() === 'error') errors.push(`panel: ${m.text()}`);
  });
  panel.on('pageerror', (e) => errors.push(`panel: ${e.message}`));
  await panel.goto(`chrome-extension://${EXTENSION_ID}/panel.html`);
  await panel.waitForSelector('[data-testid=pairing-state]');
  return panel;
}

const stateOf = (panel: Page): Promise<PanelState> =>
  panel.evaluate((kind) => chrome.runtime.sendMessage<PanelState>({ kind }), MSG.stateGet);

function externalMessage(spa: Page, message: unknown): Promise<unknown> {
  return spa.evaluate(
    ([id, body]) =>
      new Promise((resolve) => {
        const runtime = (globalThis as { chrome?: { runtime?: { sendMessage?: unknown } } }).chrome
          ?.runtime as
          | { sendMessage(id: string, message: unknown, callback: (answer: unknown) => void): void }
          | undefined;
        if (!runtime?.sendMessage) resolve('no chrome.runtime');
        else runtime.sendMessage(id as string, body, resolve);
      }),
    [EXTENSION_ID, message] as const,
  );
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

  const server = new FakeServer();
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

main()
  .catch((err: unknown) => {
    failures += 1;
    console.error(`FAIL - ${err instanceof Error ? (err.stack ?? err.message) : String(err)}`);
  })
  .finally(() => {
    console.log(failures ? `\n${failures} check(s) failed` : '\nall checks passed');
    process.exitCode = failures ? 1 : 0;
  });
