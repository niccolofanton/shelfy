import { describe, expect, it } from 'vitest';
import { buildExport, exportFileName, parseExportFile } from '../src/export-format';
import {
  RUNTIME,
  type BatchMessage,
  type CaptureSource,
  type JsonRecord,
  type Platform,
} from '../src/protocol';
import {
  LOG_LIMIT,
  appendLog,
  applyBatch,
  applyCensus,
  applyMarker,
  emptyMeta,
  isStoredItem,
  prepareBatch,
  readMeta,
  recordRefusal,
  type LogEntry,
  type Meta,
  type StoredItem,
} from '../src/store';

const T0 = 1_760_000_000_000;
const IG_SAVED = 'https://www.instagram.com/someone/saved/all-posts/';
const IG_FOLDER = 'https://www.instagram.com/someone/saved/recipes/17890000000000001/';

function igItem(pk: string, url: string, extra: JsonRecord = {}): JsonRecord {
  return {
    id: `${pk}_9000000001`,
    shortcode: '',
    postUrl: '',
    mediaType: 'image',
    timestamp: '2025-09-16T05:20:00.000Z',
    thumbnailUrl: url,
    media: [{ type: 'image', url }],
    ...extra,
  };
}

function message(
  platform: Platform,
  pageUrl: string,
  items: JsonRecord[],
  source: CaptureSource = 'passive',
  hasNextPage: boolean | null = true,
): BatchMessage {
  return { kind: RUNTIME.batch, platform, items, hasNextPage, pageUrl, source, sentAt: T0 };
}

class Store {
  meta: Meta = emptyMeta(T0);
  items = new Map<string, StoredItem>();
  log: LogEntry[] = [];

  ingest(batch: BatchMessage, at: number) {
    const result = applyBatch(this.meta, this.items, prepareBatch(batch, at), at);
    this.meta = result.meta;
    for (const item of result.changed) this.items.set(item.key, item);
    this.log = appendLog(this.log, [result.entry]);
    return result;
  }
}

const cdn = (name: string, oe = '68F00000'): string =>
  `https://scontent-synth1-1.cdninstagram.com/v/${name}.jpg?oh=00_SYNTH&oe=${oe}`;

describe('applyBatch', () => {
  it('discards batches captured outside a saved listing (scoping rule)', () => {
    const store = new Store();
    const result = store.ingest(
      message('instagram', 'https://www.instagram.com/explore/', [
        igItem('3400000000000000001', cdn('a')),
      ]),
      T0,
    );
    expect(result.changed).toEqual([]);
    expect(result.entry).toMatchObject({ kind: 'discard', reason: 'out_of_scope', received: 1 });
    expect(store.meta.platforms.instagram).toMatchObject({
      discardedBatches: 1,
      discardedItems: 1,
      uniqueItems: 0,
      batches: 0,
    });
    expect(store.meta.listings).toEqual({});
  });

  it('rejects items the desktop sanitizer or the canonical key refuses', () => {
    const store = new Store();
    const result = store.ingest(
      message('instagram', IG_SAVED, [
        igItem('3400000000000000001', cdn('a')),
        { id: '', mediaType: 'image' },
        { id: 'x'.repeat(300) },
        { id: '!!!', shortcode: '', postUrl: '' },
        'not an object' as unknown as JsonRecord,
      ]),
      T0,
    );
    expect(result.entry).toMatchObject({ kind: 'batch', received: 5, accepted: 1, rejected: 4 });
    expect(store.meta.platforms.instagram.rejectedItems).toBe(4);
  });

  it('drops non-http media and keeps only http(s) URLs', () => {
    const store = new Store();
    store.ingest(
      message('instagram', IG_SAVED, [
        igItem('3400000000000000001', 'javascript:alert(1)', {
          media: [
            { type: 'image', url: 'data:image/png;base64,AAAA' },
            { type: 'video', url: cdn('ok') },
          ],
        }),
      ]),
      T0,
    );
    const item = store.items.get('ig_3400000000000000001');
    expect(item?.media.map((m) => [m.slot, m.position, m.type])).toEqual([['slide', 0, 'video']]);
    expect(item?.mediaCount).toBe(1);
  });

  it('merges recaptures: listings, sources, raw ids and the latest media URL', () => {
    const store = new Store();
    store.ingest(
      message('instagram', IG_SAVED, [igItem('3400000000000000001', cdn('a', '68F00000'))]),
      T0,
    );
    store.ingest(
      message(
        'instagram',
        IG_FOLDER,
        [{ ...igItem('3400000000000000001', cdn('a2', '69000000')), id: '3400000000000000001' }],
        'replay',
        false,
      ),
      T0 + 1000,
    );
    const item = store.items.get('ig_3400000000000000001');
    expect(item).toMatchObject({
      rawIds: ['3400000000000000001_9000000001', '3400000000000000001'],
      firstCapturedAt: T0,
      lastCapturedAt: T0 + 1000,
      captureCount: 2,
    });
    expect(item?.listings).toEqual({
      'instagram:ig_saved': { firstAt: T0, lastAt: T0, sources: ['passive'] },
      'instagram:ig_collection:17890000000000001': {
        firstAt: T0 + 1000,
        lastAt: T0 + 1000,
        sources: ['replay'],
      },
    });
    expect(item?.media[0]).toMatchObject({
      slot: 'cover',
      url: cdn('a2', '69000000'),
      expiresAt: 0x69000000 * 1000,
      capturedAt: T0 + 1000,
      observations: 2,
    });
    expect(store.meta.platforms.instagram.uniqueItems).toBe(1);
    expect(store.meta.listings['instagram:ig_collection:17890000000000001']).toMatchObject({
      uniqueItems: 1,
      endOfFeedSeen: true,
      lastHasNextPage: false,
      bySource: { passive: 0, replay: 1, ssr: 0, dom: 0 },
    });
    expect(store.log.map((e) => e.kind === 'batch' && [e.newItems, e.newInListing])).toEqual([
      [1, 1],
      [0, 1],
    ]);
  });

  it('counts a post repeated inside one batch once', () => {
    const store = new Store();
    const result = store.ingest(
      message('twitter', 'https://x.com/i/bookmarks', [
        { id: '1800000000000000001', media: [] },
        {
          id: '1800000000000000001',
          media: [{ type: 'image', url: 'https://pbs.twimg.com/media/a.jpg' }],
        },
      ]),
      T0,
    );
    expect(result.changed).toHaveLength(1);
    expect(result.entry).toMatchObject({ newItems: 1, newInListing: 1, accepted: 2 });
    expect(store.items.get('x_1800000000000000001')?.captureCount).toBe(2);
  });

  it('records IG shortcode/pk mismatches as a diagnostic', () => {
    const store = new Store();
    store.ingest(
      message('instagram', IG_SAVED, [
        igItem('3400000000000000009', cdn('a'), { shortcode: 'C8vOfxsVAAB' }),
      ]),
      T0,
    );
    expect(store.meta.diagnostics.igShortcodeMismatch).toBe(1);
  });

  it('does not mutate the meta it was given', () => {
    const meta = emptyMeta(T0);
    const snapshot = JSON.stringify(meta);
    applyBatch(
      meta,
      new Map(),
      prepareBatch(message('twitter', 'https://x.com/i/bookmarks', [{ id: '1' }]), T0),
      T0,
    );
    expect(JSON.stringify(meta)).toBe(snapshot);
  });
});

describe('census, markers, refusals and the log', () => {
  it('aggregates census counts per platform and endpoint, splitting in-scope requests', () => {
    let meta = emptyMeta(T0);
    meta = applyCensus(meta, { 'instagram|graphql SyntheticQuery': 3 }, IG_SAVED, T0);
    meta = applyCensus(
      meta,
      { 'instagram|graphql SyntheticQuery': 2 },
      'https://www.instagram.com/',
      T0,
    );
    meta = applyCensus(
      meta,
      { 'twitter|graphql Bookmarks': 1, 'web|x': 9 },
      'https://x.com/i/bookmarks',
      T0,
    );
    expect(meta.platforms.instagram.census).toEqual({
      'graphql SyntheticQuery': { requests: 5, inScope: 3 },
    });
    expect(meta.platforms.twitter.census).toEqual({
      'graphql Bookmarks': { requests: 1, inScope: 1 },
    });
  });

  it('logs replay markers and counts refused messages', () => {
    const { meta, entry } = applyMarker(
      emptyMeta(T0),
      {
        kind: RUNTIME.marker,
        phase: 'end',
        source: 'replay',
        id: 'r1',
        detail: { pages: 3, reason: 'end_of_feed' },
        pageUrl: IG_SAVED,
        sentAt: T0,
      },
      'instagram',
      T0,
    );
    expect(entry).toMatchObject({
      kind: 'marker',
      phase: 'end',
      path: '/someone/saved/all-posts/',
      seq: 1,
    });
    expect(meta.logSeq).toBe(1);
    const refused = recordRefusal(
      recordRefusal(meta, 'platform_host_mismatch', T0),
      'platform_host_mismatch',
      T0,
    );
    expect(refused.diagnostics.refusedBatches).toEqual({ platform_host_mismatch: 2 });
  });

  it('caps the log and tolerates garbage in storage', () => {
    const entries = Array.from(
      { length: LOG_LIMIT + 10 },
      (_, i) => ({ seq: i }) as unknown as LogEntry,
    );
    const log = appendLog('garbage', entries);
    expect(log).toHaveLength(LOG_LIMIT);
    expect(log[0]).toEqual({ seq: 10 });
    expect(readMeta({ schema: 999 }, T0)).toEqual(emptyMeta(T0));
    expect(isStoredItem({ key: 'x_1', platform: 'twitter', listings: {}, media: [] })).toBe(true);
    expect(isStoredItem({ key: 'x_1', platform: 'web', listings: {}, media: [] })).toBe(false);
  });
});

describe('export', () => {
  it('builds a versioned export that compare.ts accepts, sorted by first capture', () => {
    const store = new Store();
    store.ingest(
      message('twitter', 'https://x.com/i/bookmarks', [{ id: '1800000000000000002' }]),
      T0 + 5,
    );
    store.ingest(
      message('twitter', 'https://x.com/i/bookmarks', [{ id: '1800000000000000001' }]),
      T0 + 1,
    );
    const file = buildExport(store.meta, [...store.items.values()], {
      extensionVersion: '0.1.0',
      userAgent: 'test-agent',
      now: T0 + 10,
    });
    expect(file).toMatchObject({
      format: 'shelfy-spike3-capture',
      version: 1,
      exportedAt: new Date(T0 + 10).toISOString(),
      extension: { version: '0.1.0', userAgent: 'test-agent' },
    });
    expect(file.items.map((i) => i.key)).toEqual([
      'x_1800000000000000001',
      'x_1800000000000000002',
    ]);
    expect(parseExportFile(JSON.parse(JSON.stringify(file))).items).toHaveLength(2);
  });

  it('refuses files that are not extension exports', () => {
    expect(() => parseExportFile({ posts: [] })).toThrow(/not a SPIKE-3 extension export/);
    expect(() =>
      parseExportFile({ format: 'shelfy-spike3-capture', version: 2, items: [], listings: [] }),
    ).toThrow(/unsupported export version/);
    expect(() =>
      parseExportFile({
        format: 'shelfy-spike3-capture',
        version: 1,
        items: [{ key: 'x_1' }],
        listings: [],
        platforms: {},
      }),
    ).toThrow(/item #0 is malformed/);
  });

  it('names exports with a sortable UTC timestamp', () => {
    expect(exportFileName(new Date(Date.UTC(2026, 9, 2, 13, 4, 5)))).toBe(
      'shelfy-spike3-capture-20261002-130405.json',
    );
  });
});
