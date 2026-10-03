// UX-5 — the post modal as a full-screen sheet (UX audit E8, MOD-1…MOD-13).
// The modal-on-route plumbing itself is covered by post-modal.spec.ts and the
// "on top of every shell element" assertion by mobile-shell.spec.ts; this pins
// the sheet-specific behaviour: a clean media fallback, text posts, the header
// prev/next and swipe between posts, menus as bottom sheets, the lightbox on
// top, and the focus trap / Escape.
import { test, expect, apiDetail, type MockApi } from './api';
import { apiObject, apiSlide } from '../tests/fixtures';
import type { Page } from '@playwright/test';

const IPHONE = { width: 390, height: 844 };
const ANDROID = { width: 412, height: 915 };
const DESKTOP = { width: 1440, height: 900 };

// A 1×1 transparent PNG: a data URL that actually decodes, so the modal shows a
// real image (and its click-to-zoom) rather than the failure fallback.
const PIXEL =
  'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==';

// Turn a fixture post into an image post whose stored object 404s (every
// `/media/**` does in the mock) — MediaCarousel's onError then shows the clean
// fallback. Mutates both the list and the detail the modal reads.
function makeMissingImage(api: MockApi, key: string): void {
  const i = api.posts.findIndex((p) => p.key === key);
  const object = apiObject('missing');
  api.posts[i] = {
    ...api.posts[i],
    mediaType: 'image',
    cover: object,
    media: [apiSlide({ object })],
  };
  api.details[key] = apiDetail(api.posts[i]);
}

// Turn a fixture post into an image post whose media is a decodable data URL, so
// the real <img> renders and the lightbox can open from it.
function makeRealImage(api: MockApi, key: string): void {
  const i = api.posts.findIndex((p) => p.key === key);
  api.posts[i] = {
    ...api.posts[i],
    mediaType: 'image',
    cover: null,
    coverUrl: PIXEL,
    media: [apiSlide({ object: null, sourceUrl: PIXEL })],
  };
  api.details[key] = apiDetail(api.posts[i]);
}

// A horizontal swipe across the media element (the gesture Playwright has no
// first-class helper for): dispatch the touch events the component listens for.
async function swipeMedia(page: Page, dx: number): Promise<void> {
  await page.evaluate((dx) => {
    const el = document.querySelector('[data-testid="post-modal-media"]');
    if (!el) throw new Error('no post-modal-media');
    const r = el.getBoundingClientRect();
    const y = r.top + r.height / 2;
    const x0 = r.left + r.width / 2;
    const touch = (x: number): Touch =>
      new Touch({ identifier: 1, target: el, clientX: x, clientY: y });
    el.dispatchEvent(
      new TouchEvent('touchstart', {
        bubbles: true,
        cancelable: true,
        touches: [touch(x0)],
        changedTouches: [touch(x0)],
      }),
    );
    el.dispatchEvent(
      new TouchEvent('touchend', {
        bubbles: true,
        cancelable: true,
        touches: [],
        changedTouches: [touch(x0 + dx)],
      }),
    );
  }, dx);
}

test.afterEach(({ api }) => {
  expect(api.thirdParty, 'requests outside the app').toEqual([]);
});

for (const { name, viewport } of [
  { name: 'iphone', viewport: IPHONE },
  { name: 'android', viewport: ANDROID },
]) {
  test.describe(`narrow: ${name}`, () => {
    test.use({ viewport, hasTouch: true });

    test('a missing image shows the clean fallback, not the broken glyph (MOD-2)', async ({
      page,
      api,
    }) => {
      makeMissingImage(api, 'ig_1');
      await page.goto('/p/ig_1');
      const modal = page.getByTestId('post-modal');
      await expect(modal).toBeVisible();
      // The fallback panel — platform glyph, label, open-original and retry —
      // in place of the browser's broken-image icon with the caption as alt.
      await expect(modal.getByTestId('post-modal-no-media')).toBeVisible();
      await expect(modal.getByText('Media not available')).toBeVisible();
      await expect(modal.getByTestId('post-modal-open-original')).toBeVisible();
      await expect(modal.getByTestId('post-modal-media-retry')).toBeVisible();
      await expect(modal.getByTestId('post-modal-image')).toHaveCount(0);
    });

    test('a text-only post shows its text in the media pane, not a globe (MOD-3)', async ({
      page,
    }) => {
      await page.goto('/p/x_2'); // a text tweet, caption "A thread about chairs"
      const media = page.getByTestId('post-modal-media');
      await expect(media.getByTestId('post-modal-text')).toContainText('A thread about chairs');
      await expect(page.getByTestId('post-modal-no-media')).toHaveCount(0);
      // The caption is the hero here and is not repeated in the meta column.
      await expect(page.getByTestId('post-modal').getByText('A thread about chairs')).toHaveCount(
        1,
      );
    });

    test('prev/next sit in the header and move between posts (MOD-5, MOD-12)', async ({ page }) => {
      await page.goto('/p/ig_1');
      const next = page.getByTestId('post-modal-next');
      await expect(next).toBeVisible();
      // In the header (top), not floating at mid-height like the desktop arrows.
      expect((await next.boundingBox())!.y).toBeLessThan(80);
      await next.click();
      await expect(page).toHaveURL(/\/p\/x_2$/);
      const prev = page.getByTestId('post-modal-prev');
      await expect(prev).toBeVisible();
      await prev.click();
      await expect(page).toHaveURL(/\/p\/ig_1$/);
    });

    test('a horizontal swipe on the media moves to the next post (MOD-5)', async ({ page }) => {
      await page.goto('/p/ig_1');
      await expect(page.getByTestId('post-modal-media')).toBeVisible();
      await swipeMedia(page, -160); // swipe left → next
      await expect(page).toHaveURL(/\/p\/x_2$/);
      await swipeMedia(page, 160); // swipe right → previous
      await expect(page).toHaveURL(/\/p\/ig_1$/);
    });

    test('the more menu opens as a bottom sheet (O8)', async ({ page }) => {
      await page.goto('/p/ig_1');
      await page.getByTestId('post-modal-more').click();
      // The shared Popover's sheet presentation: a scrim and a drag handle.
      await expect(page.getByTestId('popover-sheet-handle')).toBeVisible();
      await expect(page.getByTestId('post-modal-delete-post')).toBeVisible();
    });

    test('focus is trapped and Escape closes the modal (MOD-8)', async ({ page }) => {
      await page.goto('/p/ig_1');
      await expect(page.getByTestId('post-modal')).toBeVisible();
      for (let i = 0; i < 6; i++) await page.keyboard.press('Tab');
      const inside = await page.evaluate(
        () =>
          !!document.querySelector('[data-testid="post-modal"]')?.contains(document.activeElement),
      );
      expect(inside).toBe(true);
      await page.keyboard.press('Escape');
      await expect(page.getByTestId('post-modal')).toBeHidden();
      await expect(page).toHaveURL(/\/$/);
    });

    test('the lightbox opens above the modal and Escape returns to it', async ({ page, api }) => {
      makeRealImage(api, 'ig_1');
      await page.goto('/p/ig_1');
      await page.getByTestId('post-modal-image').click();
      const lightbox = page.getByTestId('image-lightbox');
      await expect(lightbox).toBeVisible();
      // On top of everything (it is portaled at z-modal-over).
      const onTop = await page.evaluate(() => {
        const lb = document.querySelector('[data-testid="image-lightbox"]');
        const el = document.elementFromPoint(window.innerWidth / 2, window.innerHeight / 2);
        return !!lb && !!el && lb.contains(el);
      });
      expect(onTop).toBe(true);
      await page.keyboard.press('Escape');
      await expect(lightbox).toBeHidden();
      await expect(page.getByTestId('post-modal')).toBeVisible(); // the modal stays
    });
  });
}

test.describe('desktop', () => {
  test.use({ viewport: DESKTOP });

  test('prev/next float at the screen edges on wide desktop', async ({ page }) => {
    await page.goto('/p/ig_1');
    const next = page.getByTestId('post-modal-next');
    await expect(next).toBeVisible();
    const box = (await next.boundingBox())!;
    // Floating mid-height at the right edge, outside the centered panel.
    expect(box.y).toBeGreaterThan(200);
    expect(box.x).toBeGreaterThan(1300);
    await next.click();
    await expect(page).toHaveURL(/\/p\/x_2$/);
  });

  test('a missing image shows the clean fallback here too (MOD-2)', async ({ page, api }) => {
    makeMissingImage(api, 'ig_1');
    await page.goto('/p/ig_1');
    await expect(page.getByTestId('post-modal-no-media')).toBeVisible();
    await expect(page.getByTestId('post-modal-image')).toHaveCount(0);
  });
});
