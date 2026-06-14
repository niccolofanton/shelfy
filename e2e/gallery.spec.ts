import { test, expect } from './electron-fixture';
import { MOCK_POSTS } from './test-data';

test.describe('Gallery – post grid and filters', () => {
  test('renders all 15 mock posts', async ({ page }) => {
    const cards = page.locator('[data-testid="post-card"]');
    await expect(cards).toHaveCount(15);
  });

  test('post count strip shows total', async ({ page }) => {
    await expect(page.locator('[data-testid="count-strip"]')).toContainText('15');
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

  test('platform filter: Instagram shows only 9 posts', async ({ page }) => {
    await page.locator('[data-testid="platform-filter-group"]').getByRole('button', { name: 'Instagram' }).click();
    const cards = page.locator('[data-testid="post-card"]');
    await expect(cards).toHaveCount(9);
  });

  test('platform filter: X / Twitter shows only 6 posts', async ({ page }) => {
    await page.locator('[data-testid="platform-filter-group"]').getByRole('button', { name: 'X / Twitter' }).click();
    const cards = page.locator('[data-testid="post-card"]');
    await expect(cards).toHaveCount(6);
  });

  test('platform filter: All restores all posts', async ({ page }) => {
    const platformGroup = page.locator('[data-testid="platform-filter-group"]');
    await platformGroup.getByRole('button', { name: 'Instagram' }).click();
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(9);

    await platformGroup.getByRole('button', { name: 'All' }).click();
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(15);
  });

  test('platform filter active pill is highlighted', async ({ page }) => {
    const btn = page.locator('[data-testid="platform-filter-group"]').getByRole('button', { name: 'Instagram' });
    await btn.click();
    await expect(btn).toHaveClass(/bg-\[#7B5CFF\]/);
  });

  // ── Media type filter ────────────────────────────────────────────────────

  test('media type filter: Video', async ({ page }) => {
    await page.locator('[data-testid="filters-toggle"]').click();
    await page.locator('[data-testid="mediatype-filter-group"]').getByRole('button', { name: 'Video' }).click();
    const videoCount = MOCK_POSTS.filter((p) => p.mediaType === 'video').length;
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(videoCount);
  });

  test('media type filter: Image', async ({ page }) => {
    await page.locator('[data-testid="filters-toggle"]').click();
    await page.locator('[data-testid="mediatype-filter-group"]').getByRole('button', { name: 'Image' }).click();
    const imageCount = MOCK_POSTS.filter((p) => p.mediaType === 'image').length;
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(imageCount);
  });

  test('media type filter: Carousel', async ({ page }) => {
    await page.locator('[data-testid="filters-toggle"]').click();
    await page.locator('[data-testid="mediatype-filter-group"]').getByRole('button', { name: 'Carousel' }).click();
    const carouselCount = MOCK_POSTS.filter((p) => p.mediaType === 'carousel').length;
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(carouselCount);
  });

  test('media type filter: All (second All pill) restores posts', async ({ page }) => {
    await page.locator('[data-testid="filters-toggle"]').click();
    const mediatypeGroup = page.locator('[data-testid="mediatype-filter-group"]');
    await mediatypeGroup.getByRole('button', { name: 'Video' }).click();
    await mediatypeGroup.getByRole('button', { name: 'All' }).click();
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(15);
  });

  // ── Combined platform + media type ───────────────────────────────────────

  test('platform + media type filter combined', async ({ page }) => {
    await page.locator('[data-testid="platform-filter-group"]').getByRole('button', { name: 'Instagram' }).click();
    await page.locator('[data-testid="filters-toggle"]').click();
    await page.locator('[data-testid="mediatype-filter-group"]').getByRole('button', { name: 'Video' }).click();
    const count = MOCK_POSTS.filter((p) => p.platform === 'instagram' && p.mediaType === 'video').length;
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(count);
  });

  // ── Search ───────────────────────────────────────────────────────────────

  test('search filters posts by text content', async ({ page }) => {
    const input = page.getByPlaceholder('Search posts...');
    await input.fill('coffee');
    // FilterBar debounces 300ms, usePosts debounces 200ms — wait for both
    await page.waitForTimeout(600);
    const coffeeCount = MOCK_POSTS.filter((p) =>
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
    const platformGroup = page.locator('[data-testid="platform-filter-group"]');

    await input.fill('coffee');
    await page.waitForTimeout(600);
    // ig_007 (coffee_culture / "Perfect morning espresso #coffee") is the only match
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(1);

    // Switching to Instagram keeps the search — still just ig_007
    await platformGroup.getByRole('button', { name: 'Instagram' }).click();
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(1);

    // Clear search while platform=instagram → all 9 Instagram posts
    await input.clear();
    await page.waitForTimeout(600);
    await expect(page.locator('[data-testid="post-card"]')).toHaveCount(9);
  });

  // ── Reload button (FilterBar) ────────────────────────────────────────────

  test('FilterBar post count matches card count', async ({ page }) => {
    // The count in FilterBar (right side) should equal total from usePosts
    const countText = await page.locator('.ml-auto.text-sm.text-gray-500').textContent();
    expect(countText).toContain('15');
  });
});
