import { test, expect, overrideHandler } from './electron-fixture';
import type { Page } from '@playwright/test';

test.describe('Import Modal – all states and transitions', () => {
  async function openModal(page: Page) {
    await page.click('[data-testid="nav-settings"]');
    await page.click('[data-testid="open-import-btn"]');
    await expect(page.locator('[data-testid="import-modal"]')).toBeVisible();
  }

  // Convenience scoped locators
  const modal = (page: Page) => page.locator('[data-testid="import-modal"]');
  const btn = (page: Page, name: string) => modal(page).getByRole('button', { name });

  test('modal opens in idle state', async ({ page }) => {
    await openModal(page);
    await expect(btn(page, 'Choose File')).toBeVisible();
    await expect(btn(page, 'Import')).toBeVisible();
    await expect(btn(page, 'Cancel')).toBeVisible();
  });

  test('Import button is disabled before a file is chosen', async ({ page, electronApp }) => {
    await overrideHandler(electronApp, 'dialog:openFile', null);
    await openModal(page);
    await expect(btn(page, 'Import')).toBeDisabled();
  });

  test('Choose File button triggers openFile IPC and shows filename', async ({ page }) => {
    // Default mock returns '/Users/test/bookmarks.json'
    await openModal(page);
    await btn(page, 'Choose File').click();
    // Modal shows just the filename (split('/').pop()) inside a <code> tag
    await expect(modal(page).locator('code')).toHaveText('bookmarks.json');
  });

  test('Import button is enabled after file is chosen', async ({ page }) => {
    await openModal(page);
    await btn(page, 'Choose File').click();
    await expect(modal(page).locator('code')).toBeVisible();
    await expect(btn(page, 'Import')).toBeEnabled();
  });

  test('clicking Choose File when dialog is cancelled does not enable Import', async ({
    page,
    electronApp,
  }) => {
    await overrideHandler(electronApp, 'dialog:openFile', null);
    await openModal(page);
    await btn(page, 'Choose File').click();
    await expect(btn(page, 'Import')).toBeDisabled();
  });

  test('successful import: idle → importing → done', async ({ page }) => {
    await openModal(page);
    await btn(page, 'Choose File').click();
    await expect(modal(page).locator('code')).toBeVisible();
    await btn(page, 'Import').click();
    await expect(page.getByText('Imported 7 new posts')).toBeVisible({ timeout: 5_000 });
  });

  test('done state shows correct imported count', async ({ page }) => {
    await openModal(page);
    await btn(page, 'Choose File').click();
    await btn(page, 'Import').click();
    await expect(page.getByText('Imported 7 new posts')).toBeVisible();
  });

  test('Close button in done state closes modal', async ({ page }) => {
    await openModal(page);
    await btn(page, 'Choose File').click();
    await btn(page, 'Import').click();
    await expect(page.getByText('Imported 7 new posts')).toBeVisible();
    await btn(page, 'Close').click();
    await expect(modal(page)).not.toBeVisible();
  });

  test('error state is shown when importJSON rejects', async ({ page, electronApp }) => {
    await electronApp.evaluate(({ ipcMain }) => {
      try {
        ipcMain.removeHandler('db:importJSON');
      } catch {
        /* ok */
      }
      ipcMain.handle('db:importJSON', () => {
        throw new Error('Invalid JSON format');
      });
    });

    await openModal(page);
    await btn(page, 'Choose File').click();
    await btn(page, 'Import').click();
    await expect(page.getByText('Invalid JSON format')).toBeVisible({ timeout: 5_000 });
  });

  test('Try Again button resets modal to idle from error state', async ({ page, electronApp }) => {
    await electronApp.evaluate(({ ipcMain }) => {
      try {
        ipcMain.removeHandler('db:importJSON');
      } catch {
        /* ok */
      }
      ipcMain.handle('db:importJSON', () => {
        throw new Error('Parse error');
      });
    });

    await openModal(page);
    await btn(page, 'Choose File').click();
    await btn(page, 'Import').click();
    await expect(page.getByText('Parse error')).toBeVisible();

    await btn(page, 'Try Again').click();

    // Back to idle
    await expect(btn(page, 'Choose File')).toBeVisible();
    await expect(btn(page, 'Cancel')).toBeVisible();
    await expect(page.getByText('Parse error')).not.toBeVisible();
  });

  test('Cancel in error state closes the modal', async ({ page, electronApp }) => {
    await electronApp.evaluate(({ ipcMain }) => {
      try {
        ipcMain.removeHandler('db:importJSON');
      } catch {
        /* ok */
      }
      ipcMain.handle('db:importJSON', () => {
        throw new Error('Fail');
      });
    });

    await openModal(page);
    await btn(page, 'Choose File').click();
    await btn(page, 'Import').click();
    await expect(page.getByText('Fail')).toBeVisible();
    await btn(page, 'Cancel').click();
    await expect(modal(page)).not.toBeVisible();
  });

  test('overlay click does NOT close modal while importing', async ({ page, electronApp }) => {
    await electronApp.evaluate(({ ipcMain }) => {
      try {
        ipcMain.removeHandler('db:importJSON');
      } catch {
        /* ok */
      }
      ipcMain.handle(
        'db:importJSON',
        () => new Promise((res) => setTimeout(() => res({ imported: 0 }), 3000)),
      );
    });

    await openModal(page);
    await btn(page, 'Choose File').click();
    await btn(page, 'Import').click();
    // The i18n string uses a typographic ellipsis (U+2026): 'Importing posts…'.
    // Match the stable prefix so the assertion doesn't hinge on the glyph.
    await expect(page.getByText('Importing posts')).toBeVisible();

    // Click the backdrop — modal must stay open during import
    await modal(page).click({ position: { x: 10, y: 10 } });
    await expect(modal(page)).toBeVisible();
  });
});
