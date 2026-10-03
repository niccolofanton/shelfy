// Playwright for the web app against a real shelfy-server (P1-20): sign-in
// with passkeys (a CDP virtual authenticator), re-authentication, device
// approval, sessions, language and consent. The mocked suite
// (../../playwright.config.ts) cannot run these: passkeys need the server's
// relying party.
//
//   cargo build --release -p shelfy-server
//   pnpm exec playwright test -c web/e2e/server/playwright.config.ts
//
// Each run starts its own server on a fresh data directory (./env.ts) and the
// production web build under `vite preview`, which proxies /api to it. The
// tests share the one owner account, in order. P1-21 moves this onto
// deploy/compose.test.yml and CI.
import { existsSync } from 'node:fs';
import { defineConfig, devices } from '@playwright/test';
import { E2E } from './env';

// The servers and the workers inherit this run's data directory.
process.env.SHELFY_E2E_DATA_DIR = E2E.dataDir;

if (!existsSync(E2E.stubBin)) {
  throw new Error(
    'Build the test provider with cargo build --release -p shelfy-ai --features stub --bin shelfy-ai-stub, or set SHELFY_E2E_STUB_BIN',
  );
}

if (!existsSync(E2E.serverBin)) {
  throw new Error(
    `no shelfy-server at ${E2E.serverBin}: run \`cargo build --release -p shelfy-server\`, or set SHELFY_E2E_SERVER_BIN`,
  );
}

export default defineConfig({
  testDir: '.',
  timeout: 60_000,
  expect: { timeout: 10_000 },
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [['line']],
  use: {
    baseURL: E2E.origin,
    // Traces would keep the sign-in links' tokens; screenshots are enough.
    trace: 'off',
    screenshot: 'only-on-failure',
    locale: 'en-US',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: [
    {
      command: 'pnpm exec tsx web/e2e/server/serve.ts',
      cwd: E2E.repoRoot,
      url: `${E2E.apiUrl}/health`,
      reuseExistingServer: false,
      timeout: 60_000,
    },
    {
      command: `pnpm run web:build && pnpm exec vite preview --config web/vite.config.ts --host 127.0.0.1 --port ${E2E.webPort} --strictPort`,
      cwd: E2E.repoRoot,
      url: E2E.origin,
      env: { SHELFY_API_URL: E2E.apiUrl },
      reuseExistingServer: false,
      timeout: 180_000,
    },
  ],
});
