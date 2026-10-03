import { fileURLToPath } from 'node:url';
import { defineConfig } from '@playwright/test';
const results = fileURLToPath(new URL('../e2e-results/', import.meta.url));
export default defineConfig({
  testDir: '.',
  testMatch: '**/*.spec.ts',
  timeout: 180_000,
  workers: 1,
  fullyParallel: false,
  retries: process.env.CI ? 1 : 0,
  reporter: [
    ['list'],
    ['./metrics-reporter.ts'],
    ['json', { outputFile: `${results}/results.json` }],
    ['html', { outputFolder: `${results}/html`, open: 'never' }],
  ],
  outputDir: `${results}/traces`,
  use: { trace: 'off', screenshot: 'only-on-failure' },
});
