// A fake signed-in account (src/api/account.ts) and a web client that holds
// it, for the suites of the account's UI.
import { vi, type Mock } from 'vitest';
import type { AccountApi, ConsentRecord } from '@ui/api/account';
import type { ShelfyClient } from '@ui/api/ShelfyClient';
import { DISCLAIMER_VERSION, PRIVACY_VERSION } from '@ui/disclaimer';
import { webCapabilities } from '../src/api/httpClient';
import { webMedia } from '../src/api/mapping';
import { OWNER } from './authFakes';

export type FakeAccount = {
  [K in keyof AccountApi]: AccountApi[K] extends (...a: never[]) => unknown ? Mock : AccountApi[K];
} & {
  // Says the storage was counted again (`onUsageChanged`).
  emitUsage: () => void;
};

export const NO_CONSENT: ConsentRecord = {
  disclaimerVersion: null,
  disclaimerAcceptedAt: null,
  privacyVersion: null,
  privacyAcceptedAt: null,
};

export const CURRENT_CONSENT: ConsentRecord = {
  disclaimerVersion: DISCLAIMER_VERSION,
  disclaimerAcceptedAt: 1,
  privacyVersion: PRIVACY_VERSION,
  privacyAcceptedAt: 1,
};

export function fakeAccount(
  overrides: Partial<Record<keyof AccountApi, unknown>> = {},
): FakeAccount {
  const usageListeners = new Set<() => void>();
  const account = {
    profile: { id: 'u1', email: 'o@x.test', role: 'owner', createdAt: 0 },
    signIn: { passkeys: true, emailLink: false },
    consent: vi.fn(() => NO_CONSENT),
    acceptConsent: vi.fn(async () => CURRENT_CONSENT),
    getSettings: vi.fn(async () => ({
      language: null,
      archiveAssetTypes: { thumbnail: true, image: true, video: false },
    })),
    updateSettings: vi.fn(async (patch: object) => ({
      language: null,
      archiveAssetTypes: { thumbnail: true, image: true, video: false },
      ...patch,
    })),
    listPasskeys: vi.fn(async () => [
      { id: 3, label: 'MacBook', createdAt: Date.UTC(2026, 9, 1), lastUsedAt: null },
    ]),
    preparePasskey: vi.fn(async () => ({
      create: vi.fn(async (label?: string) => ({
        id: 4,
        label: label ?? null,
        createdAt: 1,
        lastUsedAt: null,
      })),
    })),
    removePasskey: vi.fn(async () => {}),
    listSessions: vi.fn(async () => [
      {
        id: 'aa',
        current: true,
        createdAt: 1,
        lastSeenAt: 2,
        expiresAt: 3,
        userAgent:
          'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36',
      },
      {
        id: 'bb',
        current: false,
        createdAt: 1,
        lastSeenAt: 2,
        expiresAt: 3,
        userAgent:
          'Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Mobile/15E148 Safari/604.1',
      },
    ]),
    endSession: vi.fn(async () => {}),
    endOtherSessions: vi.fn(async () => {}),
    signOut: vi.fn(async () => {}),
    listTokens: vi.fn(async () => [
      {
        id: 't1',
        kind: 'migrate',
        label: null,
        scopes: ['migrate'],
        createdAt: 1,
        lastUsedAt: null,
        expiresAt: Date.UTC(2026, 9, 9),
      },
    ]),
    createToken: vi.fn(async (kind: string, label?: string) => ({
      value: 'shx_shown_once',
      token: {
        id: 't2',
        kind,
        label: label ?? null,
        scopes: ['links:create'],
        createdAt: 2,
        lastUsedAt: null,
        expiresAt: null,
      },
    })),
    revokeToken: vi.fn(async () => {}),
    getUsage: vi.fn(async () => ({
      usedBytes: 3 * 1024 * 1024,
      mediaBytes: 2 * 1024 * 1024,
      dbBytes: 1024 * 1024,
      quotaBytes: 0,
      updatedAt: 1,
    })),
    onUsageChanged: vi.fn((listener: () => void) => {
      usageListeners.add(listener);
      return () => usageListeners.delete(listener);
    }),
    serverVersion: vi.fn(async () => ({ version: '0.1.0', apiVersion: '1' })),
    emitUsage: () => usageListeners.forEach((l) => l()),
    ...overrides,
  };
  return account as unknown as FakeAccount;
}

// The web client of the signed-in owner, holding `account`.
export function webClient(account: FakeAccount): ShelfyClient {
  return {
    capabilities: webCapabilities(OWNER),
    media: webMedia,
    account: account as unknown as AccountApi,
    listPosts: vi.fn(),
    getPostsByIds: vi.fn(),
    getStats: vi.fn(),
    listCollections: vi.fn(),
    updatePost: vi.fn(),
    createCollection: vi.fn(),
    updateCollection: vi.fn(),
    deleteCollection: vi.fn(),
    addPostsToCollections: vi.fn(),
    removePostFromCollection: vi.fn(),
    openExternal: vi.fn(),
    on: vi.fn(() => () => {}),
    reportError: vi.fn(),
  };
}
