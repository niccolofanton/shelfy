// The recent explicit syncs, for the side panel (P2-13): a run leaves the queue once its closing
// PATCH went out, and its summary moves here (chrome.storage.local `shelfy.syncs`, newest first,
// at most 20). The accepted keys of each run stay in IndexedDB for 7 days (sw/queue `runKeys`).

import {
  isRecord,
  type Platform,
  type SyncEndReason,
  type SyncMode,
  type SyncPhase,
  type Trigger,
} from '../../shared/protocol';
import type { RunState } from '../queue/types';
import type { StorageArea } from '../settings';

export const HISTORY_KEY = 'shelfy.syncs';
export const HISTORY_SIZE = 20;

/** One explicit sync as the panel shows it. */
export interface SyncView {
  runId: string;
  serverId: string | null;
  tabId: number | null;
  platform: Platform;
  trigger: Trigger;
  listingKey: string;
  listingName: string | null;
  /** `open` while the controller walks; `ended` while its last batches go out. */
  state: RunState;
  phase: SyncPhase | null;
  stopReason: SyncEndReason | null;
  errorCode: string | null;
  skipped: SyncMode[];
  incremental: boolean;
  pages: number;
  replayPages: number;
  steps: number;
  scanned: number;
  inserted: number;
  known: number;
  queued: number;
  startedAt: number;
  endedAt: number | null;
}

export class SyncHistory {
  /** Writes run one at a time, so two closes never lose an entry. */
  private chain: Promise<unknown> = Promise.resolve();

  constructor(private readonly area: StorageArea) {}

  async list(): Promise<SyncView[]> {
    const stored = (await this.area.get(HISTORY_KEY))[HISTORY_KEY];
    return Array.isArray(stored)
      ? stored.filter(
          (entry): entry is SyncView => isRecord(entry) && typeof entry.runId === 'string',
        )
      : [];
  }

  add(view: SyncView, at: number): Promise<void> {
    const run = this.chain.then(async () => {
      const entries = (await this.list()).filter((entry) => entry.runId !== view.runId);
      entries.unshift({ ...view, state: 'ended', queued: 0, endedAt: view.endedAt ?? at });
      await this.area.set({ [HISTORY_KEY]: entries.slice(0, HISTORY_SIZE) });
    });
    this.chain = run.catch(() => undefined);
    return run;
  }
}
