import { describe, it, expect } from 'vitest';
import { rgbaToThumbHash } from 'thumbhash';
import {
  countParams,
  listPostsParams,
  mediaRef,
  toCollection,
  toFilterParams,
  toPost,
  toPostSelector,
  toSearchParams,
  toStats,
  webMedia,
} from '../src/api/mapping';
import { apiObject, apiPost, apiSlide } from './fixtures';

// `thumbhash` ships no types; @ui/lib/thumbhash declares the one export it
// uses (thumbHashToRGBA). This merges in the encoder, used only to build a
// real fixture above instead of hand-rolling ThumbHash bytes.
declare module 'thumbhash' {
  export function rgbaToThumbHash(w: number, h: number, rgba: Uint8Array): Uint8Array;
}

describe('gallery query → GET /posts parameters', () => {
  it('maps the desktop filters onto the API', () => {
    const params = listPostsParams(
      {
        platform: 'instagram',
        source: 'social',
        collectionId: 7,
        mediaType: 'video',
        downloadStatus: 'missing',
        aiTagged: 'tagged',
        tag: 'lamp',
        category: 'interior',
        contentType: 'product',
        sortOrder: 'oldest',
      },
      { limit: 50, includeTotal: true },
    );
    expect(params).toEqual({
      platform: 'instagram',
      source: 'social',
      collection: 7,
      mediaType: ['video'],
      stored: 'no',
      aiTagged: 'yes',
      tag: 'lamp',
      category: 'interior',
      contentType: 'product',
      sort: 'oldest',
      limit: 50,
      includeTotal: true,
    });
    expect(
      listPostsParams({ downloadStatus: 'downloaded', aiTagged: 'untagged' }, { limit: 1 }),
    ).toMatchObject({ stored: 'yes', aiTagged: 'no' });
  });

  it('ranks a search by relevance whatever the date order', () => {
    const params = listPostsParams(
      { search: '  lampada ', concepts: ['vetro', ' '], conceptMode: 'and', sortOrder: 'oldest' },
      { limit: 60, cursor: 'c1' },
    );
    expect(params).toEqual({
      q: 'lampada',
      concept: ['vetro'],
      conceptMode: 'and',
      limit: 60,
      cursor: 'c1',
    });
    expect(listPostsParams({ search: '   ', sortOrder: 'newest' }, { limit: 5 })).toEqual({
      limit: 5,
    });
  });

  it('drops values the API does not know and clamps the page size', () => {
    expect(
      listPostsParams({ platform: 'tiktok', mediaType: 'reel', source: 'all' }, { limit: 500 }),
    ).toEqual({ limit: 200 });
    expect(listPostsParams({}, { limit: 0 }).limit).toBe(1);
  });

  it('repeats array parameters and leaves unset ones out', () => {
    const search = toSearchParams({ mediaType: ['image', 'video'], q: undefined, limit: 3 });
    expect(search.toString()).toBe('mediaType=image&mediaType=video&limit=3');
  });

  // P1-14: the full media-type facet (§1.2 #13) and the trash/AI-status filters.
  it('passes through the new facets: every media type, trash and aiStatus', () => {
    for (const mediaType of ['images', 'text', 'website', 'file'] as const) {
      expect(listPostsParams({ mediaType }, { limit: 1 })).toMatchObject({
        mediaType: [mediaType],
      });
    }
    expect(listPostsParams({ trash: true, aiStatus: 'error' }, { limit: 1 })).toMatchObject({
      trash: true,
      aiStatus: 'error',
    });
    expect(listPostsParams({ trash: false }, { limit: 1 }).trash).toBeUndefined();
  });
});

describe('GET /posts/count parameters (P1-14 select-all-matching)', () => {
  it('mirrors listPostsParams minus paging/order', () => {
    expect(
      countParams({
        platform: 'instagram',
        mediaType: 'video',
        downloadStatus: 'missing',
        aiTagged: 'tagged',
        aiStatus: 'done',
        tag: 'lamp',
        trash: true,
      }),
    ).toEqual({
      platform: 'instagram',
      mediaType: ['video'],
      stored: 'no',
      aiTagged: 'yes',
      aiStatus: 'done',
      tag: 'lamp',
      trash: true,
    });
    expect(countParams({})).toEqual({});
  });
});

describe('bulk/trash selector (P1-11/P1-14)', () => {
  it('toFilterParams fills in every field (defaults, not undefined)', () => {
    expect(toFilterParams({ platform: 'instagram', tag: 'lamp', trash: true })).toEqual({
      platform: 'instagram',
      source: null,
      collection: null,
      mediaType: [],
      stored: null,
      aiTagged: null,
      aiStatus: null,
      tag: 'lamp',
      tagMode: null,
      tags: [],
      entity: null,
      category: null,
      contentType: null,
      q: null,
      concept: [],
      conceptMode: null,
      trash: true,
    });
  });

  it('toPostSelector: keys when there is no filter, {filter, exceptKeys} otherwise', () => {
    expect(toPostSelector({ keys: ['ig_1', 'ig_2'] })).toEqual({ keys: ['ig_1', 'ig_2'] });
    expect(toPostSelector({})).toEqual({ keys: [] });

    const withFilter = toPostSelector({
      filter: { platform: 'instagram' },
      exceptKeys: ['ig_3'],
    });
    expect(withFilter.exceptKeys).toEqual(['ig_3']);
    expect(withFilter.filter).toMatchObject({ platform: 'instagram' });

    // No exceptKeys at all when none are given — not an empty array.
    const noExceptions = toPostSelector({ filter: { trash: true } });
    expect(noExceptions).not.toHaveProperty('exceptKeys');
    expect(noExceptions.filter).toMatchObject({ trash: true });
  });
});

describe('media references', () => {
  it('carry the grid rendition after a fragment', () => {
    const withTile = mediaRef(apiObject('ab'));
    expect(withTile).toBe('/media/ab.jpg#/media/ab.g480.webp');
    expect(webMedia.file(withTile)).toBe('/media/ab.jpg');
    expect(webMedia.tile(withTile)).toBe('/media/ab.g480.webp');

    const noTile = mediaRef(apiObject('cd', 'png', false));
    expect(webMedia.file(noTile)).toBe('/media/cd.png');
    expect(webMedia.tile(noTile)).toBe('/media/cd.png');
    expect(mediaRef(null)).toBeNull();
    expect(webMedia.file(null)).toBeNull();
  });

  it('recognize stored copies', () => {
    expect(webMedia.isStored('/media/ab.jpg')).toBe(true);
    expect(webMedia.isStored('https://cdn.example.test/a.jpg')).toBe(false);
    expect(webMedia.isStored(null)).toBe(false);
  });
});

describe('API post → Shelfy.Post', () => {
  it('uses the key as id and keeps the gallery fields', () => {
    const post = toPost(
      apiPost({
        cover: apiObject('ab'),
        aiDescription: 'A lamp',
        aiTags: ['glass'],
        aiStatus: 'done',
        aiAnalyzedAt: 1_790_000_000_500,
        userNote: 'for the hall',
        userTags: ['mine'],
        collectionIds: [3],
      }),
    );
    expect(post).toMatchObject({
      id: 'ig_1001',
      platform: 'instagram',
      text: 'Lampada in vetro soffiato',
      thumbnailUrl: 'https://cdn.example.test/cover.jpg',
      thumbnailPath: '/media/ab.jpg#/media/ab.g480.webp',
      timestamp: '2026-09-01T00:00:00.000Z',
      importedAt: Date.UTC(2026, 9, 1) / 1000,
      aiDescription: 'A lamp',
      aiTags: ['glass'],
      aiAnalyzedAt: 1_790_000_000,
      aiEntities: [],
      aiKeywords: [],
      userNote: 'for the hall',
      userTags: ['mine'],
      collectionIds: [3],
      videoPath: null,
      thumbBlur: null,
    });
    expect(toPost(apiPost({ postedAt: null })).timestamp).toBeNull();
  });

  it('carries deletedAt in seconds, for the Trash view (P1-11/P1-14)', () => {
    expect(toPost(apiPost({ deletedAt: 1_790_000_000_500 })).deletedAt).toBe(1_790_000_000);
    expect(toPost(apiPost({ deletedAt: null })).deletedAt).toBeNull();
  });

  it('shows the poster of a video the server has not kept', () => {
    const post = toPost(
      apiPost({
        mediaType: 'video',
        media: [apiSlide({ kind: 'video', object: apiObject('po') })],
      }),
    );
    expect(post.media).toEqual([
      {
        position: 0,
        type: 'image',
        url: 'https://cdn.example.test/slide.jpg',
        localPath: '/media/po.jpg#/media/po.g480.webp',
      },
    ]);
    expect(post.videoPath).toBeNull();
  });

  it('plays a kept video', () => {
    const post = toPost(
      apiPost({
        mediaType: 'video',
        media: [
          apiSlide({
            kind: 'video',
            object: apiObject('po'),
            videoObject: apiObject('vi', 'mp4', false),
          }),
        ],
      }),
    );
    expect(post.media?.[0]).toMatchObject({ type: 'video', localPath: '/media/vi.mp4' });
    expect(post.videoPath).toBe('/media/vi.mp4');
  });

  it('maps image, page and file slides', () => {
    const post = toPost(
      apiPost({
        mediaType: 'carousel',
        media: [
          apiSlide({ position: 0, kind: 'image', object: apiObject('i1', 'jpg', false) }),
          apiSlide({ position: 1, kind: 'page', sourceUrl: 'https://site.test/about' }),
          apiSlide({ position: 2, kind: 'file', object: apiObject('f1', 'pdf', false) }),
        ],
      }),
    );
    expect(post.media?.map((m) => [m.type, m.url, m.localPath])).toEqual([
      ['image', 'https://cdn.example.test/slide.jpg', '/media/i1.jpg'],
      ['image', 'https://site.test/about', null],
      ['file', 'https://cdn.example.test/slide.jpg', '/media/f1.pdf'],
    ]);
  });

  it('rebuilds a missing X link from the key', () => {
    const tweet = toPost(apiPost({ key: 'x_2001', platform: 'twitter', postUrl: null }));
    expect(tweet.postUrl).toBe('https://x.com/i/web/status/2001');
    const broken = toPost(
      apiPost({ key: 'x_2002', platform: 'twitter', postUrl: 'https://x.com//status/2002' }),
    );
    expect(broken.postUrl).toBe('https://x.com/i/web/status/2002');
    const ok = toPost(
      apiPost({ key: 'x_2003', platform: 'twitter', postUrl: 'https://x.com/a/status/2003' }),
    );
    expect(ok.postUrl).toBe('https://x.com/a/status/2003');
  });

  it('flattens a website capture into the desktop web fields', () => {
    const post = toPost(
      apiPost({
        key: 'web_00a1',
        platform: 'web',
        mediaType: 'website',
        webDomain: 'studio.example.test',
        webCapture: {
          id: 1,
          capturedAt: 1_790_000_000_000,
          requestedUrl: 'https://studio.example.test',
          finalUrl: 'https://studio.example.test/',
          status: 'done',
          partial: false,
          title: 'Studio Example',
          palette: [{ hex: '#112233' }],
          fonts: [{ family: 'Inter' }],
          tech: ['react', 3],
          awards: null,
          meta: { description: 'Design studio' },
          hero: null,
          favicon: null,
        },
      }),
    );
    expect(post).toMatchObject({
      webPalette: [{ hex: '#112233' }],
      webFonts: [{ family: 'Inter' }],
      webTech: ['react'],
      webAwards: [],
      webMeta: { description: 'Design studio', title: 'Studio Example' },
      webCapturedAt: 1_790_000_000,
      webFaviconPath: null,
    });
  });

  it('maps the favicon stored at capture time (plan §1.2 #10), not a g480 fragment', () => {
    const post = toPost(
      apiPost({
        platform: 'web',
        mediaType: 'website',
        webCapture: {
          id: 1,
          capturedAt: 1_790_000_000_000,
          requestedUrl: 'https://studio.example.test',
          finalUrl: 'https://studio.example.test/',
          status: 'done',
          partial: false,
          title: null,
          palette: [],
          fonts: [],
          tech: [],
          awards: null,
          meta: null,
          hero: null,
          favicon: apiObject('fav1', 'png', false),
        },
      }),
    );
    // No g480 rendition exists for a favicon: the object's own URL, not
    // mediaRef's `url#g480Url` tile-fragment form.
    expect(post.webFaviconPath).toBe('/media/fav1.png');
  });

  it('decodes a client-side ThumbHash into thumbBlur; absent stays null', () => {
    expect(toPost(apiPost({ thumbhash: null })).thumbBlur).toBeNull();

    // A real (if tiny) ThumbHash, built the same way crates/media does.
    const w = 2;
    const h = 2;
    const rgba = new Uint8Array(w * h * 4).fill(128);
    for (let i = 0; i < w * h; i++) rgba[i * 4 + 3] = 255; // opaque
    const bytes = rgbaToThumbHash(w, h, rgba);
    let bin = '';
    for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
    const hash = btoa(bin);

    const blur = toPost(apiPost({ thumbhash: hash })).thumbBlur;
    expect(blur).toMatch(/^data:image\/bmp;base64,/);
  });

  it('fills the detail-only fields from a post detail', () => {
    const post = toPost({
      ...apiPost(),
      aiAttempts: 1,
      aiEntities: ['Murano'],
      aiError: null,
      aiKeywords: ['blown glass'],
      aiModel: 'model-a',
      aiNextAt: null,
      aiProvider: null,
      aiSchemaVersion: 2,
      aiWeb: null,
      coverUrlExpiresAt: null,
      entities: [],
      nativeId: '1001',
      tags: [],
    });
    expect(post).toMatchObject({
      aiEntities: ['Murano'],
      aiKeywords: ['blown glass'],
      aiModel: 'model-a',
    });
  });
});

describe('stats and collections', () => {
  it('reads "stored" as the desktop\'s "downloaded"', () => {
    expect(
      toStats({
        total: 10,
        byPlatform: { instagram: 5, twitter: 3, pinterest: 1, web: 1, manual: 0 },
        byMediaType: { image: 4, video: 6 },
        stored: 4,
        storedByKind: { covers: 4, images: 2, videos: 1 },
        trashed: 2,
      }),
    ).toEqual({
      total: 10,
      byPlatform: { instagram: 5, twitter: 3, pinterest: 1, web: 1, manual: 0 },
      byMediaType: { image: 4, video: 6 },
      downloaded: 4,
      downloadedByType: { thumbnails: 4, images: 2, videos: 1 },
    });
  });

  it('maps a folder', () => {
    expect(
      toCollection({
        id: 1,
        name: 'Lighting',
        color: '#3d5afe',
        count: 4,
        createdAt: 1_790_000_000_000,
        externalId: '179',
        platform: 'instagram',
        position: null,
        sourceName: 'lighting',
      }),
    ).toEqual({
      id: 1,
      name: 'Lighting',
      color: '#3d5afe',
      platform: 'instagram',
      externalId: '179',
      igName: 'lighting',
      count: 4,
      createdAt: 1_790_000_000,
    });
  });
});
