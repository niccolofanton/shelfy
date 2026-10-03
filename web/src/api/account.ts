// The web client's AccountApi (src/api/account.ts) on `/api/v1/me/*`: the
// signed-in account's passkeys, sessions, tokens, storage, settings and
// consent (plan §2.9 Account; P1-13, P1-17).
//
// Routes that need a recent sign-in answer 403 `reauth_required`; Http opens
// the re-authentication dialog and sends them again (./http.ts), so these
// calls just wait.
import type {
  AccountApi,
  ExtensionBridge,
  ExtensionStatus,
  PairingCode,
  AccountPasskey,
  AccountSession,
  AccountSettings,
  AccountToken,
  ConsentRecord,
  PasskeyDraft,
  StorageUsage,
} from '@ui/api/account';
import { createPasskey } from '../auth/passkeys';
import { createExtensionBridge } from '../extension/bridge';
import type { EventStream } from './events';
import type { Http } from './http';
import type { components } from './schema';

type Schemas = components['schemas'];

// A ceremony's options are good for 5 minutes on the server; past this age a
// draft asks for new ones before it runs the authenticator.
const DRAFT_MAX_AGE_MS = 4 * 60_000;

// The job kind that counts a user's storage (crates/server/src/jobs/usage.rs).
export const USAGE_JOB_KIND = 'usage.recompute';

async function json<T>(res: Response): Promise<T> {
  return (await res.json()) as T;
}

function toToken(token: Schemas['ApiToken']): AccountToken {
  return { ...token, scopes: [...token.scopes] };
}

function toConsent(consent: Schemas['Consent']): ConsentRecord {
  return { ...consent };
}

export interface AccountApiOptions {
  // The client's realtime stream: `job.updated` says when the storage was
  // counted again.
  events: Pick<EventStream, 'on'>;
  // Clock, for the age of a passkey draft.
  now?: () => number;
  // The page's line to the browser extension; default: the one on
  // `chrome.runtime` (./../extension/bridge.ts).
  extension?: ExtensionBridge;
}

export function createAccountApi(
  http: Http,
  me: Schemas['Me'],
  options: AccountApiOptions,
): AccountApi {
  const now = options.now ?? Date.now;
  let consent = toConsent(me.consent);
  // This browser's session, as the last session list named it.
  let currentSession: string | null = null;

  async function startRegistration(): Promise<{
    start: Schemas['PasskeyRegistrationStart'];
    at: number;
  }> {
    const start = await json<Schemas['PasskeyRegistrationStart']>(
      await http.send('POST', '/api/v1/me/passkeys/start'),
    );
    return { start, at: now() };
  }

  return {
    profile: { id: me.id, email: me.email, role: me.role, createdAt: me.createdAt },
    signIn: {
      passkeys: me.capabilities.passkeys,
      emailLink: me.capabilities.emailLink,
    },

    consent: () => consent,
    async acceptConsent({ disclaimer, privacy }) {
      const body: Schemas['ConsentRequest'] = {
        disclaimerVersion: disclaimer,
        privacyVersion: privacy,
      };
      consent = toConsent(
        await json<Schemas['Consent']>(await http.send('POST', '/api/v1/me/consent', body)),
      );
      return consent;
    },

    getSettings: () => http.get<AccountSettings>('/api/v1/me/settings'),
    async updateSettings(patch) {
      const body: Schemas['SettingsUpdate'] = {};
      if (patch.language) body.language = patch.language;
      if (patch.archiveAssetTypes) body.archiveAssetTypes = { ...patch.archiveAssetTypes };
      return json<AccountSettings>(await http.send('PUT', '/api/v1/me/settings', body));
    },

    async listPasskeys(): Promise<AccountPasskey[]> {
      const { items } = await http.get<Schemas['PasskeyList']>('/api/v1/me/passkeys');
      return items.map((p) => ({ ...p }));
    },
    async preparePasskey(): Promise<PasskeyDraft> {
      let draft: Awaited<ReturnType<typeof startRegistration>> | null = await startRegistration();
      return {
        async create(label) {
          // Options are single-use and expire: an old or spent draft starts
          // over (inside the click, which Safari lets fetch for 10 s).
          const current =
            draft && now() - draft.at < DRAFT_MAX_AGE_MS ? draft : await startRegistration();
          draft = null;
          const credential = await createPasskey(current.start.publicKey);
          const body: Schemas['NewPasskey'] = {
            ceremonyId: current.start.ceremonyId,
            credential,
            ...(label?.trim() ? { label: label.trim() } : {}),
          };
          return {
            ...(await json<Schemas['Passkey']>(
              await http.send('POST', '/api/v1/me/passkeys', body),
            )),
          };
        },
      };
    },
    async removePasskey(id) {
      await http.send('DELETE', `/api/v1/me/passkeys/${encodeURIComponent(String(id))}`);
    },

    async listSessions(): Promise<AccountSession[]> {
      const { items } = await http.get<Schemas['SessionList']>('/api/v1/me/sessions');
      currentSession = items.find((s) => s.current)?.id ?? currentSession;
      return items.map((s) => ({ ...s }));
    },
    async endSession(id) {
      await http.send('DELETE', `/api/v1/me/sessions/${encodeURIComponent(id)}`);
      // Signing this browser's session out clears the cookie: the app signs
      // out too.
      if (id === currentSession) http.sessionEnded();
    },
    async endOtherSessions() {
      await http.send('DELETE', '/api/v1/me/sessions');
    },
    async signOut() {
      try {
        await http.send('POST', '/api/v1/auth/logout');
      } finally {
        http.sessionEnded();
      }
    },

    async listTokens(): Promise<AccountToken[]> {
      const { items } = await http.get<Schemas['ApiTokenList']>('/api/v1/me/tokens');
      return items.map(toToken);
    },
    async createToken(kind, label) {
      const body: Schemas['ApiTokenRequest'] = {
        kind,
        ...(label?.trim() ? { label: label.trim() } : {}),
      };
      const created = await json<Schemas['CreatedApiToken']>(
        await http.send('POST', '/api/v1/me/tokens', body),
      );
      return { token: toToken(created.apiToken), value: created.token };
    },
    async revokeToken(id) {
      await http.send('DELETE', `/api/v1/me/tokens/${encodeURIComponent(id)}`);
    },

    async createPairingCode(): Promise<PairingCode> {
      return json<Schemas['PairingCode']>(
        await http.send('POST', '/api/v1/me/tokens/pairing-code'),
      );
    },
    extensionStatus: () => http.get<ExtensionStatus>('/api/v1/extension/status'),
    onExtensionStatus(listener) {
      return options.events.on('extension.status', (status) => listener({ ...status }));
    },
    extension: options.extension ?? createExtensionBridge(),

    getUsage: () => http.get<StorageUsage>('/api/v1/me/usage'),
    onUsageChanged(listener) {
      return options.events.on('job.updated', (job) => {
        if (job.kind === USAGE_JOB_KIND && job.state === 'succeeded') listener();
      });
    },

    serverVersion: () => http.get<Schemas['VersionInfo']>('/api/v1/version'),
  };
}
