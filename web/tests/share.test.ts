// Where the shared link hides in a Web Share Target request (P2-07 acceptance
// 3, 5): web/src/share/extractUrl.ts.
import { describe, expect, it } from 'vitest';
import { extractSharedUrl } from '../src/share/extractUrl';

describe('extractSharedUrl', () => {
  it('takes a bare URL from the url field', () => {
    expect(extractSharedUrl({ url: 'https://www.instagram.com/p/C0ffee/' })).toBe(
      'https://www.instagram.com/p/C0ffee/',
    );
  });

  it('finds the URL inside text, as Android commonly shares it', () => {
    expect(
      extractSharedUrl({
        url: null,
        text: 'Check this out: https://x.com/studio/status/2',
        title: null,
      }),
    ).toBe('https://x.com/studio/status/2');
  });

  it('falls back to title when neither url nor text has one', () => {
    expect(extractSharedUrl({ title: 'From https://www.pinterest.com/pin/3/' })).toBe(
      'https://www.pinterest.com/pin/3/',
    );
  });

  it('prefers url over text, and text over title', () => {
    expect(
      extractSharedUrl({
        url: 'https://a.example/x',
        text: 'https://b.example/y',
        title: 'https://c.example/z',
      }),
    ).toBe('https://a.example/x');
    expect(extractSharedUrl({ text: 'https://b.example/y', title: 'https://c.example/z' })).toBe(
      'https://b.example/y',
    );
  });

  it('strips trailing sentence punctuation and wrapping brackets', () => {
    expect(extractSharedUrl({ text: 'Look at this: https://x.com/a/b.' })).toBe(
      'https://x.com/a/b',
    );
    expect(extractSharedUrl({ text: 'See (https://x.com/a/b) for details' })).toBe(
      'https://x.com/a/b',
    );
  });

  it('finds nothing in plain text with no link', () => {
    expect(
      extractSharedUrl({ url: null, text: 'just a caption, no link here', title: null }),
    ).toBeNull();
  });

  it('ignores a non-http(s) scheme', () => {
    expect(extractSharedUrl({ url: 'mailto:a@example.test' })).toBeNull();
    expect(extractSharedUrl({ text: 'ftp://files.example/a' })).toBeNull();
  });

  it('is null on empty or missing fields', () => {
    expect(extractSharedUrl({})).toBeNull();
    expect(extractSharedUrl({ url: '', text: '', title: '' })).toBeNull();
    expect(extractSharedUrl({ url: null, text: null, title: null })).toBeNull();
  });
});
