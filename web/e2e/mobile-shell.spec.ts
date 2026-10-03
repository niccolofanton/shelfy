// UX-1, the mobile shell (docs/web-port/reviews/ux-audit.md §2.1, §4): the
// menu button in the layout flow instead of floating over every screen (SH-1),
// the open post modal above every shell element, the shell's height and safe
// areas (SH-2, SH-3), the drawer as a modal dialog (SH-4), keyboard-reachable
// folders (SH-5, SH-6), the Search tab (SH-11), the per-view title (SH-13), the
// 404 (SH-14) and the offline pill (ST-3). Narrow at the audit's 390×844 and
// 412×915; the desktop checks at 1024×768 and 1440×900.
//
// Safe areas: Chromium has no notch, so `env(safe-area-inset-*)` is 0 unless
// the CDP override below sets it, as an installed iOS app would see it.
//
// Screenshots for the lane report: set UX1_SHOT_DIR to an absolute path
// outside the repo; without it the screenshot test is skipped.
import path from 'node:path';
import type { Page } from '@playwright/test';
import { test, expect } from './api';

const NARROW = [
  { name: 'iphone', viewport: { width: 390, height: 844 } },
  { name: 'android', viewport: { width: 412, height: 915 } },
];
const WIDE = [
  { name: 'tablet', viewport: { width: 1024, height: 768 } },
  { name: 'desktop', viewport: { width: 1440, height: 900 } },
];
const SHOT_DIR = process.env.UX1_SHOT_DIR;

// Status bar and home indicator of a notched iPhone in portrait.
const PORTRAIT_INSETS = { top: 47, bottom: 34, left: 0, right: 0 };

async function setSafeAreaInsets(page: Page, insets: typeof PORTRAIT_INSETS): Promise<void> {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send('Emulation.setSafeAreaInsetsOverride', { insets });
}

// Probes a grid of points over the top `band` px of `selector`'s box and
// returns the ones whose topmost element lies outside it: whatever paints
// there (a floating shell control) covers the view's own top row.
async function coveredPoints(page: Page, selector: string, band = 160): Promise<string[]> {
  return page.evaluate(
    ({ selector, band }) => {
      const root = document.querySelector(selector);
      if (!root) return [`missing ${selector}`];
      const r = root.getBoundingClientRect();
      const covered: string[] = [];
      for (let y = r.top + 2; y < Math.min(r.bottom, r.top + band); y += 10) {
        for (let x = r.left + 2; x < r.right - 2; x += 10) {
          const el = document.elementFromPoint(x, y);
          if (el && !root.contains(el)) {
            const id = el.closest('[data-testid]')?.getAttribute('data-testid') ?? el.tagName;
            covered.push(`${Math.round(x)},${Math.round(y)} ${id}`);
          }
        }
      }
      return covered;
    },
    { selector, band },
  );
}

// The points of the whole viewport whose topmost element is outside `selector`.
async function pointsOutside(page: Page, selector: string): Promise<string[]> {
  return page.evaluate((selector) => {
    const root = document.querySelector(selector);
    if (!root) return [`missing ${selector}`];
    const out: string[] = [];
    for (let y = 2; y < window.innerHeight; y += 20) {
      for (let x = 2; x < window.innerWidth; x += 20) {
        const el = document.elementFromPoint(x, y);
        if (!el || !root.contains(el)) {
          const id = el?.closest('[data-testid]')?.getAttribute('data-testid') ?? el?.tagName;
          out.push(`${x},${y} ${id}`);
        }
      }
    }
    return out;
  }, selector);
}

async function activeTestId(page: Page): Promise<string | null> {
  return page.evaluate(() => document.activeElement?.getAttribute('data-testid') ?? null);
}

async function focusInside(page: Page, testId: string): Promise<boolean> {
  return page.evaluate(
    (id) => !!document.querySelector(`[data-testid="${id}"]`)?.contains(document.activeElement),
    testId,
  );
}

async function isInert(page: Page, selector: string): Promise<boolean> {
  return page.evaluate((s) => !!document.querySelector<HTMLElement>(s)?.inert, selector);
}

test.afterEach(({ api }) => {
  expect(api.thirdParty, 'requests outside the app').toEqual([]);
});

for (const { name, viewport } of NARROW) {
  test.describe(`narrow: ${name} (${viewport.width}x${viewport.height})`, () => {
    test.use({ viewport, hasTouch: true });

    test('the menu button sits in the layout: no shell control covers a view (SH-1)', async ({
      page,
    }) => {
      const screens = [
        { url: '/', view: 'gallery', ready: page.getByTestId('post-card').first() },
        { url: '/trash', view: 'trash', ready: page.getByTestId('trash-view') },
        { url: '/jobs', view: 'jobs', ready: page.getByTestId('jobs-view') },
        { url: '/settings', view: 'settings', ready: page.getByTestId('settings-tabs') },
      ];
      for (const { url, view, ready } of screens) {
        await page.goto(url);
        await expect(ready).toBeVisible();
        const button = page.getByTestId('sidebar-open');
        await expect(button).toBeVisible();
        // 44×44, and not floating: nothing between it and the page is `fixed`.
        const box = (await button.boundingBox())!;
        expect(Math.round(box.width)).toBe(44);
        expect(Math.round(box.height)).toBe(44);
        const fixed = await button.evaluate((el) => {
          for (let n: Element | null = el; n; n = n.parentElement) {
            if (getComputedStyle(n).position === 'fixed') return true;
          }
          return false;
        });
        expect(fixed, `${url}: the menu button floats`).toBe(false);
        expect(await coveredPoints(page, `[data-shell-view="${view}"]`), url).toEqual([]);
      }
    });

    test('the open post modal is on top of every shell element', async ({ page }) => {
      await page.goto('/p/ig_1');
      await expect(page.getByTestId('post-modal')).toBeVisible();
      expect(await pointsOutside(page, '[data-testid="post-modal"]')).toEqual([]);
    });

    test('the shell clears the status bar and the home indicator (SH-2, SH-3)', async ({
      page,
    }) => {
      await setSafeAreaInsets(page, PORTRAIT_INSETS);
      await page.goto('/');
      await expect(page.getByTestId('post-card').first()).toBeVisible();
      const height = viewport.height;
      // The top row starts below the status bar.
      const top = (await page.getByTestId('shell-topbar').boundingBox())!;
      expect(top.y).toBeGreaterThanOrEqual(PORTRAIT_INSETS.top);
      // The BottomNav ends at the bottom edge, its tabs above the indicator.
      const nav = page.getByTestId('bottom-nav');
      const navBox = (await nav.boundingBox())!;
      expect(Math.round(navBox.y + navBox.height)).toBe(height);
      await expect(nav).toHaveCSS('padding-bottom', `${PORTRAIT_INSETS.bottom}px`);
      const tab = (await page.getByTestId('bottom-nav-settings').boundingBox())!;
      expect(tab.y + tab.height).toBeLessThanOrEqual(height - PORTRAIT_INSETS.bottom + 0.5);
      // The shell is exactly the viewport tall: 100dvh, with 100vh as the fallback.
      const shell = await page.evaluate(() => {
        const root = document.querySelector('[data-shell-view]')?.closest('main')?.parentElement;
        return root ? root.getBoundingClientRect().height : 0;
      });
      expect(Math.round(shell)).toBe(height);
      // The drawer pads itself: its header and close button sit below the bar.
      await page.getByTestId('sidebar-open').click();
      const close = (await page.getByTestId('sidebar-close').boundingBox())!;
      expect(close.y).toBeGreaterThanOrEqual(PORTRAIT_INSETS.top);
    });

    test('the drawer is a modal dialog: focus in, Tab trapped, Escape returns focus (SH-4)', async ({
      page,
    }) => {
      await page.goto('/');
      await expect(page.getByTestId('post-card').first()).toBeVisible();
      const sidebar = page.getByTestId('sidebar');
      // Closed: off-screen and out of the tab order.
      await expect(sidebar).not.toBeInViewport();
      await expect(sidebar).toHaveCSS('visibility', 'hidden');

      await page.getByTestId('sidebar-open').click();
      await expect(sidebar).toBeInViewport();
      await expect(sidebar).toHaveAttribute('role', 'dialog');
      await expect(sidebar).toHaveAttribute('aria-modal', 'true');
      await expect(sidebar).toHaveAttribute('aria-label', 'Menu');
      await expect(page.getByTestId('sidebar-open')).toHaveAttribute('aria-expanded', 'true');
      await expect.poll(() => activeTestId(page)).toBe('sidebar-close');
      const close = (await page.getByTestId('sidebar-close').boundingBox())!;
      expect(Math.round(close.width)).toBe(44);
      expect(Math.round(close.height)).toBe(44);
      // The rest of the shell is out of reach.
      expect(await isInert(page, 'main')).toBe(true);
      expect(await isInert(page, '[data-testid="bottom-nav"]')).toBe(true);

      // Tab and Shift+Tab never leave the drawer, whichever way they wrap.
      for (let i = 0; i < 24; i++) {
        await page.keyboard.press('Tab');
        expect(await focusInside(page, 'sidebar'), `Tab #${i + 1}`).toBe(true);
      }
      for (let i = 0; i < 24; i++) {
        await page.keyboard.press('Shift+Tab');
        expect(await focusInside(page, 'sidebar'), `Shift+Tab #${i + 1}`).toBe(true);
      }

      await page.keyboard.press('Escape');
      await expect(sidebar).not.toBeInViewport();
      await expect.poll(() => activeTestId(page)).toBe('sidebar-open');
      await expect(page.getByTestId('sidebar-open')).toHaveAttribute('aria-expanded', 'false');
      expect(await isInert(page, 'main')).toBe(false);
      expect(await isInert(page, '[data-testid="bottom-nav"]')).toBe(false);
    });

    test('the scrim closes the drawer and focus returns to the menu button', async ({ page }) => {
      await page.goto('/');
      await page.getByTestId('sidebar-open').click();
      await expect(page.getByTestId('sidebar')).toBeInViewport();
      // Folder edit is reachable without hover (SH-6).
      await expect(page.getByTestId('edit-collection-1')).toBeVisible();
      await page
        .getByTestId('sidebar-backdrop')
        .click({ position: { x: viewport.width - 20, y: viewport.height / 2 } });
      await expect(page.getByTestId('sidebar')).not.toBeInViewport();
      await expect.poll(() => activeTestId(page)).toBe('sidebar-open');
    });

    test('a pick in the drawer closes it, even when it lands where the app is', async ({
      page,
    }) => {
      await page.goto('/');
      await page.getByTestId('sidebar-open').click();
      // "All posts" is the library already open: no navigation, still closes.
      await page.getByTestId('source-all').click();
      await expect(page.getByTestId('sidebar')).not.toBeInViewport();
    });

    test('the Search tab opens the library and focuses its search field (SH-11)', async ({
      page,
    }) => {
      await page.goto('/settings');
      await expect(page.getByTestId('settings-tabs')).toBeVisible();
      await page.getByTestId('bottom-nav-search').click();
      await expect(page).toHaveURL(/\/$/);
      await expect(page.getByRole('textbox', { name: 'Search posts' })).toBeFocused();
    });

    test('an offline pill shows while the browser is offline (ST-3)', async ({ page }) => {
      await page.goto('/');
      await expect(page.getByTestId('post-card').first()).toBeVisible();
      await expect(page.getByTestId('offline-pill')).toHaveCount(0);
      await page.context().setOffline(true);
      const pill = page.getByTestId('offline-pill');
      await expect(pill).toBeVisible();
      await expect(pill).toHaveText(/offline/i);
      // Above the BottomNav, not on it.
      const pillBox = (await pill.boundingBox())!;
      const navBox = (await page.getByTestId('bottom-nav').boundingBox())!;
      expect(pillBox.y + pillBox.height).toBeLessThanOrEqual(navBox.y);
      await page.context().setOffline(false);
      await expect(pill).toHaveCount(0);
    });
  });
}

test.describe('narrow landscape (844x390)', () => {
  test.use({ viewport: { width: 844, height: 390 }, hasTouch: true });

  test('the shell clears the side insets (SH-3)', async ({ page }) => {
    await setSafeAreaInsets(page, { top: 0, bottom: 21, left: 47, right: 47 });
    await page.goto('/');
    await expect(page.getByTestId('post-card').first()).toBeVisible();
    const main = (await page.locator('main').boundingBox())!;
    expect(main.x).toBeGreaterThanOrEqual(47);
    expect(main.x + main.width).toBeLessThanOrEqual(844 - 47);
  });
});

for (const { name, viewport } of WIDE) {
  test.describe(`wide: ${name} (${viewport.width}x${viewport.height})`, () => {
    test.use({ viewport });

    test('no menu button and no top bar: the sidebar column is unchanged', async ({ page }) => {
      await page.goto('/');
      await expect(page.getByTestId('post-card').first()).toBeVisible();
      await expect(page.getByTestId('sidebar-open')).toBeHidden();
      await expect(page.getByTestId('shell-topbar')).toBeHidden();
      await expect(page.getByTestId('bottom-nav')).toBeHidden();
      const sidebar = (await page.getByTestId('sidebar').boundingBox())!;
      expect(sidebar.x).toBe(0);
      expect(sidebar.width).toBe(240);
      const main = (await page.locator('main').boundingBox())!;
      expect(main.x).toBe(240);
      expect(main.y).toBe(0);
      expect(main.height).toBe(viewport.height);
      await expect(page.getByTestId('sidebar')).not.toHaveAttribute('role', 'dialog');
    });

    test('folders and tree toggles are named buttons, reachable by Tab (SH-5)', async ({
      page,
    }) => {
      await page.goto('/');
      await expect(page.getByTestId('post-card').first()).toBeVisible();
      await expect(page.getByRole('button', { name: 'Collapse All posts' })).toBeVisible();
      await expect(page.getByRole('button', { name: 'Collapse Instagram' })).toBeVisible();
      // From "All posts", Tab reaches the custom folder…
      await page.getByTestId('source-all').focus();
      let reached = false;
      for (let i = 0; i < 20 && !reached; i++) {
        await page.keyboard.press('Tab');
        reached = await page.evaluate(
          () => document.activeElement?.textContent?.trim() === 'Inspiration',
        );
      }
      expect(reached, 'Tab reaches the "Inspiration" folder').toBe(true);
      // …and its edit button, which keyboard focus reveals like hover does.
      await page.keyboard.press('Tab');
      await expect(page.getByRole('button', { name: 'Edit folder Inspiration' })).toBeFocused();
      await page.keyboard.press('Shift+Tab');
      await page.keyboard.press('Enter');
      await expect(page).toHaveURL(/\/c\/2$/);
      await expect(page.getByRole('button', { name: 'Inspiration', exact: true })).toHaveAttribute(
        'aria-current',
        'page',
      );
    });

    test('the title names the page (SH-13)', async ({ page }) => {
      await page.goto('/');
      await expect(page).toHaveTitle('All posts · Shelfy');
      await page.goto('/c/2');
      await expect(page).toHaveTitle('Inspiration · Shelfy');
      await page.goto('/trash');
      await expect(page).toHaveTitle('Trash · Shelfy');
    });

    test('a 404 highlights no sidebar row and shows no gallery toolbar (SH-14)', async ({
      page,
    }) => {
      await page.goto('/no/such/page');
      await expect(page.getByTestId('route-not-found')).toBeVisible();
      await expect(page.getByTestId('sidebar').locator('[aria-current="page"]')).toHaveCount(0);
      await expect(page.getByTestId('filters-toggle')).toBeHidden();
    });
  });
}

// ── Screenshots for the lane report (UX1_SHOT_DIR) ───────────────────────────
test.describe('screenshots', () => {
  test.skip(!SHOT_DIR, 'set UX1_SHOT_DIR to save them');

  // Each shot waits out the entrance animations (rows, cards, the modal).
  function shooter(page: Page, name: string) {
    return async (shotName: string): Promise<void> => {
      await page.waitForTimeout(700);
      await page.screenshot({ path: path.join(SHOT_DIR!, `${name}-${shotName}.png`) });
    };
  }

  for (const { name, viewport } of NARROW) {
    test(`narrow ${name}`, async ({ page }) => {
      await page.setViewportSize(viewport);
      const shot = shooter(page, name);
      await page.goto('/');
      await expect(page.getByTestId('post-card').first()).toBeVisible();
      await shot('02-gallery');
      await page.getByTestId('sidebar-open').click();
      await expect(page.getByTestId('sidebar')).toBeInViewport();
      await shot('08-drawer');
      await page.keyboard.press('Escape');
      await page.goto('/p/ig_1');
      await expect(page.getByTestId('post-modal')).toBeVisible();
      await shot('07-modal');
      await page.goto('/trash');
      await expect(page.getByTestId('trash-view')).toBeVisible();
      await shot('10-trash');
      await page.goto('/jobs');
      await expect(page.getByTestId('jobs-view')).toBeVisible();
      await shot('11-jobs');
      await page.goto('/settings');
      await expect(page.getByTestId('settings-tabs')).toBeVisible();
      await shot('12-settings');
      // As the installed iOS app sees it: a status bar and a home indicator.
      await setSafeAreaInsets(page, PORTRAIT_INSETS);
      await page.goto('/');
      await expect(page.getByTestId('post-card').first()).toBeVisible();
      await shot('15-pwa-insets');
      await page.getByTestId('sidebar-open').click();
      await shot('16-pwa-insets-drawer');
      await page.keyboard.press('Escape');
      await expect(page.getByTestId('sidebar')).not.toBeInViewport();
      await page.context().setOffline(true);
      await expect(page.getByTestId('offline-pill')).toBeVisible();
      await shot('17-offline');
      await page.context().setOffline(false);
    });
  }

  for (const { name, viewport } of WIDE) {
    test(`wide ${name}`, async ({ page }) => {
      await page.setViewportSize(viewport);
      const shot = shooter(page, name);
      await page.goto('/');
      await expect(page.getByTestId('post-card').first()).toBeVisible();
      await shot('02-gallery');
      await page.goto('/settings');
      await expect(page.getByTestId('settings-tabs')).toBeVisible();
      await shot('12-settings');
      await page.goto('/no/such/page');
      await expect(page.getByTestId('route-not-found')).toBeVisible();
      await shot('14-not-found');
    });
  }
});
