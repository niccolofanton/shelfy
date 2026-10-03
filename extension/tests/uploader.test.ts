// The uploader (sw/uploader.ts) against the fake Shelfy API: runs opened once and closed after
// their last batch, batches sent with the bearer token, the extension header and their stable
// Idempotency-Key, and every failure path of contract C1 (sw/errors.ts).

import { describe, expect, it } from 'vitest';
import { prefilterBatch } from '../src/sw/prefilter';
import type { RunSpec } from '../src/sw/queue/queue';
import { T0, harness, igItem, type Harness } from './helpers';

const visit: RunSpec = {
  platform: 'instagram',
  trigger: 'passive',
  listing: { kind: 'ig_collection', externalId: '17890000000000001', name: 'Recipes' },
  collection: { mode: 'auto' },
  tabId: 3,
  docId: 'doc-1',
  listingKey: 'instagram:ig_collection:17890000000000001',
};

async function capture(h: Harness, from: number, count: number, messageId?: string) {
  const { items } = prefilterBatch(
    Array.from({ length: count }, (_, i) => igItem(from + i)),
    'instagram',
  );
  return h.queue.capturePassive(
    { ...visit, accountTokenId: (await h.store.pairing())?.tokenId },
    {
      source: 'passive',
      hasNextPage: true,
      items,
      at: h.clock.now,
      messageId: messageId ?? `doc-1:${from}`,
    },
  );
}

/** Moves the clock past the batch window and flushes. */
async function flushLater(h: Harness, ms = 2_000, force = false): Promise<void> {
  h.clock.now += ms;
  await h.uploader.flush({ force });
}

const ingests = (h: Harness) => h.api.log.filter((r) => r.path === '/api/v1/ingest/batches');

describe('uploader', () => {
  it('discards unbound IDB records after upgrade instead of adopting their collection on account B', async () => {
    const h = harness();
    await h.pairNow();
    const pairingA = (await h.store.pairing())!;
    const legacy = await h.queue.capturePassive(
      { ...visit, collection: { mode: 'existing', id: 7 } },
      {
        source: 'passive',
        hasNextPage: true,
        items: prefilterBatch([igItem(1)], 'instagram').items,
        at: T0,
        messageId: 'legacy-A',
      },
    );
    await h.store.setPairing({ ...pairingA, token: h.api.mintToken(), tokenId: 'account-B' });
    await flushLater(h);
    expect(h.api.log).toEqual([]);
    expect(await h.queue.getRun(legacy.run!.id)).toBeNull();
    expect((await h.queue.snapshot()).counters.queuedItems).toBe(0);
    expect((await h.store.pairing())?.tokenId).toBe('account-B');
  });

  it('uploads B captures after discarding A on the same tab and listing', async () => {
    const h = harness();
    await h.pairNow();
    const first = await capture(h, 1, 1);
    const pairingA = (await h.store.pairing())!;
    const tokenB = h.api.mintToken();
    await h.store.setPairing({ ...pairingA, token: tokenB, tokenId: 'account-B' });
    const second = await capture(h, 2, 1);
    expect(second.run!.id).not.toBe(first.run!.id);
    await flushLater(h);
    await h.uploader.flush();
    expect(await h.queue.getRun(first.run!.id)).toBeNull();
    expect(h.api.ingests).toHaveLength(1);
    expect(ingests(h)[0].authorization).toBe(`Bearer ${tokenB}`);
    expect([...h.api.posts.keys()]).toEqual(['ig_3400000000000000002']);
    expect((await h.queue.snapshot()).counters.queuedItems).toBe(0);
  });

  it('sends nothing while unpaired, and keeps the items', async () => {
    const h = harness();
    await capture(h, 1, 3);
    await flushLater(h);
    expect(h.api.log).toEqual([]);
    expect((await h.queue.snapshot()).counters).toMatchObject({ queuedItems: 3, batches: 1 });
  });

  it('waits out the 2 s window, then opens the run once and sends with token, version and key', async () => {
    const h = harness();
    const token = await h.pairNow();
    await capture(h, 1, 3);
    await h.uploader.flush();
    expect(h.api.log).toEqual([]);
    expect(h.wakes).toContain(T0 + 2_000);

    await capture(h, 4, 2);
    await flushLater(h);
    expect(h.api.log.map((r) => [r.method, r.path, r.status])).toEqual([
      ['POST', '/api/v1/sync-runs', 201],
      ['POST', '/api/v1/ingest/batches', 200],
    ]);
    const [ingest] = ingests(h);
    expect(ingest.authorization).toBe(`Bearer ${token}`);
    expect(ingest.extension).toBe('0.2.0');
    expect(ingest.idempotencyKey).toMatch(/^[0-9A-HJKMNP-TV-Z]{26}$/);
    const run = [...h.api.runs.values()][0];
    expect(run).toMatchObject({
      platform: 'instagram',
      trigger: 'passive',
      listing: { kind: 'ig_collection', externalId: '17890000000000001', name: 'Recipes' },
      collection: { mode: 'auto' },
    });
    expect(h.api.ingests[0]).toMatchObject({
      runId: run.id,
      source: 'passive',
      count: 5,
      client: { ext: '0.2.0', parser: 'test-parser' },
    });
    expect(h.api.posts.size).toBe(5);
    const { counters } = await h.queue.snapshot();
    expect(counters).toMatchObject({ queuedItems: 0, sentItems: 5, inserted: 5, sentBatches: 1 });
    expect((await h.store.status()).lastOkAt).toBe(h.clock.now);
  });

  it('closes an ended run with its counters once its last batch is in', async () => {
    const h = harness();
    await h.pairNow();
    await capture(h, 1, 3);
    await capture(h, 4, 3);
    await h.queue.endRuns(() => true, 'user', h.clock.now);
    await flushLater(h);
    expect(h.api.patches).toEqual([
      {
        id: 'run-1',
        body: {
          state: 'done',
          pages: 2,
          scanned: 6,
          stopReason: 'user',
          resumeCursor: null,
          errorCode: null,
        },
      },
    ]);
    expect(await h.queue.runs()).toEqual([]);
  });

  it('with the API down, holds the batch and backs off; once it is back, sends it once', async () => {
    const h = harness();
    await h.pairNow();
    await capture(h, 1, 4);
    h.network.down = true;
    await flushLater(h);
    let status = await h.store.status();
    expect(status).toMatchObject({ failures: 1, lastError: { code: 'network' } });
    expect(status.blockedUntil).toBe(h.clock.now + 1_500);
    expect(h.wakes.at(-1)).toBe(status.blockedUntil);

    await flushLater(h, 1_000); // still inside the backoff: nothing is tried
    expect((await h.store.status()).failures).toBe(1);
    await flushLater(h, 600); // past it, still down: the backoff grows
    status = await h.store.status();
    expect(status.failures).toBe(2);
    expect(status.blockedUntil).toBe(h.clock.now + 3_000);
    expect((await h.queue.snapshot()).counters.queuedItems).toBe(4);

    h.network.down = false;
    await flushLater(h, 3_000);
    expect(ingests(h)).toHaveLength(1);
    expect(h.api.posts.size).toBe(4);
    expect([...h.api.posts.values()].every((times) => times === 1)).toBe(true);
    status = await h.store.status();
    expect(status).toMatchObject({ failures: 0, blockedUntil: 0, lastError: null });
  });

  it('a batch whose answer was lost is sent again under the same key and ingested once', async () => {
    const h = harness();
    await h.pairNow();
    await capture(h, 1, 5);
    h.api.loseNextIngestResponse = true;
    await flushLater(h);
    expect((await h.store.status()).lastError?.code).toBe('server');
    await flushLater(h, 5_000);
    const keys = ingests(h).map((r) => r.idempotencyKey);
    expect(keys).toHaveLength(2);
    expect(keys[0]).toBe(keys[1]);
    expect(h.api.ingests.map((i) => i.replayed)).toEqual([false, true]);
    expect([...h.api.posts.values()]).toEqual([1, 1, 1, 1, 1]);
    expect((await h.queue.snapshot()).counters).toMatchObject({ queuedItems: 0, inserted: 5 });
  });

  it('waits for Retry-After on 503 and 429 (30 s when the server gives none)', async () => {
    const h = harness();
    await h.pairNow();
    await capture(h, 1, 1);
    h.api.failWith = { status: 503, code: 'unavailable', retryAfter: '7' };
    await flushLater(h);
    expect((await h.store.status()).blockedUntil).toBe(h.clock.now + 7_000);
    h.api.failWith = { status: 429, code: 'rate_limited' };
    await flushLater(h, 7_000);
    expect((await h.store.status()).blockedUntil).toBe(h.clock.now + 30_000);
    expect((await h.store.status()).lastError?.code).toBe('rate_limited');
  });

  it('401: forgets the token and keeps the queue', async () => {
    const h = harness();
    await h.pairNow();
    await capture(h, 1, 2);
    h.api.revokeAll();
    await flushLater(h);
    expect(await h.store.pairing()).toBeNull();
    expect((await h.store.status()).lastError?.code).toBe('unauthorized');
    expect((await h.queue.snapshot()).counters.queuedItems).toBe(2);
  });

  it('426: marks the extension outdated and stops sending', async () => {
    const h = harness();
    await h.pairNow();
    await capture(h, 1, 2);
    h.api.minVersion = '0.3.0';
    await flushLater(h);
    expect((await h.store.status()).outdated).toBe(true);
    const sent = h.api.log.length;
    await flushLater(h, 60_000, true);
    expect(h.api.log.length).toBe(sent);
  });

  it('409 source_disabled: drops the batch and learns the kill switch from the config', async () => {
    const h = harness();
    await h.pairNow();
    await capture(h, 1, 2);
    h.api.killed.add('instagram');
    h.api.bumpConfig();
    await flushLater(h);
    const { counters } = await h.queue.snapshot();
    expect(counters).toMatchObject({ queuedItems: 0, refusedItems: 2 });
    expect((await h.config.current()).platforms.instagram.passive).toBe(false);
    expect((await h.store.status()).lastError?.code).toBe('source_disabled');
  });

  it('404 sync_run_not_found: opens the run again and resends under a new key', async () => {
    const h = harness();
    await h.pairNow();
    await capture(h, 1, 2);
    await flushLater(h);
    await capture(h, 3, 2);
    h.api.forgetRuns = true;
    await flushLater(h);
    const sent = ingests(h);
    expect(sent.map((r) => r.status)).toEqual([200, 404, 200]);
    expect(sent[1].idempotencyKey).not.toBe(sent[2].idempotencyKey);
    expect(h.api.ingests.at(-1)?.runId).toBe('run-2');
    expect(h.api.posts.size).toBe(4);
  });

  it('413: splits the batch in two and sends both halves', async () => {
    const h = harness();
    await h.pairNow();
    await capture(h, 1, 6);
    h.api.maxBatchItems = 3;
    await flushLater(h);
    expect(ingests(h).map((r) => r.status)).toEqual([413, 200, 200]);
    expect(h.api.posts.size).toBe(6);
  });

  it('an Access redirect waits at least a minute; the service-token headers get through', async () => {
    const h = harness();
    await h.pairNow();
    await capture(h, 1, 2);
    h.api.requireAccess = { clientId: 'synthetic-id.access', clientSecret: 'synthetic-secret' };
    await flushLater(h);
    const status = await h.store.status();
    expect(status.lastError?.code).toBe('access_redirect');
    expect(status.blockedUntil - h.clock.now).toBeGreaterThanOrEqual(60_000);
    await h.store.patchSettings({ access: h.api.requireAccess });
    await flushLater(h, 1, true);
    expect(ingests(h).at(-1)).toMatchObject({ status: 200, accessId: 'synthetic-id.access' });
    expect(h.api.posts.size).toBe(2);
  });

  it('runs one flush at a time', async () => {
    const h = harness();
    await h.pairNow();
    await capture(h, 1, 2);
    h.clock.now += 2_000;
    await Promise.all([h.uploader.flush(), h.uploader.flush(), h.uploader.flush()]);
    expect(ingests(h)).toHaveLength(1);
    expect(h.api.log.filter((r) => r.path === '/api/v1/sync-runs')).toHaveLength(1);
  });
});
