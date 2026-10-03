// @vitest-environment jsdom
//
// MAIN-world helpers: the Pinterest SSR first page and the X DOM scan are read through the
// desktop hook's own functions, each inside a scope; the census counts hooked requests.

import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import pinterestBoardFeed from './fixtures/pinterest-board-feed.json';
import { CENSUS_MESSAGE, INTERCEPT_MESSAGE, SCOPE_MESSAGE } from '../src/shared/protocol';
import { censusLabel, friendlyNameFrom, installCensus } from '../src/main/census';
import { createThrottle, installPassiveHelpers, type HookWindow } from '../src/main/passive';

declare const jsdom: { reconfigure(options: { url: string }): void };

const posted: Array<{ type?: string; [key: string]: unknown }> = [];

beforeAll(async () => {
  window.postMessage = ((data: { type?: string }) => {
    posted.push(structuredClone(data));
  }) as typeof window.postMessage;
  // The desktop hook: exposes window.__ssReplayPinterest / __ssScanTwitterBookmarks.
  await import('../../electron/webview-injected');
});

afterEach(() => {
  posted.length = 0;
  document.body.innerHTML = '';
  document.head.innerHTML = '';
});

describe('installPassiveHelpers', () => {
  it('Pinterest: emits the server-rendered first page of a board inside an ssr scope', () => {
    jsdom.reconfigure({ url: 'https://www.pinterest.com/someone/recipes/' });
    const script = document.createElement('script');
    script.type = 'application/json';
    script.id = '__PWS_DATA__';
    script.textContent = JSON.stringify({
      props: {
        initialReduxState: {
          resources: {
            BoardFeedResource: {
              'synthetic-args': { data: pinterestBoardFeed.resource_response.data },
            },
          },
        },
      },
    });
    document.body.append(script);

    installPassiveHelpers(window as HookWindow);

    expect(posted.map((m) => [m.type, m.phase ?? null, m.source ?? null])).toEqual([
      [SCOPE_MESSAGE, 'start', 'ssr'],
      [INTERCEPT_MESSAGE, null, null],
      [SCOPE_MESSAGE, 'end', 'ssr'],
    ]);
    const intercept = posted[1] as {
      items: Array<{ id: string }>;
      platform: string;
      hasNextPage: unknown;
    };
    expect(intercept.platform).toBe('pinterest');
    expect(intercept.hasNextPage).toBeNull();
    expect(intercept.items.map((item) => item.id)).toEqual([
      '900000000000000001',
      '900000000000000002',
    ]);
  });

  it('Pinterest: reads nothing outside a board page', () => {
    jsdom.reconfigure({ url: 'https://www.pinterest.com/' });
    installPassiveHelpers(window as HookWindow);
    expect(posted).toEqual([]);
  });

  it('X: scans rendered bookmark cards on scroll, inside a dom scope', async () => {
    jsdom.reconfigure({ url: 'https://x.com/i/bookmarks' });
    document.body.innerHTML = `
      <article data-testid="tweet">
        <div data-testid="User-Name">Synthetic @synthetic_x_user</div>
        <a href="/synthetic_x_user/status/1800000000000000077"><time datetime="2025-09-17T10:00:00Z">x</time></a>
        <div data-testid="tweetText">Synthetic DOM tweet</div>
        <img src="https://pbs.twimg.com/media/SyntheticDom.jpg" />
      </article>`;
    installPassiveHelpers(window as HookWindow);
    window.dispatchEvent(new Event('scroll'));
    await vi.waitFor(() => expect(posted).toHaveLength(3));
    expect(posted.map((m) => [m.type, m.phase ?? null, m.source ?? null])).toEqual([
      [SCOPE_MESSAGE, 'start', 'dom'],
      [INTERCEPT_MESSAGE, null, null],
      [SCOPE_MESSAGE, 'end', 'dom'],
    ]);
    expect((posted[1] as { items: Array<{ id: string }> }).items[0].id).toBe('1800000000000000077');
  });
});

describe('createThrottle', () => {
  it('runs at once, then at most once per interval with a trailing call', () => {
    let now = 0;
    const scheduled: Array<() => void> = [];
    const fn = vi.fn();
    const throttled = createThrottle(
      fn,
      750,
      () => now,
      (callback) => void scheduled.push(callback),
    );
    throttled();
    throttled();
    throttled();
    expect(fn).toHaveBeenCalledTimes(1);
    expect(scheduled).toHaveLength(1);
    now = 750;
    scheduled.shift()?.();
    expect(fn).toHaveBeenCalledTimes(2);
    now = 2000;
    throttled();
    expect(fn).toHaveBeenCalledTimes(3);
  });
});

describe('census', () => {
  const base = 'https://www.instagram.com/someone/saved/all-posts/';

  it('labels exactly the URLs the hook parses', () => {
    expect(censusLabel('/graphql/query', () => 'SyntheticSavedQuery', base)).toEqual({
      platform: 'instagram',
      label: 'graphql SyntheticSavedQuery',
    });
    expect(censusLabel('/graphql/query', () => null, base)?.label).toBe('graphql (unnamed)');
    expect(
      censusLabel('/api/v1/feed/collection/17890000000000001/posts/?max_id=', () => null, base),
    ).toEqual({ platform: 'instagram', label: 'rest /api/v1/feed/collection/:id/posts/' });
    expect(
      censusLabel('https://x.com/i/api/graphql/AbC/Bookmarks?variables=%7B%7D', () => null, base),
    ).toEqual({ platform: 'twitter', label: 'graphql Bookmarks' });
    expect(
      censusLabel('https://x.com/i/api/graphql/AbC/HomeTimeline', () => null, base),
    ).toBeNull();
    expect(censusLabel('/resource/BoardFeedResource/get/?data=1', () => null, base)).toEqual({
      platform: 'pinterest',
      label: 'resource BoardFeedResource',
    });
    expect(censusLabel('/resource/BoardsResource/get/', () => null, base)).toBeNull();
    expect(censusLabel('/api/v1/users/web_profile_info/', () => null, base)).toBeNull();
  });

  it('reads the IG friendly name from headers or the form body', () => {
    expect(friendlyNameFrom({ 'X-FB-Friendly-Name': 'A_Query1' }, null)).toBe('A_Query1');
    expect(friendlyNameFrom(new Headers({ 'x-fb-friendly-name': 'B' }), null)).toBe('B');
    expect(friendlyNameFrom([['X-FB-Friendly-Name', 'C']], null)).toBe('C');
    expect(friendlyNameFrom(null, 'doc_id=1&fb_api_req_friendly_name=D_Query')).toBe('D_Query');
    expect(friendlyNameFrom(null, new URLSearchParams({ fb_api_req_friendly_name: 'E' }))).toBe(
      'E',
    );
    expect(friendlyNameFrom({ 'x-fb-friendly-name': '<script>' }, null)).toBeNull();
  });

  it('wraps fetch and XHR transparently and posts batched counts', async () => {
    jsdom.reconfigure({ url: base });
    const inner = vi.fn(async () => new Response('{}'));
    window.fetch = inner as unknown as typeof window.fetch;
    // Stand-ins for the page's XHR (jsdom's would hit the network): the census must call them.
    const proto = XMLHttpRequest.prototype;
    const original = {
      open: proto.open,
      setRequestHeader: proto.setRequestHeader,
      send: proto.send,
    };
    const xhrOpen = vi.fn();
    const xhrSetHeader = vi.fn();
    const xhrSend = vi.fn();
    proto.open = xhrOpen as unknown as typeof proto.open;
    proto.setRequestHeader = xhrSetHeader;
    proto.send = xhrSend;
    try {
      installCensus(window, 20);

      const response = await window.fetch('/graphql/query', {
        method: 'POST',
        headers: { 'X-FB-Friendly-Name': 'SyntheticSavedQuery' },
      });
      expect(await response.text()).toBe('{}');
      await window.fetch('/api/v1/feed/saved/posts/?max_id=');
      await window.fetch('/static/unrelated.js');
      expect(inner).toHaveBeenCalledTimes(3);

      const xhr = new XMLHttpRequest();
      xhr.open('POST', '/graphql/query');
      xhr.setRequestHeader('X-FB-Friendly-Name', 'SyntheticSavedQuery');
      xhr.send('fb_api_req_friendly_name=SyntheticSavedQuery');
      expect(xhrOpen).toHaveBeenCalledWith('POST', '/graphql/query');
      expect(xhrSetHeader).toHaveBeenCalledWith('X-FB-Friendly-Name', 'SyntheticSavedQuery');
      expect(xhrSend).toHaveBeenCalledWith('fb_api_req_friendly_name=SyntheticSavedQuery');

      await vi.waitFor(() => expect(posted.some((m) => m.type === CENSUS_MESSAGE)).toBe(true));
      const census = posted.find((m) => m.type === CENSUS_MESSAGE) as {
        counts: Record<string, number>;
      };
      expect(census.counts).toEqual({
        'instagram|graphql SyntheticSavedQuery': 2,
        'instagram|rest /api/v1/feed/saved/posts/': 1,
      });
    } finally {
      Object.assign(proto, original);
    }
  });
});
