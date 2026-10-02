// Capture store: pure functions the service worker runs on each relayed batch. Storage I/O
// lives in sw.ts; everything here is deterministic and unit-tested.
//
// Storage layout (chrome.storage.local, `unlimitedStorage`):
//   meta              Meta: per-platform and per-listing counters, census, diagnostics
//   log               LogEntry[] (newest last, capped)
//   item:<key>        StoredItem, one per canonical post key (ig_<pk>, x_<id>, pin_<id>)

import { sanitizeInterceptedBatch } from '../../src/lib/browserSanitize';
import { canonicalIdentity, type CanonicalIdentity } from './identity';
import { classifyListing, type Listing, type ListingKind } from './listing';
import { classifyMediaUrl, parseCdnExpiry, urlHost, type MediaUrlKind } from './media';
import {
  MAX_CENSUS_KEYS,
  PLATFORMS,
  SOURCES,
  isPlatform,
  isRecord,
  type BatchMessage,
  type CaptureSource,
  type CensusCounts,
  type MarkerMessage,
  type Platform,
  type ScopeDetail,
} from './protocol';

export const STORAGE_SCHEMA = 1;
export const META_KEY = 'meta';
export const LOG_KEY = 'log';
export const ITEM_KEY_PREFIX = 'item:';
export const LOG_LIMIT = 300;
const MAX_RAW_IDS = 8;
const MAX_CENSUS_LABELS_PER_PLATFORM = MAX_CENSUS_KEYS;

export const itemStorageKey = (key: string): string => ITEM_KEY_PREFIX + key;

// ── Types ───────────────────────────────────────────────────────────────────

export interface MediaObservation {
  slot: 'cover' | 'slide';
  /** 0-based slide position; null for the cover. */
  position: number | null;
  type: 'image' | 'video';
  urlKind: MediaUrlKind;
  url: string;
  host: string;
  /** Last time this exact URL was observed (unix ms). */
  capturedAt: number;
  /** IG/FB `oe` expiry (unix ms), null when the URL does not expire. */
  expiresAt: number | null;
  observations: number;
}

export interface Membership {
  firstAt: number;
  lastAt: number;
  sources: CaptureSource[];
}

export interface StoredItem {
  key: string;
  platform: Platform;
  nativeId: string;
  rawIds: string[];
  shortcode: string;
  postUrl: string;
  mediaType: string;
  mediaCount: number;
  /** Post date from the parser (ISO), '' when unknown. */
  postedAt: string;
  firstCapturedAt: number;
  lastCapturedAt: number;
  captureCount: number;
  /** Listing key → membership. */
  listings: Record<string, Membership>;
  media: MediaObservation[];
}

export interface ListingStats {
  key: string;
  platform: Platform;
  kind: ListingKind;
  externalId: string | null;
  name: string | null;
  account: string | null;
  firstSeenAt: number;
  lastSeenAt: number;
  batches: number;
  itemsReceived: number;
  uniqueItems: number;
  /** Unique items of this listing captured at least once through each source. */
  bySource: Record<CaptureSource, number>;
  endOfFeedSeen: boolean;
  lastHasNextPage: boolean | null;
}

export interface CensusEntry {
  requests: number;
  /** Requests made while the page was an in-scope listing. */
  inScope: number;
}

export interface PlatformStats {
  uniqueItems: number;
  batches: number;
  itemsReceived: number;
  discardedBatches: number;
  discardedItems: number;
  rejectedItems: number;
  lastCaptureAt: number | null;
  census: Record<string, CensusEntry>;
}

export interface Diagnostics {
  /** IG items whose public shortcode does not decode to the pk of their id (§2.8 check). */
  igShortcodeMismatch: number;
  /** Batches refused by the service worker, by reason. */
  refusedBatches: Record<string, number>;
}

export interface Meta {
  schema: typeof STORAGE_SCHEMA;
  createdAt: number;
  updatedAt: number;
  logSeq: number;
  platforms: Record<Platform, PlatformStats>;
  listings: Record<string, ListingStats>;
  diagnostics: Diagnostics;
}

interface LogBase {
  seq: number;
  at: number;
  platform: Platform | null;
  path: string;
}

export interface BatchLogEntry extends LogBase {
  kind: 'batch';
  listingKey: string;
  source: CaptureSource;
  received: number;
  accepted: number;
  rejected: number;
  newItems: number;
  newInListing: number;
  hasNextPage: boolean | null;
}

export interface DiscardLogEntry extends LogBase {
  kind: 'discard';
  reason: string;
  received: number;
  source: CaptureSource | null;
}

export interface MarkerLogEntry extends LogBase {
  kind: 'marker';
  source: CaptureSource;
  phase: 'start' | 'end';
  id: string;
  detail: ScopeDetail;
}

export type LogEntry = BatchLogEntry | DiscardLogEntry | MarkerLogEntry;

// ── Construction and guards ─────────────────────────────────────────────────

function zeroBySource(): Record<CaptureSource, number> {
  return Object.fromEntries(SOURCES.map((source) => [source, 0])) as Record<CaptureSource, number>;
}

function emptyPlatformStats(): PlatformStats {
  return {
    uniqueItems: 0,
    batches: 0,
    itemsReceived: 0,
    discardedBatches: 0,
    discardedItems: 0,
    rejectedItems: 0,
    lastCaptureAt: null,
    census: {},
  };
}

export function emptyMeta(now: number): Meta {
  return {
    schema: STORAGE_SCHEMA,
    createdAt: now,
    updatedAt: now,
    logSeq: 0,
    platforms: Object.fromEntries(
      PLATFORMS.map((platform) => [platform, emptyPlatformStats()]),
    ) as Record<Platform, PlatformStats>,
    listings: {},
    diagnostics: { igShortcodeMismatch: 0, refusedBatches: {} },
  };
}

/** The stored meta, or a fresh one when storage is empty or holds another schema. */
export function readMeta(value: unknown, now: number): Meta {
  return isRecord(value) && value.schema === STORAGE_SCHEMA
    ? (value as unknown as Meta)
    : emptyMeta(now);
}

export function isStoredItem(value: unknown): value is StoredItem {
  return (
    isRecord(value) &&
    typeof value.key === 'string' &&
    isPlatform(value.platform) &&
    isRecord(value.listings) &&
    Array.isArray(value.media)
  );
}

export function readLog(value: unknown): LogEntry[] {
  return Array.isArray(value) ? (value.filter(isRecord) as unknown as LogEntry[]) : [];
}

export function appendLog(log: unknown, entries: readonly LogEntry[]): LogEntry[] {
  return [...readLog(log), ...entries].slice(-LOG_LIMIT);
}

function pathOf(pageUrl: string): string {
  try {
    return new URL(pageUrl).pathname;
  } catch {
    return '';
  }
}

// ── Batch preparation (sanitize → canonical identity → media) ───────────────

export interface PreparedItem {
  identity: CanonicalIdentity;
  rawId: string;
  shortcode: string;
  postUrl: string;
  mediaType: string;
  postedAt: string;
  media: MediaObservation[];
}

export interface PreparedBatch {
  platform: Platform;
  listing: Listing | null;
  source: CaptureSource;
  path: string;
  received: number;
  items: PreparedItem[];
  rejected: number;
  hasNextPage: boolean | null;
}

function observation(
  slot: MediaObservation['slot'],
  position: number | null,
  type: 'image' | 'video',
  url: string,
  at: number,
): MediaObservation {
  return {
    slot,
    position,
    type,
    urlKind: classifyMediaUrl(type, url),
    url,
    host: urlHost(url),
    capturedAt: at,
    expiresAt: parseCdnExpiry(url),
    observations: 1,
  };
}

/**
 * Validates a relayed batch exactly like the desktop does before ingest
 * (sanitizeInterceptedBatch: bounded ids/fields, http(s) media only, batch platform stamped on
 * every item), then derives canonical keys, the listing and the media observations.
 */
export function prepareBatch(message: BatchMessage, at: number): PreparedBatch {
  const sanitized = sanitizeInterceptedBatch(message.items, message.platform);
  const items: PreparedItem[] = [];
  for (const item of sanitized) {
    const identity = canonicalIdentity(message.platform, {
      ids: [item.id],
      shortcode: item.shortcode,
      postUrl: item.postUrl,
    });
    if (!identity) continue;
    const media: MediaObservation[] = [];
    if (item.thumbnailUrl) media.push(observation('cover', null, 'image', item.thumbnailUrl, at));
    item.media.forEach((slide, position) =>
      media.push(observation('slide', position, slide.type, slide.url, at)),
    );
    items.push({
      identity,
      rawId: item.id,
      shortcode: item.shortcode,
      postUrl: item.postUrl,
      mediaType: item.mediaType,
      postedAt: item.timestamp,
      media,
    });
  }
  return {
    platform: message.platform,
    listing: classifyListing(message.platform, message.pageUrl),
    source: message.source,
    path: pathOf(message.pageUrl),
    received: message.items.length,
    items,
    rejected: message.items.length - items.length,
    hasNextPage: message.hasNextPage,
  };
}

// ── Merge ───────────────────────────────────────────────────────────────────

function cloneItem(item: StoredItem): StoredItem {
  return structuredClone(item);
}

function mergeMedia(existing: MediaObservation[], fresh: MediaObservation[]): MediaObservation[] {
  const out = existing.map((m) => ({ ...m }));
  for (const obs of fresh) {
    const slot = out.find((m) => m.slot === obs.slot && m.position === obs.position);
    if (!slot) {
      out.push({ ...obs });
      continue;
    }
    slot.observations += 1;
    slot.capturedAt = obs.capturedAt;
    if (slot.url !== obs.url) {
      // Keep the latest URL: IG re-signs URLs on every response, the newest lives longest.
      slot.url = obs.url;
      slot.host = obs.host;
      slot.type = obs.type;
      slot.urlKind = obs.urlKind;
      slot.expiresAt = obs.expiresAt;
    }
  }
  return out.sort(
    (a, b) =>
      (a.slot === b.slot ? 0 : a.slot === 'cover' ? -1 : 1) ||
      (a.position ?? -1) - (b.position ?? -1),
  );
}

function newStoredItem(prepared: PreparedItem, platform: Platform, at: number): StoredItem {
  return {
    key: prepared.identity.key,
    platform,
    nativeId: prepared.identity.nativeId,
    rawIds: [prepared.rawId],
    shortcode: prepared.shortcode,
    postUrl: prepared.postUrl,
    mediaType: prepared.mediaType,
    mediaCount: prepared.media.filter((m) => m.slot === 'slide').length,
    postedAt: prepared.postedAt,
    firstCapturedAt: at,
    lastCapturedAt: at,
    captureCount: 0,
    listings: {},
    media: [],
  };
}

export interface ApplyResult {
  meta: Meta;
  /** Items created or updated by this batch (to persist). */
  changed: StoredItem[];
  entry: LogEntry;
}

/**
 * Merges a prepared batch into the store. `existing` holds the stored records for the batch's
 * keys (absent keys are new posts). Out-of-scope batches are counted and discarded.
 */
export function applyBatch(
  metaIn: Meta,
  existing: ReadonlyMap<string, StoredItem>,
  batch: PreparedBatch,
  at: number,
): ApplyResult {
  const meta = structuredClone(metaIn);
  meta.updatedAt = at;
  meta.logSeq += 1;
  const stats = meta.platforms[batch.platform];
  const base = { seq: meta.logSeq, at, platform: batch.platform, path: batch.path };

  if (!batch.listing) {
    stats.discardedBatches += 1;
    stats.discardedItems += batch.received;
    const entry: DiscardLogEntry = {
      ...base,
      kind: 'discard',
      reason: 'out_of_scope',
      received: batch.received,
      source: batch.source,
    };
    return { meta, changed: [], entry };
  }

  const listing = batch.listing;
  const listingStats: ListingStats = meta.listings[listing.key] ?? {
    ...listing,
    firstSeenAt: at,
    lastSeenAt: at,
    batches: 0,
    itemsReceived: 0,
    uniqueItems: 0,
    bySource: zeroBySource(),
    endOfFeedSeen: false,
    lastHasNextPage: null,
  };
  meta.listings[listing.key] = listingStats;
  listingStats.lastSeenAt = at;
  listingStats.batches += 1;
  listingStats.itemsReceived += batch.received;
  listingStats.lastHasNextPage = batch.hasNextPage;
  if (batch.hasNextPage === false) listingStats.endOfFeedSeen = true;
  stats.batches += 1;
  stats.itemsReceived += batch.received;
  stats.rejectedItems += batch.rejected;
  if (batch.items.length) stats.lastCaptureAt = at;

  const working = new Map<string, StoredItem>();
  let newItems = 0;
  let newInListing = 0;
  for (const prepared of batch.items) {
    const key = prepared.identity.key;
    let item = working.get(key);
    if (!item) {
      const stored = existing.get(key);
      item = stored ? cloneItem(stored) : newStoredItem(prepared, batch.platform, at);
      if (!stored) {
        newItems += 1;
        stats.uniqueItems += 1;
      }
      working.set(key, item);
    }
    if (prepared.identity.shortcodeMismatch) meta.diagnostics.igShortcodeMismatch += 1;

    item.lastCapturedAt = at;
    item.captureCount += 1;
    if (!item.rawIds.includes(prepared.rawId) && item.rawIds.length < MAX_RAW_IDS)
      item.rawIds.push(prepared.rawId);
    item.shortcode ||= prepared.shortcode;
    item.postUrl ||= prepared.postUrl;
    item.mediaType ||= prepared.mediaType;
    item.postedAt ||= prepared.postedAt;
    item.media = mergeMedia(item.media, prepared.media);
    item.mediaCount = Math.max(
      item.mediaCount,
      item.media.filter((m) => m.slot === 'slide').length,
    );

    let membership = item.listings[listing.key];
    if (!membership) {
      membership = { firstAt: at, lastAt: at, sources: [] };
      item.listings[listing.key] = membership;
      listingStats.uniqueItems += 1;
      newInListing += 1;
    }
    membership.lastAt = at;
    if (!membership.sources.includes(batch.source)) {
      membership.sources.push(batch.source);
      listingStats.bySource[batch.source] += 1;
    }
  }

  const entry: BatchLogEntry = {
    ...base,
    kind: 'batch',
    listingKey: listing.key,
    source: batch.source,
    received: batch.received,
    accepted: batch.items.length,
    rejected: batch.rejected,
    newItems,
    newInListing,
    hasNextPage: batch.hasNextPage,
  };
  return { meta, changed: [...working.values()], entry };
}

/** Counts a batch the service worker refused before preparing it (bad sender, bad shape…). */
export function recordRefusal(metaIn: Meta, reason: string, at: number): Meta {
  const meta = structuredClone(metaIn);
  meta.updatedAt = at;
  meta.diagnostics.refusedBatches[reason] = (meta.diagnostics.refusedBatches[reason] ?? 0) + 1;
  return meta;
}

/** Adds request counts from the MAIN-world census (keys `"<platform>|<label>"`). */
export function applyCensus(metaIn: Meta, counts: CensusCounts, pageUrl: string, at: number): Meta {
  const meta = structuredClone(metaIn);
  meta.updatedAt = at;
  for (const [compound, count] of Object.entries(counts)) {
    const split = compound.indexOf('|');
    const platform = compound.slice(0, split);
    const label = compound.slice(split + 1);
    if (!isPlatform(platform) || !label) continue;
    const census = meta.platforms[platform].census;
    if (!census[label] && Object.keys(census).length >= MAX_CENSUS_LABELS_PER_PLATFORM) continue;
    const entry = (census[label] ??= { requests: 0, inScope: 0 });
    entry.requests += count;
    if (classifyListing(platform, pageUrl)) entry.inScope += count;
  }
  return meta;
}

/** Logs a replay start/end marker relayed from the page. */
export function applyMarker(
  metaIn: Meta,
  marker: MarkerMessage,
  platform: Platform | null,
  at: number,
): { meta: Meta; entry: MarkerLogEntry } {
  const meta = structuredClone(metaIn);
  meta.updatedAt = at;
  meta.logSeq += 1;
  const entry: MarkerLogEntry = {
    seq: meta.logSeq,
    at,
    platform,
    path: pathOf(marker.pageUrl),
    kind: 'marker',
    source: marker.source,
    phase: marker.phase,
    id: marker.id,
    detail: marker.detail,
  };
  return { meta, entry };
}
