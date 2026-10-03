// P1-14: Gallery selection, bulk actions and the Trash view. `web/e2e/api.ts`
// mocks `/posts/bulk`, `/trash`, `/trash/restore` and `/trash/empty` in
// memory (P1 lane rule 6); `GET /posts` now pages for real so a synthetic
// batch can legitimately exceed one loaded page, exercising "select all
// matching" (`{filter, exceptKeys}`) the same way a real large library would.
//
// Two mock quirks the specs below work around:
//  - The mock answers instantly (no network latency), so Gallery's eager
//    infinite-scroll prefetch can race ahead and load an entire synthetic
//    batch before a test gets to click "select all" — defeating the point of
//    the test. `api.postsDelayMs` adds a small artificial delay, the same way
//    a real network would naturally pace it.
//  - `post-card` elements are virtualized (only the rows near the viewport
//    are mounted), so its DOM count reflects viewport/overscan, NOT how many
//    posts are actually loaded — never used here as a pagination proxy.
//    Once `/trash` has been visited it also stays mounted (keep-alive), so
//    `post-card` becomes ambiguous between the two grids; every query below
//    is scoped to `gallery-view` or `trash-view` accordingly.
import { test, expect, apiPost } from './api';
import { MOBILE_VIEWPORT } from '../playwright.config';

test.afterEach(({ api }) => {
  expect(api.thirdParty, 'requests outside the app').toEqual([]);
});

// A batch larger than Gallery's first load (50): selecting "all" over it can
// only be satisfied by a filter, never by materializing every id. Replaces
// the fixture's one Pinterest post (pin_3) so the resulting platform total is
// exactly `n`, not `n + 1`.
function pushSyntheticBatch(posts: ReturnType<typeof apiPost>[], n: number): void {
  const kept = posts.filter((p) => p.platform !== 'pinterest');
  posts.length = 0;
  posts.push(...kept);
  for (let i = 0; i < n; i++) {
    posts.push(
      apiPost({
        key: `synth_${i}`,
        platform: 'pinterest',
        postUrl: `https://www.pinterest.com/pin/${i}/`,
        shortcode: null,
        caption: `Synthetic pin ${i}`,
      }),
    );
  }
}

test('select all → delete → the trash count matches → restore → empty (P1-14 acceptance)', async ({
  page,
  api,
}) => {
  api.postsDelayMs = 3000;
  pushSyntheticBatch(api.posts, 70);
  const gallery = page.getByTestId('gallery-view');
  await page.goto('/');
  await page.getByTestId('sidebar').getByTestId('source-pinterest').click();
  // Settles once the platform switch's first page lands — the authoritative
  // signal (total, not DOM-rendered/virtualized card count).
  await expect(gallery.getByText('70 posts', { exact: true })).toBeVisible();

  // ── select all (over everything the filter matches, not just loaded) ───
  await page.getByTestId('select-toggle').click();
  const selectAll = page.getByTestId('select-all-matching');
  await expect(selectAll).toContainText('70');
  await selectAll.click();
  await expect(page.getByTestId('selection-count')).toHaveText('70 selected');

  // ── delete: moves them to the trash, as one {filter, exceptKeys} request ──
  await page.getByTestId('bulk-actions').click();
  await page.getByTestId('bulk-delete-posts').click();
  await page.getByTestId('bulk-delete-posts-confirm').click();
  await expect(page.getByTestId('selection-count')).toHaveCount(0); // back to browse mode
  const bulkCalls = api.requestsTo('/api/v1/posts/bulk', 'POST');
  expect(bulkCalls).toHaveLength(1);
  expect(bulkCalls[0].body).toMatchObject({
    action: 'delete',
    selector: { filter: expect.anything() },
  });

  // ── the trash count matches what was deleted ────────────────────────────
  await page.getByTestId('nav-trash').click();
  await expect(page).toHaveURL(/\/trash$/);
  const trash = page.getByTestId('trash-view');
  await expect(page.getByTestId('trash-count')).toHaveText('70 in trash');

  // ── restore: brings them all back ───────────────────────────────────────
  await page.getByTestId('trash-select-toggle').click();
  await page.getByTestId('trash-select-all').click();
  await expect(page.getByTestId('trash-selection-count')).toHaveText('70 selected');
  await page.getByTestId('trash-restore').click();
  await page.getByTestId('trash-restore').click(); // two-step confirm
  await expect(page.getByTestId('trash-empty-state')).toBeVisible();
  await expect(page.getByTestId('trash-count')).toHaveText('0 in trash');
  expect(trash).toBeTruthy(); // keeps the `trash` binding meaningful for readers

  await page.getByTestId('source-all').click();
  await page.getByTestId('sidebar').getByTestId('source-pinterest').click();
  await expect(gallery.getByText('70 posts', { exact: true })).toBeVisible();

  // ── empty: delete them again, then permanently purge the trash ─────────
  await page.getByTestId('select-toggle').click();
  await page.getByTestId('select-all-matching').click();
  await page.getByTestId('bulk-actions').click();
  await page.getByTestId('bulk-delete-posts').click();
  await page.getByTestId('bulk-delete-posts-confirm').click();

  await page.getByTestId('nav-trash').click();
  await expect(page.getByTestId('trash-count')).toHaveText('70 in trash');
  await page.getByTestId('trash-empty').click();
  await page.getByTestId('trash-empty').click(); // two-step confirm
  await expect(page.getByTestId('trash-empty-state')).toBeVisible();
  await expect(page.getByTestId('trash-count')).toHaveText('0 in trash');
  expect(api.requestsTo('/api/v1/trash/empty', 'POST')).toHaveLength(1);

  // Gone for good: the pinterest view now has nothing, even after the purge.
  await page.getByTestId('source-all').click();
  await page.getByTestId('sidebar').getByTestId('source-pinterest').click();
  await expect(gallery.getByText('0 posts', { exact: true })).toBeVisible();
});

test('a manual (click/shift) selection deletes by explicit keys, with Undo', async ({
  page,
  api,
}) => {
  const gallery = page.getByTestId('gallery-view');
  await page.goto('/');
  await expect(gallery.getByTestId('post-card')).toHaveCount(3);
  await page.getByTestId('select-toggle').click();
  await gallery.getByTestId('post-card').first().click();
  await expect(page.getByTestId('selection-count')).toHaveText('1 selected');

  await page.getByTestId('bulk-actions').click();
  await page.getByTestId('bulk-delete-posts').click();
  await page.getByTestId('bulk-delete-posts-confirm').click();
  await expect(gallery.getByTestId('post-card')).toHaveCount(2);
  const bulkCalls = api.requestsTo('/api/v1/posts/bulk', 'POST');
  expect(bulkCalls[0].body).toMatchObject({ selector: { keys: ['ig_1'] }, action: 'delete' });

  // Undo brings it back without a page reload.
  await page.getByTestId('undo-action').click();
  await expect(gallery.getByTestId('post-card')).toHaveCount(3);
  expect(api.requestsTo('/api/v1/trash/restore', 'POST')).toHaveLength(1);
});

test('clears AI descriptions and tags in bulk', async ({ page, api }) => {
  api.posts[0].aiDescription = 'A lamp';
  api.posts[0].aiTags = ['glass'];
  const gallery = page.getByTestId('gallery-view');
  await page.goto('/');
  await page.getByTestId('select-toggle').click();
  await gallery.getByTestId('post-card').first().click();

  await page.getByTestId('bulk-actions').click();
  await page.getByTestId('bulk-clear-descriptions').click();
  await page.getByTestId('bulk-clear-descriptions-confirm').click();
  // Clearing descriptions/tags never exits select mode, so the feedback
  // shows in the action toolbar itself ("bulk-feedback"), not the post-exit
  // toast ("bulk-feedback-toast") the delete/undo flows use.
  await expect(page.getByTestId('bulk-feedback')).toBeVisible();
  let calls = api.requestsTo('/api/v1/posts/bulk', 'POST');
  expect(calls[calls.length - 1].body).toMatchObject({ action: 'clearAiDescription' });

  // Clearing an action only resets the SELECTION (clearSelection), not select
  // mode itself — still in it, so re-select the card directly.
  await gallery.getByTestId('post-card').first().click();
  await page.getByTestId('bulk-actions').click();
  await page.getByTestId('bulk-clear-tags').click();
  await page.getByTestId('bulk-clear-tags-confirm').click();
  calls = api.requestsTo('/api/v1/posts/bulk', 'POST');
  expect(calls[calls.length - 1].body).toMatchObject({ action: 'clearAiTags' });
});

test('adds a select-all-matching selection to a folder (addToCollections by filter)', async ({
  page,
  api,
}) => {
  api.postsDelayMs = 3000;
  pushSyntheticBatch(api.posts, 60);
  const gallery = page.getByTestId('gallery-view');
  await page.goto('/');
  await page.getByTestId('sidebar').getByTestId('source-pinterest').click();
  await expect(gallery.getByText('60 posts', { exact: true })).toBeVisible();

  await page.getByTestId('select-toggle').click();
  await page.getByTestId('select-all-matching').click();
  await expect(page.getByTestId('selection-count')).toHaveText('60 selected');

  await page.getByTestId('bulk-actions').click();
  await page.getByTestId('assign-to-2').click(); // "Inspiration"
  const calls = api.requestsTo('/api/v1/posts/bulk', 'POST');
  expect(calls[calls.length - 1].body).toMatchObject({
    action: 'addToCollections',
    params: { collectionIds: [2] },
  });
});

test('deletes a post from its modal, with Undo', async ({ page, api }) => {
  const gallery = page.getByTestId('gallery-view');
  await page.goto('/p/ig_1');
  await page.getByTestId('post-modal-more').click();
  await page.getByTestId('post-modal-delete-post').click();
  await page.getByTestId('post-modal-delete-post').click();
  await expect(page.getByTestId('post-modal')).toBeHidden();
  await expect(gallery.getByTestId('post-card')).toHaveCount(2);
  expect(api.requestsTo('/api/v1/posts/bulk', 'POST')[0].body).toMatchObject({
    selector: { keys: ['ig_1'] },
    action: 'delete',
  });
  await expect(page.getByTestId('undo-action')).toBeVisible();
});

test('deleting a folder "with posts" moves them to the trash', async ({ page }) => {
  await page.goto('/');
  await page.getByTestId('sidebar').getByText('Lighting').hover();
  await page.getByTestId('edit-collection-1').click();
  await page.getByTestId('collection-delete').click();
  await page.getByTestId('collection-delete-mode-posts').click();
  await page.getByTestId('collection-delete-confirm').click();
  await expect(page.getByTestId('collection-modal')).toHaveCount(0);

  await page.getByTestId('nav-trash').click();
  await expect(page.getByTestId('trash-count')).toHaveText('1 in trash');
});

test('the full media-type facet and the category/content-type/AI-status selects', async ({
  page,
  api,
}) => {
  await page.goto('/');
  await page.getByTestId('filters-toggle').click();
  const drawer = page.getByTestId('filter-drawer');
  await expect(drawer.getByTestId('drawer-mediatype')).toContainText('Website');
  await expect(drawer.getByTestId('drawer-mediatype')).toContainText('File');
  await drawer.getByTestId('drawer-mediatype').getByText('Website', { exact: true }).click();
  await expect
    .poll(() => api.requestsTo('/api/v1/posts').at(-1)?.query.getAll('mediaType'))
    .toEqual(['website']);

  await drawer.getByTestId('drawer-category-select').selectOption('technology');
  await expect
    .poll(() => api.requestsTo('/api/v1/posts').at(-1)?.query.get('category'))
    .toBe('technology');

  await drawer.getByTestId('drawer-contenttype-select').selectOption('saas');
  await expect
    .poll(() => api.requestsTo('/api/v1/posts').at(-1)?.query.get('contentType'))
    .toBe('saas');

  // AI status: web only (no desktop equivalent — see FilterDrawer's
  // showAiStatus doc comment), so it must be present here.
  await drawer.getByTestId('drawer-aistatus-select').selectOption('error');
  await expect
    .poll(() => api.requestsTo('/api/v1/posts').at(-1)?.query.get('aiStatus'))
    .toBe('error');
});

test.describe('mobile (375x812)', () => {
  test.use({ viewport: MOBILE_VIEWPORT });

  test('select, bulk-delete and restore from the Trash view', async ({ page, api }) => {
    const gallery = page.getByTestId('gallery-view');
    await page.goto('/');
    await expect(gallery.getByTestId('post-card')).toHaveCount(3);
    await page.getByTestId('select-toggle').click();
    await gallery.getByTestId('post-card').first().click();
    await page.getByTestId('bulk-actions').click();
    await page.getByTestId('bulk-delete-posts').click();
    await page.getByTestId('bulk-delete-posts-confirm').click();
    await expect(gallery.getByTestId('post-card')).toHaveCount(2);

    // Narrow: the sidebar is a drawer (P1-02) — open it to reach Trash.
    await page.getByTestId('sidebar-open').click();
    await page.getByTestId('nav-trash').click();
    await expect(page).toHaveURL(/\/trash$/);
    const trash = page.getByTestId('trash-view');
    await expect(page.getByTestId('trash-count')).toHaveText('1 in trash');

    await page.getByTestId('trash-select-toggle').click();
    await trash.getByTestId('post-card').click();
    await page.getByTestId('trash-restore').click();
    await page.getByTestId('trash-restore').click();
    await expect(page.getByTestId('trash-empty-state')).toBeVisible();
    expect(api.requestsTo('/api/v1/trash/restore', 'POST')).toHaveLength(1);
  });
});
