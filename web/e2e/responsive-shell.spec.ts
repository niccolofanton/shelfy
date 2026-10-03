// P1-02's responsive shell (plan §2.17, "under 900px"): drawer sidebar,
// bottom navigation, a stacked post modal, long-press to select, tap-to-preview
// instead of hover, and pointer pinch on the canvas. Each behavior is checked
// at both required viewports; the "desktop (>=900px)" block instead guards
// that the layout the gallery shipped with stays the same above the
// breakpoint (the card's explicit requirement).
//
// Long-press enters selection through `onQuickSelect`, which Gallery wires
// once the `bulkActions` capability exists — on since P1-14. The long-press
// behavior below checks the real selection (not just that the trailing click
// is swallowed); the onQuickSelect contract itself is also covered at the
// component level in tests/components/PostCard.test.tsx.
//
// Playwright's own touch emulation (`locator.tap()`) has no notion of a long
// hold and no multi-touch pinch, so every gesture below dispatches real
// PointerEvents directly (`locator.evaluate` + `new PointerEvent(...)`)
// instead, which also sidesteps `locator.dispatchEvent`'s generic event-type
// mapping — not guaranteed to carry pointerId/pointerType/isPrimary onto the
// event the way PostCard/InfiniteCanvas read them.
import path from 'node:path';
import type { Locator } from '@playwright/test';
import { test, expect } from './api';
import { MOBILE_VIEWPORT, TABLET_VIEWPORT } from '../playwright.config';

// Opt-in human-facing screenshots: set P1_02_SHOT_DIR to an absolute path
// outside the repo to save them there (the lane report does); otherwise they
// land in the gitignored test-results/, same as any other Playwright artifact.
const SHOT_DIR = process.env.P1_02_SHOT_DIR || 'test-results/p1-02';
const SHOT_LABEL = process.env.P1_02_SHOT_LABEL || 'after';
const shotPath = (name: string): string => path.join(SHOT_DIR, `${SHOT_LABEL}-${name}.png`);

// A `data:` URI never counts as a third-party request (web/e2e/api.ts), and
// the fixture's default posts have no cover (mediaType 'text', so they render
// as the typographic TextCard with no hover overlay at all) — tap-to-preview
// needs a real image card, so these tests give one post a cover first.
const PIXEL =
  'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=';

interface PointerOpts {
  pointerId: number;
  x: number;
  y: number;
  isPrimary?: boolean;
}

async function dispatchPointer(locator: Locator, type: string, opts: PointerOpts): Promise<void> {
  await locator.evaluate(
    (el, { type, opts }) => {
      el.dispatchEvent(
        new PointerEvent(type, {
          bubbles: true,
          cancelable: true,
          composed: true,
          pointerId: opts.pointerId,
          pointerType: 'touch',
          isPrimary: opts.isPrimary ?? true,
          button: 0,
          buttons: type === 'pointerup' || type === 'pointercancel' ? 0 : 1,
          clientX: opts.x,
          clientY: opts.y,
        }),
      );
    },
    { type, opts },
  );
}

async function dispatchClick(locator: Locator): Promise<void> {
  await locator.evaluate((el) => {
    el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, composed: true }));
  });
}

// A single touch tap: pointerdown, pointerup, then the click the browser
// would dispatch on release — all at the element's center, same pointerId.
async function tap(locator: Locator, pointerId: number): Promise<void> {
  const box = (await locator.boundingBox())!;
  const x = box.x + box.width / 2;
  const y = box.y + box.height / 2;
  await dispatchPointer(locator, 'pointerdown', { pointerId, x, y });
  await dispatchPointer(locator, 'pointerup', { pointerId, x, y });
  await dispatchClick(locator);
}

// The uniform scale factor InfiniteCanvas's rAF loop writes as
// `translate3d(...) scale(s)` on its world layer — `matrix(...)` (2D) and
// `matrix3d(...)` (3D, which translate3d forces) both carry it as the first
// value.
async function canvasScale(canvas: Locator): Promise<number> {
  return canvas.evaluate((el) => {
    const world = el.firstElementChild;
    const t = world ? getComputedStyle(world).transform : 'none';
    const m = /matrix(?:3d)?\(([^)]+)\)/.exec(t);
    return m ? parseFloat(m[1].split(',')[0]) : 1;
  });
}

test.afterEach(({ api }) => {
  expect(api.thirdParty, 'requests outside the app').toEqual([]);
});

// ── Behaviors shared by both required mobile viewports ──────────────────────
function defineNarrowBehaviors(name: string, viewport: { width: number; height: number }): void {
  test.describe(`${name} (${viewport.width}x${viewport.height})`, () => {
    test.use({ viewport });

    test('bottom nav shows Library, Search and Settings; AI stays hidden', async ({ page }) => {
      await page.goto('/');
      await expect(page.getByTestId('bottom-nav')).toBeVisible();
      await expect(page.getByTestId('bottom-nav-library')).toBeVisible();
      await expect(page.getByTestId('bottom-nav-search')).toBeVisible();
      await expect(page.getByTestId('bottom-nav-settings')).toBeVisible();
      // caps.ai is false on the web until P3 (WEB_CAPABILITIES) — same gate as
      // the sidebar's own AI group.
      await expect(page.getByTestId('bottom-nav-ai')).toHaveCount(0);
      await expect(page.getByTestId('bottom-nav-library')).toHaveAttribute('aria-current', 'page');
    });

    test('bottom nav Settings navigates there, Library returns', async ({ page }) => {
      await page.goto('/');
      await page.getByTestId('bottom-nav-settings').click();
      await expect(page).toHaveURL(/\/settings/);
      await page.getByTestId('bottom-nav-library').click();
      await expect(page).toHaveURL(/\/$/);
      await expect(page.getByTestId('post-card')).toHaveCount(3);
    });

    test('sidebar is a closed-by-default drawer, opened and closed explicitly', async ({
      page,
    }) => {
      await page.goto('/');
      const sidebar = page.getByTestId('sidebar');
      // `toBeVisible()` wouldn't catch an off-canvas (translateX) panel — it
      // still has layout, just outside the viewport — so check intersection.
      await expect(sidebar).not.toBeInViewport();
      await expect(page.getByTestId('sidebar-open')).toBeVisible();

      await page.getByTestId('sidebar-open').click();
      await expect(sidebar).toBeInViewport();
      await expect(page.getByTestId('sidebar-backdrop')).toBeVisible();
      await expect(sidebar.getByText('Lighting')).toBeVisible();

      // The 240px drawer covers the left part of the (375-wide) viewport, so
      // click the backdrop in its visible remainder, not its (occluded)
      // geometric center — same as a real tap beside the open drawer.
      await page.getByTestId('sidebar-backdrop').click({ position: { x: 350, y: 400 } });
      await expect(sidebar).not.toBeInViewport();
    });

    test('picking a folder in the drawer navigates and closes it', async ({ page }) => {
      await page.goto('/');
      await page.getByTestId('sidebar-open').click();
      await expect(page.getByTestId('sidebar')).toBeInViewport();

      await page.getByTestId('sidebar').getByText('Inspiration').click();
      await expect(page).toHaveURL(/\/c\/2$/);
      await expect(page.getByTestId('sidebar')).not.toBeInViewport();
    });

    test('the post modal stacks media above the written content', async ({ page }) => {
      await page.goto('/p/ig_1');
      await expect(page.getByTestId('post-modal')).toBeVisible();
      // The fixture's posts are text-only (no cover), so MediaCarousel renders
      // its TextCard inside the `post-modal-media` root — the element the narrow
      // layout actually resizes.
      const media = (await page.getByTestId('post-modal-media').boundingBox())!;
      const meta = (await page.getByTestId('post-modal-meta').boundingBox())!;
      // Stacked (not side by side): the written content starts at or below
      // where the media block ends, and both span (close to) the full width.
      expect(meta.y).toBeGreaterThanOrEqual(media.y + media.height - 4);
      expect(media.width).toBeGreaterThan(viewport.width - 24);
      expect(meta.width).toBeGreaterThan(viewport.width - 24);
    });

    test('tap previews a card (no hover), a second tap opens it', async ({ page, api }) => {
      // The fixture's posts default to mediaType 'text' (no cover) — a
      // typographic TextCard with no hover overlay at all. Give this one a
      // cover so it renders the normal image card the preview applies to.
      api.posts[0].mediaType = 'image';
      api.posts[0].coverUrl = PIXEL;
      await page.goto('/');
      const card = page.getByTestId('post-card').first();
      await tap(card, 1);
      // First tap: preview only — still on the library, no modal. The overlay
      // mounts once and never unmounts again (PostCard's lazy-hover-chrome
      // design), so "showing" is its opacity, not Playwright's `visible`
      // (which ignores opacity — an `opacity-0` element still has layout).
      await expect(page).toHaveURL(/\/$/);
      await expect(card.getByTestId('post-card-overlay')).toHaveCSS('opacity', '1');
      await expect(page.getByTestId('post-modal')).toBeHidden();

      // Second tap on the SAME card: opens it, like a click while hovering.
      // (The gallery's own modal doesn't push a route yet — P1-06 carry-over —
      // so "opened" is the modal rendering, not the address bar.)
      await tap(card, 1);
      await expect(page.getByTestId('post-modal')).toBeVisible();
    });

    test('tapping a different card moves the preview, not the post', async ({ page, api }) => {
      api.posts[0].mediaType = 'image';
      api.posts[0].coverUrl = PIXEL;
      api.posts[1].mediaType = 'image';
      api.posts[1].coverUrl = PIXEL;
      await page.goto('/');
      const cards = page.getByTestId('post-card');
      await tap(cards.nth(0), 2);
      await expect(cards.nth(0).getByTestId('post-card-overlay')).toHaveCSS('opacity', '1');

      await tap(cards.nth(1), 3);
      await expect(cards.nth(1).getByTestId('post-card-overlay')).toHaveCSS('opacity', '1');
      await expect(cards.nth(0).getByTestId('post-card-overlay')).toHaveCSS('opacity', '0');
      await expect(page.getByTestId('post-modal')).toBeHidden(); // still just previewing
    });

    test('long-press selects the post (P1-14: bulkActions wires onQuickSelect) and swallows the trailing click', async ({
      page,
    }) => {
      await page.goto('/');
      const card = page.getByTestId('post-card').first();
      const box = (await card.boundingBox())!;
      const x = box.x + box.width / 2;
      const y = box.y + box.height / 2;
      await dispatchPointer(card, 'pointerdown', { pointerId: 9, x, y });
      await page.waitForTimeout(650); // > useLongPress's 500ms delay
      await dispatchPointer(card, 'pointerup', { pointerId: 9, x, y });
      await dispatchClick(card);
      // Long-press arms selection with exactly this post selected (the same
      // onQuickSelect contract tests/components/PostCard.test.tsx covers
      // directly) — the trailing click must not ALSO open the post.
      await expect(page.getByTestId('selection-count')).toHaveText('1 selected');
      await expect(page.getByTestId('post-modal')).toBeHidden();
    });

    test('no per-card backdrop-filter', async ({ page }) => {
      await page.goto('/');
      await expect(page.getByTestId('post-card').first()).toBeVisible();
      const filters = await page.evaluate(() => {
        const out: string[] = [];
        for (const card of document.querySelectorAll('[data-testid="post-card"]')) {
          for (const node of [card, ...card.querySelectorAll('*')]) {
            const cs = getComputedStyle(node) as CSSStyleDeclaration & {
              webkitBackdropFilter?: string;
            };
            out.push(cs.backdropFilter || cs.webkitBackdropFilter || 'none');
          }
        }
        return out;
      });
      for (const f of filters) expect(f === 'none' || f === '').toBeTruthy();
    });
  });
}

defineNarrowBehaviors('mobile', MOBILE_VIEWPORT);
defineNarrowBehaviors('tablet', TABLET_VIEWPORT);

// ── Pinch: InfiniteCanvas only, checked once (gesture math, not layout) ─────
test.describe('mobile (375x812) — canvas pinch', () => {
  test.use({ viewport: MOBILE_VIEWPORT });

  test('two-finger pinch zooms the canvas', async ({ page }) => {
    await page.goto('/');
    await page.getByTestId('view-mode-toggle').click();
    const canvas = page.getByTestId('post-canvas');
    await expect(canvas).toBeVisible();
    const box = (await canvas.boundingBox())!;
    const cx = box.x + box.width / 2;
    const cy = box.y + box.height / 2;

    const before = await canvasScale(canvas);
    await dispatchPointer(canvas, 'pointerdown', {
      pointerId: 1,
      x: cx - 20,
      y: cy,
      isPrimary: true,
    });
    await dispatchPointer(canvas, 'pointerdown', {
      pointerId: 2,
      x: cx + 20,
      y: cy,
      isPrimary: false,
    });
    for (const d of [50, 90, 130, 170]) {
      await dispatchPointer(canvas, 'pointermove', { pointerId: 1, x: cx - d, y: cy });
      await dispatchPointer(canvas, 'pointermove', { pointerId: 2, x: cx + d, y: cy });
    }
    await dispatchPointer(canvas, 'pointerup', { pointerId: 1, x: cx - 170, y: cy });
    await dispatchPointer(canvas, 'pointerup', { pointerId: 2, x: cx + 170, y: cy });
    await page.waitForTimeout(100);

    const after = await canvasScale(canvas);
    expect(after).toBeGreaterThan(before);
  });
});

// ── Desktop (>=900px): the layout the gallery already shipped with ─────────
test.describe('desktop (>=900px) parity', () => {
  // No viewport override: inherits the project's Desktop Chrome default,
  // already >=900px.

  test('bottom nav and the drawer trigger do not exist above the breakpoint', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('bottom-nav')).toBeHidden();
    await expect(page.getByTestId('sidebar-open')).toBeHidden();
    const sidebar = page.getByTestId('sidebar');
    await expect(sidebar).toBeInViewport();
    const box = (await sidebar.boundingBox())!;
    expect(box.x).toBe(0);
    expect(box.width).toBe(240);
  });

  test('the post modal keeps its two-column layout', async ({ page }) => {
    await page.goto('/p/ig_1');
    const media = (await page.getByTestId('post-modal-media').boundingBox())!;
    const meta = (await page.getByTestId('post-modal-meta').boundingBox())!;
    expect(meta.x).toBeGreaterThanOrEqual(media.x + media.width - 4);
    expect(meta.width).toBeLessThan(420); // the fixed 380px column, not full width
  });

  test('screenshots for human review (saved outside the repo via P1_02_SHOT_DIR)', async ({
    page,
  }) => {
    await page.goto('/');
    await expect(page.getByTestId('post-card')).toHaveCount(3);
    await page.screenshot({ path: shotPath('desktop-library') });
    await page.goto('/p/ig_1');
    await expect(page.getByTestId('post-modal')).toBeVisible();
    await page.screenshot({ path: shotPath('desktop-post-modal') });
  });
});
