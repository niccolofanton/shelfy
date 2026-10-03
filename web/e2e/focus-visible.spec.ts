// UX-2 (UX audit §3.3, owner decision O1): keyboard focus is visible again,
// and only for the keyboard. Tab shows the --focus-ring outline on sidebar
// rows, toolbar controls, the search field's pill, menu items and post cards
// (inset, under .u-clip-aa); a mouse click shows none.
import path from 'node:path';
import type { Locator, Page } from '@playwright/test';
import { test, expect } from './api';
import { MOBILE_VIEWPORT } from '../playwright.config';

// --focus-ring, #a48fff.
const RING = 'rgb(164, 143, 255)';

// Opt-in screenshots for the lane report: set UX2_SHOT_DIR to a directory
// outside the repo.
const SHOT_DIR = process.env.UX2_SHOT_DIR;
async function shot(page: Page, name: string): Promise<void> {
  if (SHOT_DIR) await page.screenshot({ path: path.join(SHOT_DIR, `${name}.png`) });
}

test.afterEach(({ api }) => {
  expect(api.thirdParty, 'requests outside the app').toEqual([]);
});

async function outline(locator: Locator): Promise<{ style: string; width: string; color: string }> {
  return locator.evaluate((el) => {
    const s = getComputedStyle(el);
    return { style: s.outlineStyle, width: s.outlineWidth, color: s.outlineColor };
  });
}

// Presses Tab until `target` has focus (at most `max` times).
async function tabTo(page: Page, target: Locator, max = 40): Promise<void> {
  for (let i = 0; i < max; i++) {
    await page.keyboard.press('Tab');
    if (await target.evaluate((el) => el === document.activeElement)) return;
  }
  throw new Error('Tab never reached the target');
}

test('Tab rings sidebar rows and toolbar controls; a click does not', async ({ page }) => {
  await page.goto('/');
  await expect(page.getByTestId('post-card')).toHaveCount(3);

  const row = page.getByTestId('source-all');
  await tabTo(page, row);
  expect(await outline(row)).toEqual({ style: 'solid', width: '2px', color: RING });
  await shot(page, 'desk-sidebar-row-focus');

  const filters = page.getByTestId('filters-toggle');
  await tabTo(page, filters);
  expect(await outline(filters)).toEqual({ style: 'solid', width: '2px', color: RING });
  await shot(page, 'desk-toolbar-focus');

  // A pointer click focuses the button but is not focus-visible.
  await page.mouse.click(5, 5);
  await row.click();
  await expect(row).toBeFocused();
  expect((await outline(row)).style).toBe('none');
});

test('the search field rings its pill, not the bare input', async ({ page }) => {
  await page.goto('/');
  const input = page.getByRole('textbox', { name: /search/i });
  await tabTo(page, input);
  expect((await outline(input)).style).toBe('none');
  const pill = input.locator('xpath=..');
  expect(await outline(pill)).toEqual({ style: 'solid', width: '2px', color: RING });
  await shot(page, 'desk-search-focus');
});

test('menu items and post cards show the ring', async ({ page }) => {
  await page.goto('/');
  const card = page.getByTestId('post-card').first();
  await tabTo(page, card, 60);
  // .u-clip-aa masks outlines: the ring is an inset box-shadow on ::after.
  const after = await card.evaluate((el) => getComputedStyle(el, '::after').boxShadow);
  expect(after).toContain(RING);
  expect(after).toContain('inset');
  await shot(page, 'desk-card-focus');

  await page.goto('/p/x_2');
  await expect(page.getByTestId('post-modal')).toBeVisible();
  await page.getByTestId('post-modal-more').click();
  const menu = page.getByTestId('post-modal-menu');
  await expect(menu).toBeVisible();
  // The more menu is now an APG menu (role="menu"): it focuses its first item on
  // open and the arrow keys move between items (Tab closes it). ArrowDown moves
  // focus by keyboard, so the focused item shows the ring.
  await page.keyboard.press('ArrowDown');
  const item = menu.locator('[role="menuitem"]:focus');
  await expect(item).toHaveCount(1);
  expect(await outline(item)).toEqual({ style: 'solid', width: '2px', color: RING });
  await shot(page, 'desk-menu-item-focus');
});

test.describe('narrow', () => {
  test.use({ viewport: MOBILE_VIEWPORT, hasTouch: true });

  test('the BottomNav tabs ring on Tab', async ({ page }) => {
    await page.goto('/');
    const tab = page.getByTestId('bottom-nav-library');
    await tabTo(page, tab, 60);
    expect(await outline(tab)).toEqual({ style: 'solid', width: '2px', color: RING });
    await shot(page, 'ios-bottomnav-focus');
  });
});
