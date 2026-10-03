import { defineConfig } from '@playwright/test';
import base from './playwright.config';
export default defineConfig({
  ...base,
  testMatch: 'taxonomy-jobs.spec.ts',
  webServer: (
    base.webServer as NonNullable<typeof base.webServer> & Array<{ command: string }>
  ).map((server, index) =>
    index === 0 ? { ...server, command: 'pnpm exec tsx web/e2e/server/serveTaxonomy.ts' } : server,
  ),
});
