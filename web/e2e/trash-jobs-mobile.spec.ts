// UX-6 (docs/web-port/reviews/ux-audit.md): Trash and Jobs on phones.
//   TR-1/TR-2  an empty trash shows the shared EmptyState, no retention line, no "Empty trash"
//   TR-3/TR-4  the MenuButton sits in the header row; toasts clear the BottomNav
//   TR-5       selection actions in a bottom bar, 44px targets
//   JOB-1/2    queues only while they have work; EmptyState; labeled queue menu
//   JOB-3/4    attempts only after a retry; the state chips are one scrolling row
// Mocked API (web/e2e/api.ts). Screenshots for the lane report: UX6_SHOT_DIR.
import path from 'node:path';
import type { Page } from '@playwright/test';
import { test, expect, apiJob, apiPost } from './api';

const NARROW = [
  { name: 'iphone', viewport: { width: 390, height: 844 } },
  { name: 'android', viewport: { width: 412, height: 915 } },
];
const SHOT_DIR = process.env.UX6_SHOT_DIR;

test.afterEach(({ api }) => {
  expect(api.thirdParty, 'requests outside the app').toEqual([]);
});

async function shot(page: Page, name: string): Promise<void> {
  if (SHOT_DIR) await page.screenshot({ path: path.join(SHOT_DIR, `${name}.png`) });
}

for (const { name, viewport } of NARROW) {
  test.describe(`narrow: ${name}`, () => {
    test.use({ viewport, hasTouch: true });

    test('an empty trash: EmptyState, no retention line, no Empty button, menu in the header', async ({
      page,
    }) => {
      await page.goto('/trash');
      const empty = page.getByTestId('trash-empty-state');
      await expect(empty).toBeVisible();
      await expect(empty).toContainText('Trash is empty');
      await expect(page.getByTestId('trash-empty')).toHaveCount(0);
      await expect(page.getByTestId('trash-view').getByText(/before they are deleted/)).toHaveCount(
        0,
      );
      const menu = await page.getByTestId('trash-view').getByTestId('sidebar-open').boundingBox();
      expect(menu!.width).toBeGreaterThanOrEqual(44);
      expect(menu!.height).toBeGreaterThanOrEqual(44);
      await shot(page, `trash-empty-${name}`);
    });

    test('selecting in the trash: a bottom bar with 44px actions, toasts above the BottomNav', async ({
      page,
      api,
    }) => {
      api.posts.length = 0;
      api.posts.push(
        apiPost({ key: 'a_1', deletedAt: 1 }),
        apiPost({ key: 'a_2', deletedAt: 2, shortcode: 'two' }),
      );
      await page.goto('/trash');
      await expect(page.getByTestId('trash-view').getByTestId('post-card')).toHaveCount(2);
      await expect(
        page.getByTestId('trash-view').getByText(/before they are deleted/),
      ).toBeVisible();
      await shot(page, `trash-${name}`);

      for (const id of ['trash-select-toggle', 'trash-empty']) {
        const box = (await page.getByTestId(id).boundingBox())!;
        expect(box.width, id).toBeGreaterThanOrEqual(44);
        expect(box.height, id).toBeGreaterThanOrEqual(44);
        expect(box.x + box.width, id).toBeLessThanOrEqual(viewport.width);
      }

      await page.getByTestId('trash-select-toggle').click();
      const bar = page.getByTestId('trash-selection-bar');
      await expect(bar).toBeVisible();
      await page.getByTestId('trash-select-all').click();
      await expect(page.getByTestId('trash-selection-count')).toHaveText('2 selected');
      for (const id of ['trash-select-cancel', 'trash-select-all', 'trash-restore']) {
        const box = (await bar.getByTestId(id).boundingBox())!;
        expect(box.height, id).toBeGreaterThanOrEqual(44);
        expect(box.x, id).toBeGreaterThanOrEqual(0);
        expect(box.x + box.width, id).toBeLessThanOrEqual(viewport.width);
      }
      await shot(page, `trash-select-${name}`);

      await page.getByTestId('trash-restore').click();
      await page.getByTestId('trash-restore').click(); // two-step confirm
      const toast = page.getByTestId('trash-feedback-toast').filter({ hasText: 'restored' });
      await expect(toast).toBeVisible();
      const nav = await page.getByTestId('bottom-nav').boundingBox();
      const toastBox = (await toast.boundingBox())!;
      expect(toastBox.y + toastBox.height).toBeLessThanOrEqual(nav!.y);
    });

    test('jobs: EmptyState with no queues; chips in one scrolling row (JOB-2, JOB-4)', async ({
      page,
    }) => {
      await page.goto('/jobs');
      await expect(page.getByTestId('jobs-empty')).toContainText('No background jobs');
      await expect(page.getByTestId('jobs-queue-bar')).toHaveCount(0);
      const menu = await page.getByTestId('jobs-view').getByTestId('sidebar-open').boundingBox();
      expect(menu!.height).toBeGreaterThanOrEqual(44);

      const chips = page.getByTestId('jobs-state-filters');
      const ys = await chips
        .locator('button')
        .evaluateAll((els) => els.map((e) => Math.round(e.getBoundingClientRect().top)));
      expect(new Set(ys).size, 'chips wrap onto several rows').toBe(1);
      const h = (await page.getByTestId('jobs-state-filter-failed').boundingBox())!.height;
      expect(h).toBeGreaterThanOrEqual(36);
      await shot(page, `jobs-empty-${name}`);
    });

    test('jobs: a queue with work shows a labeled menu; attempts only after a retry', async ({
      page,
      api,
    }) => {
      api.jobs.push(
        apiJob({ id: 1, kind: 'capture.site', state: 'queued', attempts: 0 }),
        apiJob({ id: 2, kind: 'usage.recompute', state: 'succeeded', attempts: 1 }),
        apiJob({ id: 3, kind: 'import', state: 'failed', errorCode: 'internal', attempts: 2 }),
      );
      await page.goto('/jobs');
      // Only the queue with queued/running work is shown.
      const rows = page.getByTestId('jobs-queue-row');
      await expect(rows).toHaveCount(1);
      await expect(rows).toHaveAttribute('data-kind', 'capture.site');
      await expect(
        page.getByTestId('job-row').filter({ hasText: 'Updating storage usage' }),
      ).toHaveCount(1);
      await expect(page.getByTestId('job-row').filter({ hasText: 'Attempt 2 of' })).toHaveCount(1);
      await expect(page.getByText(/Attempt 1 of/)).toHaveCount(0);

      await rows.getByTestId('jobs-queue-menu').click();
      await expect(page.getByRole('menuitem', { name: 'Pause queue' })).toBeVisible();
      await expect(page.getByRole('menuitem', { name: 'Cancel queued' })).toBeVisible();
      await expect(page.getByRole('menuitem', { name: 'Clear finished' })).toBeVisible();
      await shot(page, `jobs-menu-${name}`);
    });
  });
}
