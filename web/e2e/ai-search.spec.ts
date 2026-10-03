import { test, expect, OWNER, apiPost, apiDetail } from './api';
import type { Page } from '@playwright/test';
const result = {
  tags: { general: ['design'], specific: ['lamp'] },
  keywords: ['brass'],
  remove: [],
  modelUsed: true,
};
const answer = (fallback = false) =>
  `event: run\ndata: {"runId":"synthetic-chat-run"}\n\nevent: token\ndata: {"text":"Lamp suggestions."}\n\nevent: result\ndata: ${JSON.stringify({ ...result, modelUsed: !fallback, ...(fallback ? { replyCode: 'suggestions' } : {}) })}\n\n`;
async function prepare(page: Page, fallback = false) {
  const chats: Record<string, unknown>[] = [];
  const searches: URLSearchParams[] = [];
  await page.route('**/api/v1/me', (route) =>
    route.fulfill({
      json: { ...OWNER, capabilities: { ...OWNER.capabilities, 'ai.tasks': true } },
    }),
  );
  await page.route('**/api/v1/me/settings', (route) =>
    route.fulfill({
      json: { language: 'en', aiRouting: { chat: 'operator' }, archiveAssetTypes: {} },
    }),
  );
  await page.route('**/api/v1/me/providers', (route) =>
    route.fulfill({
      json: fallback
        ? []
        : ['operator', 'custom'].map((id) => ({
            id,
            label: id === 'operator' ? 'Local node' : 'My BYOK',
            configured: true,
            managed: id === 'operator',
            models: { text: 'stub-chat' },
            status: 'ok',
          })),
    }),
  );
  await page.route('**/api/v1/me/usage/ai*', (route) => route.fulfill({ json: { days: [] } }));
  const posts = [
    apiPost({ key: 'chat_1', caption: 'Brass lamp one', aiTags: ['design', 'lamp'] }),
    apiPost({ key: 'chat_2', caption: 'Brass lamp two', aiTags: ['design', 'lamp'] }),
  ];
  for (const post of posts)
    await page.route(`**/api/v1/posts/${post.key}`, (route) =>
      route.fulfill({ json: apiDetail(post) }),
    );
  await page.route('**/api/v1/search?*', (route) => {
    searches.push(new URL(route.request().url()).searchParams);
    return route.fulfill({ json: { items: posts, total: posts.length, nextCursor: null } });
  });
  await page.route('**/api/v1/search/chat', (route) => {
    chats.push(route.request().postDataJSON());
    return route.fulfill({ contentType: 'text/event-stream', body: answer(fallback) });
  });
  await page.goto('/ai/search');
  await expect(page.getByTestId('aisearch-view')).toBeVisible();
  return { chats, searches };
}
async function ask(page: Page, text = 'brass lamp') {
  await page.getByTestId('chat-input').fill(text);
  await page.getByTestId('chat-send-btn').click();
  await expect(page.getByTestId('chat-message-assistant').last()).toBeVisible();
}
test('web chat streams a result, switches provider and scope, applies filters and opens consecutive posts', async ({
  page,
}) => {
  const { chats, searches } = await prepare(page);
  await expect(page.getByTestId('nav-aisearch')).toHaveAttribute('aria-current', 'page');
  await expect(page.getByTestId('nav-aiqueue')).toHaveCount(0);
  await page.getByTestId('chat-provider-select').selectOption('custom');
  await page.getByTestId('aisearch-view').getByTestId('source-web').click();
  await ask(page);
  await expect(page.getByTestId('chat-message-assistant')).toContainText('Lamp suggestions.');
  await expect(page.getByTestId('aisearch-view').getByTestId('post-card')).toHaveCount(2);
  expect(chats[0]).toMatchObject({ scope: 'sites', providerId: 'custom' });
  expect(searches.at(-1)?.getAll('tags')).toEqual(['design', 'lamp']);
  expect(searches.at(-1)?.get('q')).toBe('brass');
  await page.getByRole('button', { name: 'and', exact: true }).click();
  await expect.poll(() => searches.at(-1)?.get('tagMode')).toBe('and');
  await page
    .getByTestId('proposed-tag-chip')
    .filter({ hasText: 'lamp' })
    .getByRole('button')
    .click();
  await expect(page.getByTestId('active-tag-chip')).toHaveCount(1);
  await page.getByTestId('apply-message-tags-btn').click();
  await expect(page.getByTestId('active-tag-chip')).toHaveCount(2);
  await page.getByTestId('aisearch-view').getByTestId('post-card').first().click();
  await expect(page.getByTestId('post-modal')).toBeVisible();
  await page.getByTestId('post-modal-next').click();
  await expect(page.getByTestId('post-modal')).toContainText('Brass lamp two');
  await page.getByTestId('post-modal-prev').click();
  await expect(page.getByTestId('post-modal')).toContainText('Brass lamp one');
  await page.getByTestId('post-modal-close').click();
  const markdown = page.waitForEvent('download');
  await page.getByRole('button', { name: /markdown/i }).click();
  expect((await markdown).suggestedFilename()).toMatch(/^shelfy-aisearch-.*\.md$/);
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.getByRole('button', { name: 'Copy links', exact: true }).click();
  await expect
    .poll(() => page.evaluate(() => navigator.clipboard.readText()))
    .toContain('https://www.instagram.com/');
  await page.getByTestId('search-promote-collection-btn').click();
  await page.getByTestId('collection-name-input').fill('Chat findings');
  await page.getByTestId('collection-save').click();
  await expect(page.getByTestId('collection-name-input')).toHaveCount(0);
  await page.getByTestId('chat-reset-btn').click();
  await expect(page.getByTestId('chat-message-assistant')).toHaveCount(0);
  await expect(page.getByTestId('active-tag-chip')).toHaveCount(0);
});
for (const viewport of [
  { width: 375, height: 812 },
  { width: 768, height: 1024 },
]) {
  test.describe(`fallback at ${viewport.width}px`, () => {
    test.use({ viewport });
    test('keeps chat and filtered results reachable without local model actions or overflow', async ({
      page,
    }) => {
      const { chats, searches } = await prepare(page, true);
      await expect(page.getByTestId('model-download-btn')).toHaveCount(0);
      await expect(page.getByTestId('chat-mic-btn')).toBeDisabled();
      await ask(page);
      await expect(page.getByTestId('chat-message-assistant')).toContainText(
        'Here are suggested filters from your archive.',
      );
      await expect(page.getByTestId('chat-message-assistant')).not.toContainText(
        'Lamp suggestions.',
      );
      expect(chats[0].scope).toBe('all');
      await page.getByTestId('chat-mobile-results').click();
      await expect(page.getByTestId('aisearch-view').getByTestId('post-card')).toHaveCount(2);
      await page.getByTestId('source-social').click();
      await expect.poll(() => searches.at(-1)?.get('scope')).toBe('social');
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(
        true,
      );
      await page.screenshot({ path: `/tmp/shelfy-p3-22-${viewport.width}.png` });
      await page.getByTestId('chat-mobile-chat').click();
      await expect(page.getByTestId('chat-input')).toBeVisible();
      await expect(page.getByTestId('chat-message-assistant')).toBeVisible();
    });
  });
}
