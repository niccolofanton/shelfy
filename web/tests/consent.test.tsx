// The consent gate on the web client (src/components/DisclaimerGate.tsx,
// ConsentGate): the account records the disclaimer and the privacy notice
// v1, once per version; the desktop keeps localStorage.
import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { ConsentGate } from '@ui/components/DisclaimerGate';
import { ShelfyProvider } from '@ui/api/ShelfyProvider';
import { DISCLAIMER_VERSION, PRIVACY_VERSION } from '@ui/disclaimer';
import { ApiError } from '../src/api/http';
import { CURRENT_CONSENT, fakeAccount, webClient, type FakeAccount } from './accountFakes';

function renderGate(account: FakeAccount) {
  render(
    <ShelfyProvider client={webClient(account)}>
      <ConsentGate />
    </ShelfyProvider>,
  );
}

describe('ConsentGate on the web', () => {
  it('asks once: the account records the disclaimer and the privacy notice', async () => {
    const account = fakeAccount();
    renderGate(account);
    expect(screen.queryByTestId('disclaimer-dont-show')).toBeNull();
    fireEvent.click(screen.getByTestId('disclaimer-toggle-privacy'));
    expect(screen.getByTestId('privacy-notice')).toBeInTheDocument();
    expect(screen.getByTestId('disclaimer-accept')).toBeDisabled();
    fireEvent.click(screen.getByTestId('disclaimer-checkbox'));
    fireEvent.click(screen.getByTestId('disclaimer-accept'));
    await waitFor(() => expect(screen.queryByTestId('disclaimer-gate')).toBeNull());
    expect(account.acceptConsent).toHaveBeenCalledWith({
      disclaimer: DISCLAIMER_VERSION,
      privacy: PRIVACY_VERSION,
    });
    expect(localStorage.getItem('app:disclaimerAcceptance')).toBeNull();
  });

  it('stays when the acceptance could not be recorded', async () => {
    const account = fakeAccount({
      acceptConsent: vi.fn().mockRejectedValue(new ApiError(0, 'network')),
    });
    renderGate(account);
    fireEvent.click(screen.getByTestId('disclaimer-checkbox'));
    fireEvent.click(screen.getByTestId('disclaimer-accept'));
    expect(await screen.findByTestId('disclaimer-record-error')).toBeInTheDocument();
    expect(screen.getByTestId('disclaimer-gate')).toBeInTheDocument();
  });

  it('does not show once the current versions are accepted', () => {
    renderGate(fakeAccount({ consent: vi.fn(() => CURRENT_CONSENT) }));
    expect(screen.queryByTestId('disclaimer-gate')).toBeNull();
  });

  it('shows again for a new privacy notice', () => {
    renderGate(
      fakeAccount({ consent: vi.fn(() => ({ ...CURRENT_CONSENT, privacyVersion: '0' })) }),
    );
    expect(screen.getByTestId('disclaimer-gate')).toBeInTheDocument();
  });
});
