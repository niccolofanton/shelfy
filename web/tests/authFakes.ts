// Fakes of the web app's sign-in API and transport, for the auth suites.
import { vi } from 'vitest';
import type { AuthApi, Me } from '../src/api/auth';
import type { Http, ReauthHandler } from '../src/api/http';

export function fakeAuth(overrides: Partial<AuthApi> = {}): AuthApi {
  return {
    methods: vi.fn().mockResolvedValue({ emailLink: true, passkeys: false }),
    me: vi.fn().mockResolvedValue(null),
    settings: vi.fn().mockResolvedValue({
      language: null,
      archiveAssetTypes: { thumbnail: true, image: true, video: true },
    }),
    requestLink: vi.fn().mockResolvedValue(undefined),
    redeem: vi.fn().mockResolvedValue(undefined),
    signInWithPasskey: vi.fn().mockResolvedValue(undefined),
    signOut: vi.fn().mockResolvedValue(undefined),
    reauthWithPasskey: vi.fn().mockResolvedValue(undefined),
    requestReauthLink: vi.fn().mockResolvedValue(undefined),
    reauthWithLink: vi.fn().mockResolvedValue(undefined),
    approveDevice: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  };
}

export type FakeHttp = Http & {
  // Ends the session as a 401 would.
  expire: () => void;
  // Asks the registered re-authentication handler, as a 403 `reauth_required` would.
  requireReauth: (again?: boolean) => Promise<boolean>;
};

export function fakeHttp(): FakeHttp {
  const listeners = new Set<() => void>();
  let reauth: ReauthHandler | null = null;
  return {
    get: vi.fn(),
    send: vi.fn(),
    onUnauthorized: (l) => {
      listeners.add(l);
      return () => {
        listeners.delete(l);
      };
    },
    sessionEnded: () => listeners.forEach((l) => l()),
    onReauthRequired: (handler) => {
      reauth = handler;
      return () => {
        if (reauth === handler) reauth = null;
      };
    },
    expire: () => listeners.forEach((l) => l()),
    requireReauth: (again = false) =>
      reauth ? reauth({ again }) : Promise.reject(new Error('no re-authentication handler')),
  };
}

export const OWNER: Me = {
  id: 'u1',
  email: 'o@x.test',
  role: 'owner',
  createdAt: 0,
  capabilities: {
    admin: true,
    passkeys: true,
    emailLink: true,
    extension: false,
    'ai.tasks': false,
    capture: false,
    'video.onDemand': false,
  },
  consent: {
    disclaimerVersion: null,
    disclaimerAcceptedAt: null,
    privacyVersion: null,
    privacyAcceptedAt: null,
  },
};

// A browser with WebAuthn: just enough of `PublicKeyCredential` and
// `navigator.credentials` for the passkey buttons to show. Returns the undo.
export function withWebAuthn(): () => void {
  const win = window as unknown as { PublicKeyCredential?: unknown };
  const nav = navigator as unknown as { credentials?: unknown };
  const hadPkc = 'PublicKeyCredential' in win;
  const previousPkc = win.PublicKeyCredential;
  const previousCredentials = Object.getOwnPropertyDescriptor(navigator, 'credentials');
  win.PublicKeyCredential = function PublicKeyCredential() {};
  Object.defineProperty(navigator, 'credentials', {
    configurable: true,
    value: { create: vi.fn(), get: vi.fn() },
  });
  return () => {
    if (hadPkc) win.PublicKeyCredential = previousPkc;
    else delete win.PublicKeyCredential;
    if (previousCredentials) Object.defineProperty(navigator, 'credentials', previousCredentials);
    else delete nav.credentials;
  };
}
