import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen, fireEvent, waitFor, act, cleanup } from '@testing-library/react';
import { I18nProvider } from '@ui/i18n';
import type { ShelfyClient } from '@ui/api/ShelfyClient';
import { ApiError } from '../src/api/http';
import LoginScreen from '../src/auth/LoginScreen';
import MagicLinkScreen from '../src/auth/MagicLinkScreen';
import { PasskeyError } from '../src/auth/passkeys';
import Root from '../src/Root';
import { fakeAuth, fakeHttp, OWNER, withWebAuthn } from './authFakes';

// The app itself has its own suite (slice.test.tsx); here it is a marker.
vi.mock('@ui/App', () => ({ default: () => <div data-testid="the-app" /> }));

let undoWebAuthn: (() => void) | null = null;

afterEach(() => {
  undoWebAuthn?.();
  undoWebAuthn = null;
  window.history.replaceState(null, '', '/');
});

describe('LoginScreen', () => {
  it('emails a sign-in link when email sign-in is on', async () => {
    const auth = fakeAuth();
    render(<LoginScreen auth={auth} />);
    const input = await screen.findByPlaceholderText('tu@esempio.com');
    fireEvent.change(input, { target: { value: ' owner@example.test ' } });
    fireEvent.submit(screen.getByTestId('login-form'));
    await screen.findByTestId('login-link-sent');
    expect(auth.requestLink).toHaveBeenCalledWith('owner@example.test');
  });

  it('explains a refused address and a rate limit', async () => {
    const auth = fakeAuth({
      requestLink: vi
        .fn()
        .mockRejectedValueOnce(new ApiError(422, 'validation_failed'))
        .mockRejectedValueOnce(new ApiError(429, 'rate_limited')),
    });
    render(<LoginScreen auth={auth} />);
    const input = await screen.findByPlaceholderText('tu@esempio.com');
    fireEvent.change(input, { target: { value: 'nope' } });
    fireEvent.submit(screen.getByTestId('login-form'));
    expect(await screen.findByTestId('login-send-error')).toHaveTextContent(
      'Inserisci un indirizzo email valido.',
    );
    fireEvent.submit(screen.getByTestId('login-form'));
    await waitFor(() =>
      expect(screen.getByTestId('login-send-error')).toHaveTextContent('Troppe richieste'),
    );
  });

  it('sends the user to the operator when email sign-in is off', async () => {
    const auth = fakeAuth({
      methods: vi.fn().mockResolvedValue({ emailLink: false, passkeys: false }),
    });
    render(<LoginScreen auth={auth} />);
    await screen.findByTestId('login-ask-operator');
    expect(screen.queryByTestId('login-form')).toBeNull();
    expect(screen.queryByTestId('login-passkey')).toBeNull();
  });

  it('explains a link that did not work', async () => {
    render(<LoginScreen auth={fakeAuth()} error="invalid_link" />);
    expect(screen.getByTestId('login-invalid-link')).toBeInTheDocument();
    await screen.findByTestId('login-form');
  });

  it('says when the server does not answer', async () => {
    const auth = fakeAuth({ methods: vi.fn().mockRejectedValue(new ApiError(0, 'network')) });
    render(<LoginScreen auth={auth} />);
    expect(await screen.findByTestId('login-methods-error')).toHaveTextContent(
      'Il server non risponde',
    );
  });

  it('signs in with a passkey, without a username', async () => {
    undoWebAuthn = withWebAuthn();
    const auth = fakeAuth({
      methods: vi.fn().mockResolvedValue({ emailLink: false, passkeys: true }),
    });
    const onSignedIn = vi.fn();
    render(<LoginScreen auth={auth} onSignedIn={onSignedIn} />);
    fireEvent.click(await screen.findByTestId('login-passkey'));
    await waitFor(() => expect(onSignedIn).toHaveBeenCalled());
    expect(auth.signInWithPasskey).toHaveBeenCalledTimes(1);
    // Email off: the operator's link stays the other way in.
    expect(screen.getByTestId('login-ask-operator')).toBeInTheDocument();
  });

  it('explains a cancelled passkey and one the server does not know', async () => {
    undoWebAuthn = withWebAuthn();
    const auth = fakeAuth({
      methods: vi.fn().mockResolvedValue({ emailLink: true, passkeys: true }),
      signInWithPasskey: vi
        .fn()
        .mockRejectedValueOnce(new PasskeyError('cancelled'))
        .mockRejectedValueOnce(new ApiError(400, 'passkey_invalid')),
    });
    render(<LoginScreen auth={auth} onSignedIn={vi.fn()} />);
    fireEvent.click(await screen.findByTestId('login-passkey'));
    expect(await screen.findByTestId('login-passkey-error')).toHaveTextContent(
      'Nessuna passkey usata',
    );
    fireEvent.click(screen.getByTestId('login-passkey'));
    await waitFor(() =>
      expect(screen.getByTestId('login-passkey-error')).toHaveTextContent('non è registrata qui'),
    );
    // The email form is still there.
    expect(screen.getByTestId('login-form')).toBeInTheDocument();
  });

  it('offers no passkey where the browser has none', async () => {
    const auth = fakeAuth({
      methods: vi.fn().mockResolvedValue({ emailLink: true, passkeys: true }),
    });
    render(<LoginScreen auth={auth} />);
    await screen.findByTestId('login-form');
    expect(screen.queryByTestId('login-passkey')).toBeNull();
  });
});

describe('MagicLinkScreen', () => {
  it('spends the link only when the user asks', async () => {
    const auth = fakeAuth();
    const onSignedIn = vi.fn();
    render(<MagicLinkScreen token="tok_1" auth={auth} onSignedIn={onSignedIn} />);
    expect(auth.redeem).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId('magic-sign-in'));
    await waitFor(() => expect(onSignedIn).toHaveBeenCalled());
    expect(auth.redeem).toHaveBeenCalledWith('tok_1');
  });

  it('offers the way back to the sign-in page for an unusable link', async () => {
    const auth = fakeAuth({ redeem: vi.fn().mockRejectedValue(new ApiError(400, 'invalid_link')) });
    render(<MagicLinkScreen token="tok_2" auth={auth} onSignedIn={vi.fn()} />);
    fireEvent.click(screen.getByTestId('magic-sign-in'));
    await screen.findByTestId('magic-invalid');
    expect(screen.getByTestId('magic-back')).toHaveAttribute('href', '/login');
  });

  it('treats a link without a token as unusable', () => {
    render(<MagicLinkScreen token={null} auth={fakeAuth()} onSignedIn={vi.fn()} />);
    expect(screen.getByTestId('magic-invalid')).toBeInTheDocument();
  });

  it('keeps the link usable after a transient failure', async () => {
    const auth = fakeAuth({
      redeem: vi.fn().mockRejectedValueOnce(new ApiError(0, 'network')),
    });
    render(<MagicLinkScreen token="tok_3" auth={auth} onSignedIn={vi.fn()} />);
    fireEvent.click(screen.getByTestId('magic-sign-in'));
    expect(await screen.findByTestId('magic-error')).toHaveTextContent('Il server non risponde');
    expect(screen.getByTestId('magic-sign-in')).not.toBeDisabled();
  });
});

describe('Root', () => {
  const client = {} as ShelfyClient;
  const createClient = vi.fn(() => client);
  const at = (path: string) => window.history.replaceState(null, '', path);
  const address = () => window.location.pathname + window.location.search;

  afterEach(() => {
    createClient.mockClear();
  });

  it('shows the sign-in page without a session', async () => {
    at('/');
    render(<Root createClient={createClient} auth={fakeAuth()} http={fakeHttp()} />);
    await screen.findByTestId('login-form');
    expect(address()).toBe('/login');
    expect(createClient).not.toHaveBeenCalled();
  });

  it('opens the app with a session and leaves /login', async () => {
    at('/login');
    const auth = fakeAuth({ me: vi.fn().mockResolvedValue(OWNER) });
    render(<Root createClient={createClient} auth={auth} http={fakeHttp()} />);
    await screen.findByTestId('the-app');
    expect(address()).toBe('/');
  });

  it('makes the client once per session, for the signed-in user', async () => {
    at('/');
    const http = fakeHttp();
    const auth = fakeAuth({ me: vi.fn().mockResolvedValue(OWNER) });
    const { rerender } = render(<Root createClient={createClient} auth={auth} http={http} />);
    await screen.findByTestId('the-app');
    rerender(<Root createClient={createClient} auth={auth} http={http} />);
    expect(createClient).toHaveBeenCalledTimes(1);
    expect(createClient).toHaveBeenCalledWith(OWNER);

    // Signed out, then in again: a new session gets a new client.
    act(() => http.expire());
    await screen.findByTestId('login-form');
    cleanup();
    render(<Root createClient={createClient} auth={auth} http={fakeHttp()} />);
    await screen.findByTestId('the-app');
    expect(createClient).toHaveBeenCalledTimes(2);
  });

  it('takes the language of the account', async () => {
    at('/');
    localStorage.setItem('app:language', 'it');
    const auth = fakeAuth({
      me: vi.fn().mockResolvedValue(OWNER),
      settings: vi.fn().mockResolvedValue({
        language: 'en',
        archiveAssetTypes: { thumbnail: true, image: true, video: true },
      }),
    });
    render(
      <I18nProvider>
        <Root createClient={createClient} auth={auth} http={fakeHttp()} />
      </I18nProvider>,
    );
    await screen.findByTestId('the-app');
    await waitFor(() => expect(document.documentElement.lang).toBe('en'));
    expect(localStorage.getItem('app:language')).toBe('en');
    localStorage.setItem('app:language', 'it');
  });

  it('opens the app even when the settings cannot be read', async () => {
    at('/');
    const auth = fakeAuth({
      me: vi.fn().mockResolvedValue(OWNER),
      settings: vi.fn().mockRejectedValue(new ApiError(503, 'unavailable')),
    });
    render(<Root createClient={createClient} auth={auth} http={fakeHttp()} />);
    await screen.findByTestId('the-app');
  });

  it('opens a deep link in place', async () => {
    at('/c/5');
    const auth = fakeAuth({ me: vi.fn().mockResolvedValue(OWNER) });
    render(<Root createClient={createClient} auth={auth} http={fakeHttp()} />);
    await screen.findByTestId('the-app');
    expect(address()).toBe('/c/5');
  });

  it('remembers a deep link while signed out and opens it once signed in', async () => {
    at('/p/ig_7');
    render(<Root createClient={createClient} auth={fakeAuth()} http={fakeHttp()} />);
    await screen.findByTestId('login-form');
    expect(address()).toBe('/login?next=%2Fp%2Fig_7');
    cleanup();

    // Signed in elsewhere (the link's tab), then this tab reloads.
    const auth = fakeAuth({ me: vi.fn().mockResolvedValue(OWNER) });
    render(<Root createClient={createClient} auth={auth} http={fakeHttp()} />);
    await screen.findByTestId('the-app');
    expect(address()).toBe('/p/ig_7');
  });

  it('goes on to the remembered page after a passkey sign-in', async () => {
    undoWebAuthn = withWebAuthn();
    at('/settings/account');
    const me = vi.fn().mockResolvedValueOnce(null).mockResolvedValue(OWNER);
    const auth = fakeAuth({
      me,
      methods: vi.fn().mockResolvedValue({ emailLink: false, passkeys: true }),
    });
    render(<Root createClient={createClient} auth={auth} http={fakeHttp()} />);
    fireEvent.click(await screen.findByTestId('login-passkey'));
    await screen.findByTestId('the-app');
    expect(auth.signInWithPasskey).toHaveBeenCalled();
    expect(address()).toBe('/settings/account');
  });

  it('ignores a next page outside the app', async () => {
    at('/login?next=%2F%2Fevil.example');
    const auth = fakeAuth({ me: vi.fn().mockResolvedValue(OWNER) });
    render(<Root createClient={createClient} auth={auth} http={fakeHttp()} />);
    await screen.findByTestId('the-app');
    expect(address()).toBe('/');
  });

  it('goes back to the sign-in page when the session expires', async () => {
    at('/c/2');
    const http = fakeHttp();
    const auth = fakeAuth({ me: vi.fn().mockResolvedValue(OWNER) });
    render(<Root createClient={createClient} auth={auth} http={http} />);
    await screen.findByTestId('the-app');
    act(() => http.expire());
    await screen.findByTestId('login-form');
    expect(address()).toBe('/login?next=%2Fc%2F2');
  });

  it('shows the device page to a signed-in user, with the code of the address', async () => {
    at('/device');
    const auth = fakeAuth({ me: vi.fn().mockResolvedValue(OWNER) });
    render(
      <Root
        fragments={{ deviceCode: 'bcdfghjk' }}
        createClient={createClient}
        auth={auth}
        http={fakeHttp()}
      />,
    );
    expect(await screen.findByTestId('device-code')).toHaveValue('BCDF-GHJK');
    expect(screen.getByTestId('device-warning')).toBeInTheDocument();
    expect(screen.queryByTestId('the-app')).toBeNull();
    expect(auth.approveDevice).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId('device-cancel'));
    await screen.findByTestId('the-app');
    expect(address()).toBe('/');
  });

  it('keeps the device code through the sign-in page', async () => {
    undoWebAuthn = withWebAuthn();
    at('/device');
    const auth = fakeAuth({
      me: vi.fn().mockResolvedValueOnce(null).mockResolvedValue(OWNER),
      methods: vi.fn().mockResolvedValue({ emailLink: false, passkeys: true }),
    });
    render(
      <Root
        fragments={{ deviceCode: 'BCDF-GHJK' }}
        createClient={createClient}
        auth={auth}
        http={fakeHttp()}
      />,
    );
    fireEvent.click(await screen.findByTestId('login-passkey'));
    expect(await screen.findByTestId('device-code')).toHaveValue('BCDF-GHJK');
    expect(address()).toBe('/device');
  });

  it('signs in from a link and then opens the app', async () => {
    at('/login/magic');
    const auth = fakeAuth({ me: vi.fn().mockResolvedValue(OWNER) });
    render(
      <Root
        fragments={{ magicToken: 'tok_4' }}
        createClient={createClient}
        auth={auth}
        http={fakeHttp()}
      />,
    );
    expect(auth.me).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId('magic-sign-in'));
    await screen.findByTestId('the-app');
    expect(auth.redeem).toHaveBeenCalledWith('tok_4');
    expect(address()).toBe('/');
  });

  it('takes a link opened over the link page, which only changes the fragment', async () => {
    const auth = fakeAuth({
      redeem: vi.fn().mockRejectedValueOnce(new ApiError(400, 'invalid_link')),
    });
    at('/login/magic');
    render(
      <Root
        fragments={{ magicToken: 'tok_old' }}
        createClient={createClient}
        auth={auth}
        http={fakeHttp()}
      />,
    );
    fireEvent.click(screen.getByTestId('magic-sign-in'));
    await screen.findByTestId('magic-invalid');

    act(() => {
      window.history.replaceState(null, '', '/login/magic#tok_new');
      window.dispatchEvent(new HashChangeEvent('hashchange'));
    });
    expect(window.location.hash).toBe('');
    fireEvent.click(await screen.findByTestId('magic-sign-in'));
    await waitFor(() => expect(auth.redeem).toHaveBeenLastCalledWith('tok_new'));
  });

  it('confirms a re-authentication link without checking the session first', async () => {
    at('/login/reauth');
    const auth = fakeAuth();
    render(
      <Root
        fragments={{ reauthToken: 'tok_r' }}
        createClient={createClient}
        auth={auth}
        http={fakeHttp()}
      />,
    );
    expect(auth.me).not.toHaveBeenCalled();
    expect(auth.reauthWithLink).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId('reauth-link-confirm'));
    await screen.findByTestId('reauth-link-done');
    expect(auth.reauthWithLink).toHaveBeenCalledWith('tok_r');
    expect(address()).toBe('/login/reauth');
  });

  it('opens the re-authentication dialog when a request needs it', async () => {
    undoWebAuthn = withWebAuthn();
    at('/');
    const http = fakeHttp();
    const auth = fakeAuth({ me: vi.fn().mockResolvedValue(OWNER) });
    render(<Root createClient={createClient} auth={auth} http={http} />);
    await screen.findByTestId('the-app');

    let answer!: Promise<boolean>;
    act(() => {
      answer = http.requireReauth();
    });
    fireEvent.click(await screen.findByTestId('reauth-passkey'));
    await expect(answer).resolves.toBe(true);
    expect(auth.reauthWithPasskey).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(screen.queryByTestId('reauth-dialog')).toBeNull());
  });
});
