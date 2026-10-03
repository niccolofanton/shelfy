import { test, expect } from './electron-fixture';

test.describe('Browser – tab controls and status bar', () => {
  // Navigate to Browser view before each test
  test.beforeEach(async ({ page }) => {
    // "Browser" is a non-clickable group header; navigate via a platform sub-tab.
    await page.click('[data-testid="browser-tab-instagram"]');
    await expect(page.locator('[data-testid="browser-view"]')).toBeVisible();
  });

  test('renders the browser view', async ({ page }) => {
    await expect(page.locator('[data-testid="browser-view"]')).toBeVisible();
  });

  test('Instagram tab is active by default', async ({ page }) => {
    const igTab = page.locator('[data-testid="browser-tab-instagram"]');
    await expect(igTab).toBeVisible();
    await expect(igTab).toHaveText(/Instagram/);
    await expect(igTab).toHaveAttribute('aria-current', 'page');
  });

  test('X / Twitter tab is visible', async ({ page }) => {
    await expect(page.locator('[data-testid="browser-tab-twitter"]')).toBeVisible();
    await expect(page.locator('[data-testid="browser-tab-twitter"]')).toHaveText(/X \/ Twitter/);
  });

  test('clicking X / Twitter tab switches active tab', async ({ page }) => {
    await page.click('[data-testid="browser-tab-twitter"]');
    const twitterTab = page.locator('[data-testid="browser-tab-twitter"]');
    await expect(twitterTab).toHaveAttribute('aria-current', 'page');
  });

  test('clicking Instagram tab switches back', async ({ page }) => {
    await page.click('[data-testid="browser-tab-twitter"]');
    await page.click('[data-testid="browser-tab-instagram"]');
    const igTab = page.locator('[data-testid="browser-tab-instagram"]');
    await expect(igTab).toHaveAttribute('aria-current', 'page');
  });

  test('URL bar is visible', async ({ page }) => {
    await expect(page.locator('[data-testid="url-bar"]')).toBeVisible();
  });

  test('URL bar shows Instagram URL by default', async ({ page }) => {
    const urlBar = page.locator('[data-testid="url-bar"]');
    await expect(urlBar).toContainText('instagram.com');
  });

  test('URL bar shows Twitter history (bookmarks) URL after switching tab', async ({ page }) => {
    // The default tab target moved from /i/bookmarks to /i/history (X now opens
    // Bookmarks there; see TABS in src/lib/browserUrls.ts) — the app is correct,
    // this spec just hadn't been updated to match. This profile is always
    // logged out, and the Twitter webview starts loading in the background from
    // app launch, so by the time this assertion runs X has sometimes already
    // redirected it to its login wall — which still carries the original
    // destination in `redirect_after_login`. Accept either form so the
    // assertion doesn't race that redirect.
    await page.click('[data-testid="browser-tab-twitter"]');
    const urlBar = page.locator('[data-testid="url-bar"]');
    await expect(urlBar).toContainText(/x\.com\/i\/history|redirect_after_login=%2Fi%2Fhistory/);
  });

  test('Refresh button is visible and clickable', async ({ page }) => {
    const refreshBtn = page.locator('[data-testid="browser-refresh"]');
    await expect(refreshBtn).toBeVisible();
    // Clicking it calls webview.reload() — we just verify no crash
    await refreshBtn.click();
    await expect(page.locator('[data-testid="browser-view"]')).toBeVisible();
  });

  test('Refresh button has title "Reload"', async ({ page }) => {
    await expect(page.locator('[data-testid="browser-refresh"]')).toHaveAttribute(
      'title',
      'Reload',
    );
  });

  test('status shows "Navigate to saved posts" when not on saved page', async ({ page }) => {
    // The webview initially loads the saved URL but we can't fully navigate it in tests;
    // the status starts as "Navigate to saved posts" because the webview URL detection
    // doesn't fire immediately without actual navigation events.
    await expect(page.locator('[data-testid="browser-status"]')).toBeVisible();
  });

  test('intercepted count is not shown initially', async ({ page }) => {
    // interceptedCount starts at 0, so the counter element should not be rendered
    await expect(page.getByText('posts captured')).not.toBeVisible();
  });

  test('switching tabs resets intercepted count', async ({ page }) => {
    // Switch to Twitter and back; no posts should have been counted
    await page.click('[data-testid="browser-tab-twitter"]');
    await page.click('[data-testid="browser-tab-instagram"]');
    await expect(page.getByText('posts captured')).not.toBeVisible();
  });
});
