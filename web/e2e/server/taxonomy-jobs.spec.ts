import { expect, test } from '@playwright/test';
import { E2E } from './env';
import { newContext, signInWithLink, shot } from './support';

test('taxonomy jobs show progress, preserve review decisions, and cancel their own run', async ({
  browser,
}) => {
  const context = await newContext(browser);
  const page = await context.newPage();
  try {
    await signInWithLink(page, E2E.ownerEmail);
    await page.goto('/ai/tags');
    await expect(page.getByTestId('aitags-dashboard')).toBeVisible();
    expect(
      (
        await fetch(`${E2E.stubUrl}/_stub/latency`, {
          method: 'PUT',
          body: JSON.stringify({ ms: 500 }),
        })
      ).ok,
    ).toBe(true);
    // Explicit provider refusal exercises the backend's deterministic raw-group
    // fallback. Schema-valid stub placeholder tags would be outside the allowlist.
    expect(
      (
        await fetch(`${E2E.stubUrl}/_stub/faults`, {
          method: 'POST',
          body: JSON.stringify({ fault: { kind: 'refusal' }, endpoint: 'chat', times: null }),
        })
      ).ok,
    ).toBe(true);
    const started = page.waitForResponse(
      (response) =>
        response.url().endsWith('/tag-clusters/regenerate') &&
        response.request().method() === 'POST',
    );
    await page.getByTestId('regenerate-clusters-btn').click();
    const accepted = await started;
    expect(accepted.status()).toBe(202);
    const job = await accepted.json();
    await expect(page.getByRole('progressbar', { name: 'Clusters', exact: true })).toBeVisible();
    await expect(page.getByTestId('tag-cluster-card').first()).toBeVisible({ timeout: 30000 });
    await expect(page.getByTestId('cancel-clusters-btn')).toHaveCount(0);
    const before = await (await page.request.get('/api/v1/tag-clusters')).json();
    expect(before.items.length).toBeGreaterThan(0);
    const card = page.getByTestId('tag-cluster-card').first();
    await card.getByRole('button', { name: 'Accept', exact: true }).click();
    await expect
      .poll(
        async () =>
          (await (await page.request.get('/api/v1/tag-clusters')).json()).items.filter(
            (item: { status: string }) => item.status === 'accepted',
          ).length,
      )
      .toBe(1);
    await page.reload();
    await expect(page.getByTestId('aitags-dashboard')).toBeVisible();
    await shot(page, 'taxonomy-cluster-review');
    expect((await fetch(`${E2E.stubUrl}/_stub/faults`, { method: 'DELETE' })).ok).toBe(true);
    expect(
      (
        await fetch(`${E2E.stubUrl}/_stub/latency`, {
          method: 'PUT',
          body: JSON.stringify({ ms: 3000 }),
        })
      ).ok,
    ).toBe(true);
    const proposing = page.waitForResponse(
      (response) =>
        response.url().endsWith('/tag-aliases/propose') && response.request().method() === 'POST',
    );
    await page.getByTestId('propose-aliases-btn').click();
    const aliasJob = await (await proposing).json();
    const cancelled = page.waitForResponse((response) =>
      response.url().endsWith(`/jobs/${aliasJob.id}/cancel`),
    );
    await expect(page.getByTestId('cancel-aliases-btn')).toBeVisible();
    await page.getByTestId('cancel-aliases-btn').click();
    expect((await cancelled).status()).toBe(200);
    await expect(page.getByTestId('cancel-aliases-btn')).toHaveCount(0);
    const jobs = await (await page.request.get('/api/v1/jobs?kind=ai.run')).json();
    expect(jobs.items.find((item: { id: number }) => item.id === job.id).state).toBe('succeeded');
    expect(jobs.items.find((item: { id: number }) => item.id === aliasJob.id).state).toBe(
      'cancelled',
    );
    // Connection refusal is a real node outage. The durable job requeues and
    // the UI learns its future runAt from polling, then cancels only that run.
    expect((await fetch(`${E2E.stubControlUrl}/stop`, { method: 'POST' })).ok).toBe(true);
    const waiting = page.waitForResponse(
      (response) =>
        response.url().endsWith('/tag-clusters/regenerate') &&
        response.request().method() === 'POST',
    );
    await page.getByTestId('regenerate-clusters-btn').click();
    const waitingJob = await (await waiting).json();
    await expect(page.getByText('Waiting for your AI node', { exact: true })).toBeVisible({
      timeout: 20000,
    });
    await shot(page, 'taxonomy-waiting');
    const stopped = page.waitForResponse((response) =>
      response.url().endsWith(`/jobs/${waitingJob.id}/cancel`),
    );
    await page.getByTestId('cancel-clusters-btn').click();
    expect((await stopped).status()).toBe(200);
    await expect(page.getByTestId('cancel-clusters-btn')).toHaveCount(0);
    const after = await (await page.request.get('/api/v1/tag-clusters')).json();
    expect(
      after.items.filter((item: { status: string }) => item.status === 'accepted'),
    ).toHaveLength(1);
  } finally {
    await context.close();
  }
});
