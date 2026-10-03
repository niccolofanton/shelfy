// P2-16 acceptance: unpacked MV3 extension + synthetic listings + in-memory authenticated API.
// Every HTTP request is intercepted or sent to the localhost fake; no accounts or real CDN.
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type {} from '../../electron/webview-select';
import type { Page } from 'playwright-core';
import { buildExtension } from '../build';
import { igPkToShortcode } from '../src/shared/identity';
import {
  FakeServer,
  check,
  externalMessage,
  extensionWorker,
  htmlPage,
  launch,
  openPanel,
  runSmoke,
  waitFor,
} from './smoke-lib';
const PORT = 18296,
  ORIGIN = `http://localhost:${PORT}`;
const IG = 'https://www.instagram.com/someone/saved/recipes/178999999999/';
const X = 'https://x.com/i/bookmarks';
const PIN = 'https://www.pinterest.com/someone/weekend/';
const pk = (n: number) => String(3500000000000000000n + BigInt(n));
const code = (n: number) => igPkToShortcode(pk(n))!;
function tiles(platform: 'ig' | 'x' | 'pin', count: number) {
  return Array.from({ length: count }, (_, i) => {
    const n = i + 1,
      key = platform === 'ig' ? code(n) : String(n);
    return platform === 'x'
      ? `<article data-testid="tweet" style="height:100px"><a href="/fixture/status/${key}"><time>now</time></a></article>`
      : `<a href="/${platform === 'ig' ? 'p' : 'pin'}/${key}/" style="display:block;width:80px;height:80px">${n}<img src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7" /></a>`;
  }).join('');
}
const listing = (platform: 'ig' | 'x' | 'pin', count: number) =>
  `<main><h1>Recipes</h1><div id="feed" style="display:grid;grid-template-columns:repeat(11,80px);gap:4px">${tiles(platform, count)}</div></main>`;
async function click(panel: Page, id: string) {
  if (
    !(await waitFor(
      () =>
        panel.evaluate((key) => {
          const button = document.querySelector(`[data-testid=${key}]`) as HTMLButtonElement | null;
          return !!button && !button.disabled && !button.hidden;
        }, id),
      `${id} ready`,
    ))
  )
    throw new Error(`${id} is not ready`);
  await panel.evaluate(
    (key) => (document.querySelector(`[data-testid=${key}]`) as HTMLButtonElement).click(),
    id,
  );
}
async function select(panel: Page, tab: Page) {
  await tab.bringToFront();
  if (
    !(await waitFor(
      () =>
        panel.evaluate((url) => {
          const button = document.querySelector('[data-testid=select-start]') as HTMLButtonElement;
          return (
            !button.disabled &&
            !button.hidden &&
            (document.querySelector('[data-section=select]') as HTMLElement).dataset.activeUrl ===
              url
          );
        }, tab.url()),
      'Select offered',
    ))
  )
    throw new Error(
      'Select unavailable on ' +
        tab.url() +
        ' active: ' +
        JSON.stringify(
          await panel.evaluate(() => chrome.tabs.query({ active: true, currentWindow: true })),
        ) +
        ' errors: ' +
        (await panel.locator('[data-section=select]').evaluate((node) => node.outerHTML)),
    );
  await click(panel, 'select-start');
  await tab.waitForSelector('[data-ss-check]');
}

async function main() {
  const work = mkdtempSync(join(tmpdir(), 'shelfy-selection-smoke-'));
  const dist = join(work, 'dist');
  const build = await buildExtension({ origin: ORIGIN, outDir: dist });
  check(!build.problems.length, 'extension build sanity', build.problems.join('; '));
  const server = new FakeServer(PORT);
  await server.start();
  server.api.posts.set(`ig_${pk(1)}`, 1);
  const context = await launch(join(work, 'profile'), dist);
  const errors: string[] = [],
    unexpected: string[] = [];
  try {
    await context.route(/^https?:\/\//, (route) => {
      const url = route.request().url();
      if (url.startsWith(`${ORIGIN}/`)) return route.continue();
      if (url === IG) return route.fulfill(htmlPage(listing('ig', 1102)));
      if (url === X) return route.fulfill(htmlPage(listing('x', 3)));
      if (url === PIN)
        return route.fulfill(
          htmlPage(
            `<script type="application/json" id="__PWS_DATA__">{"props":{"context":{"user":{"username":"someone","is_auth":true}}}}</script>${listing('pin', 3)}`,
          ),
        );
      unexpected.push(
        route.request().resourceType() + ' ' + route.request().frame().url() + ' -> ' + url,
      );
      return route.abort();
    });
    await extensionWorker(context);
    const spa = await context.newPage();
    await spa.goto(`${ORIGIN}/settings/connections`);
    check(
      JSON.stringify(
        await externalMessage(spa, { type: 'shelfy.pair', code: server.api.issuePairingCode() }),
      ) === '{"ok":true}',
      'extension pairs',
    );
    const panel = await openPanel(context, errors);
    const ig = await context.newPage();
    ig.on('pageerror', (err) => errors.push(err.message));
    await ig.goto(IG);
    await select(panel, ig);
    check(
      await waitFor(
        () =>
          ig.evaluate(
            () =>
              document.querySelector('[data-ss-open]')?.textContent?.includes('Already saved') ===
              true,
          ),
        'saved badge',
      ),
      'EN saved badge from lookup',
    );
    await waitFor(
      () => server.api.lookups.reduce((sum, call) => sum + call.keys.length, 0) >= 1102,
      'lookup chunks',
    );
    check(
      server.api.lookups.some((call) => call.keys.length === 1000) &&
        server.api.lookups.every((call) => call.keys.length <= 1000),
      'lookup capped at 1000 IDs',
    );
    check(
      (await panel.inputValue('[data-testid=select-name]')) === 'Recipes',
      'folder chooser uses listing heading',
    );

    // Select an anchor, unmount it and intermediate posts, and recycle one card.
    await ig.evaluate(() => {
      const boxes = document.querySelectorAll('[data-ss-check]');
      (boxes[1] as HTMLElement).click();
      for (let i = 1; i < 4; i++) boxes[i].closest('a')!.remove();
      const card = boxes[4].closest('a')!;
      card.setAttribute('href', '/p/ZZZNew/');
      window.__ssSelect?.refresh();
      const last = document.querySelectorAll('[data-ss-check]');
      last[last.length - 1].dispatchEvent(
        new MouseEvent('click', { bubbles: true, cancelable: true, shiftKey: true }),
      );
    });
    const entries = await ig.evaluate(
      () =>
        JSON.parse(window.__ssSelect!.collectEntriesJSON()) as Array<{
          key: string;
          item: { shortcode: string };
        }>,
    );
    check(
      entries.length === 1102 &&
        entries.some((entry) => entry.key === code(2) && entry.item.shortcode === code(2)),
      'shift range includes unmounted anchor and snapshots retain its post',
      String(entries.length),
    );
    check(
      (await ig.locator('[data-ss-key=ZZZNew]').count()) === 1,
      'recycled card checkbox points to new post',
    );
    await ig.evaluate((key) => {
      const card = document.createElement('a');
      card.href = '/p/' + key + '/';
      card.style.cssText = 'display:block;width:80px;height:80px';
      document.querySelector('#feed')!.append(card);
      window.__ssSelect!.refresh();
    }, code(2));
    check(
      (await ig.locator('[data-ss-key=' + code(2) + '] svg').count()) === 1,
      'checkbox selection survives remount',
    );
    await waitFor(
      () =>
        panel.evaluate(
          () =>
            !(document.querySelector('[data-testid=select-import]') as HTMLButtonElement).disabled,
        ),
      'Import selected enabled',
    );
    check(
      (await panel.textContent('[data-testid=select-import]')) === 'Import selected',
      'EN action says Import selected',
    );
    await click(panel, 'select-import');
    check(
      await waitFor(
        () =>
          [...server.api.runs.values()].some(
            (run) => run.trigger === 'selection' && run.state === 'done',
          ),
        'selection import done',
      ),
      'selection run closes',
    );
    const runs = [...server.api.runs.values()].filter((run) => run.trigger === 'selection');
    const ingests = server.api.ingests.filter((batch) => batch.runId === runs[0]?.id);
    check(
      JSON.stringify(ingests.map((batch) => batch.count)) === '[500,500,102]' &&
        ingests.every((batch) => batch.source === 'selection'),
      'selection batch cap + source',
      JSON.stringify(ingests.map((batch) => batch.count)),
    );
    check(
      JSON.stringify(runs[0]?.collection) === '{"mode":"auto"}',
      'folder chooser files selection into auto collection',
    );
    check(
      (await ig.evaluate(() => window.__ssSelect!.status().count)) === 0,
      'import clears accepted selection',
    );
    await click(panel, 'select-stop');
    await waitFor(
      () =>
        ig
          .locator('[data-ss-check]')
          .count()
          .then((count) => count === 0),
      'overlay disabled',
    );
    await select(panel, ig);
    check(
      await waitFor(
        () =>
          ig.evaluate(
            () =>
              Array.from(document.querySelectorAll('[data-ss-open]')).filter(
                (label) => (label as HTMLElement).style.display === 'flex',
              ).length > 1000,
          ),
        'persistent saved badges',
      ),
      'saved badges survive disable and re-enable',
    );
    const newTab = context.waitForEvent('page');
    await ig.evaluate(() => (document.querySelector('[data-ss-open]') as HTMLElement).click());
    const savedTab = await newTab;
    await savedTab.waitForLoadState();
    check(savedTab.url() === `${ORIGIN}/p/ig_${pk(1)}`, 'badge opens Shelfy /p/key');
    await savedTab.close();

    await context.addInitScript(
      "Object.defineProperty(navigator, 'languages', { get: function () { return ['it-IT']; } });",
    );
    const italianPanel = await openPanel(context, errors);
    check(
      (await italianPanel.textContent('[data-testid=select-import]')) === 'Importa selezionati',
      'IT action says Importa selezionati',
    );
    for (const url of [X, PIN]) {
      const tab = ig;
      await tab.goto(url);
      await select(italianPanel, tab);
      check(
        (await tab.locator('[data-ss-check]').count()) === 3,
        `checkboxes on ${url === X ? 'X' : 'Pinterest'} listing`,
      );
      await tab.evaluate(() => {
        const boxes = document.querySelectorAll('[data-ss-check]');
        (boxes[0] as HTMLElement).click();
        boxes[2].dispatchEvent(
          new MouseEvent('click', { bubbles: true, cancelable: true, shiftKey: true }),
        );
      });
      check(
        (await tab.evaluate(() => window.__ssSelect!.status().count)) === 3,
        'three-post shift selection',
      );
      if (url === PIN)
        await italianPanel.evaluate(() =>
          (document.querySelector('[data-testid=select-into-none]') as HTMLInputElement).click(),
        );
      await waitFor(
        () =>
          italianPanel.evaluate(
            () =>
              !(document.querySelector('[data-testid=select-import]') as HTMLButtonElement)
                .disabled,
          ),
        'IT import enabled',
      );
      const before = server.api.runs.size;
      await click(italianPanel, 'select-import');
      check(
        await waitFor(
          () =>
            [...server.api.runs.values()]
              .slice(before)
              .some((run) => run.trigger === 'selection' && run.state === 'done'),
          'platform selection import',
        ),
        'IT imports selection on ' + (url === X ? 'X' : 'Pinterest'),
      );
      if (url === PIN)
        check(
          JSON.stringify([...server.api.runs.values()].slice(before)[0]?.collection) ===
            '{"mode":"none"}',
          'no-collection chooser on Pinterest',
        );
      await click(italianPanel, 'select-stop');
      await waitFor(
        () =>
          tab
            .locator('[data-ss-check]')
            .count()
            .then((count) => count === 0),
        'overlay disabled',
      );
    }
    check(!errors.length, 'no page or panel errors', errors.join('; '));
    check(!unexpected.length, 'no platform requests left synthetic routes', unexpected.join('; '));
  } finally {
    await context.close();
    await server.stop();
    rmSync(work, { recursive: true, force: true });
  }
}
runSmoke(main);
