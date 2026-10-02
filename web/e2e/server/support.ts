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

export async function newContext(browser: Browser): Promise<BrowserContext> {
  return browser.newContext({
    baseURL: E2E.origin,
    locale: 'en-US',
    extraHTTPHeaders: { 'CF-Connecting-IP': clientIp() },
  });
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
export function loginLink(purpose: 'login' | 'reauth' = 'login'): string {
  const out = admin('login-link', '--email', E2E.ownerEmail, '--purpose', purpose);
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
export async function signInWithLink(page: Page): Promise<void> {
  await page.goto(loginLink('login'));
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
  if (!E2E.shots) return;
  mkdirSync(E2E.shots, { recursive: true });
  await page.waitForTimeout(700);
  await page.screenshot({ path: join(E2E.shots, `${name}.png`), fullPage: false });
}
