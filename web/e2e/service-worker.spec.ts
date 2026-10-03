// The service worker (web/src/sw.ts; P2-07 acceptance 1, 5). Every other spec
// in this suite runs with `serviceWorkers: 'block'` (web/playwright.config.ts):
// once a worker claims a page it handles that page's fetches itself, which
// `page.route()` cannot see, breaking web/e2e/api.ts's mocking (the regression
// this file's own header comment explains). This is the one place that turns
// the worker back on, to test the worker itself instead of around it.
import { test, expect } from './api';

test.use({ serviceWorkers: 'allow' });

test.afterEach(({ api }) => {
  expect(api.thirdParty, 'requests outside the app').toEqual([]);
});

const G480_CACHE = 'shelfy-media-g480';

test('registers and reaches the active state', async ({ page }) => {
  await page.goto('/');
  // `.ready` resolves once there is an active worker, which can still be
  // mid-transition out of 'activating' at that exact microtask; wait for the
  // state itself to settle rather than racing it.
  await page.waitForFunction(async () => {
    const registration = await navigator.serviceWorker.ready;
    return registration.active?.state === 'activated';
  });
});

test('a cached grid tile still answers offline; an uncached one does not', async ({
  page,
  context,
}) => {
  await page.goto('/');
  await page.evaluate(() => navigator.serviceWorker.ready);

  // Seeds the cache the CacheFirst rule reads (web/src/sw.ts), the same way a
  // real online visit would have filled it — without needing a real image
  // fixture or a network round trip a Service Worker's own fetch would make
  // outside page.route()'s reach anyway.
  await page.evaluate(async (cacheName) => {
    const cache = await caches.open(cacheName);
    await cache.put(
      '/media/cached-sha.g480.webp',
      new Response('fake-webp-bytes', {
        status: 200,
        headers: { 'Content-Type': 'image/webp' },
      }),
    );
  }, G480_CACHE);

  await context.setOffline(true);
  try {
    const cached = await page.evaluate(async () => {
      const res = await fetch('/media/cached-sha.g480.webp');
      return { ok: res.ok, body: await res.text() };
    });
    expect(cached).toEqual({ ok: true, body: 'fake-webp-bytes' });

    // A tile nothing ever cached: offline means it genuinely fails, proving
    // the rule serves from its own cache rather than fabricating an answer.
    const uncachedFailed = await page
      .evaluate(() => fetch('/media/never-seen.g480.webp'))
      .then(() => false)
      .catch(() => true);
    expect(uncachedFailed).toBe(true);
  } finally {
    await context.setOffline(false);
  }
});
