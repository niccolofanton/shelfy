import { expect, test } from '@playwright/test';
import { E2E } from './env';
import { newContext, signInWithLink, shot } from './support';

test('operator settings persist and a stopped AI node reports offline then recovers over SSE', async ({
  browser,
}) => {
  test.setTimeout(180_000);
  const context = await newContext(browser);
  const page = await context.newPage();
  try {
    await signInWithLink(page);
    await page.goto('/settings/ai');
    const operator = page.getByTestId('provider-operator');
    await expect(operator).toContainText('E2E AI node');
    await expect(operator).toContainText('Managed by the server');
    await expect(operator).toContainText('stub-vision');
    await expect(operator.locator('input,button')).toHaveCount(0);
    const providers = await page.request.get('/api/v1/me/providers');
    expect(providers.ok()).toBe(true);
    const summary = await providers.json();
    expect(summary[0].managed).toBe(true);
    expect(JSON.stringify(summary)).not.toContain(process.env.SHELFY_E2E_STUB_KEY);
    await page.getByLabel('Cataloging', { exact: true }).selectOption('operator');
    await page.getByLabel('Concurrent BYOK calls', { exact: true }).selectOption('8');
    const suggestions = page.getByLabel('Generate search suggestions', { exact: true });
    await suggestions.uncheck();
    const saved = page.waitForResponse(
      (response) =>
        response.url().endsWith('/api/v1/me/settings') && response.request().method() === 'PUT',
    );
    await page.getByRole('button', { name: 'Save preferences', exact: true }).click();
    expect((await saved).status()).toBe(200);
    await expect(page.getByText('Preferences saved.', { exact: true })).toBeVisible();
    await page.reload();
    await expect(page.getByLabel('Cataloging', { exact: true })).toHaveValue('operator');
    await expect(page.getByLabel('Concurrent BYOK calls', { exact: true })).toHaveValue('8');
    await expect(page.getByLabel('Generate search suggestions', { exact: true })).not.toBeChecked();
    await expect(page.getByTestId('ai-usage')).toContainText('No AI calls');
    await shot(page, 'ai-settings-ready');
    const stopped = await fetch(`${E2E.stubControlUrl}/stop`, { method: 'POST' });
    expect(stopped.status).toBe(204);
    await expect(page.getByTestId('provider-status-banner')).toContainText(
      'Your AI node is offline; work waits',
      { timeout: 75_000 },
    );
    await expect(operator).toContainText('Offline');
    await shot(page, 'ai-settings-offline');
    const restarted = await fetch(`${E2E.stubControlUrl}/start`, { method: 'POST' });
    expect(restarted.status).toBe(204);
    await expect(page.getByTestId('provider-status-banner')).toHaveCount(0, { timeout: 75_000 });
    await expect(operator).toContainText('Ready');
    // Keep account preferences neutral for the other real-server suites.
    await page.request.put('/api/v1/me/settings', {
      data: { aiRouting: {}, aiConcurrency: 4, aiSuggestions: true },
      headers: { 'X-Shelfy-Client': 'web', Origin: E2E.origin },
    });
  } finally {
    await fetch(`${E2E.stubControlUrl}/start`, { method: 'POST' });
    await context.close();
  }
});
