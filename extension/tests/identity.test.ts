import { describe, expect, it } from 'vitest';
import { isAllowedUrl } from '../../src/lib/browserUrls';
import { PINTEREST_HOSTS, SOCIAL_MATCHES, platformForUrl } from '../src/hosts';
import {
  canonicalIdentity,
  igPkFromId,
  igPkToShortcode,
  igShortcodeFromUrl,
  igShortcodeToPk,
  pinIdFromUrl,
  tweetIdFromUrl,
} from '../src/identity';
import { classifyListing, listingKey, listingLabel, parseListingKey } from '../src/listing';
import { classifyMediaUrl, parseCdnExpiry } from '../src/media';

const PK = '3400000000000000001';
const SHORTCODE = 'C8vOfxsVAAB';

describe('Instagram ids', () => {
  it('splits REST "<pk>_<owner>" ids and accepts bare pks', () => {
    expect(igPkFromId(`${PK}_9000000001`)).toBe(PK);
    expect(igPkFromId(PK)).toBe(PK);
    expect(igPkFromId(SHORTCODE)).toBeNull();
    expect(igPkFromId('12_34_56')).toBeNull();
  });

  it('decodes public shortcodes to the pk and back', () => {
    expect(igShortcodeToPk(SHORTCODE)).toBe(PK);
    expect(igPkToShortcode(PK)).toBe(SHORTCODE);
    expect(igShortcodeToPk('bad!chars')).toBeNull();
    expect(igShortcodeToPk('A'.repeat(65))).toBeNull();
  });

  it('reads shortcodes, tweet ids and pin ids from post URLs', () => {
    expect(igShortcodeFromUrl(`https://www.instagram.com/p/${SHORTCODE}/`)).toBe(SHORTCODE);
    expect(igShortcodeFromUrl(`https://www.instagram.com/someone/reel/${SHORTCODE}/?x=1`)).toBe(
      SHORTCODE,
    );
    expect(tweetIdFromUrl('https://x.com/someone/status/1800000000000000001')).toBe(
      '1800000000000000001',
    );
    expect(tweetIdFromUrl('https://twitter.com/i/status/1800000000000000001?s=20')).toBe(
      '1800000000000000001',
    );
    expect(pinIdFromUrl('https://www.pinterest.it/pin/900000000000000001/')).toBe(
      '900000000000000001',
    );
  });
});

describe('canonicalIdentity (plan §2.8)', () => {
  it('collapses the three IG id forms onto ig_<pk>', () => {
    const rest = canonicalIdentity('instagram', {
      ids: [`${PK}_9000000001`],
      shortcode: SHORTCODE,
    });
    const graphql = canonicalIdentity('instagram', { ids: [PK], shortcode: SHORTCODE });
    const shortcodeOnly = canonicalIdentity('instagram', {
      ids: [SHORTCODE],
      shortcode: SHORTCODE,
    });
    for (const identity of [rest, graphql, shortcodeOnly]) {
      expect(identity?.key).toBe(`ig_${PK}`);
      expect(identity?.nativeId).toBe(PK);
      expect(identity?.aliases).toEqual(expect.arrayContaining([`ig_${PK}`, `igsc_${SHORTCODE}`]));
      expect(identity?.shortcodeMismatch).toBe(false);
    }
  });

  it('falls back to the shortcode in the post URL', () => {
    const identity = canonicalIdentity('instagram', {
      ids: ['not-an-id!'],
      postUrl: `https://www.instagram.com/p/${SHORTCODE}/`,
    });
    expect(identity?.key).toBe(`ig_${PK}`);
  });

  it('flags a public shortcode that does not decode to the id', () => {
    const identity = canonicalIdentity('instagram', {
      ids: ['3400000000000000009_1'],
      shortcode: SHORTCODE,
    });
    expect(identity?.key).toBe('ig_3400000000000000009');
    expect(identity?.shortcodeMismatch).toBe(true);
    expect(identity?.aliases).toContain(`ig_${PK}`);
  });

  it('keys X and Pinterest by their numeric ids, with the URL as fallback', () => {
    expect(canonicalIdentity('twitter', { ids: ['1800000000000000001'] })?.key).toBe(
      'x_1800000000000000001',
    );
    expect(
      canonicalIdentity('twitter', {
        ids: ['weird'],
        postUrl: 'https://x.com/someone/status/1800000000000000002',
      })?.key,
    ).toBe('x_1800000000000000002');
    expect(canonicalIdentity('pinterest', { ids: ['900000000000000001'] })?.key).toBe(
      'pin_900000000000000001',
    );
    expect(canonicalIdentity('pinterest', { ids: ['abc'] })).toBeNull();
  });
});

describe('media URLs', () => {
  it('parses the hex oe expiry of signed IG/FB CDN URLs only', () => {
    expect(
      parseCdnExpiry('https://scontent-synth1-1.cdninstagram.com/v/a.jpg?oh=00_X&oe=68F00000'),
    ).toBe(0x68f00000 * 1000);
    expect(parseCdnExpiry('https://video-synth.xx.fbcdn.net/v/a.mp4?oe=68F00000')).toBe(
      0x68f00000 * 1000,
    );
    expect(parseCdnExpiry('https://pbs.twimg.com/media/a.jpg?oe=68F00000')).toBeNull();
    expect(parseCdnExpiry('https://scontent.cdninstagram.com/v/a.jpg')).toBeNull();
    expect(parseCdnExpiry('https://scontent.cdninstagram.com/v/a.jpg?oe=zz')).toBeNull();
    expect(parseCdnExpiry('https://scontent.cdninstagram.com/v/a.jpg?oe=1')).toBeNull();
  });

  it('tells posters (the image the parser kept for a video) from direct video URLs', () => {
    expect(classifyMediaUrl('image', 'https://pbs.twimg.com/media/a.jpg')).toBe('image');
    expect(
      classifyMediaUrl('video', 'https://pbs.twimg.com/ext_tw_video_thumb/1/pu/img/a.jpg'),
    ).toBe('poster');
    expect(classifyMediaUrl('video', 'https://v1.pinimg.com/videos/mc/720p/a/b/c/a.mp4')).toBe(
      'video',
    );
    expect(classifyMediaUrl('video', 'https://v1.pinimg.com/videos/mc/hls/a/b/c/a.m3u8')).toBe(
      'video',
    );
  });
});

describe('hosts', () => {
  it('every Pinterest ccTLD in the manifest passes the desktop allowlist', () => {
    for (const host of PINTEREST_HOSTS) {
      expect(isAllowedUrl('pinterest', `https://www.${host}/someone/board/`)).toBe(true);
      expect(platformForUrl(`https://it.${host}/`)).toBe('pinterest');
    }
  });

  it('maps hosts to platforms and refuses lookalikes and plain http', () => {
    expect(platformForUrl('https://www.instagram.com/someone/saved/')).toBe('instagram');
    expect(platformForUrl('https://x.com/i/bookmarks')).toBe('twitter');
    expect(platformForUrl('https://twitter.com/i/bookmarks')).toBe('twitter');
    expect(platformForUrl('https://pinterest.com.evil.io/')).toBeNull();
    expect(platformForUrl('http://www.instagram.com/')).toBeNull();
    expect(platformForUrl('https://example.com/')).toBeNull();
  });

  it('exposes one https match pattern per supported host', () => {
    expect(SOCIAL_MATCHES).toContain('https://www.instagram.com/*');
    expect(SOCIAL_MATCHES).toContain('https://*.pinterest.co.uk/*');
    expect(new Set(SOCIAL_MATCHES).size).toBe(SOCIAL_MATCHES.length);
  });
});

describe('classifyListing (passive-capture scope, plan §2.16)', () => {
  it('Instagram: all posts, folders and the folder index; nothing else', () => {
    expect(
      classifyListing('instagram', 'https://www.instagram.com/someone/saved/all-posts/'),
    ).toEqual({
      key: 'instagram:ig_saved',
      platform: 'instagram',
      kind: 'ig_saved',
      externalId: null,
      name: 'all-posts',
      account: 'someone',
    });
    expect(
      classifyListing(
        'instagram',
        'https://www.instagram.com/someone/saved/recipes/17890000000000001/',
      ),
    ).toMatchObject({
      key: 'instagram:ig_collection:17890000000000001',
      kind: 'ig_collection',
      externalId: '17890000000000001',
      name: 'recipes',
    });
    expect(classifyListing('instagram', 'https://www.instagram.com/someone/saved/')?.kind).toBe(
      'ig_saved_index',
    );
    expect(classifyListing('instagram', 'https://www.instagram.com/')).toBeNull();
    expect(classifyListing('instagram', 'https://www.instagram.com/explore/')).toBeNull();
    expect(classifyListing('instagram', 'https://www.instagram.com/p/C8vOfxsVAAB/')).toBeNull();
  });

  it('X: always bookmarks (the hook only accepts bookmark responses)', () => {
    expect(classifyListing('twitter', 'https://x.com/i/history')?.key).toBe('twitter:x_bookmarks');
    expect(classifyListing('twitter', 'https://x.com/i/bookmarks')?.key).toBe(
      'twitter:x_bookmarks',
    );
  });

  it('Pinterest: board pages (sections file under the board), not pins or profile tabs', () => {
    expect(classifyListing('pinterest', 'https://www.pinterest.it/someone/recipes/')).toMatchObject(
      {
        key: 'pinterest:pin_board:someone/recipes',
        externalId: 'someone/recipes',
        account: 'someone',
      },
    );
    expect(
      classifyListing('pinterest', 'https://it.pinterest.com/someone/recipes/desserts/')?.key,
    ).toBe('pinterest:pin_board:someone/recipes');
    expect(
      classifyListing('pinterest', 'https://www.pinterest.com/pin/900000000000000001/'),
    ).toBeNull();
    expect(classifyListing('pinterest', 'https://www.pinterest.com/someone/_saved/')).toBeNull();
    expect(classifyListing('pinterest', 'https://www.pinterest.com/search/pins/?q=x')).toBeNull();
    expect(classifyListing('pinterest', 'https://www.pinterest.com/')).toBeNull();
  });

  it('listing keys round-trip and validate their platform and external id', () => {
    expect(parseListingKey(listingKey('instagram', 'ig_collection', '1789'))).toEqual({
      platform: 'instagram',
      kind: 'ig_collection',
      externalId: '1789',
    });
    expect(parseListingKey('pinterest:pin_board:someone/recipes')?.externalId).toBe(
      'someone/recipes',
    );
    expect(parseListingKey('twitter:x_bookmarks')?.externalId).toBeNull();
    expect(parseListingKey('twitter:ig_saved')).toBeNull();
    expect(parseListingKey('instagram:ig_collection')).toBeNull();
    expect(parseListingKey('instagram:ig_saved:1')).toBeNull();
    expect(
      listingLabel({ kind: 'pin_board', externalId: 'someone/recipes', name: 'recipes' }),
    ).toBe('Pinterest · board someone/recipes');
  });
});
