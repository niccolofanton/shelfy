import { expect, test } from '@playwright/test';
import { E2E } from './env';
import { newContext, signInWithLink, shot } from './support';

const headers = { 'X-Shelfy-Client': 'web', Origin: E2E.origin };
test('catalog queue confirms twenty posts, persists analyses and survives provider stop/restart and cancel', async ({
  browser,
}) => {
  test.setTimeout(240_000);
  const context = await newContext(browser);
  const page = await context.newPage();
  try {
    await signInWithLink(page, E2E.queueEmail);
    await page.goto('/ai/queue');
    const queue = page.getByTestId('web-ai-queue');
    await expect(queue.getByTestId('queue-counts')).toContainText('Missing: 20');
    await queue.getByRole('button', { name: 'Analyze missing', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: 'Analyze posts' });
    await expect(dialog.getByTestId('analyze-estimate')).toContainText('Analyzable20');
    const before = await page.request.get('/api/v1/ai/queue');
    expect((await before.json()).counts.pending).toBe(0);
    await shot(page, 'ai-queue-estimate');
    await dialog.getByRole('button', { name: 'Confirm analysis' }).click();
    await expect(dialog).toHaveCount(0);
    await expect(queue.getByTestId('queue-counts')).toContainText('Completed: 20', {
      timeout: 120_000,
    });
    await page.reload();
    await expect(queue.getByTestId('queue-counts')).toContainText('Completed: 20');
    await queue.getByRole('button', { name: 'Completed', exact: true }).click();
    const firstKey = await queue.locator('li button').first().textContent();
    expect(firstKey).toBeTruthy();
    const posts = await page.request.get('/api/v1/posts?limit=50');
    const persisted = await posts.json();
    expect(persisted.items).toHaveLength(20);
    expect(
      persisted.items.every(
        (post: { aiStatus: string; aiDescription: string }) =>
          post.aiStatus === 'done' && !!post.aiDescription,
      ),
    ).toBe(true);
    await shot(page, 'ai-queue-complete');
    // Pause before re-enqueueing so that stop/restart cannot race a fast stub.
    await queue.getByRole('button', { name: 'Pause', exact: true }).click();
    await expect(queue.getByRole('button', { name: 'Resume', exact: true })).toBeEnabled();
    await queue.getByRole('button', { name: 'Reanalyze all', exact: true }).click();
    await dialog.getByRole('button', { name: 'Confirm analysis' }).click();
    await expect(dialog).toHaveCount(0);
    expect((await fetch(`${E2E.stubControlUrl}/stop`, { method: 'POST' })).status).toBe(204);
    await queue.getByRole('button', { name: 'Resume', exact: true }).click();
    await expect(queue).toContainText('Waiting for the provider', { timeout: 75_000 });
    await expect(queue.getByTestId('queue-counts')).toContainText('Queued: 20');
    const banner = await page.getByTestId('provider-status-banner').boundingBox();
    const heading = await queue.getByRole('heading').boundingBox();
    expect(heading!.y).toBeGreaterThanOrEqual(banner!.y + banner!.height);
    await shot(page, 'ai-queue-offline');
    expect((await fetch(`${E2E.stubControlUrl}/start`, { method: 'POST' })).status).toBe(204);
    await expect(queue.getByTestId('queue-counts')).toContainText('Completed: 20', {
      timeout: 120_000,
    });
    // Cancel pending regeneration: the previous persistent AI layer survives.
    await queue.getByRole('button', { name: 'Pause', exact: true }).click();
    await expect(queue.getByRole('button', { name: 'Resume', exact: true })).toBeEnabled();
    await queue.getByRole('button', { name: 'Reanalyze all', exact: true }).click();
    await dialog.getByRole('button', { name: 'Confirm analysis' }).click();
    await expect(dialog).toHaveCount(0);
    await queue.getByRole('button', { name: 'Cancel all', exact: true }).click();
    await expect(queue.getByTestId('queue-counts')).toContainText('Completed: 20');
    await page.reload();
    await expect(queue.getByTestId('queue-counts')).toContainText('Completed: 20');
    // Neutral paused state for repeatable runs.
    const response = await page.request.post('/api/v1/queues/ai.drain/resume', { headers });
    expect(response.ok()).toBe(true);
  } finally {
    await fetch(`${E2E.stubControlUrl}/start`, { method: 'POST' });
    await context.close();
  }
});

test('gallery selection and single-post previews do not enqueue; AI clears use the persistent bulk API', async ({
  browser,
}) => {
  test.setTimeout(120_000);
  const context = await newContext(browser);
  const page = await context.newPage();
  try {
    await signInWithLink(page, E2E.queueEmail);
    await page.goto('/ai/queue');
    const queue = page.getByTestId('web-ai-queue');
    await expect(queue.getByTestId('queue-counts')).toBeVisible();
    const snapshot = await (await page.request.get('/api/v1/ai/queue')).json();
    if (snapshot.counts.done !== 20) {
      await queue.getByRole('button', { name: 'Reanalyze all', exact: true }).click();
      await page
        .getByRole('dialog', { name: 'Analyze posts' })
        .getByRole('button', { name: 'Confirm analysis' })
        .click();
    }
    await expect(queue.getByTestId('queue-counts')).toContainText('Completed: 20', {
      timeout: 30_000,
    });
    await page.goto('/');
    const gallery = page.getByTestId('gallery-view');
    await expect(gallery.getByTestId('post-card').first()).toBeVisible();
    await gallery.getByTestId('select-toggle').click();
    await gallery.getByTestId('select-all-matching').click();
    await gallery.getByTestId('bulk-actions').click();
    const bulkPreview = page.waitForRequest((request) =>
      request.url().endsWith('/api/v1/ai/analyze'),
    );
    await page.getByTestId('bulk-analyze').click();
    const bulkRequest = (await bulkPreview).postDataJSON();
    expect(bulkRequest.estimateOnly).toBe(true);
    expect(bulkRequest.selector.keys).toHaveLength(20);
    const preview = page.getByRole('dialog', { name: 'Analyze posts' });
    await expect(preview.getByTestId('analyze-estimate')).toContainText('Analyzable20');
    await preview.getByRole('button', { name: 'Close', exact: true }).click();
    await gallery.getByTestId('select-cancel').click();
    await gallery.getByTestId('post-card').first().click();
    const modal = page.getByTestId('post-modal');
    const control = modal.getByTestId('web-post-analysis');
    await expect(control.getByRole('button', { name: 'Regenerate' })).toBeEnabled();
    const singlePreview = page.waitForRequest((request) =>
      request.url().endsWith('/api/v1/ai/analyze'),
    );
    await control.getByRole('button', { name: 'Regenerate' }).click();
    const single = (await singlePreview).postDataJSON();
    expect(single.estimateOnly).toBe(true);
    expect(single.selector.keys).toHaveLength(1);
    await expect(preview.getByTestId('analyze-estimate')).toContainText('Analyzable1');
    await preview.getByRole('button', { name: 'Close', exact: true }).click();
    const unchanged = await page.request.get('/api/v1/ai/queue');
    expect((await unchanged.json()).counts).toMatchObject({ pending: 0, analyzing: 0, done: 20 });
    await modal.getByTestId('post-modal-ai-more').click();
    await page.getByTestId('post-modal-delete-description').click();
    const cleared = page.waitForResponse(
      (response) =>
        response.url().endsWith('/api/v1/posts/bulk') && response.request().method() === 'POST',
    );
    await page.getByTestId('post-modal-delete-description').click();
    expect((await cleared).status()).toBe(200);
    await expect(control.getByRole('button', { name: 'Analyze', exact: true })).toBeEnabled();
    await modal.getByTestId('post-modal-close').click();
    await page.reload();
    const stored = await page.request.get('/api/v1/posts?limit=50');
    const affected = (await stored.json()).items.find(
      (post: { key: string }) => post.key === single.selector.keys[0],
    );
    expect(affected.aiDescription).toBeNull();
    expect(affected.aiStatus).toBeNull();
    // Regenerate through the post UI; completed server fields replace the
    // manual-clear snapshot without having to close and reopen the modal.
    await page.goto(`/p/${encodeURIComponent(single.selector.keys[0])}`);
    await modal
      .getByTestId('web-post-analysis')
      .getByRole('button', { name: 'Analyze', exact: true })
      .click();
    await preview.getByRole('button', { name: 'Confirm analysis' }).click();
    await expect(preview).toHaveCount(0);
    await expect(modal.getByTestId('web-post-analysis')).toContainText('Completed', {
      timeout: 30_000,
    });
    await modal.getByTestId('post-modal-ai-more').click();
    await expect(page.getByTestId('post-modal-delete-description')).toBeVisible();
  } finally {
    await context.close();
  }
});

test('a member without a provider gets an actionable empty state and no model downloads', async ({
  browser,
}) => {
  const context = await newContext(browser);
  const page = await context.newPage();
  try {
    await signInWithLink(page, E2E.synthEmail);
    await page.goto('/ai/queue');
    const queue = page.getByTestId('web-ai-queue');
    await expect(queue).toContainText('No AI provider available');
    await expect(
      queue.getByRole('button', { name: 'Analyze missing', exact: true }),
    ).toBeDisabled();
    await expect(queue.getByText(/download.*model/i)).toHaveCount(0);
  } finally {
    await context.close();
  }
});
