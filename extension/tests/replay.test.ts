// @vitest-environment jsdom
//
// The IG replay is a port of the desktop's IG_FEED_REPLAY string. These tests pin its
// endpoints and pacing to the desktop script and check it survives the toString()
// serialization chrome.scripting.executeScript applies to `func`.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { IG_FEED_REPLAY } from '../../src/lib/browserScripts';
import {
  IG_REPLAY_GAP_MS,
  IG_REPLAY_MAX_PAGES,
  igFeedReplay,
  isCaptureHookInstalled,
  stopIgFeedReplay,
  type ReplayOptions,
  type ReplayResult,
} from '../src/replay';

declare const jsdom: { reconfigure(options: { url: string }): void };

type Page = { more_available: boolean; next_max_id?: string; items: unknown[] };

const posted: Array<{ type?: string; phase?: string; detail?: Record<string, unknown> }> = [];
let requests: Array<{ url: string; init?: RequestInit }> = [];

function serveFeed(pages: Page[], status = 200): void {
  window.fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    requests.push({ url, init });
    const page = pages[Math.min(requests.length - 1, pages.length - 1)];
    return new Response(JSON.stringify(page), { status });
  }) as typeof window.fetch;
}

const options = (extra: Partial<ReplayOptions> = {}): ReplayOptions => ({
  maxPages: 10,
  gapMs: 1,
  runId: 'r1',
  ...extra,
});

beforeEach(() => {
  posted.length = 0;
  requests = [];
  window.postMessage = ((data: (typeof posted)[number]) =>
    void posted.push(data)) as typeof window.postMessage;
  delete (window as Window & { __syncStop?: boolean }).__syncStop;
});

afterEach(() => {
  document.head.innerHTML = '';
});

describe('igFeedReplay', () => {
  it('uses the same endpoints, app-id fallback, gap and page cap as the desktop script', () => {
    expect(IG_FEED_REPLAY).toContain("'/api/v1/feed/saved/posts/'");
    expect(IG_FEED_REPLAY).toContain("'/api/v1/feed/collection/' + id + '/posts/'");
    expect(IG_FEED_REPLAY).toContain("'936619743392459'");
    expect(IG_FEED_REPLAY).toContain(`setTimeout(res, ${IG_REPLAY_GAP_MS})`);
    expect(IG_FEED_REPLAY).toContain(`i < ${IG_REPLAY_MAX_PAGES}`);
    expect(IG_FEED_REPLAY).toContain("credentials: 'include'");
    const source = igFeedReplay.toString();
    for (const fragment of [
      '/api/v1/feed/saved/posts/',
      '/api/v1/feed/collection/',
      '936619743392459',
      '"X-IG-App-ID"',
    ])
      expect(source).toContain(fragment);
  });

  it('follows the cursor of a folder feed until more_available is false', async () => {
    jsdom.reconfigure({
      url: 'https://www.instagram.com/someone/saved/recipes/17890000000000001/',
    });
    serveFeed([
      { more_available: true, next_max_id: 'CUR_1', items: [] },
      { more_available: true, next_max_id: 'CUR/2', items: [] },
      { more_available: false, items: [] },
    ]);
    const result = await igFeedReplay(options());
    expect(result).toMatchObject({ started: true, reason: 'end_of_feed', pages: 3, status: 200 });
    expect(requests.map((r) => r.url)).toEqual([
      '/api/v1/feed/collection/17890000000000001/posts/?max_id=',
      '/api/v1/feed/collection/17890000000000001/posts/?max_id=CUR_1',
      '/api/v1/feed/collection/17890000000000001/posts/?max_id=CUR%2F2',
    ]);
    expect(posted.map((m) => [m.type, m.phase])).toEqual([
      ['SHELFY_SPIKE_SCOPE', 'start'],
      ['SHELFY_SPIKE_SCOPE', 'end'],
    ]);
  });

  it('reads the app id from the page config when present', async () => {
    jsdom.reconfigure({ url: 'https://www.instagram.com/someone/saved/all-posts/' });
    const config = document.createElement('script');
    config.type = 'application/json';
    config.textContent = '{"X-IG-App-ID":"1234567890"}';
    document.head.append(config);
    serveFeed([{ more_available: false, items: [] }]);
    const result = await igFeedReplay(options());
    expect(result.appIdSource).toBe('page');
    expect(requests[0].init?.headers).toEqual({ 'X-IG-App-ID': '1234567890' });
  });

  it('stops at the page cap, on the stop flag and on HTTP errors', async () => {
    jsdom.reconfigure({ url: 'https://www.instagram.com/someone/saved/all-posts/' });
    serveFeed([{ more_available: true, next_max_id: 'CUR', items: [] }]);
    expect(await igFeedReplay(options({ maxPages: 2 }))).toMatchObject({
      reason: 'page_cap',
      pages: 2,
    });

    requests = [];
    serveFeed([{ more_available: true, next_max_id: 'CUR', items: [] }]);
    const running = igFeedReplay(options({ gapMs: 30 }));
    await vi.waitFor(() => expect(requests).toHaveLength(1));
    stopIgFeedReplay();
    expect(await running).toMatchObject({ reason: 'stopped', pages: 1 });

    requests = [];
    serveFeed([{ more_available: true, items: [] }], 429);
    expect(await igFeedReplay(options())).toMatchObject({
      reason: 'http_error',
      status: 429,
      pages: 0,
    });
  });

  it('does nothing outside a saved listing', async () => {
    serveFeed([]);
    jsdom.reconfigure({ url: 'https://www.instagram.com/explore/' });
    expect(await igFeedReplay(options())).toMatchObject({
      started: false,
      reason: 'not_a_saved_listing',
    });
    jsdom.reconfigure({ url: 'https://www.instagram.com/someone/saved/' });
    expect(await igFeedReplay(options())).toMatchObject({
      started: false,
      reason: 'unknown_listing_shape',
    });
    expect(requests).toEqual([]);
    expect(posted).toEqual([]);
  });

  it('is self-contained: the toString() copy Chrome injects behaves the same', async () => {
    jsdom.reconfigure({ url: 'https://www.instagram.com/someone/saved/all-posts/' });
    serveFeed([{ more_available: false, items: [] }]);
    const injected = new Function(`return (${igFeedReplay.toString()});`)() as (
      o: ReplayOptions,
    ) => Promise<ReplayResult>;
    expect(await injected(options())).toMatchObject({
      started: true,
      reason: 'end_of_feed',
      pages: 1,
    });
    const probe = new Function(`return (${isCaptureHookInstalled.toString()});`)() as () => boolean;
    expect(probe()).toBe(false);
  });
});
