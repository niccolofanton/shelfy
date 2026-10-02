// The account on a real server (P1-20): consent, passkeys, re-authentication,
// device approval, sessions and language. The tests share the owner account
// and run in order: the first one is the account's first sign-in.
import { test, expect, request as apiRequest } from '@playwright/test';
import { E2E } from './env';
import {
  addAuthenticator,
  addPasskey,
  ageSignIn,
  loginLink,
  newContext,
  redeemLink,
  shot,
  signInWithLink,
  tickConsent,
} from './support';

test.describe.configure({ mode: 'serial' });

test('the consent gate shows once, then never again for the account', async ({ browser }) => {
  const context = await newContext(browser);
  const page = await context.newPage();
  await page.goto(loginLink('login'));
  // The token left the address before anything rendered.
  await expect(page).toHaveURL(`${E2E.origin}/login/magic`);
  await shot(page, '01-sign-in-link');
  await page.getByTestId('magic-sign-in').click();

  const gate = page.getByTestId('disclaimer-gate');
  await expect(gate).toBeVisible();
  await expect(page.getByTestId('disclaimer-dont-show')).toHaveCount(0);
  await page.getByTestId('disclaimer-toggle-privacy').click();
  await expect(page.getByTestId('privacy-notice')).toContainText('Hetzner');
  await shot(page, '02-consent-gate');
  await expect(page.getByTestId('disclaimer-accept')).toBeDisabled();
  await tickConsent(page);
  const recorded = page.waitForResponse(
    (res) => res.url().endsWith('/api/v1/me/consent') && res.request().method() === 'POST',
  );
  await page.getByTestId('disclaimer-accept').click();
  expect((await recorded).status()).toBe(200);
  await expect(gate).toBeHidden();

  await page.reload();
  await expect(page.getByTestId('sidebar')).toBeVisible();
  await expect(gate).toHaveCount(0);

  // Another browser, with nothing stored: the account remembers.
  const other = await newContext(browser);
  const otherPage = await other.newPage();
  await otherPage.goto(loginLink('login'));
  await otherPage.getByTestId('magic-sign-in').click();
  await expect(otherPage.getByTestId('sidebar')).toBeVisible();
  await expect(otherPage.getByTestId('disclaimer-gate')).toHaveCount(0);

  await page.goto('/settings/legal');
  await expect(page.getByTestId('legal-privacy-status')).toContainText('version 1');
  await shot(page, '03-settings-legal');
  await other.close();
  await context.close();
});

test('register a passkey, sign out, and sign in with it', async ({ browser }) => {
  const context = await newContext(browser);
  const page = await context.newPage();
  const authenticator = await addAuthenticator(page);
  await signInWithLink(page);

  await page.goto('/settings/account');
  await expect(page.getByTestId('account-email')).toHaveText(E2E.ownerEmail);
  await expect(page.getByTestId('passkeys-empty')).toBeVisible();
  // Within 5 minutes of the sign-in: no confirmation is asked.
  await addPasskey(page, 'E2E laptop');
  await expect(page.getByTestId('reauth-dialog')).toHaveCount(0);
  const credentials = await authenticator.credentials();
  expect(credentials).toHaveLength(1);
  expect(credentials[0].isResidentCredential).toBe(true);
  await shot(page, '04-settings-account');

  await page.getByTestId('account-sign-out').click();
  await expect(page).toHaveURL(/\/login\?next=%2Fsettings%2Faccount$/);
  await expect(page.getByTestId('login-passkey')).toBeVisible();
  await shot(page, '05-sign-in');

  // Username-less: the authenticator offers the account's passkey.
  await page.getByTestId('login-passkey').click();
  await expect(page.getByTestId('settings-section-account')).toBeVisible();
  await expect(page).toHaveURL(`${E2E.origin}/settings/account`);
  await expect(page.getByTestId('passkey-row').filter({ hasText: 'E2E laptop' })).toContainText(
    'last used',
  );
  await context.close();
});

test('approving a device asks for a passkey when the sign-in is old', async ({ browser }) => {
  const context = await newContext(browser);
  const page = await context.newPage();
  await addAuthenticator(page);
  await signInWithLink(page);
  await addPasskey(page, 'E2E phone');

  // A new session of this browser whose sign-in is 10 minutes old.
  await redeemLink(context.request);
  await ageSignIn(context);

  // The migration CLI asks for a code: no cookie, no CSRF headers.
  const cli = await apiRequest.newContext({
    baseURL: E2E.origin,
    extraHTTPHeaders: { 'CF-Connecting-IP': '198.51.100.7' },
  });
  const start = await (await cli.post('/api/v1/auth/device/start')).json();
  expect(start.verificationUriComplete).toBe(`${E2E.origin}/device#${start.userCode}`);

  await page.goto(`/device#${start.userCode}`);
  await expect(page.getByTestId('device-code')).toHaveValue(start.userCode);
  await expect(page).toHaveURL(`${E2E.origin}/device`);
  await expect(page.getByTestId('device-warning')).toContainText('your own terminal');
  await shot(page, '06-device');

  await page.getByTestId('device-approve').click();
  const dialog = page.getByTestId('reauth-dialog');
  await expect(dialog).toBeVisible();
  await expect(page.getByTestId('reauth-email')).toBeVisible();
  await shot(page, '07-reauth-dialog');
  await page.getByTestId('reauth-passkey').click();
  await expect(page.getByTestId('device-approved')).toBeVisible();
  await expect(dialog).toBeHidden();
  await shot(page, '08-device-approved');

  // The CLI's next poll gets a `migrate` token (never printed).
  const poll = await (
    await cli.post('/api/v1/auth/device/poll', { data: { deviceCode: start.deviceCode } })
  ).json();
  expect(poll.status).toBe('approved');
  expect(poll.scopes).toEqual(['migrate']);
  expect(String(poll.token).startsWith('shx_')).toBe(true);
  await cli.dispose();

  await page.goto('/settings/account');
  await expect(page.getByTestId('token-row').filter({ hasText: 'Migration tool' })).toBeVisible();
  await context.close();
});

test('a session signed out from another browser stops working', async ({ browser }) => {
  const first = await newContext(browser);
  const firstPage = await first.newPage();
  await signInWithLink(firstPage);
  const second = await newContext(browser);
  const secondPage = await second.newPage();
  await signInWithLink(secondPage);

  const sessions = await (await second.request.get('/api/v1/me/sessions')).json();
  const secondId = sessions.items.find((s: { current: boolean }) => s.current).id as string;

  await firstPage.goto('/settings/account');
  const row = firstPage.locator(`[data-session-id="${secondId}"]`);
  await expect(row).toBeVisible();
  await row.getByTestId('session-end').click();
  await row.getByTestId('session-end-confirm').click();
  await expect(row).toHaveCount(0);
  await expect(firstPage.locator('[data-current="true"]')).toHaveCount(1);

  // The other browser's next request is refused: it goes to the sign-in page.
  await secondPage.reload();
  await expect(secondPage).toHaveURL(/\/login$/);
  await expect(secondPage.getByTestId('login-passkey')).toBeVisible();
  await first.close();
  await second.close();
});

test('the language follows the account to another browser', async ({ browser }) => {
  const first = await newContext(browser);
  const firstPage = await first.newPage();
  await signInWithLink(firstPage);
  await firstPage.goto('/settings/language');
  await expect(firstPage.getByTestId('settings-tab-language')).toHaveText('Language');
  const saved = firstPage.waitForResponse(
    (res) => res.url().endsWith('/api/v1/me/settings') && res.request().method() === 'PUT',
  );
  await firstPage.getByTestId('language-select').selectOption('it');
  expect((await saved).status()).toBe(200);
  await expect(firstPage.getByTestId('settings-tab-language')).toHaveText('Lingua');

  // A new browser, English by its locale and with nothing stored.
  const second = await newContext(browser);
  const secondPage = await second.newPage();
  await secondPage.goto(loginLink('login'));
  await expect(secondPage.getByTestId('magic-sign-in')).toHaveText('Sign in');
  await secondPage.getByTestId('magic-sign-in').click();
  await expect(secondPage.getByTestId('sidebar')).toBeVisible();
  await secondPage.goto('/settings/language');
  await expect(secondPage.getByTestId('settings-tab-language')).toHaveText('Lingua');
  await expect(secondPage.getByTestId('language-select')).toHaveValue('it');
  await shot(secondPage, '09-settings-language-it');

  // Back to English for the tests that follow.
  const reset = secondPage.waitForResponse(
    (res) => res.url().endsWith('/api/v1/me/settings') && res.request().method() === 'PUT',
  );
  await secondPage.getByTestId('language-select').selectOption('en');
  expect((await reset).status()).toBe(200);
  await first.close();
  await second.close();
});

test('a re-authentication link confirms the action waiting in another tab', async ({ browser }) => {
  const context = await newContext(browser);
  const page = await context.newPage();
  await redeemLink(context.request);
  await ageSignIn(context);

  await page.goto('/settings/account');
  await page.getByTestId('token-new').click();
  await page.getByTestId('token-label').fill('E2E Shortcut');
  await page.getByTestId('token-create').click();
  const dialog = page.getByTestId('reauth-dialog');
  await expect(dialog).toBeVisible();

  // The operator's link, opened in another tab of this browser.
  const linkTab = await context.newPage();
  await linkTab.goto(loginLink('reauth'));
  await expect(linkTab).toHaveURL(`${E2E.origin}/login/reauth`);
  await shot(linkTab, '10-reauth-link');
  await linkTab.getByTestId('reauth-link-confirm').click();
  await expect(linkTab.getByTestId('reauth-link-done')).toBeVisible();

  // The first tab carries on by itself and shows the new token once.
  await expect(dialog).toBeHidden();
  const value = page.getByTestId('token-value');
  await expect(value).toHaveValue(/^shx_/);
  await page.getByTestId('token-done').click();
  await expect(value).toHaveCount(0);
  await expect(page.getByTestId('token-row').filter({ hasText: 'iOS Shortcut' })).toBeVisible();

  // A spent link stays spent.
  await linkTab.reload();
  await expect(linkTab.getByTestId('reauth-link-invalid')).toBeVisible();
  await context.close();
});

test('Storage shows the space used, counted by the server', async ({ browser }) => {
  const context = await newContext(browser);
  const page = await context.newPage();
  await signInWithLink(page);
  await page.goto('/settings/storage');
  await expect(page.getByTestId('storage-used')).toBeVisible();
  await expect(page.getByTestId('storage-counted')).toContainText('Counted');
  await expect(page.getByTestId('archive-thumbnail')).toBeChecked();
  await shot(page, '11-settings-storage');
  await context.close();
});
