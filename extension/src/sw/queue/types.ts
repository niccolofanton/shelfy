// Records of the offline queue (IndexedDB database `shelfy`, sw/queue/idb.ts) and the
// transaction interface the queue logic (sw/queue/queue.ts) runs on. An in-memory store
// (sw/queue/memory.ts) implements the same interface for the unit tests.
//
//   chunks   one relayed hook message (or a part of it), waiting to be sealed into a batch;
//            keyed by an auto-increment `seq`, indexed by `group` = `<runId>|<source>`
//   batches  sealed ingest batches, keyed by `id` (a ULID: creation order), indexed by run;
//            `key` is the Idempotency-Key and stays the same across retries
//   runs     sync runs the queued items belong to (C4), keyed by a local ULID
//   meta     one record: counters, the open groups, recent message ids
//   runKeys  the keys the server accepted, per batch and run, kept 7 days (P2-13, for P2-19's
//            run report); keyed by the batch's Idempotency-Key, indexed by run and time

import type { WireListing } from '../../shared/listing';
import type {
  Platform,
  SyncEndReason,
  SyncMode,
  SyncPhase,
  Trigger,
  WireSource,
} from '../../shared/protocol';
import type { BatchClient, CollectionMode } from '../contracts';
import type { WireItem } from '../prefilter';

export interface Chunk {
  /** Assigned by the store (auto-increment). */
  seq?: number;
  group: string;
  runId: string;
  source: WireSource;
  hasNextPage: boolean | null;
  items: WireItem[];
  count: number;
  bytes: number;
  at: number;
}

export interface Batch {
  /** Queue order (ULID, or a ULID with `.1`/`.2` suffixes after a split). */
  id: string;
  /** Idempotency-Key (a ULID). Changes only when the body must change (new run, split). */
  key: string;
  runId: string;
  platform: Platform;
  source: WireSource;
  hasNextPage: boolean | null;
  client: BatchClient;
  items: WireItem[];
  count: number;
  bytes: number;
  createdAt: number;
  attempts: number;
  /** The `syncRunId` the body was first sent with; another run id needs another key. */
  sentRunId: string | null;
}

export type RunState = 'open' | 'ended';

export interface Run {
  /** Pairing owning a run; an old account's queued captures must never migrate. */
  accountTokenId?: string;
  id: string;
  /** The server's run id, once `POST /sync-runs` answered. */
  serverId: string | null;
  platform: Platform;
  trigger: Trigger;
  listing: WireListing;
  collection: CollectionMode;
  /** Where the run's captures come from (passive runs: one listing visit in one document). */
  tabId: number | null;
  docId: string | null;
  listingKey: string;
  state: RunState;
  stopReason: SyncEndReason | null;
  /** Relayed messages and accepted items, reported in the closing PATCH (C4). */
  pages: number;
  scanned: number;
  /** Items of this run still queued (chunks and batches). */
  queued: number;
  createdAt: number;
  lastAt: number;
  endedAt: number | null;
  // P2-13. Explicit runs (manual, web, scheduled) fill these; passive runs keep the defaults
  // (normalizeRun completes records stored by an older build).
  /** What the server answered at POST /sync-runs (C4). */
  incremental: boolean;
  stopAfterKnown: number;
  collectionId: number | null;
  /** From the ingest results (C5): new and known items, and the trailing run of known ones. */
  inserted: number;
  known: number;
  knownStreak: number;
  /** Reported in the closing PATCH: the cursor of a capped walk, and an error code. */
  resumeCursor: string | null;
  errorCode: string | null;
  /** The controller's last report. */
  phase: SyncPhase | null;
  steps: number;
  replayPages: number;
  /** Modes the config killed, skipped by the walk. */
  skipped: SyncMode[];
}

/** The P2-13 fields of a run, as a passive run (or a record of an older build) has them. */
export const RUN_SYNC_DEFAULTS = {
  incremental: false,
  stopAfterKnown: 0,
  collectionId: null,
  inserted: 0,
  known: 0,
  knownStreak: 0,
  resumeCursor: null,
  errorCode: null,
  phase: null,
  steps: 0,
  replayPages: 0,
  skipped: [],
} satisfies Partial<Run>;

/** A stored run completed with the fields a newer build added. */
export function normalizeRun(value: Run): Run {
  return { ...structuredClone(RUN_SYNC_DEFAULTS), ...value };
}

/** The keys one ingest batch got accepted under (C5 `results[].key`), for the run report. */
export interface RunKeys {
  /** The batch's Idempotency-Key. */
  id: string;
  runId: string;
  /** The server's run id the batch was sent with. */
  serverRunId: string | null;
  at: number;
  keys: string[];
}

/** Accepted keys are kept this long (P2-13). */
export const RUN_KEYS_TTL_MS = 7 * 24 * 60 * 60_000;

export interface GroupInfo {
  runId: string;
  firstAt: number;
  count: number;
  bytes: number;
  /** An explicit sync's group: sealed at once, so each page's outcome comes back (P2-13). */
  eager?: boolean;
}

export const DISCARD_REASONS = [
  'out_of_scope',
  'unpaired',
  'disabled',
  'killed',
  'outdated',
  'not_own_board',
  'viewer_unknown',
  'invalid',
  'sender',
] as const;
export type DiscardReason = (typeof DISCARD_REASONS)[number];

export interface Counters {
  /** Items queued now: in chunks and in batches. */
  queuedItems: number;
  pendingItems: number;
  batches: number;
  /** Items dropped, oldest first, to keep the queue at its cap. */
  droppedItems: number;
  sentBatches: number;
  sentItems: number;
  inserted: number;
  updated: number;
  known: number;
  rejected: number;
  /** Batches and items the server refused for good (dropped after a 4xx). */
  refusedBatches: number;
  refusedItems: number;
  /** Items accepted into the queue, per platform. */
  captured: Record<Platform, number>;
  /** Items not captured, by reason. */
  discarded: Record<DiscardReason, number>;
  lastCaptureAt: number | null;
  lastSentAt: number | null;
}

export interface QueueMeta {
  key: 'meta';
  counters: Counters;
  groups: Record<string, GroupInfo>;
  /** Ids of recently queued messages (`<docId>:<seq>`), to drop a delivery made twice. */
  recent: string[];
}

export function emptyCounters(): Counters {
  return {
    queuedItems: 0,
    pendingItems: 0,
    batches: 0,
    droppedItems: 0,
    sentBatches: 0,
    sentItems: 0,
    inserted: 0,
    updated: 0,
    known: 0,
    rejected: 0,
    refusedBatches: 0,
    refusedItems: 0,
    captured: { instagram: 0, twitter: 0, pinterest: 0 },
    discarded: Object.fromEntries(DISCARD_REASONS.map((reason) => [reason, 0])) as Record<
      DiscardReason,
      number
    >,
    lastCaptureAt: null,
    lastSentAt: null,
  };
}

export function emptyMeta(): QueueMeta {
  return { key: 'meta', counters: emptyCounters(), groups: {}, recent: [] };
}

/** A stored meta record completed with the fields a newer build added. */
export function normalizeMeta(value: unknown): QueueMeta {
  const base = emptyMeta();
  if (!value || typeof value !== 'object') return base;
  const stored = value as Partial<QueueMeta>;
  const counters = { ...base.counters, ...(stored.counters ?? {}) };
  counters.captured = { ...base.counters.captured, ...(stored.counters?.captured ?? {}) };
  counters.discarded = { ...base.counters.discarded, ...(stored.counters?.discarded ?? {}) };
  return {
    key: 'meta',
    counters,
    groups: stored.groups && typeof stored.groups === 'object' ? stored.groups : {},
    recent: Array.isArray(stored.recent) ? stored.recent : [],
  };
}

/** One transaction over the queue's stores. Only these calls may be awaited inside it. */
export interface QueueTx {
  meta(): Promise<QueueMeta>;
  putMeta(meta: QueueMeta): Promise<void>;
  addChunk(chunk: Chunk): Promise<number>;
  /** The chunks of a group, by `seq`. */
  groupChunks(group: string): Promise<Chunk[]>;
  /** The chunk with the lowest `seq`, any group. */
  firstChunk(): Promise<Chunk | null>;
  deleteChunk(seq: number): Promise<void>;
  putBatch(batch: Batch): Promise<void>;
  getBatch(id: string): Promise<Batch | null>;
  deleteBatch(id: string): Promise<void>;
  /** The batch with the lowest `id`. */
  firstBatch(): Promise<Batch | null>;
  /** Ids of a run's batches, in order. */
  runBatchIds(runId: string): Promise<string[]>;
  getRun(id: string): Promise<Run | null>;
  putRun(run: Run): Promise<void>;
  deleteRun(id: string): Promise<void>;
  runs(): Promise<Run[]>;
  putRunKeys(record: RunKeys): Promise<void>;
  /** A run's accepted keys, oldest batch first. */
  runKeys(runId: string): Promise<RunKeys[]>;
  /** Deletes the records older than `before`; returns how many. */
  pruneRunKeys(before: number): Promise<number>;
}

export interface QueueStore {
  /** Runs `body` in one read-write transaction; transactions run one after the other. */
  transaction<T>(body: (tx: QueueTx) => Promise<T>): Promise<T>;
}
