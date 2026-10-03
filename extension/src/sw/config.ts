// The server's extension config (C3): kill switches, pacing, stop thresholds and the minimum
// extension version. Cached in storage with its ETag and refreshed every 5 minutes (the
// maintenance alarm), at start-up, after pairing, and at once after a 409 `source_disabled`, so a
// kill switch applies within one refresh. Data only, never code (plan §2.16).

import type { Platform } from '../shared/protocol';
import { isBelowMinVersion } from '../shared/version';
import type { ApiClient } from './api';
import { API, parseConfig, type ExtensionConfig } from './contracts';
import type { ApiFailure } from './errors';
import type { SettingsStore } from './settings';

export const CONFIG_REFRESH_MS = 5 * 60_000;

export type RefreshOutcome =
  | { outcome: 'updated' | 'unchanged' | 'fresh' | 'unpaired' }
  | { outcome: 'failed'; failure: ApiFailure };

export interface ConfigDeps {
  api: ApiClient;
  store: SettingsStore;
  version: string;
  now(): number;
}

export class ConfigService {
  constructor(private readonly deps: ConfigDeps) {}

  current(): Promise<ExtensionConfig> {
    return this.deps.store.configValue();
  }

  /** Fetches the config when the cached one is older than 5 minutes, or always with `force`. */
  async refresh(force = false): Promise<RefreshOutcome> {
    const { api, store, now } = this.deps;
    if (!(await store.pairing())) return { outcome: 'unpaired' };
    const stored = await store.config();
    if (!force && stored && now() - stored.fetchedAt < CONFIG_REFRESH_MS)
      return { outcome: 'fresh' };
    const response = await api.get(API.config, { auth: 'token', etag: stored?.etag ?? null });
    if (!response.ok) return { outcome: 'failed', failure: response.failure };
    let value: ExtensionConfig;
    let outcome: 'updated' | 'unchanged';
    if (response.status === 304 && stored) {
      value = stored.value;
      outcome = 'unchanged';
      await store.setConfig({ ...stored, fetchedAt: now() });
    } else {
      value = parseConfig(response.data);
      outcome = 'updated';
      await store.setConfig({ value, etag: response.etag, fetchedAt: now() });
    }
    await store.patchStatus({
      outdated: isBelowMinVersion(this.deps.version, value.minVersion),
      minVersion: value.minVersion,
    });
    return { outcome };
  }
}

/** Whether the server lets passive capture run on `platform` (its kill switch). */
export function passiveAllowed(config: ExtensionConfig, platform: Platform): boolean {
  return config.platforms[platform].passive;
}
