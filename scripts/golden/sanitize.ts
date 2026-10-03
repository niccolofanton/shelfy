// Golden set for the desktop's `sanitizeInterceptedBatch`
// (src/lib/browserSanitize.ts): the check of a capture batch, ported to
// `shelfy_core::ingest::sanitize` (crates/core/src/ingest/sanitize.rs).
//
// Each case is one synthetic batch and its platform; the output is what the
// desktop keeps. The Rust check runs the port's `clean_item` on every item
// and serializes the items it keeps, which must equal this output byte for
// byte. That covers the rules both share: ids, string clamping (UTF-16 code
// units, cut on a character boundary), the media filter, the platform stamp.
//
// The port is stricter, and the cases stay clear of where it differs on
// purpose (crates/core/src/ingest/sanitize/tests.rs covers those rules):
// - every URL is on the platform's allowlist and has no credentials, port or
//   whitespace; the desktop takes any http(s) URL;
// - an id is a string or an integer: the desktop also turns `true`, `1.5` or
//   `[1]` into text, and rounds integers above 2^53;
// - a batch of more than 500 items is refused; the desktop keeps 1,000;
// - `videoUrl` is kept for the post (the desktop drops it); it is not part of
//   the compared output.

import { sanitizeInterceptedBatch } from '../../src/lib/browserSanitize';
import type { GoldenCase, GoldenSet } from './lib';

const IG_IMAGE =
  'https://scontent-mxp1-1.cdninstagram.com/v/t51.2885-15/1_n.jpg?stp=dst-jpg_e35&_nc_ht=scontent-mxp1-1.cdninstagram.com&oe=6A1B2C3D&_nc_sid=1';
const IG_POSTER = 'https://instagram.fmxp1-1.fna.fbcdn.net/v/t51.2885-15/2_n.jpg?oe=6A1B2C3E';
const IG_VIDEO =
  'https://instagram.fmxp1-1.fna.fbcdn.net/o1/v/t16/f2/m86/AQx.mp4?efg=1&oe=6A1B2C3F';
const X_PHOTO = 'https://pbs.twimg.com/media/GAbCdEfXwAAxyz.jpg';
const X_PHOTO_2 = 'https://pbs.twimg.com/media/GAbCdEfXwAAabc.png';
const X_POSTER = 'https://pbs.twimg.com/ext_tw_video_thumb/1700000000000000003/pu/img/a.jpg';
const X_VIDEO =
  'https://video.twimg.com/ext_tw_video/1700000000000000003/pu/vid/720x1280/a.mp4?tag=12';
const X_AVATAR = 'https://pbs.twimg.com/profile_images/1/someone_normal.jpg';
const PIN_IMAGE = 'https://i.pinimg.com/originals/aa/bb/cc/pin.jpg';
const PIN_IMAGE_2 = 'https://i.pinimg.com/736x/aa/bb/cc/pin-2.jpg';
const PIN_VIDEO = 'https://v1.pinimg.com/videos/mc/720p/aa/bb/cc/pin.mp4';
const PIN_HLS = 'https://v1.pinimg.com/videos/mc/hls/aa/bb/cc/pin.m3u8';

type Platform = 'instagram' | 'twitter' | 'pinterest';

/** [id, platform, items] */
const BATCHES: [string, Platform, unknown[]][] = [
  [
    'instagram-saved-page',
    'instagram',
    [
      {
        // A REST item: `<pk>_<owner>` id, a carousel with a video, extra fields.
        id: '3191575067010950169_25025320',
        platform: 'web',
        shortcode: 'CxKwJ0fLmQZ',
        postUrl: 'https://www.instagram.com/p/CxKwJ0fLmQZ/',
        profileUrl: 'https://www.instagram.com/someone/',
        authorUsername: 'someone',
        authorName: '',
        text: 'Lampada in vetro soffiato 💡\n\n#design #glass',
        thumbnailUrl: IG_IMAGE,
        mediaType: 'carousel',
        media: [
          { type: 'image', url: IG_IMAGE },
          { type: 'video', url: IG_POSTER, videoUrl: IG_VIDEO },
          { type: 'image', url: IG_POSTER, width: 1080, height: 1350 },
        ],
        timestamp: '2023-09-14T09:56:58.000Z',
        likes: 12,
        user: { username: 'someone', pk: 25025320 },
      },
      {
        // A GraphQL node: the pk as id, a media entry without a type.
        id: '3191575067010950170',
        platform: 'instagram',
        shortcode: 'CxKwJ0fLmQa',
        postUrl: 'https://www.instagram.com/p/CxKwJ0fLmQa/',
        profileUrl: '',
        authorUsername: '',
        authorName: '',
        text: '',
        thumbnailUrl: IG_IMAGE,
        mediaType: 'image',
        media: [{ url: IG_IMAGE }],
        timestamp: '',
      },
      {
        // The DOM fallback: the shortcode as id.
        id: 'CxKwJ0fLmQb',
        shortcode: 'CxKwJ0fLmQb',
        postUrl: 'https://www.instagram.com/p/CxKwJ0fLmQb/',
        text: 'Reel',
        thumbnailUrl: IG_POSTER,
        mediaType: 'video',
        media: [{ type: 'video', url: IG_POSTER }],
      },
    ],
  ],
  [
    'x-bookmarks-page',
    'twitter',
    [
      {
        id: '1700000000000000001',
        platform: 'twitter',
        shortcode: '',
        postUrl: 'https://x.com/someone/status/1700000000000000001',
        profileUrl: 'https://x.com/someone',
        authorUsername: 'someone',
        authorName: 'Some One',
        text: 'Two photos https://t.co/abc',
        thumbnailUrl: X_PHOTO,
        mediaType: 'images',
        media: [
          { type: 'image', url: X_PHOTO },
          { type: 'image', url: X_PHOTO_2 },
        ],
        timestamp: '2024-02-29T12:00:00.000Z',
      },
      {
        id: '1700000000000000003',
        platform: 'instagram',
        postUrl: 'https://x.com//status/1700000000000000003',
        authorUsername: '',
        text: 'A video',
        thumbnailUrl: X_POSTER,
        mediaType: 'video',
        media: [{ type: 'video', url: X_POSTER, videoUrl: X_VIDEO }],
        timestamp: '2024-03-01T08:30:00.000Z',
      },
      {
        // A text tweet: the author's avatar is the cover.
        id: '1700000000000000004',
        postUrl: 'https://twitter.com/someone/status/1700000000000000004',
        authorUsername: 'someone',
        text: 'Only words, no media',
        thumbnailUrl: X_AVATAR,
        mediaType: 'text',
        media: [],
      },
    ],
  ],
  [
    'pinterest-board-page',
    'pinterest',
    [
      {
        id: '987654321012345678',
        platform: 'pinterest',
        shortcode: '',
        postUrl: 'https://www.pinterest.com/pin/987654321012345678/',
        profileUrl: 'https://www.pinterest.com/someone/',
        authorUsername: 'someone',
        authorName: 'Some One',
        text: 'Kitchen — Small kitchen ideas\nhttps://example.com/kitchen',
        thumbnailUrl: PIN_IMAGE,
        mediaType: 'image',
        media: [{ type: 'image', url: PIN_IMAGE }],
        timestamp: '2025-08-01T19:57:38.000Z',
      },
      {
        // A video pin: Pinterest keeps the MP4 itself as the media url.
        id: '987654321012345679',
        postUrl: 'https://it.pinterest.com/pin/987654321012345679/',
        thumbnailUrl: PIN_IMAGE_2,
        mediaType: 'video',
        media: [{ type: 'video', url: PIN_VIDEO }],
      },
      {
        // An idea pin: an image page and a video page as an HLS playlist.
        id: '987654321012345680',
        postUrl: 'https://www.pinterest.co.uk/pin/987654321012345680/',
        thumbnailUrl: PIN_IMAGE,
        mediaType: 'carousel',
        media: [
          { type: 'image', url: PIN_IMAGE },
          { type: 'video', url: PIN_HLS },
          { type: 'image', url: PIN_IMAGE_2 },
        ],
      },
    ],
  ],
  [
    'ids',
    'pinterest',
    [
      { id: 12345 },
      { id: 0 },
      { id: -7 },
      { id: 9007199254740991 },
      { id: '' },
      { id: null },
      { shortcode: 'no id' },
      { id: ' padded ' },
      { id: 'a'.repeat(256) },
      { id: 'a'.repeat(257) },
      { id: 'é'.repeat(256) },
      { id: '😀'.repeat(128) },
      { id: '😀'.repeat(129) },
      { id: 'x' + '😀'.repeat(128) },
    ],
  ],
  ['not-objects', 'twitter', [null, 5, 'item', true, false, [], [1, 2], {}, { id: 'ok' }]],
  [
    'clamping',
    'twitter',
    [
      {
        id: '1',
        shortcode: 's'.repeat(4097),
        postUrl: `https://x.com/${'p'.repeat(5000)}`,
        profileUrl: 'https://x.com/someone',
        authorUsername: 'u'.repeat(4096),
        authorName: 'n'.repeat(5000),
        mediaType: 'm'.repeat(4100),
        timestamp: `2024-01-01T00:00:00.000Z${'x'.repeat(100)}`,
        text: 'a'.repeat(20001),
      },
      {
        // A surrogate pair across the limit is dropped whole.
        id: '2',
        authorUsername: `${'a'.repeat(4095)}😀`,
        authorName: `${'b'.repeat(4094)}😀c`,
        mediaType: 'é'.repeat(4097),
        timestamp: `${'t'.repeat(63)}😀`,
        text: 'controls \u0000\u0007\u001f\u007f and lines    survive',
      },
      {
        // 2,049 astral characters are 4,098 code units: 2,048 fit exactly.
        id: '3',
        authorName: '😀'.repeat(2049),
        authorUsername: 'é café الحروف 東京 👩‍💻',
      },
    ],
  ],
  [
    'not-strings',
    'instagram',
    [
      {
        id: '3191575067010950169',
        shortcode: 123,
        postUrl: { href: 'https://www.instagram.com/p/x/' },
        profileUrl: ['https://www.instagram.com/someone/'],
        authorUsername: true,
        authorName: null,
        mediaType: 8,
        timestamp: 1694685418,
        text: 42,
        thumbnailUrl: 42,
        media: 'https://scontent.cdninstagram.com/a.jpg',
      },
      { id: '3191575067010950170', media: { url: IG_IMAGE } },
      { id: '3191575067010950171', media: null, thumbnailUrl: [IG_IMAGE] },
    ],
  ],
  [
    'media-filter',
    'twitter',
    [
      {
        id: '1700000000000000005',
        media: [
          null,
          7,
          X_PHOTO,
          [X_PHOTO],
          {},
          { url: 5 },
          { url: null },
          { type: 'image' },
          { url: 'ftp://pbs.twimg.com/media/a.jpg' },
          { url: 'javascript:alert(1)' },
          { url: 'not a url' },
          { url: 'https://' },
          { url: '/media/relative.jpg' },
          { url: `https://pbs.twimg.com/media/${'a'.repeat(4100)}.jpg` },
          { type: 'VIDEO', url: X_PHOTO },
          { type: 'video', url: X_POSTER, videoUrl: X_VIDEO },
          { url: X_PHOTO_2 },
          { type: 'gif', url: X_PHOTO },
          { type: 'image', url: 'http://pbs.twimg.com/media/plain-http.jpg' },
          { type: 'video', url: X_POSTER, extra: { nested: [1, 2, 3] } },
        ],
      },
    ],
  ],
  [
    'media-cap',
    'pinterest',
    [
      {
        id: '987654321012345681',
        media: [
          { url: 'ftp://i.pinimg.com/a.jpg' },
          null,
          ...Array.from({ length: 30 }, (_, n) => ({ url: `https://i.pinimg.com/736x/${n}.jpg` })),
          { url: 'javascript:void(0)' },
          ...Array.from({ length: 40 }, (_, n) => ({
            type: n % 2 ? 'video' : 'image',
            url: `https://i.pinimg.com/736x/${30 + n}.jpg`,
          })),
        ],
      },
    ],
  ],
  [
    'thumbnails',
    'twitter',
    [
      { id: '1', thumbnailUrl: X_PHOTO },
      { id: '2', thumbnailUrl: 'http://pbs.twimg.com/media/plain-http.jpg' },
      { id: '3', thumbnailUrl: 'ftp://pbs.twimg.com/media/a.jpg' },
      { id: '4', thumbnailUrl: 'javascript:alert(1)' },
      { id: '5', thumbnailUrl: '' },
      { id: '6', thumbnailUrl: '/media/relative.jpg' },
      { id: '7', thumbnailUrl: `https://pbs.twimg.com/media/${'a'.repeat(4100)}.jpg` },
      { id: '8', thumbnailUrl: `https://pbs.twimg.com/media/${'a'.repeat(4096 - 28)}` },
    ],
  ],
  [
    'url-forms',
    'twitter',
    [
      {
        id: '1700000000000000006',
        thumbnailUrl: 'HTTPS://PBS.TWIMG.COM/media/Upper.jpg',
        media: [
          { url: 'https://pbs.twimg.com:443/media/default-port.jpg' },
          { url: 'https://pbs.twimg.com/media/a.jpg?format=jpg&name=large#fragment' },
          { url: 'https://pbs.twimg.com/media/a%20b.jpg' },
          { url: 'https://pbs.twimg.com/media/caffè.jpg' },
          { url: 'https://pbs.twimg.com/media/../media/dots.jpg' },
          { url: 'https:pbs.twimg.com/media/no-slashes.jpg' },
        ],
      },
    ],
  ],
  [
    'platform-stamp',
    'pinterest',
    [
      { id: '1', platform: 'web' },
      { id: '2', platform: 'manual' },
      { id: '3', platform: 'instagram' },
      { id: '4', platform: 42 },
      { id: '5' },
    ],
  ],
  ['empty', 'instagram', []],
];

const sanitizeSet: GoldenSet = {
  name: 'sanitize',
  source: 'src/lib/browserSanitize.ts#sanitizeInterceptedBatch',
  generator: 'scripts/golden/sanitize.ts',
  build(): GoldenCase[] {
    return BATCHES.map(([id, platform, items]) => ({
      id,
      args: [items, platform],
      output: sanitizeInterceptedBatch(items, platform),
    }));
  },
};

export default sanitizeSet;
