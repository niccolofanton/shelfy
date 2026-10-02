// Message protocol between the parts of the SPIKE-3 extension.
//
//   page MAIN world                                ISOLATED world          service worker
//   electron/webview-injected.ts  SOCIAL_SAVED_INTERCEPT ─┐
//   main/passive.ts (SSR, X DOM)  SHELFY_SPIKE_SCOPE     ─┼─► bridge.ts ──────► sw.ts ──► storage
//   main/census.ts                SHELFY_SPIKE_CENSUS    ─┤   (relay.ts:       chrome.runtime
//   replay.ts (chrome.scripting)  SHELFY_SPIKE_SCOPE     ─┘    tags source)    .sendMessage
//
// Window messages are visible to the page and forgeable: a page can post the same shapes.
// Every parser below checks structure and bounds sizes, and the service worker re-sanitizes
// items with the desktop's browserSanitize before anything is stored.

import { MAX_BATCH_ITEMS } from '../../src/lib/browserSanitize';

export const PLATFORMS = ['instagram', 'twitter', 'pinterest'] as const;
export type Platform = (typeof PLATFORMS)[number];

/**
 * How a batch reached the hook:
 * - `passive`: the page's own fetch/XHR traffic (the hook's main path);
 * - `replay`: the owner-triggered Instagram REST replay (replay.ts);
 * - `ssr`: Pinterest's server-rendered first page, read from inline JSON;
 * - `dom`: X bookmark cards read from the rendered DOM (the desktop's fallback scan).
 */
export const SOURCES = ['passive', 'replay', 'ssr', 'dom'] as const;
export type CaptureSource = (typeof SOURCES)[number];

/** Posted by electron/webview-injected.ts (its postMessage fallback relay). */
export const INTERCEPT_MESSAGE = 'SOCIAL_SAVED_INTERCEPT';
/** Opens/closes a non-passive capture scope (replay, SSR read, DOM scan). */
export const SCOPE_MESSAGE = 'SHELFY_SPIKE_SCOPE';
/** Request counts from the MAIN-world census (main/census.ts). */
export const CENSUS_MESSAGE = 'SHELFY_SPIKE_CENSUS';

/** chrome.runtime message kinds. */
export const RUNTIME = {
  batch: 'shelfy-spike/batch',
  marker: 'shelfy-spike/marker',
  census: 'shelfy-spike/census',
  getState: 'shelfy-spike/get-state',
  clear: 'shelfy-spike/clear',
  state: 'shelfy-spike/state',
  /** Side panel → bridge (chrome.tabs.sendMessage): only a live bridge answers. */
  ping: 'shelfy-spike/ping',
} as const;

const MAX_PAGE_URL_LEN = 4096;
const MAX_SCOPE_ID_LEN = 128;
const MAX_DETAIL_KEYS = 8;
const MAX_DETAIL_STRING_LEN = 200;
const MAX_OPEN_SCOPES = 16;
export const MAX_CENSUS_KEYS = 64;
const MAX_CENSUS_KEY_LEN = 160;
const MAX_CENSUS_COUNT = 1_000_000;

export type JsonRecord = Record<string, unknown>;

export function isRecord(value: unknown): value is JsonRecord {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

export function isPlatform(value: unknown): value is Platform {
  return typeof value === 'string' && (PLATFORMS as readonly string[]).includes(value);
}

export function isSource(value: unknown): value is CaptureSource {
  return typeof value === 'string' && (SOURCES as readonly string[]).includes(value);
}

function boundedString(value: unknown, max: number): string | null {
  return typeof value === 'string' && value.length > 0 && value.length <= max ? value : null;
}

// ── MAIN → ISOLATED (window messages) ───────────────────────────────────────

export interface InterceptMessage {
  platform: Platform;
  items: unknown[];
  /** Tri-state pagination signal from the parsers: true = more, false = end, null = unknown. */
  hasNextPage: boolean | null;
}

/** Parses the hook's relay message `{type, items, hasNextPage, platform}` (webview-injected.ts). */
export function parseInterceptMessage(data: unknown): InterceptMessage | null {
  if (!isRecord(data) || data.type !== INTERCEPT_MESSAGE) return null;
  if (!isPlatform(data.platform) || !Array.isArray(data.items)) return null;
  return {
    platform: data.platform,
    items: data.items.slice(0, MAX_BATCH_ITEMS),
    hasNextPage: typeof data.hasNextPage === 'boolean' ? data.hasNextPage : null,
  };
}

/**
 * Fields of the hook's InterceptItem the spike keeps. Captions (`text`) and author fields are
 * dropped at the first hop, so they never reach extension storage or the export.
 */
export const KEPT_ITEM_FIELDS = [
  'id',
  'shortcode',
  'postUrl',
  'mediaType',
  'timestamp',
  'thumbnailUrl',
  'media',
] as const;

export function projectItem(raw: unknown): JsonRecord | null {
  if (!isRecord(raw)) return null;
  const out: JsonRecord = {};
  for (const field of KEPT_ITEM_FIELDS) if (field in raw) out[field] = raw[field];
  return out;
}

export type ScopeDetail = Record<string, string | number | boolean | null>;

export interface ScopeMessage {
  phase: 'start' | 'end';
  source: CaptureSource;
  id: string;
  detail: ScopeDetail;
}

function parseDetail(value: unknown): ScopeDetail {
  const out: ScopeDetail = {};
  if (!isRecord(value)) return out;
  for (const [key, raw] of Object.entries(value).slice(0, MAX_DETAIL_KEYS)) {
    if (key.length > 40) continue;
    if (typeof raw === 'string') out[key] = raw.slice(0, MAX_DETAIL_STRING_LEN);
    else if (typeof raw === 'number' && Number.isFinite(raw)) out[key] = raw;
    else if (typeof raw === 'boolean' || raw === null) out[key] = raw;
  }
  return out;
}

export function parseScopeMessage(data: unknown): ScopeMessage | null {
  if (!isRecord(data) || data.type !== SCOPE_MESSAGE) return null;
  if (data.phase !== 'start' && data.phase !== 'end') return null;
  if (!isSource(data.source) || data.source === 'passive') return null;
  const id = boundedString(data.id, MAX_SCOPE_ID_LEN);
  if (!id) return null;
  return { phase: data.phase, source: data.source, id, detail: parseDetail(data.detail) };
}

/** Census counts keyed `"<platform>|<label>"`; see main/census.ts. */
export type CensusCounts = Record<string, number>;

function parseCensusCounts(value: unknown): CensusCounts | null {
  if (!isRecord(value)) return null;
  const out: CensusCounts = {};
  for (const [key, count] of Object.entries(value).slice(0, MAX_CENSUS_KEYS)) {
    if (key.length > MAX_CENSUS_KEY_LEN || !isPlatform(key.split('|')[0])) continue;
    if (typeof count !== 'number' || !Number.isInteger(count) || count < 1) continue;
    out[key] = Math.min(count, MAX_CENSUS_COUNT);
  }
  return Object.keys(out).length ? out : null;
}

export function parseCensusMessage(data: unknown): CensusCounts | null {
  if (!isRecord(data) || data.type !== CENSUS_MESSAGE) return null;
  return parseCensusCounts(data.counts);
}

/**
 * Open non-passive scopes of one document. Window messages are delivered in posting order, so
 * a batch posted between a scope's start and end belongs to it; batches are tagged with the
 * innermost open scope, or `passive` when none is open.
 */
export class ScopeTracker {
  private readonly open = new Map<string, CaptureSource>();

  apply(message: ScopeMessage): void {
    if (message.phase === 'end') this.open.delete(message.id);
    else if (this.open.size < MAX_OPEN_SCOPES) this.open.set(message.id, message.source);
  }

  current(): CaptureSource {
    let innermost: CaptureSource = 'passive';
    for (const source of this.open.values()) innermost = source;
    return innermost;
  }
}

// ── ISOLATED → service worker (chrome.runtime messages) ─────────────────────

export interface BatchMessage {
  kind: typeof RUNTIME.batch;
  platform: Platform;
  items: JsonRecord[];
  hasNextPage: boolean | null;
  /** location.href of the document that relayed the batch (listing context). */
  pageUrl: string;
  source: CaptureSource;
  sentAt: number;
}

export interface RelayContext {
  pageUrl: string;
  source: CaptureSource;
  sentAt: number;
}

export function toBatchMessage(message: InterceptMessage, context: RelayContext): BatchMessage {
  const items: JsonRecord[] = [];
  for (const raw of message.items) {
    const item = projectItem(raw);
    if (item) items.push(item);
  }
  return {
    kind: RUNTIME.batch,
    platform: message.platform,
    items,
    hasNextPage: message.hasNextPage,
    pageUrl: context.pageUrl,
    source: context.source,
    sentAt: context.sentAt,
  };
}

function parsePageUrl(value: unknown): string | null {
  return boundedString(value, MAX_PAGE_URL_LEN);
}

function parseTime(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) && value > 0 ? value : null;
}

export function parseBatchMessage(value: unknown): BatchMessage | null {
  if (!isRecord(value) || value.kind !== RUNTIME.batch) return null;
  const pageUrl = parsePageUrl(value.pageUrl);
  const sentAt = parseTime(value.sentAt);
  if (!isPlatform(value.platform) || !Array.isArray(value.items) || !pageUrl || !sentAt)
    return null;
  if (!isSource(value.source)) return null;
  return {
    kind: RUNTIME.batch,
    platform: value.platform,
    items: value.items.slice(0, MAX_BATCH_ITEMS).filter(isRecord),
    hasNextPage: typeof value.hasNextPage === 'boolean' ? value.hasNextPage : null,
    pageUrl,
    source: value.source,
    sentAt,
  };
}

export interface MarkerMessage {
  kind: typeof RUNTIME.marker;
  phase: 'start' | 'end';
  source: CaptureSource;
  id: string;
  detail: ScopeDetail;
  pageUrl: string;
  sentAt: number;
}

export function toMarkerMessage(
  scope: ScopeMessage,
  pageUrl: string,
  sentAt: number,
): MarkerMessage {
  return { kind: RUNTIME.marker, ...scope, pageUrl, sentAt };
}

export function parseMarkerMessage(value: unknown): MarkerMessage | null {
  if (!isRecord(value) || value.kind !== RUNTIME.marker) return null;
  const scope = parseScopeMessage({ ...value, type: SCOPE_MESSAGE });
  const pageUrl = parsePageUrl(value.pageUrl);
  const sentAt = parseTime(value.sentAt);
  if (!scope || !pageUrl || !sentAt) return null;
  return { kind: RUNTIME.marker, ...scope, pageUrl, sentAt };
}

export interface CensusRuntimeMessage {
  kind: typeof RUNTIME.census;
  counts: CensusCounts;
  pageUrl: string;
  sentAt: number;
}

export function parseCensusRuntimeMessage(value: unknown): CensusRuntimeMessage | null {
  if (!isRecord(value) || value.kind !== RUNTIME.census) return null;
  const counts = parseCensusCounts(value.counts);
  const pageUrl = parsePageUrl(value.pageUrl);
  const sentAt = parseTime(value.sentAt);
  if (!counts || !pageUrl || !sentAt) return null;
  return { kind: RUNTIME.census, counts, pageUrl, sentAt };
}
