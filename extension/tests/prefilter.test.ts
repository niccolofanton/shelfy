// The worker's pre-filter is the desktop's sanitizeInterceptedBatch plus the direct video URL:
// on any batch, the output without `videoUrl` (and with the batch platform) equals the desktop's.

import { describe, expect, it } from 'vitest';
import {
  MAX_BATCH_ITEMS,
  MAX_MEDIA,
  sanitizeInterceptedBatch,
} from '../../src/lib/browserSanitize';
import { itemBytes, prefilterBatch, utf8Length, type WireItem } from '../src/sw/prefilter';
import { igItem } from './helpers';

const withoutVideo = (items: WireItem[], platform: string) =>
  items.map((item) => ({
    ...item,
    platform,
    media: item.media.map(({ type, url }) => ({ type, url })),
  }));

const hostile: unknown[] = [
  igItem(1),
  null,
  'string',
  42,
  { id: '' },
  { id: 'x'.repeat(257) },
  { id: 12345, platform: 'web', text: 'z'.repeat(25_000) },
  {
    id: 'media-mix',
    thumbnailUrl: 'javascript:alert(1)',
    media: [
      {
        type: 'video',
        url: 'https://v1.pinimg.com/videos/a.jpg',
        videoUrl: 'https://v1.pinimg.com/videos/a.mp4',
      },
      { type: 'image', url: 'ftp://nope/x.jpg', videoUrl: 'https://v1.pinimg.com/videos/b.mp4' },
      'junk',
      { type: 'video', url: 'https://v1.pinimg.com/videos/c.jpg', videoUrl: 'javascript:alert(1)' },
      {
        type: 'image',
        url: 'https://i.pinimg.com/d.jpg',
        videoUrl: 'https://v1.pinimg.com/videos/d.mp4',
      },
      { type: 'video', url: 'https://v1.pinimg.com/videos/e.jpg', videoUrl: 42 },
    ],
  },
  {
    id: 'many-media',
    media: Array.from({ length: MAX_MEDIA + 10 }, (_, i) => ({
      type: 'image',
      url: `https://i.pinimg.com/${i}.jpg`,
    })),
  },
  { id: 'emoji', text: `${'a'.repeat(19_999)}😀` },
];

describe('prefilterBatch', () => {
  it("equals the desktop's sanitizeInterceptedBatch, apart from videoUrl", () => {
    const { items, rejected } = prefilterBatch(hostile, 'pinterest');
    expect(withoutVideo(items, 'pinterest')).toEqual(
      sanitizeInterceptedBatch(hostile, 'pinterest'),
    );
    expect(rejected).toBe(hostile.length - items.length);
    expect(items.map((item) => item.id)).toEqual([
      '3400000000000000001_9000000001',
      '12345',
      'media-mix',
      'many-media',
      'emoji',
    ]);
  });

  it('keeps a direct video URL on video slides only, when it is http(s)', () => {
    const mix = prefilterBatch(hostile, 'pinterest').items.find((item) => item.id === 'media-mix');
    expect(mix?.thumbnailUrl).toBe('');
    expect(mix?.media).toEqual([
      {
        type: 'video',
        url: 'https://v1.pinimg.com/videos/a.jpg',
        videoUrl: 'https://v1.pinimg.com/videos/a.mp4',
      },
      { type: 'video', url: 'https://v1.pinimg.com/videos/c.jpg' },
      { type: 'image', url: 'https://i.pinimg.com/d.jpg' },
      { type: 'video', url: 'https://v1.pinimg.com/videos/e.jpg' },
    ]);
  });

  it('stamps nothing per item: the batch carries the platform', () => {
    const [item] = prefilterBatch([igItem(1, { platform: 'web' })], 'instagram').items;
    expect(item).not.toHaveProperty('platform');
    expect(Object.keys(item).sort()).toEqual(
      [
        'authorName',
        'authorUsername',
        'id',
        'media',
        'mediaType',
        'postUrl',
        'profileUrl',
        'shortcode',
        'text',
        'thumbnailUrl',
        'timestamp',
      ].sort(),
    );
  });

  it('caps a batch like the desktop', () => {
    const many = Array.from({ length: MAX_BATCH_ITEMS + 5 }, (_, i) => ({ id: String(i) }));
    expect(prefilterBatch(many, 'twitter').items).toHaveLength(MAX_BATCH_ITEMS);
  });
});

describe('sizes', () => {
  it('counts UTF-8 bytes', () => {
    expect(utf8Length('abc')).toBe(3);
    expect(utf8Length('è')).toBe(2);
    expect(utf8Length('€')).toBe(3);
    expect(utf8Length('😀')).toBe(4);
    expect(utf8Length('\ud800x')).toBe(4);
    for (const text of ['hello', 'città', '😀 emoji', 'mixed € 😀 è'])
      expect(utf8Length(text)).toBe(new TextEncoder().encode(text).length);
  });

  it("an item's size is its JSON plus a comma", () => {
    const [item] = prefilterBatch([igItem(1)], 'instagram').items;
    expect(itemBytes(item)).toBe(new TextEncoder().encode(JSON.stringify(item)).length + 1);
  });
});
