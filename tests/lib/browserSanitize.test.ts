import { describe, it, expect } from 'vitest';
import {
  MAX_BATCH_ITEMS,
  MAX_ID_LEN,
  MAX_TEXT_LEN,
  MAX_URL_LEN,
  MAX_MEDIA,
  STR_FIELDS,
  sanitizeInterceptedItem,
  sanitizeInterceptedBatch,
} from '../../src/lib/browserSanitize';

// The sanitizer is the renderer-side trust boundary for payloads coming from the
// webview MAIN world (fully page-controlled). These tests pin its semantics:
// same limits, same rejects, same platform stamping as the Browser view relies on.

describe('sanitizeInterceptedItem', () => {
  it('rejects non-objects and missing/oversized ids', () => {
    expect(sanitizeInterceptedItem(null, 'instagram')).toBeNull();
    expect(sanitizeInterceptedItem('x', 'instagram')).toBeNull();
    expect(sanitizeInterceptedItem({}, 'instagram')).toBeNull();
    expect(sanitizeInterceptedItem({ id: '' }, 'instagram')).toBeNull();
    expect(sanitizeInterceptedItem({ id: 'a'.repeat(MAX_ID_LEN + 1) }, 'instagram')).toBeNull();
    // Exactly at the cap is still accepted.
    expect(sanitizeInterceptedItem({ id: 'a'.repeat(MAX_ID_LEN) }, 'instagram')).not.toBeNull();
  });

  it('coerces a numeric id to string', () => {
    const out = sanitizeInterceptedItem({ id: 123 }, 'twitter');
    expect(out?.id).toBe('123');
  });

  it('stamps the validated batch platform, ignoring the page-supplied one', () => {
    const out = sanitizeInterceptedItem({ id: '1', platform: 'manual' }, 'instagram');
    expect(out?.platform).toBe('instagram');
  });

  it('clamps the textual fields and coerces non-strings to empty', () => {
    const it_ = {
      id: '1',
      text: 'x'.repeat(MAX_TEXT_LEN + 5),
      timestamp: 't'.repeat(100),
      postUrl: 'u'.repeat(MAX_URL_LEN + 5),
      authorName: 42,
    };
    const out = sanitizeInterceptedItem(it_, 'instagram');
    expect(out?.text.length).toBe(MAX_TEXT_LEN);
    expect(out?.timestamp.length).toBe(64); // timestamp has its own 64-char cap
    expect(out?.postUrl.length).toBe(MAX_URL_LEN);
    expect(out?.authorName).toBe('');
    // Every declared string field is always present on the output.
    for (const f of STR_FIELDS) expect(typeof out?.[f]).toBe('string');
  });

  it('only keeps an http(s) thumbnailUrl', () => {
    expect(
      sanitizeInterceptedItem({ id: '1', thumbnailUrl: 'https://a.com/x.jpg' }, 'instagram')
        ?.thumbnailUrl,
    ).toBe('https://a.com/x.jpg');
    for (const bad of [
      'javascript:alert(1)',
      'file:///etc/passwd',
      'not a url',
      'https://a.com/' + 'x'.repeat(MAX_URL_LEN),
      42,
    ]) {
      expect(
        sanitizeInterceptedItem({ id: '1', thumbnailUrl: bad }, 'instagram')?.thumbnailUrl,
      ).toBe('');
    }
  });

  it('filters media to http(s) urls, caps the list and normalizes types', () => {
    const media = [
      { type: 'video', url: 'https://a.com/v.mp4' },
      { type: 'gif', url: 'http://a.com/g.gif' }, // unknown type → image
      { type: 'image', url: 'javascript:alert(1)' }, // dropped
      null, // dropped
      { type: 'image' }, // no url → dropped
    ];
    const out = sanitizeInterceptedItem({ id: '1', media }, 'instagram');
    expect(out?.media).toEqual([
      { type: 'video', url: 'https://a.com/v.mp4' },
      { type: 'image', url: 'http://a.com/g.gif' },
    ]);

    const many = Array.from({ length: MAX_MEDIA + 10 }, (_, i) => ({
      type: 'image',
      url: `https://a.com/${i}.jpg`,
    }));
    expect(sanitizeInterceptedItem({ id: '1', media: many }, 'instagram')?.media.length).toBe(
      MAX_MEDIA,
    );
    expect(sanitizeInterceptedItem({ id: '1', media: 'nope' }, 'instagram')?.media).toEqual([]);
  });
});

describe('sanitizeInterceptedBatch', () => {
  it('drops invalid items and keeps the valid ones', () => {
    const out = sanitizeInterceptedBatch([{ id: 'a' }, {}, null, { id: 'b' }], 'twitter');
    expect(out.map((i) => i.id)).toEqual(['a', 'b']);
    expect(out.every((i) => i.platform === 'twitter')).toBe(true);
  });

  it('caps the batch at MAX_BATCH_ITEMS accepted items', () => {
    const items = Array.from({ length: MAX_BATCH_ITEMS + 50 }, (_, i) => ({ id: String(i) }));
    expect(sanitizeInterceptedBatch(items, 'instagram').length).toBe(MAX_BATCH_ITEMS);
  });
});
