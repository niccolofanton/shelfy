// Helpers of the web e2e suite on a real server: the operator's commands,
// sign-in, a virtual authenticator, and an older sign-in.
//
// Tokens never reach the output: links are used, not printed.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import {
  expect,
  type APIRequestContext,
  type Browser,
  type BrowserContext,
  type CDPSession,
  type Page,
} from '@playwright/test';
import { E2E } from './env';

export const SESSION_COOKIE = '__Host-shelfy_session';

// Each browser context gets its own client address: the server counts the
// sign-in limit (10 a minute) per address, and believes this header from the
// local proxy (SHELFY_TRUSTED_PROXIES).
let clients = 0;
function clientIp(): string {
  clients += 1;
  return `203.0.113.${clients}`;
}

// P1-21: every real-server test gets this for free (no per-test wiring) —
// zero third-party requests and zero CSP violations, asserted when the
// context closes (every test below already ends with `context.close()`, the
// same hook Playwright's own teardown uses on anything left open). The CSP
// violation listener is a `securitypolicyviolation` DOM event bridged out via
// `exposeFunction`, not `page.on('console')`: Chromium logs CSP violations
// through the Log domain, which is not a `console.*()` call and so never
// reaches Playwright's `console` event.
let violationSeq = 0;

async function withViolationTracking(
  context: BrowserContext,
  homeOrigin: string,
): Promise<BrowserContext> {
  const violations: string[] = [];
  const report = `__e2eViolation${(violationSeq += 1)}`;
  await context.exposeFunction(report, (detail: string) => violations.push(detail));
  await context.addInitScript((fnName: string) => {
    document.addEventListener('securitypolicyviolation', (event) => {
      (window as unknown as Record<string, (detail: string) => void>)[fnName](
        `CSP ${event.violatedDirective} blocked ${event.blockedURI}`,
      );
    });
  }, report);
  // Third-party *network* requests (sendBeacon included: it still goes
  // through the browser's network stack and shows up here).
  context.on('request', (request) => {
    const url = request.url();
    if (!url.startsWith(homeOrigin) && !url.startsWith('data:') && !url.startsWith('blob:')) {
      violations.push(`third-party request: ${request.method()} ${url}`);
    }
  });
  const rawClose = context.close.bind(context);
  context.close = (async (options?: Parameters<BrowserContext['close']>[0]) => {
    expect(violations, 'third-party requests or CSP violations').toEqual([]);
    return rawClose(options);
  }) as BrowserContext['close'];
  return context;
}

// `homeOrigin` defaults to the SPA's own origin (E2E.origin, behind vite
// preview); sse-latency.spec.ts passes E2E.apiUrl instead, since it talks to
// shelfy-server directly and never loads the SPA.
export async function newContext(
  browser: Browser,
  homeOrigin = E2E.origin,
): Promise<BrowserContext> {
  const context = await browser.newContext({
    baseURL: homeOrigin,
    locale: 'en-US',
    extraHTTPHeaders: { 'CF-Connecting-IP': clientIp() },
  });
  return withViolationTracking(context, homeOrigin);
}

// Runs `shelfy-server admin …` on the suite's data directory; its stdout.
export function admin(...args: string[]): string {
  return execFileSync(E2E.serverBin, ['admin', ...args], {
    env: { ...process.env, ...E2E.serverEnv },
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  });
}

// A one-time link from the operator (`admin login-link`), as a path of the
// web app: `/login/magic#…` or `/login/reauth#…`.
export function loginLink(purpose: 'login' | 'reauth' = 'login', email = E2E.ownerEmail): string {
  const out = admin('login-link', '--email', email, '--purpose', purpose);
  const url = out
    .split('\n')
    .map((line) => line.trim())
    .find((line) => line.startsWith(E2E.origin));
  if (!url) throw new Error('admin login-link printed no link');
  return url.slice(E2E.origin.length);
}

// Ticks the consent gate's box: the input is visually hidden, its label is not.
export async function tickConsent(page: Page): Promise<void> {
  await page.getByTestId('disclaimer-checkbox').locator('xpath=..').click();
  await expect(page.getByTestId('disclaimer-checkbox')).toBeChecked();
}

// Accepts the consent gate when it shows (only the account's first sign-in).
export async function acceptConsentIfAsked(page: Page): Promise<void> {
  const gate = page.getByTestId('disclaimer-gate');
  const library = page.getByTestId('sidebar');
  await expect(gate.or(library).first()).toBeVisible();
  if (await gate.isVisible()) {
    await tickConsent(page);
    await page.getByTestId('disclaimer-accept').click();
    await expect(gate).toBeHidden();
  }
}

// Adds a passkey from Settings → Account, with the page's authenticator.
export async function addPasskey(page: Page, label: string): Promise<void> {
  await page.goto('/settings/account');
  await page.getByTestId('passkey-add').click();
  await page.getByTestId('passkey-label-input').fill(label);
  await page.getByTestId('passkey-create').click();
  await expect(page.getByTestId('passkey-added')).toBeVisible();
  await expect(page.getByTestId('passkey-label').filter({ hasText: label })).toBeVisible();
}

// Signs the page's context in with an operator's link, through the link page.
export async function signInWithLink(page: Page, email = E2E.ownerEmail): Promise<void> {
  await page.goto(loginLink('login', email));
  await page.getByTestId('magic-sign-in').click();
  await acceptConsentIfAsked(page);
}

// Signs `request` (a context's request API, which shares its cookies) in
// with an operator's link, without loading the app.
export async function redeemLink(request: APIRequestContext): Promise<void> {
  const token = loginLink('login').split('#')[1];
  const res = await request.post('/api/v1/auth/magic-links/redeem', {
    data: { token },
    headers: { 'X-Shelfy-Client': 'web', Origin: E2E.origin },
  });
  expect(res.status()).toBe(204);
}

// Moves the last sign-in of the context's session 10 minutes back, as if the
// user had signed in then: the routes that need a recent sign-in then answer
// `reauth_required`. Only for a session the server has not looked up yet: it
// caches sessions for 60 s.
export async function ageSignIn(context: BrowserContext): Promise<void> {
  const cookie = (await context.cookies(E2E.origin)).find((c) => c.name === SESSION_COOKIE);
  if (!cookie) throw new Error('the context has no session');
  const hash = createHash('sha256').update(cookie.value).digest('hex');
  const changed = execFileSync(
    'sqlite3',
    [
      '-cmd',
      '.timeout 5000',
      E2E.controlDb,
      `UPDATE sessions SET reauth_at = reauth_at - 600000 WHERE id_hash = x'${hash}'; SELECT changes();`,
    ],
    { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] },
  );
  if (changed.trim() !== '1') throw new Error('the session is not in the control database');
}

// A virtual authenticator on the page, as a phone or laptop's platform
// authenticator: discoverable credentials and user verification.
export interface Authenticator {
  cdp: CDPSession;
  id: string;
  credentials(): Promise<{ credentialId: string; isResidentCredential: boolean }[]>;
}

export async function addAuthenticator(page: Page): Promise<Authenticator> {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send('WebAuthn.enable', { enableUI: false });
  const { authenticatorId } = await cdp.send('WebAuthn.addVirtualAuthenticator', {
    options: {
      protocol: 'ctap2',
      ctap2Version: 'ctap2_1',
      transport: 'internal',
      hasResidentKey: true,
      hasUserVerification: true,
      isUserVerified: true,
      automaticPresenceSimulation: true,
    },
  });
  return {
    cdp,
    id: authenticatorId,
    async credentials() {
      const { credentials } = await cdp.send('WebAuthn.getCredentials', { authenticatorId });
      return credentials;
    },
  };
}

// Saves a screenshot for the report when SHELFY_E2E_SHOTS names a directory,
// once the entrance animations (at most 440 ms) are over.
export async function shot(page: Page, name: string): Promise<void> {
  // Keep the same settled screens and navigation pace without screenshot
  // output too; otherwise the full harness bursts past real read limits.
  await page.waitForTimeout(700);
  if (!E2E.shots) return;
  mkdirSync(E2E.shots, { recursive: true });
  await page.screenshot({ path: join(E2E.shots, `${name}.png`), fullPage: false });
}

// ── UX harness (UX-0) ───────────────────────────────────────────────────────
// The four viewports of the audit (docs/web-port/reviews/ux-audit.md, App. A):
// desktop and tablet with a mouse, the two phones with touch. `narrow` is the
// app's own `narrow:` breakpoint (below 900 px).
export interface UxViewport {
  name: 'desktop' | 'tablet' | 'ios' | 'android';
  width: number;
  height: number;
  touch: boolean;
  narrow: boolean;
}

export const UX_VIEWPORTS: readonly UxViewport[] = [
  { name: 'desktop', width: 1440, height: 900, touch: false, narrow: false },
  { name: 'tablet', width: 1024, height: 768, touch: false, narrow: false },
  { name: 'ios', width: 390, height: 844, touch: true, narrow: true },
  { name: 'android', width: 412, height: 915, touch: true, narrow: true },
];

// A context sized and (for the phones) touch-emulated like one of the
// audit's devices, signed in with `storageState` when given.
export async function newViewportContext(
  browser: Browser,
  viewport: UxViewport,
  storageState?: Awaited<ReturnType<BrowserContext['storageState']>>,
): Promise<BrowserContext> {
  const context = await browser.newContext({
    baseURL: E2E.origin,
    locale: 'en-US',
    viewport: { width: viewport.width, height: viewport.height },
    hasTouch: viewport.touch,
    isMobile: viewport.touch,
    deviceScaleFactor: viewport.touch ? 2 : 1,
    storageState,
    extraHTTPHeaders: { 'CF-Connecting-IP': clientIp() },
  });
  return withViolationTracking(context, E2E.origin);
}

// Signs the synth account (a library of real posts) in once and returns the
// cookies to reuse in every viewport's context.
export async function signedInState(
  browser: Browser,
  email = E2E.synthEmail,
): Promise<Awaited<ReturnType<BrowserContext['storageState']>>> {
  const context = await newContext(browser);
  const page = await context.newPage();
  await signInWithLink(page, email);
  const state = await context.storageState();
  await context.close();
  return state;
}

// How far a page's content reaches past the viewport on the right, and the
// widest scroll offsets anywhere: any `scrollLeft` above 0 means the column
// can be dragged sideways (audit §4, "Layout at 390").
export interface OverflowReport {
  pageOverflow: number;
  scrolledLeft: { selector: string; scrollLeft: number }[];
}

export async function overflowReport(page: Page): Promise<OverflowReport> {
  return page.evaluate(() => {
    const doc = document.documentElement;
    const describe = (el: Element): string =>
      el.getAttribute('data-testid')
        ? `[data-testid="${el.getAttribute('data-testid')}"]`
        : `${el.tagName.toLowerCase()}${el.id ? `#${el.id}` : ''}`;
    const scrolledLeft: { selector: string; scrollLeft: number }[] = [];
    for (const el of [doc, document.body, ...document.querySelectorAll('*')]) {
      if (el.scrollLeft > 0)
        scrolledLeft.push({ selector: describe(el), scrollLeft: el.scrollLeft });
    }
    return { pageOverflow: Math.max(0, doc.scrollWidth - doc.clientWidth), scrolledLeft };
  });
}

// A control's interactive box against the viewport. A presentational child
// can name the control that owns its click (for example a selection marker
// inside a card); the caller must also verify that owner's hit area works.
export interface HitReport {
  testId: string;
  width: number;
  height: number;
  inViewport: boolean;
}

export async function hitReport(
  page: Page,
  testId: string,
  hitAreaTestId = testId,
): Promise<HitReport | null> {
  const loc = page.getByTestId(hitAreaTestId).first();
  if ((await loc.count()) === 0) return null;
  const box = await loc.boundingBox();
  if (!box) return null;
  const vp = page.viewportSize()!;
  return {
    testId,
    width: Math.round(box.width * 10) / 10,
    height: Math.round(box.height * 10) / 10,
    inViewport:
      box.x >= 0 && box.y >= 0 && box.x + box.width <= vp.width && box.y + box.height <= vp.height,
  };
}

// WCAG contrast of an element's text against its effective background (the
// nearest ancestors' backgrounds composited over each other), with colors
// normalized by the browser's own canvas so any CSS color syntax works.
export async function contrastOf(page: Page, selector: string): Promise<number | null> {
  return page.evaluate((sel) => {
    const el = document.querySelector(sel);
    if (!el) return null;
    const canvas = document.createElement('canvas');
    canvas.width = canvas.height = 1;
    const ctx = canvas.getContext('2d', { willReadFrequently: true })!;
    const rgba = (css: string): [number, number, number, number] => {
      ctx.clearRect(0, 0, 1, 1);
      ctx.fillStyle = '#000';
      ctx.fillStyle = css;
      ctx.fillRect(0, 0, 1, 1);
      const d = ctx.getImageData(0, 0, 1, 1).data;
      return [d[0], d[1], d[2], d[3] / 255];
    };
    // Background: stack the translucent layers from the root down.
    const layers: [number, number, number, number][] = [];
    for (let n: Element | null = el; n; n = n.parentElement) {
      const bg = rgba(getComputedStyle(n).backgroundColor);
      if (bg[3] > 0) layers.push(bg);
      if (bg[3] === 1) break;
    }
    let base: [number, number, number] = [255, 255, 255];
    if (layers.length && layers[layers.length - 1][3] < 1) base = [255, 255, 255];
    for (const [r, g, b, a] of layers.reverse()) {
      base = [r * a + base[0] * (1 - a), g * a + base[1] * (1 - a), b * a + base[2] * (1 - a)];
    }
    const fg = rgba(getComputedStyle(el).color);
    const text: [number, number, number] = [
      fg[0] * fg[3] + base[0] * (1 - fg[3]),
      fg[1] * fg[3] + base[1] * (1 - fg[3]),
      fg[2] * fg[3] + base[2] * (1 - fg[3]),
    ];
    const lum = ([r, g, b]: [number, number, number]): number => {
      const f = (c: number): number => {
        const s = c / 255;
        return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
      };
      return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
    };
    const [hi, lo] = [lum(text), lum(base)].sort((a, b) => b - a);
    return Math.round(((hi + 0.05) / (lo + 0.05)) * 100) / 100;
  }, selector);
}

// Every visible text element of the page that falls under `min` contrast
// (disabled controls and placeholders are not text a user must read, so they
// are left out), keyed by its test id or its tag and first words.
export async function lowContrastText(
  page: Page,
  min: number,
  within = 'body',
): Promise<Record<string, string>> {
  return page.evaluate(
    ([root, threshold]) => {
      const canvas = document.createElement('canvas');
      canvas.width = canvas.height = 1;
      const ctx = canvas.getContext('2d', { willReadFrequently: true })!;
      const rgba = (css: string): [number, number, number, number] => {
        ctx.clearRect(0, 0, 1, 1);
        ctx.fillStyle = '#000';
        ctx.fillStyle = css;
        ctx.fillRect(0, 0, 1, 1);
        const d = ctx.getImageData(0, 0, 1, 1).data;
        return [d[0], d[1], d[2], d[3] / 255];
      };
      const lum = (c: number[]): number => {
        const f = (v: number): number => {
          const s = v / 255;
          return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
        };
        return 0.2126 * f(c[0]) + 0.7152 * f(c[1]) + 0.0722 * f(c[2]);
      };
      const found: Record<string, string> = {};
      const rootEl = document.querySelector(root);
      if (!rootEl) return found;
      const walker = document.createTreeWalker(rootEl, NodeFilter.SHOW_TEXT);
      for (let node = walker.nextNode(); node; node = walker.nextNode()) {
        const text = (node.textContent ?? '').trim();
        const el = node.parentElement;
        if (!text || !el || !/\p{L}/u.test(text)) continue;
        if (el.closest('script,style,[disabled],[aria-disabled="true"],[aria-hidden="true"]'))
          continue;
        const rect = el.getBoundingClientRect();
        const cs = getComputedStyle(el);
        if (rect.width === 0 || rect.height === 0 || cs.visibility === 'hidden') continue;
        if (rect.bottom < 0 || rect.top > innerHeight || rect.right < 0 || rect.left > innerWidth)
          continue;
        let opacity = 1;
        for (let n: Element | null = el; n; n = n.parentElement)
          opacity *= parseFloat(getComputedStyle(n).opacity);
        if (opacity === 0) continue;
        const layers: number[][] = [];
        for (let n: Element | null = el; n; n = n.parentElement) {
          const bg = rgba(getComputedStyle(n).backgroundColor);
          if (bg[3] > 0) layers.push(bg);
          if (bg[3] === 1) break;
        }
        let base = [255, 255, 255];
        for (const [r, g, b, a] of layers.reverse()) {
          base = [r * a + base[0] * (1 - a), g * a + base[1] * (1 - a), b * a + base[2] * (1 - a)];
        }
        const fg = rgba(cs.color);
        const a = fg[3] * opacity;
        const t = [0, 1, 2].map((i) => fg[i] * a + base[i] * (1 - a));
        const [hi, lo] = [lum(t), lum(base)].sort((x, y) => y - x);
        const ratio = Math.round(((hi + 0.05) / (lo + 0.05)) * 100) / 100;
        if (ratio >= threshold) continue;
        const id = el.closest('[data-testid]')?.getAttribute('data-testid');
        // Keyed without the text, which can be data; the lowest ratio wins.
        const key = `${id ? `[${id}] ` : ''}${el.tagName.toLowerCase()}`;
        const prior = found[key] ? parseFloat(found[key]) : Infinity;
        if (ratio < prior) found[key] = `${ratio}:1 (${text.slice(0, 24)})`;
      }
      return found;
    },
    [within, min] as const,
  );
}

// Presses Tab up to `max` times; the first focused element that matches
// `:focus-visible` is described with the outline or ring it paints, or null
// when nothing took a visible ring (audit §3.3: "no focus visible").
export interface FocusRing {
  target: string;
  outlineWidth: number;
  boxShadow: string;
}

export async function tabToFocusRing(page: Page, max = 8): Promise<FocusRing | null> {
  for (let i = 0; i < max; i += 1) {
    await page.keyboard.press('Tab');
    const ring = await page.evaluate(() => {
      const el = document.activeElement;
      if (!el || el === document.body || !el.matches(':focus-visible')) return null;
      const cs = getComputedStyle(el);
      const outlineWidth = cs.outlineStyle === 'none' ? 0 : parseFloat(cs.outlineWidth) || 0;
      return {
        target: el.getAttribute('data-testid') ?? `${el.tagName.toLowerCase()}`,
        outlineWidth,
        boxShadow: cs.boxShadow === 'none' ? '' : cs.boxShadow,
      };
    });
    if (ring && (ring.outlineWidth > 0 || ring.boxShadow)) return ring;
  }
  return null;
}

// The element on top at a point of the page, as a test id walked up from it.
export async function topmostTestIds(page: Page, x: number, y: number): Promise<string[]> {
  return page.evaluate(
    ([px, py]) => {
      const ids: string[] = [];
      for (let n = document.elementFromPoint(px, py); n; n = n.parentElement) {
        const id = n.getAttribute('data-testid');
        if (id) ids.push(id);
      }
      return ids;
    },
    [x, y] as const,
  );
}
