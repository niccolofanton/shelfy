// Messages between the parts of the extension. Shared registry S7 (P2 lane rule 2): later
// lanes add their message kinds here, next to these.
//
//   page MAIN world                                      ISOLATED world          service worker
//   electron/webview-injected.ts  SOCIAL_SAVED_INTERCEPT ─┐
//   main/passive.ts (SSR, X DOM)  SHELFY_SCOPE           ─┼─► content/bridge.ts ──► sw/router.ts
//   main/census.ts (debug builds) SHELFY_CENSUS          ─┘   (relay.ts)   MSG.capture   │
//                                                                                        ├─► capture → queue → API
//   side panel ───────────── MSG.* (chrome.runtime.sendMessage) ────────────────────────┤
//   Shelfy SPA ── EXTERNAL.* (chrome.runtime.sendMessage(EXTENSION_ID, …)) ─────────────┘
//
// Window messages are visible to the page and forgeable: a page can post the same shapes. Every
// parser below checks the structure and bounds sizes, and the service worker re-sanitizes items
// with the desktop's sanitizeInterceptedBatch before anything is queued (plan §2.16).

import { MAX_BATCH_ITEMS } from '../../../src/lib/browserSanitize';

export const PLATFORMS = ['instagram', 'twitter', 'pinterest'] as const;
export type Platform = (typeof PLATFORMS)[number];

/**
 * How a batch reached the hook, as the page declared it (informational, local counters only):
 * - `passive`: the page's own fetch/XHR traffic (the hook's main path);
 * - `replay`: an Instagram REST replay (main/replay.ts, productized by P2-13);
 * - `ssr`: Pinterest's server-rendered first page, read from inline JSON;
 * - `dom`: X bookmark cards read from the rendered DOM (the desktop's fallback scan).
 */
export const CAPTURE_SOURCES = ['passive', 'replay', 'ssr', 'dom'] as const;
export type CaptureSource = (typeof CAPTURE_SOURCES)[number];

/** `source` of an ingest batch (contract C5). */
export const WIRE_SOURCES = ['passive', 'replay', 'scroll', 'selection', 'refresh'] as const;
export type WireSource = (typeof WIRE_SOURCES)[number];

/** `trigger` of a sync run (contract C4). */
export const TRIGGERS = ['manual', 'web', 'scheduled', 'passive', 'selection', 'refresh'] as const;
export type Trigger = (typeof TRIGGERS)[number];

export type JsonRecord = Record<string, unknown>;

export function isRecord(value: unknown): value is JsonRecord {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

export function isPlatform(value: unknown): value is Platform {
  return typeof value === 'string' && (PLATFORMS as readonly string[]).includes(value);
}

export function isCaptureSource(value: unknown): value is CaptureSource {
  return typeof value === 'string' && (CAPTURE_SOURCES as readonly string[]).includes(value);
}

export function boundedString(value: unknown, max: number): string | null {
  return typeof value === 'string' && value.length > 0 && value.length <= max ? value : null;
}

function finiteTime(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) && value > 0 ? value : null;
}

// ── MAIN → ISOLATED (window messages) ───────────────────────────────────────

/** Posted by electron/webview-injected.ts (its postMessage fallback relay). */
export const INTERCEPT_MESSAGE = 'SOCIAL_SAVED_INTERCEPT';
/** Opens or closes a non-passive capture scope (replay, SSR read, DOM scan). */
export const SCOPE_MESSAGE = 'SHELFY_SCOPE';
/** Request counts from the MAIN-world census (debug builds only, main/census.ts). */
export const CENSUS_MESSAGE = 'SHELFY_CENSUS';

/** Items per relayed message: the desktop sanitizer's batch cap. */
export const MAX_RELAY_ITEMS = MAX_BATCH_ITEMS;
const MAX_PAGE_URL_LEN = 4096;
const MAX_SCOPE_ID_LEN = 128;
const MAX_DETAIL_KEYS = 8;
const MAX_DETAIL_STRING_LEN = 200;
const MAX_CENSUS_KEYS = 64;
const MAX_CENSUS_KEY_LEN = 160;
const MAX_CENSUS_COUNT = 1_000_000;

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
    items: data.items.slice(0, MAX_RELAY_ITEMS),
    hasNextPage: typeof data.hasNextPage === 'boolean' ? data.hasNextPage : null,
  };
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
  if (!isCaptureSource(data.source) || data.source === 'passive') return null;
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

// ── Runtime messages (chrome.runtime.sendMessage / chrome.tabs.sendMessage) ──

/** Message kinds inside the extension. The router checks who may send each one. */
export const MSG = {
  /** bridge → worker: one relayed hook message. */
  capture: 'shelfy/capture',
  /** bridge → worker: census counts (debug builds). */
  census: 'shelfy/census',
  /** panel → bridge (chrome.tabs.sendMessage): only a live bridge answers. */
  bridgePing: 'shelfy/bridge.ping',
  /** panel → worker. */
  stateGet: 'shelfy/state.get',
  settingsSet: 'shelfy/settings.set',
  connectionCheck: 'shelfy/connection.check',
  queueFlush: 'shelfy/queue.flush',
  pairingForget: 'shelfy/pairing.forget',
  /** worker → extension pages: something the panel shows changed; it pulls the state. */
  stateChanged: 'shelfy/state.changed',
} as const;

const MAX_DOC_ID_LEN = 64;
const MAX_VIEWER_LEN = 64;

/** bridge → worker: a hook message with the context only the content script knows. */
export interface CaptureMessage {
  kind: typeof MSG.capture;
  platform: Platform;
  /** Raw hook items: records only, at most MAX_RELAY_ITEMS. The worker sanitizes them. */
  items: JsonRecord[];
  hasNextPage: boolean | null;
  /** location.href of the document that relayed the batch (listing context). */
  pageUrl: string;
  /** Random id of the relaying document; a reload or another tab gets a new one. */
  docId: string;
  /** Per-document sequence number: the worker drops a delivery it already queued. */
  seq: number;
  capture: CaptureSource;
  /** Pinterest only: the signed-in user's username, read from the page; null when unknown. */
  viewer: string | null;
  sentAt: number;
}

export interface CaptureContext {
  pageUrl: string;
  docId: string;
  seq: number;
  capture: CaptureSource;
  viewer: string | null;
  sentAt: number;
}

export function toCaptureMessage(
  message: InterceptMessage,
  context: CaptureContext,
): CaptureMessage {
  return {
    kind: MSG.capture,
    platform: message.platform,
    items: message.items.filter(isRecord),
    hasNextPage: message.hasNextPage,
    ...context,
  };
}

export function parseCaptureMessage(value: unknown): CaptureMessage | null {
  if (!isRecord(value) || value.kind !== MSG.capture) return null;
  const pageUrl = boundedString(value.pageUrl, MAX_PAGE_URL_LEN);
  const docId = boundedString(value.docId, MAX_DOC_ID_LEN);
  const sentAt = finiteTime(value.sentAt);
  if (!isPlatform(value.platform) || !Array.isArray(value.items) || !pageUrl || !docId || !sentAt)
    return null;
  if (!isCaptureSource(value.capture)) return null;
  if (typeof value.seq !== 'number' || !Number.isSafeInteger(value.seq) || value.seq < 0)
    return null;
  const viewer = value.viewer === null ? null : boundedString(value.viewer, MAX_VIEWER_LEN);
  if (value.viewer !== null && viewer === null) return null;
  return {
    kind: MSG.capture,
    platform: value.platform,
    items: value.items.slice(0, MAX_RELAY_ITEMS).filter(isRecord),
    hasNextPage: typeof value.hasNextPage === 'boolean' ? value.hasNextPage : null,
    pageUrl,
    docId,
    seq: value.seq,
    capture: value.capture,
    viewer,
    sentAt,
  };
}

export interface CensusRuntimeMessage {
  kind: typeof MSG.census;
  counts: CensusCounts;
  pageUrl: string;
  sentAt: number;
}

export function parseCensusRuntimeMessage(value: unknown): CensusRuntimeMessage | null {
  if (!isRecord(value) || value.kind !== MSG.census) return null;
  const counts = parseCensusCounts(value.counts);
  const pageUrl = boundedString(value.pageUrl, MAX_PAGE_URL_LEN);
  const sentAt = finiteTime(value.sentAt);
  if (!counts || !pageUrl || !sentAt) return null;
  return { kind: MSG.census, counts, pageUrl, sentAt };
}

/** Answer of a live bridge to MSG.bridgePing. */
export interface BridgePong {
  ok: true;
  docId: string;
  /** The signed-in Pinterest user the page shows (Pinterest pages only). */
  viewer: string | null;
}

// ── Panel → worker ──────────────────────────────────────────────────────────

/** Access service-token headers (E5, P2-22). Both are secrets: never logged or shown back. */
export interface AccessHeaders {
  clientId: string;
  clientSecret: string;
}

export interface SettingsPatch {
  passive?: Partial<Record<Platform, boolean>>;
  /** Passive captures of IG folders and Pinterest boards go into their collection (§2.16). */
  passiveFolders?: boolean;
  /** null clears the headers. */
  access?: AccessHeaders | null;
}

const ACCESS_VALUE = /^[\x21-\x7e]{1,512}$/;

/** Validates a settings patch from the panel; unknown fields are ignored. */
export function parseSettingsPatch(value: unknown): SettingsPatch | null {
  if (!isRecord(value)) return null;
  const patch: SettingsPatch = {};
  if (value.passive !== undefined) {
    if (!isRecord(value.passive)) return null;
    const passive: Partial<Record<Platform, boolean>> = {};
    for (const [platform, enabled] of Object.entries(value.passive)) {
      if (!isPlatform(platform) || typeof enabled !== 'boolean') return null;
      passive[platform] = enabled;
    }
    patch.passive = passive;
  }
  if (value.passiveFolders !== undefined) {
    if (typeof value.passiveFolders !== 'boolean') return null;
    patch.passiveFolders = value.passiveFolders;
  }
  if (value.access !== undefined) {
    if (value.access === null) patch.access = null;
    else if (
      isRecord(value.access) &&
      typeof value.access.clientId === 'string' &&
      typeof value.access.clientSecret === 'string' &&
      ACCESS_VALUE.test(value.access.clientId) &&
      ACCESS_VALUE.test(value.access.clientSecret)
    )
      patch.access = { clientId: value.access.clientId, clientSecret: value.access.clientSecret };
    else return null;
  }
  return patch;
}

// ── Shelfy SPA → worker (externally_connectable, contract C9) ────────────────

export const EXTERNAL = {
  ping: 'shelfy.ping',
  pair: 'shelfy.pair',
  syncStart: 'shelfy.sync.start',
  syncStop: 'shelfy.sync.stop',
  tasksPoll: 'shelfy.tasks.poll',
} as const;
export type ExternalType = (typeof EXTERNAL)[keyof typeof EXTERNAL];

/** A pairing code (C2: 43 base64url characters); the bounds leave the server room to change it. */
const PAIRING_CODE = /^[A-Za-z0-9_-]{16,128}$/;

export type SyncTarget = { platform: Platform } | { platform: Platform; collectionId: number };

export type ExternalRequest =
  | { type: typeof EXTERNAL.ping }
  | { type: typeof EXTERNAL.pair; code: string }
  | { type: typeof EXTERNAL.syncStart; target: SyncTarget }
  | { type: typeof EXTERNAL.syncStop; platform: Platform }
  | { type: typeof EXTERNAL.tasksPoll };

/** Parses a message from the SPA; null when it is malformed or unknown. */
export function parseExternalMessage(value: unknown): ExternalRequest | null {
  if (!isRecord(value)) return null;
  switch (value.type) {
    case EXTERNAL.ping:
      return { type: EXTERNAL.ping };
    case EXTERNAL.pair:
      return typeof value.code === 'string' && PAIRING_CODE.test(value.code)
        ? { type: EXTERNAL.pair, code: value.code }
        : null;
    case EXTERNAL.syncStart: {
      const target = value.target;
      if (!isRecord(target) || !isPlatform(target.platform)) return null;
      if (target.collectionId === undefined)
        return { type: EXTERNAL.syncStart, target: { platform: target.platform } };
      const id = target.collectionId;
      return typeof id === 'number' && Number.isSafeInteger(id) && id > 0
        ? { type: EXTERNAL.syncStart, target: { platform: target.platform, collectionId: id } }
        : null;
    }
    case EXTERNAL.syncStop:
      return isPlatform(value.platform)
        ? { type: EXTERNAL.syncStop, platform: value.platform }
        : null;
    case EXTERNAL.tasksPoll:
      return { type: EXTERNAL.tasksPoll };
    default:
      return null;
  }
}

/** Answer to `shelfy.ping` (C9). */
export interface PingAnswer {
  ok: true;
  version: string;
  paired: boolean;
  outdated: boolean;
  syncing: Record<Platform, boolean>;
}

/** Answer of every other C9 message: `{ok: true}` or a code the SPA maps to its own message. */
export type ExternalAnswer = { ok: true } | { ok: false; code: string };
