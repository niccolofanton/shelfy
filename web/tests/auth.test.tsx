import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react';
import type { AuthApi } from '../src/api/auth';
import { ApiError, type Http } from '../src/api/http';
import { readInitialRoute } from '../src/route';
import LoginScreen from '../src/auth/LoginScreen';
import MagicLinkScreen from '../src/auth/MagicLinkScreen';
import Root from '../src/Root';
import type { ShelfyClient } from '@ui/api/ShelfyClient';

// The app itself has its own suite (slice.test.tsx); here it is a marker.
vi.mock('@ui/App', () => ({ default: () => <div data-testid="the-app" /> }));

function fakeAuth(overrides: Partial<AuthApi> = {}): AuthApi {
  return {
    methods: vi.fn().mockResolvedValue({ emailLink: true, passkeys: false }),
    me: vi.fn().mockResolvedValue(null),
    requestLink: vi.fn().mockResolvedValue(undefined),
    redeem: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  };
}

afterEach(() => {
  window.history.replaceState(null, '', '/');
});

describe('initial route', () => {
  it('takes a sign-in token out of the address at once', () => {
    const history = { replaceState: vi.fn() };
    const route = readInitialRoute(
      { pathname: '/login/magic', search: '', hash: '#tok_123' },
      history,
    );
    expect(route).toEqual({ kind: 'magic', token: 'tok_123' });
    expect(history.replaceState).toHaveBeenCalledWith(null, '', '/login/magic');
  });

  it('reads a sign-in page error and anything else as the app', () => {
    const history = { replaceState: vi.fn() };
    expect(
      readInitialRoute({ pathname: '/login', search: '?error=invalid_link', hash: '' }, history),
    ).toEqual({ kind: 'login', error: 'invalid_link' });
    expect(readInitialRoute({ pathname: '/login/magic/', search: '', hash: '' }, history)).toEqual({
      kind: 'magic',
      token: null,
    });
    expect(readInitialRoute({ pathname: '/', search: '', hash: '#x' }, history)).toEqual({
      kind: 'app',
    });
    expect(history.replaceState).not.toHaveBeenCalled();
  });
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
  function fakeHttp(): Http & { expire: () => void } {
    const listeners = new Set<() => void>();
    return {
      get: vi.fn(),
      send: vi.fn(),
      onUnauthorized: (l) => {
        listeners.add(l);
        return () => listeners.delete(l);
      },
      expire: () => listeners.forEach((l) => l()),
    };
  }
  const client = {} as ShelfyClient;

  it('shows the sign-in page without a session', async () => {
    window.history.replaceState(null, '', '/');
    render(<Root route={{ kind: 'app' }} client={client} auth={fakeAuth()} http={fakeHttp()} />);
    await screen.findByTestId('login-form');
    expect(window.location.pathname).toBe('/login');
  });

  it('opens the app with a session and leaves /login', async () => {
    window.history.replaceState(null, '', '/login');
    const auth = fakeAuth({
      me: vi.fn().mockResolvedValue({ id: 'u1', email: 'o@x.test', role: 'owner', createdAt: 0 }),
    });
    render(
      <Root route={{ kind: 'login', error: null }} client={client} auth={auth} http={fakeHttp()} />,
    );
    await screen.findByTestId('the-app');
    expect(window.location.pathname).toBe('/');
  });

  it('goes back to the sign-in page when the session expires', async () => {
    const http = fakeHttp();
    const auth = fakeAuth({
      me: vi.fn().mockResolvedValue({ id: 'u1', email: 'o@x.test', role: 'owner', createdAt: 0 }),
    });
    render(<Root route={{ kind: 'app' }} client={client} auth={auth} http={http} />);
    await screen.findByTestId('the-app');
    act(() => http.expire());
    await screen.findByTestId('login-form');
  });

  it('signs in from a link and then opens the app', async () => {
    const auth = fakeAuth();
    vi.mocked(auth.me).mockResolvedValue({
      id: 'u1',
      email: 'o@x.test',
      role: 'owner',
      createdAt: 0,
    });
    render(
      <Root
        route={{ kind: 'magic', token: 'tok_4' }}
        client={client}
        auth={auth}
        http={fakeHttp()}
      />,
    );
    expect(auth.me).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId('magic-sign-in'));
    await screen.findByTestId('the-app');
    expect(auth.redeem).toHaveBeenCalledWith('tok_4');
    expect(window.location.pathname).toBe('/');
  });
});
