import { test, expect, OWNER, HELLO, sse } from './api';
import type { components } from '../src/api/schema';
import type { Page } from '@playwright/test';
type Run = components['schemas']['SyncRun'];
function run(id = 'run-ig', overrides: Partial<Run> = {}): Run {
  return {
    id,
    platform: 'instagram',
    trigger: 'web',
    listing: { kind: 'ig_saved', externalId: null, name: null },
    collectionId: null,
    state: 'running',
    incremental: false,
    stopAfterKnown: 0,
    resumeCursor: null,
    stopReason: null,
    scanned: 10,
    inserted: 7,
    known: 3,
    updated: 0,
    pages: 1,
    startedAt: Date.now(),
    finishedAt: null,
    errorCode: null,
    ...overrides,
  };
}
const progress = (run: Run) => ({ ...run, runId: run.id });
async function enable(page: Page, ready = true) {
  await page.route('**/api/v1/me', (route) =>
    route.fulfill({ json: { ...OWNER, capabilities: { ...OWNER.capabilities, extension: true } } }),
  );
  await page.addInitScript((ready) => {
    const w = window as unknown as {
      chrome: unknown;
      __syncMessages: unknown[];
      __syncing: Record<string, boolean>;
    };
    w.__syncMessages = [];
    w.__syncing = {};
    w.chrome = {
      runtime: {
        lastError: null,
        sendMessage(
          _id: string,
          message: { type: string; target?: { platform: string }; platform?: string },
          callback: (response: unknown) => void,
        ) {
          w.__syncMessages.push(message);
          if (!ready) {
            callback(null);
            return;
          }
          if (message.type === 'shelfy.ping')
            callback({
              ok: true,
              paired: true,
              outdated: false,
              version: '1',
              syncing: w.__syncing,
            });
          else {
            if (message.type === 'shelfy.sync.start') w.__syncing[message.target!.platform] = true;
            if (message.type === 'shelfy.sync.stop') w.__syncing[message.platform!] = false;
            callback({ ok: true });
          }
        },
      },
    };
  }, ready);
}
async function sent(page: Page) {
  return page.evaluate(() => (window as unknown as { __syncMessages: unknown[] }).__syncMessages);
}
test.afterEach(({ api }) => expect(api.thirdParty).toEqual([]));

test('Gallery and Connections start/stop sync with independent live SSE progress', async ({
  page,
  api,
}) => {
  await enable(page);
  await page.goto('/');
  await page.getByTestId('source-instagram').click();
  await page.getByTestId('gallery-sync-source').click();
  await expect
    .poll(() => sent(page))
    .toContainEqual({ type: 'shelfy.sync.start', target: { platform: 'instagram' } });
  await expect(page.getByTestId('connection-syncing-instagram')).toBeVisible();
  const ig = run(),
    x = run('run-x', {
      platform: 'twitter',
      listing: { kind: 'x_bookmarks', externalId: null, name: null },
      inserted: 4,
      known: 6,
    });
  api.syncRuns.push(ig, x);
  api.streams.push(
    HELLO + sse('sync.progress', progress(ig), 'ig-1') + sse('sync.progress', progress(x), 'x-1'),
  );
  await page.getByTestId('activity-strip').click();
  await expect(page.getByTestId('activity-sync-run-ig')).toContainText(
    '10 scanned · 7 new · 3 known',
  );
  await expect(page.getByTestId('activity-sync-run-ig')).toContainText('Step 1 of 2');
  await expect(page.getByTestId('activity-sync-run-x')).toContainText(
    '10 scanned · 4 new · 6 known',
  );
  api.streams.push(
    HELLO + sse('sync.progress', progress({ ...ig, scanned: 40, inserted: 27, known: 13 }), 'ig-2'),
  );
  await expect(page.getByTestId('activity-sync-run-ig')).toContainText('40 scanned');
  await expect(page.getByTestId('activity-sync-run-x')).toContainText('10 scanned');
  await page.getByTestId('activity-sync-stop-instagram').click();
  await expect
    .poll(() => sent(page))
    .toContainEqual({ type: 'shelfy.sync.stop', platform: 'instagram' });
});

test('native-folder target, login-required open action and persisted sync result after reload', async ({
  page,
  api,
}) => {
  await enable(page);
  await page.goto('/c/1');
  await page.getByTestId('gallery-sync-source').click();
  await expect
    .poll(() => sent(page))
    .toContainEqual({
      type: 'shelfy.sync.start',
      target: { platform: 'instagram', collectionId: 1 },
    });
  const failed = run('login', {
    state: 'failed',
    errorCode: 'login_required',
    stopReason: 'login_required',
    finishedAt: Date.now(),
  });
  api.syncRuns.push(failed);
  api.notifications.push({
    id: 18,
    kind: 'sync',
    code: 'sync.login_required',
    params: { platform: 'Instagram' },
    target: null,
    createdAt: Date.now(),
    readAt: null,
  });
  api.streams.push(
    HELLO +
      sse('sync.progress', progress(failed), 'login') +
      sse('notification', api.notifications[0], 'notif-18'),
  );
  await page.getByTestId('activity-strip').click();
  await expect(page.getByTestId('activity-sync-login')).toContainText(
    'Sign in to your social account',
  );
  await expect(page.getByTestId('activity-sync-open-instagram')).toHaveText('Open Instagram');
  await page.reload();
  await page.getByTestId('activity-strip').click();
  await expect(page.getByTestId('activity-log-18')).toContainText('Sign in to Instagram');
  await expect(page.getByTestId('activity-sync-open-instagram')).toBeVisible();
  expect(
    api
      .requestsTo('/api/v1/sync-runs')
      .some((request) => request.query.get('platform') === 'instagram'),
  ).toBe(true);
});

test('missing extension opens installation/pairing guidance in Connections', async ({ page }) => {
  await enable(page, false);
  await page.goto('/');
  await page.getByTestId('source-instagram').click();
  await page.getByTestId('gallery-sync-source').click();
  await expect(page.getByTestId('sync-help')).toContainText('Install the Shelfy extension');
  await page
    .getByTestId('sync-help')
    .getByRole('button', { name: 'Settings → Connections' })
    .click();
  await expect(page).toHaveURL(/\/settings\/connections$/);
  await expect(page.getByTestId('ext-missing')).toBeVisible();
});

test('phone explains desktop Chrome and maintains a 44px sync touch target', async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 812 });
  await enable(page);
  await page.addInitScript(() =>
    Object.defineProperty(navigator, 'userAgent', { get: () => 'iPhone', configurable: true }),
  );
  await page.goto('/c/1');
  const button = page.getByTestId('gallery-sync-source');
  const box = await button.boundingBox();
  expect(box!.width).toBeGreaterThanOrEqual(44);
  expect(box!.height).toBeGreaterThanOrEqual(44);
  await button.click();
  await expect(page.getByTestId('sync-help')).toContainText('Syncs run in desktop Chrome');
  expect(
    (await sent(page)).filter((value) => (value as { type: string }).type === 'shelfy.sync.start'),
  ).toEqual([]);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
});
