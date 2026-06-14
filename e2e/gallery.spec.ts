import { test, expect } from './electron-fixture';
import { MOCK_POSTS } from './test-data';

test.describe('Gallery – post grid and filters', () => {
  test('renders all 15 mock posts', async ({ page }) => {
    const cards = page.locator('[data-testid="post-card"]');
    await expect(cards).toHaveCount(15);
  });

  test('post count strip shows total', async ({ page }) => {
    // The total moved into the FilterBar's right-hand count span ("{n} posts").
    // The old standalone count-strip testid was dropped in the toolbar refactor.
    // Scope to the gallery view: the sidebar header also renders "15 posts".
    await expect(page.locator('[data-testid="gallery-view"]').getByText('15 posts')).toBeVisible();
  });

  test('each post card is rendered', async ({ page }) => {
    const cards = page.locator('[data-testid="post-card"]');
    await expect(cards.first()).toBeVisible();
    await expect(cards.last()).toBeVisible();
  });

  test('clicking a card opens the post modal', async ({ page }) => {
    const firstCard = page.locator('[data-testid="post-card"]').first();
    await firstCard.click();
    await expect(page.locator('[data-testid="post-modal"]')).toBeVisible();
  });

  test('post modal closes via close button', async ({ page }) => {
    await page.locator('[data-testid="post-card"]').first().click();
    await expect(page.locator('[data-testid="post-modal"]')).toBeVisible();
    await page.locator('[data-testid="post-modal-close"]').click();
    await expect(page.locator('[data-testid="post-modal"]')).toHaveCount(0);
  });

  // ── Platform filter ──────────────────────────────────────────────────────
  // Platform selection moved out of an inline pill group into the sidebar
  // Library sources (source-all / source-<platform>): clicking a source routes
  // through App, re-applies the gallery filter and returns to the gallery view.

  test('platform filter: Instagram shows only 9 posts', async ({ page }) => {
    await page.locator('[data-testid="source-instagram"]').click();
    const cards = page.locator('[data-testid="post-card"]');
    await expect(cards).toHaveCount(9);
  });

  test('platform filter: X / Twitter shows only 6 posts', async ({ page }) => {
    await page.locator('[data-testid="source-twitter"]').click();
    const cards = page.locator('[data-testid="post-card"]');
    await expect(cards).toHaveCount(6);
  });

  test('platform filter: All restores all posts', async ({ page }) => {
    await page.locator('[data-testid="source-instagram"]').click();
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(9);

    await page.locator('[data-testid="source-all"]').click();
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(15);
  });

  test('platform filter active row is highlighted', async ({ page }) => {
    await page.locator('[data-testid="source-instagram"]').click();
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(9);
    // The Instagram source button sits inside a row div that carries the active
    // selection background (bg-[#1e1e1e]); the highlight is on the wrapping row.
    const row = page.locator('[data-testid="source-instagram"]').locator('..');
    await expect(row).toHaveClass(/bg-\[#1e1e1e\]/);
  });

  // ── Media type filter ────────────────────────────────────────────────────
  // Media-type / download / AI-tag filters live in the right-hand FilterDrawer,
  // opened by the FilterBar's "Filters" toggle (filters-toggle). The drawer's
  // media-type segmented control is scoped by the drawer-mediatype testid.

  const openMediaTypeFilter = (page: import('@playwright/test').Page) =>
    page.locator('[data-testid="drawer-mediatype"]');

  test('media type filter: Video', async ({ page }) => {
    await page.locator('[data-testid="filters-toggle"]').click();
    await openMediaTypeFilter(page).getByRole('button', { name: 'Video' }).click();
    const videoCount = MOCK_POSTS.filter((p) => p.mediaType === 'video').length;
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(videoCount);
  });

  test('media type filter: Image', async ({ page }) => {
    await page.locator('[data-testid="filters-toggle"]').click();
    await openMediaTypeFilter(page).getByRole('button', { name: 'Image' }).click();
    const imageCount = MOCK_POSTS.filter((p) => p.mediaType === 'image').length;
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(imageCount);
  });

  test('media type filter: Carousel', async ({ page }) => {
    await page.locator('[data-testid="filters-toggle"]').click();
    await openMediaTypeFilter(page).getByRole('button', { name: 'Carousel' }).click();
    const carouselCount = MOCK_POSTS.filter((p) => p.mediaType === 'carousel').length;
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(carouselCount);
  });

  test('media type filter: All restores posts', async ({ page }) => {
    await page.locator('[data-testid="filters-toggle"]').click();
    const mediatypeGroup = openMediaTypeFilter(page);
    await mediatypeGroup.getByRole('button', { name: 'Video' }).click();
    const videoCount = MOCK_POSTS.filter((p) => p.mediaType === 'video').length;
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(videoCount);
    await mediatypeGroup.getByRole('button', { name: 'All' }).click();
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(15);
  });

  // ── Combined platform + media type ───────────────────────────────────────

  test('platform + media type filter combined', async ({ page }) => {
    await page.locator('[data-testid="source-instagram"]').click();
    await page.locator('[data-testid="filters-toggle"]').click();
    await openMediaTypeFilter(page).getByRole('button', { name: 'Video' }).click();
    const count = MOCK_POSTS.filter(
      (p) => p.platform === 'instagram' && p.mediaType === 'video',
    ).length;
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(count);
  });

  // ── Search ───────────────────────────────────────────────────────────────

  test('search filters posts by text content', async ({ page }) => {
    const input = page.getByPlaceholder('Search posts...');
    await input.fill('coffee');
    // FilterBar debounces 300ms, usePosts debounces 200ms — wait for both
    await page.waitForTimeout(600);
    const coffeeCount = MOCK_POSTS.filter(
      (p) =>
        (p.text || '').toLowerCase().includes('coffee') ||
        (p.authorUsername || '').toLowerCase().includes('coffee'),
    ).length;
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(coffeeCount);
  });

  test('search filters posts by author username', async ({ page }) => {
    const input = page.getByPlaceholder('Search posts...');
    await input.fill('devdude');
    await page.waitForTimeout(600);
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(1);
  });

  test('clearing search restores all posts', async ({ page }) => {
    const input = page.getByPlaceholder('Search posts...');
    await input.fill('coffee');
    await page.waitForTimeout(600);
    await input.clear();
    await page.waitForTimeout(600);
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(15);
  });

  test('search with no matches shows empty state', async ({ page }) => {
    const input = page.getByPlaceholder('Search posts...');
    await input.fill('zzznomatchzzz');
    await page.waitForTimeout(600);
    await expect(page.locator('[data-testid="empty-state"]')).toBeVisible();
    await expect(page.getByText('No posts found')).toBeVisible();
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(0);
  });

  // ── Filter change resets limit ───────────────────────────────────────────

  test('platform filter and search combine: only matching posts shown', async ({ page }) => {
    const input = page.getByPlaceholder('Search posts...');

    await input.fill('coffee');
    await page.waitForTimeout(600);
    // ig_007 (coffee_culture / "Perfect morning espresso #coffee") is the only match
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(1);

    // Switching to Instagram keeps the search — still just ig_007
    await page.locator('[data-testid="source-instagram"]').click();
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(1);

    // Clear search while platform=instagram → all 9 Instagram posts
    await input.clear();
    await page.waitForTimeout(600);
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(9);
  });

  // ── FilterBar total count ────────────────────────────────────────────────

  test('FilterBar post count matches card count', async ({ page }) => {
    // The total in the FilterBar (right side, "{n} posts") should equal the
    // total reported by usePosts. Scope to the gallery view: the sidebar header
    // also renders "15 posts".
    await expect(page.locator('[data-testid="gallery-view"]').getByText('15 posts')).toBeVisible();
  });
});
