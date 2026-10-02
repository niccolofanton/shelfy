// P1-06: the gallery's own post modal moves onto the `/p/:key` route (a card
// click navigates, closing goes back, prev/next replace the route — so one
// modal owns the address, on both the default (desktop) viewport and the
// P1-02 mobile breakpoint), and its edits (note, manual tags, folders) go
// through the ShelfyClient seam and persist across a reload.
import { test, expect } from './api';
import { MOBILE_VIEWPORT } from '../playwright.config';

test.afterEach(({ api }) => {
  expect(api.thirdParty, 'requests outside the app').toEqual([]);
});

// Shared behaviors, run at both the default (>=900px) viewport and the P1-02
// mobile breakpoint — the modal-on-route plumbing is viewport-independent.
function defineModalRouteBehaviors(
  name: string,
  viewport?: { width: number; height: number },
): void {
  test.describe(name, () => {
    if (viewport) test.use({ viewport });

    test('a card click opens the post at /p/:key; closing it returns to the folder it was opened from', async ({
      page,
    }) => {
      await page.goto('/c/1');
      await expect(page.getByTestId('post-card')).toHaveCount(1);
      await page.getByTestId('post-card').click();
      await expect(page).toHaveURL(/\/p\/ig_1$/);
      const modal = page.getByTestId('post-modal');
      await expect(modal).toBeVisible();

      await page.getByTestId('post-modal-close').click();
      await expect(modal).toBeHidden();
      // Closing goes back in history (not to the library): the folder's own
      // address, since that's where the click happened.
      await expect(page).toHaveURL(/\/c\/1$/);
      await expect(page.getByTestId('active-collection-chip')).toHaveText('Lighting');
    });

    test('prev/next replace the route instead of pushing a new history entry', async ({ page }) => {
      await page.goto('/');
      await expect(page.getByTestId('post-card')).toHaveCount(3);
      await page.getByTestId('post-card').first().click();
      await expect(page).toHaveURL(/\/p\/ig_1$/);

      await page.getByTestId('post-modal-next').click();
      await expect(page).toHaveURL(/\/p\/x_2$/);
      await expect(page.getByTestId('post-modal').getByText('A thread about chairs')).toBeVisible();

      await page.getByTestId('post-modal-next').click();
      await expect(page).toHaveURL(/\/p\/pin_3$/);

      // Both "next" clicks replaced the route: one `back()` from the third
      // post leaves the modal entirely (there was only ever one push, the
      // original card click), landing on the library it was opened from.
      await page.goBack();
      await expect(page).toHaveURL(/\/$/);
      await expect(page.getByTestId('post-modal')).toBeHidden();
    });
  });
}

defineModalRouteBehaviors('desktop (>=900px)');
defineModalRouteBehaviors('mobile (375x812)', MOBILE_VIEWPORT);

test('editing a note goes through the seam and survives a reload', async ({ page }) => {
  await page.goto('/p/x_2');
  const modal = page.getByTestId('post-modal');
  await expect(modal).toBeVisible();
  // x_2 has no note yet (the fixture's default): the "add a note" affordance.
  await modal.getByTestId('post-modal-note-add').click();
  await modal.getByTestId('post-modal-note-input').fill('Needs a better photo');
  await modal.getByTestId('post-modal-note-save').click();
  await expect(modal.getByTestId('post-modal-note')).toHaveText('Needs a better photo');

  await page.reload();
  await expect(page.getByTestId('post-modal').getByTestId('post-modal-note')).toHaveText(
    'Needs a better photo',
  );
});

test('adds this post to a folder, and the same picker removes it again (§1.2 #12)', async ({
  page,
}) => {
  await page.goto('/p/x_2'); // x_2 starts in no folder
  await page.getByTestId('post-modal-assign-toggle').click();
  await page.getByTestId('post-modal-assign-to-1').click(); // add
  await page.reload();
  await page.getByTestId('post-modal-assign-toggle').click();
  const lightingRow = page.getByTestId('post-modal-assign-to-1');
  await expect(lightingRow).toContainText('Lighting');
  await expect(lightingRow.locator('svg')).toHaveCount(1); // the green check

  await lightingRow.click(); // already a member: the same row removes it
  await page.reload();
  await page.getByTestId('post-modal-assign-toggle').click();
  await expect(page.getByTestId('post-modal-assign-to-1').locator('svg')).toHaveCount(0);
});

test('the sidebar has an unconditional Trash entry', async ({ page }) => {
  await page.goto('/');
  await page.getByTestId('nav-trash').click();
  await expect(page).toHaveURL(/\/trash$/);
  // P1-11/P1-14 build the view itself; today it says so.
  await expect(page.getByTestId('route-unavailable')).toBeVisible();
});

test('creates a new folder from the sidebar, then deletes it (label only)', async ({ page }) => {
  await page.goto('/');
  await page.getByTestId('add-source-btn').click();
  await page.getByTestId('collection-name-input').fill('Ricette');
  await page.getByTestId('collection-save').click();
  // The fixture's two folders are ids 1 and 2: the mock assigns the next one, 3.
  const row = page.getByTestId('source-collection-3');
  await expect(row).toContainText('Ricette');

  await row.hover();
  await row.getByTestId('edit-collection-3').click();
  await page.getByTestId('collection-delete').click();
  // Only "label" is offered: bulkActions (P1-14) isn't on yet.
  await expect(page.getByTestId('collection-delete-mode-posts')).toHaveCount(0);
  await page.getByTestId('collection-delete-confirm').click();
  await expect(page.getByTestId('source-collection-3')).toHaveCount(0);
});
