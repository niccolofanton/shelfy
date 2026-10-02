// Real-Chrome smoke test of the built extension (extension/dist), with synthetic pages only:
// every Instagram/X/Pinterest URL is answered by Playwright route interception from
// extension/tests/fixtures and every other http(s) request is aborted, so nothing reaches the
// real platforms. It checks what unit tests cannot: the manifest loads, the MAIN-world hook is
// in place before page scripts run, the bridge relays, the worker stores, the IG replay runs
// through chrome.scripting, the side panel renders and "Export JSON" downloads a valid file.
//
//   pnpm exec tsx extension/build.ts
//   pnpm exec tsx extension/scripts/smoke.ts [--chrome <binary>] [--headed]
//
// Branded Google Chrome ignores --load-extension (Chrome 137+). Use Playwright's Chromium
// (`pnpm exec playwright install chromium`) or pass a Chrome for Testing / Chromium binary.

import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { chromium, type BrowserContext, type Page, type Route } from 'playwright-core';
import { parseExportFile } from '../src/export-format';
import { RUNTIME } from '../src/protocol';

const here = dirname(fileURLToPath(import.meta.url));
const dist = join(here, '..', 'dist');
const fixtures = join(here, '..', 'tests', 'fixtures');
const fixture = (name: string): string => readFileSync(join(fixtures, name), 'utf8');

const IG_SAVED = 'https://www.instagram.com/someone/saved/all-posts/';
const IG_EXPLORE = 'https://www.instagram.com/explore/';
const X_BOOKMARKS = 'https://x.com/i/bookmarks';
const PIN_BOARD = 'https://www.pinterest.com/someone/recipes/';

const { values } = parseArgs({
  options: { chrome: { type: 'string' }, headed: { type: 'boolean' } },
  strict: true,
});

let failures = 0;
function check(condition: boolean, what: string, detail = ''): void {
  if (!condition) failures += 1;
  console.log(`${condition ? 'ok  ' : 'FAIL'} - ${what}${detail ? ` (${detail})` : ''}`);
}

/** The exact source of igFeedReplay as bundled: what chrome.scripting serializes into the page. */
function bundledReplaySource(): string {
  const code = readFileSync(join(dist, 'panel.js'), 'utf8');
  const start = code.indexOf('async function igFeedReplay(');
  if (start < 0) throw new Error('igFeedReplay not found in dist/panel.js');
  let depth = 0;
  for (let i = code.indexOf('{', start); i < code.length; i++) {
    if (code[i] === '{') depth += 1;
    else if (code[i] === '}' && --depth === 0) return code.slice(start, i + 1);
  }
  throw new Error('unbalanced igFeedReplay source');
}

const page = (body: string) => ({
  status: 200,
  contentType: 'text/html',
  body: `<!doctype html><html><head></head><body>${body}</body></html>`,
});
const json = (body: string) => ({ status: 200, contentType: 'application/json', body });
const markLoaded = "then((r) => r.text()).then(() => { document.title = 'loaded'; })";

async function serveSyntheticPlatforms(
  context: BrowserContext,
  unexpected: string[],
): Promise<void> {
  // Registered first, so it runs last: anything not answered below never leaves the browser.
  await context.route(/^https?:\/\//, (route: Route) => {
    unexpected.push(route.request().url());
    return route.abort();
  });
  await context.route('https://www.instagram.com/**', (route: Route) => {
    const url = new URL(route.request().url());
    if (url.href === IG_SAVED)
      return route.fulfill(
        page(
          `<script>fetch('/graphql/query', { method: 'POST', headers: { 'X-FB-Friendly-Name': 'SyntheticSavedQuery' }, body: 'doc_id=1' }).${markLoaded}</script>`,
        ),
      );
    if (url.href === IG_EXPLORE)
      return route.fulfill(page(`<script>fetch('/graphql/query').${markLoaded}</script>`));
    if (url.pathname === '/graphql/query')
      return route.fulfill(json(fixture('ig-graphql-legacy.json')));
    if (url.pathname === '/api/v1/feed/saved/posts/')
      return route.fulfill(
        json(
          fixture(
            url.searchParams.get('max_id') === 'SYNTHETIC_CURSOR_1'
              ? 'ig-saved-rest-page2.json'
              : 'ig-saved-rest-page1.json',
          ),
        ),
      );
    return route.fulfill({ status: 404, body: '' });
  });
  await context.route('https://x.com/**', (route: Route) => {
    const url = new URL(route.request().url());
    if (url.href === X_BOOKMARKS)
      return route.fulfill(
        page(
          `<script>fetch('/i/api/graphql/SYNTH/Bookmarks?variables=%7B%7D').${markLoaded}</script>`,
        ),
      );
    if (url.pathname.startsWith('/i/api/graphql/'))
      return route.fulfill(json(fixture('x-bookmarks.json')));
    return route.fulfill({ status: 404, body: '' });
  });
  const pins = (
    JSON.parse(fixture('pinterest-board-feed.json')) as { resource_response: { data: unknown } }
  ).resource_response.data;
  const ssr = {
    props: { initialReduxState: { resources: { BoardFeedResource: { args: { data: pins } } } } },
  };
  await context.route('https://www.pinterest.com/**', (route: Route) => {
    if (route.request().url() === PIN_BOARD)
      return route.fulfill(
        page(
          `<script type="application/json" id="__PWS_DATA__">${JSON.stringify(ssr)}</script><script>document.title = 'loaded';</script>`,
        ),
      );
    return route.fulfill({ status: 404, body: '' });
  });
}

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

interface StoreSnapshot {
  sources: Record<string, string[]>;
  discarded: number;
  refused: Record<string, number>;
  listings: string[];
}

async function main(): Promise<void> {
  if (!existsSync(join(dist, 'manifest.json')))
    throw new Error('extension/dist is missing: run `pnpm exec tsx extension/build.ts` first');
  const profile = mkdtempSync(join(tmpdir(), 'shelfy-spike3-smoke-'));
  const context = await chromium.launchPersistentContext(profile, {
    headless: !values.headed,
    executablePath: values.chrome,
    acceptDownloads: true,
    args: [
      `--disable-extensions-except=${dist}`,
      `--load-extension=${dist}`,
      '--disable-background-networking',
      '--disable-component-update',
      '--no-first-run',
    ],
  });
  const unexpected: string[] = [];
  const errors: string[] = [];
  try {
    await serveSyntheticPlatforms(context, unexpected);
    const worker = context.serviceWorkers()[0] ?? (await context.waitForEvent('serviceworker'));
    const extensionId = new URL(worker.url()).host;
    check(/^[a-p]{32}$/.test(extensionId), 'extension loaded with a service worker', extensionId);

    await visit(context, IG_SAVED, errors);
    const ping = await worker.evaluate(
      `chrome.tabs.query({ url: ${JSON.stringify(IG_SAVED)} }).then(([tab]) => chrome.tabs.sendMessage(tab.id, { kind: ${JSON.stringify(RUNTIME.ping)} }))`,
    );
    check(JSON.stringify(ping) === '{"ok":true}', 'the bridge answers the side panel ping');

    // The production path: chrome.scripting.executeScript({ world: 'MAIN', func }) with the
    // bundled function, as the side panel's "Run IG replay" button does.
    const replay = (await worker.evaluate(`(async () => {
      const [tab] = await chrome.tabs.query({ url: ${JSON.stringify(IG_SAVED)} });
      const [injection] = await chrome.scripting.executeScript({
        target: { tabId: tab.id },
        world: 'MAIN',
        func: ${bundledReplaySource()},
        args: [{ maxPages: 10, gapMs: 50, runId: 'smoke' }],
      });
      return injection.result;
    })()`)) as { reason?: string; pages?: number };
    check(
      replay.reason === 'end_of_feed' && replay.pages === 2,
      'IG replay runs in the MAIN world through chrome.scripting',
      JSON.stringify(replay),
    );

    await visit(context, IG_EXPLORE, errors);
    await visit(context, X_BOOKMARKS, errors);
    await visit(context, PIN_BOARD, errors);
    await new Promise((resolve) => setTimeout(resolve, 3000)); // census flush + worker writes

    const panel = await context.newPage();
    panel.on('pageerror', (e) => errors.push(`panel: ${e.message}`));
    await panel.goto(`chrome-extension://${extensionId}/panel.html`);
    const store = (await panel.evaluate(async () => {
      const all = await chrome.storage.local.get(null);
      const meta = all.meta as {
        platforms: Record<string, { discardedItems: number }>;
        listings: Record<string, unknown>;
        diagnostics: { refusedBatches: Record<string, number> };
      };
      const sources: Record<string, string[]> = {};
      for (const [key, value] of Object.entries(all)) {
        if (!key.startsWith('item:')) continue;
        const listings = (value as { listings: Record<string, { sources: string[] }> }).listings;
        sources[key.slice(5)] = Object.entries(listings).map(
          ([l, m]) => `${l}=${m.sources.join('+')}`,
        );
      }
      return {
        sources,
        discarded: meta.platforms.instagram.discardedItems,
        refused: meta.diagnostics.refusedBatches,
        listings: Object.keys(meta.listings).sort(),
      };
    })) as StoreSnapshot;
    const expected: Record<string, string[]> = {
      ig_3400000000000000001: ['instagram:ig_saved=passive+replay'],
      ig_3400000000000000002: ['instagram:ig_saved=replay'],
      ig_3400000000000000003: ['instagram:ig_saved=replay'],
      ig_3400000000000000004: ['instagram:ig_saved=passive'],
      x_1800000000000000001: ['twitter:x_bookmarks=passive'],
      x_1800000000000000002: ['twitter:x_bookmarks=passive'],
      pin_900000000000000001: ['pinterest:pin_board:someone/recipes=ssr'],
      pin_900000000000000002: ['pinterest:pin_board:someone/recipes=ssr'],
    };
    const sorted = (o: Record<string, string[]>) => JSON.stringify(Object.entries(o).sort());
    check(
      sorted(store.sources) === sorted(expected),
      'passive capture, replay, Pinterest SSR and X bookmarks stored with listing and source',
      sorted(store.sources) === sorted(expected) ? '' : JSON.stringify(store.sources),
    );
    check(
      store.discarded === 2,
      'batches captured outside a saved listing are discarded',
      `${store.discarded}`,
    );
    check(
      Object.keys(store.refused).length === 0,
      'no relayed message was refused',
      JSON.stringify(store.refused),
    );

    await panel.reload();
    await panel.waitForFunction(() => document.querySelectorAll('#listings tbody tr').length === 3);
    check(true, 'the side panel renders the three listings');
    const downloadEvent = panel.waitForEvent('download');
    await panel.click('#export');
    const download = await downloadEvent;
    const exportPath = join(profile, 'export.json');
    await download.saveAs(exportPath);
    const exported = parseExportFile(JSON.parse(readFileSync(exportPath, 'utf8')));
    check(
      exported.items.length === 8 &&
        /^shelfy-spike3-capture-\d{8}-\d{6}\.json$/.test(download.suggestedFilename()),
      '"Export JSON" downloads a valid export',
      `${download.suggestedFilename()}, ${exported.items.length} items`,
    );
    check(errors.length === 0, 'no console errors or warnings', errors.join(' | '));
    check(unexpected.length === 0, 'no request left the browser', unexpected.join(' '));
  } finally {
    await context.close();
    rmSync(profile, { recursive: true, force: true });
  }
}

main()
  .catch((err: unknown) => {
    failures += 1;
    console.error(`FAIL - ${err instanceof Error ? err.message : String(err)}`);
  })
  .finally(() => {
    console.log(failures ? `\n${failures} check(s) failed` : '\nall checks passed');
    process.exitCode = failures ? 1 : 0;
  });
