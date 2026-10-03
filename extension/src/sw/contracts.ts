// The server contracts the worker speaks (docs/web-port/phases/P2.md, Contracts C2–C5), with
// parsers that accept only the documented shapes. Paths are under /api/v1; times are unix ms.

import type { WireListing } from '../shared/listing';
import {
  PLATFORMS,
  boundedString,
  isRecord,
  type Platform,
  type Trigger,
  type WireSource,
} from '../shared/protocol';
import type { WireItem } from './prefilter';

export const API = {
  pair: '/api/v1/extension/pair',
  config: '/api/v1/extension/config',
  syncRuns: '/api/v1/sync-runs',
  syncRun: (id: string) => `/api/v1/sync-runs/${encodeURIComponent(id)}`,
  ingestBatches: '/api/v1/ingest/batches',
  health: '/health',
} as const;

// ── C2 · Pairing ────────────────────────────────────────────────────────────

export interface PairRequest {
  code: string;
  installId: string;
  label: string;
  version: string;
}

export interface PairResponse {
  accountId: string;
  token: string;
  tokenId: string;
  scopes: string[];
}

/** `shx_` and the secret (43 base64url characters today; bounded loosely). */
const TOKEN = /^shx_[A-Za-z0-9_-]{16,256}$/;

export function parsePairResponse(value: unknown): PairResponse | null {
  if (!isRecord(value) || typeof value.token !== 'string' || !TOKEN.test(value.token)) return null;
  const tokenId = boundedString(value.tokenId, 128);
  const accountId = boundedString(value.accountId, 128);
  if (!tokenId || !accountId || !Array.isArray(value.scopes)) return null;
  const scopes = value.scopes.filter((s): s is string => typeof s === 'string' && s.length <= 64);
  return { token: value.token, tokenId, accountId, scopes: scopes.slice(0, 16) };
}

// ── C3 · Extension config ───────────────────────────────────────────────────

export interface PlatformConfig {
  passive: boolean;
  replay: boolean;
  scroll: boolean;
  stopAfterKnown: number;
  scrollSettleMs: number;
  /** Instagram only. */
  replayGapMs: number;
  replayMaxPages: number;
}

export interface ExtensionConfig {
  minVersion: string | null;
  platforms: Record<Platform, PlatformConfig>;
  maxSteps: number;
  maxRunMs: number;
  taskPollMinutes: number;
  refreshPerSession: number;
}

/** The C3 defaults (plan §2.16 pacing, P2-G1 thresholds): used until the server answers. */
export const DEFAULT_CONFIG: ExtensionConfig = {
  minVersion: null,
  platforms: {
    instagram: {
      passive: true,
      replay: true,
      scroll: true,
      stopAfterKnown: 10,
      scrollSettleMs: 650,
      replayGapMs: 700,
      replayMaxPages: 100,
    },
    twitter: {
      passive: true,
      replay: false,
      scroll: true,
      stopAfterKnown: 20,
      scrollSettleMs: 750,
      replayGapMs: 700,
      replayMaxPages: 100,
    },
    pinterest: {
      passive: true,
      replay: false,
      scroll: true,
      stopAfterKnown: 25,
      scrollSettleMs: 650,
      replayGapMs: 700,
      replayMaxPages: 100,
    },
  },
  maxSteps: 16_000,
  maxRunMs: 1_800_000,
  taskPollMinutes: 5,
  refreshPerSession: 200,
};

type Bound = { min: number; max: number };

/**
 * Pacing is fixed (P2 lane rule 4): the server may make the extension slower or stop sooner,
 * never faster or longer than the plan's values, so the bounds below start or end at them.
 */
const PLATFORM_BOUNDS: Record<
  'stopAfterKnown' | 'scrollSettleMs' | 'replayGapMs' | 'replayMaxPages',
  (platform: Platform) => Bound
> = {
  stopAfterKnown: () => ({ min: 1, max: 10_000 }),
  scrollSettleMs: (platform) => ({
    min: DEFAULT_CONFIG.platforms[platform].scrollSettleMs,
    max: 60_000,
  }),
  replayGapMs: () => ({ min: 700, max: 60_000 }),
  replayMaxPages: () => ({ min: 1, max: 100 }),
};

const TOP_BOUNDS: Record<'maxSteps' | 'maxRunMs' | 'taskPollMinutes' | 'refreshPerSession', Bound> =
  {
    maxSteps: { min: 1, max: 16_000 },
    maxRunMs: { min: 1, max: 1_800_000 },
    taskPollMinutes: { min: 1, max: 1440 },
    refreshPerSession: { min: 0, max: 200 },
  };

function boundedInt(value: unknown, bound: Bound, fallback: number): number {
  return typeof value === 'number' &&
    Number.isInteger(value) &&
    value >= bound.min &&
    value <= bound.max
    ? value
    : fallback;
}

function parsePlatformConfig(value: unknown, platform: Platform): PlatformConfig {
  const base = DEFAULT_CONFIG.platforms[platform];
  if (!isRecord(value)) return { ...base };
  const flag = (key: 'passive' | 'replay' | 'scroll'): boolean =>
    typeof value[key] === 'boolean' ? (value[key] as boolean) : base[key];
  const number = (key: keyof typeof PLATFORM_BOUNDS): number =>
    boundedInt(value[key], PLATFORM_BOUNDS[key](platform), base[key]);
  return {
    passive: flag('passive'),
    replay: flag('replay'),
    scroll: flag('scroll'),
    stopAfterKnown: number('stopAfterKnown'),
    scrollSettleMs: number('scrollSettleMs'),
    replayGapMs: number('replayGapMs'),
    replayMaxPages: number('replayMaxPages'),
  };
}

/** The config the server sent, completed with the defaults; out-of-bounds values are ignored. */
export function parseConfig(value: unknown): ExtensionConfig {
  if (!isRecord(value)) return structuredClone(DEFAULT_CONFIG);
  const platforms = isRecord(value.platforms) ? value.platforms : {};
  const top = (key: keyof typeof TOP_BOUNDS): number =>
    boundedInt(value[key], TOP_BOUNDS[key], DEFAULT_CONFIG[key]);
  return {
    minVersion: boundedString(value.minVersion, 32),
    platforms: Object.fromEntries(
      PLATFORMS.map((platform) => [platform, parsePlatformConfig(platforms[platform], platform)]),
    ) as Record<Platform, PlatformConfig>,
    maxSteps: top('maxSteps'),
    maxRunMs: top('maxRunMs'),
    taskPollMinutes: top('taskPollMinutes'),
    refreshPerSession: top('refreshPerSession'),
  };
}

// ── C4 · Sync runs ──────────────────────────────────────────────────────────

export type CollectionMode = { mode: 'auto' } | { mode: 'none' } | { mode: 'existing'; id: number };

export interface SyncRunCreate {
  platform: Platform;
  trigger: Trigger;
  listing: WireListing;
  collection: CollectionMode;
}

export interface SyncRunCreated {
  id: string;
  incremental: boolean;
  stopAfterKnown: number | null;
  collectionId: number | null;
  resumeCursor: string | null;
}

export function parseSyncRunCreated(value: unknown): SyncRunCreated | null {
  if (!isRecord(value)) return null;
  const id = boundedString(value.id, 128);
  if (!id) return null;
  return {
    id,
    incremental: value.incremental === true,
    stopAfterKnown:
      typeof value.stopAfterKnown === 'number' && Number.isInteger(value.stopAfterKnown)
        ? value.stopAfterKnown
        : null,
    collectionId:
      typeof value.collectionId === 'number' && Number.isSafeInteger(value.collectionId)
        ? value.collectionId
        : null,
    resumeCursor: boundedString(value.resumeCursor, 4096),
  };
}

export const RUN_STATES = ['running', 'done', 'stopped', 'failed'] as const;
export type RunState = (typeof RUN_STATES)[number];

export const STOP_REASONS = [
  'end_of_feed',
  'known_run',
  'page_cap',
  'time_cap',
  'user',
  'login_required',
  'error',
] as const;
export type StopReason = (typeof STOP_REASONS)[number];

export interface SyncRunPatch {
  state: RunState;
  pages: number;
  scanned: number;
  stopReason: StopReason | null;
  resumeCursor: string | null;
  errorCode: string | null;
}

// ── C5 · Ingest ─────────────────────────────────────────────────────────────

export interface BatchClient {
  ext: string;
  parser: string;
}

export interface IngestBatchBody {
  syncRunId: string;
  platform: Platform;
  source: WireSource;
  hasNextPage: boolean | null;
  client: BatchClient;
  items: WireItem[];
}

export interface IngestItemResult {
  index: number;
  key: string;
  outcome: 'inserted' | 'known';
  changed: boolean;
}

export interface IngestResult {
  inserted: number;
  updated: number;
  known: number;
  results: IngestItemResult[];
  rejected: Array<{ index: number; code: string }>;
}

const count = (value: unknown): number =>
  typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : 0;

/** The ingest answer; missing counters read as 0, malformed entries are skipped. */
export function parseIngestResult(value: unknown): IngestResult {
  const record = isRecord(value) ? value : {};
  const results: IngestItemResult[] = [];
  if (Array.isArray(record.results))
    for (const entry of record.results) {
      if (!isRecord(entry) || typeof entry.key !== 'string') continue;
      if (entry.outcome !== 'inserted' && entry.outcome !== 'known') continue;
      results.push({
        index: count(entry.index),
        key: entry.key,
        outcome: entry.outcome,
        changed: entry.changed === true,
      });
    }
  const rejected: Array<{ index: number; code: string }> = [];
  if (Array.isArray(record.rejected))
    for (const entry of record.rejected)
      if (isRecord(entry) && typeof entry.code === 'string')
        rejected.push({ index: count(entry.index), code: entry.code.slice(0, 64) });
  return {
    inserted: count(record.inserted),
    updated: count(record.updated),
    known: count(record.known),
    results,
    rejected,
  };
}
