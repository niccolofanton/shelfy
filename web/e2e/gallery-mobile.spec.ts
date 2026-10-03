// UX-3 (docs/web-port/reviews/ux-audit.md): the gallery's mobile toolbar,
// filter sheet, bottom selection bar and per-case empty states.
//   GAL-1  the toolbar fits 390px and the grid never slides sideways
//   GAL-11 the selection bar sits at the bottom on narrow
//   GAL-5  below 1280px the filter panel overlays the grid (a backdrop)
//   GAL-2  one empty state per situation (search / filters / folder / library)
// Mocked API (web/e2e/api.ts). Narrow at the audit's 390×844 and 412×915; the
// filter overlay at 1024×768. Screenshots for the lane report: set UX3_SHOT_DIR
// to an absolute path outside the repo.
import path from 'node:path';
import type { Page } from '@playwright/test';
import { test, expect } from './api';

const NARROW = [
  { name: 'iphone', viewport: { width: 390, height: 844 } },
  { name: 'android', viewport: { width: 412, height: 915 } },
];
const SHOT_DIR = process.env.UX3_SHOT_DIR;

test.afterEach(({ api }) => {
  expect(api.thirdParty, 'requests outside the app').toEqual([]);
});

// The largest scrollLeft of the gallery column and every scrollable descendant:
// the P0 was the toolbar overflowing and the column sliding to scrollLeft 358.
async function maxScrollLeft(page: Page): Promise<number> {
  return page.evaluate(() => {
    const root = document.querySelector('[data-testid="gallery-view"]');
    if (!root) return -1;
    let max = 0;
    for (const el of [root, ...root.querySelectorAll('*')]) {
      max = Math.max(max, (el as HTMLElement).scrollLeft || 0);
    }
    return max;
  });
}

for (const { name, viewport } of NARROW) {
  test.describe(`narrow: ${name} (${viewport.width}x${viewport.height})`, () => {
    test.use({ viewport, hasTouch: true });

    test('the toolbar fits and every control is tappable; the grid never slides (GAL-1)', async ({
      page,
    }) => {
      await page.goto('/');
      await expect(page.getByTestId('post-card').first()).toBeVisible();

      // Every toolbar control is inside the viewport (nothing off to the right).
      for (const id of ['sidebar-open', 'filters-toggle', 'select-toggle']) {
        const box = (await page.getByTestId(id).boundingBox())!;
        expect(box, id).toBeTruthy();
        expect(box.x, `${id} starts off-screen`).toBeGreaterThanOrEqual(0);
        expect(box.x + box.width, `${id} runs off the right edge`).toBeLessThanOrEqual(
          viewport.width + 0.5,
        );
      }

      // Focusing any control (keyboard / a11y) must not scroll the column.
      for (const id of ['sidebar-open', 'filters-toggle', 'select-toggle']) {
        await page.getByTestId(id).focus();
        expect(await maxScrollLeft(page), `${id} slid the grid`).toBe(0);
      }
      await page.getByRole('searchbox', { name: 'Search posts' }).focus();
      expect(await maxScrollLeft(page)).toBe(0);
    });

    test('the filter sheet opens from the bottom with a "Show N posts" footer (GAL-1, §4)', async ({
      page,
    }) => {
      await page.goto('/');
      await expect(page.getByTestId('post-card').first()).toBeVisible();
      await page.getByTestId('filters-toggle').click();

      const sheet = page.getByTestId('filter-drawer');
      await expect(sheet).toBeVisible();
      await expect(sheet).toHaveAttribute('role', 'dialog');
      await page.waitForTimeout(400); // let the slide-up settle before measuring
      // A bottom sheet: it reaches the bottom edge and, capped at 85dvh, leaves a
      // gap at the top (its rounded corners) rather than filling the screen.
      const box = (await sheet.boundingBox())!;
      expect(box.y + box.height).toBeGreaterThanOrEqual(viewport.height - 1);
      expect(box.y).toBeGreaterThan(20);
      // The count moved to the footer button.
      await expect(page.getByTestId('drawer-apply')).toContainText('3');
      // The View section carries the controls that left the toolbar.
      await expect(sheet.getByTestId('view-mode-toggle')).toBeVisible();
      await expect(sheet.getByTestId('grid-cols-3')).toBeVisible();
      // Tapping the footer applies (closes) the sheet.
      await page.getByTestId('drawer-apply').click();
      await expect(sheet).toBeHidden();
    });

    test('the selection bar sits at the bottom, its × not clipped (GAL-11)', async ({ page }) => {
      await page.goto('/');
      await expect(page.getByTestId('post-card').first()).toBeVisible();
      await page.getByTestId('select-toggle').click();

      const bar = page.getByTestId('selection-bar');
      await expect(bar).toBeVisible();
      await page.waitForTimeout(400); // let the fade-in-up settle before measuring
      const box = (await bar.boundingBox())!;
      expect(box.y + box.height).toBeGreaterThanOrEqual(viewport.height - 1);
      expect(box.y + box.height).toBeLessThanOrEqual(viewport.height + 1);
      // The exit (×), the count and the Actions trigger are all on the bar and
      // fully inside the viewport (the audit's × was clipped at the right edge).
      for (const id of ['select-cancel', 'selection-count', 'bulk-actions']) {
        const b = (await bar.getByTestId(id).boundingBox())!;
        expect(b, id).toBeTruthy();
        expect(b.x + b.width, `${id} clipped`).toBeLessThanOrEqual(viewport.width + 0.5);
      }
      // 44px touch targets for the two icon controls.
      const x = (await bar.getByTestId('select-cancel').boundingBox())!;
      expect(Math.round(x.height)).toBeGreaterThanOrEqual(44);

      // The bulk menu opens as a bottom sheet (Popover presentation="auto").
      await page.getByTestId('post-card').first().click();
      await page.getByTestId('bulk-actions').click();
      await expect(page.getByTestId('bulk-actions-menu')).toBeVisible();
      await expect(page.getByTestId('popover-sheet-handle')).toBeVisible();
    });

    test('per-situation empty states (GAL-2)', async ({ page, api }) => {
      // Empty folder: the fixture seeds pin_3 into folder 2, so clear it first.
      const pin = api.posts.find((p) => p.key === 'pin_3');
      if (pin) pin.collectionIds = [];
      await page.goto('/c/2');
      await expect(page.getByTestId('empty-state')).toContainText('This folder is empty');

      // No filter results: filter to a media type nothing has.
      await page.goto('/');
      await expect(page.getByTestId('post-card').first()).toBeVisible();
      await page.getByTestId('filters-toggle').click();
      await page.getByTestId('drawer-mediatype').getByText('Video', { exact: true }).click();
      await page.getByTestId('drawer-apply').click();
      await expect(page.getByTestId('empty-state')).toContainText('No posts match these filters');
      await expect(page.getByTestId('empty-reset-filters')).toBeVisible();

      // Empty library (web): points at the extension, not a desktop-only import.
      api.posts.length = 0;
      await page.goto('/');
      await expect(page.getByTestId('empty-state')).toContainText('Your library is empty');
      await expect(page.getByTestId('empty-setup')).toBeVisible();
      await expect(page.getByTestId('empty-state')).not.toContainText('JSON');
    });
  });
}

test.describe('filter overlay at 1024×768 (GAL-5)', () => {
  test.use({ viewport: { width: 1024, height: 768 } });

  test('the panel overlays the grid with a backdrop, not a push column', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('post-card').first()).toBeVisible();
    const gridWidthBefore = (await page.getByTestId('post-grid').boundingBox())!.width;

    await page.getByTestId('filters-toggle').click();
    await expect(page.getByTestId('filter-drawer')).toBeVisible();
    await expect(page.getByTestId('filter-drawer-backdrop')).toBeVisible();
    // Overlay, not push: the grid keeps its width (the audit's panel shrank the
    // cards to ~95px by pushing the column).
    const gridWidthAfter = (await page.getByTestId('post-grid').boundingBox())!.width;
    expect(Math.round(gridWidthAfter)).toBe(Math.round(gridWidthBefore));
  });
});

// ── Screenshots for the lane report (UX3_SHOT_DIR) ───────────────────────────
test.describe('screenshots', () => {
  test.skip(!SHOT_DIR, 'set UX3_SHOT_DIR to save them');

  for (const { name, viewport } of NARROW) {
    test(`narrow ${name}`, async ({ page, api }) => {
      await page.setViewportSize(viewport);
      const shot = async (n: string): Promise<void> => {
        await page.waitForTimeout(500);
        await page.screenshot({ path: path.join(SHOT_DIR!, `${name}-${n}.png`) });
      };
      await page.goto('/');
      await expect(page.getByTestId('post-card').first()).toBeVisible();
      await shot('02-gallery-toolbar');
      await page.getByTestId('filters-toggle').click();
      await expect(page.getByTestId('filter-drawer')).toBeVisible();
      await shot('04-filter-sheet');
      await page.getByTestId('drawer-apply').click();
      await page.getByTestId('select-toggle').click();
      await page.getByTestId('post-card').first().click();
      await shot('06-selection-bar');
      await page.getByTestId('bulk-actions').click();
      await expect(page.getByTestId('bulk-actions-menu')).toBeVisible();
      await shot('06b-actions-sheet');
      await page.keyboard.press('Escape');
      await page.getByTestId('select-cancel').click();
      const pin = api.posts.find((p) => p.key === 'pin_3');
      if (pin) pin.collectionIds = [];
      await page.goto('/c/2');
      await expect(page.getByTestId('empty-state')).toBeVisible();
      await shot('05-empty-folder');
      api.posts.length = 0;
      await page.goto('/');
      await expect(page.getByTestId('empty-state')).toBeVisible();
      await shot('05b-empty-library');
    });
  }

  test('tablet 1024 filter overlay', async ({ page }) => {
    await page.setViewportSize({ width: 1024, height: 768 });
    await page.goto('/');
    await expect(page.getByTestId('post-card').first()).toBeVisible();
    await page.getByTestId('filters-toggle').click();
    await expect(page.getByTestId('filter-drawer')).toBeVisible();
    await page.waitForTimeout(500);
    await page.screenshot({ path: path.join(SHOT_DIR!, 'tab-04-filter-overlay.png') });
  });
});
