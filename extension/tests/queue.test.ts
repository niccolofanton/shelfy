// The offline queue and its batcher (sw/queue/queue.ts) on the in-memory store: runs per listing
// visit, sealing at 500 items / the byte cap / 2 s, the 50,000-item cap (oldest dropped and
// counted), stable Idempotency-Keys, and the bookkeeping the uploader relies on.

import { describe, expect, it } from 'vitest';
import { isUlid } from '../src/shared/ulid';
import { prefilterBatch, type WireItem } from '../src/sw/prefilter';
import type { RunSpec } from '../src/sw/queue/queue';
import { T0, harness, igItem } from './helpers';

const items = (from: number, count: number, extra: Record<string, unknown> = {}): WireItem[] =>
  prefilterBatch(
    Array.from({ length: count }, (_, i) => igItem(from + i, extra)),
    'instagram',
  ).items;

const visit = (overrides: Partial<RunSpec> = {}): RunSpec => ({
  platform: 'instagram',
  trigger: 'passive',
  listing: { kind: 'ig_saved', externalId: null, name: null },
  collection: { mode: 'none' },
  tabId: 7,
  docId: 'doc-a',
  listingKey: 'instagram:ig_saved',
  ...overrides,
});

const input = (batch: WireItem[], at: number, messageId: string | null = null) => ({
  source: 'passive' as const,
  hasNextPage: true,
  items: batch,
  at,
  messageId,
});

describe('runs: one passive run per listing visit', () => {
  it('keeps B items in a new run when pairing changes on the same tab, document and listing', async () => {
    const { queue } = harness();
    const first = await queue.capturePassive(
      visit({ accountTokenId: 'account-A' }),
      input(items(1, 1), T0, 'doc-a:0'),
    );
    const second = await queue.capturePassive(
      visit({ accountTokenId: 'account-B' }),
      input(items(2, 1), T0 + 1, 'doc-a:1'),
    );
    expect(second.run?.id).not.toBe(first.run?.id);
    expect(await queue.getRun(first.run!.id)).toMatchObject({
      accountTokenId: 'account-A',
      state: 'ended',
      queued: 1,
    });
    expect(await queue.getRun(second.run!.id)).toMatchObject({
      accountTokenId: 'account-B',
      state: 'open',
      queued: 1,
    });
    await queue.dropRun(first.run!.id);
    expect(await queue.getRun(second.run!.id)).toMatchObject({
      accountTokenId: 'account-B',
      queued: 1,
    });
    expect((await queue.snapshot()).counters.queuedItems).toBe(1);
  });

  it('reuses the run of the same tab, document and listing', async () => {
    const { queue } = harness();
    const first = await queue.capturePassive(visit(), input(items(1, 3), T0, 'doc-a:0'));
    const second = await queue.capturePassive(visit(), input(items(4, 2), T0 + 10, 'doc-a:1'));
    expect(second.run?.id).toBe(first.run?.id);
    const [run] = await queue.runs();
    expect(run).toMatchObject({
      state: 'open',
      pages: 2,
      scanned: 5,
      queued: 5,
      trigger: 'passive',
    });
  });

  it('ends the previous run when the tab shows another listing or reloads', async () => {
    const { queue } = harness();
    const saved = await queue.capturePassive(visit(), input(items(1, 1), T0));
    const folder = await queue.capturePassive(
      visit({
        listing: { kind: 'ig_collection', externalId: '1789', name: 'Recipes' },
        listingKey: 'instagram:ig_collection:1789',
        collection: { mode: 'auto' },
      }),
      input(items(2, 1), T0 + 5),
    );
    const reloaded = await queue.capturePassive(
      visit({
        docId: 'doc-b',
        listing: { kind: 'ig_collection', externalId: '1789', name: 'Recipes' },
        listingKey: 'instagram:ig_collection:1789',
      }),
      input(items(3, 1), T0 + 9),
    );
    const otherTab = await queue.capturePassive(visit({ tabId: 8 }), input(items(4, 1), T0 + 12));
    const runs = new Map((await queue.runs()).map((run) => [run.id, run]));
    expect(runs.get(saved.run!.id)).toMatchObject({
      state: 'ended',
      stopReason: 'user',
      endedAt: T0 + 5,
    });
    expect(runs.get(folder.run!.id)).toMatchObject({ state: 'ended', endedAt: T0 + 9 });
    expect(runs.get(reloaded.run!.id)?.state).toBe('open');
    expect(runs.get(otherTab.run!.id)?.state).toBe('open');
  });

  it('ignores a delivery it already queued (the relay retried it)', async () => {
    const { queue } = harness();
    await queue.capturePassive(visit(), input(items(1, 3), T0, 'doc-a:0'));
    const again = await queue.capturePassive(visit(), input(items(1, 3), T0 + 1, 'doc-a:0'));
    expect(again).toMatchObject({ duplicate: true, accepted: 0, run: null });
    expect((await queue.snapshot()).counters.queuedItems).toBe(3);
  });

  it('ends runs by rule, and lists ended runs whose items all left the queue', async () => {
    const { queue } = harness();
    const { run } = await queue.capturePassive(visit(), input(items(1, 2), T0));
    expect(await queue.runsToClose()).toEqual([]);
    expect(await queue.endRuns((r) => r.tabId === 7, 'user', T0 + 1)).toBe(1);
    expect(await queue.runsToClose()).toEqual([]); // two items still queued
    await queue.sealDue(T0 + 2_000);
    const batch = await queue.nextBatch();
    await queue.complete(
      batch!.id,
      { inserted: 2, updated: 0, known: 0, results: [], rejected: [] },
      T0 + 3_000,
    );
    expect((await queue.runsToClose()).map((r) => r.id)).toEqual([run!.id]);
  });
});

describe('batcher: ≤ 500 items or 2 s per batch', () => {
  it('seals a group 2 s after its first item, into one batch in arrival order', async () => {
    const { queue } = harness();
    await queue.capturePassive(visit(), { ...input(items(1, 2), T0), hasNextPage: true });
    await queue.capturePassive(visit(), { ...input(items(3, 2), T0 + 500), hasNextPage: false });
    expect(await queue.sealDue(T0 + 1_999)).toEqual({ sealed: 0, nextDueAt: T0 + 2_000 });
    expect(await queue.nextBatch()).toBeNull();
    expect(await queue.sealDue(T0 + 2_000)).toEqual({ sealed: 1, nextDueAt: null });
    const batch = await queue.nextBatch();
    expect(batch).toMatchObject({
      platform: 'instagram',
      source: 'passive',
      count: 4,
      hasNextPage: false,
      attempts: 0,
      sentRunId: null,
      client: { ext: '0.2.0', parser: 'test-parser' },
    });
    expect(isUlid(batch!.id) && isUlid(batch!.key)).toBe(true);
    expect(batch!.key).not.toBe(batch!.id);
    expect(batch!.items.map((item) => item.text)).toEqual([
      'Synthetic caption 1',
      'Synthetic caption 2',
      'Synthetic caption 3',
      'Synthetic caption 4',
    ]);
    expect((await queue.snapshot()).counters).toMatchObject({
      queuedItems: 4,
      pendingItems: 0,
      batches: 1,
    });
  });

  it('splits 1,200 items into batches of 500, 500 and 200, and flags a full group', async () => {
    const { queue } = harness();
    const result = await queue.capturePassive(visit(), input(items(1, 1_000), T0));
    expect(result.full).toBe(true);
    await queue.capturePassive(visit(), input(items(1_001, 200), T0 + 1));
    await queue.sealDue(T0 + 1);
    const counts: number[] = [];
    for (let batch = await queue.nextBatch(); batch; batch = await queue.nextBatch()) {
      counts.push(batch.count);
      await queue.complete(
        batch.id,
        { inserted: batch.count, updated: 0, known: 0, results: [], rejected: [] },
        T0 + 2,
      );
    }
    expect(counts).toEqual([500, 500, 200]);
  });

  it('keeps every batch under the byte cap', async () => {
    const { queue, queueStore } = harness({ maxBatchBytes: 4_000 });
    await queue.capturePassive(visit(), input(items(1, 12, { text: 'x'.repeat(900) }), T0));
    await queue.sealDue(T0 + 2_000);
    const { batches } = queueStore.dump();
    expect(batches.length).toBeGreaterThan(2);
    for (const batch of batches) expect(batch.bytes).toBeLessThanOrEqual(4_000);
    expect(batches.reduce((sum, batch) => sum + batch.count, 0)).toBe(12);
  });

  it('keeps sources and runs apart', async () => {
    const { queue, queueStore } = harness();
    await queue.capturePassive(visit(), input(items(1, 2), T0));
    await queue.capturePassive(visit({ tabId: 9 }), input(items(3, 2), T0));
    await queue.sealDue(T0 + 2_000);
    expect(queueStore.dump().batches.map((batch) => batch.count)).toEqual([2, 2]);
  });
});

describe('the 50,000-item cap', () => {
  it('drops the oldest batches first, then the oldest pending chunks, and counts them', async () => {
    const { queue } = harness({ maxItems: 10 });
    await queue.capturePassive(visit(), input(items(1, 6), T0));
    await queue.sealDue(T0 + 2_000);
    const result = await queue.capturePassive(visit(), input(items(7, 6), T0 + 2_001));
    expect(result.dropped).toBe(6);
    let { counters } = await queue.snapshot();
    expect(counters).toMatchObject({
      queuedItems: 6,
      droppedItems: 6,
      batches: 0,
      pendingItems: 6,
    });
    // Only pending chunks left: the oldest chunk goes.
    await queue.capturePassive(visit(), input(items(13, 5), T0 + 2_002));
    ({ counters } = await queue.snapshot());
    expect(counters).toMatchObject({ queuedItems: 5, droppedItems: 12 });
    const [run] = await queue.runs();
    expect(run.queued).toBe(5);
  });

  it('defaults to 50,000 items, 500 per batch, 2 s', () => {
    const { queue } = harness();
    expect(queue.options).toMatchObject({ maxItems: 50_000, maxBatchItems: 500, windowMs: 2_000 });
  });
});

describe('sending bookkeeping', () => {
  async function sealedBatch() {
    const h = harness();
    await h.queue.capturePassive(visit(), input(items(1, 4), T0));
    await h.queue.sealDue(T0 + 2_000);
    const batch = (await h.queue.nextBatch())!;
    return { ...h, batch };
  }

  it('keeps the Idempotency-Key across retries of the same body', async () => {
    const { queue, batch } = await sealedBatch();
    const first = await queue.prepareSend(batch.id, 'run-1');
    const second = await queue.prepareSend(batch.id, 'run-1');
    expect(first?.key).toBe(batch.key);
    expect(second).toMatchObject({ key: batch.key, attempts: 2, sentRunId: 'run-1' });
  });

  it('takes a new key when the body must name another run', async () => {
    const { queue, batch } = await sealedBatch();
    await queue.prepareSend(batch.id, 'run-1');
    const moved = await queue.prepareSend(batch.id, 'run-2');
    expect(moved?.key).not.toBe(batch.key);
    expect(moved?.sentRunId).toBe('run-2');
  });

  it('completes, refuses, splits and re-keys', async () => {
    const { queue, queueStore, batch } = await sealedBatch();
    expect(await queue.split(batch.id)).toBe(true);
    const halves = queueStore.dump().batches;
    expect(halves.map((b) => [b.id, b.count])).toEqual([
      [`${batch.id}.1`, 2],
      [`${batch.id}.2`, 2],
    ]);
    expect(new Set(halves.map((b) => b.key)).size).toBe(2);
    expect(halves.every((b) => b.key !== batch.key)).toBe(true);

    await queue.rekey(halves[0].id);
    expect(queueStore.dump().batches[0].key).not.toBe(halves[0].key);

    await queue.complete(
      halves[0].id,
      { inserted: 1, updated: 0, known: 1, results: [], rejected: [{ index: 1, code: 'bad_id' }] },
      T0 + 3_000,
    );
    expect(await queue.refuse(halves[1].id)).toBe(2);
    const { counters } = await queue.snapshot();
    expect(counters).toMatchObject({
      queuedItems: 0,
      batches: 0,
      sentBatches: 1,
      sentItems: 2,
      inserted: 1,
      known: 1,
      rejected: 1,
      refusedBatches: 1,
      refusedItems: 2,
      lastSentAt: T0 + 3_000,
    });
  });

  it('drops a run the server refuses, with everything it still has queued', async () => {
    const { queue, batch } = await sealedBatch();
    await queue.capturePassive(visit(), input(items(10, 3), T0 + 2_500));
    expect(await queue.dropRun(batch.runId)).toBe(7);
    const { counters, runs } = await queue.snapshot();
    expect(runs).toEqual([]);
    expect(counters).toMatchObject({
      queuedItems: 0,
      pendingItems: 0,
      batches: 0,
      refusedItems: 7,
    });
  });

  it('a transaction that fails leaves nothing behind', async () => {
    const { queueStore } = harness();
    await expect(
      queueStore.transaction(async (tx) => {
        const meta = await tx.meta();
        meta.counters.queuedItems = 99;
        await tx.putMeta(meta);
        throw new Error('boom');
      }),
    ).rejects.toThrow('boom');
    expect(queueStore.dump().meta.counters.queuedItems).toBe(0);
  });
});
