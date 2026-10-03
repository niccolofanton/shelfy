// Settings on the web client (P1-20): one section per address, the account's
// sections only, and each section against a fake account. No preload bridge,
// as in a browser.
import { describe, it, expect, vi, beforeEach, afterEach, type Mock } from 'vitest';
import React from 'react';
import { render, screen, fireEvent, waitFor, within, act } from '@testing-library/react';
import Settings from '@ui/views/Settings';
import { ShelfyProvider } from '@ui/api/ShelfyProvider';
import { DISCLAIMER_VERSION, PRIVACY_VERSION } from '@ui/disclaimer';
import { I18nProvider } from '@ui/i18n';
import type { ElectronAPI } from '../../types/electron-api';
import { ApiError } from '../src/api/http';
import { PasskeyError } from '../src/auth/passkeys';
import { WebNavigation } from '../src/routes';
import { fakeAccount, webClient } from './accountFakes';
import { withWebAuthn } from './authFakes';

function renderSettings(path: string, account = fakeAccount()) {
  window.history.replaceState(null, '', path);
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

// A browser has no preload bridge: any call to it would throw.
let bridge: ElectronAPI;
let undoWebAuthn: (() => void) | null = null;
beforeEach(() => {
  bridge = window.electronAPI;
  delete (window as Partial<Window>).electronAPI;
});
afterEach(() => {
  window.electronAPI = bridge;
  undoWebAuthn?.();
  undoWebAuthn = null;
  window.history.replaceState(null, '', '/');
  localStorage.setItem('app:language', 'it');
});

describe('Settings on the web', () => {
  it('shows one section per address, with the account’s sections only', async () => {
    renderSettings('/settings/account');
    const tabs = screen.getByTestId('settings-tabs');
    expect(
      within(tabs)
        .getAllByRole('tab')
        .map((t) => t.dataset.testid),
    ).toEqual([
      'settings-tab-account',
      'settings-tab-connections',
      'settings-tab-language',
      'settings-tab-storage',
      'settings-tab-legal',
    ]);
    expect(await screen.findByTestId('settings-account')).toBeInTheDocument();
    for (const desktopOnly of ['open-import-btn', 'open-export-btn']) {
      expect(screen.queryByTestId(desktopOnly)).toBeNull();
    }
    fireEvent.click(screen.getByTestId('settings-tab-storage'));
    expect(window.location.pathname).toBe('/settings/storage');
    expect(await screen.findByTestId('settings-storage')).toBeInTheDocument();
    expect(screen.queryByTestId('settings-account')).toBeNull();
  });

  it('sends an unknown section to the first one', async () => {
    renderSettings('/settings/updates');
    await screen.findByTestId('settings-account');
    expect(window.location.pathname).toBe('/settings/account');
  });

  it('keeps its last section while another view has the address', async () => {
    const account = renderSettings('/settings/storage');
    await screen.findByTestId('settings-storage');
    act(() => {
      window.history.pushState(null, '', '/c/3');
      window.dispatchEvent(new PopStateEvent('popstate'));
    });
    // Still the storage section, hidden behind the library: nothing else loads.
    expect(screen.getByTestId('settings-section-storage')).toBeInTheDocument();
    expect(screen.queryByTestId('settings-account')).toBeNull();
    expect(account.listPasskeys).not.toHaveBeenCalled();
    expect(window.location.pathname).toBe('/c/3');
  });

  it('shows the server version and the web build', async () => {
    renderSettings('/settings/legal');
    await waitFor(() =>
      expect(screen.getByTestId('version-pill')).toHaveTextContent('server v0.1.0 · web 1970'),
    );
  });
});

describe('Settings → Account', () => {
  it('shows the email read-only, the passkeys, the sessions and the tokens', async () => {
    renderSettings('/settings/account');
    expect(await screen.findByTestId('account-email')).toHaveTextContent('o@x.test');
    expect(screen.queryByRole('textbox', { name: /email/i })).toBeNull();
    expect(await screen.findByTestId('passkey-label')).toHaveTextContent('MacBook');
    const sessions = await screen.findAllByTestId('session-row');
    expect(sessions[0]).toHaveTextContent('Chrome su macOS');
    expect(sessions[0]).toHaveTextContent('Questo browser');
    expect(sessions[1]).toHaveTextContent('Safari su iOS');
    expect(await screen.findByTestId('token-row')).toHaveTextContent('Strumento di migrazione');
  });

  it('adds a passkey: options first, then the authenticator on the click', async () => {
    undoWebAuthn = withWebAuthn();
    const account = renderSettings('/settings/account');
    fireEvent.click(await screen.findByTestId('passkey-add'));
    const input = await screen.findByTestId('passkey-label-input');
    expect(account.preparePasskey).toHaveBeenCalledTimes(1);
    fireEvent.change(input, { target: { value: 'iPhone' } });
    fireEvent.click(screen.getByTestId('passkey-create'));
    await screen.findByTestId('passkey-added');
    const draft = await (account.preparePasskey.mock.results[0].value as Promise<{ create: Mock }>);
    expect(draft.create).toHaveBeenCalledWith('iPhone');
    expect(account.listPasskeys).toHaveBeenCalledTimes(2);
  });

  it('explains a passkey the device already holds', async () => {
    undoWebAuthn = withWebAuthn();
    const account = fakeAccount({
      preparePasskey: vi.fn(async () => ({
        create: vi.fn(async () => {
          throw new PasskeyError('exists');
        }),
      })),
    });
    renderSettings('/settings/account', account);
    fireEvent.click(await screen.findByTestId('passkey-add'));
    fireEvent.click(await screen.findByTestId('passkey-create'));
    expect(await screen.findByTestId('passkeys-error')).toHaveTextContent(
      'Questo dispositivo ha già una passkey',
    );
  });

  it('says so when the user does not confirm who they are', async () => {
    undoWebAuthn = withWebAuthn();
    const account = fakeAccount({
      preparePasskey: vi.fn().mockRejectedValue(new ApiError(403, 'reauth_required')),
    });
    renderSettings('/settings/account', account);
    fireEvent.click(await screen.findByTestId('passkey-add'));
    expect(await screen.findByTestId('passkeys-error')).toHaveTextContent(
      'Conferma la tua identità per continuare.',
    );
  });

  it('removes a passkey after a confirmation', async () => {
    const account = renderSettings('/settings/account');
    fireEvent.click(await screen.findByTestId('passkey-remove'));
    expect(account.removePasskey).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId('passkey-remove-confirm'));
    await waitFor(() => expect(account.removePasskey).toHaveBeenCalledWith(3));
  });

  it('signs another session out, and all the others', async () => {
    const account = renderSettings('/settings/account');
    await screen.findAllByTestId('session-row');
    fireEvent.click(screen.getByTestId('session-end'));
    fireEvent.click(screen.getByTestId('session-end-confirm'));
    await waitFor(() => expect(account.endSession).toHaveBeenCalledWith('bb'));
    fireEvent.click(screen.getByTestId('sessions-end-others'));
    fireEvent.click(screen.getByTestId('sessions-end-others-confirm'));
    await waitFor(() => expect(account.endOtherSessions).toHaveBeenCalled());
  });

  it('shows a new token once, then forgets it', async () => {
    const account = renderSettings('/settings/account');
    fireEvent.click(await screen.findByTestId('token-new'));
    fireEvent.change(screen.getByTestId('token-label'), { target: { value: 'iPhone' } });
    expect(screen.getByTestId('token-expiry')).toHaveValue('90');
    fireEvent.click(screen.getByTestId('token-create'));
    expect(await screen.findByTestId('token-value')).toHaveValue('shx_shown_once');
    expect(account.createToken).toHaveBeenCalledWith('shortcut', 'iPhone', { ttlDays: 90 });
    fireEvent.click(screen.getByTestId('token-done'));
    expect(screen.queryByTestId('token-value')).toBeNull();
    expect(document.body.textContent).not.toContain('shx_shown_once');
  });

  it('offers bounded token validity and sends the chosen days', async () => {
    const account = renderSettings('/settings/account');
    fireEvent.click(await screen.findByTestId('token-new'));
    const expiry = screen.getByTestId('token-expiry');
    expect(
      within(expiry)
        .getAllByRole('option')
        .map((o) => o.getAttribute('value')),
    ).toEqual(['7', '30', '90', '365']);
    fireEvent.change(expiry, { target: { value: '7' } });
    fireEvent.click(screen.getByTestId('token-create'));
    await waitFor(() =>
      expect(account.createToken).toHaveBeenCalledWith('shortcut', '', { ttlDays: 7 }),
    );
  });
  it('signs out', async () => {
    const account = renderSettings('/settings/account');
    fireEvent.click(await screen.findByTestId('account-sign-out'));
    await waitFor(() => expect(account.signOut).toHaveBeenCalled());
  });
});

describe('Settings → Storage', () => {
  it('shows the use, and reads it again when the server counted it again', async () => {
    const account = renderSettings('/settings/storage');
    expect(await screen.findByTestId('storage-used')).toHaveTextContent('3 MB');
    expect(screen.getByTestId('storage-media')).toHaveTextContent('2 MB');
    expect(screen.getByTestId('storage-db')).toHaveTextContent('1 MB');
    expect(account.getUsage).toHaveBeenCalledTimes(1);
    act(() => account.emitUsage());
    await waitFor(() => expect(account.getUsage).toHaveBeenCalledTimes(2));
  });

  it('says a first count is running', async () => {
    renderSettings(
      '/settings/storage',
      fakeAccount({
        getUsage: vi.fn(async () => ({
          usedBytes: 0,
          mediaBytes: 0,
          dbBytes: 0,
          quotaBytes: 0,
          updatedAt: null,
        })),
      }),
    );
    expect(await screen.findByTestId('storage-counted')).toHaveTextContent('Conteggio in corso');
  });

  it('saves the asset types to archive, all three at once', async () => {
    const account = renderSettings('/settings/storage');
    const video = await screen.findByTestId('archive-video');
    expect(video).not.toBeChecked();
    fireEvent.click(video);
    await waitFor(() =>
      expect(account.updateSettings).toHaveBeenCalledWith({
        archiveAssetTypes: { thumbnail: true, image: true, video: true },
      }),
    );
  });

  it('puts a toggle back when saving fails', async () => {
    const account = fakeAccount({
      updateSettings: vi.fn().mockRejectedValue(new ApiError(0, 'network')),
    });
    renderSettings('/settings/storage', account);
    const image = await screen.findByTestId('archive-image');
    fireEvent.click(image);
    expect(await screen.findByTestId('archive-error')).toHaveTextContent('Il server non risponde');
    expect(screen.getByTestId('archive-image')).toBeChecked();
  });
});

describe('Settings → Language and Legal', () => {
  it('saves the language to the account', async () => {
    const account = renderSettings('/settings/language');
    fireEvent.change(await screen.findByTestId('language-select'), { target: { value: 'en' } });
    await waitFor(() => expect(account.updateSettings).toHaveBeenCalledWith({ language: 'en' }));
    expect(screen.getByTestId('settings-tab-language')).toHaveTextContent('Language');
  });

  it('shows what the account accepted, and the privacy notice', async () => {
    renderSettings(
      '/settings/legal',
      fakeAccount({
        consent: vi.fn(() => ({
          disclaimerVersion: DISCLAIMER_VERSION,
          disclaimerAcceptedAt: Date.UTC(2026, 9, 2),
          privacyVersion: PRIVACY_VERSION,
          privacyAcceptedAt: Date.UTC(2026, 9, 2),
        })),
      }),
    );
    expect(await screen.findByTestId('legal-disclaimer-status')).toHaveTextContent(
      `versione ${DISCLAIMER_VERSION}`,
    );
    expect(screen.getByTestId('legal-privacy-status')).toHaveTextContent(
      `versione ${PRIVACY_VERSION}`,
    );
    fireEvent.click(screen.getByTestId('legal-privacy-read'));
    expect(
      within(screen.getByTestId('privacy-dialog')).getByTestId('privacy-notice'),
    ).toHaveTextContent('Hetzner');
  });
});
