// The offline queue (plan §2.16): captured items wait here until the server takes them.
//
// - Items arrive in chunks (one relayed hook message, or a part of one) grouped by run and
//   source. A group is sealed into an ingest batch when it holds 500 items (or ~7 MiB), or 2 s
//   after its first item arrived — "batches of ≤ 500 items or 2 s".
// - Each batch gets a ULID `key`, sent as `Idempotency-Key`. The key and the body stay the same
//   across retries, so a batch the server already took (its answer lost) is never ingested
//   twice: the server answers the repeat with the first response. Only a change of body (a new
//   run id after the server lost the run, or a split after a 413) takes a new key.
// - At most 50,000 items wait; past that, the oldest batches (then the oldest chunks) are
//   dropped and counted.
// - Everything is in IndexedDB, so the queue survives worker suspension and browser restarts.
//   Each method is one transaction.

import type { WireListing } from '../../shared/listing';
import type { Platform, SyncEndReason, Trigger, WireSource } from '../../shared/protocol';
import { ulid as defaultUlid } from '../../shared/ulid';
import type { BatchClient, CollectionMode, IngestResult } from '../contracts';
import { itemBytes, type WireItem } from '../prefilter';
import {
  RUN_SYNC_DEFAULTS,
  RUN_KEYS_TTL_MS,
  type Batch,
  type Chunk,
  type Counters,
  type DiscardReason,
  type QueueMeta,
  type QueueStore,
  type QueueTx,
  type Run,
  type RunKeys,
} from './types';

export interface QueueOptions {
  maxItems: number;
  maxBatchItems: number;
  maxBatchBytes: number;
  windowMs: number;
  maxRecent: number;
  client: BatchClient;
  ulid: () => string;
}

export const QUEUE_LIMITS = {
  maxItems: 50_000,
  maxBatchItems: 500,
  /** The server takes ≤ 8 MiB per batch; the items get 7 MiB, the envelope the rest. */
  maxBatchBytes: 7 * 1024 * 1024,
  windowMs: 2_000,
  maxRecent: 256,
} as const;

export interface RunSpec {
  accountTokenId?: string;
  platform: Platform;
  trigger: Trigger;
  listing: WireListing;
  collection: CollectionMode;
  tabId: number | null;
  docId: string | null;
  listingKey: string;
}

export interface CaptureInput {
  source: WireSource;
  hasNextPage: boolean | null;
  items: WireItem[];
  at: number;
  /** `<docId>:<seq>` of the relayed message; a second delivery of it is ignored. */
  messageId: string | null;
}

export interface CaptureResult {
  /** The run the items went to; null for a duplicate delivery. */
  run: Run | null;
  accepted: number;
  /** Items dropped, oldest first, to make room. */
  dropped: number;
  duplicate: boolean;
  /** The run's group holds a full batch: seal it now. */
  full: boolean;
}

export interface SealResult {
  sealed: number;
  /** When the next open group falls due, or null when none is open. */
  nextDueAt: number | null;
}

export interface QueueSnapshot {
  counters: Counters;
  runs: Run[];
}

type Piece = { items: WireItem[]; bytes: number };

function combineHasNextPage(chunks: readonly Chunk[]): boolean | null {
  let value: boolean | null = null;
  for (const chunk of chunks) {
    if (chunk.hasNextPage === false) return false;
    if (chunk.hasNextPage !== null) value = chunk.hasNextPage;
  }
  return value;
}

export class Queue {
  readonly options: QueueOptions;

  constructor(
    private readonly store: QueueStore,
    options: Partial<QueueOptions> & { client: BatchClient },
  ) {
    this.options = { ...QUEUE_LIMITS, ulid: defaultUlid, ...options };
  }

  private get ulid(): () => string {
    return this.options.ulid;
  }

  /** Splits a message's items into chunks that each fit in one batch. */
  private pieces(items: readonly WireItem[]): Piece[] {
    const out: Piece[] = [];
    let current: Piece = { items: [], bytes: 0 };
    for (const item of items) {
      const bytes = itemBytes(item);
      if (
        current.items.length &&
        (current.items.length >= this.options.maxBatchItems ||
          current.bytes + bytes > this.options.maxBatchBytes)
      ) {
        out.push(current);
        current = { items: [], bytes: 0 };
      }
      current.items.push(item);
      current.bytes += bytes;
    }
    if (current.items.length) out.push(current);
    return out;
  }

  private async touchRun(
    tx: QueueTx,
    touched: Map<string, Run>,
    runId: string,
    change: (run: Run) => void,
  ): Promise<void> {
    const run = touched.get(runId) ?? (await tx.getRun(runId));
    if (!run) return;
    change(run);
    touched.set(run.id, run);
  }

  private async saveRuns(tx: QueueTx, touched: Map<string, Run>): Promise<void> {
    for (const run of touched.values()) await tx.putRun(run);
  }

  /** Removes the oldest batch, or the oldest chunk; returns the items removed. */
  private async dropOldest(tx: QueueTx, meta: QueueMeta, touched: Map<string, Run>) {
    const counters = meta.counters;
    const batch = await tx.firstBatch();
    if (batch) {
      await tx.deleteBatch(batch.id);
      counters.batches -= 1;
      counters.queuedItems -= batch.count;
      counters.droppedItems += batch.count;
      await this.touchRun(tx, touched, batch.runId, (run) => (run.queued -= batch.count));
      return batch.count;
    }
    const chunk = await tx.firstChunk();
    if (!chunk || chunk.seq === undefined) return 0;
    await tx.deleteChunk(chunk.seq);
    counters.pendingItems -= chunk.count;
    counters.queuedItems -= chunk.count;
    counters.droppedItems += chunk.count;
    const group = meta.groups[chunk.group];
    if (group) {
      group.count -= chunk.count;
      group.bytes -= chunk.bytes;
      // firstAt stays: the group just falls due a little earlier.
      if (group.count <= 0) delete meta.groups[chunk.group];
    }
    await this.touchRun(tx, touched, chunk.runId, (run) => (run.queued -= chunk.count));
    return chunk.count;
  }

  private async sealGroup(
    tx: QueueTx,
    meta: QueueMeta,
    group: string,
    now: number,
    touched: Map<string, Run>,
  ): Promise<number> {
    const info = meta.groups[group];
    delete meta.groups[group];
    const chunks = await tx.groupChunks(group);
    if (!chunks.length) return 0;
    const runId = info?.runId ?? chunks[0].runId;
    const run = touched.get(runId) ?? (await tx.getRun(runId));
    if (!run) {
      // Orphaned chunks (their run is gone): count them as dropped.
      for (const chunk of chunks) {
        if (chunk.seq !== undefined) await tx.deleteChunk(chunk.seq);
        meta.counters.pendingItems -= chunk.count;
        meta.counters.queuedItems -= chunk.count;
        meta.counters.droppedItems += chunk.count;
      }
      return 0;
    }
    let sealed = 0;
    let current: Chunk[] = [];
    let count = 0;
    let bytes = 0;
    const flush = async (): Promise<void> => {
      if (!current.length) return;
      const batch: Batch = {
        id: this.ulid(),
        key: this.ulid(),
        runId: run.id,
        platform: run.platform,
        source: current[0].source,
        hasNextPage: combineHasNextPage(current),
        client: this.options.client,
        items: current.flatMap((chunk) => chunk.items),
        count,
        bytes,
        createdAt: now,
        attempts: 0,
        sentRunId: null,
      };
      await tx.putBatch(batch);
      for (const chunk of current) if (chunk.seq !== undefined) await tx.deleteChunk(chunk.seq);
      meta.counters.batches += 1;
      meta.counters.pendingItems -= count;
      sealed += 1;
      current = [];
      count = 0;
      bytes = 0;
    };
    for (const chunk of chunks) {
      if (
        current.length &&
        (count + chunk.count > this.options.maxBatchItems ||
          bytes + chunk.bytes > this.options.maxBatchBytes)
      )
        await flush();
      current.push(chunk);
      count += chunk.count;
      bytes += chunk.bytes;
    }
    await flush();
    return sealed;
  }

  // ── Capture ───────────────────────────────────────────────────────────────

  /**
   * Queues a passive capture: finds the open passive run of this tab, document and listing, or
   * ends the tab's previous passive run (another listing or a reload) and opens a new one, then
   * appends the items to it. One transaction, so a run is never closed under a capture.
   */
  capturePassive(spec: RunSpec, input: CaptureInput): Promise<CaptureResult> {
    return this.store.transaction(async (tx) => {
      const meta = await tx.meta();
      if (input.messageId && meta.recent.includes(input.messageId))
        return { run: null, accepted: 0, dropped: 0, duplicate: true, full: false };
      const touched = new Map<string, Run>();
      const open = (await tx.runs()).filter(
        (run) => run.state === 'open' && run.trigger === 'passive' && run.tabId === spec.tabId,
      );
      let run =
        open.find(
          (r) =>
            r.docId === spec.docId &&
            r.listingKey === spec.listingKey &&
            r.accountTokenId === spec.accountTokenId,
        ) ?? null;
      for (const other of open)
        if (other !== run) {
          other.state = 'ended';
          other.stopReason = 'user';
          other.endedAt = input.at;
          touched.set(other.id, other);
        }
      if (!run) {
        run = {
          id: this.ulid(),
          serverId: null,
          ...spec,
          ...structuredClone(RUN_SYNC_DEFAULTS),
          state: 'open',
          stopReason: null,
          pages: 0,
          scanned: 0,
          queued: 0,
          createdAt: input.at,
          lastAt: input.at,
          endedAt: null,
        };
      }
      touched.set(run.id, run);
      const result = await this.appendTo(tx, meta, touched, run, input);
      await this.saveRuns(tx, touched);
      await tx.putMeta(meta);
      return { run, duplicate: false, ...result };
    });
  }

  private async appendTo(
    tx: QueueTx,
    meta: QueueMeta,
    touched: Map<string, Run>,
    run: Run,
    input: CaptureInput,
  ): Promise<{ accepted: number; dropped: number; full: boolean }> {
    const total = input.items.length;
    let dropped = 0;
    let excess = meta.counters.queuedItems + total - this.options.maxItems;
    while (excess > 0) {
      const removed = await this.dropOldest(tx, meta, touched);
      if (removed === 0) break;
      dropped += removed;
      excess -= removed;
    }
    const group = `${run.id}|${input.source}`;
    for (const piece of this.pieces(input.items)) {
      await tx.addChunk({
        group,
        runId: run.id,
        source: input.source,
        hasNextPage: input.hasNextPage,
        items: piece.items,
        count: piece.items.length,
        bytes: piece.bytes,
        at: input.at,
      });
      const info = meta.groups[group] ?? {
        runId: run.id,
        firstAt: input.at,
        count: 0,
        bytes: 0,
        ...(run.trigger === 'passive' ? {} : { eager: true }),
      };
      info.count += piece.items.length;
      info.bytes += piece.bytes;
      meta.groups[group] = info;
    }
    const counters = meta.counters;
    counters.queuedItems += total;
    counters.pendingItems += total;
    counters.captured[run.platform] += total;
    counters.lastCaptureAt = input.at;
    run.queued += total;
    run.pages += 1;
    run.scanned += total;
    run.lastAt = input.at;
    if (input.messageId) {
      meta.recent.push(input.messageId);
      if (meta.recent.length > this.options.maxRecent)
        meta.recent.splice(0, meta.recent.length - this.options.maxRecent);
    }
    const info = meta.groups[group];
    return {
      accepted: total,
      dropped,
      full:
        !!info &&
        (info.count >= this.options.maxBatchItems || info.bytes >= this.options.maxBatchBytes),
    };
  }

  /** Counts items that were not captured. */
  countDiscard(reason: DiscardReason, items: number): Promise<void> {
    return this.store.transaction(async (tx) => {
      const meta = await tx.meta();
      meta.counters.discarded[reason] += items;
      await tx.putMeta(meta);
    });
  }

  // ── Batches ───────────────────────────────────────────────────────────────

  /** Seals the groups that are due (2 s old or full), or all of them with `all`. */
  sealDue(now: number, all = false): Promise<SealResult> {
    return this.store.transaction(async (tx) => {
      const meta = await tx.meta();
      const touched = new Map<string, Run>();
      let sealed = 0;
      let changed = false;
      let nextDueAt: number | null = null;
      for (const [group, info] of Object.entries(meta.groups)) {
        const due =
          all ||
          info.eager === true ||
          info.firstAt + this.options.windowMs <= now ||
          info.count >= this.options.maxBatchItems ||
          info.bytes >= this.options.maxBatchBytes;
        if (due) {
          sealed += await this.sealGroup(tx, meta, group, now, touched);
          changed = true;
        } else nextDueAt = Math.min(nextDueAt ?? Infinity, info.firstAt + this.options.windowMs);
      }
      if (changed) {
        await this.saveRuns(tx, touched);
        await tx.putMeta(meta);
      }
      return { sealed, nextDueAt };
    });
  }

  /** The oldest sealed batch. */
  nextBatch(): Promise<Batch | null> {
    return this.store.transaction((tx) => tx.firstBatch());
  }

  /**
   * Records an attempt to send `batchId` for the server run `serverRunId`, and returns the batch
   * as it must be sent. A body first sent for another run id gets a new key.
   */
  prepareSend(batchId: string, serverRunId: string): Promise<Batch | null> {
    return this.store.transaction(async (tx) => {
      const batch = await tx.getBatch(batchId);
      if (!batch) return null;
      if (batch.sentRunId !== null && batch.sentRunId !== serverRunId) batch.key = this.ulid();
      batch.sentRunId = serverRunId;
      batch.attempts += 1;
      await tx.putBatch(batch);
      return batch;
    });
  }

  /** Records accepted keys from direct C5 clients, such as selection imports. */
  recordAcceptedKeys(record: RunKeys): Promise<void> {
    return this.store.transaction(async (tx) => {
      await tx.putRunKeys(record);
      await tx.pruneRunKeys(record.at - RUN_KEYS_TTL_MS);
    });
  }

  /** The server took the batch. */
  complete(batchId: string, result: IngestResult, at: number): Promise<void> {
    return this.store.transaction(async (tx) => {
      const meta = await tx.meta();
      const counters = meta.counters;
      const batch = await tx.getBatch(batchId);
      const touched = new Map<string, Run>();
      if (batch) {
        await tx.deleteBatch(batch.id);
        counters.batches -= 1;
        counters.queuedItems -= batch.count;
        counters.sentItems += batch.count;
        await this.touchRun(tx, touched, batch.runId, (run) => {
          run.queued -= batch.count;
          run.inserted += result.inserted;
          run.known += result.known;
          // The trailing run of known items, in item order (P2-G1): batches of a run are sent
          // in order, so this follows the listing.
          for (const entry of [...result.results].sort((a, b) => a.index - b.index))
            run.knownStreak = entry.outcome === 'known' ? run.knownStreak + 1 : 0;
        });
        const keys = result.results.map((entry) => entry.key);
        const owner = touched.get(batch.runId);
        if (keys.length)
          await tx.putRunKeys({
            ...(owner?.accountTokenId
              ? {
                  accountTokenId: owner.accountTokenId,
                  platform: owner.platform,
                  listingKey: owner.listingKey,
                  trigger: owner.trigger,
                }
              : {}),
            id: batch.key,
            runId: batch.runId,
            serverRunId: batch.sentRunId,
            at,
            keys,
          });
      }
      counters.sentBatches += 1;
      counters.inserted += result.inserted;
      counters.updated += result.updated;
      counters.known += result.known;
      counters.rejected += result.rejected.length;
      counters.lastSentAt = at;
      await this.saveRuns(tx, touched);
      await tx.putMeta(meta);
    });
  }

  /** The server refused the batch for good: drop it and count it. */
  refuse(batchId: string): Promise<number> {
    return this.store.transaction(async (tx) => {
      const batch = await tx.getBatch(batchId);
      if (!batch) return 0;
      const meta = await tx.meta();
      const touched = new Map<string, Run>();
      await tx.deleteBatch(batch.id);
      meta.counters.batches -= 1;
      meta.counters.queuedItems -= batch.count;
      meta.counters.refusedBatches += 1;
      meta.counters.refusedItems += batch.count;
      await this.touchRun(tx, touched, batch.runId, (run) => (run.queued -= batch.count));
      await this.saveRuns(tx, touched);
      await tx.putMeta(meta);
      return batch.count;
    });
  }

  /**
   * Splits a batch the server found too large (413) into two halves that keep its place in the
   * queue, each with a new key. A single item cannot be split: it is refused.
   */
  split(batchId: string): Promise<boolean> {
    return this.store.transaction(async (tx) => {
      const batch = await tx.getBatch(batchId);
      if (!batch) return false;
      if (batch.count < 2) return false;
      const meta = await tx.meta();
      const half = Math.ceil(batch.items.length / 2);
      const parts = [batch.items.slice(0, half), batch.items.slice(half)];
      await tx.deleteBatch(batch.id);
      for (const [index, items] of parts.entries())
        await tx.putBatch({
          ...batch,
          id: `${batch.id}.${index + 1}`,
          key: this.ulid(),
          items,
          count: items.length,
          bytes: items.reduce((sum, item) => sum + itemBytes(item), 0),
          attempts: 0,
          sentRunId: null,
        });
      meta.counters.batches += 1;
      await tx.putMeta(meta);
      return true;
    });
  }

  /** Gives a batch a new key (the server saw its key with another body). */
  rekey(batchId: string): Promise<void> {
    return this.store.transaction(async (tx) => {
      const batch = await tx.getBatch(batchId);
      if (!batch) return;
      batch.key = this.ulid();
      batch.sentRunId = null;
      await tx.putBatch(batch);
    });
  }

  // ── Runs ──────────────────────────────────────────────────────────────────

  getRun(id: string): Promise<Run | null> {
    return this.store.transaction((tx) => tx.getRun(id));
  }

  runs(): Promise<Run[]> {
    return this.store.transaction((tx) => tx.runs());
  }

  setRunServerId(id: string, serverId: string | null): Promise<void> {
    return this.store.transaction(async (tx) => {
      const run = await tx.getRun(id);
      if (!run) return;
      run.serverId = serverId;
      await tx.putRun(run);
    });
  }

  /** Ends the open runs `match` selects; returns how many it ended. */
  endRuns(match: (run: Run) => boolean, reason: SyncEndReason, at: number): Promise<number> {
    return this.store.transaction(async (tx) => {
      let ended = 0;
      for (const run of await tx.runs()) {
        if (run.state !== 'open' || !match(run)) continue;
        run.state = 'ended';
        run.stopReason = reason;
        run.endedAt = at;
        await tx.putRun(run);
        ended += 1;
      }
      return ended;
    });
  }

  /** Ended runs whose items have all left the queue: ready for their closing PATCH. */
  runsToClose(): Promise<Run[]> {
    return this.store.transaction(async (tx) =>
      (await tx.runs()).filter((run) => run.state === 'ended' && run.queued <= 0),
    );
  }

  // ── Explicit runs (P2-13) ─────────────────────────────────────────────────

  /** Opens an explicit run (manual, web, scheduled): the sync controller walks it. */
  openRun(spec: RunSpec & Partial<Pick<Run, 'incremental' | 'stopAfterKnown'>>, at: number) {
    return this.store.transaction(async (tx): Promise<Run> => {
      const run: Run = {
        id: this.ulid(),
        serverId: null,
        ...structuredClone(RUN_SYNC_DEFAULTS),
        ...spec,
        state: 'open',
        stopReason: null,
        pages: 0,
        scanned: 0,
        queued: 0,
        createdAt: at,
        lastAt: at,
        endedAt: null,
      };
      await tx.putRun(run);
      return run;
    });
  }

  /**
   * Appends a capture to an open run (an explicit sync's batches): like capturePassive, without
   * finding or opening a run. A run that is gone or ended takes nothing (`run: null`).
   */
  captureToRun(runId: string, input: CaptureInput): Promise<CaptureResult> {
    return this.store.transaction(async (tx) => {
      const meta = await tx.meta();
      if (input.messageId && meta.recent.includes(input.messageId))
        return { run: null, accepted: 0, dropped: 0, duplicate: true, full: false };
      const run = await tx.getRun(runId);
      if (!run || run.state !== 'open')
        return { run: null, accepted: 0, dropped: 0, duplicate: false, full: false };
      const touched = new Map<string, Run>([[run.id, run]]);
      const result = await this.appendTo(tx, meta, touched, run, input);
      await this.saveRuns(tx, touched);
      await tx.putMeta(meta);
      return { run, duplicate: false, ...result };
    });
  }

  /** Changes a run in one transaction; returns it as saved, or null when it is gone. */
  updateRun(id: string, change: (run: Run) => void): Promise<Run | null> {
    return this.store.transaction(async (tx) => {
      const run = await tx.getRun(id);
      if (!run) return null;
      change(run);
      await tx.putRun(run);
      return run;
    });
  }

  acceptedRecords(): Promise<RunKeys[]> {
    return this.store.transaction((tx) => tx.allRunKeys());
  }

  /** The keys the server accepted for a run, in the last 7 days (P2-19's run report). */
  runKeys(runId: string): Promise<RunKeys[]> {
    return this.store.transaction((tx) => tx.runKeys(runId));
  }

  /** Forgets accepted keys older than `before`. */
  pruneRunKeys(before: number): Promise<number> {
    return this.store.transaction((tx) => tx.pruneRunKeys(before));
  }

  forgetRun(id: string): Promise<void> {
    return this.store.transaction((tx) => tx.deleteRun(id));
  }

  /** Removes a run the server refused, with its queued items (counted as refused). */
  dropRun(id: string): Promise<number> {
    return this.store.transaction(async (tx) => {
      const meta = await tx.meta();
      let removed = 0;
      for (const [group, info] of Object.entries(meta.groups)) {
        if (info.runId !== id) continue;
        for (const chunk of await tx.groupChunks(group)) {
          if (chunk.seq !== undefined) await tx.deleteChunk(chunk.seq);
          meta.counters.pendingItems -= chunk.count;
          meta.counters.queuedItems -= chunk.count;
          removed += chunk.count;
        }
        delete meta.groups[group];
      }
      for (const batchId of await tx.runBatchIds(id)) {
        const batch = await tx.getBatch(batchId);
        if (!batch) continue;
        await tx.deleteBatch(batchId);
        meta.counters.batches -= 1;
        meta.counters.queuedItems -= batch.count;
        meta.counters.refusedBatches += 1;
        removed += batch.count;
      }
      meta.counters.refusedItems += removed;
      await tx.deleteRun(id);
      await tx.putMeta(meta);
      return removed;
    });
  }

  // ── State ─────────────────────────────────────────────────────────────────

  snapshot(): Promise<QueueSnapshot> {
    return this.store.transaction(async (tx) => ({
      counters: (await tx.meta()).counters,
      runs: await tx.runs(),
    }));
  }

  /** True when chunks wait to be sealed. */
  hasPending(): Promise<boolean> {
    return this.store.transaction(async (tx) => Object.keys((await tx.meta()).groups).length > 0);
  }
}
