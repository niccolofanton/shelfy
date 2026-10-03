// Confirming who you are (web/src/auth/ReauthDialog.tsx, ReauthLinkScreen.tsx)
// and approving a device's code (web/src/auth/DevicePage.tsx).
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react';
import { ApiError } from '../src/api/http';
import DevicePage, { formatUserCode } from '../src/auth/DevicePage';
import { AUTH_CHANNEL, REAUTH_COMMAND, ReauthDialog, ReauthHost } from '../src/auth/ReauthDialog';
import ReauthLinkScreen from '../src/auth/ReauthLinkScreen';
import { PasskeyError } from '../src/auth/passkeys';
import { fakeAuth, fakeHttp, withWebAuthn } from './authFakes';

let undoWebAuthn: (() => void) | null = null;
afterEach(() => {
  undoWebAuthn?.();
  undoWebAuthn = null;
});

describe('ReauthDialog', () => {
  it('confirms with a passkey', async () => {
    undoWebAuthn = withWebAuthn();
    const auth = fakeAuth();
    const onDone = vi.fn();
    render(
      <ReauthDialog
        auth={auth}
        methods={{ passkeys: true, emailLink: false }}
        again={false}
        onDone={onDone}
      />,
    );
    expect(screen.queryByTestId('reauth-email')).toBeNull();
    expect(screen.getByText(REAUTH_COMMAND)).toBeInTheDocument();
    fireEvent.click(screen.getByTestId('reauth-passkey'));
    await waitFor(() => expect(onDone).toHaveBeenCalledWith(true));
  });

  it('explains a cancelled passkey, and offers links when the account has none', async () => {
    undoWebAuthn = withWebAuthn();
    const auth = fakeAuth({
      reauthWithPasskey: vi
        .fn()
        .mockRejectedValueOnce(new PasskeyError('cancelled'))
        .mockRejectedValueOnce(new ApiError(404, 'not_found')),
    });
    const onDone = vi.fn();
    render(
      <ReauthDialog
        auth={auth}
        methods={{ passkeys: true, emailLink: true }}
        again={false}
        onDone={onDone}
      />,
    );
    fireEvent.click(screen.getByTestId('reauth-passkey'));
    expect(await screen.findByTestId('reauth-error')).toHaveTextContent('Nessuna passkey usata');
    fireEvent.click(screen.getByTestId('reauth-passkey'));
    await waitFor(() =>
      expect(screen.getByTestId('reauth-error')).toHaveTextContent('non ha ancora una passkey'),
    );
    expect(screen.queryByTestId('reauth-passkey')).toBeNull();
    expect(screen.getByTestId('reauth-email')).toBeInTheDocument();
    expect(onDone).not.toHaveBeenCalled();
  });

  it('emails a link, and carries on when another tab confirms it', async () => {
    const auth = fakeAuth();
    const onDone = vi.fn();
    render(
      <ReauthDialog
        auth={auth}
        methods={{ passkeys: false, emailLink: true }}
        again={false}
        onDone={onDone}
      />,
    );
    fireEvent.click(screen.getByTestId('reauth-email'));
    await screen.findByTestId('reauth-email-sent');
    expect(auth.requestReauthLink).toHaveBeenCalledTimes(1);
    const other = new BroadcastChannel(AUTH_CHANNEL);
    other.postMessage({ type: 'reauth' });
    other.close();
    await waitFor(() => expect(onDone).toHaveBeenCalledWith(true));
  });

  it('says when the last confirmation did not take, and cancels on Escape', () => {
    const onDone = vi.fn();
    render(
      <ReauthDialog
        auth={fakeAuth()}
        methods={{ passkeys: false, emailLink: false }}
        again
        onDone={onDone}
      />,
    );
    expect(screen.getByTestId('reauth-again')).toBeInTheDocument();
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(onDone).toHaveBeenCalledWith(false);
  });
});

describe('ReauthHost', () => {
  it('answers requests refused together with one dialog', async () => {
    const http = fakeHttp();
    render(
      <ReauthHost http={http} auth={fakeAuth()} methods={{ passkeys: false, emailLink: false }} />,
    );
    let first!: Promise<boolean>;
    let second!: Promise<boolean>;
    act(() => {
      first = http.requireReauth();
      second = http.requireReauth();
    });
    expect(screen.getAllByTestId('reauth-dialog')).toHaveLength(1);
    fireEvent.click(screen.getByTestId('reauth-cancel'));
    await expect(first).resolves.toBe(false);
    await expect(second).resolves.toBe(false);
    expect(screen.queryByTestId('reauth-dialog')).toBeNull();
  });

  it('cancels an open dialog when it goes away', async () => {
    const http = fakeHttp();
    const { unmount } = render(
      <ReauthHost http={http} auth={fakeAuth()} methods={{ passkeys: false, emailLink: false }} />,
    );
    let answer!: Promise<boolean>;
    act(() => {
      answer = http.requireReauth();
    });
    unmount();
    await expect(answer).resolves.toBe(false);
  });
});

describe('ReauthLinkScreen', () => {
  it('confirms only when asked, and tells the other tabs', async () => {
    const auth = fakeAuth();
    const heard = vi.fn();
    const listener = new BroadcastChannel(AUTH_CHANNEL);
    listener.onmessage = (event) => heard(event.data);
    render(<ReauthLinkScreen token="tok_r" auth={auth} />);
    expect(auth.reauthWithLink).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId('reauth-link-confirm'));
    await screen.findByTestId('reauth-link-done');
    expect(auth.reauthWithLink).toHaveBeenCalledWith('tok_r');
    await waitFor(() => expect(heard).toHaveBeenCalledWith({ type: 'reauth' }));
    listener.close();
  });

  it('explains a used link, and a browser that is not signed in', async () => {
    const auth = fakeAuth({
      reauthWithLink: vi
        .fn()
        .mockRejectedValueOnce(new ApiError(400, 'invalid_link'))
        .mockRejectedValueOnce(new ApiError(401, 'unauthorized')),
    });
    const { unmount } = render(<ReauthLinkScreen token="tok_1" auth={auth} />);
    fireEvent.click(screen.getByTestId('reauth-link-confirm'));
    await screen.findByTestId('reauth-link-invalid');
    unmount();
    render(<ReauthLinkScreen token="tok_2" auth={auth} />);
    fireEvent.click(screen.getByTestId('reauth-link-confirm'));
    await screen.findByTestId('reauth-link-signedOut');
  });

  it('treats a page without a token as a used link', () => {
    render(<ReauthLinkScreen token={null} auth={fakeAuth()} />);
    expect(screen.getByTestId('reauth-link-invalid')).toBeInTheDocument();
  });
});

describe('DevicePage', () => {
  it('formats the code as it is typed', () => {
    expect(formatUserCode('bcdf')).toBe('BCDF');
    expect(formatUserCode('bcdfg')).toBe('BCDF-G');
    expect(formatUserCode(' bc-df gh jk xyz')).toBe('BCDF-GHJK');
    expect(formatUserCode('12 34')).toBe('');
  });

  it('approves a complete code on the click, never before', async () => {
    const auth = fakeAuth();
    render(<DevicePage auth={auth} />);
    const approve = screen.getByTestId('device-approve');
    expect(approve).toBeDisabled();
    fireEvent.change(screen.getByTestId('device-code'), { target: { value: 'bcdfghjk' } });
    expect(auth.approveDevice).not.toHaveBeenCalled();
    fireEvent.click(approve);
    await screen.findByTestId('device-approved');
    expect(auth.approveDevice).toHaveBeenCalledWith('BCDF-GHJK');
  });

  it('explains a bad code and a limit', async () => {
    const auth = fakeAuth({
      approveDevice: vi
        .fn()
        .mockRejectedValueOnce(new ApiError(400, 'invalid_device_code'))
        .mockRejectedValueOnce(new ApiError(429, 'rate_limited')),
    });
    render(<DevicePage auth={auth} initialCode="BCDF-GHJK" />);
    const expected = ['non è valido', 'Troppi tentativi'];
    for (const text of expected) {
      fireEvent.click(screen.getByTestId('device-approve'));
      await waitFor(() => expect(screen.getByTestId('device-error')).toHaveTextContent(text));
    }
    expect(screen.getByTestId('device-code')).toHaveValue('BCDF-GHJK');
  });

  // F10: clicks on Approve while the session has to re-authenticate sent
  // approvals the server refused, and spent the sign-in limit.
  it('keeps Approve off until the re-authentication, then approves once', async () => {
    let confirmed: (ok: boolean) => void = () => {};
    const auth = fakeAuth({
      approveDevice: vi
        .fn()
        .mockRejectedValueOnce(new ApiError(403, 'reauth_required'))
        .mockResolvedValueOnce(undefined),
      confirmIdentity: vi
        .fn()
        .mockResolvedValueOnce(false)
        .mockImplementationOnce(
          () =>
            new Promise<boolean>((resolve) => {
              confirmed = resolve;
            }),
        ),
    });
    render(<DevicePage auth={auth} initialCode="BCDF-GHJK" />);
    const approve = screen.getByTestId('device-approve');
    fireEvent.click(approve);
    fireEvent.click(approve);
    await screen.findByTestId('device-reauth');
    expect(screen.getByTestId('device-reauth')).toHaveTextContent('conferma prima che sei tu');
    expect(approve).toBeDisabled();
    fireEvent.click(approve);
    expect(auth.approveDevice).toHaveBeenCalledTimes(1);

    // Cancelled: still off, and nothing sent.
    fireEvent.click(screen.getByTestId('device-confirm'));
    await waitFor(() => expect(auth.confirmIdentity).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(screen.getByTestId('device-confirm')).toBeEnabled());
    expect(approve).toBeDisabled();
    expect(auth.approveDevice).toHaveBeenCalledTimes(1);

    // In progress: both buttons off; confirmed: the approval goes once.
    fireEvent.click(screen.getByTestId('device-confirm'));
    await waitFor(() => expect(screen.getByTestId('device-confirm')).toBeDisabled());
    fireEvent.click(screen.getByTestId('device-confirm'));
    expect(approve).toBeDisabled();
    expect(auth.confirmIdentity).toHaveBeenCalledTimes(2);
    await act(async () => confirmed(true));
    await screen.findByTestId('device-approved');
    expect(auth.approveDevice).toHaveBeenCalledTimes(2);
    expect(auth.approveDevice).toHaveBeenLastCalledWith('BCDF-GHJK');
  });

  it('approves once a link confirms in another tab', async () => {
    const auth = fakeAuth({
      approveDevice: vi
        .fn()
        .mockRejectedValueOnce(new ApiError(403, 'reauth_required'))
        .mockResolvedValueOnce(undefined),
    });
    render(<DevicePage auth={auth} initialCode="BCDF-GHJK" />);
    fireEvent.click(screen.getByTestId('device-approve'));
    await screen.findByTestId('device-reauth');
    const tab = new BroadcastChannel(AUTH_CHANNEL);
    tab.postMessage({ type: 'reauth' });
    tab.close();
    await screen.findByTestId('device-approved');
    expect(auth.approveDevice).toHaveBeenCalledTimes(2);
    expect(auth.confirmIdentity).not.toHaveBeenCalled();
  });
});
