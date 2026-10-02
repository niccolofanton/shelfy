import { describe, expect, it, vi } from 'vitest';
import { MAX_BATCH_ITEMS } from '../../src/lib/browserSanitize';
import {
  CENSUS_MESSAGE,
  INTERCEPT_MESSAGE,
  RUNTIME,
  SCOPE_MESSAGE,
  ScopeTracker,
  parseBatchMessage,
  parseCensusMessage,
  parseCensusRuntimeMessage,
  parseInterceptMessage,
  parseMarkerMessage,
  parseScopeMessage,
  projectItem,
  toBatchMessage,
  toMarkerMessage,
} from '../src/protocol';
import { createRelay } from '../src/relay';

const PAGE = 'https://www.instagram.com/someone/saved/all-posts/';

const hookItem = {
  id: '3400000000000000001_9000000001',
  platform: 'instagram',
  shortcode: 'C8vOfxsVAAB',
  postUrl: 'https://www.instagram.com/p/C8vOfxsVAAB/',
  profileUrl: 'https://www.instagram.com/synthetic_author_a/',
  authorUsername: 'synthetic_author_a',
  authorName: '',
  text: 'Synthetic caption A',
  thumbnailUrl: 'https://scontent-synth1-1.cdninstagram.com/v/a.jpg?oe=68F00000',
  mediaType: 'image',
  media: [{ type: 'image', url: 'https://scontent-synth1-1.cdninstagram.com/v/a.jpg?oe=68F00000' }],
  timestamp: '2025-09-16T05:20:00.000Z',
};

describe('parseInterceptMessage (hook → bridge)', () => {
  it('accepts the shape posted by the webview-injected.ts fallback relay', () => {
    const parsed = parseInterceptMessage({
      type: INTERCEPT_MESSAGE,
      items: [hookItem],
      hasNextPage: true,
      platform: 'instagram',
    });
    expect(parsed).toEqual({ platform: 'instagram', items: [hookItem], hasNextPage: true });
  });

  it('keeps the tri-state pagination signal and coerces anything else to null', () => {
    const base = { type: INTERCEPT_MESSAGE, items: [], platform: 'twitter' };
    expect(parseInterceptMessage({ ...base, hasNextPage: false })?.hasNextPage).toBe(false);
    expect(parseInterceptMessage({ ...base, hasNextPage: null })?.hasNextPage).toBeNull();
    expect(parseInterceptMessage({ ...base, hasNextPage: 'yes' })?.hasNextPage).toBeNull();
  });

  it('rejects other message types, unknown platforms and non-array items', () => {
    expect(parseInterceptMessage({ type: 'OTHER', items: [], platform: 'instagram' })).toBeNull();
    expect(
      parseInterceptMessage({ type: INTERCEPT_MESSAGE, items: [], platform: 'web' }),
    ).toBeNull();
    expect(
      parseInterceptMessage({ type: INTERCEPT_MESSAGE, items: {}, platform: 'pinterest' }),
    ).toBeNull();
    expect(parseInterceptMessage('SOCIAL_SAVED_INTERCEPT')).toBeNull();
    expect(parseInterceptMessage(null)).toBeNull();
  });

  it('caps a batch at the desktop sanitizer limit', () => {
    const items = Array.from({ length: MAX_BATCH_ITEMS + 50 }, (_, i) => ({ id: String(i) }));
    const parsed = parseInterceptMessage({ type: INTERCEPT_MESSAGE, items, platform: 'twitter' });
    expect(parsed?.items).toHaveLength(MAX_BATCH_ITEMS);
  });
});

describe('projectItem', () => {
  it('drops captions and author fields at the first hop', () => {
    const projected = projectItem(hookItem);
    expect(projected).toEqual({
      id: hookItem.id,
      shortcode: hookItem.shortcode,
      postUrl: hookItem.postUrl,
      mediaType: 'image',
      timestamp: hookItem.timestamp,
      thumbnailUrl: hookItem.thumbnailUrl,
      media: hookItem.media,
    });
    expect(projected).not.toHaveProperty('text');
    expect(projected).not.toHaveProperty('authorUsername');
    expect(projectItem('not an item')).toBeNull();
  });
});

describe('scope messages', () => {
  it('parses start/end and bounds the detail', () => {
    const scope = parseScopeMessage({
      type: SCOPE_MESSAGE,
      phase: 'end',
      source: 'replay',
      id: 'replay-1',
      detail: {
        pages: 3,
        reason: 'end_of_feed',
        status: 200,
        nested: { no: 1 },
        long: 'x'.repeat(500),
      },
    });
    expect(scope).toEqual({
      phase: 'end',
      source: 'replay',
      id: 'replay-1',
      detail: { pages: 3, reason: 'end_of_feed', status: 200, long: 'x'.repeat(200) },
    });
  });

  it('rejects passive scopes, unknown sources and missing ids', () => {
    const base = { type: SCOPE_MESSAGE, phase: 'start', id: 'a' };
    expect(parseScopeMessage({ ...base, source: 'passive' })).toBeNull();
    expect(parseScopeMessage({ ...base, source: 'magic' })).toBeNull();
    expect(parseScopeMessage({ ...base, source: 'ssr', id: '' })).toBeNull();
    expect(parseScopeMessage({ ...base, source: 'ssr', phase: 'middle' })).toBeNull();
  });

  it('ScopeTracker tags with the innermost open scope, else passive', () => {
    const tracker = new ScopeTracker();
    expect(tracker.current()).toBe('passive');
    tracker.apply({ phase: 'start', source: 'replay', id: 'r', detail: {} });
    expect(tracker.current()).toBe('replay');
    tracker.apply({ phase: 'start', source: 'ssr', id: 's', detail: {} });
    expect(tracker.current()).toBe('ssr');
    tracker.apply({ phase: 'end', source: 'ssr', id: 's', detail: {} });
    expect(tracker.current()).toBe('replay');
    tracker.apply({ phase: 'end', source: 'replay', id: 'unknown', detail: {} });
    expect(tracker.current()).toBe('replay');
    tracker.apply({ phase: 'end', source: 'replay', id: 'r', detail: {} });
    expect(tracker.current()).toBe('passive');
  });
});

describe('census messages', () => {
  it('keeps positive integer counts with a known platform prefix', () => {
    expect(
      parseCensusMessage({
        type: CENSUS_MESSAGE,
        counts: {
          'instagram|graphql PolarisSavedQuery': 3,
          'web|whatever': 1,
          'twitter|graphql Bookmarks': 0,
          'pinterest|resource BoardFeedResource': 1.5,
        },
      }),
    ).toEqual({ 'instagram|graphql PolarisSavedQuery': 3 });
    expect(parseCensusMessage({ type: CENSUS_MESSAGE, counts: { 'web|x': 1 } })).toBeNull();
  });
});

describe('runtime messages (bridge → service worker)', () => {
  it('round-trips a batch, projecting items and keeping the relay context', () => {
    const message = toBatchMessage(
      { platform: 'instagram', items: [hookItem, 'junk'], hasNextPage: true },
      { pageUrl: PAGE, source: 'passive', sentAt: 1_760_000_000_000 },
    );
    expect(message.kind).toBe(RUNTIME.batch);
    expect(message.items).toHaveLength(1);
    expect(message.items[0]).not.toHaveProperty('text');
    expect(parseBatchMessage(message)).toEqual(message);
  });

  it('refuses malformed batches', () => {
    const good = toBatchMessage(
      { platform: 'twitter', items: [], hasNextPage: null },
      { pageUrl: 'https://x.com/i/bookmarks', source: 'dom', sentAt: 1 },
    );
    expect(parseBatchMessage({ ...good, kind: 'other' })).toBeNull();
    expect(parseBatchMessage({ ...good, platform: 'web' })).toBeNull();
    expect(parseBatchMessage({ ...good, source: 'magic' })).toBeNull();
    expect(parseBatchMessage({ ...good, pageUrl: '' })).toBeNull();
    expect(parseBatchMessage({ ...good, sentAt: -1 })).toBeNull();
  });

  it('round-trips markers and census messages', () => {
    const marker = toMarkerMessage(
      {
        phase: 'start',
        source: 'replay',
        id: 'r1',
        detail: { endpoint: '/api/v1/feed/saved/posts/' },
      },
      PAGE,
      5,
    );
    expect(parseMarkerMessage(marker)).toEqual(marker);
    expect(
      parseCensusRuntimeMessage({
        kind: RUNTIME.census,
        counts: { 'instagram|rest /api/v1/feed/saved/posts/': 2 },
        pageUrl: PAGE,
        sentAt: 5,
      }),
    ).toEqual({
      kind: RUNTIME.census,
      counts: { 'instagram|rest /api/v1/feed/saved/posts/': 2 },
      pageUrl: PAGE,
      sentAt: 5,
    });
  });
});

describe('createRelay (bridge core)', () => {
  type Send = (message: unknown) => Promise<unknown>;
  function setup(impl: Send = async () => ({ ok: true })) {
    const send = vi.fn(impl);
    const win = {} as Window;
    const warn = vi.fn();
    const scheduled: Array<() => void> = [];
    const relay = createRelay(win, {
      send,
      pageUrl: () => PAGE,
      now: () => 42,
      warn,
      schedule: (callback) => void scheduled.push(callback),
    });
    const post = (data: unknown, source: unknown = win): void =>
      relay({ data, source } as unknown as MessageEvent);
    return { send, warn, scheduled, post };
  }
  const intercept = {
    type: INTERCEPT_MESSAGE,
    items: [hookItem],
    hasNextPage: true,
    platform: 'instagram',
  };

  it('accepts only messages whose source is the page window (webview-preload rule)', () => {
    const { send, post } = setup();
    post(intercept, {});
    post(intercept, null);
    expect(send).not.toHaveBeenCalled();
    post(intercept);
    expect(send).toHaveBeenCalledTimes(1);
    expect(send.mock.calls[0][0]).toMatchObject({
      kind: RUNTIME.batch,
      platform: 'instagram',
      pageUrl: PAGE,
      source: 'passive',
      sentAt: 42,
    });
  });

  it('tags batches posted inside a scope and forwards only replay markers', () => {
    const { send, post } = setup();
    post({ type: SCOPE_MESSAGE, phase: 'start', source: 'replay', id: 'r1', detail: {} });
    post(intercept);
    post({ type: SCOPE_MESSAGE, phase: 'end', source: 'replay', id: 'r1', detail: { pages: 1 } });
    post({ type: SCOPE_MESSAGE, phase: 'start', source: 'ssr', id: 's1', detail: {} });
    post(intercept);
    post({ type: SCOPE_MESSAGE, phase: 'end', source: 'ssr', id: 's1', detail: {} });
    post(intercept);
    const messages = send.mock.calls.map(
      ([m]) => m as { kind: string; source?: string; phase?: string },
    );
    expect(messages.map((m) => [m.kind, m.source, m.phase ?? null])).toEqual([
      [RUNTIME.marker, 'replay', 'start'],
      [RUNTIME.batch, 'replay', null],
      [RUNTIME.marker, 'replay', 'end'],
      [RUNTIME.batch, 'ssr', null],
      [RUNTIME.batch, 'passive', null],
    ]);
  });

  it('forwards census counts with the page URL', () => {
    const { send, post } = setup();
    post({ type: CENSUS_MESSAGE, counts: { 'instagram|graphql X': 2 } });
    expect(send).toHaveBeenCalledWith({
      kind: RUNTIME.census,
      counts: { 'instagram|graphql X': 2 },
      pageUrl: PAGE,
      sentAt: 42,
    });
  });

  it('retries a failed delivery, then gives up with a warning', async () => {
    const { send, warn, scheduled, post } = setup(async () => {
      throw new Error('Could not establish connection. Receiving end does not exist.');
    });
    post(intercept);
    await vi.waitFor(() => expect(scheduled).toHaveLength(1));
    scheduled.shift()?.();
    await vi.waitFor(() => expect(scheduled).toHaveLength(1));
    scheduled.shift()?.();
    await vi.waitFor(() => expect(warn).toHaveBeenCalledTimes(1));
    expect(send).toHaveBeenCalledTimes(3);
    expect(warn.mock.calls[0][0]).toMatch(/dropped a message after 3 attempts/);
  });

  it('stops relaying once the extension context is invalidated', async () => {
    const { send, warn, scheduled, post } = setup(async () => {
      throw new Error('Extension context invalidated.');
    });
    post(intercept);
    await vi.waitFor(() => expect(warn).toHaveBeenCalledTimes(1));
    post(intercept);
    expect(send).toHaveBeenCalledTimes(1);
    expect(scheduled).toHaveLength(0);
    expect(warn.mock.calls[0][0]).toMatch(/refresh this tab/);
  });
});
