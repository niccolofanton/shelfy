// "Export JSON" format of the side panel. Read by extension/scripts/compare.ts (SPIKE-3) and by
// the SPIKE-2 CDN fetch test, which needs the fresh media URLs with their capture time and expiry.
// All timestamps are unix milliseconds. Captions and author fields are never captured.

import type { ListingKind } from './listing';
import type { MediaUrlKind } from './media';
import {
  PLATFORMS,
  SOURCES,
  isPlatform,
  isRecord,
  type CaptureSource,
  type Platform,
} from './protocol';
import type { Meta, PlatformStats, StoredItem } from './store';

export const EXPORT_FORMAT = 'shelfy-spike3-capture';
export const EXPORT_VERSION = 1;

export interface ExportMedia {
  slot: 'cover' | 'slide';
  position: number | null;
  type: 'image' | 'video';
  /** `poster` = the image the parser kept for a video slide; `video` = a direct video URL. */
  urlKind: MediaUrlKind;
  url: string;
  host: string;
  capturedAt: number;
  expiresAt: number | null;
}

export interface ExportItemListing {
  key: string;
  sources: CaptureSource[];
  firstCapturedAt: number;
  lastCapturedAt: number;
}

export interface ExportItem {
  /** Canonical key (plan §2.8): ig_<pk>, x_<id>, pin_<id>. */
  key: string;
  platform: Platform;
  nativeId: string;
  /** Raw ids as the parsers emitted them (IG: "<pk>_<owner>", "<pk>" or a shortcode). */
  rawIds: string[];
  shortcode: string;
  postUrl: string;
  mediaType: string;
  mediaCount: number;
  postedAt: string;
  firstCapturedAt: number;
  lastCapturedAt: number;
  captureCount: number;
  listings: ExportItemListing[];
  media: ExportMedia[];
}

export interface ExportListing {
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
  bySource: Record<CaptureSource, number>;
  endOfFeedSeen: boolean;
  lastHasNextPage: boolean | null;
}

export interface ExportFile {
  format: typeof EXPORT_FORMAT;
  version: typeof EXPORT_VERSION;
  exportedAt: string;
  extension: { version: string; userAgent: string };
  storeCreatedAt: number;
  platforms: Record<Platform, PlatformStats>;
  diagnostics: Meta['diagnostics'];
  listings: ExportListing[];
  items: ExportItem[];
}

export interface ExportEnvironment {
  extensionVersion: string;
  userAgent: string;
  now: number;
}

export function buildExport(
  meta: Meta,
  items: readonly StoredItem[],
  env: ExportEnvironment,
): ExportFile {
  const sorted = [...items].sort(
    (a, b) => a.firstCapturedAt - b.firstCapturedAt || a.key.localeCompare(b.key),
  );
  return {
    format: EXPORT_FORMAT,
    version: EXPORT_VERSION,
    exportedAt: new Date(env.now).toISOString(),
    extension: { version: env.extensionVersion, userAgent: env.userAgent },
    storeCreatedAt: meta.createdAt,
    platforms: meta.platforms,
    diagnostics: meta.diagnostics,
    listings: Object.values(meta.listings).sort((a, b) => a.key.localeCompare(b.key)),
    items: sorted.map((item) => ({
      key: item.key,
      platform: item.platform,
      nativeId: item.nativeId,
      rawIds: item.rawIds,
      shortcode: item.shortcode,
      postUrl: item.postUrl,
      mediaType: item.mediaType,
      mediaCount: item.mediaCount,
      postedAt: item.postedAt,
      firstCapturedAt: item.firstCapturedAt,
      lastCapturedAt: item.lastCapturedAt,
      captureCount: item.captureCount,
      listings: Object.entries(item.listings)
        .map(([key, membership]) => ({
          key,
          sources: membership.sources,
          firstCapturedAt: membership.firstAt,
          lastCapturedAt: membership.lastAt,
        }))
        .sort((a, b) => a.key.localeCompare(b.key)),
      media: item.media.map((m) => ({
        slot: m.slot,
        position: m.position,
        type: m.type,
        urlKind: m.urlKind,
        url: m.url,
        host: m.host,
        capturedAt: m.capturedAt,
        expiresAt: m.expiresAt,
      })),
    })),
  };
}

export function exportFileName(now: Date): string {
  const stamp = now.toISOString().replace(/[-:]/g, '').replace('T', '-').slice(0, 15);
  return `shelfy-spike3-capture-${stamp}.json`;
}

function isExportItem(value: unknown): value is ExportItem {
  return (
    isRecord(value) &&
    typeof value.key === 'string' &&
    isPlatform(value.platform) &&
    typeof value.nativeId === 'string' &&
    Array.isArray(value.rawIds) &&
    value.rawIds.every((id) => typeof id === 'string') &&
    Array.isArray(value.listings) &&
    value.listings.every(
      (l) =>
        isRecord(l) &&
        typeof l.key === 'string' &&
        Array.isArray(l.sources) &&
        l.sources.every((s) => (SOURCES as readonly unknown[]).includes(s)),
    )
  );
}

/** Validates the parts of an export that compare.ts relies on; throws with a reason otherwise. */
export function parseExportFile(json: unknown): ExportFile {
  if (!isRecord(json) || json.format !== EXPORT_FORMAT)
    throw new Error(`not a SPIKE-3 extension export (expected format "${EXPORT_FORMAT}")`);
  if (json.version !== EXPORT_VERSION)
    throw new Error(
      `unsupported export version ${String(json.version)} (expected ${EXPORT_VERSION})`,
    );
  if (!Array.isArray(json.items) || !Array.isArray(json.listings) || !isRecord(json.platforms))
    throw new Error('export is missing items, listings or platforms');
  json.items.forEach((item, index) => {
    if (!isExportItem(item)) throw new Error(`export item #${index} is malformed`);
  });
  for (const platform of PLATFORMS)
    if (!isRecord(json.platforms[platform]))
      throw new Error(`export has no counters for platform ${platform}`);
  return json as unknown as ExportFile;
}
