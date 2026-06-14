import type { Page } from '@playwright/test';
import { test, expect, overrideHandler } from './electron-fixture';

test.describe('Downloads – controls, checkboxes, and job list', () => {
  test.beforeEach(async ({ page }) => {
    // Asset-type prefs live in localStorage and persist across the shared
    // userData dir — reset so each test starts from the all-enabled default.
    await page.evaluate(() => localStorage.removeItem('download:assetTypes'));
    await page.click('[data-testid="nav-downloads"]');
    await expect(page.locator('[data-testid="downloads-view"]')).toBeVisible();
  });

  // ── Header ───────────────────────────────────────────────────────────────

  test('renders the Downloads header', async ({ page }) => {
    await expect(page.getByText('Downloads').first()).toBeVisible();
  });

  // ── Stats bar ────────────────────────────────────────────────────────────

  test('stats bar is visible with correct total', async ({ page }) => {
    await expect(page.locator('[data-testid="downloads-stats"]')).toBeVisible();
    await expect(page.locator('[data-testid="downloads-stats"]')).toContainText('Total');
    await expect(page.locator('[data-testid="downloads-stats"]')).toContainText('15');
  });

  test('stats bar shows thumbnail, image, and video counts', async ({ page }) => {
    const statsBar = page.locator('[data-testid="downloads-stats"]');
    await expect(statsBar).toContainText('With thumbnails');
    await expect(statsBar).toContainText('With images');
    await expect(statsBar).toContainText('With videos');
  });

  // ── Asset-type selection (moved to Settings) ──────────────────────────────

  async function setAssetType(page: Page, label: string, checked: boolean) {
    await page.click('[data-testid="nav-settings"]');
    const box = page.getByLabel(label, { exact: true });
    if (checked) await box.check();
    else await box.uncheck();
    await page.click('[data-testid="nav-downloads"]');
  }

  test('Download view no longer shows asset-type checkboxes', async ({ page }) => {
    await expect(page.getByLabel('Thumbnails', { exact: true })).toHaveCount(0);
  });

  test('Settings exposes the three asset-type toggles, checked by default', async ({ page }) => {
    await page.click('[data-testid="nav-settings"]');
    await expect(page.getByLabel('Thumbnails', { exact: true })).toBeChecked();
    await expect(page.getByLabel('Images', { exact: true })).toBeChecked();
    await expect(page.getByLabel('Videos', { exact: true })).toBeChecked();
  });

  // ── Download All button ──────────────────────────────────────────────────

  test('Download All button is enabled when at least one asset type is selected', async ({ page }) => {
    await expect(page.locator('[data-testid="download-all"]')).toBeEnabled();
  });

  test('Download All button is disabled when all asset types are unchecked in Settings', async ({ page }) => {
    await setAssetType(page, 'Thumbnails', false);
    await setAssetType(page, 'Images', false);
    await setAssetType(page, 'Videos', false);
    await expect(page.locator('[data-testid="download-all"]')).toBeDisabled();
  });

  test('Download All button re-enables after re-checking an asset type in Settings', async ({ page }) => {
    await setAssetType(page, 'Thumbnails', false);
    await setAssetType(page, 'Images', false);
    await setAssetType(page, 'Videos', false);
    await expect(page.locator('[data-testid="download-all"]')).toBeDisabled();

    await setAssetType(page, 'Thumbnails', true);
    await expect(page.locator('[data-testid="download-all"]')).toBeEnabled();
  });

  test('clicking Download All does not crash the app', async ({ page }) => {
    await page.locator('[data-testid="download-all"]').click();
    await expect(page.locator('[data-testid="downloads-view"]')).toBeVisible();
  });

  // ── Download Missing button ──────────────────────────────────────────────

  test('Download Missing button is enabled by default', async ({ page }) => {
    await expect(page.locator('[data-testid="download-missing"]')).toBeEnabled();
  });

  test('Download Missing button is disabled when all asset types unchecked in Settings', async ({ page }) => {
    await setAssetType(page, 'Thumbnails', false);
    await setAssetType(page, 'Images', false);
    await setAssetType(page, 'Videos', false);
    await expect(page.locator('[data-testid="download-missing"]')).toBeDisabled();
  });

  test('clicking Download Missing does not crash the app', async ({ page }) => {
    await page.locator('[data-testid="download-missing"]').click();
    await expect(page.locator('[data-testid="downloads-view"]')).toBeVisible();
  });

  // ── Job list ─────────────────────────────────────────────────────────────

  test('job list renders the 4 mock download jobs', async ({ page }) => {
    await expect(page.locator('[data-testid="download-job"]')).toHaveCount(4);
  });

  test('done job shows checkmark icon', async ({ page }) => {
    // ig_001 is 'done' — its row should contain "done" text
    const doneRow = page.locator('[data-testid="download-job"]').filter({ hasText: 'done' }).first();
    await expect(doneRow).toBeVisible();
  });

  test('downloading job shows progress percentage', async ({ page }) => {
    // ig_002 is 'downloading' at 0.65 → should show "65%"
    const downloadingRow = page.locator('[data-testid="download-job"]').filter({ hasText: '65%' });
    await expect(downloadingRow).toBeVisible();
  });

  test('error job shows error text', async ({ page }) => {
    // tw_001 has error 'Network timeout'
    const errorRow = page.locator('[data-testid="download-job"]').filter({ hasText: 'Network timeout' });
    await expect(errorRow).toBeVisible();
  });

  test('pending job shows "pending" status', async ({ page }) => {
    const pendingRow = page.locator('[data-testid="download-job"]').filter({ hasText: 'pending' });
    await expect(pendingRow).toBeVisible();
  });

  test('progress summary shows done/total counts', async ({ page }) => {
    // 1 done out of 4 total
    await expect(page.getByText('1 / 4 posts downloaded')).toBeVisible();
  });

  // ── Empty state ──────────────────────────────────────────────────────────

  test('empty state shows when no jobs', async ({ page, electronApp }) => {
    await overrideHandler(electronApp, 'download:status', []);

    // Navigate away and back to trigger a re-mount with the new mock
    await page.click('[data-testid="nav-gallery"]');
    await page.click('[data-testid="nav-downloads"]');

    await expect(page.locator('[data-testid="downloads-empty"]')).toBeVisible();
    await expect(page.getByText('No active downloads', { exact: false })).toBeVisible();
  });
});
