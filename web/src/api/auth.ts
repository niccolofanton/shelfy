// Signing in and out of the web app (plan §2.11, E4: owner-only; sign-in links
// from the email form or from `shelfy-server admin login-link`). The desktop
// app has no sessions, so these calls live outside the ShelfyClient seam.
import { isApiError, type Http } from './http';
import type { components } from './schema';

export type AuthMethods = components['schemas']['AuthMethods'];
export type Me = components['schemas']['Me'];

export interface AuthApi {
  // How this server lets users sign in.
  methods(): Promise<AuthMethods>;
  // The signed-in user, or null without a valid session.
  me(): Promise<Me | null>;
  // Asks for a sign-in link by email. The server answers the same whether or
  // not the address has an account.
  requestLink(email: string): Promise<void>;
  // Spends a sign-in link's token: on success the response sets the session
  // cookie. An unusable link rejects with ApiError `invalid_link`.
  redeem(token: string): Promise<void>;
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
    async requestLink(email) {
      await http.send('POST', '/api/v1/auth/magic-links', { email });
    },
    async redeem(token) {
      await http.send('POST', '/api/v1/auth/magic-links/redeem', { token });
    },
  };
}
