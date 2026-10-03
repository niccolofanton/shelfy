// P2-15 real-browser synthetic smoke: C9 starts two IG steps, then a login
// wall aborts the platform before its second step. All social requests are
// intercepted fixtures; every unexpected non-local request is aborted.
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { parseArgs } from 'node:util';
import { buildExtension } from '../build';
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
  waitFor,
} from './smoke-lib';
import type { PlannerJob } from '../src/sw/planner/service';
import { MSG } from '../src/shared/protocol';

const { values } = parseArgs({
  options: { chrome: { type: 'string' }, headed: { type: 'boolean' }, port: { type: 'string' } },
  strict: true,
});
const PORT = Number(values.port ?? 18315);
const ORIGIN = `http://localhost:${PORT}`;
const FOLDER_ID = '17890000000000002';
const FOLDER = `https://www.instagram.com/someone/saved/recipes/${FOLDER_ID}/`;
const SAVED = 'https://www.instagram.com/someone/saved/all-posts/';
let login = false;
function media(n: number) {
  return {
    media: {
      pk: String(3500000000000000000n + BigInt(n)),
      id: `${3500000000000000000n + BigInt(n)}_9000000002`,
      code: `synthetic${n}`,
      media_type: 1,
      taken_at: 1758100000,
      user: { username: 'synthetic_planner_author' },
      caption: { text: `Planner fixture ${n}` },
      image_versions2: {
        candidates: [{ url: `https://scontent-synth1-1.cdninstagram.com/v/planner_${n}.jpg` }],
      },
    },
  };
}
async function main() {
  const work = mkdtempSync(join(tmpdir(), 'shelfy-planner-smoke-'));
  const dist = join(work, 'dist');
  const build = await buildExtension({ origin: ORIGIN, outDir: dist });
  check(build.problems.length === 0, 'extension build sanity');
  const server = new FakeServer(PORT);
  server.api.platformConfig.instagram = { replay: true, scroll: false };
  server.api.sources = [
    {
      platform: 'instagram',
      listing: { kind: 'ig_collection', externalId: FOLDER_ID, name: 'Recipes' },
      collectionId: 7,
    },
  ];
  await server.start();
  const context = await launch(join(work, 'profile'), dist, {
    chrome: values.chrome,
    headed: values.headed,
  });
  const unexpected: string[] = [];
  const errors: string[] = [];
  try {
    await context.route(/^https?:\/\//, (route) => {
      if (route.request().url().startsWith(`${ORIGIN}/`)) return route.continue();
      unexpected.push(route.request().url());
      return route.abort();
    });
    await context.route('https://www.instagram.com/**', (route) => {
      const url = new URL(route.request().url());
      if (url.pathname === '/api/v1/accounts/current_user/')
        return route.fulfill(jsonBody(JSON.stringify({ user: { username: 'someone' } })));
      if (
        url.pathname === '/api/v1/feed/saved/posts/' ||
        url.pathname === `/api/v1/feed/collection/${FOLDER_ID}/posts/`
      )
        return route.fulfill(
          jsonBody(
            JSON.stringify({
              items: [media(url.pathname.includes('collection') ? 2 : 1)],
              more_available: false,
            }),
          ),
        );
      if (url.href === SAVED && login)
        return route.fulfill(
          htmlPage(
            '<script>location.replace("https://www.instagram.com/accounts/login/")</script>',
          ),
        );
      if (url.pathname === '/someone/saved/')
        return route.fulfill(htmlPage(`<main><a href="${FOLDER}">Recipes</a></main>`));
      if (url.href === SAVED || url.href === FOLDER)
        return route.fulfill(
          htmlPage('<main><h1>Saved fixture</h1><a href="/p/synthetic/">Post</a></main>'),
        );
      return route.fulfill(htmlPage('<main>Fixture account page</main>'));
    });
    const worker = await extensionWorker(context);
    const spa = await context.newPage();
    await spa.goto(`${ORIGIN}/settings/connections`);
    check(
      JSON.stringify(
        await externalMessage(spa, {
          type: 'shelfy.sync.start',
          target: { platform: 'instagram' },
        }),
      ) === '{"ok":false,"code":"not_paired"}',
      'C9 unpaired refuses before opening a window',
    );
    check(
      JSON.stringify(
        await externalMessage(spa, { type: 'shelfy.pair', code: server.api.issuePairingCode() }),
      ) === '{"ok":true}',
      'extension pairs with fake SPA',
    );
    const panel = await openPanel(context, errors);
    const snapshot = () =>
      panel.evaluate(
        (kind) => chrome.runtime.sendMessage<{ jobs: PlannerJob[] }>({ kind }),
        MSG.plannerGet,
      );
    const started = await externalMessage(spa, {
      type: 'shelfy.sync.start',
      target: { platform: 'instagram' },
    });
    check(
      JSON.stringify(started) === '{"ok":true}',
      'C9 starts source sync',
      JSON.stringify(started),
    );
    check(
      JSON.stringify(
        await externalMessage(spa, {
          type: 'shelfy.sync.start',
          target: { platform: 'instagram' },
        }),
      ) === '{"ok":false,"code":"busy"}',
      'one planner per platform',
    );
    await waitFor(
      async () =>
        (await snapshot()).jobs.some(
          (job) => job.platform === 'instagram' && job.status === 'done',
        ),
      'two-step planner run',
      60_000,
    );
    const done = (await snapshot()).jobs.find((job) => job.platform === 'instagram');
    check(
      done?.status === 'done' && done.step === 2 && done.skipped === 0,
      'all saved and native folder both run',
      JSON.stringify(done),
    );
    check(
      [...server.api.runs.values()].filter((run) => run.trigger === 'web').length === 2,
      'controller receives two web-triggered runs',
    );
    check(
      await waitFor(
        () => [...server.api.runs.values()].every((run) => run.state !== 'running'),
        'two closing PATCHes',
        20_000,
      ),
      'both runs close on fake API',
    );
    check(
      server.api.posts.size === 2,
      'synthetic posts from both steps reach ingest',
      String(server.api.posts.size),
    );
    const window = await worker.evaluate(
      async (windowId) => chrome.windows.get(windowId!, { populate: true }),
      done?.windowId ?? null,
    );
    check(
      window.type === 'normal' && window.state === 'normal' && window.tabs?.length === 1,
      'dedicated normal visible window with one tab',
    );
    const alarms = await worker.evaluate(async () => ({
      poll: await chrome.alarms.get('shelfy.tasks.poll'),
      reminder: await chrome.alarms.get('shelfy.sync.reminder'),
    }));
    check(
      alarms.poll?.periodInMinutes === 5 && !alarms.reminder,
      '5-minute task alarm; reminder disabled by default',
    );
    login = true;
    const before = server.api.runs.size;
    await externalMessage(spa, { type: 'shelfy.sync.start', target: { platform: 'instagram' } });
    await waitFor(
      async () =>
        (await snapshot()).jobs.some(
          (job) => job.status === 'error' && job.code === 'login_required',
        ),
      'login wall aborts planner',
      20_000,
    );
    const failed = (await snapshot()).jobs.find((job) => job.platform === 'instagram');
    check(
      failed?.status === 'error' && failed.code === 'login_required' && failed.step === 1,
      'login aborts the platform instead of skipping to the folder',
      JSON.stringify(failed),
    );
    check(server.api.runs.size === before, 'the later native folder step never starts after login');
    check(
      unexpected.length === 0,
      'no unexpected network or real social traffic',
      unexpected.join('; '),
    );
    check(errors.length === 0, 'panel has no uncaught errors', errors.join('; '));
  } finally {
    await context.close();
    await server.stop();
    rmSync(work, { recursive: true, force: true });
  }
}
runSmoke(main);
