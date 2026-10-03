// @vitest-environment jsdom
//
// Passive-capture scoping (plan §2.16): IG saved and folders, X bookmarks, the signed-in user's
// Pinterest boards; everything else discarded. And the bridge's reading of the signed-in
// Pinterest user from the page.

import { afterEach, describe, expect, it } from 'vitest';
import {
  createViewerReader,
  readPinterestViewer,
  viewerFromPageData,
} from '../src/content/scoping';
import { passiveScope, toWireListing, classifyListing } from '../src/shared/listing';

afterEach(() => {
  document.head.innerHTML = '';
  document.body.innerHTML = '';
});

describe('passiveScope', () => {
  it('Instagram: all saved posts and folders; not the folder index, a post, the feed', () => {
    expect(
      passiveScope('instagram', 'https://www.instagram.com/someone/saved/all-posts/', null),
    ).toMatchObject({
      ok: true,
      listing: { key: 'instagram:ig_saved' },
      wire: { kind: 'ig_saved', externalId: null, name: null },
    });
    expect(
      passiveScope(
        'instagram',
        'https://www.instagram.com/someone/saved/ricette-veloci/17890000000000001/',
        null,
      ),
    ).toMatchObject({
      ok: true,
      wire: { kind: 'ig_collection', externalId: '17890000000000001', name: 'Ricette Veloci' },
    });
    for (const url of [
      'https://www.instagram.com/someone/saved/',
      'https://www.instagram.com/p/C8vOfxsVAAB/',
      'https://www.instagram.com/',
      'https://www.instagram.com/explore/',
      'https://www.instagram.com/someone/',
    ])
      expect(passiveScope('instagram', url, null), url).toEqual({
        ok: false,
        reason: 'out_of_scope',
      });
  });

  it('X: bookmarks, always', () => {
    expect(passiveScope('twitter', 'https://x.com/i/bookmarks', null)).toMatchObject({
      ok: true,
      wire: { kind: 'x_bookmarks', externalId: null, name: null },
    });
  });

  it("Pinterest: the signed-in user's boards only", () => {
    const board = 'https://www.pinterest.it/someone/dolci-di-natale/';
    expect(passiveScope('pinterest', board, 'someone')).toMatchObject({
      ok: true,
      wire: { kind: 'pin_board', externalId: 'someone/dolci-di-natale', name: 'Dolci Di Natale' },
    });
    expect(passiveScope('pinterest', board, 'SomeOne').ok).toBe(true);
    expect(passiveScope('pinterest', board, 'someone_else')).toEqual({
      ok: false,
      reason: 'not_own_board',
    });
    expect(passiveScope('pinterest', board, null)).toEqual({ ok: false, reason: 'viewer_unknown' });
    expect(
      passiveScope('pinterest', 'https://www.pinterest.com/pin/900000000000000001/', 'someone'),
    ).toEqual({
      ok: false,
      reason: 'out_of_scope',
    });
    expect(passiveScope('pinterest', 'https://www.pinterest.com/', 'someone')).toEqual({
      ok: false,
      reason: 'out_of_scope',
    });
  });

  it('the IG folder index has no wire listing (not a sync target)', () => {
    const index = classifyListing('instagram', 'https://www.instagram.com/someone/saved/');
    expect(index?.kind).toBe('ig_saved_index');
    expect(toWireListing(index!)).toBeNull();
  });
});

describe('the signed-in Pinterest user', () => {
  const blob = (id: string, value: unknown): void => {
    const script = document.createElement('script');
    script.id = id;
    script.type = 'application/json';
    script.textContent = JSON.stringify(value);
    document.head.append(script);
  };

  it('is read from the page context blobs', () => {
    expect(viewerFromPageData({ props: { context: { user: { username: 'someone' } } } })).toBe(
      'someone',
    );
    expect(viewerFromPageData({ context: { user: { username: 'someone', is_auth: true } } })).toBe(
      'someone',
    );
    expect(
      viewerFromPageData({
        props: { initialReduxState: { context: { user: { username: 'a_b.c' } } } },
      }),
    ).toBe('a_b.c');
    expect(
      viewerFromPageData({ context: { user: { username: 'someone', is_auth: false } } }),
    ).toBeNull();
    expect(viewerFromPageData({ context: { user: { username: '<script>' } } })).toBeNull();
    expect(
      viewerFromPageData({ props: { initialReduxState: { users: { x: { username: 'other' } } } } }),
    ).toBeNull();

    blob('__PWS_DATA__', { props: { context: { user: { username: 'someone' } } } });
    expect(readPinterestViewer(document)).toBe('someone');
  });

  it('falls back to the header profile link', () => {
    document.body.innerHTML = '<div data-test-id="header-profile"><a href="/someone/">me</a></div>';
    expect(readPinterestViewer(document)).toBe('someone');
    document.body.innerHTML =
      '<div data-test-id="header-profile"><a href="/someone/boards/">x</a></div>';
    expect(readPinterestViewer(document)).toBeNull();
  });

  it('is cached once known, and retried at most every 5 s while unknown', () => {
    let now = 0;
    const read = createViewerReader(document, () => now);
    expect(read()).toBeNull();
    blob('__PWS_DATA__', { props: { context: { user: { username: 'someone' } } } });
    now = 4_999;
    expect(read()).toBeNull();
    now = 5_000;
    expect(read()).toBe('someone');
    document.head.innerHTML = '';
    expect(read()).toBe('someone');
  });
});
