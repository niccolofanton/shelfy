// Settings → Connections (P2-12) on a mocked API and a fake extension: pairing,
// revoking a paired browser, and the Shortcut token shown once.
import { test, expect } from './api';

test.afterEach(({ api }) => {
  expect(api.thirdParty, 'requests outside the app').toEqual([]);
});

// A page where an extension answers `shelfy.ping` / `shelfy.pair`, as Chromium
// shows one that matches `externally_connectable`.
test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const w = window as unknown as { chrome: unknown; __sent: unknown[] };
    w.__sent = [];
    let paired = false;
    w.chrome = {
      runtime: {
        lastError: null,
        sendMessage(_id: string, message: { type: string }, callback: (r: unknown) => void) {
          w.__sent.push(message);
          if (message.type === 'shelfy.ping') {
            callback({ ok: true, version: '0.1.0', paired, outdated: false, syncing: {} });
          } else {
            paired = true;
            callback({ ok: true });
          }
        },
      },
    };
  });
});

test('pairs the extension and lists the paired browser for revoking', async ({ page, api }) => {
  await page.goto('/settings/connections');
  await expect(page.getByTestId('ext-state')).toContainText('not yet linked');
  await page.getByTestId('ext-pair').click();
  await expect(page.getByTestId('ext-paired')).toBeVisible();
  expect(api.requestsTo('/api/v1/me/tokens/pairing-code', 'POST')).toHaveLength(1);
  const sent = await page.evaluate(() => (window as unknown as { __sent: unknown[] }).__sent);
  expect(sent).toContainEqual({ type: 'shelfy.pair', code: 'p'.repeat(43) });
  await expect(page.getByTestId('ext-state')).toHaveAttribute('data-connected', 'false');

  api.tokens.push({
    id: 'tok_ext',
    kind: 'extension',
    label: 'Chrome on macOS',
    scopes: ['ingest'],
    createdAt: 1,
    lastUsedAt: null,
    expiresAt: null,
  });
  await page.reload();
  const row = page.getByTestId('ext-tokens-row');
  await expect(row).toContainText('Chrome on macOS');
  await row.getByTestId('ext-tokens-revoke').click();
  await row.getByRole('button', { name: 'Confirm' }).click();
  await expect(page.getByTestId('ext-tokens-empty')).toBeVisible();
  expect(api.requestsTo('/api/v1/me/tokens/tok_ext', 'DELETE')).toHaveLength(1);
});

test('shows the Shortcut token once', async ({ page, api }) => {
  await page.goto('/settings/connections');
  await page.getByTestId('sc-new').click();
  await page.getByTestId('sc-label').fill('Phone');
  await page.getByTestId('sc-submit').click();
  await expect(page.getByTestId('sc-value')).toHaveValue('shx_e2e_secret');
  expect(api.requestsTo('/api/v1/me/tokens', 'POST')[0].body).toEqual({
    kind: 'shortcut',
    label: 'Phone',
  });
  await expect(page.getByTestId('sc-tokens-row')).toContainText('Phone');
  await page.getByTestId('sc-done').click();
  await expect(page.getByTestId('sc-value')).toHaveCount(0);
  await page.reload();
  await expect(page.getByTestId('sc-value')).toHaveCount(0);
  await expect(page.getByTestId('sc-tokens-row')).toContainText('Phone');
});
