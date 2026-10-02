// @vitest-environment jsdom
//
// End-to-end relay on synthetic platform responses: the real desktop hook
// (electron/webview-injected.ts, bundled by hook.main.ts) parses page responses and posts them
// with its postMessage fallback, the real bridge forwards them to a fake chrome.runtime, and
// the service worker's pure pipeline (prepareBatch → applyBatch → buildExport) stores them.

import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';
import igGraphqlLegacy from './fixtures/ig-graphql-legacy.json';
import igRestPage1 from './fixtures/ig-saved-rest-page1.json';
import igRestPage2 from './fixtures/ig-saved-rest-page2.json';
import pinterestBoardFeed from './fixtures/pinterest-board-feed.json';
import xBookmarks from './fixtures/x-bookmarks.json';
import { buildExport, parseExportFile } from '../src/export-format';
import { RUNTIME, parseBatchMessage, type BatchMessage, type MarkerMessage } from '../src/protocol';
import { igFeedReplay } from '../src/replay';
import { applyBatch, emptyMeta, prepareBatch, type StoredItem } from '../src/store';

declare const jsdom: { reconfigure(options: { url: string }): void };

const IG_SAVED = 'https://www.instagram.com/someone/saved/all-posts/';
const PIN_BOARD = 'https://www.pinterest.it/someone/recipes/';
const NOW = 1_760_000_000_000;

type RuntimeListener = (
  message: unknown,
  sender: unknown,
  sendResponse: (response: unknown) => void,
) => boolean | void;

const sent: unknown[] = [];
const fetched: Array<{ url: string; init?: RequestInit }> = [];
const runtimeListeners: RuntimeListener[] = [];

function respond(url: string): unknown {
  if (url.includes('/api/v1/feed/saved/posts/'))
    return url.includes('max_id=SYNTHETIC_CURSOR_1') ? igRestPage2 : igRestPage1;
  if (url.includes('/graphql/query')) return igGraphqlLegacy;
  if (url.includes('/i/api/graphql/')) return xBookmarks;
  if (url.includes('/resource/BoardFeedResource/get/')) return pinterestBoardFeed;
  return { unrelated: true };
}

const batches = (): BatchMessage[] =>
  sent.filter((m): m is BatchMessage => (m as { kind?: string }).kind === RUNTIME.batch);
const markers = (): MarkerMessage[] =>
  sent.filter((m): m is MarkerMessage => (m as { kind?: string }).kind === RUNTIME.marker);

beforeAll(async () => {
  jsdom.reconfigure({ url: IG_SAVED });
  vi.stubGlobal('chrome', {
    runtime: {
      id: 'spike-test',
      sendMessage: vi.fn(async (message: unknown) => {
        sent.push(message);
        return { ok: true };
      }),
      onMessage: {
        addListener: (listener: RuntimeListener) => void runtimeListeners.push(listener),
      },
    },
  });
  // jsdom's postMessage leaves event.source null; Chrome sets it to the window, which is
  // exactly what the bridge checks. Deliver like Chrome does (async, structured clone).
  window.postMessage = ((data: unknown) => {
    setTimeout(() => {
      window.dispatchEvent(
        new MessageEvent('message', {
          data: structuredClone(data),
          source: window,
          origin: window.location.origin,
        }),
      );
    }, 0);
  }) as typeof window.postMessage;
  // The page's network: captured by the hook as its "original" fetch.
  window.fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
    fetched.push({ url, init });
    return new Response(JSON.stringify(respond(url)), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    });
  }) as typeof window.fetch;
  await import('../src/bridge');
  await import('../src/hook.main');
});

afterAll(() => {
  vi.unstubAllGlobals();
});

describe('MAIN-world hook → bridge → service worker', () => {
  it('installs the desktop hook in the page', () => {
    expect((window as Window & { __socialSavedInjected?: boolean }).__socialSavedInjected).toBe(
      true,
    );
  });

  it('the bridge answers the side panel ping, and nothing else', () => {
    expect(runtimeListeners).toHaveLength(1);
    const sendResponse = vi.fn();
    runtimeListeners[0]({ kind: RUNTIME.ping }, {}, sendResponse);
    runtimeListeners[0]({ kind: RUNTIME.getState }, {}, sendResponse);
    expect(sendResponse.mock.calls).toEqual([[{ ok: true }]]);
  });

  it('relays a passive IG REST page with the listing URL and no captions', async () => {
    await window.fetch('/api/v1/feed/saved/posts/?max_id=');
    await vi.waitFor(() => expect(batches()).toHaveLength(1));
    const [batch] = batches();
    expect(batch).toMatchObject({
      kind: RUNTIME.batch,
      platform: 'instagram',
      hasNextPage: true,
      pageUrl: IG_SAVED,
      source: 'passive',
    });
    expect(batch.items.map((item) => item.id)).toEqual([
      '3400000000000000001_9000000001',
      '3400000000000000002_9000000002',
    ]);
    expect(batch.items[0]).not.toHaveProperty('text');
    expect(parseBatchMessage(batch)).toEqual(batch);
  });

  it('relays the legacy GraphQL shape (node.id is the bare pk)', async () => {
    await window.fetch('/graphql/query', {
      method: 'POST',
      headers: { 'X-FB-Friendly-Name': 'SyntheticSavedQuery' },
      body: 'fb_api_req_friendly_name=SyntheticSavedQuery&doc_id=1',
    });
    await vi.waitFor(() => expect(batches()).toHaveLength(2));
    expect(batches()[1].items.map((item) => item.id)).toEqual([
      '3400000000000000001',
      '3400000000000000004',
    ]);
    expect(batches()[1].hasNextPage).toBe(false);
  });

  it('runs the IG replay through the hook and tags its batches', async () => {
    const result = await igFeedReplay({ maxPages: 10, gapMs: 1, runId: 'replay-test' });
    expect(result).toEqual({
      started: true,
      reason: 'end_of_feed',
      pages: 2,
      status: 200,
      endpoint: '/api/v1/feed/saved/posts/',
      appIdSource: 'fallback',
    });
    await vi.waitFor(() => expect(markers()).toHaveLength(2));
    const replayCalls = fetched.filter((f) =>
      f.url.startsWith('/api/v1/feed/saved/posts/?max_id='),
    );
    expect(replayCalls.map((f) => f.url).slice(-2)).toEqual([
      '/api/v1/feed/saved/posts/?max_id=',
      '/api/v1/feed/saved/posts/?max_id=SYNTHETIC_CURSOR_1',
    ]);
    expect(replayCalls.at(-1)?.init).toMatchObject({
      credentials: 'include',
      headers: { 'X-IG-App-ID': '936619743392459' },
    });
    expect(
      batches()
        .slice(2)
        .map((b) => b.source),
    ).toEqual(['replay', 'replay']);
    expect(markers().map((m) => [m.phase, m.detail])).toEqual([
      ['start', { endpoint: '/api/v1/feed/saved/posts/' }],
      ['end', { pages: 2, reason: 'end_of_feed', status: 200 }],
    ]);
  });

  it('relays X bookmarks and a Pinterest board page', async () => {
    jsdom.reconfigure({ url: 'https://x.com/i/bookmarks' });
    await window.fetch('https://x.com/i/api/graphql/SYNTH/Bookmarks?variables=%7B%7D');
    await vi.waitFor(() => expect(batches()).toHaveLength(5));
    expect(batches()[4]).toMatchObject({ platform: 'twitter', hasNextPage: true });

    jsdom.reconfigure({ url: PIN_BOARD });
    await window.fetch('/resource/BoardFeedResource/get/?source_url=%2Fsomeone%2Frecipes%2F');
    await vi.waitFor(() => expect(batches()).toHaveLength(6));
    expect(batches()[5]).toMatchObject({ platform: 'pinterest', pageUrl: PIN_BOARD });
    expect(batches()[5].items.map((item) => item.id)).toEqual([
      '900000000000000001',
      '900000000000000002',
    ]);
  });

  it('stores what the desktop would ingest, keyed canonically, with listing and sources', () => {
    let meta = emptyMeta(NOW);
    const store = new Map<string, StoredItem>();
    batches().forEach((message, index) => {
      const at = NOW + index;
      const batch = prepareBatch(message, at);
      const result = applyBatch(meta, store, batch, at);
      meta = result.meta;
      for (const item of result.changed) store.set(item.key, item);
    });

    expect([...store.keys()].sort()).toEqual([
      'ig_3400000000000000001',
      'ig_3400000000000000002',
      'ig_3400000000000000003',
      'ig_3400000000000000004',
      'pin_900000000000000001',
      'pin_900000000000000002',
      'x_1800000000000000001',
      'x_1800000000000000002',
    ]);

    const carousel = store.get('ig_3400000000000000001');
    expect(carousel?.rawIds).toEqual(['3400000000000000001_9000000001', '3400000000000000001']);
    expect(carousel?.listings['instagram:ig_saved'].sources).toEqual(['passive', 'replay']);
    expect(carousel?.mediaType).toBe('carousel');
    expect(carousel?.media.map((m) => [m.slot, m.position, m.urlKind])).toEqual([
      ['cover', null, 'image'],
      ['slide', 0, 'image'],
      ['slide', 1, 'poster'],
    ]);
    // The REST replay ran last, so its re-signed URLs (oe=68F00000) are the ones kept.
    expect(carousel?.media[0].expiresAt).toBe(0x68f00000 * 1000);
    expect(carousel?.media[0].observations).toBe(3);

    expect(store.get('ig_3400000000000000003')?.listings['instagram:ig_saved'].sources).toEqual([
      'replay',
    ]);
    expect(store.get('x_1800000000000000002')?.media.map((m) => m.urlKind)).toEqual([
      'image',
      'poster',
    ]);
    expect(store.get('pin_900000000000000002')?.media.map((m) => [m.urlKind, m.host])).toEqual([
      ['image', 'i.pinimg.com'],
      ['video', 'v1.pinimg.com'],
    ]);

    expect(meta.listings['instagram:ig_saved']).toMatchObject({
      uniqueItems: 4,
      batches: 4,
      endOfFeedSeen: true,
      bySource: { passive: 3, replay: 3, ssr: 0, dom: 0 },
    });
    expect(meta.listings['twitter:x_bookmarks']).toMatchObject({
      uniqueItems: 2,
      endOfFeedSeen: false,
    });
    expect(meta.listings['pinterest:pin_board:someone/recipes']).toMatchObject({
      uniqueItems: 2,
      account: 'someone',
    });
    expect(meta.platforms.instagram).toMatchObject({
      uniqueItems: 4,
      batches: 4,
      rejectedItems: 0,
    });
    expect(meta.diagnostics.igShortcodeMismatch).toBe(0);

    const exported = parseExportFile(
      JSON.parse(
        JSON.stringify(
          buildExport(meta, [...store.values()], {
            extensionVersion: '0.1.0',
            userAgent: 'test',
            now: NOW + 100,
          }),
        ),
      ),
    );
    expect(exported.items).toHaveLength(8);
    expect(exported.listings.map((l) => l.key)).toEqual([
      'instagram:ig_saved',
      'pinterest:pin_board:someone/recipes',
      'twitter:x_bookmarks',
    ]);
    expect(JSON.stringify(exported)).not.toContain('Synthetic caption');
  });
});
