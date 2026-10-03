import { spawn } from 'node:child_process';
import { expect, test } from '@playwright/test';
import { E2E } from './env';
import { newContext, signInWithLink } from './support';

test('BYOK connects, tests synthetic data, records consent, edits and deletes without showing the key', async ({
  browser,
}) => {
  test.setTimeout(120_000);
  // A distinct IPv6 loopback host keeps the BYOK endpoint separate from the managed operator host.
  const stub = spawn(
    E2E.stubBin,
    ['--listen', `[::1]:${E2E.byokStubPort}`, '--key-env', 'SHELFY_E2E_STUB_KEY'],
    { env: process.env, stdio: ['ignore', 'ignore', 'inherit'] },
  );
  const context = await newContext(browser);
  const page = await context.newPage();
  const key = process.env.SHELFY_E2E_STUB_KEY!;
  try {
    await expect
      .poll(async () => {
        try {
          return (await fetch(`${E2E.byokStubUrl}/health`)).status;
        } catch {
          return 0;
        }
      })
      .toBe(200);
    await signInWithLink(page);
    await page.goto('/settings/ai');
    await page.getByRole('button', { name: 'Add provider', exact: true }).click();
    const dialog = page.getByRole('dialog');
    await dialog.getByLabel('Provider name', { exact: true }).fill('BYOK synthetic node');
    await dialog.getByLabel('Base URL', { exact: true }).fill(`${E2E.byokStubUrl}/v1`);
    await dialog.getByRole('button', { name: 'Continue', exact: true }).click();
    await dialog.getByLabel('API key', { exact: true }).fill(key);
    await dialog.getByLabel('Chat', { exact: true }).fill('stub-text');
    await dialog.getByLabel('Cataloging', { exact: true }).fill('stub-vision');
    await dialog.getByLabel('Tag aliases', { exact: true }).fill('stub-text');
    await dialog.getByRole('button', { name: 'Save provider', exact: true }).click();
    await expect(
      dialog.getByRole('button', { name: 'Run synthetic test', exact: true }),
    ).toBeVisible();
    expect(await page.locator('body').innerHTML()).not.toContain(key);
    await expect(dialog.locator('input[type=password]')).toHaveCount(0);
    const before = await (await page.request.get('/api/v1/me/providers')).json();
    const provider = before.find(
      (value: { label: string }) => value.label === 'BYOK synthetic node',
    );
    expect(provider.configured).toBe(true);
    expect(provider.consent).toBeUndefined();
    expect(JSON.stringify(before)).not.toContain(key);
    await dialog.getByRole('button', { name: 'Run synthetic test', exact: true }).click();
    await expect(dialog.getByRole('button', { name: 'Continue', exact: true })).toBeEnabled();
    await expect(dialog.getByText('Passed', { exact: true })).toHaveCount(4);
    await dialog.getByRole('button', { name: 'Continue', exact: true }).click();
    await expect(
      dialog.getByRole('button', { name: 'Record consent', exact: true }),
    ).toBeDisabled();
    await dialog
      .getByLabel('I consent to sending my content to this provider.', { exact: true })
      .check();
    await dialog.getByRole('button', { name: 'Record consent', exact: true }).click();
    await expect(
      dialog.getByRole('combobox', { name: 'Task to connect', exact: true }),
    ).toBeVisible();
    await dialog.getByRole('button', { name: 'Activate route', exact: true }).click();
    await expect(dialog).toHaveCount(0);
    await expect(page.getByLabel('Chat', { exact: true })).toHaveValue(provider.id);
    const after = await (await page.request.get('/api/v1/me/providers')).json();
    expect(after.find((value: { id: string }) => value.id === provider.id).consent.version).toBe(
      'ai-provider-v1',
    );
    expect(JSON.stringify(after)).not.toContain(key);
    const card = page.getByTestId(`provider-${provider.id}`);
    await card.getByRole('button', { name: 'Edit provider', exact: true }).click();
    await expect(dialog.getByLabel('Tag aliases', { exact: true })).toHaveValue('stub-text');
    await dialog.getByLabel('API key', { exact: true }).fill('synthetic-wrong-key');
    await dialog.getByRole('button', { name: 'Save provider', exact: true }).click();
    await dialog.getByRole('button', { name: 'Run synthetic test', exact: true }).click();
    await expect(page.getByTestId('provider-status-banner')).toContainText('refused its key');
    await expect(dialog.getByRole('button', { name: 'Continue', exact: true })).toBeDisabled();
    expect(await page.locator('body').innerHTML()).not.toContain('synthetic-wrong-key');
    await dialog.getByRole('button', { name: 'Close', exact: true }).click();
    await card.getByRole('button', { name: 'Delete provider', exact: true }).click();
    await expect(card).toHaveCount(0);
    const settings = await (await page.request.get('/api/v1/me/settings')).json();
    expect(Object.values(settings.aiRouting)).not.toContain(provider.id);
    // A task-specific QC/alias descriptor still exercises synthetic vision/text/schema.
    const headers = { 'X-Shelfy-Client': 'web', Origin: E2E.origin };
    expect(
      (
        await page.request.put('/api/v1/me/providers/qc-only', {
          headers,
          data: {
            kind: 'openai_compatible',
            label: 'QC synthetic node',
            baseUrl: `${E2E.byokStubUrl}/v1`,
            models: { qc: 'stub-vision', alias: 'stub-text' },
            key,
          },
        })
      ).status(),
    ).toBe(204);
    const probe = await page.request.post('/api/v1/me/providers/qc-only/test', { headers });
    expect(probe.status()).toBe(200);
    const checks = await probe.json();
    for (const name of ['models', 'text', 'vision', 'schema']) expect(checks[name].ok).toBe(true);
    expect((await page.request.delete('/api/v1/me/providers/qc-only', { headers })).status()).toBe(
      204,
    );
  } finally {
    stub.kill('SIGTERM');
    await context.close();
  }
});
