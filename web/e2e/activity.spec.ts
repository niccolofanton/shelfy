import { test, expect, apiJob, sse, HELLO } from './api';
import type { components } from '../src/api/schema';
type Notification = components['schemas']['Notification'];
const notification = (id = 1, overrides: Partial<Notification> = {}): Notification => ({
  id,
  kind: 'job',
  code: 'job.succeeded',
  params: {},
  createdAt: Date.now(),
  readAt: null,
  target: '/jobs',
  ...overrides,
});

test('job progress completes into persisted notifications and marks read across reload', async ({
  page,
  api,
}) => {
  api.jobs.push(apiJob({ id: 7, kind: 'bulk', state: 'running', progress: 0.1 }));
  api.streams.push(HELLO);
  await page.goto('/');
  await page.getByTestId('activity-strip').click();
  const job = page.getByTestId('activity-item-job-7');
  await expect(job).toContainText('10%');
  api.streams.push(
    HELLO +
      sse(
        'job.updated',
        {
          id: 7,
          kind: 'bulk',
          state: 'running',
          progress: 0.7,
          stage: 'working',
          postKey: null,
          errorCode: null,
        },
        'progress-7',
      ),
  );
  await expect(job).toContainText('70%');
  const done = notification(9);
  api.notifications.push(done);
  api.jobs[0].state = 'succeeded';
  api.streams.push(
    HELLO +
      sse(
        'job.updated',
        {
          id: 7,
          kind: 'bulk',
          state: 'succeeded',
          progress: 1,
          stage: null,
          postKey: null,
          errorCode: null,
        },
        'done-7',
      ) +
      sse('notification', done, 'notification-9'),
  );
  await expect(job).toHaveCount(0);
  await expect(page.getByTestId('activity-log-9')).toContainText('Job completed.');
  await page.getByTestId('activity-mark-read-9').click();
  await expect(page.getByTestId('activity-log-9')).toHaveAttribute('data-read', 'true');
  expect(api.notifications[0].readAt).not.toBeNull();
  expect(api.requestsTo('/api/v1/notifications/read', 'POST')[0].body).toEqual({ ids: [9] });
  await page.reload();
  await page.getByTestId('activity-strip').click();
  await expect(page.getByTestId('activity-log-9')).toHaveAttribute('data-read', 'true');
  await expect(page.getByTestId('activity-unread')).toHaveCount(0);
});

test('cancel, retry, queue pause and resume use the existing jobs API', async ({ page, api }) => {
  api.jobs.push(
    apiJob({ id: 1, kind: 'bulk', state: 'running' }),
    apiJob({ id: 2, kind: 'bulk', state: 'failed' }),
  );
  await page.goto('/');
  await page.getByTestId('activity-strip').click();
  await page.getByTestId('activity-queue-bulk').click();
  expect(api.pausedKinds.has('bulk')).toBe(true);
  await expect(page.getByTestId('activity-queue-bulk')).toHaveText('Resume');
  await page.getByTestId('activity-queue-bulk').click();
  expect(api.pausedKinds.has('bulk')).toBe(false);
  await page.getByTestId('activity-action-cancel-1').click();
  await expect(page.getByTestId('activity-item-job-1')).toContainText('Cancelled');
  await page.getByTestId('activity-action-retry-2').click();
  await expect(page.getByTestId('activity-item-job-2')).toContainText('Queued');
});

test('notifications are explicit read actions and navigate safely to storage', async ({
  page,
  api,
}) => {
  api.notifications.push(
    notification(1, {
      kind: 'quota',
      code: 'quota.exceeded',
      params: { usedBytes: 100, quotaBytes: 100 },
      target: '/settings/storage',
    }),
    notification(2, { kind: 'future', code: 'future.unknown', target: 'https://evil.test/' }),
  );
  await page.goto('/');
  await page.getByTestId('activity-strip').click();
  await expect(page.getByTestId('activity-log-1')).toContainText('Storage full');
  await expect(page.getByTestId('activity-unread')).toHaveText('2');
  await expect(page.getByTestId('activity-log-2').getByRole('button').first()).toBeDisabled();
  await page.getByTestId('activity-mark-all-read').click();
  await expect(page.getByTestId('activity-unread')).toHaveCount(0);
  await page.getByTestId('activity-log-1').getByRole('button').click();
  await expect(page).toHaveURL(/\/settings\/storage$/);
  expect(api.thirdParty).toEqual([]);
});

test.describe('mobile', () => {
  test.use({ viewport: { width: 375, height: 812 } });
  test('opens as a sheet, keeps touch targets and closes with Escape', async ({ page, api }) => {
    api.notifications.push(notification());
    await page.goto('/');
    await page.getByTestId('sidebar-open').click();
    await page.getByTestId('activity-strip').click();
    const sheet = page.getByTestId('activity-popover');
    await expect(sheet).toBeVisible();
    await expect(page.getByTestId('sidebar')).not.toBeInViewport();
    await expect(page.getByRole('dialog').filter({ has: sheet })).toBeVisible();
    const button = page.getByTestId('activity-mark-read-1');
    const bounds = await button.boundingBox();
    expect(bounds?.height).toBeGreaterThanOrEqual(44);
    await page.keyboard.press('Escape');
    await expect(sheet).toHaveCount(0);
  });
});
