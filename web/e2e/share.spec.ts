// `/share` (plan §2.17, §2.19 Routes; contract C7; P2-07 acceptance 3, 5):
// where Android's share target, the bookmarklet and the iOS Shortcut land.
// `POST /links` itself is not in web/e2e/api.ts's mock yet — it lands with
// P2-11 — so each test here adds its own route for it, registered after the
// `api` fixture's (Playwright runs the most-recently-registered matching
// route first), the same way a test overrides any other default.
import type { Route } from '@playwright/test';
import { test, expect } from './api';

interface LinkAnswer {
  status: number;
  key: string;
  platform: string;
  created: boolean;
}

async function mockLinks(
  page: import('@playwright/test').Page,
  answer: LinkAnswer | { error: number },
): Promise<{ bodies: unknown[] }> {
  const bodies: unknown[] = [];
  await page.route('**/api/v1/links', async (route: Route) => {
    bodies.push(route.request().postDataJSON());
    if ('error' in answer) {
      return route.fulfill({
        status: answer.error,
        contentType: 'application/problem+json',
        body: JSON.stringify({ type: 'about:blank', title: 'error', status: answer.error }),
      });
    }
    return route.fulfill({
      status: answer.status,
      json: { key: answer.key, platform: answer.platform, created: answer.created },
    });
  });
  return { bodies };
}

test('a bare ?url= (the bookmarklet) is saved and offers Open', async ({ page }) => {
  const { bodies } = await mockLinks(page, {
    status: 201,
    key: 'ig_1',
    platform: 'instagram',
    created: true,
  });
  await page.goto('/share?url=https%3A%2F%2Fwww.instagram.com%2Fp%2FC0ffee%2F');
  await expect(page.getByTestId('share-saved')).toBeVisible();
  await expect(page.getByTestId('share-saved')).toContainText('Saved');
  expect(bodies).toEqual([{ url: 'https://www.instagram.com/p/C0ffee/', note: null, tags: null }]);
  await page.getByTestId('share-open').click();
  await expect(page).toHaveURL(/\/p\/ig_1$/);
});

test('?text= (how Android shares a page) extracts the URL out of the caption', async ({ page }) => {
  const { bodies } = await mockLinks(page, {
    status: 200,
    key: 'x_2',
    platform: 'twitter',
    created: false,
  });
  await page.goto('/share?text=Check%20this%20out%3A%20https%3A%2F%2Fx.com%2Fstudio%2Fstatus%2F2');
  await expect(page.getByTestId('share-saved')).toBeVisible();
  await expect(page.getByTestId('share-saved')).toContainText('already');
  expect(bodies).toEqual([{ url: 'https://x.com/studio/status/2', note: null, tags: null }]);
});

test('no URL anywhere in the share: says so, without calling the API', async ({ page }) => {
  const { bodies } = await mockLinks(page, { status: 201, key: 'x', platform: 'x', created: true });
  await page.goto('/share?text=just%20a%20caption%2C%20no%20link%20here');
  await expect(page.getByTestId('share-no-link')).toBeVisible();
  expect(bodies).toEqual([]);
  await page.getByTestId('share-back').click();
  await expect(page).toHaveURL(/\/$/);
});

test('a failed save shows the error and retry tries again', async ({ page }) => {
  await mockLinks(page, { error: 500 });
  await page.goto('/share?url=https%3A%2F%2Fexample.test%2Fa');
  await expect(page.getByTestId('share-error')).toBeVisible();

  const { bodies } = await mockLinks(page, {
    status: 201,
    key: 'web_1',
    platform: 'web',
    created: true,
  });
  await page.getByTestId('share-retry').click();
  await expect(page.getByTestId('share-saved')).toBeVisible();
  expect(bodies).toEqual([{ url: 'https://example.test/a', note: null, tags: null }]);
});

test('signed out, /share waits behind sign-in with its query intact', async ({ page, api }) => {
  api.signedIn = false;
  await page.goto('/share?url=https%3A%2F%2Fexample.test%2Fa');
  await expect(page.getByTestId('login-form')).toBeVisible();
  await expect(page).toHaveURL(/\/login\?next=/);

  const { bodies } = await mockLinks(page, {
    status: 201,
    key: 'web_1',
    platform: 'web',
    created: true,
  });
  api.signedIn = true;
  await page.reload();
  await expect(page.getByTestId('share-saved')).toBeVisible();
  expect(bodies).toEqual([{ url: 'https://example.test/a', note: null, tags: null }]);
});
