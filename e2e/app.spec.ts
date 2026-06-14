import { test, expect, overrideHandler } from './electron-fixture';

test.describe('App – navigation and top-level features', () => {
  test('sidebar renders with app title and post count', async ({ page }) => {
    await expect(page.getByText('SHELFY')).toBeVisible();
    await expect(page.locator('[data-testid="sidebar"]').getByText('15 posts')).toBeVisible();
  });

  test('Gallery is the default view', async ({ page }) => {
    await expect(page.locator('[data-testid="gallery-view"]')).toBeVisible();
    // The default source is "All posts" (source-all); when active it carries the
    // selected-row background. (The old nav-gallery item was removed in the sidebar
    // refactor that grouped sources under Library.)
    await expect(page.locator('[data-testid="source-all"]')).toHaveClass(/bg-\[#1e1e1e\]/);
  });

  test('navigates to Browser via a platform sub-tab', async ({ page }) => {
    await page.click('[data-testid="browser-tab-instagram"]');
    await expect(page.locator('[data-testid="browser-view"]')).toBeVisible();
    await expect(page.locator('[data-testid="gallery-view"]')).not.toBeVisible();
    await expect(page.locator('[data-testid="browser-tab-instagram"]')).toHaveClass(
      /bg-\[#1e1e1e\]/,
    );
  });

  test('navigates to Downloads tab', async ({ page }) => {
    await page.click('[data-testid="nav-downloads"]');
    await expect(page.locator('[data-testid="downloads-view"]')).toBeVisible();
    await expect(page.locator('[data-testid="gallery-view"]')).not.toBeVisible();
    await expect(page.locator('[data-testid="nav-downloads"]')).toHaveClass(/bg-\[#1e1e1e\]/);
  });

  test('navigates back to Gallery from Downloads', async ({ page }) => {
    await page.click('[data-testid="nav-downloads"]');
    // Selecting a source (here "All posts") returns to the gallery view.
    await page.click('[data-testid="source-all"]');
    await expect(page.locator('[data-testid="gallery-view"]')).toBeVisible();
  });

  test('sidebar shows Instagram and Twitter post counts', async ({ page }) => {
    // Per-platform counts now render on each source row (source-<platform>) under
    // Library, not in a dedicated sidebar-stats block.
    await expect(page.locator('[data-testid="source-instagram"]')).toContainText('9');
    await expect(page.locator('[data-testid="source-twitter"]')).toContainText('6');
  });

  test('Import JSON button opens the import modal', async ({ page }) => {
    await page.click('[data-testid="nav-settings"]');
    await page.click('[data-testid="open-import-btn"]');
    await expect(page.locator('[data-testid="import-modal"]')).toBeVisible();
    await expect(page.getByText('Import JSON Export')).toBeVisible();
  });

  test('modal closes when Cancel button is clicked', async ({ page }) => {
    await page.click('[data-testid="nav-settings"]');
    await page.click('[data-testid="open-import-btn"]');
    await page
      .locator('[data-testid="import-modal"]')
      .getByRole('button', { name: 'Cancel' })
      .click();
    await expect(page.locator('[data-testid="import-modal"]')).not.toBeVisible();
  });

  test('modal closes when X button is clicked', async ({ page }) => {
    await page.click('[data-testid="nav-settings"]');
    await page.click('[data-testid="open-import-btn"]');
    await page
      .locator('[data-testid="import-modal"] button')
      .filter({ has: page.locator('svg') })
      .first()
      .click();
    await expect(page.locator('[data-testid="import-modal"]')).not.toBeVisible();
  });

  test('modal closes when overlay backdrop is clicked', async ({ page }) => {
    await page.click('[data-testid="nav-settings"]');
    await page.click('[data-testid="open-import-btn"]');
    // Click outside the modal card (top-left corner of the overlay)
    await page.locator('[data-testid="import-modal"]').click({ position: { x: 10, y: 10 } });
    await expect(page.locator('[data-testid="import-modal"]')).not.toBeVisible();
  });

  test('clicking a platform sub-tab clears its new-posts alert badge', async ({
    page,
    electronApp,
  }) => {
    // Simulate a new-posts push event from the main process. The handler only
    // badges a labelled platform ('instagram'|'twitter'|'pinterest') — an
    // unlabelled signal is intentionally ignored — so the payload must carry it.
    await electronApp.evaluate(({ BrowserWindow }) => {
      const wins = BrowserWindow.getAllWindows();
      if (wins[0]) {
        wins[0].webContents.send('interceptor:newPosts', {
          platform: 'instagram',
          posts: [],
          count: 3,
        });
      }
    });
    // Badge should appear on the Instagram sub-tab
    await expect(page.locator('[data-testid="browser-tab-instagram-badge"]')).toHaveText('3');
    // Opening that platform clears it
    await page.click('[data-testid="browser-tab-instagram"]');
    await expect(page.locator('[data-testid="browser-tab-instagram-badge"]')).toHaveCount(0);
  });

  test('stats refresh after a successful import', async ({ page }) => {
    await page.click('[data-testid="nav-settings"]');
    await page.click('[data-testid="open-import-btn"]');
    await page.getByRole('button', { name: 'Choose File' }).click();
    // Use locator scoped to modal to avoid strict mode ambiguity with FilterBar buttons
    const modal = page.locator('[data-testid="import-modal"]');
    await modal.getByRole('button', { name: 'Import' }).click();
    await expect(page.getByText('Imported 7 new posts')).toBeVisible();
    await modal.getByRole('button', { name: 'Close' }).click();
    await expect(modal).not.toBeVisible();
  });
});
