// Canonical post identity (plan §2.8), shared by compare.ts and the extension (the server
// canonicalizes keys again at ingest, P2-02).
//
// | Platform  | native id                  | key          | derived from                          |
// |-----------|----------------------------|--------------|---------------------------------------|
// | Instagram | media pk (decimal string)  | `ig_<pk>`    | REST `<pk>_<owner>`, GraphQL `<pk>`,  |
// |           |                            |              | else the shortcode decoded to the pk  |
// | X         | tweet id                   | `x_<id>`     | `rest_id`, else the status URL        |
// | Pinterest | pin id                     | `pin_<id>`   | pin `id`, else the /pin/<id>/ URL     |
//
// The parsers emit IG ids in three forms depending on the capture path (02 risk 4), so every
// record also carries alias keys: matching on any alias collapses the variants of one post.

import type { Platform } from './protocol';

export const KEY_PREFIX: Record<Platform, string> = {
  instagram: 'ig_',
  twitter: 'x_',
  pinterest: 'pin_',
};

/** Alias for an IG shortcode that is kept verbatim (long private shortcodes decode past the pk). */
const IG_SHORTCODE_ALIAS = 'igsc_';
/** Same alphabet as IG_SC_ALPHABET in electron/webview-injected.ts (base64url). */
const IG_ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';
/** Same cap as igTimestampFromShortcode in electron/webview-injected.ts. */
const MAX_SHORTCODE_LEN = 64;
/** Public shortcodes are 11 characters and decode exactly to the pk. */
const PUBLIC_SHORTCODE_LEN = 11;

/** `"<pk>_<owner>"` or `"<pk>"` → pk; anything else → null. */
export function igPkFromId(id: string): string | null {
  const match = /^(\d{1,40})(?:_\d{1,40})?$/.exec(id);
  return match ? match[1] : null;
}

/** Base64url-decodes a shortcode to the pk (as text: long private shortcodes exceed 64 bits). */
export function igShortcodeToPk(shortcode: string): string | null {
  if (!shortcode || shortcode.length > MAX_SHORTCODE_LEN) return null;
  let value = 0n;
  for (const ch of shortcode) {
    const digit = IG_ALPHABET.indexOf(ch);
    if (digit < 0) return null;
    value = value * 64n + BigInt(digit);
  }
  return value > 0n ? value.toString() : null;
}

/** Inverse of igShortcodeToPk for public posts (used by tests and fixtures). */
export function igPkToShortcode(pk: string): string | null {
  if (!/^\d{1,40}$/.test(pk)) return null;
  let value = BigInt(pk);
  if (value === 0n) return null;
  let out = '';
  while (value > 0n) {
    out = IG_ALPHABET[Number(value % 64n)] + out;
    value /= 64n;
  }
  return out;
}

export function igShortcodeFromUrl(url: string): string | null {
  const match = /instagram\.com\/(?:[^/?#]+\/)?(?:p|reel|reels|tv)\/([A-Za-z0-9_-]{1,64})/.exec(
    url,
  );
  return match ? match[1] : null;
}

export function tweetIdFromUrl(url: string): string | null {
  const match = /(?:x|twitter)\.com\/(?:[^/?#]+|i)\/status(?:es)?\/(\d{1,30})(?:[/?#]|$)/.exec(url);
  return match ? match[1] : null;
}

export function pinIdFromUrl(url: string): string | null {
  const match = /pinterest\.[a-z.]+\/pin\/(\d{1,30})(?:[/?#]|$)/.exec(url);
  return match ? match[1] : null;
}

export interface IdentityInput {
  /** Raw ids seen for the post, in preference order. */
  ids: readonly string[];
  shortcode?: string;
  postUrl?: string;
}

export interface CanonicalIdentity {
  platform: Platform;
  key: string;
  nativeId: string;
  /** Every key under which this post may appear on either side (always includes `key`). */
  aliases: string[];
  /** IG only: a public shortcode that does not decode to the pk of the raw id. */
  shortcodeMismatch: boolean;
}

function digitsOnly(value: string | undefined | null): string | null {
  return value && /^\d{1,30}$/.test(value) ? value : null;
}

export function canonicalIdentity(
  platform: Platform,
  input: IdentityInput,
): CanonicalIdentity | null {
  const aliases: string[] = [];
  const addAlias = (alias: string): void => {
    if (!aliases.includes(alias)) aliases.push(alias);
  };
  let nativeId: string | null = null;
  let shortcodeMismatch = false;

  if (platform === 'instagram') {
    const pksFromIds: string[] = [];
    const pksFromIdShortcodes: string[] = [];
    for (const id of input.ids) {
      const pk = igPkFromId(id);
      if (pk) pksFromIds.push(pk);
      else {
        // The parsers fall back to the shortcode as the id when a node has no id/pk.
        const decoded = igShortcodeToPk(id);
        if (decoded) pksFromIdShortcodes.push(decoded);
      }
    }
    const shortcode =
      input.shortcode || (input.postUrl ? igShortcodeFromUrl(input.postUrl) : null) || '';
    const pkFromShortcode = shortcode ? igShortcodeToPk(shortcode) : null;
    nativeId = pksFromIds[0] ?? pkFromShortcode ?? pksFromIdShortcodes[0] ?? null;
    for (const pk of [...pksFromIds, ...pksFromIdShortcodes]) addAlias(KEY_PREFIX.instagram + pk);
    if (pkFromShortcode) addAlias(KEY_PREFIX.instagram + pkFromShortcode);
    if (shortcode) addAlias(IG_SHORTCODE_ALIAS + shortcode);
    shortcodeMismatch =
      !!pksFromIds[0] &&
      !!pkFromShortcode &&
      shortcode.length <= PUBLIC_SHORTCODE_LEN &&
      pkFromShortcode !== pksFromIds[0];
  } else {
    const fromUrl = input.postUrl
      ? platform === 'twitter'
        ? tweetIdFromUrl(input.postUrl)
        : pinIdFromUrl(input.postUrl)
      : null;
    const fromIds = input.ids.map(digitsOnly).filter((id): id is string => id !== null);
    nativeId = fromIds[0] ?? fromUrl;
    for (const id of fromIds) addAlias(KEY_PREFIX[platform] + id);
    if (fromUrl) addAlias(KEY_PREFIX[platform] + fromUrl);
  }

  if (!nativeId) return null;
  const key = KEY_PREFIX[platform] + nativeId;
  if (!aliases.includes(key)) aliases.unshift(key);
  return { platform, key, nativeId, aliases, shortcodeMismatch };
}
