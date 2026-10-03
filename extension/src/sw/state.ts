// What the side panel shows, as one snapshot (MSG.stateGet). Never the token, and never the
// Access service-token values: only whether they are set.

import { PLATFORMS, type CensusCounts, type Platform } from '../shared/protocol';
import { passiveAllowed, type ConfigService } from './config';
import type { Queue } from './queue/queue';
import type { Counters } from './queue/types';
import type { ConnectionCheck, SettingsStore, Status } from './settings';

export interface PanelState {
  version: string;
  origin: string;
  debug: boolean;
  paired: boolean;
  pairedAt: number | null;
  outdated: boolean;
  minVersion: string | null;
  passive: Record<Platform, boolean>;
  /** The server's kill switches: false = passive capture turned off for the platform. */
  serverPassive: Record<Platform, boolean>;
  passiveFolders: boolean;
  accessHeadersSet: boolean;
  configFetchedAt: number | null;
  queue: Counters;
  openRuns: number;
  blockedUntil: number;
  lastError: Status['lastError'];
  lastOkAt: number | null;
  connection: ConnectionCheck | null;
  census: CensusCounts | null;
}

export interface StateDeps {
  version: string;
  origin: string;
  debug: boolean;
  store: SettingsStore;
  queue: Queue;
  config: ConfigService;
  census(): CensusCounts | null;
}

export async function panelState(deps: StateDeps): Promise<PanelState> {
  const [pairing, status, settings, config, stored, snapshot] = await Promise.all([
    deps.store.pairing(),
    deps.store.status(),
    deps.store.settings(),
    deps.config.current(),
    deps.store.config(),
    deps.queue.snapshot(),
  ]);
  return {
    version: deps.version,
    origin: deps.origin,
    debug: deps.debug,
    paired: pairing !== null,
    pairedAt: pairing?.pairedAt ?? null,
    outdated: status.outdated,
    minVersion: status.minVersion,
    passive: settings.passive,
    serverPassive: Object.fromEntries(
      PLATFORMS.map((platform) => [platform, passiveAllowed(config, platform)]),
    ) as Record<Platform, boolean>,
    passiveFolders: settings.passiveFolders,
    accessHeadersSet: settings.access !== null,
    configFetchedAt: stored?.fetchedAt ?? null,
    queue: snapshot.counters,
    openRuns: snapshot.runs.filter((run) => run.state === 'open').length,
    blockedUntil: status.blockedUntil,
    lastError: status.lastError,
    lastOkAt: status.lastOkAt,
    connection: status.connection,
    census: deps.debug ? deps.census() : null,
  };
}
