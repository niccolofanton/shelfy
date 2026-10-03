// Real extension + fake API + fixture CDN + synthetic IG only. Worker CDN
// fetches are fixture responses; every other non-local request is refused.
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { parseArgs } from 'node:util';
import { buildExtension } from '../build';
import { MSG } from '../src/shared/protocol';
import type { ExtensionTask } from '../src/sw/tasks/contracts';
import { igPkToShortcode } from '../src/shared/identity';
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

const { values } = parseArgs({
  options: { chrome: { type: 'string' }, headed: { type: 'boolean' }, port: { type: 'string' } },
  strict: true,
});
const PORT = Number(values.port ?? 18317);
const ORIGIN = `http://localhost:${PORT}`;
const CDN = 'https://scontent-synth1-1.cdninstagram.com/v/poster.png';
const PNG =
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aAAEAAAAASUVORK5CYII=';
function task(n: number, kind: ExtensionTask['kind']): ExtensionTask {
  const nativeId = String(3500000000000000000n + BigInt(n));
  return {
    id: `${kind}.ig_${nativeId}.post`,
    kind,
    platform: 'instagram',
    nativeId,
    postKey: `ig_${nativeId}`,
    postUrl: `https://www.instagram.com/p/${igPkToShortcode(nativeId)}/`,
    shortcode: igPkToShortcode(nativeId),
    position: null,
    url: kind === 'upload_media' ? CDN : null,
    expiresAt: null,
    leaseId: `generation-${n}`,
    leaseUntil: Date.now() + 300_000,
  };
}
async function main() {
  const work = mkdtempSync(join(tmpdir(), 'shelfy-tasks-smoke-'));
  const dist = join(work, 'dist');
  const build = await buildExtension({ origin: ORIGIN, outDir: dist });
  check(build.problems.length === 0, 'bundle contains the real IG REST entry and passes sanity');
  const server = new FakeServer(PORT);
  await server.start();
  const context = await launch(join(work, 'profile'), dist, {
    chrome: values.chrome,
    headed: values.headed,
  });
  const unexpected: string[] = [];
  const errors: string[] = [];
  const reads: Array<{ id: string; at: number }> = [];
  let limited = false;
  try {
    await context.route(/^https?:\/\//, (route) => {
      if (route.request().url().startsWith(`${ORIGIN}/`)) return route.continue();
      unexpected.push(route.request().url());
      return route.abort();
    });
    await context.route('https://www.instagram.com/**', (route) => {
      const url = new URL(route.request().url());
      const match = /^\/api\/v1\/media\/(\d+)\/info\/$/.exec(url.pathname);
      if (match) {
        reads.push({ id: match[1], at: Date.now() });
        if (limited)
          return route.fulfill({
            status: 429,
            contentType: 'application/json',
            body: '{"message":"Please wait"}',
          });
        return route.fulfill(
          jsonBody(
            JSON.stringify({
              items: [
                {
                  pk: match[1],
                  id: `${match[1]}_9000000002`,
                  code: igPkToShortcode(match[1]),
                  media_type: 2,
                  taken_at: 1758100000,
                  user: { username: 'synthetic_tasks_author' },
                  caption: { text: 'Synthetic poster recovery' },
                  image_versions2: { candidates: [{ url: CDN }] },
                  video_versions: [
                    { url: 'https://scontent-synth1-1.cdninstagram.com/v/never-download.mp4' },
                  ],
                },
              ],
              more_available: false,
            }),
          ),
        );
      }
      return route.fulfill(htmlPage('<main><h1>Synthetic Instagram account</h1></main>'));
    });
    const worker = await extensionWorker(context);
    await worker.evaluate(
      ({ origin, cdn, png }) => {
        const actualFetch = globalThis.fetch;
        const records: Array<{ url: string; credentials?: RequestCredentials; headers: unknown }> =
          [];
        const blocked: string[] = [];
        Object.assign(globalThis, { __taskCdn: records, __taskBlocked: blocked });
        globalThis.fetch = async (input, init) => {
          const url =
            typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
          if (url === cdn) {
            records.push({ url, credentials: init?.credentials, headers: init?.headers ?? null });
            return new Response(
              Uint8Array.from(atob(png), (char) => char.charCodeAt(0)),
              { headers: { 'content-type': 'image/png' } },
            );
          }
          if (!url.startsWith(`${origin}/`)) {
            blocked.push(url);
            throw new Error('synthetic-only request');
          }
          return actualFetch(input, init);
        };
      },
      { origin: ORIGIN, cdn: CDN, png: PNG },
    );
    const spa = await context.newPage();
    await spa.goto(`${ORIGIN}/settings/connections`);
    check(
      JSON.stringify(
        await externalMessage(spa, { type: 'shelfy.pair', code: server.api.issuePairingCode() }),
      ) === '{"ok":true}',
      'pairs synthetic account',
    );
    const panel = await openPanel(context, errors);
    const poll = () =>
      panel.evaluate((kind) => chrome.runtime.sendMessage({ kind }), MSG.tasksPoll);
    const upload = task(1, 'upload_media');
    const refresh = task(2, 'refresh_media');
    const hydrate = task(3, 'hydrate_link');
    for (const item of [upload, refresh, hydrate]) server.api.tasks.set(item.id, item);
    const before = context.pages().length;
    await poll();
    check(
      server.api.taskCompletions.some((entry) => entry.id === upload.id && entry.status === 204),
      'CDN image lands through tus and completes its lease',
    );
    const object = [...server.api.uploads.values()][0];
    check(
      object?.metadata.purpose === 'archive-object' &&
        object.metadata.ext === 'png' &&
        object.bytes.length === Buffer.from(PNG, 'base64').length,
      'archive-object bytes, SHA and image extension validated by fake API',
    );
    check(
      context.pages().length === before && reads.length === 0,
      'waiting tasks never open an IG tab',
    );
    await waitFor(
      async () => (await panel.getByTestId('tasks-waiting').textContent())?.includes('2') === true,
      'panel waiting count',
    );
    check(
      (await panel.getByTestId('tasks-waiting').textContent())?.includes('Instagram') === true,
      'Waiting section asks to open Instagram',
    );
    const instagram = await context.newPage();
    await instagram.goto('https://www.instagram.com/');
    await poll();
    check(
      server.api.tasks.size === 0 && server.api.ingests.length === 2,
      'refresh and hydration reach normal ingest before completion',
    );
    check(
      server.api.ingests.every((batch) => batch.source === 'refresh'),
      'batches are tagged refresh',
    );
    check(
      server.api.taskCompletions.every(
        (entry) =>
          entry.status === 204 &&
          (entry.body as { leaseId: string }).leaseId.startsWith('generation-'),
      ),
      'all completions echo current leaseId',
    );
    check(
      reads.length === 2 && reads[1].at - reads[0].at >= 700,
      'MAIN reads remain at least 700ms apart',
      JSON.stringify(reads),
    );
    check(context.pages().length === before + 1, 'refresh uses only the manually opened IG tab');
    const video = {
      ...task(4, 'upload_media'),
      url: 'https://scontent-synth1-1.cdninstagram.com/v/never-download.mp4',
    };
    server.api.tasks.set(video.id, video);
    await poll();
    check(
      server.api.taskCompletions.some(
        (entry) =>
          entry.id === video.id &&
          (entry.body as { errorCode: string }).errorCode === 'cdn_not_allowed',
      ),
      'MP4 task is refused before any download',
    );
    limited = true;
    const first = task(5, 'refresh_media'),
      later = task(6, 'refresh_media');
    server.api.tasks.set(first.id, first);
    server.api.tasks.set(later.id, later);
    await poll();
    await poll();
    check(
      reads.length === 3 && reads[2].id === first.nativeId,
      'first 429 stops this IG-tab session, including the next poll',
    );
    check(
      server.api.tasks.has(first.id) && server.api.tasks.has(later.id),
      'rate limit leaves tasks waiting without failed/gone completions',
    );
    const cdn = await worker.evaluate(
      () => (globalThis as typeof globalThis & { __taskCdn: unknown[] }).__taskCdn,
    );
    const blocked = await worker.evaluate(
      () => (globalThis as typeof globalThis & { __taskBlocked: string[] }).__taskBlocked,
    );
    check(
      JSON.stringify(cdn) === JSON.stringify([{ url: CDN, credentials: 'omit', headers: null }]),
      'fixture CDN uses omit and no Shelfy credentials; no MP4 fetch',
    );
    check(
      blocked.length === 0 && unexpected.length === 0,
      'zero real social or unexpected network traffic',
      [...blocked, ...unexpected].join('\n'),
    );
    check(errors.length === 0, 'panel has no uncaught errors', errors.join('\n'));
  } finally {
    await context.close();
    await server.stop();
    rmSync(work, { recursive: true, force: true });
  }
}
runSmoke(main);
