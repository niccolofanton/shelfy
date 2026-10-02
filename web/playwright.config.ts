// Playwright for the web app. Specs in web/e2e/ run in Chromium against
// `vite preview` of the production build (web/dist), which the web server
// command builds first. Until the P1-05 synthetic libraries exist, the API is
// mocked per test with route handlers (web/e2e/api.ts; P1 lane rule 6).
//
//   pnpm exec playwright test -c web/playwright.config.ts
//
// The desktop suite (playwright.config.ts at the repo root) reads only e2e/,
// so `pnpm test:e2e` never runs these specs. P1-02 adds its viewports here;
// P1-21 moves the suite onto deploy/compose.test.yml and CI.
//
// `serviceWorkers: 'block'` (P2-07): the production build now ships one
// (web/src/sw.ts). On a brand-new origin — every test's fresh context — it
// has no older worker to wait behind, so it activates and `clients.claim()`s
// the page mid-test; fetches it then handles itself (even a pass-through
// `NetworkOnly`) run in the worker's own execution context, which
// `page.route()` does not reach, breaking web/e2e/api.ts's mocking. Blocking
// is the default for every spec here; web/e2e/service-worker.spec.ts turns it
// back on where the worker itself is what is under test.
import { fileURLToPath } from 'node:url';
import { defineConfig, devices } from '@playwright/test';

// P1 lane rule 1: task P1-n uses port 18180 + n.
const port = Number(process.env.SHELFY_WEB_E2E_PORT || 18184);
const origin = `http://127.0.0.1:${port}`;
const repoRoot = fileURLToPath(new URL('..', import.meta.url));

// P1-02's responsive-shell breakpoint (plan §2.17 "under 900px"): the two
// required viewports, shared so every mobile-behavior spec agrees on one
// size instead of inlining the numbers. The single `chromium` project below
// stays at its (≥900px) Desktop Chrome default for the rest of the suite;
// specs opt into these per test/describe-block with `test.use({ viewport })`.
export const MOBILE_VIEWPORT = { width: 375, height: 812 };
export const TABLET_VIEWPORT = { width: 768, height: 1024 };

export default defineConfig({
  testDir: './e2e',
  // The specs on a real server have their own config (e2e/server/).
  testIgnore: ['server/**'],
  timeout: 30_000,
  expect: { timeout: 10_000 },
  fullyParallel: true,
  retries: 0,
  reporter: [['line']],
  use: {
    baseURL: origin,
    trace: 'retain-on-failure',
    serviceWorkers: 'block',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: `pnpm run web:build && pnpm exec vite preview --config web/vite.config.ts --host 127.0.0.1 --port ${port} --strictPort`,
    cwd: repoRoot,
    url: origin,
    // Never test a stale build served by someone else's preview.
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
