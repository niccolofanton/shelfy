// F10: on `/device`, Approve stays off while a re-authentication is required
// or in progress, and the approval is sent once the re-authentication
// succeeds. Before, every click on Approve while the session had to
// re-authenticate sent an approval the server refused with 403
// `reauth_required`, and each one spent the sign-in limit (10 a minute per
// client) until the owner got 429. The API is mocked (web/e2e/api.ts); the
// same flow on a real server is in e2e/server/account.spec.ts.
import type { Route } from '@playwright/test';
import { test, expect } from './api';

const CODE = 'BCDF-GHJK';

function reauthRequired(route: Route): Promise<void> {
  return route.fulfill({
    status: 403,
    contentType: 'application/problem+json',
    json: { type: 'about:blank', title: 'Forbidden', status: 403, code: 'reauth_required' },
  });
}

test.afterEach(({ api }) => {
  expect(api.thirdParty, 'requests outside the app').toEqual([]);
});

test('Approve waits for the re-authentication, then approves once', async ({ page }) => {
  // The session's sign-in is old until the re-authentication link is redeemed.
  let recent = false;
  const approvals: number[] = [];
  await page.route('**/api/v1/auth/device/approve', (route) => {
    approvals.push(Date.now());
    return recent ? route.fulfill({ status: 204 }) : reauthRequired(route);
  });

  await page.goto(`/device#${CODE}`);
  await expect(page.getByTestId('device-code')).toHaveValue(CODE);
  await expect(page.getByTestId('device-warning')).toContainText('your own terminal');
  const approve = page.getByTestId('device-approve');

  // The first click finds the sign-in too old: the dialog opens, and Approve
  // is off while it is open.
  await approve.click();
  const dialog = page.getByTestId('reauth-dialog');
  await expect(dialog).toBeVisible();
  await expect(approve).toBeDisabled();
  expect(approvals).toHaveLength(1);

  // Cancelled: Approve stays off, so more clicks send nothing.
  await page.getByTestId('reauth-cancel').click();
  await expect(dialog).toBeHidden();
  await expect(page.getByTestId('device-reauth')).toContainText('confirm it’s you first');
  await expect(approve).toBeDisabled();
  await approve.click({ force: true });
  await approve.click({ force: true });
  expect(approvals).toHaveLength(1);

  // "Confirm it's you" opens the dialog without sending the approval.
  await page.getByTestId('device-confirm').click();
  await expect(dialog).toBeVisible();
  await expect(approve).toBeDisabled();
  await expect(page.getByTestId('device-confirm')).toBeDisabled();
  expect(approvals).toHaveLength(1);

  // The operator's link was opened: the approval goes, once.
  recent = true;
  await page.getByTestId('reauth-continue').click();
  await expect(page.getByTestId('device-approved')).toBeVisible();
  await expect(dialog).toBeHidden();
  expect(approvals).toHaveLength(2);
});

test('a link confirmed in another tab sends the waiting approval', async ({ page, context }) => {
  let recent = false;
  let approvals = 0;
  await page.route('**/api/v1/auth/device/approve', (route) => {
    approvals += 1;
    return recent ? route.fulfill({ status: 204 }) : reauthRequired(route);
  });

  await page.goto(`/device#${CODE}`);
  await page.getByTestId('device-approve').click();
  await page.getByTestId('reauth-cancel').click();
  await expect(page.getByTestId('device-reauth')).toBeVisible();
  expect(approvals).toBe(1);

  // ReauthLinkScreen announces the confirmation on the BroadcastChannel.
  recent = true;
  const tab = await context.newPage();
  await tab.goto('/login/magic');
  await tab.evaluate(() => {
    const channel = new BroadcastChannel('shelfy:auth');
    channel.postMessage({ type: 'reauth' });
    channel.close();
  });
  await expect(page.getByTestId('device-approved')).toBeVisible();
  expect(approvals).toBe(2);
});
