// The signed-in account of a Shelfy server (web port plan §2.11, §2.19
// Settings): who is signed in, the passkeys and sessions that sign them in,
// the API tokens of their devices, their storage and settings, and the legal
// notices they accepted. Only the web client has one (ShelfyClient.account):
// the desktop app has no account, and its client leaves it undefined.
//
// The operations are transport-neutral. The web client (web/src/api/account.ts)
// maps them onto `/api/v1/me/*` and runs the passkey ceremonies in the
// browser. An operation that needs a recent sign-in (adding or removing a
// passkey, creating a token) asks the user to confirm who they are first: the
// web client opens its re-authentication dialog, then carries on. Failures
// reject with the API's problem `code` (src/api/errors.ts), or with a
// passkey's `reason` (PasskeyFailure).

export type AccountRole = 'owner' | 'member';

// A language the account can store (the app's languages, src/i18n).
export type AccountLanguage = 'it' | 'en';

export interface AccountProfile {
  id: string;
  // The sign-in address. The app shows it read-only: the operator manages it
  // (owner decision E4).
  email: string;
  role: AccountRole;
  // Unix ms.
  createdAt: number;
}

// How the user can prove who they are on this server.
export interface SignInMethods {
  // Passkeys work on the server and in this browser.
  passkeys: boolean;
  // "Email me a link" works: the server sends email.
  emailLink: boolean;
}

export interface AccountPasskey {
  id: number;
  // The user's name for it.
  label: string | null;
  createdAt: number;
  // The last sign-in or confirmation with it.
  lastUsedAt: number | null;
}

// A passkey being added: the server's options are ready. `create` runs this
// device's authenticator and saves the new passkey; call it from the user's
// click (Safari runs a passkey ceremony only inside a user gesture).
export interface PasskeyDraft {
  create(label?: string): Promise<AccountPasskey>;
}

// Why a passkey ceremony failed in the browser, as `PasskeyFailure.reason`:
// `cancelled` (the user closed the prompt, or it timed out), `exists` (this
// authenticator already holds a passkey of the account), `unsupported` (no
// passkeys in this browser or on this device), `origin` (the page is not on the
// server's address), `failed` (anything else).
export type PasskeyFailureReason = 'cancelled' | 'exists' | 'unsupported' | 'origin' | 'failed';

export interface PasskeyFailure {
  readonly reason: PasskeyFailureReason;
}

export function isPasskeyFailure(err: unknown): err is PasskeyFailure {
  return (
    !!err &&
    typeof err === 'object' &&
    'reason' in err &&
    typeof (err as { reason: unknown }).reason === 'string' &&
    (err as { name?: unknown }).name === 'PasskeyError'
  );
}

export interface AccountSession {
  // Names the session for `endSession`; it is not the cookie.
  id: string;
  // The session of this browser.
  current: boolean;
  createdAt: number;
  lastSeenAt: number;
  expiresAt: number;
  // The browser's User-Agent at sign-in.
  userAgent: string | null;
}

// Who holds a token: the browser extension, the iOS Shortcut, or the
// migration tool (which gets its token by approving a device code).
export type TokenKind = 'extension' | 'shortcut' | 'migrate';

export interface AccountToken {
  id: string;
  kind: TokenKind;
  label: string | null;
  scopes: string[];
  createdAt: number;
  lastUsedAt: number | null;
  // When it stops working; null when it never expires.
  expiresAt: number | null;
}

export interface NewToken {
  token: AccountToken;
  // The value, shown only now: the server keeps its hash.
  value: string;
}

export interface StorageUsage {
  // Media plus database.
  usedBytes: number;
  mediaBytes: number;
  dbBytes: number;
  // 0: no limit.
  quotaBytes: number;
  // When the use was counted; null until the first count, which has started.
  updatedAt: number | null;
}

// Which assets of a post the server archives.
export interface ArchiveAssetTypes {
  thumbnail: boolean;
  image: boolean;
  video: boolean;
}

export interface AccountSettings {
  // null until the user picks one.
  language: AccountLanguage | null;
  archiveAssetTypes: ArchiveAssetTypes;
}

// The legal notices the user accepted, with when; null until accepted.
export interface ConsentRecord {
  disclaimerVersion: string | null;
  disclaimerAcceptedAt: number | null;
  privacyVersion: string | null;
  privacyAcceptedAt: number | null;
}

export interface ServerVersion {
  version: string;
  apiVersion: string;
}

export interface AccountApi {
  readonly profile: AccountProfile;
  readonly signIn: SignInMethods;

  // What the user accepted, as last known (the session's start, or the last
  // `acceptConsent`).
  consent(): ConsentRecord;
  // Records that the user accepted these versions of the disclaimer and the
  // privacy notice, now.
  acceptConsent(versions: { disclaimer: string; privacy: string }): Promise<ConsentRecord>;

  getSettings(): Promise<AccountSettings>;
  // Changes some settings; resolves to all of them.
  updateSettings(patch: Partial<AccountSettings>): Promise<AccountSettings>;

  // Oldest first.
  listPasskeys(): Promise<AccountPasskey[]>;
  // Starts adding a passkey (after a re-authentication if the last one is
  // older than 5 minutes).
  preparePasskey(): Promise<PasskeyDraft>;
  removePasskey(id: number): Promise<void>;

  // The current session first.
  listSessions(): Promise<AccountSession[]>;
  // Signs a session out; the current one signs this browser out.
  endSession(id: string): Promise<void>;
  endOtherSessions(): Promise<void>;
  // Signs this browser out.
  signOut(): Promise<void>;

  // The working tokens, newest first.
  listTokens(): Promise<AccountToken[]>;
  createToken(kind: Exclude<TokenKind, 'migrate'>, label?: string): Promise<NewToken>;
  revokeToken(id: string): Promise<void>;

  getUsage(): Promise<StorageUsage>;
  // Called when the storage use was counted again (read it again then).
  onUsageChanged(listener: () => void): () => void;

  serverVersion(): Promise<ServerVersion>;
}
