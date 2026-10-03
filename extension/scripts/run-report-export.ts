// Adapter for the existing canonical identity/listing parity calculation.
import { parseRunReport } from '../src/shared/run-report';
import { parseListingKey } from '../src/shared/listing';
import { KEY_PREFIX } from '../src/shared/identity';
import {
  EXPORT_FORMAT,
  EXPORT_VERSION,
  emptyDiagnostics,
  emptyPlatformStats,
  type ExportFile,
  type ExportItem,
  type ExportListing,
} from './export-format';
export function runReportExport(value: unknown): ExportFile {
  const report = parseRunReport(value);
  const items = new Map<string, ExportItem>();
  const listings = new Map<string, ExportListing>();
  for (const run of report.runs) {
    const parsed = parseListingKey(run.listingKey)!;
    const source = 'accepted' as const;
    if (!listings.has(run.listingKey))
      listings.set(run.listingKey, {
        key: run.listingKey,
        platform: run.platform,
        kind: parsed.kind,
        externalId: parsed.externalId,
        name: null,
        account: null,
        firstSeenAt: run.firstAcceptedAt,
        lastSeenAt: run.lastAcceptedAt,
        batches: 0,
        itemsReceived: 0,
        uniqueItems: 0,
        bySource: { passive: 0, replay: 0, ssr: 0, dom: 0, refresh: 0 },
        endOfFeedSeen: false,
        lastHasNextPage: null,
      });
    for (const key of run.keys) {
      if (!key.startsWith(KEY_PREFIX[run.platform]))
        throw new Error('accepted key platform mismatch');
      const nativeId = key.slice(KEY_PREFIX[run.platform].length);
      const item = items.get(key) ?? {
        key,
        platform: run.platform,
        nativeId,
        rawIds: [nativeId],
        shortcode: '',
        postUrl: '',
        mediaType: '',
        mediaCount: 0,
        postedAt: '',
        firstCapturedAt: run.firstAcceptedAt,
        lastCapturedAt: run.lastAcceptedAt,
        captureCount: 0,
        listings: [],
        media: [],
      };
      const membership = item.listings.find((listing) => listing.key === run.listingKey);
      if (membership) {
        if (!membership.sources.includes(source)) membership.sources.push(source);
      } else
        item.listings.push({
          key: run.listingKey,
          sources: [source],
          firstCapturedAt: run.firstAcceptedAt,
          lastCapturedAt: run.lastAcceptedAt,
        });
      item.captureCount++;
      items.set(key, item);
    }
  }
  return {
    format: EXPORT_FORMAT,
    version: EXPORT_VERSION,
    exportedAt: report.exportedAt,
    extension: { version: 'run-report-v1', userAgent: '' },
    storeCreatedAt: 0,
    platforms: {
      instagram: emptyPlatformStats(),
      twitter: emptyPlatformStats(),
      pinterest: emptyPlatformStats(),
    },
    diagnostics: emptyDiagnostics(),
    listings: [...listings.values()],
    items: [...items.values()],
  };
}
