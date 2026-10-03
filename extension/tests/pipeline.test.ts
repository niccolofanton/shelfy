// @vitest-environment jsdom
//
// Passive capture end to end on synthetic platform responses: the real desktop hook
// (electron/webview-injected.ts, bundled by content/hook.main.ts) parses page responses and posts
// them with its postMessage fallback; the real bridge relays them to a fake chrome.runtime that
// hands them to the worker's capture path (sw/capture.ts → the queue); the uploader sends them to
// the fake Shelfy API. Nothing here touches a real platform or server.

import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';
import igGraphqlLegacy from './fixtures/ig-graphql-legacy.json';
import igRestPage1 from './fixtures/ig-saved-rest-page1.json';
import igRestPage2 from './fixtures/ig-saved-rest-page2.json';
import pinterestBoardFeed from './fixtures/pinterest-board-feed.json';
import xBookmarks from './fixtures/x-bookmarks.json';
import { igFeedReplay } from '../src/main/replay';
import { MSG, parseCaptureMessage, type CaptureMessage } from '../src/shared/protocol';
import { handleCapture, type CaptureOutcome } from '../src/sw/capture';
import { harness, type Harness } from './helpers';

declare const jsdom: { reconfigure(options: { url: string }): void };

const IG_SAVED = 'https://www.instagram.com/someone/saved/all-posts/';
const IG_EXPLORE = 'https://www.instagram.com/explore/';
const PIN_BOARD = 'https://www.pinterest.it/someone/recipes/';
const PIN_OTHER_BOARD = 'https://www.pinterest.it/someone_else/cakes/';

type RuntimeListener = (
  message: unknown,
  sender: unknown,
  sendResponse: (response: unknown) => void,
) => boolean | void;

const h: Harness = harness();
const sent: CaptureMessage[] = [];
const outcomes: CaptureOutcome[] = [];
const runtimeListeners: RuntimeListener[] = [];

function respond(url: string): unknown {
  if (url.includes('/api/v1/feed/saved/posts/'))
    return url.includes('max_id=SYNTHETIC_CURSOR_1') ? igRestPage2 : igRestPage1;
  if (url.includes('/graphql/query')) return igGraphqlLegacy;
  if (url.includes('/i/api/graphql/')) return xBookmarks;
  if (url.includes('/resource/BoardFeedResource/get/')) return pinterestBoardFeed;
  return { unrelated: true };
}

async function settle(count: number): Promise<void> {
  await vi.waitFor(() => expect(outcomes).toHaveLength(count));
}

beforeAll(async () => {
  await h.pairNow();
  jsdom.reconfigure({ url: IG_SAVED });
  vi.stubGlobal('chrome', {
    runtime: {
      id: 'test-extension',
      sendMessage: vi.fn(async (message: unknown) => {
        const capture = parseCaptureMessage(message);
        if (!capture) return { ok: false };
        sent.push(capture);
        // The browser vouches for the sender: the tab's top frame on the current page.
        const outcome = await handleCapture(
          capture,
          { tabId: 1, frameId: 0, url: window.location.href, tabUrl: window.location.href },
          { queue: h.queue, store: h.store, config: h.config, now: () => h.clock.now },
        );
        outcomes.push(outcome);
        return outcome;
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
  window.fetch = vi.fn(async (input: RequestInfo | URL) => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
    return new Response(JSON.stringify(respond(url)), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    });
  }) as typeof window.fetch;
  await import('../src/content/bridge');
  await import('../src/content/hook.main');
});

afterAll(() => {
  vi.unstubAllGlobals();
});

describe('hook → bridge → worker → API', () => {
  it('installs the desktop hook, and the bridge answers the panel ping only', () => {
    expect((window as Window & { __socialSavedInjected?: boolean }).__socialSavedInjected).toBe(
      true,
    );
    expect(runtimeListeners).toHaveLength(1);
    const sendResponse = vi.fn();
    runtimeListeners[0]({ kind: MSG.bridgePing }, { id: 'test-extension' }, sendResponse);
    runtimeListeners[0]({ kind: MSG.bridgePing }, { id: 'another-extension' }, sendResponse);
    runtimeListeners[0]({ kind: MSG.stateGet }, { id: 'test-extension' }, sendResponse);
    expect(sendResponse.mock.calls).toEqual([
      [
        {
          ok: true,
          docId: expect.stringMatching(/^[0-9a-f]{24}$/),
          viewer: null,
          heading: null,
          syncing: null,
        },
      ],
    ]);
  });

  it('queues a passive IG REST page from the saved listing, captions included', async () => {
    await window.fetch('/api/v1/feed/saved/posts/?max_id=');
    await settle(1);
    expect(sent[0]).toMatchObject({
      platform: 'instagram',
      hasNextPage: true,
      pageUrl: IG_SAVED,
      capture: 'passive',
      seq: 0,
    });
    expect(sent[0].items[0]).toHaveProperty('text', 'Synthetic caption A');
    expect(outcomes[0]).toMatchObject({ ok: true, queued: 2 });
  });

  it('queues the replayed pages through the same hook (kept for P2-13)', async () => {
    const result = await igFeedReplay({ maxPages: 10, gapMs: 1, runId: 'replay-test' });
    expect(result).toMatchObject({ started: true, reason: 'end_of_feed', pages: 2 });
    await settle(3);
    expect(sent.slice(1).map((m) => m.capture)).toEqual(['replay', 'replay']);
  });

  it('discards what a page outside the saved listings loads', async () => {
    jsdom.reconfigure({ url: IG_EXPLORE });
    await window.fetch('/graphql/query', { method: 'POST', body: 'doc_id=1' });
    await settle(4);
    expect(outcomes[3]).toMatchObject({ queued: 0, discarded: 'out_of_scope' });
  });

  it('queues X bookmarks, and a Pinterest board of the signed-in user only', async () => {
    jsdom.reconfigure({ url: 'https://x.com/i/bookmarks' });
    await window.fetch('https://x.com/i/api/graphql/SYNTH/Bookmarks?variables=%7B%7D');
    await settle(5);
    expect(outcomes[4]).toMatchObject({ queued: 2 });

    const context = document.createElement('script');
    context.id = '__PWS_DATA__';
    context.type = 'application/json';
    context.textContent = JSON.stringify({ props: { context: { user: { username: 'someone' } } } });
    document.head.append(context);
    jsdom.reconfigure({ url: PIN_BOARD });
    await window.fetch('/resource/BoardFeedResource/get/?source_url=%2Fsomeone%2Frecipes%2F');
    await settle(6);
    expect(sent[5]).toMatchObject({ platform: 'pinterest', viewer: 'someone' });
    expect(outcomes[5]).toMatchObject({ queued: 2 });

    jsdom.reconfigure({ url: PIN_OTHER_BOARD });
    await window.fetch('/resource/BoardFeedResource/get/?source_url=%2Fsomeone_else%2Fcakes%2F');
    await settle(7);
    expect(outcomes[6]).toMatchObject({ queued: 0, discarded: 'not_own_board' });
  });

  it('sends one passive run per listing visit, keyed canonically on the server', async () => {
    await h.queue.endRuns(() => true, 'user', h.clock.now);
    h.clock.now += 2_000;
    await h.uploader.flush();
    const runs = [...h.api.runs.values()];
    expect(
      runs.map((run) => [run.platform, run.trigger, run.listing.kind, run.listing.externalId]),
    ).toEqual([
      ['instagram', 'passive', 'ig_saved', null],
      ['twitter', 'passive', 'x_bookmarks', null],
      ['pinterest', 'passive', 'pin_board', 'someone/recipes'],
    ]);
    expect(runs.map((run) => run.state)).toEqual(['done', 'done', 'done']);
    expect([...h.api.posts.keys()].sort()).toEqual([
      'ig_3400000000000000001',
      'ig_3400000000000000002',
      'ig_3400000000000000003',
      'pin_900000000000000001',
      'pin_900000000000000002',
      'x_1800000000000000001',
      'x_1800000000000000002',
    ]);
    // The REST page and the replay's first page carry the same posts: the server saw them as
    // known, and every batch was sent once.
    expect(h.api.ingests.every((ingest) => !ingest.replayed && ingest.source === 'passive')).toBe(
      true,
    );
    const { counters } = await h.queue.snapshot();
    expect(counters).toMatchObject({ queuedItems: 0, inserted: 7 });
    expect(counters.known).toBeGreaterThan(0);
    // Captions and direct video URLs reach the server.
    const items = [...h.api.batchItems.values()].flat() as Array<Record<string, unknown>>;
    expect(items.some((item) => item.text === 'Synthetic caption A')).toBe(true);
  });
});
