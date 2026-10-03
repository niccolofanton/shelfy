import { describe, expect, it, vi } from 'vitest';
import { createRelay } from '../src/content/relay';
import { ScopeTracker } from '../src/content/scoping';
import {
  CENSUS_MESSAGE,
  EXTERNAL,
  INTERCEPT_MESSAGE,
  MAX_RELAY_ITEMS,
  MSG,
  SCOPE_MESSAGE,
  parseCaptureMessage,
  parseCensusMessage,
  parseCensusRuntimeMessage,
  parseExternalMessage,
  parseInterceptMessage,
  parseScopeMessage,
  parseSettingsPatch,
  toCaptureMessage,
} from '../src/shared/protocol';

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

  it('caps a message at the desktop sanitizer limit', () => {
    const items = Array.from({ length: MAX_RELAY_ITEMS + 50 }, (_, i) => ({ id: String(i) }));
    const parsed = parseInterceptMessage({ type: INTERCEPT_MESSAGE, items, platform: 'twitter' });
    expect(parsed?.items).toHaveLength(MAX_RELAY_ITEMS);
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
    expect(
      parseCensusRuntimeMessage({
        kind: MSG.census,
        counts: { 'instagram|rest /api/v1/feed/saved/posts/': 2 },
        pageUrl: PAGE,
        sentAt: 5,
      }),
    ).toEqual({
      kind: MSG.census,
      counts: { 'instagram|rest /api/v1/feed/saved/posts/': 2 },
      pageUrl: PAGE,
      sentAt: 5,
    });
  });
});

describe('capture messages (bridge → worker)', () => {
  const context = {
    pageUrl: PAGE,
    docId: 'a1b2c3',
    seq: 4,
    capture: 'passive' as const,
    viewer: null,
    sentAt: 1_760_000_000_000,
  };

  it('round-trip with every field, captions and authors included (P2-G15)', () => {
    const message = toCaptureMessage(
      { platform: 'instagram', items: [hookItem, 'junk'], hasNextPage: true },
      context,
    );
    expect(message).toEqual({
      kind: MSG.capture,
      platform: 'instagram',
      items: [hookItem],
      hasNextPage: true,
      ...context,
    });
    expect(message.items[0]).toHaveProperty('text', 'Synthetic caption A');
    expect(message.items[0]).toHaveProperty('authorUsername', 'synthetic_author_a');
    expect(parseCaptureMessage(message)).toEqual(message);
  });

  it('refuses malformed messages', () => {
    const good = toCaptureMessage(
      { platform: 'pinterest', items: [], hasNextPage: null },
      { ...context, pageUrl: 'https://www.pinterest.com/someone/recipes/', viewer: 'someone' },
    );
    expect(parseCaptureMessage(good)).toEqual(good);
    for (const bad of [
      { ...good, kind: 'other' },
      { ...good, platform: 'web' },
      { ...good, capture: 'magic' },
      { ...good, pageUrl: '' },
      { ...good, docId: '' },
      { ...good, docId: 'x'.repeat(65) },
      { ...good, seq: -1 },
      { ...good, seq: 1.5 },
      { ...good, sentAt: -1 },
      { ...good, viewer: '' },
      { ...good, viewer: 3 },
      { ...good, items: 'nope' },
    ])
      expect(parseCaptureMessage(bad)).toBeNull();
  });
});

describe('external messages (SPA → worker, C9)', () => {
  it('parses the five C9 messages', () => {
    const code = 'A'.repeat(43);
    expect(parseExternalMessage({ type: 'shelfy.ping' })).toEqual({ type: EXTERNAL.ping });
    expect(parseExternalMessage({ type: 'shelfy.pair', code })).toEqual({
      type: EXTERNAL.pair,
      code,
    });
    expect(
      parseExternalMessage({ type: 'shelfy.sync.start', target: { platform: 'instagram' } }),
    ).toEqual({ type: EXTERNAL.syncStart, target: { platform: 'instagram' } });
    expect(
      parseExternalMessage({
        type: 'shelfy.sync.start',
        target: { platform: 'pinterest', collectionId: 12 },
      }),
    ).toEqual({ type: EXTERNAL.syncStart, target: { platform: 'pinterest', collectionId: 12 } });
    expect(parseExternalMessage({ type: 'shelfy.sync.stop', platform: 'twitter' })).toEqual({
      type: EXTERNAL.syncStop,
      platform: 'twitter',
    });
    expect(parseExternalMessage({ type: 'shelfy.tasks.poll' })).toEqual({
      type: EXTERNAL.tasksPoll,
    });
  });

  it('refuses unknown types and malformed fields', () => {
    for (const bad of [
      null,
      'shelfy.ping',
      { type: 'shelfy.unknown' },
      { type: 'shelfy.pair' },
      { type: 'shelfy.pair', code: 'short' },
      { type: 'shelfy.pair', code: 'has spaces in it, sixteen+' },
      { type: 'shelfy.pair', code: 'A'.repeat(129) },
      { type: 'shelfy.sync.start', target: { platform: 'web' } },
      { type: 'shelfy.sync.start', target: { platform: 'instagram', collectionId: 0 } },
      { type: 'shelfy.sync.start', target: { platform: 'instagram', collectionId: '12' } },
      { type: 'shelfy.sync.stop', platform: 'tiktok' },
    ])
      expect(parseExternalMessage(bad)).toBeNull();
  });
});

describe('settings patches (panel → worker)', () => {
  it('accepts toggles, folder mapping and Access headers, and clears headers with null', () => {
    expect(
      parseSettingsPatch({
        passive: { instagram: false },
        passiveFolders: false,
        access: { clientId: 'id.access', clientSecret: 'secret-value' },
      }),
    ).toEqual({
      passive: { instagram: false },
      passiveFolders: false,
      access: { clientId: 'id.access', clientSecret: 'secret-value' },
    });
    expect(parseSettingsPatch({ access: null })).toEqual({ access: null });
    expect(parseSettingsPatch({})).toEqual({});
  });

  it('refuses unknown platforms, non-booleans and malformed headers', () => {
    for (const bad of [
      null,
      { passive: { web: true } },
      { passive: { instagram: 'yes' } },
      { passiveFolders: 1 },
      { access: { clientId: 'has space', clientSecret: 'x' } },
      { access: { clientId: 'id', clientSecret: '' } },
      { access: { clientId: 'id' } },
      { access: 'id:secret' },
    ])
      expect(parseSettingsPatch(bad)).toBeNull();
  });
});

describe('createRelay (bridge core)', () => {
  type Send = (message: unknown) => Promise<unknown>;
  function setup(impl: Send = async () => ({ ok: true }), viewer: string | null = null) {
    const send = vi.fn(impl);
    const win = {} as Window;
    const warn = vi.fn();
    const scheduled: Array<() => void> = [];
    const relay = createRelay(win, {
      send,
      pageUrl: () => PAGE,
      now: () => 42,
      warn,
      docId: 'doc-1',
      viewer: (platform) => (platform === 'pinterest' ? viewer : null),
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
    expect(send.mock.calls[0][0]).toEqual({
      kind: MSG.capture,
      platform: 'instagram',
      items: [hookItem],
      hasNextPage: true,
      pageUrl: PAGE,
      docId: 'doc-1',
      seq: 0,
      capture: 'passive',
      viewer: null,
      sentAt: 42,
    });
  });

  it('numbers messages per document and tags those posted inside a scope', () => {
    const { send, post } = setup();
    post({ type: SCOPE_MESSAGE, phase: 'start', source: 'ssr', id: 's1', detail: {} });
    post(intercept);
    post({ type: SCOPE_MESSAGE, phase: 'end', source: 'ssr', id: 's1', detail: {} });
    post(intercept);
    const messages = send.mock.calls.map(([m]) => m as { seq: number; capture: string });
    expect(messages.map((m) => [m.seq, m.capture])).toEqual([
      [0, 'ssr'],
      [1, 'passive'],
    ]);
  });

  it('attaches the Pinterest viewer to Pinterest messages only', () => {
    const { send, post } = setup(undefined, 'someone');
    post({ ...intercept, platform: 'pinterest' });
    post(intercept);
    const viewers = send.mock.calls.map(([m]) => (m as { viewer: string | null }).viewer);
    expect(viewers).toEqual(['someone', null]);
  });

  it('forwards census counts with the page URL', () => {
    const { send, post } = setup();
    post({ type: CENSUS_MESSAGE, counts: { 'instagram|graphql X': 2 } });
    expect(send).toHaveBeenCalledWith({
      kind: MSG.census,
      counts: { 'instagram|graphql X': 2 },
      pageUrl: PAGE,
      sentAt: 42,
    });
  });

  it('retries a failed delivery under the same sequence number, then gives up', async () => {
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
    expect(new Set(send.mock.calls.map(([m]) => (m as { seq: number }).seq))).toEqual(new Set([0]));
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
