import { describe, it, expect } from 'vitest';
import {
  SAVED_PATTERNS,
  parseIgFolder,
  parsePinBoard,
  deslugify,
  isAllowedUrl,
  safeUrl,
  type BrowserTab,
} from '../../src/lib/browserUrls';

describe('SAVED_PATTERNS', () => {
  it('matches IG saved listings and folders', () => {
    expect(SAVED_PATTERNS.instagram.test('https://www.instagram.com/user/saved/')).toBe(true);
    expect(SAVED_PATTERNS.instagram.test('https://www.instagram.com/user/saved/all-posts/')).toBe(
      true,
    );
    expect(SAVED_PATTERNS.instagram.test('https://www.instagram.com/user/')).toBe(false);
  });

  it('matches X bookmarks', () => {
    expect(SAVED_PATTERNS.twitter.test('https://x.com/i/bookmarks')).toBe(true);
    expect(SAVED_PATTERNS.twitter.test('https://x.com/home')).toBe(false);
  });

  it('matches Pinterest boards but not reserved roots / profile tabs / pin details', () => {
    expect(SAVED_PATTERNS.pinterest.test('https://www.pinterest.it/user/my-board/')).toBe(true);
    expect(SAVED_PATTERNS.pinterest.test('https://www.pinterest.com/pin/12345/')).toBe(false);
    expect(SAVED_PATTERNS.pinterest.test('https://www.pinterest.com/search/pins/')).toBe(false);
    expect(SAVED_PATTERNS.pinterest.test('https://www.pinterest.com/user/_saved/')).toBe(false);
  });
});

describe('parseIgFolder', () => {
  it('parses a saved folder URL (slug + numeric id)', () => {
    expect(
      parseIgFolder('https://www.instagram.com/user/saved/ricette/17900000000000000/'),
    ).toEqual({ slug: 'ricette', folderId: '17900000000000000' });
  });

  it('returns null for all-posts, the folder index and non-saved URLs', () => {
    expect(parseIgFolder('https://www.instagram.com/user/saved/all-posts/')).toBeNull();
    expect(parseIgFolder('https://www.instagram.com/user/saved/')).toBeNull();
    expect(parseIgFolder('https://www.instagram.com/user/')).toBeNull();
    expect(parseIgFolder(undefined)).toBeNull();
  });
});

describe('parsePinBoard', () => {
  it('parses a board URL into user/slug/boardId', () => {
    expect(parsePinBoard('https://www.pinterest.it/mario/idee-cucina/')).toEqual({
      user: 'mario',
      slug: 'idee-cucina',
      boardId: 'mario/idee-cucina',
    });
  });

  it('returns null for reserved roots, profile tabs and pin details', () => {
    expect(parsePinBoard('https://www.pinterest.com/pin/12345/')).toBeNull();
    expect(parsePinBoard('https://www.pinterest.com/ideas/food/')).toBeNull();
    expect(parsePinBoard('https://www.pinterest.com/user/_created/')).toBeNull();
    expect(parsePinBoard('')).toBeNull();
  });
});

describe('deslugify', () => {
  it('turns a slug into a title-cased name', () => {
    expect(deslugify('ricette-veloci')).toBe('Ricette Veloci');
    expect(deslugify('a__b')).toBe('A B');
  });

  it('falls back when the slug is empty', () => {
    expect(deslugify('', 'Cartella')).toBe('Cartella');
    expect(deslugify(null, 'Folder')).toBe('Folder');
  });
});

describe('isAllowedUrl / safeUrl (webview origin allowlist)', () => {
  it('accepts only https on the tab host (subdomains included)', () => {
    expect(isAllowedUrl('instagram', 'https://www.instagram.com/user/saved/')).toBe(true);
    expect(isAllowedUrl('instagram', 'http://www.instagram.com/')).toBe(false);
    expect(isAllowedUrl('twitter', 'https://x.com/i/bookmarks')).toBe(true);
    expect(isAllowedUrl('twitter', 'https://twitter.com/i/bookmarks')).toBe(true);
    expect(isAllowedUrl('pinterest', 'https://www.pinterest.co.uk/u/b/')).toBe(true);
  });

  it('rejects foreign hosts, lookalikes and garbage', () => {
    expect(isAllowedUrl('instagram', 'https://evil.com/instagram.com')).toBe(false);
    expect(isAllowedUrl('instagram', 'https://instagram.com.evil.io/')).toBe(false);
    expect(isAllowedUrl('pinterest', 'https://pinterest.com.evil.io/')).toBe(false);
    expect(isAllowedUrl('instagram', 'javascript:alert(1)')).toBe(false);
    expect(isAllowedUrl('instagram', 'not a url')).toBe(false);
    expect(isAllowedUrl('nope' as BrowserTab, 'https://example.com/')).toBe(false);
  });

  it('safeUrl falls back to the default for anything not allowed', () => {
    const fb = 'https://www.instagram.com/';
    expect(safeUrl('instagram', 'https://www.instagram.com/u/saved/', fb)).toBe(
      'https://www.instagram.com/u/saved/',
    );
    expect(safeUrl('instagram', 'https://evil.com/', fb)).toBe(fb);
    expect(safeUrl('instagram', null, fb)).toBe(fb);
  });
});
