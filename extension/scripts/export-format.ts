// The SPIKE-3 capture export (`shelfy-spike3-capture`, version 1): what the spike build's side
// panel exported with "Export JSON" (T5, on `web/t5-extension-spike`), read by compare.ts and by
// SPIKE-2's CDN fetch test. The product extension (P2-06) has no local item store, so it writes
// no such file; P2-19 adds the run report next to this format. All timestamps are unix ms.
// The spike export never held captions or author fields.

import type { ListingKind } from '../src/shared/listing';
import type { MediaUrlKind } from '../src/shared/media';
import {
  CAPTURE_SOURCES,
  PLATFORMS,
  isPlatform,
  isRecord,
  type CaptureSource,
  type Platform,
} from '../src/shared/protocol';

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

export type ExportSource = CaptureSource | 'accepted';
export interface ExportItemListing {
  key: string;
  sources: ExportSource[];
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

export interface ExportCensusEntry {
  requests: number;
  /** Requests made while the page was an in-scope listing. */
  inScope: number;
}

export interface ExportPlatformStats {
  uniqueItems: number;
  batches: number;
  itemsReceived: number;
  discardedBatches: number;
  discardedItems: number;
  rejectedItems: number;
  lastCaptureAt: number | null;
  census: Record<string, ExportCensusEntry>;
}

export interface ExportDiagnostics {
  /** IG items whose public shortcode does not decode to the pk of their id (§2.8 check). */
  igShortcodeMismatch: number;
  /** Batches refused by the spike's service worker, by reason. */
  refusedBatches: Record<string, number>;
}

export interface ExportFile {
  format: typeof EXPORT_FORMAT;
  version: typeof EXPORT_VERSION;
  exportedAt: string;
  extension: { version: string; userAgent: string };
  storeCreatedAt: number;
  platforms: Record<Platform, ExportPlatformStats>;
  diagnostics: ExportDiagnostics;
  listings: ExportListing[];
  items: ExportItem[];
}

export function emptyPlatformStats(): ExportPlatformStats {
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

export function emptyDiagnostics(): ExportDiagnostics {
  return { igShortcodeMismatch: 0, refusedBatches: {} };
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
        l.sources.every((s) => (CAPTURE_SOURCES as readonly unknown[]).includes(s)),
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
  const platforms = json.platforms;
  for (const platform of PLATFORMS)
    if (!isRecord(platforms[platform]))
      throw new Error(`export has no counters for platform ${platform}`);
  return json as unknown as ExportFile;
}
