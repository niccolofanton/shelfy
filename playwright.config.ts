import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './e2e',
  timeout: 40_000,
  expect: { timeout: 10_000 },
  // Each e2e spec launches a real Electron process whose main.js opens (and runs
  // DDL/migrations against) the SAME <userData>/shelfy.sqlite. The fixture does
  // not isolate userData per worker, so parallel workers would race on the
  // exclusive SQLite write lock during initialize() → SQLITE_BUSY → fatal "The
  // database could not be opened" with no retries (retries: 0) → flaky e2e.
  // Serialize the runs until the fixture isolates userData per worker.
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [['line'], ['html', { open: 'never', outputFolder: 'e2e-report' }]],
  use: {
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
  },
  // Start Vite dev server before tests so Electron can connect to it
  webServer: {
    command: 'npx vite --port 5173',
    port: 5173,
    // Reusing an external Vite instance is the main source of e2e flakiness: a
    // dev server started outside the run can die mid-run, cascading into
    // ERR_CONNECTION_REFUSED across the remaining specs. Always start a fresh,
    // isolated server locally and in CI; only opt into reuse when explicitly on CI
    // tooling that manages its own server.
    reuseExistingServer: !process.env.CI,
    timeout: 30_000,
  },
});
