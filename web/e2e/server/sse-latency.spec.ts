// Write → SSE delivered, through a real server (P1-21; plan §6.2 "write →
// SSE delivered p95 ≤ 300 ms" — the same budget P1-01's in-process probe
// checks, listed in IMPLEMENTATION-PLAN.md's budget table as "P1-01 probe,
// P1-21 e2e"). P1-01's probe stops at the server process; this spec goes one
// hop further, into an actual browser's event stream, without P1-23/P1-26's
// live-hostname dependency.
//
// Talks to shelfy-server directly (E2E.apiUrl), bypassing the vite-preview
// origin (support.ts's usual E2E.origin) every other real-server spec loads
// the SPA from: a first measurement through the preview's dev-only `/api`
// proxy (web/vite.config.ts) found every single sample sitting at ~300 ms,
// right at this budget, while a raw curl/fetch straight to shelfy-server
// measured single digits — the preview process is a local dev convenience
// with its own connection-handling cost, and production serves the API and
// the SPA from the same origin with no such hop (plan §2.2, P1-09). Sign-in
// is a direct API call for the same reason: there is no SPA served on
// E2E.apiUrl to click a "sign in" button on.
//
// Measured on the SSE wire a second EventSource opens in the page
// (independent of the app's own ShelfyClient stream), not through the app's
// UI: the UI's own path to a rendered change adds at least one more network
// round trip on top (e.g. `stats.changed` → a fresh `GET /stats`), which
// would time that extra hop instead of "write → SSE delivered". `posts.
// changed` is the signal — every write emits it (P1-03) — read through
// `context.exposeFunction` so both ends of the measurement share Node's
// clock.
//
// Posts and their edits belong to a dedicated, non-owner account (E2E.
// synthEmail, synth-seeded in ./serve.ts): account.spec.ts and jobs.spec.ts
// need their own accounts to stay empty ("a fresh account" — see env.ts's
// comment on `jobsEmail`), so this probe's library lives on a third one.
import { test, expect } from '@playwright/test';
import { E2E } from './env';
import { loginLink, newContext } from './support';

const SAMPLES = 12;
// The server throttles `posts.changed` per user, leading edge, at most once
// per POSTS_WINDOW (crates/server/src/events/mod.rs: 2s) — long enough that
// consecutive samples need a cooldown clearly past it, or a later write
// would be held instead of sent at once, and this probe would time its own
// artifact instead of an isolated write's true latency.
const COOLDOWN_MS = 2_200;
const BUDGET_MS = 300;

test('write -> SSE delivered stays within budget', async ({ browser }) => {
  const context = await newContext(browser, E2E.apiUrl);
  const page = await context.newPage();

  // A direct API sign-in (no SPA on this origin to click through): the same
  // redeem call the magic-link landing page makes.
  const token = loginLink('login', E2E.synthEmail).split('#')[1];
  const redeemed = await context.request.post(`${E2E.apiUrl}/api/v1/auth/magic-links/redeem`, {
    data: { token },
    headers: { 'X-Shelfy-Client': 'web', Origin: E2E.origin },
  });
  expect(redeemed.status(), await redeemed.text()).toBe(204);

  // Lands the page on the API's own origin (same-origin for the EventSource
  // and fetch calls below); `/health` is fast and always there.
  await page.goto(`${E2E.apiUrl}/health`);

  const events: number[] = [];
  await context.exposeFunction('__e2eSseEvent', () => events.push(Date.now()));
  await page.evaluate(() => {
    const source = new EventSource('/api/v1/events');
    source.addEventListener('posts.changed', () => {
      (window as unknown as { __e2eSseEvent(): void }).__e2eSseEvent();
    });
    (window as unknown as { __e2eProbeSource: EventSource }).__e2eProbeSource = source;
  });
  await page.waitForFunction(
    () =>
      (window as unknown as { __e2eProbeSource: EventSource }).__e2eProbeSource.readyState === 1,
  );

  const libraryRes = await page.request.get(`${E2E.apiUrl}/api/v1/posts?limit=1`);
  const library = await libraryRes.json();
  expect(libraryRes.ok(), JSON.stringify(library)).toBe(true);
  const key = (library.items as { key: string }[])[0]?.key;
  expect(key, 'the synth library needs at least one post').toBeTruthy();

  const deltas: number[] = [];
  for (let i = 0; i < SAMPLES; i += 1) {
    events.length = 0;
    const t0 = Date.now();
    const res = await page.request.patch(`${E2E.apiUrl}/api/v1/posts/${key}`, {
      data: { userNote: `sse-latency probe ${i} ${t0}` },
      headers: { 'X-Shelfy-Client': 'web', Origin: E2E.origin },
    });
    expect(res.ok(), await res.text()).toBe(true);
    await expect.poll(() => events.length, { timeout: 2_000 }).toBeGreaterThan(0);
    deltas.push(events[0] - t0);
    await page.waitForTimeout(COOLDOWN_MS);
  }

  deltas.sort((a, b) => a - b);
  const p50 = deltas[Math.floor(deltas.length * 0.5)];
  const p95 = deltas[Math.min(deltas.length - 1, Math.floor(deltas.length * 0.95))];
  // Runtime and flake rate (P1-21 acceptance): the raw samples go to stdout,
  // which CI keeps in the job log, next to the pass/fail budget assertion.
  console.log(
    `[sse-latency] write -> SSE delivered, ${deltas.length} samples: ` +
      `p50=${p50}ms p95=${p95}ms max=${deltas.at(-1)}ms raw=${JSON.stringify(deltas)}`,
  );
  expect(p95, 'write -> SSE delivered p95').toBeLessThanOrEqual(BUDGET_MS);

  await page.evaluate(() =>
    (window as unknown as { __e2eProbeSource: EventSource }).__e2eProbeSource.close(),
  );
  await context.close();
});
