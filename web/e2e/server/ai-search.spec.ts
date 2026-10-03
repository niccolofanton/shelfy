// P3-22 real HTTP acceptance. Requires a server binary containing P3-14;
// no real provider, personal archive or credentials. The runner seeds chatEmail.
import { spawn } from 'node:child_process';
import { expect, test } from '@playwright/test';
import { E2E } from './env';
import { newContext, signInWithLink } from './support';
import type { components } from '../../src/api/schema';
type Schemas = components['schemas'];
test('chat filters a synthetic archive with no route, streaming BYOK, and a stopped provider', async ({
  browser,
}) => {
  test.setTimeout(120_000);
  const context = await newContext(browser);
  const page = await context.newPage();
  // A distinct loopback host: BYOK must never target the operator's host.
  const port = E2E.apiPort + 4;
  const base = `http://127.0.0.2:${port}`;
  const stub = spawn(
    E2E.stubBin,
    ['--listen', `127.0.0.2:${port}`, '--key-env', 'SHELFY_E2E_STUB_KEY'],
    { env: process.env, stdio: 'ignore' },
  );
  const stop = async () => {
    if (stub.exitCode !== null) return;
    await new Promise<void>((resolve) => {
      stub.once('exit', () => resolve());
      stub.kill('SIGTERM');
    });
  };
  try {
    await expect
      .poll(async () => {
        try {
          return (await fetch(`${base}/health`)).status;
        } catch {
          return 0;
        }
      })
      .toBe(200);
    await signInWithLink(page, E2E.chatEmail);
    const rows = (await (
      await page.request.get('/api/v1/posts?limit=40')
    ).json()) as Schemas['PostPage'];
    const tag = rows.items.flatMap((p) => p.aiTags).find((t) => t.length >= 2);
    expect(tag, 'AI fixture needs searchable tags').toBeTruthy();
    await page.goto('/ai/search');
    async function ask(modelUsed: boolean) {
      await page.getByTestId('chat-input').fill(tag!);
      const chat = page.waitForResponse(
        (r) => r.url().endsWith('/api/v1/search/chat') && r.request().method() === 'POST',
      );
      const search = page.waitForResponse((r) => r.url().includes('/api/v1/search?'));
      await page.getByTestId('chat-send-btn').click();
      const response = await chat;
      expect(response.status()).toBe(200);
      const wire = await response.text();
      expect(wire).toContain('event: run');
      expect(wire).not.toContain('\nid:');
      const payload = JSON.parse(
        wire.match(/event: result\r?\ndata: (.+)/)![1],
      ) as Schemas['ResultEvent'];
      expect(payload.modelUsed).toBe(modelUsed);
      if (modelUsed) expect(wire).toContain('event: token');
      else expect(payload.replyCode).toBeDefined();
      const found = (await (await search).json()) as Schemas['SearchPage'];
      expect(found.total).toBeGreaterThan(0);
      await expect(
        page.getByTestId('aisearch-view').getByTestId('post-card').first(),
      ).toBeVisible();
      await page.getByTestId('chat-reset-btn').click();
    }
    await ask(false);
    const headers = { 'X-Shelfy-Client': 'web', Origin: E2E.origin };
    const installed = await page.request.put('/api/v1/me/providers/chat-e2e', {
      headers,
      data: {
        kind: 'openai_compatible',
        label: 'Chat synthetic node',
        baseUrl: `${base}/v1`,
        models: { chat: 'stub-text' },
        key: process.env.SHELFY_E2E_STUB_KEY,
      },
    });
    expect(installed.status()).toBe(204);
    expect(
      (
        await page.request.post('/api/v1/me/providers/chat-e2e/consent', {
          headers,
          data: { version: 'ai-provider-v1' },
        })
      ).status(),
    ).toBe(204);
    await page.reload();
    await expect(page.getByTestId('chat-provider-select')).toHaveValue('chat-e2e');
    await ask(true);
    await stop();
    await ask(false);
    // Cached breaker/offline path remains usable without automatic chat replay.
    await ask(false);
    expect((await page.request.delete('/api/v1/me/providers/chat-e2e', { headers })).status()).toBe(
      204,
    );
  } finally {
    await stop();
    await context.close();
  }
});
