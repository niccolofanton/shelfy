// Listing context: which saved listing a batch was captured on, derived from the URL of the
// document that relayed it. Mirrors the passive-capture scoping rules of plan §2.16: IG only on
// saved listings (SAVED_PATTERNS), Pinterest only on board pages, X bookmarks always (the hook's
// matcher only accepts bookmark responses). Everything else is out of scope and discarded.

import { SAVED_PATTERNS, parseIgFolder, parsePinBoard } from '../../src/lib/browserUrls';
import { isPlatform, type Platform } from './protocol';

export const LISTING_KINDS = [
  'ig_saved', // /<user>/saved/all-posts/
  'ig_collection', // /<user>/saved/<slug>/<folderId>/  (externalId = numeric folder id)
  'ig_saved_index', // /<user>/saved/ (folder index; not a desktop sync target)
  'x_bookmarks', // /i/bookmarks, /i/history (Bookmarks tab)
  'pin_board', // /<user>/<board>/[<section>/]  (externalId = "<user>/<board>", as the desktop)
] as const;
export type ListingKind = (typeof LISTING_KINDS)[number];

const KIND_PLATFORM: Record<ListingKind, Platform> = {
  ig_saved: 'instagram',
  ig_collection: 'instagram',
  ig_saved_index: 'instagram',
  x_bookmarks: 'twitter',
  pin_board: 'pinterest',
};

const KINDS_WITH_EXTERNAL_ID: readonly ListingKind[] = ['ig_collection', 'pin_board'];

export interface Listing {
  key: string;
  platform: Platform;
  kind: ListingKind;
  externalId: string | null;
  /** Folder or board slug, when the URL carries one. */
  name: string | null;
  /** Account segment of the URL (IG username, Pinterest board owner), informational. */
  account: string | null;
}

export function isListingKind(value: unknown): value is ListingKind {
  return typeof value === 'string' && (LISTING_KINDS as readonly string[]).includes(value);
}

export function listingKey(
  platform: Platform,
  kind: ListingKind,
  externalId: string | null,
): string {
  return externalId ? `${platform}:${kind}:${externalId}` : `${platform}:${kind}`;
}

export interface ParsedListingKey {
  platform: Platform;
  kind: ListingKind;
  externalId: string | null;
}

export function parseListingKey(key: string): ParsedListingKey | null {
  const match = /^([a-z]+):([a-z_]+)(?::(.+))?$/.exec(key);
  if (!match || !isPlatform(match[1]) || !isListingKind(match[2])) return null;
  const platform = match[1];
  const kind = match[2];
  const externalId = match[3] ?? null;
  if (KIND_PLATFORM[kind] !== platform) return null;
  if (KINDS_WITH_EXTERNAL_ID.includes(kind) !== (externalId !== null)) return null;
  return { platform, kind, externalId };
}

function make(
  platform: Platform,
  kind: ListingKind,
  externalId: string | null,
  name: string | null,
  account: string | null,
): Listing {
  return { key: listingKey(platform, kind, externalId), platform, kind, externalId, name, account };
}

/** The saved listing `pageUrl` shows on `platform`, or null when the page is out of scope. */
export function classifyListing(platform: Platform, pageUrl: string): Listing | null {
  let url: URL;
  try {
    url = new URL(pageUrl);
  } catch {
    return null;
  }
  const href = url.href;
  const segments = url.pathname.split('/').filter(Boolean);
  switch (platform) {
    case 'instagram': {
      const folder = parseIgFolder(href);
      if (folder) return make(platform, 'ig_collection', folder.folderId, folder.slug, segments[0]);
      if (!SAVED_PATTERNS.instagram.test(href)) return null;
      const savedAt = segments.indexOf('saved');
      const account = savedAt > 0 ? segments[savedAt - 1] : null;
      if (segments[savedAt + 1] === 'all-posts')
        return make(platform, 'ig_saved', null, 'all-posts', account);
      return make(platform, 'ig_saved_index', null, null, account);
    }
    case 'twitter':
      return make(platform, 'x_bookmarks', null, null, null);
    case 'pinterest': {
      if (!SAVED_PATTERNS.pinterest.test(href)) return null;
      const board = parsePinBoard(href);
      return board ? make(platform, 'pin_board', board.boardId, board.slug, board.user) : null;
    }
  }
}

export function listingLabel(listing: Pick<Listing, 'kind' | 'externalId' | 'name'>): string {
  switch (listing.kind) {
    case 'ig_saved':
      return 'Instagram · saved (all posts)';
    case 'ig_collection':
      return `Instagram · folder ${listing.name ?? '?'} (${listing.externalId ?? '?'})`;
    case 'ig_saved_index':
      return 'Instagram · saved index (folder grid)';
    case 'x_bookmarks':
      return 'X · bookmarks';
    case 'pin_board':
      return `Pinterest · board ${listing.externalId ?? '?'}`;
  }
}
