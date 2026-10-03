// Listing context: which saved listing a batch was captured on, derived from the URL of the
// document that relayed it, and the passive-capture scope of plan §2.16: IG only on saved
// listings (SAVED_PATTERNS), X bookmarks always (the hook's matcher only accepts bookmark
// responses), Pinterest only on the signed-in user's own boards. Everything else is discarded,
// as the desktop's pre-sync buffer does (SYNC-16).

import {
  SAVED_PATTERNS,
  deslugify,
  parseIgFolder,
  parsePinBoard,
} from '../../../src/lib/browserUrls';
import { isPlatform, type Platform } from './protocol';

export const LISTING_KINDS = [
  'ig_saved', // /<user>/saved/all-posts/
  'ig_collection', // /<user>/saved/<slug>/<folderId>/  (externalId = numeric folder id)
  'ig_saved_index', // /<user>/saved/ (folder index; not a sync target, never captured)
  'x_bookmarks', // /i/bookmarks, /i/history (Bookmarks tab)
  'pin_board', // /<user>/<board>/[<section>/]  (externalId = "<user>/<board>", as the desktop)
] as const;
export type ListingKind = (typeof LISTING_KINDS)[number];

/** The listing kinds of contract C4 (`listing.kind` of a sync run). */
export const WIRE_LISTING_KINDS = [
  'ig_saved',
  'ig_collection',
  'x_bookmarks',
  'pin_board',
] as const;
export type WireListingKind = (typeof WIRE_LISTING_KINDS)[number];

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
  /** Account segment of the URL (IG username, Pinterest board owner). */
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

/** A listing as a sync run names it (contract C4). */
export interface WireListing {
  kind: WireListingKind;
  externalId: string | null;
  name: string | null;
}

/**
 * The C4 listing of a captured listing, or null for the IG folder index (not a sync target).
 * Folder and board names come from the URL slug, deslugified as the desktop names new folders;
 * the server uses the name only when it creates the collection.
 */
export function toWireListing(listing: Listing): WireListing | null {
  switch (listing.kind) {
    case 'ig_saved':
    case 'x_bookmarks':
      return { kind: listing.kind, externalId: null, name: null };
    case 'ig_collection':
    case 'pin_board':
      return {
        kind: listing.kind,
        externalId: listing.externalId,
        name: listing.name ? deslugify(listing.name, listing.name) : null,
      };
    case 'ig_saved_index':
      return null;
  }
}

/** Why a passive batch was not captured. */
export type PassiveScopeRefusal = 'out_of_scope' | 'not_own_board' | 'viewer_unknown';

export type PassiveScope =
  | { ok: true; listing: Listing; wire: WireListing }
  | { ok: false; reason: PassiveScopeRefusal };

const sameAccount = (a: string, b: string): boolean => a.toLowerCase() === b.toLowerCase();

/**
 * The passive-capture scope of a batch relayed from `pageUrl` (plan §2.16): IG saved (all posts)
 * and folders, X bookmarks, and the boards of the signed-in Pinterest user, whose username the
 * bridge reads from the page (`viewer`). Another user's board, or a board seen while the
 * signed-in user is unknown, is discarded.
 */
export function passiveScope(
  platform: Platform,
  pageUrl: string,
  viewer: string | null,
): PassiveScope {
  const listing = classifyListing(platform, pageUrl);
  const wire = listing ? toWireListing(listing) : null;
  if (!listing || !wire) return { ok: false, reason: 'out_of_scope' };
  if (listing.kind === 'pin_board') {
    if (!viewer) return { ok: false, reason: 'viewer_unknown' };
    if (!listing.account || !sameAccount(listing.account, viewer))
      return { ok: false, reason: 'not_own_board' };
  }
  return { ok: true, listing, wire };
}

/**
 * The listing an explicit sync of `pageUrl` walks (P2-13): a saved listing (SAVED_PATTERNS)
 * that is a sync target, i.e. not the IG folder index. Unlike classifyListing, which reads any
 * X page as the bookmarks, it needs the page itself to be the listing.
 */
export function syncTarget(platform: Platform, pageUrl: string): Listing | null {
  if (!SAVED_PATTERNS[platform].test(pageUrl)) return null;
  const listing = classifyListing(platform, pageUrl);
  return listing && toWireListing(listing) ? listing : null;
}
