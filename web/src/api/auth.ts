// Signing in and out of the web app, and proving who you are again (plan
// §2.11; E4: owner-only, with sign-in links from the email form or from
// `shelfy-server admin login-link`). The desktop app has no sessions, so these
// calls live outside the ShelfyClient seam; the account's own operations are
// the client's (./account.ts).
import { signWithPasskey } from '../auth/passkeys';
import { isApiError, type Http } from './http';
import type { components } from './schema';

type Schemas = components['schemas'];
export type AuthMethods = Schemas['AuthMethods'];
export type Me = Schemas['Me'];
export type ServerSettings = Schemas['Settings'];

export interface AuthApi {
  // How this server lets users sign in.
  methods(): Promise<AuthMethods>;
  // The signed-in user, or null without a valid session.
  me(): Promise<Me | null>;
  // The signed-in user's settings (the interface language follows them).
  settings(): Promise<ServerSettings>;
  // Asks for a sign-in link by email. The server answers the same whether or
  // not the address has an account.
  requestLink(email: string): Promise<void>;
  // Spends a sign-in link's token: on success the response sets the session
  // cookie. An unusable link rejects with ApiError `invalid_link`.
  redeem(token: string): Promise<void>;
  // Signs in with a passkey of this device, without a username: the browser
  // offers the passkeys it holds for this site. Rejects with a PasskeyError
  // (the browser's side) or an ApiError (`challenge_expired`,
  // `passkey_invalid`, `rate_limited`…).
  signInWithPasskey(): Promise<void>;
  // Ends this browser's session.
  signOut(): Promise<void>;
  // Re-authenticates the session with a passkey of the account.
  reauthWithPasskey(): Promise<void>;
  // Emails a re-authentication link to the account's address.
  requestReauthLink(): Promise<void>;
  // Spends a re-authentication link (`/login/reauth#<token>`) in this
  // browser's session.
  reauthWithLink(token: string): Promise<void>;
  // Approves a device's sign-in code (the migration CLI's): that device gets a
  // `migrate` token for this account. Needs a recent sign-in.
  approveDevice(userCode: string): Promise<void>;
  // Opens the re-authentication dialog without a refused request; true once
  // the user confirmed (Http.reauthenticate).
  confirmIdentity(): Promise<boolean>;
}

async function json<T>(res: Response): Promise<T> {
  return (await res.json()) as T;
}

export function createAuthApi(http: Http): AuthApi {
  return {
    methods: () => http.get<AuthMethods>('/api/v1/auth/methods'),
    async me() {
      try {
        return await http.get<Me>('/api/v1/me');
      } catch (err) {
        if (isApiError(err, 'unauthorized')) return null;
        throw err;
      }
    },
    settings: () => http.get<ServerSettings>('/api/v1/me/settings'),
    async requestLink(email) {
      await http.send('POST', '/api/v1/auth/magic-links', { email });
    },
    async redeem(token) {
      await http.send('POST', '/api/v1/auth/magic-links/redeem', { token });
    },
    async signInWithPasskey() {
      const start = await json<Schemas['PasskeyAssertionStart']>(
        await http.send('POST', '/api/v1/auth/passkeys/login/start'),
      );
      const credential = await signWithPasskey(start.publicKey);
      const body: Schemas['PasskeySignIn'] = { ceremonyId: start.ceremonyId, credential };
      await http.send('POST', '/api/v1/auth/passkeys/login/finish', body);
    },
    async signOut() {
      await http.send('POST', '/api/v1/auth/logout');
    },
    async reauthWithPasskey() {
      const start = await json<Schemas['PasskeyAssertionStart']>(
        await http.send('POST', '/api/v1/auth/reauth/start', { method: 'passkey' }),
      );
      const credential = await signWithPasskey(start.publicKey);
      const body: Schemas['ReauthFinish'] = {
        method: 'passkey',
        ceremonyId: start.ceremonyId,
        credential,
      };
      await http.send('POST', '/api/v1/auth/reauth/finish', body);
    },
    async requestReauthLink() {
      await http.send('POST', '/api/v1/auth/reauth/start', { method: 'email' });
    },
    async reauthWithLink(token) {
      const body: Schemas['ReauthFinish'] = { method: 'link', token };
      await http.send('POST', '/api/v1/auth/reauth/finish', body);
    },
    async approveDevice(userCode) {
      const body: Schemas['DeviceApproval'] = { userCode };
      await http.send('POST', '/api/v1/auth/device/approve', body);
    },
    confirmIdentity: () => http.reauthenticate(),
  };
}
