// UX-4 (UX audit GAL-3, GAL-4, GAL-8): post-card polish. A media-less social
// card prints its handle once (the hover overlay drops its own), a selected
// card shows a ring and tint above the cover, the quick-select checkbox has a
// 44 px hit area on narrow screens, and the cover's alt text is short.
import { test, expect, apiPost } from './api';
import { MOBILE_VIEWPORT } from '../playwright.config';

test.afterEach(({ api }) => {
  expect(api.thirdParty, 'requests outside the app').toEqual([]);
});

const fallbackPost = (): ReturnType<typeof apiPost> =>
  apiPost({
    key: 'ig_fallback',
    mediaType: 'image',
    mediaCount: 1,
    caption: 'A long caption that must never become alt text',
    media: [{ type: 'image', url: 'https://example.invalid/x.jpg' }] as never,
  });

test('fallback card: hover overlay does not repeat the handle (GAL-3)', async ({ page, api }) => {
  api.posts.length = 0;
  api.posts.push(fallbackPost());
  await page.goto('/');
  const card = page.getByTestId('post-card').first();
  await expect(card.getByTestId('social-fallback')).toBeVisible();
  await card.hover();
  await expect(card.getByTestId('post-card-overlay')).toBeVisible();
  // One handle in the card, not two; no centered platform icon in the fallback.
  await expect(card.getByText('@studio.example')).toHaveCount(1);
  await expect(card.getByTestId('social-fallback').locator('svg')).toHaveCount(0);
});

test('selected card shows a ring overlay above the cover (GAL-4)', async ({ page }) => {
  await page.goto('/');
  const card = page.getByTestId('post-card').first();
  await card.hover();
  await card.getByTestId('quick-select-checkbox').click();
  const selected = page.getByTestId('post-card').first();
  await expect(selected).toHaveAttribute('data-selected', 'true');
  const overlay = selected.getByTestId('selected-overlay');
  await expect(overlay).toBeAttached();
  const s = await overlay.evaluate((el) => {
    const c = getComputedStyle(el);
    return { shadow: c.boxShadow, bg: c.backgroundColor, z: c.zIndex };
  });
  expect(s.shadow).toContain('inset');
  expect(s.bg).not.toBe('rgba(0, 0, 0, 0)');
  expect(s.z).toBe('20');
});

test('quick-select has a 44 px hit area on narrow (§4)', async ({ page }) => {
  await page.setViewportSize(MOBILE_VIEWPORT);
  await page.goto('/');
  const card = page.getByTestId('post-card').first();
  await card.focus();
  const box = await card.getByTestId('quick-select-checkbox').evaluate((el) => {
    const r = getComputedStyle(el, '::after');
    return { w: parseFloat(r.width), h: parseFloat(r.height) };
  });
  expect(box.w).toBeGreaterThanOrEqual(44);
  expect(box.h).toBeGreaterThanOrEqual(44);
});

test('cover alt is a short label, not the caption', async ({ page, api }) => {
  api.posts.length = 0;
  api.posts.push(fallbackPost());
  await page.goto('/');
  await expect(page.getByTestId('post-card')).toHaveCount(1);
  const alts = await page
    .getByTestId('post-card')
    .locator('img')
    .evaluateAll((els) => els.map((e) => e.getAttribute('alt')));
  for (const alt of alts) expect(alt ?? '').not.toContain('long caption');
});
