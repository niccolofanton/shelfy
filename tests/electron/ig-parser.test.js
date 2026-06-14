import igParser from '../../electron/ig-parser.js';

const { parseResponseBody, normalizeExportedPost, igDateFromShortcode } = igParser;

// ---------------------------------------------------------------------------
// parseResponseBody — REST format (data.items / data.feed_items)
// ---------------------------------------------------------------------------

describe('parseResponseBody – REST format', () => {
  it('parses a single image item', () => {
    const data = {
      items: [
        {
          id: '111',
          pk: '111',
          code: 'abc123',
          media_type: 1,
          taken_at: 1700000000,
          user: { username: 'alice' },
          caption: { text: 'Hello world' },
          image_versions2: { candidates: [{ url: 'https://cdn.example.com/img.jpg' }] },
        },
      ],
      more_available: false,
    };

    const { items, hasNextPage } = parseResponseBody(data);

    expect(items).toHaveLength(1);
    const item = items[0];
    expect(item.id).toBe('111');
    expect(item.platform).toBe('instagram');
    expect(item.shortcode).toBe('abc123');
    expect(item.postUrl).toBe('https://www.instagram.com/p/abc123/');
    expect(item.profileUrl).toBe('https://www.instagram.com/alice/');
    expect(item.authorUsername).toBe('alice');
    expect(item.text).toBe('Hello world');
    expect(item.thumbnailUrl).toBe('https://cdn.example.com/img.jpg');
    expect(item.mediaType).toBe('image');
    expect(item.timestamp).toBe(new Date(1700000000 * 1000).toISOString());
    expect(hasNextPage).toBe(false);
  });

  it('parses a video item (media_type 2)', () => {
    const data = {
      items: [
        {
          id: '222',
          code: 'vid456',
          media_type: 2,
          taken_at: 1700000100,
          user: { username: 'bob' },
          caption: { text: 'Watch this' },
          image_versions2: { candidates: [{ url: 'https://cdn.example.com/thumb.jpg' }] },
        },
      ],
    };

    const { items } = parseResponseBody(data);

    expect(items).toHaveLength(1);
    expect(items[0].mediaType).toBe('video');
  });

  it('parses a carousel item (media_type 8) and falls back to carousel_media thumbnail', () => {
    const data = {
      items: [
        {
          id: '333',
          code: 'car789',
          media_type: 8,
          taken_at: 1700000200,
          user: { username: 'carol' },
          caption: { text: 'Carousel post' },
          carousel_media: [
            { image_versions2: { candidates: [{ url: 'https://cdn.example.com/carousel.jpg' }] } },
          ],
        },
      ],
    };

    const { items } = parseResponseBody(data);

    expect(items).toHaveLength(1);
    expect(items[0].mediaType).toBe('carousel');
    expect(items[0].thumbnailUrl).toBe('https://cdn.example.com/carousel.jpg');
  });

  it('reflects more_available: true as hasNextPage', () => {
    const data = {
      items: [
        {
          id: '444',
          code: 'next1',
          media_type: 1,
          user: { username: 'dave' },
          caption: { text: '' },
        },
      ],
      more_available: true,
    };

    const { hasNextPage } = parseResponseBody(data);

    expect(hasNextPage).toBe(true);
  });

  it('handles a null caption gracefully', () => {
    const data = {
      items: [
        {
          id: '555',
          code: 'nocap',
          media_type: 1,
          user: { username: 'eve' },
          caption: null,
        },
      ],
    };

    const { items } = parseResponseBody(data);

    expect(items).toHaveLength(1);
    expect(items[0].text).toBe('');
  });

  it('filters out items that have no shortcode', () => {
    const data = {
      items: [
        { id: '666', media_type: 1, user: { username: 'frank' } }, // no code / shortcode
        { id: '777', code: 'good1', media_type: 1, user: { username: 'frank' } },
      ],
    };

    const { items } = parseResponseBody(data);

    expect(items).toHaveLength(1);
    expect(items[0].shortcode).toBe('good1');
  });

  it('uses feed_items as an alias for items', () => {
    const data = {
      feed_items: [
        {
          id: '888',
          code: 'feed1',
          media_type: 1,
          user: { username: 'grace' },
          caption: { text: 'feed' },
        },
      ],
    };

    const { items } = parseResponseBody(data);

    expect(items).toHaveLength(1);
    expect(items[0].shortcode).toBe('feed1');
  });
});

// ---------------------------------------------------------------------------
// parseResponseBody — GraphQL edge format
// ---------------------------------------------------------------------------

describe('parseResponseBody – GraphQL edge format', () => {
  const makeEdge = (node) => ({ node });

  it('parses a nested image node (GraphImage)', () => {
    const data = {
      data: {
        user: {
          edge_saved_media: {
            edges: [
              makeEdge({
                id: 'gql1',
                shortcode: 'gql_sc1',
                __typename: 'GraphImage',
                owner: { username: 'henry' },
                taken_at_timestamp: 1700001000,
                thumbnail_src: 'https://cdn.example.com/gql.jpg',
                edge_media_to_caption: { edges: [{ node: { text: 'GQL caption' } }] },
              }),
            ],
            page_info: { has_next_page: true, end_cursor: 'cursor_abc' },
          },
        },
      },
    };

    const { items, hasNextPage } = parseResponseBody(data);

    expect(items).toHaveLength(1);
    const item = items[0];
    expect(item.id).toBe('gql1');
    expect(item.shortcode).toBe('gql_sc1');
    expect(item.mediaType).toBe('image');
    expect(item.authorUsername).toBe('henry');
    expect(item.text).toBe('GQL caption');
    expect(item.thumbnailUrl).toBe('https://cdn.example.com/gql.jpg');
    expect(hasNextPage).toBe(true);
  });

  it('parses a video node (GraphVideo)', () => {
    const data = {
      data: {
        user: {
          edge_saved_media: {
            edges: [
              makeEdge({
                id: 'gql2',
                shortcode: 'gql_sc2',
                __typename: 'GraphVideo',
                owner: { username: 'ivan' },
                taken_at_timestamp: 1700002000,
                thumbnail_src: 'https://cdn.example.com/vid.jpg',
                edge_media_to_caption: { edges: [] },
              }),
            ],
            page_info: { has_next_page: false },
          },
        },
      },
    };

    const { items, hasNextPage } = parseResponseBody(data);

    expect(items[0].mediaType).toBe('video');
    expect(hasNextPage).toBe(false);
  });

  it('parses a sidecar node (GraphSidecar)', () => {
    const data = {
      data: {
        user: {
          edge_saved_media: {
            edges: [
              makeEdge({
                id: 'gql3',
                shortcode: 'gql_sc3',
                __typename: 'GraphSidecar',
                edge_sidecar_to_children: { edges: [] },
                owner: { username: 'julia' },
                taken_at_timestamp: 1700003000,
                display_url: 'https://cdn.example.com/sidecar.jpg',
                edge_media_to_caption: { edges: [] },
              }),
            ],
          },
        },
      },
    };

    const { items } = parseResponseBody(data);

    expect(items[0].mediaType).toBe('carousel');
  });

  it('leaves authorUsername empty when owner.username is absent', () => {
    const data = {
      data: {
        user: {
          edge_saved_media: {
            edges: [
              makeEdge({
                id: 'gql4',
                shortcode: 'gql_sc4',
                __typename: 'GraphImage',
                owner: {},
                taken_at_timestamp: 1700004000,
                thumbnail_src: '',
                edge_media_to_caption: { edges: [] },
              }),
            ],
          },
        },
      },
    };

    const { items } = parseResponseBody(data);

    expect(items[0].authorUsername).toBe('');
    expect(items[0].profileUrl).toBe('');
  });
});

// ---------------------------------------------------------------------------
// parseResponseBody — edge cases
// ---------------------------------------------------------------------------

describe('parseResponseBody – empty / null data', () => {
  it('returns empty items and null hasNextPage for an empty object', () => {
    const { items, hasNextPage } = parseResponseBody({});
    expect(items).toEqual([]);
    expect(hasNextPage).toBeNull();
  });

  it('returns empty items for a data object with no known keys', () => {
    const { items, hasNextPage } = parseResponseBody({ unrelated: true });
    expect(items).toEqual([]);
    expect(hasNextPage).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// normalizeExportedPost
// ---------------------------------------------------------------------------

describe('normalizeExportedPost', () => {
  it('maps caption to text', () => {
    const post = { id: 'p1', shortcode: 'sc1', caption: 'A caption', authorUsername: 'zara' };
    const result = normalizeExportedPost(post);
    expect(result.text).toBe('A caption');
  });

  it('falls back to text when caption is absent', () => {
    const post = { id: 'p2', shortcode: 'sc2', text: 'Fallback text', authorUsername: 'zara' };
    const result = normalizeExportedPost(post);
    expect(result.text).toBe('Fallback text');
  });

  it('infers profileUrl from authorUsername when not provided', () => {
    const post = { id: 'p3', shortcode: 'sc3', authorUsername: 'mia' };
    const result = normalizeExportedPost(post);
    expect(result.profileUrl).toBe('https://www.instagram.com/mia/');
  });

  it('preserves an explicit profileUrl', () => {
    const post = {
      id: 'p4',
      shortcode: 'sc4',
      authorUsername: 'mia',
      profileUrl: 'https://www.instagram.com/mia/',
    };
    const result = normalizeExportedPost(post);
    expect(result.profileUrl).toBe('https://www.instagram.com/mia/');
  });

  it('infers postUrl from shortcode when not provided', () => {
    const post = { id: 'p5', shortcode: 'sc5', authorUsername: 'nina' };
    const result = normalizeExportedPost(post);
    expect(result.postUrl).toBe('https://www.instagram.com/p/sc5/');
  });

  it('falls back to shortcode when id is missing', () => {
    const post = { shortcode: 'sc6', authorUsername: 'otto' };
    const result = normalizeExportedPost(post);
    expect(result.id).toBe('sc6');
  });

  it('defaults mediaType to "image"', () => {
    const post = { id: 'p7', shortcode: 'sc7' };
    const result = normalizeExportedPost(post);
    expect(result.mediaType).toBe('image');
  });

  it('sets platform to instagram', () => {
    const post = { id: 'p8', shortcode: 'sc8' };
    const result = normalizeExportedPost(post);
    expect(result.platform).toBe('instagram');
  });
});

// ---------------------------------------------------------------------------
// parseResponseBody — media array (carousels)
// ---------------------------------------------------------------------------

describe('parseResponseBody – media array (REST carousel)', () => {
  it('returns one media entry per carousel_media slide', () => {
    const data = {
      items: [
        {
          id: '900',
          code: 'carA',
          media_type: 8,
          user: { username: 'kim' },
          carousel_media: [
            {
              media_type: 1,
              image_versions2: { candidates: [{ url: 'https://cdn.example.com/s0.jpg' }] },
            },
            {
              media_type: 2,
              image_versions2: { candidates: [{ url: 'https://cdn.example.com/s1.jpg' }] },
            },
            {
              media_type: 1,
              image_versions2: { candidates: [{ url: 'https://cdn.example.com/s2.jpg' }] },
            },
          ],
        },
      ],
    };
    const { items } = parseResponseBody(data);
    expect(items[0].mediaType).toBe('carousel');
    expect(items[0].media).toEqual([
      { type: 'image', url: 'https://cdn.example.com/s0.jpg' },
      { type: 'video', url: 'https://cdn.example.com/s1.jpg' },
      { type: 'image', url: 'https://cdn.example.com/s2.jpg' },
    ]);
    expect(items[0].thumbnailUrl).toBe('https://cdn.example.com/s0.jpg');
  });

  it('returns a single media entry for a plain image item', () => {
    const data = {
      items: [
        {
          id: '901',
          code: 'imgA',
          media_type: 1,
          user: { username: 'leo' },
          image_versions2: { candidates: [{ url: 'https://cdn.example.com/one.jpg' }] },
        },
      ],
    };
    const { items } = parseResponseBody(data);
    expect(items[0].media).toEqual([{ type: 'image', url: 'https://cdn.example.com/one.jpg' }]);
  });
});

describe('parseResponseBody – media array (GraphQL sidecar)', () => {
  it('returns one media entry per sidecar child', () => {
    const data = {
      data: {
        user: {
          edge_saved_media: {
            edges: [
              {
                node: {
                  id: 'gqlSC',
                  shortcode: 'gqlSCcode',
                  __typename: 'GraphSidecar',
                  owner: { username: 'mara' },
                  display_url: 'https://cdn.example.com/cover.jpg',
                  edge_media_to_caption: { edges: [] },
                  edge_sidecar_to_children: {
                    edges: [
                      {
                        node: {
                          __typename: 'GraphImage',
                          display_url: 'https://cdn.example.com/c0.jpg',
                        },
                      },
                      {
                        node: {
                          __typename: 'GraphVideo',
                          is_video: true,
                          display_url: 'https://cdn.example.com/c1.jpg',
                        },
                      },
                    ],
                  },
                },
              },
            ],
          },
        },
      },
    };
    const { items } = parseResponseBody(data);
    expect(items[0].mediaType).toBe('carousel');
    expect(items[0].media).toEqual([
      { type: 'image', url: 'https://cdn.example.com/c0.jpg' },
      { type: 'video', url: 'https://cdn.example.com/c1.jpg' },
    ]);
  });
});

// ---------------------------------------------------------------------------
// igDateFromShortcode — shortcode-derived dates (fallback for payloads with no
// taken_at, so those posts aren't left undated and sorted as the oldest)
// ---------------------------------------------------------------------------

// Encode a unix-ms timestamp into an IG shortcode the same way IG builds media
// ids (top 41 bits = ms since the IG epoch 2011-08-24, base64'd with the IG
// alphabet). Lets the tests assert an exact round-trip.
const IG_SC_ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';
function shortcodeForMs(ms) {
  let id = (BigInt(ms) - 1314220021721n) << 23n;
  let out = '';
  while (id > 0n) {
    out = IG_SC_ALPHABET[Number(id % 64n)] + out;
    id /= 64n;
  }
  return out;
}

describe('igDateFromShortcode', () => {
  const knownMs = Date.UTC(2023, 10, 14, 22, 13, 20); // 2023-11-14T22:13:20Z
  const knownShortcode = shortcodeForMs(knownMs);

  it('round-trips a known timestamp through the shortcode encoding', () => {
    expect(igDateFromShortcode(knownShortcode)).toBe(new Date(knownMs).toISOString());
  });

  it('returns "" for garbage input', () => {
    expect(igDateFromShortcode('')).toBe('');
    expect(igDateFromShortcode(null)).toBe('');
    expect(igDateFromShortcode('not!valid')).toBe(''); // '!' not in the alphabet
    expect(igDateFromShortcode('____________')).toBe(''); // decodes past year 2100
  });

  it('REST items without taken_at fall back to the shortcode date', () => {
    const data = {
      items: [
        {
          id: '999',
          code: knownShortcode,
          media_type: 1,
          user: { username: 'alice' },
          image_versions2: { candidates: [{ url: 'https://cdn.example.com/img.jpg' }] },
        },
      ],
    };
    const { items } = parseResponseBody(data);
    expect(items[0].timestamp).toBe(new Date(knownMs).toISOString());
  });

  it('REST items with taken_at still prefer it over the shortcode date', () => {
    const data = {
      items: [
        {
          id: '999',
          code: knownShortcode,
          media_type: 1,
          taken_at: 1700000000,
          user: { username: 'alice' },
          image_versions2: { candidates: [{ url: 'https://cdn.example.com/img.jpg' }] },
        },
      ],
    };
    const { items } = parseResponseBody(data);
    expect(items[0].timestamp).toBe(new Date(1700000000 * 1000).toISOString());
  });

  it('normalizeExportedPost derives the date from the shortcode when absent', () => {
    const post = normalizeExportedPost({ shortcode: knownShortcode, caption: 'x' });
    expect(post.timestamp).toBe(new Date(knownMs).toISOString());
  });

  it('normalizeExportedPost keeps an explicit timestamp', () => {
    const post = normalizeExportedPost({
      shortcode: knownShortcode,
      timestamp: '2020-01-01T00:00:00.000Z',
    });
    expect(post.timestamp).toBe('2020-01-01T00:00:00.000Z');
  });
});
