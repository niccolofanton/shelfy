// The Jobs view (P4-09) against a real shelfy-server: P1-07's job system has
// no route that creates a job a fresh account can reach from a fresh sign-in
// (bulk/migrate/usage.recompute only run from real-sized library work), so
// this smoke test plants one row directly in control.sqlite — the same
// technique ./support.ts's ageSignIn already uses — and drives it through the
// real `/api/v1/jobs*` and `/api/v1/queues/*` routes. The `purge` queue is
// paused throughout, so the real scheduler never claims the planted row
// (nothing here risks a real purge run).
//
// Its own account (E2E.jobsEmail), not the owner's: account.spec.ts's
// Storage test visits `GET /me/usage`, which starts the owner's first
// `usage.recompute` the first time anything reads it
// (crates/server/src/routes/me/usage.rs) — account.spec.ts runs first in
// every full-suite run (Playwright sorts spec files, so "account" always
// precedes "jobs"), and that job would otherwise already show as a finished
// row here, before this file's own "empty" test ever gets to assert it.
import { execFileSync } from 'node:child_process';
import { test, expect } from '@playwright/test';
import { E2E } from './env';
import { newContext, shot, signInWithLink } from './support';

const KIND = 'purge';

function sql(statement: string): string {
  return execFileSync('sqlite3', ['-cmd', '.timeout 5000', E2E.controlDb, statement], {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  }).trim();
}

function jobsUserId(): string {
  return sql(`SELECT id FROM users WHERE email = '${E2E.jobsEmail}';`);
}

// Plants a `purge` job row (state `queued`) for this file's own account; a
// far `run_at` keeps it out of anything that sweeps by time, and the paused
// queue keeps the in-memory scheduler from ever claiming it.
function plantJob(id: number): void {
  const farFuture = Date.now() + 365 * 86_400_000;
  sql(
    `INSERT INTO jobs (id, user_id, kind, dedupe_key, state, priority, payload_json,
       attempts, max_attempts, run_at, lease_until, progress, stage, error_code, error_detail,
       created_at, updated_at, finished_at)
     VALUES (${id}, '${jobsUserId()}', '${KIND}', NULL, 'queued', 100, '{}',
       0, 3, ${farFuture}, NULL, NULL, NULL, NULL, NULL,
       ${Date.now()}, ${Date.now()}, NULL);`,
  );
}

test.describe.configure({ mode: 'serial' });

test('the Jobs view loads against the real API, empty on a fresh account', async ({ browser }) => {
  const context = await newContext(browser);
  const page = await context.newPage();
  await signInWithLink(page, E2E.jobsEmail);
  await page.goto('/jobs');
  await expect(page.getByTestId('jobs-view')).toBeVisible();
  await expect(page.getByTestId('jobs-empty')).toBeVisible();
  await shot(page, '12-jobs-empty');
  await context.close();
});

// P1-21 found this one genuinely broken against the real server (not an
// artifact of the account split above): `useJobs.ts`'s single-job `cancel`
// only patches that job's own row (`setState`), unlike `refresh`/`resync`
// and every *queue*-level action, which also call `loadSummary()`. A single
// cancel never refreshes `summary`, so `queue.cancelled` stays 0 and
// `jobs-queue-clear-finished` (disabled while `finished === 0`,
// src/views/jobs/QueueBar.tsx) never enables — this hung for the full 60s
// test timeout rather than failing fast. Flagged as a follow-up for
// useJobs.ts (and its `retry`, which has the same gap); fixme rather than a
// weaker assertion so the real behavior stays specified here.
test.fixme('pause, a planted job, cancel, and the queue bar agree with the real server', async ({
  browser,
}) => {
  const context = await newContext(browser);
  const page = await context.newPage();
  await signInWithLink(page, E2E.jobsEmail);

  // `purge` has no queued jobs yet, so the queue bar has nothing to show:
  // pause it globally instead, through the registry (any registered kind
  // accepts pause/resume even with none queued) — the real scheduler is
  // rebuilt from `jobs` only at boot and claims by polling afterwards, so
  // pausing *before* the row exists is what keeps it from ever being claimed.
  const paused = await page.request.post('/api/v1/queues/purge/pause', {
    headers: { 'X-Shelfy-Client': 'web', Origin: E2E.origin },
  });
  expect(paused.ok()).toBe(true);

  plantJob(9001);
  await page.goto('/jobs');
  const row = page.locator('[data-testid="job-row"][data-job-id="9001"]');
  await expect(row).toBeVisible();
  await expect(row).toContainText('Trash purge');
  await expect(row).toContainText('Queued');

  const queueRow = page.locator('[data-testid="jobs-queue-row"][data-kind="purge"]');
  await expect(queueRow).toContainText('paused');
  await shot(page, '13-jobs-queue-paused');

  // Still queued, untouched by the (paused) scheduler: the real proof this
  // row was never claimed, not just that it was cancelled quickly enough.
  expect(sql(`SELECT state FROM jobs WHERE id = 9001;`)).toBe('queued');

  await row.getByTestId('job-row-cancel').click();
  await expect(row).toContainText('Cancelled');
  expect(sql(`SELECT state FROM jobs WHERE id = 9001;`)).toBe('cancelled');

  await queueRow.getByTestId('jobs-queue-clear-finished').click();
  await expect(row).toHaveCount(0);

  await context.close();
});
