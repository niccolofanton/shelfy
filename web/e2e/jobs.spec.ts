// The Jobs view (P4-09, plan §2.19, §2.12 Controls): lists the user's jobs,
// filters by kind and state, stays live from `job.updated`, and offers the
// per-job and per-queue controls. Replaces Downloads on the web (PG18).
import { test, expect, apiJob, sse, HELLO } from './api';

test.afterEach(({ api }) => {
  expect(api.thirdParty, 'requests outside the app').toEqual([]);
});

test('the sidebar shows Jobs, not Downloads, and opens the view', async ({ page }) => {
  await page.goto('/');
  await expect(page.getByTestId('nav-jobs')).toBeVisible();
  await expect(page.getByTestId('nav-downloads')).toHaveCount(0);
  await page.getByTestId('nav-jobs').click();
  await expect(page).toHaveURL(/\/jobs$/);
  await expect(page.getByTestId('jobs-view')).toBeVisible();
  await expect(page.getByTestId('nav-jobs')).toHaveAttribute('aria-current', 'page');
});

test('lists jobs newest first, with a kind label and a post link', async ({ page, api }) => {
  api.jobs.push(
    apiJob({ id: 1, kind: 'capture.site', state: 'succeeded', postKey: 'ig_1' }),
    apiJob({ id: 2, kind: 'import', state: 'running', progress: 0.5 }),
  );
  await page.goto('/jobs');
  const rows = page.getByTestId('job-row');
  await expect(rows).toHaveCount(2);
  // Newest (highest id) first.
  await expect(rows.nth(0)).toContainText('Import');
  await expect(rows.nth(0)).toContainText('50%');
  await expect(rows.nth(1)).toContainText('Site capture');

  await rows.nth(1).getByTestId('job-row-post-link').click();
  await expect(page).toHaveURL(/\/p\/ig_1$/);
  await expect(page.getByTestId('post-modal')).toBeVisible();
});

test('cancels a running job and retries a failed one', async ({ page, api }) => {
  api.jobs.push(
    apiJob({ id: 1, state: 'running' }),
    apiJob({ id: 2, kind: 'import', state: 'failed', errorCode: 'unavailable', attempts: 1 }),
  );
  await page.goto('/jobs');
  // By id, not by its (about to change) state text: a `hasText` locator would
  // stop matching the moment the row's own text changes under it.
  const job1 = page.locator('[data-testid="job-row"][data-job-id="1"]');
  const job2 = page.locator('[data-testid="job-row"][data-job-id="2"]');
  await expect(job1).toContainText('Running');
  await job1.getByTestId('job-row-cancel').click();
  await expect(job1).toContainText('Cancelled');
  expect(api.jobs.find((j) => j.id === 1)?.state).toBe('cancelled');

  await expect(job2).toContainText('Failed');
  await expect(job2).toContainText('A dependency was unavailable.');
  const retried = page.waitForResponse(
    (res) => res.url().endsWith('/api/v1/jobs/2/retry') && res.request().method() === 'POST',
  );
  await job2.getByTestId('job-row-retry').click();
  const response = await retried;
  expect(response.request().headers()['idempotency-key']).toBeTruthy();
  await expect(job2).toContainText('Queued');
});

test('filters by state through the address, and a deep link pre-filters', async ({ page, api }) => {
  api.jobs.push(
    apiJob({ id: 1, kind: 'capture.site', state: 'running' }),
    apiJob({ id: 2, kind: 'capture.site', state: 'failed', errorCode: 'internal' }),
  );
  await page.goto('/jobs');
  await expect(page.getByTestId('job-row')).toHaveCount(2);
  await page.getByTestId('jobs-state-filter-failed').click();
  await expect(page).toHaveURL(/\/jobs\?state=failed$/);
  await expect(page.getByTestId('job-row')).toHaveCount(1);
  await expect(page.getByTestId('job-row')).toContainText('Failed');

  // Reloading the same address keeps the filter (it is the source of truth).
  await page.reload();
  await expect(page.getByTestId('job-row')).toHaveCount(1);
  await page.getByTestId('jobs-clear-filters').click();
  await expect(page).toHaveURL(/\/jobs$/);
  await expect(page.getByTestId('job-row')).toHaveCount(2);
});

test('the queue bar pauses, cancels and clears finished jobs per kind', async ({ page, api }) => {
  api.jobs.push(
    apiJob({ id: 1, kind: 'capture.site', state: 'queued' }),
    apiJob({ id: 2, kind: 'capture.site', state: 'succeeded' }),
  );
  await page.goto('/jobs');
  const row = page.getByTestId('jobs-queue-row');
  await expect(row).toHaveAttribute('data-kind', 'capture.site');

  await row.getByTestId('jobs-queue-menu').click();
  await page.getByTestId('jobs-queue-pause-toggle').click();
  await expect(row).toContainText('paused');
  expect(api.pausedKinds.has('capture.site')).toBe(true);

  await row.getByTestId('jobs-queue-menu').click();
  await page.getByTestId('jobs-queue-cancel-all').click();
  await expect(page.getByTestId('job-row').filter({ hasText: 'Cancelled' })).toBeVisible();

  await row.getByTestId('jobs-queue-menu').click();
  await page.getByTestId('jobs-queue-clear-finished').click();
  await expect(page.getByTestId('job-row')).toHaveCount(0);
  await expect(page.getByTestId('jobs-empty')).toBeVisible();
});

test('a job.updated event patches a row live, with no extra list request', async ({
  page,
  api,
}) => {
  api.jobs.push(apiJob({ id: 1, kind: 'capture.site', state: 'running', progress: 0.1 }));
  api.streams.push(HELLO);
  await page.goto('/jobs');
  await expect(page.getByTestId('job-row')).toContainText('10%');
  const before = api.requestsTo('/api/v1/jobs').length;

  api.streams.push(
    HELLO +
      sse(
        'job.updated',
        {
          id: 1,
          kind: 'capture.site',
          state: 'succeeded',
          progress: 1,
          stage: null,
          postKey: null,
          errorCode: null,
        },
        'e-1',
      ),
  );
  await expect(page.getByTestId('job-row')).toContainText('Succeeded');
  expect(api.requestsTo('/api/v1/jobs')).toHaveLength(before);
});

test('loads another page by cursor', async ({ page, api }) => {
  // useJobs's own page size (src/hooks/useJobs.ts PAGE_SIZE), not the
  // server's bare default: the client always sends an explicit `limit`.
  for (let id = 1; id <= 65; id++) api.jobs.push(apiJob({ id, kind: 'import' }));
  await page.goto('/jobs');
  await expect(page.getByTestId('job-row')).toHaveCount(50);
  await page.getByTestId('jobs-load-more').click();
  await expect(page.getByTestId('job-row')).toHaveCount(65);
  await expect(page.getByTestId('jobs-load-more')).toHaveCount(0);
});
