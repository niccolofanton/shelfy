import { isPlatform, isRecord, type Platform, type SyncTarget } from '../../shared/protocol';
import type { WireListing } from '../../shared/listing';
import type { CollectionMode } from '../contracts';

export interface ExtensionSource {
  platform: Platform;
  listing: WireListing;
  collectionId: number | null;
}
export interface SyncStep {
  platform: Platform;
  kind: 'ig-all' | 'ig-folder' | 'x-bookmarks' | 'pin-board';
  externalId: string | null;
  name: string | null;
  collection: CollectionMode;
}
export function parseSources(value: unknown): ExtensionSource[] {
  if (!isRecord(value) || !Array.isArray(value.items)) return [];
  return value.items.flatMap((entry): ExtensionSource[] => {
    if (!isRecord(entry) || !isPlatform(entry.platform) || !isRecord(entry.listing)) return [];
    const { kind, externalId, name } = entry.listing;
    const expected =
      entry.platform === 'instagram'
        ? 'ig_collection'
        : entry.platform === 'pinterest'
          ? 'pin_board'
          : 'x_bookmarks';
    if (kind !== expected || typeof externalId !== 'string' || externalId.length > 256) return [];
    if (kind === 'ig_collection' && !/^\d{1,32}$/.test(externalId)) return [];
    if (kind === 'pin_board' && !/^[\w.-]+\/[\w.-]+(?:\/[\w.-]+)?$/.test(externalId)) return [];
    return [
      {
        platform: entry.platform,
        listing: {
          kind: expected,
          externalId,
          name: typeof name === 'string' ? name.slice(0, 120) : null,
        },
        collectionId:
          typeof entry.collectionId === 'number' &&
          Number.isSafeInteger(entry.collectionId) &&
          entry.collectionId > 0
            ? entry.collectionId
            : null,
      },
    ];
  });
}
// A collection target resolves exactly one native listing. Custom collections
// have no source mapping and never turn into guessed URLs.
export function buildSyncSteps(target: SyncTarget, sources: ExtensionSource[]): SyncStep[] {
  const native = sources.filter(
    (source) =>
      source.platform === target.platform &&
      (source.listing.kind === 'ig_collection' || source.listing.kind === 'pin_board'),
  );
  const step = (source: ExtensionSource): SyncStep => ({
    platform: source.platform,
    kind: source.platform === 'instagram' ? 'ig-folder' : 'pin-board',
    externalId: source.listing.externalId,
    name: source.listing.name,
    collection:
      source.collectionId == null
        ? { mode: 'auto' }
        : { mode: 'existing', id: source.collectionId },
  });
  if ('collectionId' in target) {
    const source = native.find((source) => source.collectionId === target.collectionId);
    return source ? [step(source)] : [];
  }
  const deduped = [
    ...new Map(native.map((source) => [source.listing.externalId, source])).values(),
  ];
  switch (target.platform) {
    case 'instagram':
      return [
        {
          platform: 'instagram',
          kind: 'ig-all',
          externalId: null,
          name: null,
          collection: { mode: 'none' },
        },
        ...deduped.map(step),
      ];
    case 'twitter':
      return [
        {
          platform: 'twitter',
          kind: 'x-bookmarks',
          externalId: null,
          name: null,
          collection: { mode: 'none' },
        },
      ];
    case 'pinterest':
      return deduped.map(step);
  }
}
export type StepErrorCode =
  | 'login_required'
  | 'navigation'
  | 'bridge'
  | 'folder_missing'
  | 'step_timeout'
  | 'start'
  | 'cancelled';
export class StepError extends Error {
  constructor(readonly code: StepErrorCode) {
    super(code);
  }
}
export function stepErrorAction(code: StepErrorCode): 'abort' | 'stop' | 'skip' {
  return code === 'login_required' ? 'abort' : code === 'cancelled' ? 'stop' : 'skip';
}
export interface ScheduleSettings {
  enabled: boolean;
  hour: number;
  minute: number;
  unattended: boolean;
}
export const DEFAULT_SCHEDULE: ScheduleSettings = {
  enabled: false,
  hour: 18,
  minute: 0,
  unattended: false,
};
export function parseSchedule(value: unknown): ScheduleSettings | null {
  if (
    !isRecord(value) ||
    typeof value.enabled !== 'boolean' ||
    typeof value.unattended !== 'boolean' ||
    !Number.isInteger(value.hour) ||
    !Number.isInteger(value.minute)
  )
    return null;
  if (
    (value.hour as number) < 0 ||
    (value.hour as number) > 23 ||
    (value.minute as number) < 0 ||
    (value.minute as number) > 59
  )
    return null;
  return {
    enabled: value.enabled,
    unattended: value.unattended,
    hour: value.hour as number,
    minute: value.minute as number,
  };
}
export function nextReminder(now: number, schedule: ScheduleSettings): number {
  const next = new Date(now);
  next.setHours(schedule.hour, schedule.minute, 0, 0);
  if (next.getTime() <= now) next.setDate(next.getDate() + 1);
  return next.getTime();
}
