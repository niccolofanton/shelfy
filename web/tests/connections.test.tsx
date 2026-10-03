// Settings → Connections (P2-12) against a fake account and a fake extension.
import { describe, it, expect, beforeEach, afterEach, type Mock } from 'vitest';
import React from 'react';
import { render, screen, fireEvent, waitFor, within, act } from '@testing-library/react';
import Settings from '@ui/views/Settings';
import { ShelfyProvider } from '@ui/api/ShelfyProvider';
import { I18nProvider } from '@ui/i18n';
import type { ElectronAPI } from '../../types/electron-api';
import { WebNavigation } from '../src/routes';
import { fakeAccount, webClient, type FakeAccount } from './accountFakes';

const ext = (account: FakeAccount) => account.extension as unknown as { probe: Mock; pair: Mock };

function renderConnections(account = fakeAccount()) {
  window.history.replaceState(null, '', '/settings/connections');
  localStorage.setItem('app:language', 'en');
  render(
    <I18nProvider>
      <ShelfyProvider client={webClient(account)}>
        <WebNavigation>
          <Settings />
        </WebNavigation>
      </ShelfyProvider>
    </I18nProvider>,
  );
  return account;
}

const READY = { state: 'ready', version: '0.1.0', paired: false, outdated: false };

let bridge: ElectronAPI;
beforeEach(() => {
  bridge = window.electronAPI;
  delete (window as Partial<Window>).electronAPI;
});
afterEach(() => {
  window.electronAPI = bridge;
  window.history.replaceState(null, '', '/');
  localStorage.setItem('app:language', 'it');
});

describe('Connections', () => {
  it('shows install steps when no extension answers', async () => {
    renderConnections();
    const missing = await screen.findByTestId('ext-missing');
    expect(within(missing).getAllByRole('listitem')).toHaveLength(4);
  });

  it('says so in a browser that cannot run it', async () => {
    const account = fakeAccount();
    ext(account).probe.mockResolvedValue({ state: 'unsupported' });
    renderConnections(account);
    expect(await screen.findByTestId('ext-unsupported')).toBeTruthy();
  });

  it('pairs: code, then the extension, then a confirmation', async () => {
    const account = fakeAccount();
    ext(account).probe.mockResolvedValue(READY);
    renderConnections(account);
    fireEvent.click(await screen.findByTestId('ext-pair'));
    expect(await screen.findByTestId('ext-paired')).toBeTruthy();
    expect(account.createPairingCode).toHaveBeenCalledTimes(1);
    expect(ext(account).pair).toHaveBeenCalledWith('c'.repeat(43));
  });

  it('names the extension’s failure', async () => {
    const account = fakeAccount();
    ext(account).probe.mockResolvedValue(READY);
    ext(account).pair.mockResolvedValue({ ok: false, code: 'invalid_pairing_code' });
    renderConnections(account);
    fireEvent.click(await screen.findByTestId('ext-pair'));
    expect((await screen.findByTestId('ext-error')).textContent).toMatch(/invalid, has expired/);
  });

  it('follows extension.status once paired', async () => {
    const account = fakeAccount();
    ext(account).probe.mockResolvedValue({ ...READY, paired: true });
    renderConnections(account);
    const state = await screen.findByTestId('ext-state');
    await waitFor(() => expect(state.dataset.connected).toBe('false'));
    act(() => account.emitExtensionStatus({ connected: true, lastSeenAt: 5, version: '0.1.0' }));
    await waitFor(() => expect(state.dataset.connected).toBe('true'));
  });

  it('flags an outdated extension', async () => {
    const account = fakeAccount();
    ext(account).probe.mockResolvedValue({ ...READY, paired: true, outdated: true });
    renderConnections(account);
    expect(await screen.findByTestId('ext-outdated')).toBeTruthy();
  });

  it('lists and revokes paired browsers', async () => {
    const account = fakeAccount();
    account.listTokens.mockResolvedValue([
      {
        id: 'e1',
        kind: 'extension',
        label: 'Chrome on Mac',
        scopes: [],
        createdAt: 1,
        lastUsedAt: null,
        expiresAt: null,
      },
      {
        id: 's1',
        kind: 'shortcut',
        label: 'Phone',
        scopes: [],
        createdAt: 1,
        lastUsedAt: 2,
        expiresAt: null,
      },
    ]);
    renderConnections(account);
    const rows = await within(await screen.findByTestId('ext-tokens')).findAllByTestId(
      'ext-tokens-row',
    );
    expect(rows).toHaveLength(1);
    fireEvent.click(within(rows[0]).getByTestId('ext-tokens-revoke'));
    fireEvent.click(within(rows[0]).getByRole('button', { name: /confirm/i }));
    await waitFor(() => expect(account.revokeToken).toHaveBeenCalledWith('e1'));
  });

  it('shows a Shortcut token once, with the request to build', async () => {
    const account = renderConnections();
    fireEvent.click(await screen.findByTestId('sc-new'));
    fireEvent.change(screen.getByTestId('sc-label'), { target: { value: 'Phone' } });
    fireEvent.click(screen.getByTestId('sc-submit'));
    const value = (await screen.findByTestId('sc-value')) as HTMLInputElement;
    expect(value.value).toBe('shx_shown_once');
    expect(account.createToken).toHaveBeenCalledWith('shortcut', 'Phone');
    expect(screen.getByTestId('sc-steps').textContent).toContain('/api/v1/links');
    fireEvent.click(screen.getByTestId('sc-done'));
    expect(screen.queryByTestId('sc-value')).toBeNull();
  });

  it('offers a bookmarklet to /share', async () => {
    renderConnections();
    const field = (await screen.findByTestId('bookmarklet')) as HTMLInputElement;
    expect(field.value).toContain('/share?url=');
    expect(field.value.startsWith('javascript:')).toBe(true);
  });
});
