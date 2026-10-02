// Smoke test of the web app's addresses (P1-04): every route opened directly
// renders its page, the sign-in page remembers a deep link, navigation keeps
// the address bar in step, the live stream reloads the library, and a view
// that crashes shows its boundary and reports to `POST /client-errors`.
import { test, expect, apiDetail, apiPost, sse, HELLO } from './api';

test.afterEach(({ api }) => {
  expect(api.thirdParty, 'requests outside the app').toEqual([]);
});

test('/ shows the whole library', async ({ page, api }) => {
  await page.goto('/');
  await expect(page.getByTestId('post-card')).toHaveCount(3);
  const sidebar = page.getByTestId('sidebar');
  await expect(sidebar.getByText('Lighting')).toBeVisible();
  await expect(sidebar.getByTestId('source-all')).toHaveAttribute('aria-current', 'page');
  expect(api.requestsTo('/api/v1/posts')[0].query.get('collection')).toBeNull();
});

test('/c/:collectionId shows one folder', async ({ page, api }) => {
  await page.goto('/c/1');
  await expect(page.getByTestId('active-collection-chip')).toHaveText('Lighting');
  await expect(page.getByTestId('post-card')).toHaveCount(1);
  expect(api.requestsTo('/api/v1/posts').map((r) => r.query.get('collection'))).toEqual(['1']);
});

test('/p/:key opens the post over the library, and closing it leaves for /', async ({ page }) => {
  await page.goto('/p/x_2');
  const modal = page.getByTestId('post-modal');
  await expect(modal).toBeVisible();
  await expect(modal.getByText('A thread about chairs')).toBeVisible();
  await expect(page.getByTestId('post-card')).toHaveCount(3);
  await page.getByTestId('post-modal-close').click();
  await expect(modal).toBeHidden();
  await expect(page).toHaveURL(/\/$/);
});

test('pages this client cannot show yet say so', async ({ page }) => {
  await page.goto('/trash');
  await expect(page.getByTestId('route-unavailable')).toBeVisible();
  await expect(page.getByTestId('sidebar')).toBeVisible();
});

test('/settings and /device open their pages', async ({ page }) => {
  for (const path of ['/settings/account', '/settings']) {
    await page.goto(path);
    await expect(page.getByTestId('settings-section-account'), path).toBeVisible();
    await expect(page.getByTestId('account-email'), path).toHaveText('owner@example.test');
  }
  await page.goto('/device#BCDF-GHJK');
  await expect(page.getByTestId('device-code')).toHaveValue('BCDF-GHJK');
  await expect(page).toHaveURL(/\/device$/);
  await page.getByTestId('device-cancel').click();
  await expect(page.getByTestId('post-card')).toHaveCount(3);
});

test('an address with nothing behind it is not found', async ({ page }) => {
  await page.goto('/somewhere/else');
  await expect(page.getByTestId('route-not-found')).toBeVisible();
  await page.goto('/p/ig_404');
  await expect(page.getByTestId('route-not-found')).toBeVisible();
  await page.getByTestId('route-back').click();
  await expect(page).toHaveURL(/\/$/);
  await expect(page.getByTestId('route-not-found')).toBeHidden();
  await expect(page.getByTestId('post-card')).toHaveCount(3);
});

test('signed out, a deep link waits behind the sign-in page', async ({ page, api }) => {
  api.signedIn = false;
  await page.goto('/c/1');
  await expect(page.getByTestId('login-form')).toBeVisible();
  await expect(page).toHaveURL(/\/login\?next=%2Fc%2F1$/);

  // Signed in meanwhile (in the link's tab): this page opens the deep link.
  api.signedIn = true;
  await page.reload();
  await expect(page.getByTestId('active-collection-chip')).toHaveText('Lighting');
  await expect(page).toHaveURL(/\/c\/1$/);
});

test('signed in, /login leaves for the library', async ({ page }) => {
  await page.goto('/login');
  await expect(page.getByTestId('post-card')).toHaveCount(3);
  await expect(page).toHaveURL(/\/$/);
});

test('folders get an address, and the back button follows', async ({ page }) => {
  await page.goto('/');
  await expect(page.getByTestId('post-card')).toHaveCount(3);
  await page.getByTestId('sidebar').getByText('Inspiration').click();
  await expect(page).toHaveURL(/\/c\/2$/);
  await expect(page.getByTestId('active-collection-chip')).toHaveText('Inspiration');
  await expect(page.getByTestId('post-card')).toHaveCount(1);
  await page.goBack();
  await expect(page).toHaveURL(/\/$/);
  await expect(page.getByTestId('active-collection-chip')).toBeHidden();
  await expect(page.getByTestId('post-card')).toHaveCount(3);
  await page.goForward();
  await expect(page.getByTestId('active-collection-chip')).toHaveText('Inspiration');
});

test('a posts.changed event reloads the library', async ({ page, api }) => {
  api.streams.push(HELLO);
  await page.goto('/');
  await expect(page.getByTestId('post-card')).toHaveCount(3);

  // The next connection (the stream reconnects after each mocked response)
  // carries a new post.
  api.posts.unshift(apiPost({ key: 'ig_4', caption: 'A new lamp' }));
  api.streams.push(HELLO + sse('posts.changed', { keys: ['ig_4'], reason: 'ingest' }, 'e-1'));
  await expect(page.getByTestId('post-card')).toHaveCount(4);
  const resumed = api.requestsTo('/api/v1/events').map((r) => r.query.get('lastEventId'));
  expect(resumed.slice(0, 2)).toEqual([null, 'e-0']);
});

test('a view that crashes shows its boundary and is reported', async ({ page, api }) => {
  // A malformed post the modal cannot render (tags must be a list; unlike
  // PostCard, AiPanel doesn't guard it). In `posts` (P1-06: a deep link
  // resolves from POST /posts/batch-get first, the same list shape a card
  // click already has) as well as `details` (GET /posts/{key}).
  const post = apiPost({ key: 'ig_9', caption: 'PRIVATE CAPTION', aiDescription: 'A lamp' });
  const malformed = { ...post, aiTags: 'not a list' } as unknown as typeof post;
  api.posts.push(malformed);
  api.details.ig_9 = apiDetail(malformed);
  const reported = page.waitForRequest('**/api/v1/client-errors');
  await page.goto('/p/ig_9');
  await expect(page.getByTestId('error-boundary')).toBeVisible();
  await expect(page.getByTestId('sidebar')).toBeVisible();

  const report = (await reported).postDataJSON();
  expect(report).toMatchObject({ view: 'postModal', route: '/p/:key', name: 'TypeError' });
  expect(JSON.stringify(report)).not.toContain('PRIVATE CAPTION');
  expect(api.requestsTo('/api/v1/client-errors', 'POST')).toHaveLength(1);

  await page.getByTestId('error-retry').click();
  await expect(page.getByTestId('error-boundary')).toBeVisible();
});
