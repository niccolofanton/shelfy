// The worker's small persistent state, in chrome.storage.local (the queue lives in IndexedDB):
//
//   shelfy.install   {installId}                 random, made once (C2 `installId`)
//   shelfy.pairing   {token, tokenId, scopes, pairedAt}   the extension token: never sent to a
//                                                 content script or a page; the worker restricts
//                                                 storage.local to trusted contexts (sw/index.ts)
//   shelfy.settings  passive toggles, folder mapping, Access service-token headers
//   shelfy.config    the last GET /extension/config answer, its ETag and time
//   shelfy.status    outdated, retry gate, last error, last "Check connection" result

import {
  PLATFORMS,
  isPlatform,
  isRecord,
  type AccessHeaders,
  type Platform,
  type SettingsPatch,
} from '../shared/protocol';
import { DEFAULT_CONFIG, parseConfig, type ExtensionConfig } from './contracts';
import { ACCOUNT_STATE_KEYS } from './account-state';

export const KEYS = {
  install: 'shelfy.install',
  pairing: 'shelfy.pairing',
  settings: 'shelfy.settings',
  config: 'shelfy.config',
  status: 'shelfy.status',
} as const;

/** The subset of chrome.storage.StorageArea the worker uses (fakes implement it in tests). */
export interface StorageArea {
  get(keys: string | string[]): Promise<Record<string, unknown>>;
  set(items: Record<string, unknown>): Promise<void>;
  remove(keys: string | string[]): Promise<void>;
}

export interface Pairing {
  /** Missing on legacy pairings: web commands fail closed until paired again. */
  accountId?: string;
  token: string;
  tokenId: string;
  scopes: string[];
  pairedAt: number;
}

export interface Settings {
  passive: Record<Platform, boolean>;
  passiveFolders: boolean;
  access: AccessHeaders | null;
}

export const DEFAULT_SETTINGS: Settings = {
  passive: { instagram: true, twitter: true, pinterest: true },
  passiveFolders: true,
  access: null,
};

export interface StoredConfig {
  value: ExtensionConfig;
  etag: string | null;
  fetchedAt: number;
}

export type ProbeOutcome = 'ok' | 'access_redirect' | 'failed';

export interface Probe {
  outcome: ProbeOutcome;
  status: number | null;
  detail: string | null;
  /** The server version `/health` reported. */
  version: string | null;
}

export type TokenOutcome = 'ok' | 'unauthorized' | 'outdated' | 'unpaired' | 'failed';

/** The result of "Check connection" (P2-22 reads it). */
export interface ConnectionCheck {
  at: number;
  origin: string;
  /** `/health` with the browser's cookie and no service-token headers. */
  cookie: Probe;
  /** `/health` with the service-token headers and no cookie; null when none are saved. */
  headers: Probe | null;
  token: { outcome: TokenOutcome; detail: string | null };
}

export interface Status {
  /** The server needs a newer extension (426, or `minVersion` above ours). */
  outdated: boolean;
  minVersion: string | null;
  /** No request is sent before this time (Retry-After, backoff, hold). */
  blockedUntil: number;
  /** Consecutive failed attempts, for the backoff. */
  failures: number;
  lastError: { code: string; at: number } | null;
  lastOkAt: number | null;
  connection: ConnectionCheck | null;
}

export const DEFAULT_STATUS: Status = {
  outdated: false,
  minVersion: null,
  blockedUntil: 0,
  failures: 0,
  lastError: null,
  lastOkAt: null,
  connection: null,
};

function readPairing(value: unknown): Pairing | null {
  if (!isRecord(value) || typeof value.token !== 'string' || typeof value.tokenId !== 'string')
    return null;
  return {
    token: value.token,
    tokenId: value.tokenId,
    ...(typeof value.accountId === 'string' && value.accountId.length > 0
      ? { accountId: value.accountId }
      : {}),
    scopes: Array.isArray(value.scopes) ? value.scopes.filter((s) => typeof s === 'string') : [],
    pairedAt: typeof value.pairedAt === 'number' ? value.pairedAt : 0,
  };
}

function readSettings(value: unknown): Settings {
  const settings = structuredClone(DEFAULT_SETTINGS);
  if (!isRecord(value)) return settings;
  if (isRecord(value.passive))
    for (const [platform, enabled] of Object.entries(value.passive))
      if (isPlatform(platform) && typeof enabled === 'boolean')
        settings.passive[platform] = enabled;
  if (typeof value.passiveFolders === 'boolean') settings.passiveFolders = value.passiveFolders;
  if (
    isRecord(value.access) &&
    typeof value.access.clientId === 'string' &&
    typeof value.access.clientSecret === 'string'
  )
    settings.access = { clientId: value.access.clientId, clientSecret: value.access.clientSecret };
  return settings;
}

function readStatus(value: unknown): Status {
  return isRecord(value)
    ? { ...DEFAULT_STATUS, ...(value as Partial<Status>) }
    : { ...DEFAULT_STATUS };
}

function readConfig(value: unknown): StoredConfig | null {
  if (!isRecord(value) || typeof value.fetchedAt !== 'number') return null;
  return {
    value: parseConfig(value.value),
    etag: typeof value.etag === 'string' ? value.etag : null,
    fetchedAt: value.fetchedAt,
  };
}

/** A random install id: 128 bits, hex. */
export function randomInstallId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');
}

export class SettingsStore {
  /** Read-modify-write updates run one at a time, so concurrent patches never lose a field. */
  private chain: Promise<unknown> = Promise.resolve();

  constructor(private readonly area: StorageArea) {}

  private serial<T>(task: () => Promise<T>): Promise<T> {
    const run = this.chain.then(task);
    this.chain = run.catch(() => undefined);
    return run;
  }

  private async read(key: string): Promise<unknown> {
    return (await this.area.get(key))[key];
  }

  installId(): Promise<string> {
    return this.serial(async () => {
      const stored = await this.read(KEYS.install);
      if (isRecord(stored) && typeof stored.installId === 'string' && stored.installId.length >= 16)
        return stored.installId;
      const installId = randomInstallId();
      await this.area.set({ [KEYS.install]: { installId } });
      return installId;
    });
  }

  async pairing(): Promise<Pairing | null> {
    return readPairing(await this.read(KEYS.pairing));
  }

  async setPairing(pairing: Pairing | null): Promise<void> {
    const previous = await this.pairing();
    if (!pairing || previous?.tokenId !== pairing.tokenId)
      await this.area.remove([...ACCOUNT_STATE_KEYS]);
    if (pairing) await this.area.set({ [KEYS.pairing]: pairing });
    else await this.area.remove(KEYS.pairing);
  }

  async settings(): Promise<Settings> {
    return readSettings(await this.read(KEYS.settings));
  }

  patchSettings(patch: SettingsPatch): Promise<Settings> {
    return this.serial(async () => {
      const settings = await this.settings();
      if (patch.passive)
        for (const platform of PLATFORMS)
          if (typeof patch.passive[platform] === 'boolean')
            settings.passive[platform] = patch.passive[platform] as boolean;
      if (patch.passiveFolders !== undefined) settings.passiveFolders = patch.passiveFolders;
      if (patch.access !== undefined) settings.access = patch.access;
      await this.area.set({ [KEYS.settings]: settings });
      return settings;
    });
  }

  async status(): Promise<Status> {
    return readStatus(await this.read(KEYS.status));
  }

  /** Applies `patch` (or the patch `update` derives from the current status) atomically. */
  patchStatus(patch: Partial<Status> | ((status: Status) => Partial<Status>)): Promise<Status> {
    return this.serial(async () => {
      const current = await this.status();
      const status = { ...current, ...(typeof patch === 'function' ? patch(current) : patch) };
      await this.area.set({ [KEYS.status]: status });
      return status;
    });
  }

  async config(): Promise<StoredConfig | null> {
    return readConfig(await this.read(KEYS.config));
  }

  async configValue(): Promise<ExtensionConfig> {
    return (await this.config())?.value ?? structuredClone(DEFAULT_CONFIG);
  }

  async setConfig(config: StoredConfig): Promise<void> {
    await this.area.set({ [KEYS.config]: config });
  }
}
