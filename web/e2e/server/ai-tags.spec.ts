import { expect, test } from '@playwright/test';
import { E2E } from './env';
import { TAGS_EMAIL } from './seedTags';
import { newContext, signInWithLink, shot } from './support';

test('Tag Explorer browses synthetic tiers and persists merge, rename and alias review', async ({
  browser,
}) => {
  const context = await newContext(browser);
  const page = await context.newPage();
  const headers = { 'X-Shelfy-Client': 'web', Origin: E2E.origin };
  try {
    await signInWithLink(page, TAGS_EMAIL);
    const posts = await (await page.request.get('/api/v1/posts?limit=2')).json();
    for (const [index, tags] of [['fixture-lamp', 'fixture-alias'], ['fixture-lamps']].entries()) {
      const result = await page.request.patch(`/api/v1/posts/${posts.items[index].key}`, {
        data: { userTags: tags },
        headers,
      });
      expect(result.ok()).toBe(true);
    }
    await page.goto('/ai/tags');
    await expect(page.getByTestId('aitags-dashboard')).toBeVisible();
    expect(
      await page.evaluate(
        () => typeof (window as unknown as { electronAPI?: unknown }).electronAPI,
      ),
    ).toBe('undefined');
    await expect(page.getByTestId('analyze-missing-btn')).toHaveCount(0);
    await page.getByTestId('aitags-tier').selectOption('general');
    await expect(page.getByTestId('aitags-tag-index').locator('button').first()).toBeVisible();
    await page.getByTestId('aitags-tier').selectOption('specific');
    await expect(page.getByTestId('aitags-tag-index').locator('button').first()).toBeVisible();
    await page.getByTestId('aitags-tier').selectOption('manual');
    const index = page.getByTestId('aitags-tag-index');
    await index.getByText('fixture-lamp', { exact: true }).click();
    await index.getByText('fixture-lamps', { exact: true }).click();
    await expect(page.getByTestId('aitags-grid')).toBeVisible();
    await page.getByRole('button', { name: 'and', exact: true }).click();
    await expect(page.getByText('No posts for this filter.', { exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'or', exact: true }).click();
    await expect(page.getByTestId('aitags-grid')).toBeVisible();
    const download = page.waitForEvent('download');
    await page.getByRole('button', { name: 'Export Markdown', exact: true }).click();
    expect((await download).suggestedFilename()).toMatch(/\.md$/);
    page.once('dialog', (dialog) => void dialog.accept('Tag results'));
    await page.getByRole('button', { name: 'Create collection from these', exact: true }).click();
    await expect(
      page.getByText('Collection "Tag results" created with 2 posts', { exact: true }),
    ).toBeVisible();
    await page.getByRole('button', { name: 'Manage', exact: true }).click();
    const modal = page.getByTestId('aitags-merge-modal');
    const suggestion = modal
      .getByTestId('tag-merge-suggestion')
      .filter({ hasText: 'fixture-lamps' });
    const merged = page.waitForResponse(
      (response) =>
        response.url().endsWith('/tags/merge') && response.request().method() === 'POST',
    );
    await suggestion.getByRole('button', { name: 'Merge', exact: true }).click();
    expect((await merged).status()).toBe(200);
    await page.reload();
    await page.getByTestId('aitags-tier').selectOption('manual');
    const stats = await (await page.request.get('/api/v1/tags?tier=manual')).json();
    const surviving = stats.items.filter((item: { tag: string }) =>
      ['fixture-lamp', 'fixture-lamps'].includes(item.tag),
    );
    expect(surviving).toHaveLength(1);
    expect(surviving[0].count).toBe(2);
    await page.getByRole('button', { name: 'Manage', exact: true }).click();
    await modal.getByPlaceholder('from', { exact: true }).fill(surviving[0].tag);
    await modal.getByPlaceholder('to', { exact: true }).fill('fixture-light');
    const renamed = page.waitForResponse(
      (response) =>
        response.url().endsWith('/tags/rename') && response.request().method() === 'POST',
    );
    await modal.getByRole('button', { name: 'Rename', exact: true }).click();
    expect((await renamed).status()).toBe(200);
    await modal.getByRole('button', { name: 'Close', exact: true }).click();
    const alias = page.getByTestId('tag-alias-card').filter({ hasText: 'fixture-alias' });
    const accepted = page.waitForResponse((response) =>
      response.url().endsWith('/fixture-alias/accept'),
    );
    await alias.getByRole('button', { name: 'Accept', exact: true }).click();
    expect((await accepted).status()).toBe(200);
    await page.reload();
    await expect(page.getByTestId('tag-alias-card')).toHaveCount(0);
    const persisted = await (await page.request.get('/api/v1/tags?tier=manual')).json();
    expect(
      persisted.items.find((item: { tag: string }) => item.tag === 'fixture-light').count,
    ).toBe(2);
    expect(
      persisted.items.find((item: { tag: string }) => item.tag === 'fixture-canonical').count,
    ).toBe(1);
    await shot(page, 'tag-explorer-review');
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(page.getByTestId('aitags-dashboard')).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(
      true,
    );
    await shot(page, 'tag-explorer-narrow');
  } finally {
    await context.close();
  }
});
