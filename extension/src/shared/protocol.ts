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
export const CAPTURE_SOURCES = ['passive', 'replay', 'ssr', 'dom', 'refresh'] as const;
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
  selectCommand: 'shelfy.select.command',
  selectLookup: 'shelfy.select.lookup',
  selectOpen: 'shelfy.select.open',
  /** bridge → worker: one relayed hook message. */
  capture: 'shelfy/capture',
  /** bridge → worker: census counts (debug builds). */
  census: 'shelfy/census',
  /** panel → bridge (chrome.tabs.sendMessage): only a live bridge answers. */
  bridgePing: 'shelfy/bridge.ping',
  taskPrepare: 'shelfy/tasks.prepare',
  taskCollect: 'shelfy/tasks.collect',
  tasksGet: 'shelfy/tasks.get',
  tasksPoll: 'shelfy/tasks.poll',
  /** panel → worker. */
  stateGet: 'shelfy/state.get',
  settingsSet: 'shelfy/settings.set',
  connectionCheck: 'shelfy/connection.check',
  queueFlush: 'shelfy/queue.flush',
  pairingForget: 'shelfy/pairing.forget',
  /** worker → extension pages: something the panel shows changed; it pulls the state. */
  stateChanged: 'shelfy/state.changed',
  // P2-13, the sync controller (content/sync/, sw/sync/). Parsers in the "Sync controller"
  // block at the end of this file.
  /** panel → worker: sync the listing of a tab now (trigger `manual`). */
  plannerGet: 'shelfy/planner.get',
  plannerStartAll: 'shelfy/planner.start-all',
  plannerStop: 'shelfy/planner.stop',
  plannerSchedule: 'shelfy/planner.schedule',
  syncStart: 'shelfy/sync.start',
  /** panel → worker: stop the sync running in a tab. */
  syncStop: 'shelfy/sync.stop',
  /** worker → bridge (chrome.tabs.sendMessage): start the controller with this plan. */
  syncRun: 'shelfy/sync.run',
  /** worker → bridge: stop the controller (the user, or the worker saw the tab leave). */
  syncAbort: 'shelfy/sync.abort',
  /** controller → worker: run a MAIN-world helper in the controller's document. */
  syncMain: 'shelfy/sync.main',
  /** controller → worker: the run's consecutive-known count, after its items were ingested. */
  syncKnown: 'shelfy/sync.known',
  /** controller → worker: phase and counters (at most once a second). */
  syncProgress: 'shelfy/sync.progress',
  /** controller → worker: the walk ended, and why. */
  syncEnd: 'shelfy/sync.end',
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
  /** The page's heading (`main h1`, else `h1`), trimmed: names a new folder (P2-13). */
  heading?: string | null;
  /** The sync the controller of this document runs, if any (P2-13). */
  syncing?: string | null;
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

// ── Sync controller (P2-13) ─────────────────────────────────────────────────
//
//   panel ── MSG.syncStart/syncStop ──► worker (sw/sync/) ── MSG.syncRun/syncAbort ──► bridge
//   bridge: content/sync/controller.ts ── MSG.syncMain/syncKnown/syncProgress/syncEnd ──► worker
//   worker ── chrome.scripting (MAIN world) ──► main/replay.ts ── SHELFY_REPLAY_PAGE ──► bridge
//   bridge ── SHELFY_REPLAY_GATE ──► main/replay.ts (continue, stop, or jump to a cursor)
//
// The replay messages cross the page's window like the hook's: the page can read and forge
// them. The worst a page can do is stop its own replay or report a wrong cursor.

/** MAIN → ISOLATED: the IG replay read a page (main/replay.ts inlines the value). */
export const REPLAY_PAGE_MESSAGE = 'SHELFY_REPLAY_PAGE';
/** ISOLATED → MAIN: the controller's answer at a page boundary of a gated replay. */
export const REPLAY_GATE_MESSAGE = 'SHELFY_REPLAY_GATE';
/** IG `next_max_id` cursors, with room (C4 `resumeCursor`). */
export const MAX_CURSOR_LEN = 4096;
const MAX_RUN_ID_LEN = 64;

/** The modes a sync walks with; each has its config kill switch (C3). */
export const SYNC_MODES = ['replay', 'scroll'] as const;
export type SyncMode = (typeof SYNC_MODES)[number];

export const SYNC_PHASES = ['starting', 'replay', 'ssr', 'scroll', 'ending'] as const;
export type SyncPhase = (typeof SYNC_PHASES)[number];

/**
 * Why a walk ended: the C4 stop reasons, plus `stalled` (the scroll reached the bottom, or gave
 * up, without an end-of-feed signal), which closes the run `done` without a stop reason, so the
 * server never takes it for a full walk.
 */
export const SYNC_END_REASONS = [
  'end_of_feed',
  'known_run',
  'page_cap',
  'time_cap',
  'user',
  'login_required',
  'error',
  'stalled',
] as const;
export type SyncEndReason = (typeof SYNC_END_REASONS)[number];

export type GateAction = 'continue' | 'stop' | 'jump';

export interface ReplayPageMessage {
  /** The replay's id (one per replay, chosen by the controller). */
  id: string;
  /** Pages read so far. */
  page: number;
  /** The cursor of the next page (`next_max_id`), null at the end of the feed. */
  cursor: string | null;
  /** The replay waits for a gate answer before it reads the next page. */
  wait: boolean;
}

export function parseReplayPageMessage(data: unknown): ReplayPageMessage | null {
  if (!isRecord(data) || data.type !== REPLAY_PAGE_MESSAGE) return null;
  const id = boundedString(data.id, MAX_SCOPE_ID_LEN);
  if (!id || typeof data.page !== 'number' || !Number.isSafeInteger(data.page) || data.page < 0)
    return null;
  const cursor = data.cursor === null ? null : boundedString(data.cursor, MAX_CURSOR_LEN);
  if (data.cursor !== null && cursor === null) return null;
  return { id, page: data.page, cursor, wait: data.wait === true };
}

const isOneOf = <T extends string>(list: readonly T[], value: unknown): value is T =>
  typeof value === 'string' && (list as readonly string[]).includes(value);

const count = (value: unknown, max = 1_000_000_000): number | null =>
  typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 && value <= max
    ? value
    : null;

/** What the worker hands the controller (MSG.syncRun): everything it needs, nothing secret. */
export interface SyncPlan {
  /** The local run id (sw/queue `Run.id`). */
  runId: string;
  platform: Platform;
  /** The listing the run walks (shared/listing.ts `listingKey`): leaving it ends the run. */
  listingKey: string;
  /** Stop at the first page boundary where `stopAfterKnown` consecutive items were known. */
  incremental: boolean;
  stopAfterKnown: number;
  /** Where a capped walk of this source stopped: walk the head, then continue from here. */
  resumeCursor: string | null;
  /** The config kill switches (C3): a killed mode is skipped and reported. */
  replay: boolean;
  scroll: boolean;
  scrollSettleMs: number;
  maxSteps: number;
  maxRunMs: number;
}

export interface SyncRunMessage extends SyncPlan {
  kind: typeof MSG.syncRun;
}

export function parseSyncRunMessage(value: unknown): SyncRunMessage | null {
  if (!isRecord(value) || value.kind !== MSG.syncRun) return null;
  const runId = boundedString(value.runId, MAX_RUN_ID_LEN);
  const listingKey = boundedString(value.listingKey, 512);
  const stopAfterKnown = count(value.stopAfterKnown, 10_000);
  const scrollSettleMs = count(value.scrollSettleMs, 60_000);
  const maxSteps = count(value.maxSteps, 16_000);
  const maxRunMs = count(value.maxRunMs, 1_800_000);
  const resumeCursor =
    value.resumeCursor === null ? null : boundedString(value.resumeCursor, MAX_CURSOR_LEN);
  if (!runId || !listingKey || !isPlatform(value.platform)) return null;
  if (stopAfterKnown === null || scrollSettleMs === null || maxSteps === null || maxRunMs === null)
    return null;
  if (value.resumeCursor !== null && resumeCursor === null) return null;
  return {
    kind: MSG.syncRun,
    runId,
    platform: value.platform,
    listingKey,
    incremental: value.incremental === true,
    stopAfterKnown: Math.max(1, stopAfterKnown),
    resumeCursor,
    replay: value.replay === true,
    scroll: value.scroll === true,
    scrollSettleMs,
    maxSteps,
    maxRunMs,
  };
}

export const SYNC_MAIN_OPS = ['replay', 'pinterest_ssr'] as const;
export type SyncMainOp = (typeof SYNC_MAIN_OPS)[number];

export interface SyncMainRequest {
  kind: typeof MSG.syncMain;
  runId: string;
  op: SyncMainOp;
  /** The replay's id, for its page and gate messages (op `replay`). */
  replayId: string | null;
}

export function parseSyncMainRequest(value: unknown): SyncMainRequest | null {
  if (!isRecord(value) || value.kind !== MSG.syncMain) return null;
  const runId = boundedString(value.runId, MAX_RUN_ID_LEN);
  if (!runId || !isOneOf(SYNC_MAIN_OPS, value.op)) return null;
  const replayId = value.op === 'replay' ? boundedString(value.replayId, MAX_SCOPE_ID_LEN) : null;
  if (value.op === 'replay' && !replayId) return null;
  return { kind: MSG.syncMain, runId, op: value.op, replayId };
}

export interface SyncKnownRequest {
  kind: typeof MSG.syncKnown;
  runId: string;
  /** Start counting again from 0 (the replay jumped to the resume cursor). */
  reset: boolean;
}

export function parseSyncKnownRequest(value: unknown): SyncKnownRequest | null {
  if (!isRecord(value) || value.kind !== MSG.syncKnown) return null;
  const runId = boundedString(value.runId, MAX_RUN_ID_LEN);
  return runId ? { kind: MSG.syncKnown, runId, reset: value.reset === true } : null;
}

/**
 * The run's trailing run of known items, in item order. `settled`: every item the controller
 * relayed so far was ingested; false when the API is unreachable, and then the controller does
 * not stop on a count it cannot trust.
 */
export type SyncKnownAnswer = { ok: true; streak: number; settled: boolean } | { ok: false };

export interface SyncProgressMessage {
  kind: typeof MSG.syncProgress;
  runId: string;
  phase: SyncPhase;
  steps: number;
  replayPages: number;
}

export function parseSyncProgressMessage(value: unknown): SyncProgressMessage | null {
  if (!isRecord(value) || value.kind !== MSG.syncProgress) return null;
  const runId = boundedString(value.runId, MAX_RUN_ID_LEN);
  const steps = count(value.steps);
  const replayPages = count(value.replayPages);
  if (!runId || !isOneOf(SYNC_PHASES, value.phase) || steps === null || replayPages === null)
    return null;
  return { kind: MSG.syncProgress, runId, phase: value.phase, steps, replayPages };
}

export interface SyncEndMessage {
  kind: typeof MSG.syncEnd;
  runId: string;
  reason: SyncEndReason;
  /** The replay's cursor when the page cap stopped it (P2-G2). */
  resumeCursor: string | null;
  errorCode: string | null;
  /** Modes the config killed, skipped. */
  skipped: SyncMode[];
  steps: number;
  replayPages: number;
}

export function parseSyncEndMessage(value: unknown): SyncEndMessage | null {
  if (!isRecord(value) || value.kind !== MSG.syncEnd) return null;
  const runId = boundedString(value.runId, MAX_RUN_ID_LEN);
  if (!runId || !isOneOf(SYNC_END_REASONS, value.reason)) return null;
  const resumeCursor =
    value.resumeCursor === null || value.resumeCursor === undefined
      ? null
      : boundedString(value.resumeCursor, MAX_CURSOR_LEN);
  const errorCode =
    value.errorCode === null || value.errorCode === undefined
      ? null
      : boundedString(value.errorCode, 64);
  const skipped = Array.isArray(value.skipped)
    ? SYNC_MODES.filter((mode) => (value.skipped as unknown[]).includes(mode))
    : [];
  return {
    kind: MSG.syncEnd,
    runId,
    reason: value.reason,
    resumeCursor,
    errorCode,
    skipped,
    steps: count(value.steps) ?? 0,
    replayPages: count(value.replayPages) ?? 0,
  };
}

/** Where an explicit sync files its posts (IMP-10's chooser): the listing's collection, or none. */
export const SYNC_COLLECTION_CHOICES = ['auto', 'none'] as const;
export type SyncCollectionChoice = (typeof SYNC_COLLECTION_CHOICES)[number];

export interface SyncStartRequest {
  kind: typeof MSG.syncStart;
  tabId: number;
  collection: SyncCollectionChoice;
  /** The name of a collection created for the folder or board (the page heading by default). */
  name: string | null;
}

const MAX_COLLECTION_NAME_LEN = 120;

export function parseSyncStartRequest(value: unknown): SyncStartRequest | null {
  if (!isRecord(value) || value.kind !== MSG.syncStart) return null;
  if (typeof value.tabId !== 'number' || !Number.isSafeInteger(value.tabId)) return null;
  if (!isOneOf(SYNC_COLLECTION_CHOICES, value.collection)) return null;
  const name =
    typeof value.name === 'string' && value.name.trim()
      ? value.name.trim().slice(0, MAX_COLLECTION_NAME_LEN)
      : null;
  return { kind: MSG.syncStart, tabId: value.tabId, collection: value.collection, name };
}

export interface SyncStopRequest {
  kind: typeof MSG.syncStop;
  tabId: number;
}

export function parseSyncStopRequest(value: unknown): SyncStopRequest | null {
  if (!isRecord(value) || value.kind !== MSG.syncStop) return null;
  return typeof value.tabId === 'number' && Number.isSafeInteger(value.tabId)
    ? { kind: MSG.syncStop, tabId: value.tabId }
    : null;
}

/**
 * Answer to MSG.syncStart (and the shape P2-15's `shelfy.sync.start` maps to C9): the codes are
 * `not_paired`, `outdated`, `busy`, `disabled`, `not_a_listing`, `not_own_board`, `reload_tab`,
 * or an API failure code (sw/errors.ts failureCode).
 */
export type SyncStartAnswer = { ok: true; runId: string } | { ok: false; code: string };
