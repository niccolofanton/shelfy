// @vitest-environment jsdom
//
// Direct video URLs in the capture hook (electron/webview-injected.ts), web port P2-05, with
// SPIKE-9's picks:
// - X: the highest-bitrate MP4 of `video_info.variants` at 1080p or less becomes
//   `media[].videoUrl`, for videos and GIFs; Pinterest: the `V_720P` MP4, else the widest MP4,
//   while `url` keeps the desktop's own pick;
// - hostile payloads (huge arrays, non-string URLs, deep nesting) stay inside the hook's
//   existing bounds: the 200,000-node GraphQL walk, the depth of 50 and the 5,000-key store;
// - the desktop sees none of it, because its sanitizer drops `videoUrl`. The desktop build of
//   the hook is checked in webview-injected-desktop.test.ts (esbuild cannot run under jsdom).
// Every payload is synthetic, and no request leaves the test.

import { beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { sanitizeInterceptedBatch } from '../../src/lib/browserSanitize';

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

/** The page fetches `url` and gets `body` (raw text when a string). */
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

const BOOKMARKS_URL = 'https://x.com/i/api/graphql/SYNTHETIC/Bookmarks?variables=%7B%7D';
const PIN_FEED_URL =
  'https://www.pinterest.com/resource/BoardFeedResource/get/?source_url=%2Fsynthetic%2Fboard%2F';
const IG_GRAPHQL_URL = 'https://www.instagram.com/graphql/query';

const XV = 'https://video.twimg.com/ext_tw_video/1800000000000000002/pu/vid/avc1';
const xPoster = 'https://pbs.twimg.com/ext_tw_video_thumb/1800000000000000002/pu/img/Poster.jpg';
const xPhoto = 'https://pbs.twimg.com/media/SyntheticPhoto.jpg';
const PV = 'https://v1.pinimg.com/videos/mc';
const pinCover = 'https://i.pinimg.com/originals/aa/bb/cc/synthetic.jpg';
const IG_CDN = 'https://scontent-synth1-1.cdninstagram.com';
const igJpg = (name: string): string => `${IG_CDN}/v/t51.2885-15/${name}.jpg?oe=68F00000`;
const igMp4 = (name: string): string => `${IG_CDN}/o1/v/t16/f2/m69/${name}.mp4?oe=68F00000`;

function tweet(id: string, media: unknown[]): Record<string, unknown> {
  return {
    entryId: `tweet-${id}`,
    content: {
      entryType: 'TimelineTimelineItem',
      itemContent: {
        itemType: 'TimelineTweet',
        tweet_results: {
          result: {
            __typename: 'Tweet',
            rest_id: id,
            core: {
              user_results: {
                result: { core: { screen_name: 'synthetic_x', name: 'Synthetic X' } },
              },
            },
            legacy: {
              id_str: id,
              full_text: 'Synthetic tweet',
              created_at: 'Fri, 01 Aug 2025 19:57:38 +0000',
              extended_entities: { media },
            },
          },
        },
      },
    },
  };
}

function bookmarksPage(...entries: unknown[]): Record<string, unknown> {
  const cursor = { entryId: 'cursor-bottom-1', content: { cursorType: 'Bottom', value: 'NEXT' } };
  return {
    data: {
      bookmark_timeline_v2: {
        timeline: { instructions: [{ type: 'TimelineAddEntries', entries: [...entries, cursor] }] },
      },
    },
  };
}

const xVideo = (variants: unknown, type = 'video'): Record<string, unknown> => ({
  type,
  media_url_https: xPoster,
  video_info: { aspect_ratio: [16, 9], variants },
});

/** The media the hook relays for one bookmarked tweet with these media entities. */
async function tweetMedia(...media: unknown[]): Promise<Media[]> {
  send.mockClear();
  const [[items]] = await pageFetch(
    BOOKMARKS_URL,
    bookmarksPage(tweet('1800000000000000002', media)),
  );
  return items[0].media;
}

function pinPage(...pins: unknown[]): Record<string, unknown> {
  // '-end-' ends the feed outright, so no stuck-cursor state carries over between tests.
  return { resource_response: { data: pins }, resource: { options: { bookmarks: ['-end-'] } } };
}

function pin(id: string, over: Record<string, unknown>): Record<string, unknown> {
  return {
    id,
    type: 'pin',
    title: 'Synthetic pin',
    created_at: 'Fri, 01 Aug 2025 19:57:38 +0000',
    pinner: { username: 'synthetic_pinner' },
    images: { orig: { url: pinCover } },
    ...over,
  };
}

/** The item the hook relays for one pin. */
async function pinItem(over: Record<string, unknown>): Promise<Item> {
  send.mockClear();
  const [[items]] = await pageFetch(PIN_FEED_URL, pinPage(pin('900000000000000010', over)));
  return items[0];
}

function igRestVideo(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: '3400000000000000002_9000000002',
    code: 'C8vOfxsVAAC',
    media_type: 2,
    taken_at: 1758100000,
    image_versions2: { candidates: [{ url: igJpg('reel') }] },
    video_versions: [{ width: 720, height: 1280, url: igMp4('reel') }],
    ...over,
  };
}

// ── X ───────────────────────────────────────────────────────────────────────

describe('X video_info.variants → media[].videoUrl', () => {
  it('a video takes the highest-bitrate MP4; the HLS playlist is ignored', async () => {
    const media = await tweetMedia(
      xVideo([
        { content_type: 'application/x-mpegURL', url: `${XV}/pl/Synthetic.m3u8?tag=12` },
        { bitrate: 256000, content_type: 'video/mp4', url: `${XV}/480x270/Synthetic.mp4?tag=12` },
        { bitrate: 2176000, content_type: 'video/mp4', url: `${XV}/1280x720/Synthetic.mp4?tag=12` },
        { bitrate: 832000, content_type: 'video/mp4', url: `${XV}/640x360/Synthetic.mp4?tag=12` },
      ]),
    );
    expect(media).toEqual([
      { type: 'video', url: xPoster, videoUrl: `${XV}/1280x720/Synthetic.mp4?tag=12` },
    ]);
  });

  it('stays at 1080p or less (short side), unless every MP4 is larger', async () => {
    const at = (size: string): string => `${XV}/${size}/Synthetic.mp4?tag=12`;
    const mp4 = (bitrate: number, size: string): Record<string, unknown> => ({
      bitrate,
      content_type: 'video/mp4',
      url: at(size),
    });
    expect(
      (
        await tweetMedia(
          xVideo([
            mp4(2176000, '1280x720'),
            mp4(12000000, '3840x2160'),
            mp4(5000000, '1920x1080'),
            mp4(9000000, '2560x1440'),
          ]),
        )
      )[0].videoUrl,
    ).toBe(at('1920x1080'));
    // Portrait: the short side is the width.
    expect(
      (await tweetMedia(xVideo([mp4(9000000, '1440x2560'), mp4(5000000, '1080x1920')])))[0]
        .videoUrl,
    ).toBe(at('1080x1920'));
    // Nothing at 1080p or less: the highest bitrate of any size.
    expect(
      (await tweetMedia(xVideo([mp4(9000000, '2560x1440'), mp4(12000000, '3840x2160')])))[0]
        .videoUrl,
    ).toBe(at('3840x2160'));
  });

  it('a GIF takes its single MP4, whose bitrate is 0', async () => {
    const gif = 'https://video.twimg.com/tweet_video/SyntheticGif.mp4';
    const media = await tweetMedia(
      xVideo([{ bitrate: 0, content_type: 'video/mp4', url: gif }], 'animated_gif'),
    );
    expect(media).toEqual([{ type: 'video', url: xPoster, videoUrl: gif }]);
  });

  it('photos get no videoUrl key, next to a video in the same tweet', async () => {
    const mp4 = `${XV}/720x720/Synthetic.mp4`;
    const media = await tweetMedia(
      { type: 'photo', media_url_https: xPhoto, video_info: { variants: [] } },
      xVideo([{ bitrate: 950000, content_type: 'video/mp4', url: mp4 }]),
    );
    expect(media).toEqual([
      { type: 'image', url: xPhoto },
      { type: 'video', url: xPoster, videoUrl: mp4 },
    ]);
    expect(media[0]).not.toHaveProperty('videoUrl');
  });

  it('skips variants that are not usable MP4 URLs', async () => {
    const [slide] = await tweetMedia(
      xVideo([
        null,
        'https://video.twimg.com/bare-string.mp4',
        { bitrate: 9e9, content_type: 'video/mp4' },
        { bitrate: 9e9, content_type: 'video/mp4', url: 7 },
        { bitrate: 9e9, content_type: 'video/mp4', url: { href: `${XV}/object.mp4` } },
        { bitrate: 9e9, content_type: 'video/mp4', url: 'data:video/mp4;base64,AAAA' },
        { bitrate: 9e9, content_type: 7, url: `${XV}/typeless.mp4` },
        { bitrate: 9e9, content_type: 'video/webm', url: `${XV}/a.webm` },
        { bitrate: 'high', content_type: 'video/mp4', url: `${XV}/string_bitrate.mp4` },
        { bitrate: 100, content_type: 'VIDEO/MP4', url: `${XV}/valid.mp4` },
      ]),
    );
    expect(slide.videoUrl).toBe(`${XV}/valid.mp4`);

    // Nothing usable: the slide keeps the desktop shape, without a videoUrl key.
    for (const variants of [
      undefined,
      'x',
      {},
      [],
      [{ content_type: 'application/x-mpegURL', url: `${XV}/pl/a.m3u8` }],
    ])
      expect(await tweetMedia(xVideo(variants))).toEqual([{ type: 'video', url: xPoster }]);
    expect(
      await tweetMedia({ type: 'video', media_url_https: xPoster, video_info: 'not an object' }),
    ).toEqual([{ type: 'video', url: xPoster }]);
  });
});

// ── Pinterest ───────────────────────────────────────────────────────────────

describe('Pinterest direct MP4 → media[].videoUrl', () => {
  it('the V_720P MP4 is also videoUrl, and url is unchanged', async () => {
    const mp4 = `${PV}/720p/aa/bb/cc/synthetic.mp4`;
    const item = await pinItem({
      videos: {
        video_list: { V_HLSV4: { url: `${PV}/hls/aa/bb/cc/synthetic.m3u8` }, V_720P: { url: mp4 } },
      },
    });
    expect(item.mediaType).toBe('video');
    expect(item.thumbnailUrl).toBe(pinCover);
    expect(item.media).toEqual([{ type: 'video', url: mp4, videoUrl: mp4 }]);
  });

  it('without V_720P, the widest MP4; url keeps the desktop pick', async () => {
    const p480 = `${PV}/480p/aa/bb/cc/synthetic.mp4`;
    const exp = `${PV}/iht/expMp4/aa/bb/cc/synthetic.mp4`;
    const item = await pinItem({
      videos: {
        video_list: {
          V_480P: { url: p480, width: 480 },
          V_EXP7: { url: exp, width: 720 },
          V_HLSV4: { url: `${PV}/hls/aa/bb/cc/synthetic.m3u8`, width: 1080 },
        },
      },
    });
    expect(item.media).toEqual([{ type: 'video', url: p480, videoUrl: exp }]);

    // Keys are compared without underscores or case.
    const camel = `${PV}/720p/aa/bb/cc/camel.mp4`;
    const wider = `${PV}/1080p/aa/bb/cc/wider.mp4`;
    const pin2 = await pinItem({
      videos: {
        video_list: { V_EXP7: { url: wider, width: 1080 }, v720P: { url: camel, width: 720 } },
      },
    });
    expect(pin2.media).toEqual([{ type: 'video', url: wider, videoUrl: camel }]);
  });

  it('an HLS-only pin keeps its playlist in url, without videoUrl', async () => {
    const m3u8 = `${PV}/hls/aa/bb/cc/synthetic.m3u8`;
    const item = await pinItem({ videos: { video_list: { V_HLSV4: { url: m3u8 } } } });
    expect(item.media).toEqual([{ type: 'video', url: m3u8 }]);
  });

  it('reads the path, not the query: an MP4 with a query string counts, a playlist does not', async () => {
    const mp4 = `${PV}/720p/aa/bb/cc/synthetic.mp4?x=1#t=2`;
    expect((await pinItem({ videos: { video_list: { V_720P: { url: mp4 } } } })).media).toEqual([
      { type: 'video', url: mp4, videoUrl: mp4 },
    ]);
    const fake = `${PV}/hls/aa/bb/cc/synthetic.m3u8?name=a.mp4`;
    expect((await pinItem({ videos: { video_list: { V_720P: { url: fake } } } })).media).toEqual([
      { type: 'video', url: fake },
    ]);
  });

  it('story pins: a video block gets videoUrl, an image page does not', async () => {
    const mp4 = `${PV}/720p/dd/ee/ff/story.mp4`;
    const item = await pinItem({
      story_pin_data: {
        pages: [
          { blocks: [{ video: { video_list: { V_720P: { url: mp4 } } } }] },
          { blocks: [{ image: { images: { orig: { url: pinCover } } } }] },
        ],
      },
    });
    expect(item.mediaType).toBe('carousel');
    expect(item.media).toEqual([
      { type: 'video', url: mp4, videoUrl: mp4 },
      { type: 'image', url: pinCover },
    ]);
  });

  it('image pins get no videoUrl key; a non-string url stays as the desktop relays it', async () => {
    const image = await pinItem({});
    expect(image.media).toEqual([{ type: 'image', url: pinCover }]);
    expect(image.media[0]).not.toHaveProperty('videoUrl');
    const odd = await pinItem({ videos: { video_list: { V_720P: { url: 7 } } } });
    expect(odd.media).toEqual([{ type: 'video', url: 7 }]);
  });
});

// ── Hostile payloads ────────────────────────────────────────────────────────

describe('hostile payloads stay inside the hook bounds', () => {
  const ladder = (n: number, entry: (i: number) => unknown): unknown[] =>
    Array.from({ length: n }, (_, i) => entry(i));

  it('reads only the first 64 variants of an array, however long', async () => {
    // Instagram: 63 unusable entries, then 20,000 usable ones; the 64th entry is the pick.
    emitRest({
      items: [
        igRestVideo({
          video_versions: [
            ...ladder(63, () => ({ url: 7 })),
            ...ladder(20_000, (i) => ({ url: igMp4(`v${i + 63}`) })),
          ],
        }),
      ],
    });
    expect(relays()[0][0][0].media[0].videoUrl).toBe(igMp4('v63'));

    // X: in 20,000 variants, each beats every one before it, and the 64th is the pick.
    const [slide] = await tweetMedia(
      xVideo(
        ladder(20_000, (i) => ({
          bitrate: i,
          content_type: 'video/mp4',
          url: `${XV}/v${i}.mp4`,
        })),
      ),
    );
    expect(slide.videoUrl).toBe(`${XV}/v63.mp4`);

    // A usable entry past the first 64 is never reached.
    send.mockClear();
    emitRest({
      items: [
        igRestVideo({
          video_versions: [...ladder(64, () => ({ url: 7 })), { url: igMp4('late') }],
        }),
      ],
    });
    expect(relays()[0][0][0].media[0]).toEqual({ type: 'video', url: igJpg('reel') });
  });

  it('a page of hostile posts is parsed quickly, and junk never becomes a videoUrl', () => {
    const hostileVersions = [
      ...ladder(500, (i) => [null, 7, 'x', { url: 9 }, { url: { deep: { deeper: [] } } }][i % 5]),
      { url: igMp4('valid') },
    ];
    const items = ladder(2_000, (i) =>
      igRestVideo({
        id: `syn_hostile_${i}`,
        code: `SYNHOSTILE${i}`,
        media_type: 8,
        carousel_media: ladder(10, () => ({
          media_type: 2,
          image_versions2: { candidates: [{ url: igJpg(`c${i}`) }] },
          video_versions: hostileVersions,
        })),
      }),
    );
    const started = performance.now();
    const summary = emitRest({ items });
    const elapsed = performance.now() - started;
    expect(summary.count).toBe(2_000);
    // Each carousel child scans 64 junk entries and stops before the valid one.
    const slides = relays()[0][0].flatMap((it) => it.media);
    expect(slides).toHaveLength(20_000);
    expect(slides.filter((m) => 'videoUrl' in m)).toEqual([]);
    expect(slides[0]).toEqual({ type: 'video', url: igJpg('c0') });
    expect(elapsed).toBeLessThan(1_000);
  });

  it('deep nesting: the GraphQL walk still stops below depth 50', async () => {
    const edges = { edges: [{ node: { shortcode: 'C8vOfxsVAAZ', display_url: igJpg('deep') } }] };
    // `{"a":{"a":…{edges}…}}`, built as text so that the test itself never recurses.
    const nested = (levels: number): string =>
      '{"a":'.repeat(levels) + JSON.stringify(edges) + '}'.repeat(levels);
    // The holder of `edges` sits at depth `levels` below the response root.
    expect(await pageFetch(IG_GRAPHQL_URL, nested(50))).toHaveLength(1);
    send.mockClear();
    expect(await pageFetch(IG_GRAPHQL_URL, nested(51))).toEqual([]);
    expect(await pageFetch(IG_GRAPHQL_URL, nested(100_000))).toEqual([]);
  });

  it('wide payloads: the GraphQL walk still visits at most 200,000 nodes', async () => {
    const page = (junk: number): string =>
      `{"data":{"junk":[${'{},'.repeat(junk - 1)}{}],"saved":{"edges":[{"node":{"shortcode":"C8vOfxsVAAW","display_url":"${igJpg('wide')}"}}]}}}`;
    // Root, data, junk array, the junk objects, then the holder of `edges`: 200,000 in all.
    expect(await pageFetch(IG_GRAPHQL_URL, page(199_996))).toHaveLength(1);
    send.mockClear();
    expect(await pageFetch(IG_GRAPHQL_URL, page(199_997))).toEqual([]);
  });

  it('the captured store keeps at most 5,000 keys; the growth signal keeps counting', () => {
    const before = window.__ssCapturedOrder?.length ?? 0;
    const items = ladder(3_000, (i) => igRestVideo({ id: `syn_store_${i}`, code: `SYNSTORE${i}` }));
    expect(emitRest({ items }).count).toBe(3_000);
    const store = window.__ssCapturedItems ?? {};
    expect(Object.keys(store)).toHaveLength(5_000);
    // Each post adds two keys (shortcode and id); the oldest keys are evicted first.
    expect(window.__ssCapturedOrder).toHaveLength(before + 6_000);
    expect(store['SYNSTORE2999']).toBeDefined();
    expect(store['syn_store_0']).toBeUndefined();
  });
});

// ── The desktop sees none of it ─────────────────────────────────────────────

describe('desktop parity', () => {
  it('the desktop sanitizer drops videoUrl, so desktop data is unchanged', async () => {
    emitRest({ items: [igRestVideo()] });
    const [[instagram]] = relays();
    send.mockClear();
    const [[twitter]] = await pageFetch(
      BOOKMARKS_URL,
      bookmarksPage(
        tweet('1800000000000000003', [
          xVideo([{ bitrate: 1, content_type: 'video/mp4', url: `${XV}/a.mp4` }]),
        ]),
      ),
    );
    send.mockClear();
    const mp4 = `${PV}/720p/aa/bb/cc/parity.mp4`;
    const [[pinterest]] = await pageFetch(
      PIN_FEED_URL,
      pinPage(pin('900000000000000011', { videos: { video_list: { V_720P: { url: mp4 } } } })),
    );
    const batches: Array<[Item[], string]> = [
      [instagram, 'instagram'],
      [twitter, 'twitter'],
      [pinterest, 'pinterest'],
    ];

    for (const [items, platform] of batches) {
      expect(items[0].media[0].videoUrl).toBeTruthy();
      const before = structuredClone(items);
      for (const item of before) for (const m of item.media) delete m.videoUrl;
      const sanitized = sanitizeInterceptedBatch(items, platform);
      expect(sanitized).toEqual(sanitizeInterceptedBatch(before, platform));
      for (const item of sanitized)
        for (const m of item.media) expect(Object.keys(m).sort()).toEqual(['type', 'url']);
    }
  });
});
