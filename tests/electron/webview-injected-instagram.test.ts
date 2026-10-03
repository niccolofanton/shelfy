// @vitest-environment jsdom
//
// Instagram in the capture hook (electron/webview-injected.ts), web port P2-05: video slides
// carry their direct URL as `media[].videoUrl` (REST `video_versions[0]`, the pick SPIKE-9
// measured, and GraphQL `video_url`), and the REST parse-and-emit entry
// (window.__ssEmitInstagramRest) lets the extension's MAIN-world helpers relay a body the
// passive hook does not capture (P2-17). Every payload is synthetic, and no request leaves the
// test.

import { readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';

interface Media {
  type: string;
  url: unknown;
  videoUrl?: string;
}
interface Item {
  id: string;
  shortcode: string;
  mediaType: string;
  thumbnailUrl: string;
  media: Media[];
}
type Relay = [items: Item[], hasNextPage: boolean | null, platform: string];

const send = vi.fn();
let served: unknown = {};

const relays = (): Relay[] => send.mock.calls as Relay[];

/** The page fetches `url` and gets `body`; the hook sees the response like any other. */
async function pageFetch(url: string, body: unknown): Promise<Relay[]> {
  served = body;
  await window.fetch(url);
  return relays();
}

function emitRest(body: unknown): { count: number; ids: string[]; shortcodes: string[] } {
  const entry = window.__ssEmitInstagramRest;
  if (!entry) throw new Error('the hook did not install its Instagram REST entry');
  return entry(body);
}

beforeAll(async () => {
  window.__socialSavedBridge = { send };
  // The page's own network, which the hook keeps as its original fetch.
  window.fetch = vi.fn(
    async () =>
      new Response(typeof served === 'string' ? served : JSON.stringify(served), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
  ) as typeof window.fetch;
  await import('../../electron/webview-injected');
});

beforeEach(() => {
  send.mockClear();
});

// ── Synthetic payloads ──────────────────────────────────────────────────────

const CDN = 'https://scontent-synth1-1.cdninstagram.com';
const SIGNED = '?oh=00_SYNTHETIC&oe=68F00000';
const jpg = (name: string): string => `${CDN}/v/t51.2885-15/${name}.jpg${SIGNED}`;
const mp4 = (name: string): string => `${CDN}/o1/v/t16/f2/m69/${name}.mp4${SIGNED}`;

const SAVED_URL = 'https://www.instagram.com/api/v1/feed/saved/posts/?max_id=';
const GRAPHQL_URL = 'https://www.instagram.com/graphql/query';
const MEDIA_INFO_URL = 'https://www.instagram.com/api/v1/media/3400000000000000002/info/';

/** A REST media object: a saved-feed `items[].media`, or a media-info `items[]` entry. */
function restMedia(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: '3400000000000000002_9000000002',
    pk: '3400000000000000002',
    code: 'C8vOfxsVAAC',
    media_type: 2,
    taken_at: 1758100000,
    user: { username: 'synthetic_author' },
    caption: { text: 'Synthetic caption' },
    image_versions2: { candidates: [{ width: 1080, height: 1920, url: jpg('reel') }] },
    video_versions: [
      { type: 101, width: 720, height: 1280, url: mp4('reel_720') },
      { type: 102, width: 480, height: 854, url: mp4('reel_480') },
      { type: 103, width: 480, height: 854, url: mp4('reel_480_baseline') },
    ],
    ...over,
  };
}

function savedPage(...media: Array<Record<string, unknown>>): Record<string, unknown> {
  return {
    items: media.map((m) => ({ media: m })),
    more_available: false,
    next_max_id: null,
    status: 'ok',
  };
}

/** The body of GET /api/v1/media/<pk>/info/: the media objects are not wrapped. */
function mediaInfo(...media: Array<Record<string, unknown>>): Record<string, unknown> {
  return {
    items: media,
    num_results: media.length,
    more_available: false,
    auto_load_more_enabled: false,
    status: 'ok',
  };
}

/** The first slide the hook relays for a single post with these `video_versions`. */
function firstSlide(videoVersions: unknown): Media {
  send.mockClear();
  emitRest(mediaInfo(restMedia({ video_versions: videoVersions })));
  return relays()[0][0][0].media[0];
}

// ── REST ────────────────────────────────────────────────────────────────────

describe('REST video_versions → media[].videoUrl', () => {
  it('a video keeps its poster in url and video_versions[0] in videoUrl', async () => {
    const [[items, hasNextPage, platform]] = await pageFetch(SAVED_URL, savedPage(restMedia()));
    expect(platform).toBe('instagram');
    expect(hasNextPage).toBe(false);
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({
      id: '3400000000000000002_9000000002',
      shortcode: 'C8vOfxsVAAC',
      mediaType: 'video',
      thumbnailUrl: jpg('reel'),
    });
    expect(items[0].media).toEqual([
      { type: 'video', url: jpg('reel'), videoUrl: mp4('reel_720') },
    ]);
  });

  it('takes video_versions[0], as SPIKE-9 measured, even when a later entry is larger', () => {
    expect(
      firstSlide([
        { width: 480, height: 854, url: mp4('first_listed') },
        { width: 1080, height: 1920, url: mp4('larger') },
      ]).videoUrl,
    ).toBe(mp4('first_listed'));
    // The URL is kept verbatim, with its own `oe`: a video can expire before its poster.
    const signed = `${CDN}/o1/v/t16/f2/m69/early.mp4?efg=SYNTHETIC&oh=00_SYNTHETIC&oe=68E00000`;
    expect(firstSlide([{ url: signed }]).videoUrl).toBe(signed);
  });

  it('passes over entries that are not usable URLs', () => {
    expect(
      firstSlide([
        null,
        mp4('a_bare_string'),
        42,
        { width: 4000, height: 4000 },
        { width: 4000, height: 4000, url: 42 },
        { width: 4000, height: 4000, url: { href: mp4('object') } },
        { width: 4000, height: 4000, url: [mp4('array')] },
        { width: 4000, height: 4000, url: '' },
        { width: 4000, height: 4000, url: 'javascript:alert(1)' },
        { width: 4000, height: 4000, url: 'ftp://cdn.example/a.mp4' },
        { width: 4000, height: 4000, url: `${CDN}/${'a'.repeat(4100)}.mp4` },
        { width: 320, height: 320, url: mp4('first_usable') },
        { width: 720, height: 1280, url: mp4('second_usable') },
      ]).videoUrl,
    ).toBe(mp4('first_usable'));

    // Nothing usable: the slide keeps the desktop shape, without a videoUrl key.
    for (const versions of [undefined, null, 'x', {}, [], [null, { url: 7 }]])
      expect(firstSlide(versions)).toEqual({ type: 'video', url: jpg('reel') });
  });

  it('image posts and image slides get no videoUrl key', () => {
    emitRest(mediaInfo(restMedia({ media_type: 1, video_versions: [{ url: mp4('stray') }] })));
    const [items] = relays()[0];
    expect(items[0].mediaType).toBe('image');
    expect(items[0].media).toEqual([{ type: 'image', url: jpg('reel') }]);
    expect(items[0].media[0]).not.toHaveProperty('videoUrl');
  });

  it('carousel children: each video child carries its own videoUrl', () => {
    emitRest(
      mediaInfo(
        restMedia({
          media_type: 8,
          video_versions: undefined,
          carousel_media: [
            { media_type: 1, image_versions2: { candidates: [{ url: jpg('slide_0') }] } },
            {
              media_type: 2,
              image_versions2: { candidates: [{ url: jpg('slide_1') }] },
              video_versions: [
                { width: 1080, height: 1080, url: mp4('slide_1_1080') },
                { width: 640, height: 640, url: mp4('slide_1_640') },
              ],
            },
            { media_type: 2, image_versions2: { candidates: [{ url: jpg('slide_2') }] } },
          ],
        }),
      ),
    );
    const [items] = relays()[0];
    expect(items[0].mediaType).toBe('carousel');
    expect(items[0].media).toEqual([
      { type: 'image', url: jpg('slide_0') },
      { type: 'video', url: jpg('slide_1'), videoUrl: mp4('slide_1_1080') },
      { type: 'video', url: jpg('slide_2') },
    ]);
  });

  it('a video slide without a poster is still dropped, as on the desktop', () => {
    emitRest(mediaInfo(restMedia({ image_versions2: undefined })));
    const [items] = relays()[0];
    expect(items[0].media).toEqual([]);
    expect(items[0].thumbnailUrl).toBe('');
  });
});

// ── GraphQL ─────────────────────────────────────────────────────────────────

describe('GraphQL video_url → media[].videoUrl', () => {
  it('reads video_url on video nodes and sidecar children only', async () => {
    const owner = { username: 'synthetic_author' };
    const body = {
      data: {
        user: {
          edge_saved_media: {
            page_info: { has_next_page: true, end_cursor: 'SYNTHETIC_CURSOR' },
            edges: [
              {
                node: {
                  __typename: 'GraphVideo',
                  id: '3400000000000000010',
                  shortcode: 'C8vOfxsVAAK',
                  is_video: true,
                  display_url: jpg('graph_video'),
                  video_url: mp4('graph_video'),
                  taken_at_timestamp: 1758200000,
                  owner,
                },
              },
              {
                node: {
                  __typename: 'GraphSidecar',
                  id: '3400000000000000011',
                  shortcode: 'C8vOfxsVAAL',
                  display_url: jpg('graph_sidecar'),
                  taken_at_timestamp: 1758200001,
                  owner,
                  edge_sidecar_to_children: {
                    edges: [
                      { node: { __typename: 'GraphImage', display_url: jpg('child_0') } },
                      {
                        node: {
                          __typename: 'GraphVideo',
                          is_video: true,
                          display_url: jpg('child_1'),
                          video_url: mp4('child_1'),
                        },
                      },
                      {
                        node: {
                          __typename: 'GraphVideo',
                          is_video: true,
                          display_url: jpg('child_2'),
                          video_url: 12345,
                        },
                      },
                    ],
                  },
                },
              },
              {
                node: {
                  __typename: 'GraphImage',
                  id: '3400000000000000012',
                  shortcode: 'C8vOfxsVAAM',
                  display_url: jpg('graph_image'),
                  video_url: mp4('stray'),
                  taken_at_timestamp: 1758200002,
                  owner,
                },
              },
            ],
          },
        },
      },
    };
    const [[items, hasNextPage]] = await pageFetch(GRAPHQL_URL, body);
    expect(hasNextPage).toBe(true);
    expect(items.map((it) => [it.shortcode, it.mediaType])).toEqual([
      ['C8vOfxsVAAK', 'video'],
      ['C8vOfxsVAAL', 'carousel'],
      ['C8vOfxsVAAM', 'image'],
    ]);
    expect(items[0].media).toEqual([
      { type: 'video', url: jpg('graph_video'), videoUrl: mp4('graph_video') },
    ]);
    expect(items[1].media).toEqual([
      { type: 'image', url: jpg('child_0') },
      { type: 'video', url: jpg('child_1'), videoUrl: mp4('child_1') },
      { type: 'video', url: jpg('child_2') },
    ]);
    expect(items[2].media).toEqual([{ type: 'image', url: jpg('graph_image') }]);
  });
});

// ── The REST entry ──────────────────────────────────────────────────────────

describe('window.__ssEmitInstagramRest (REST parse-and-emit entry)', () => {
  it('parses a media-info body and relays it once, with hasNextPage null', () => {
    const summary = emitRest(mediaInfo(restMedia()));
    expect(summary).toEqual({
      count: 1,
      ids: ['3400000000000000002_9000000002'],
      shortcodes: ['C8vOfxsVAAC'],
    });
    expect(send).toHaveBeenCalledTimes(1);
    const [items, hasNextPage, platform] = relays()[0];
    expect(platform).toBe('instagram');
    // more_available=false in a media-info body is not the end of a feed.
    expect(hasNextPage).toBeNull();
    expect(items[0].media).toEqual([
      { type: 'video', url: jpg('reel'), videoUrl: mp4('reel_720') },
    ]);
  });

  it('relays the same item as the passive hook does for a saved-feed page', async () => {
    const [[passive]] = await pageFetch(SAVED_URL, savedPage(restMedia()));
    send.mockClear();
    emitRest(mediaInfo(restMedia()));
    expect(relays()[0][0]).toEqual(passive);
  });

  it('accepts the raw response text as well as the parsed body', () => {
    const body = mediaInfo(restMedia());
    const fromObject = emitRest(body);
    const fromText = emitRest(JSON.stringify(body));
    expect(fromText).toEqual(fromObject);
    expect(relays()[1][0]).toEqual(relays()[0][0]);
  });

  it('ignores anything that is not a REST page, without a GraphQL walk', () => {
    const graphql = {
      data: { edges: [{ node: { shortcode: 'C8vOfxsVAAK', display_url: jpg('graph') } }] },
    };
    const none = { count: 0, ids: [], shortcodes: [] };
    for (const body of [
      undefined,
      null,
      42,
      true,
      'not json',
      '[]',
      [],
      {},
      { items: [] },
      { items: 'x' },
      { feed_items: {} },
      { status: 'fail', message: 'Media not found or unavailable' },
      graphql,
      JSON.stringify(graphql),
    ])
      expect(emitRest(body)).toEqual(none);
    expect(send).not.toHaveBeenCalled();
  });

  it('never throws on a malformed body', () => {
    const none = { count: 0, ids: [], shortcodes: [] };
    const throwing = {
      get items(): unknown {
        throw new Error('hostile getter');
      },
    };
    for (const body of [{ items: [null] }, { items: [{ media: null }] }, throwing])
      expect(emitRest(body)).toEqual(none);
    expect(send).not.toHaveBeenCalled();
  });

  it('stores the relayed post for the selection overlay, as emit() does', () => {
    emitRest(mediaInfo(restMedia()));
    const store = window.__ssCapturedItems ?? {};
    const byShortcode = store['C8vOfxsVAAC'] as unknown as Item;
    expect(byShortcode.media[0].videoUrl).toBe(mp4('reel_720'));
    expect(store['3400000000000000002_9000000002']).toBe(store['C8vOfxsVAAC']);
  });

  it('the media-info endpoint is not captured passively: the desktop does not see it', async () => {
    expect(await pageFetch(MEDIA_INFO_URL, mediaInfo(restMedia()))).toEqual([]);
  });

  it('only the hook mentions the entry: no desktop code calls it', () => {
    const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
    const sources = (dir: string): string[] =>
      readdirSync(dir).flatMap((name) => {
        const path = join(dir, name);
        if (statSync(path).isDirectory()) return sources(path);
        return /\.(?:ts|tsx|js|cjs|mjs)$/.test(name) ? [path] : [];
      });
    const callers = ['src', 'electron']
      .flatMap((dir) => sources(join(root, dir)))
      .filter((path) => readFileSync(path, 'utf8').includes('__ssEmitInstagramRest'))
      .map((path) => relative(root, path).split('\\').join('/'));
    expect(callers).toEqual(['electron/webview-injected.ts']);
  });
});
