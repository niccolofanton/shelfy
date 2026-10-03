// Serve the local capture fixtures (scripts/web-capture-eval/fixtures) to a
// Playwright browser via its request router, exactly as SPIKE-11 did: fixture
// URLs under https://fixtures.shelfy.test/ are fulfilled before any DNS lookup
// or proxy connection, so a capture costs no network and needs no allow rule.

import fs from 'fs';
import path from 'path';
import type { Browser, Route } from 'playwright-core';

export const FIXTURE_ORIGIN = 'https://fixtures.shelfy.test';

async function fulfillFixture(route: Route, dir: string): Promise<void> {
  const name = path.basename(new URL(route.request().url()).pathname);
  const file = path.join(dir, name);
  if (!/^[\w.-]+\.html$/.test(name) || !fs.existsSync(file)) {
    await route.fulfill({ status: 404, contentType: 'text/plain', body: 'not found' });
    return;
  }
  await route.fulfill({
    status: 200,
    contentType: 'text/html; charset=utf-8',
    body: fs.readFileSync(file),
  });
}

// Wrap browser.newContext so every context fulfils FIXTURE_ORIGIN/** from `dir`.
// capture v2 registers its own catch-all route AFTER this one (in
// configureContext), and Playwright runs the last-added route first — but for
// fixture URLs the catch-all falls through to this handler.
export function serveFixturesOnBrowser(browser: Browser, dir: string): void {
  const newContext = browser.newContext.bind(browser);
  browser.newContext = async (...a: Parameters<Browser['newContext']>) => {
    const context = await newContext(...a);
    await context.route(`${FIXTURE_ORIGIN}/**`, (route) => fulfillFixture(route, dir));
    return context;
  };
}
