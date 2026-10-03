// Puppeteer script Lighthouse CI runs, in the same browser it audits with,
// before it loads the Gallery (P1-21; plan §6.2 "Gallery LCP ≤ 2.5 s,
// mobile"): the Gallery needs a signed-in session, so a bare, unauthenticated
// Lighthouse pass would just measure the login page's LCP instead.
//
// `web/e2e/lighthouse/run.ts` mints a one-time `admin login-link` right
// before this runs and passes it in SHELFY_LHCI_LOGIN_URL; this script opens
// it, clicks through, and dismisses the one-time consent gate (a fresh
// account has never accepted it) — otherwise the gate blocks the Gallery
// the real audit navigates to next. CommonJS (`.cjs`, not `.mjs`): LHCI loads
// a `puppeteerScript` with `require()`, which fails on an ESM file in this
// `"type": "module"` package.
const TIMEOUT_MS = 20_000;

async function acceptConsentIfAsked(page) {
  await page.waitForSelector('[data-testid="disclaimer-gate"], [data-testid="sidebar"]', {
    timeout: TIMEOUT_MS,
  });
  const gate = await page.$('[data-testid="disclaimer-gate"]');
  if (!gate) return;
  // The checkbox input is visually hidden; its parent label is the clickable
  // surface (web/e2e/server/support.ts's tickConsent does the same thing
  // through Playwright's `xpath=..`).
  await page.$eval('[data-testid="disclaimer-checkbox"]', (el) => el.parentElement.click());
  await page.waitForFunction(
    () => document.querySelector('[data-testid="disclaimer-accept"]')?.disabled === false,
    { timeout: TIMEOUT_MS },
  );
  await page.click('[data-testid="disclaimer-accept"]');
  await page.waitForSelector('[data-testid="disclaimer-gate"]', {
    hidden: true,
    timeout: TIMEOUT_MS,
  });
}

module.exports = async (browser) => {
  const loginUrl = process.env.SHELFY_LHCI_LOGIN_URL;
  if (!loginUrl) throw new Error('SHELFY_LHCI_LOGIN_URL is not set');

  const page = await browser.newPage();
  try {
    await page.goto(loginUrl, { waitUntil: 'networkidle2', timeout: TIMEOUT_MS });
    await page.waitForSelector('[data-testid="magic-sign-in"]', { timeout: TIMEOUT_MS });
    await page.click('[data-testid="magic-sign-in"]');
    await acceptConsentIfAsked(page);
    await page.waitForSelector('[data-testid="sidebar"]', { timeout: TIMEOUT_MS });
  } finally {
    await page.close();
  }
};
