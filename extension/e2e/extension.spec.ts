// This gate uses real auth/C2/C4/C5/C6/tus/archive code on a throwaway local server.
// Social pages and CDN bytes are fixtures. It proves no live-account parity (P2-23/24).
import { test, expect } from '@playwright/test';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { buildExtension } from '../build';
import { launch, extensionWorker, openPanel } from '../scripts/smoke-lib';
import { MSG } from '../src/shared/protocol';
import { runCompareCli } from '../scripts/compare-lib';
import { parseRunReport } from '../src/shared/run-report';
import type { PanelState } from '../src/sw/state';
import type { SyncStartAnswer } from '../src/shared/protocol';
import { RealServer, ORIGIN } from './server';
import { routePlatforms, type FixtureState, SAVED, FOLDER, FOLDER_ID, KEY, PK } from './fixtures';

test('synthetic real-server extension lifecycle and archival fallback', async ({}, info) => {
  const started = Date.now();
  const checkpoints: string[] = [];
  const mark = (label: string) => {
    checkpoints.push(label);
    console.log(`[synthetic] ${label}`);
  };
  const work = mkdtempSync(join(tmpdir(), 'shelfy-extension-real-'));
  const server = new RealServer(work);
  const dist = join(work, 'extension');
  const build = await buildExtension({ origin: ORIGIN, outDir: dist });
  expect(build.problems).toEqual([]);
  let context: Awaited<ReturnType<typeof launch>> | undefined;
  try {
    await server.start();
    context = await launch(join(work, 'profile'), dist);
    context.setDefaultTimeout(15_000);
    context.setDefaultNavigationTimeout(30_000);
    const state: FixtureState = {
      mode: 'normal',
      replayPages: 0,
      unexpected: [],
      uploadRequests: 0,
    };
    await routePlatforms(context, state);
    let worker = await extensionWorker(context);
    const spa = await context.newPage();
    const link = server
      .admin('login-link', '--email', 'extension-ci@example.test', '--purpose', 'login')
      .split('\n')
      .find((line) => line.startsWith(ORIGIN));
    expect(link).toBeTruthy();
    await spa.goto(link!.trim());
    await spa.getByTestId('magic-sign-in').click();
    await expect(spa.getByTestId('disclaimer-gate')).toBeVisible();
    await spa.getByTestId('disclaimer-checkbox').locator('xpath=..').click();
    await spa.getByTestId('disclaimer-accept').click();
    await expect(spa.getByTestId('disclaimer-gate')).toBeHidden();
    await expect(spa.getByTestId('sidebar')).toBeVisible();
    const me = (await (await spa.request.get(`${ORIGIN}/api/v1/me`)).json()) as { id: string };
    await spa.goto(`${ORIGIN}/settings/connections`);
    await spa.getByTestId('ext-pair').click();
    await expect(spa.getByTestId('ext-paired')).toBeVisible();
    mark('SPA Connections pairing');
    const errors: string[] = [];
    const panel = await openPanel(context, errors);
    const send = <T>(message: unknown) =>
      panel.evaluate(async (value) => chrome.runtime.sendMessage<T>(value), message);
    const snapshot = () => send<PanelState>({ kind: MSG.stateGet });
    const posts = () => server.rows(me.id, 'SELECT key, cover_object FROM posts');
    const has = (key: string) => posts().some((row) => row.key === key);
    const ig = await context.newPage();
    await ig.goto(SAVED);
    await ig.waitForFunction(() => document.title === 'loaded');
    const x = await context.newPage();
    await x.goto('https://x.com/i/bookmarks');
    const pin = await context.newPage();
    await pin.goto('https://www.pinterest.com/someone/recipes/');
    await expect.poll(() => has(KEY(1))).toBe(true);
    await expect
      .poll(() => posts().filter((row) => String(row.key).startsWith('x_')).length)
      .toBe(2);
    await expect
      .poll(() => posts().filter((row) => String(row.key).startsWith('pin_')).length)
      .toBe(2);
    mark('IG/X/Pinterest passive capture');
    await expect
      .poll(() => posts().filter((row) => row.cover_object != null).length, { timeout: 30_000 })
      .toBeGreaterThan(0)
      .catch(async (error: unknown) => {
        await info.attach('archive-diagnostics', {
          body: JSON.stringify({
            requests: server.cdnRequests,
            rows: server.rows(
              me.id,
              'SELECT key, archive_state, cover_url, cover_fetch_error, cover_fetch_attempts FROM posts',
            ),
            jobs: server.controlRows(
              'SELECT id, kind, state, stage, error_code, run_at, attempts FROM jobs',
            ),
            logs: server.logs,
          }),
          contentType: 'application/json',
        });
        throw error;
      });
    expect(server.cdnRequests).toBeGreaterThan(0);
    mark('real server archives TLS CDN bytes');
    await ig.bringToFront();
    const tabId = await panel.evaluate(
      async (url) => (await chrome.tabs.query({})).find((tab) => tab.url === url)!.id!,
      SAVED,
    );
    const sync = async () => {
      const answer = await send<SyncStartAnswer>({
        kind: MSG.syncStart,
        tabId,
        collection: 'auto',
        name: null,
      });
      expect(answer.ok).toBe(true);
      if (!answer.ok) throw new Error(answer.code);
      await expect
        .poll(
          async () => (await snapshot()).syncs.find((run) => run.runId === answer.runId)?.state,
          { timeout: 30_000 },
        )
        .toBe('ended');
      await send({ kind: MSG.queueFlush });
      return (await snapshot()).syncs.find((run) => run.runId === answer.runId)!;
    };
    const first = await sync();
    expect(first.replayPages).toBe(3);
    const beforePages = state.replayPages;
    const second = await sync();
    expect(second.incremental).toBe(true);
    expect(second.replayPages).toBeLessThanOrEqual(2);
    expect(state.replayPages - beforePages).toBeLessThanOrEqual(2);
    mark('IG replay then incremental <=2 pages');
    await ig.goto(FOLDER);
    await ig.waitForFunction(() => document.title === 'loaded');
    await expect.poll(() => has(KEY(201))).toBe(true);
    await expect
      .poll(() =>
        server
          .rows(me.id, 'SELECT external_id FROM collections WHERE platform = ?', 'instagram')
          .some((row) => row.external_id === FOLDER_ID),
      )
      .toBe(true);
    mark('native folder mapping');
    await ig.goto(SAVED);
    await ig.waitForFunction(() => document.title === 'loaded');
    await ig.bringToFront();
    await expect.poll(() => panel.locator('[data-testid=select-start]').isEnabled()).toBe(true);
    await panel
      .locator('[data-testid=select-start]')
      .evaluate((button: HTMLButtonElement) => button.click());
    await ig.locator('[data-ss-check]').first().click();
    await expect.poll(() => panel.locator('[data-testid=select-import]').isEnabled()).toBe(true);
    await panel
      .locator('[data-testid=select-import]')
      .evaluate((button: HTMLButtonElement) => button.click());
    await expect.poll(() => has(KEY(321))).toBe(true);
    mark('selection import');
    await server.stopServer();
    state.mode = 'offline';
    await ig.reload();
    await expect.poll(async () => (await snapshot()).queue.queuedItems).toBeGreaterThan(0);
    await server.restart();
    await send({ kind: MSG.queueFlush });
    await expect.poll(() => has(KEY(801)), { timeout: 20_000 }).toBe(true);
    mark('offline queue survives actual server stop/restart');
    server.blockCdn = true;
    state.mode = 'blocked';
    await ig.reload();
    await expect.poll(() => server.cdnRefusals, { timeout: 30_000 }).toBeGreaterThanOrEqual(10);
    await expect
      .poll(
        async () => {
          await send({ kind: MSG.tasksPoll });
          return state.uploadRequests;
        },
        { timeout: 45_000 },
      )
      .toBeGreaterThan(0);
    await expect
      .poll(
        () =>
          posts().filter(
            (row) =>
              String(row.key).startsWith('ig_') &&
              BigInt(String(row.key).slice(3)) >= BigInt(PK(600)) &&
              BigInt(String(row.key).slice(3)) <= BigInt(PK(614)) &&
              row.cover_object != null,
          ).length,
        { timeout: 45_000 },
      )
      .toBeGreaterThan(0);
    mark('real CDN breaker hands off to extension tus upload');
    const report = parseRunReport(await send({ kind: MSG.syncReport }));
    expect(
      report.runs.some((run) => run.listingKey === `instagram:ig_collection:${FOLDER_ID}`),
    ).toBe(true);
    const download = panel.waitForEvent('download');
    await panel
      .locator('[data-testid=sync-export-report]')
      .evaluate((button: HTMLButtonElement) => button.click());
    await (await download).saveAs(info.outputPath('run-report.json'));
    const desktop = join(work, 'desktop.json'),
      input = join(work, 'run-report.json');
    writeFileSync(input, JSON.stringify(report));
    writeFileSync(
      desktop,
      JSON.stringify({
        posts: [
          ...[1, 2, 3, 4, 5, 6, 201, 202, 321, 801].map((n) => ({
            id: PK(n),
            platform: 'instagram',
            collections: n === 201 || n === 202 ? [`x:${FOLDER_ID}`] : [],
          })),
          ...['1800000000000000001', '1800000000000000002'].map((id) => ({
            id,
            platform: 'twitter',
            collections: [],
          })),
          ...['900000000000000001', '900000000000000002'].map((id) => ({
            id,
            platform: 'pinterest',
            collections: [],
          })),
        ],
        collections: [{ platform: 'instagram', externalId: FOLDER_ID, name: 'Recipes fixture' }],
      }),
    );
    const parity: string[] = [];
    const parityStatus = runCompareCli(['--run-report', input, '--desktop-export', desktop], {
      stdout: (text) => parity.push(text),
      stderr: (text) => {
        throw new Error(text);
      },
      writeFile: () => {},
    });
    await info.attach('synthetic-parity', { body: parity.join(''), contentType: 'text/plain' });
    expect(parityStatus, parity.join('')).toBe(0);
    expect(
      report.runs.some((run) => run.trigger === 'selection' && run.keys.includes(KEY(321))),
    ).toBe(true);
    mark('panel accepted-key run report and per-listing parity');
    server.admin('flags', 'set', 'extension.instagram.passive', 'false');
    await server.restart();
    state.mode = 'killed';
    await ig.reload();
    await send({ kind: MSG.queueFlush });
    await expect
      .poll(async () => (await snapshot()).serverPassive.instagram, { timeout: 20_000 })
      .toBe(false);
    expect(has(KEY(901))).toBe(false);
    mark('admin flags kill switch');
    server.admin('flags', 'set', 'extension.minVersion', '99.0.0');
    await server.restart();
    worker = await extensionWorker(context);
    const status = await worker.evaluate(async (origin) => {
      const stored = await chrome.storage.local.get('shelfy.pairing');
      const pairing = stored['shelfy.pairing'] as { token: string };
      return (
        await fetch(`${origin}/api/v1/ingest/tasks?wait=0`, {
          headers: { Authorization: `Bearer ${pairing.token}`, 'X-Shelfy-Extension': '0.2.0' },
        })
      ).status;
    }, ORIGIN);
    expect(status).toBe(426);
    mark('real 426 for old extension version');
    expect(state.unexpected).toEqual([]);
    expect(errors).toEqual([]);
  } finally {
    await info.attach('synthetic-ci-report', {
      body: JSON.stringify(
        {
          scope: 'synthetic CI; no live-account validation',
          runtimeMs: Date.now() - started,
          retry: info.retry,
          maxRetries: 1,
          checkpoints,
        },
        null,
        2,
      ),
      contentType: 'application/json',
    });
    await context?.close();
    await server.close();
    rmSync(work, { recursive: true, force: true });
  }
});
