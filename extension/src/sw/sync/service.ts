// The worker's side of an explicit sync (P2-13): it opens the run, hands the walk to the syncing
// tab's controller (content/sync/controller.ts), serves the controller's requests, and ends the
// run when the controller reports, or when the tab closes, crashes, reloads or hits a login
// wall. The run's batches and its closing PATCH go through the queue and the uploader like a
// passive run's (sw/capture.ts routes the tab's captures to it).
//
//   start    POST /sync-runs at once (C4: `incremental`, `stopAfterKnown`, `resumeCursor`,
//            `collectionId`), then MSG.syncRun to the tab. Offline, the walk starts anyway, not
//            incremental, and the uploader opens the run later.
//   main     the MAIN-world helpers, injected with chrome.scripting into the controller's own
//            document: the IG replay (gated) and Pinterest's inline first page.
//   known    seals the run's captures, sends them, and answers the trailing run of known items
//            once they are ingested (or `settled: false` when the API is unreachable).
//   end      the controller's report: state, stop reason, cursor, error code, skipped modes.
//
// P2-15 starts and stops runs through `start` and `stopPlatform` (C9's shelfy.sync.*).

import { LOGIN_PATTERNS } from '../../../../src/lib/browserUrls';
import { pinterestSsrRead, igFeedReplay } from '../../main/replay';
import { platformForUrl } from '../../shared/hosts';
import { passiveScope, syncTarget, toWireListing } from '../../shared/listing';
import {
  MSG,
  PLATFORMS,
  type BridgePong,
  type Platform,
  type SyncCollectionChoice,
  type SyncEndMessage,
  type SyncEndReason,
  type SyncKnownAnswer,
  type SyncKnownRequest,
  type SyncMainRequest,
  type SyncProgressMessage,
  type SyncRunMessage,
  type SyncStartAnswer,
  type Trigger,
} from '../../shared/protocol';
import type { ApiClient } from '../api';
import { isControllerRun } from '../capture';
import type { ConfigService } from '../config';
import { API, parseSyncRunCreated, type CollectionMode, type SyncRunCreated } from '../contracts';
import { classifyFailure, failureCode, type ApiFailure, type FailureAction } from '../errors';
import type { Queue } from '../queue/queue';
import { RUN_KEYS_TTL_MS, type Run } from '../queue/types';
import type { SettingsStore, StorageArea } from '../settings';
import { SyncHistory, type SyncView } from './history';

/** How long `known` waits for a run's captures to be ingested. */
export const KNOWN_WAIT_MS = 8_000;
const KNOWN_POLL_MS = 150;
/** The replay gives up when a page boundary gets no answer within this time (the tab is gone). */
export const REPLAY_GATE_TIMEOUT_MS = 30_000;
/** After a navigation in a syncing tab, the bridge is asked whether its document survived. */
export const NAVIGATION_CHECK_MS = 1_500;
/**
 * The controller reports every second (once a minute at worst, in a hidden tab Chrome
 * throttles): a run silent this long lost its controller (a crashed tab, an orphaned script).
 */
export const CONTROLLER_SILENT_MS = 5 * 60_000;

/** The parts of chrome.tabs the service uses (a fake implements it in tests). */
export interface TabsApi {
  get(tabId: number): Promise<{ id?: number; url?: string }>;
  sendMessage<T = unknown>(
    tabId: number,
    message: unknown,
    options?: { frameId?: number; documentId?: string },
  ): Promise<T>;
}

/** chrome.scripting.executeScript, narrowed to what the service injects. */
export type ExecuteScript = (injection: {
  target: { tabId: number; frameIds: number[] } | { tabId: number; documentIds: string[] };
  world: 'MAIN';
  func: (...args: never[]) => unknown;
  args: unknown[];
}) => Promise<unknown>;

export interface SyncDeps {
  queue: Queue;
  store: SettingsStore;
  config: ConfigService;
  api: ApiClient;
  tabs: TabsApi;
  executeScript: ExecuteScript;
  storage: StorageArea;
  now(): number;
  sleep(ms: number): Promise<void>;
  /** Sends what is queued (the uploader); `force` ignores the retry gate. */
  flush(force?: boolean): void;
  /** Stops sending after a failure that concerns every request (the uploader's block). */
  block(failure: ApiFailure, action: FailureAction): Promise<void>;
  changed(): void;
  log(where: string, err: unknown): void;
}

export interface StartRequest {
  expectedPairing?: { token: string; tokenId: string };
  /** P2-15/P2-17: a deliberate full walk ignores server incremental/resume hints. */
  full?: boolean;
  tabId: number;
  trigger?: Extract<Trigger, 'manual' | 'web' | 'scheduled'>;
  /** The panel's chooser, or a mapping P2-15 already knows (`existing`). */
  collection: SyncCollectionChoice | CollectionMode;
  /** Names a collection created for the folder or board; the page heading by default. */
  name?: string | null;
}

/** The sender of a controller message, as the router hands it over. */
export interface ControllerSender {
  frameId?: number;
  documentId?: string;
  tab?: { id?: number };
}

const no = (code: string): SyncStartAnswer => ({ ok: false, code });

export class SyncService {
  readonly history: SyncHistory;
  /** Starts run one at a time, so two of them never both pass the busy check. */
  private starting: Promise<unknown> = Promise.resolve();

  constructor(private readonly deps: SyncDeps) {
    this.history = new SyncHistory(deps.storage);
  }

  // ── Start and stop ────────────────────────────────────────────────────────

  start(request: StartRequest): Promise<SyncStartAnswer> {
    const started = this.starting.then(() => this.startNow(request));
    this.starting = started.catch(() => undefined);
    return started;
  }

  private async startNow(request: StartRequest): Promise<SyncStartAnswer> {
    const { queue, store, config, tabs, now } = this.deps;
    const [pairing, status, current] = await Promise.all([
      store.pairing(),
      store.status(),
      config.current(),
    ]);
    if (!pairing) return no('not_paired');
    if (
      request.expectedPairing &&
      (request.expectedPairing.token !== pairing.token ||
        request.expectedPairing.tokenId !== pairing.tokenId)
    )
      return no('not_paired');
    const same = () => this.samePairing(pairing);
    if (status.outdated) return no('outdated');
    const tab = await tabs.get(request.tabId).catch(() => null);
    if (!(await same())) return no('not_paired');
    const url = tab?.url ?? '';
    const platform = platformForUrl(url);
    const listing = platform ? syncTarget(platform, url) : null;
    const wire = listing ? toWireListing(listing) : null;
    if (!platform || !listing || !wire) return no('not_a_listing');
    const pong = await tabs
      .sendMessage<BridgePong>(request.tabId, { kind: MSG.bridgePing }, { frameId: 0 })
      .catch(() => null);
    if (!(await same())) return no('not_paired');
    if (!pong?.ok) return no('reload_tab');
    if (pong.syncing) return no('busy');
    if (platform === 'pinterest') {
      // The scope of passive capture applies: the signed-in user's own boards (plan §2.16).
      const scope = passiveScope(platform, url, pong.viewer);
      if (!scope.ok) return no(scope.reason);
    }
    const settings = current.platforms[platform];
    const replay = platform === 'instagram' && settings.replay;
    if (!replay && !settings.scroll) return no('disabled');
    const open = (await queue.runs()).filter((run) => run.state === 'open');
    if (!(await same())) return no('not_paired');
    if (open.some((run) => isControllerRun(run) && run.platform === platform)) return no('busy');

    // The tab's passive run ends: from now on its captures belong to the sync.
    await queue.endRuns(
      (run) => run.trigger === 'passive' && run.tabId === request.tabId,
      'user',
      now(),
    );
    if (!(await same())) return no('not_paired');
    const collection = collectionMode(request.collection, wire.externalId);
    const name =
      wire.externalId === null ? null : (request.name ?? pong.heading ?? wire.name ?? null);
    const trigger = request.trigger ?? 'manual';
    const spec = {
      accountTokenId: pairing.tokenId,
      platform,
      trigger,
      listing: { ...wire, name },
      collection,
      tabId: request.tabId,
      docId: pong.docId,
      listingKey: listing.key,
    };
    let run = await queue.openRun(spec, now());
    if (!(await same())) {
      await queue.dropRun(run.id);
      return no('not_paired');
    }

    // C4: open the run on the server first: the walk depends on its answer.
    const opened = await this.openOnServer(spec, pairing);
    if (!(await same())) {
      await queue.dropRun(run.id);
      return no('not_paired');
    }
    if ('refused' in opened) {
      await queue.forgetRun(run.id);
      this.deps.changed();
      return no(opened.refused);
    }
    const created = opened.created;
    const stopAfterKnown = created?.stopAfterKnown ?? settings.stopAfterKnown;
    run =
      (await queue.updateRun(run.id, (r) => {
        r.serverId = created?.id ?? null;
        r.incremental = request.full ? false : (created?.incremental ?? false);
        r.stopAfterKnown = stopAfterKnown;
        r.collectionId = created?.collectionId ?? null;
        r.phase = 'starting';
      })) ?? run;
    if (!(await same())) {
      await queue.dropRun(run.id);
      return no('not_paired');
    }

    const plan: SyncRunMessage = {
      kind: MSG.syncRun,
      runId: run.id,
      platform,
      listingKey: listing.key,
      incremental: run.incremental,
      stopAfterKnown,
      resumeCursor: request.full ? null : (created?.resumeCursor ?? null),
      replay,
      scroll: settings.scroll,
      scrollSettleMs: settings.scrollSettleMs,
      maxSteps: current.maxSteps,
      maxRunMs: current.maxRunMs,
    };
    const answer = await tabs
      .sendMessage<{ ok?: boolean; code?: string }>(request.tabId, plan, { frameId: 0 })
      .catch(() => null);
    if (!(await same())) {
      await tabs
        .sendMessage(request.tabId, { kind: MSG.syncAbort, runId: run.id }, { frameId: 0 })
        .catch(() => null);
      await queue.dropRun(run.id);
      return no('not_paired');
    }
    if (!answer?.ok) {
      await this.endRun(run.id, 'error', { errorCode: 'controller_unreachable' });
      return no(answer?.code === 'busy' ? 'busy' : 'reload_tab');
    }
    this.deps.changed();
    return { ok: true, runId: run.id };
  }

  private async openOnServer(
    spec: {
      platform: Platform;
      trigger: Trigger;
      listing: unknown;
      collection: CollectionMode;
    },
    pairing: { token: string; tokenId: string },
  ): Promise<{ created: SyncRunCreated | null } | { refused: string }> {
    const response = await this.deps.api.post(
      API.syncRuns,
      {
        platform: spec.platform,
        trigger: spec.trigger,
        listing: spec.listing,
        collection: spec.collection,
      },
      { auth: 'token', expectedToken: pairing.token },
    );
    if (!(await this.samePairing(pairing))) return { refused: 'not_paired' };
    if (response.ok) return { created: parseSyncRunCreated(response.data) };
    const action = classifyFailure(response.failure);
    switch (action.action) {
      case 'unpair':
      case 'outdated':
        await this.deps.block(response.failure, action);
        return { refused: failureCode(response.failure) };
      case 'disable_source':
        return { refused: 'disabled' };
      case 'drop':
        return { refused: failureCode(response.failure) };
      default:
        // Unreachable or busy: walk anyway, not incremental; the uploader opens the run later.
        return { created: null };
    }
  }

  /** Stops the sync of a tab: the controller ends it, or the worker does when it is gone. */
  async stop(
    tabId: number,
    expectedRun?: { id: string; accountTokenId?: string },
  ): Promise<{ ok: boolean; code?: string }> {
    const run = await this.openRunOfTab(tabId);
    if (
      !run ||
      (expectedRun &&
        (run.id !== expectedRun.id || run.accountTokenId !== expectedRun.accountTokenId))
    )
      return { ok: false, code: 'not_syncing' };
    const answer = await this.deps.tabs
      .sendMessage<{
        ok?: boolean;
        running?: boolean;
      }>(tabId, { kind: MSG.syncAbort, runId: run.id }, { frameId: 0 })
      .catch(() => null);
    if (!answer?.running) await this.endRun(run.id, 'user');
    return { ok: true };
  }

  /** Stops the sync of a platform (C9 `shelfy.sync.stop`, P2-15). */
  async stopPlatform(platform: Platform, accountTokenId?: string): Promise<{ ok: boolean }> {
    for (const run of await this.openRuns())
      if (
        run.platform === platform &&
        run.tabId !== null &&
        (!accountTokenId || run.accountTokenId === accountTokenId)
      )
        await this.stop(run.tabId, run);
    return { ok: true };
  }

  /** Which platforms sync now (C9 `shelfy.ping`). */
  async syncing(accountTokenId?: string): Promise<Record<Platform, boolean>> {
    const open = await this.openRuns();
    return Object.fromEntries(
      PLATFORMS.map((platform) => [
        platform,
        open.some(
          (run) =>
            run.platform === platform && (!accountTokenId || run.accountTokenId === accountTokenId),
        ),
      ]),
    ) as Record<Platform, boolean>;
  }

  // ── The controller's requests ─────────────────────────────────────────────

  async main(request: SyncMainRequest, sender: ControllerSender): Promise<{ ok: boolean }> {
    const run = await this.controllerRun(request.runId, sender);
    if (!run || run.tabId === null) return { ok: false };
    const target = sender.documentId
      ? { tabId: run.tabId, documentIds: [sender.documentId] }
      : { tabId: run.tabId, frameIds: [0] };
    const settings = (await this.deps.config.current()).platforms[run.platform];
    if (request.op === 'replay') {
      if (run.platform !== 'instagram' || !request.replayId || !settings.replay)
        return { ok: false };
      // Not awaited: the replay runs for minutes, and the controller follows it through its
      // window messages. A failed injection shows there as a replay that never starts.
      this.deps
        .executeScript({
          target,
          world: 'MAIN',
          func: igFeedReplay,
          args: [
            {
              maxPages: settings.replayMaxPages,
              gapMs: settings.replayGapMs,
              runId: request.replayId,
              gate: true,
              gateTimeoutMs: REPLAY_GATE_TIMEOUT_MS,
            },
          ],
        })
        .catch((err: unknown) => this.deps.log('replay', err));
      return { ok: true };
    }
    if (run.platform !== 'pinterest') return { ok: false };
    await this.deps.executeScript({
      target,
      world: 'MAIN',
      func: pinterestSsrRead,
      args: [`ssr-${run.id}`],
    });
    return { ok: true };
  }

  async known(request: SyncKnownRequest, sender: ControllerSender): Promise<SyncKnownAnswer> {
    const { queue, store, now, sleep } = this.deps;
    let run = await this.controllerRun(request.runId, sender);
    if (!run) return { ok: false };
    if (request.reset) {
      await queue.updateRun(run.id, (r) => (r.knownStreak = 0));
      return { ok: true, streak: 0, settled: true };
    }
    this.deps.flush();
    const deadline = now() + KNOWN_WAIT_MS;
    for (;;) {
      if (run.queued <= 0) return { ok: true, streak: run.knownStreak, settled: true };
      const [pairing, status] = await Promise.all([store.pairing(), store.status()]);
      if (!pairing || status.outdated || status.blockedUntil > now() || now() >= deadline)
        return { ok: true, streak: run.knownStreak, settled: false };
      await sleep(KNOWN_POLL_MS);
      run = await queue.getRun(request.runId);
      if (!run) return { ok: false };
    }
  }

  async progress(message: SyncProgressMessage, sender: ControllerSender): Promise<{ ok: boolean }> {
    const run = await this.controllerRun(message.runId, sender);
    if (!run) return { ok: false };
    await this.deps.queue.updateRun(run.id, (r) => {
      r.phase = message.phase;
      r.steps = message.steps;
      r.replayPages = message.replayPages;
      r.lastAt = this.deps.now();
    });
    this.deps.changed();
    return { ok: true };
  }

  async end(message: SyncEndMessage, sender: ControllerSender): Promise<{ ok: boolean }> {
    const run = await this.controllerRun(message.runId, sender);
    if (!run) return { ok: false };
    await this.endRun(run.id, message.reason, {
      resumeCursor: message.resumeCursor,
      errorCode: message.errorCode,
      skipped: message.skipped,
      steps: message.steps,
      replayPages: message.replayPages,
    });
    return { ok: true };
  }

  // ── The tab ───────────────────────────────────────────────────────────────

  /**
   * A syncing tab navigated: a login wall ends the run at once; otherwise the bridge is asked
   * whether its document survived (an in-page navigation, which the controller judges itself)
   * or not (a reload or a new page, which ended the controller with it).
   */
  async tabUpdated(tabId: number, change: { url?: string; status?: string }): Promise<void> {
    const run = await this.openRunOfTab(tabId);
    if (!run || (!change.url && change.status !== 'loading')) return;
    if (change.url && LOGIN_PATTERNS[run.platform].test(change.url)) {
      await this.endRun(run.id, 'login_required');
      return;
    }
    await this.deps.sleep(NAVIGATION_CHECK_MS);
    const pong = await this.deps.tabs
      .sendMessage<BridgePong>(tabId, { kind: MSG.bridgePing }, { frameId: 0 })
      .catch(() => null);
    if (pong?.ok && pong.docId === run.docId && pong.syncing === run.id) return;
    const latest = await this.deps.tabs.get(tabId).catch(() => null);
    const login = !!latest?.url && LOGIN_PATTERNS[run.platform].test(latest.url);
    await this.endRun(run.id, login ? 'login_required' : 'user');
  }

  /** Ends controller runs whose tab is gone, or whose controller stopped reporting. */
  async endStale(tabExists: (tabId: number) => Promise<boolean>): Promise<void> {
    const { now } = this.deps;
    for (const run of await this.openRuns()) {
      if (run.tabId !== null && !(await tabExists(run.tabId))) await this.endRun(run.id, 'user');
      else if (now() - run.lastAt > CONTROLLER_SILENT_MS)
        await this.endRun(run.id, 'error', { errorCode: 'controller_lost' });
    }
    await this.deps.queue.pruneRunKeys(now() - RUN_KEYS_TTL_MS);
  }

  // ── The panel ─────────────────────────────────────────────────────────────

  /** Live controller runs (open, or ended and still sending), then the recent closed ones. */
  async views(): Promise<SyncView[]> {
    const live = (await this.deps.queue.runs()).filter(isControllerRun).map(viewOf);
    const ids = new Set(live.map((view) => view.runId));
    return [...live, ...(await this.history.list()).filter((view) => !ids.has(view.runId))];
  }

  /** The uploader closed a run (sent its PATCH, or found nothing to send). */
  async closed(run: Run): Promise<void> {
    if (run.accountTokenId && (await this.deps.store.pairing())?.tokenId !== run.accountTokenId)
      return;
    if (isControllerRun(run)) await this.history.add(viewOf(run), this.deps.now());
  }

  // ── Helpers ───────────────────────────────────────────────────────────────

  private async openRuns(): Promise<Run[]> {
    return (await this.deps.queue.runs()).filter(
      (run) => run.state === 'open' && isControllerRun(run),
    );
  }

  private async openRunOfTab(tabId: number): Promise<Run | null> {
    return (await this.openRuns()).find((run) => run.tabId === tabId) ?? null;
  }

  /** The open run a controller message is about, if it comes from that run's tab. */
  private async controllerRun(runId: string, sender: ControllerSender): Promise<Run | null> {
    const run = await this.deps.queue.getRun(runId);
    if (!run || run.state !== 'open' || !isControllerRun(run)) return null;
    if (sender.tab?.id !== run.tabId || sender.frameId !== 0) return null;
    if (run.accountTokenId && (await this.deps.store.pairing())?.tokenId !== run.accountTokenId) {
      await this.deps.queue.dropRun(run.id);
      return null;
    }
    return run;
  }
  private async samePairing(expected: { token: string; tokenId: string }): Promise<boolean> {
    const current = await this.deps.store.pairing();
    return current?.tokenId === expected.tokenId && current.token === expected.token;
  }

  private async endRun(
    runId: string,
    reason: SyncEndReason,
    details: Partial<
      Pick<Run, 'resumeCursor' | 'errorCode' | 'skipped' | 'steps' | 'replayPages'>
    > = {},
  ): Promise<void> {
    const at = this.deps.now();
    await this.deps.queue.updateRun(runId, (run) => {
      if (run.state !== 'open') return;
      Object.assign(run, details);
      run.state = 'ended';
      run.stopReason = reason;
      run.endedAt = at;
      run.lastAt = at;
      run.phase = null;
    });
    this.deps.changed();
    this.deps.flush();
  }
}

function collectionMode(
  choice: SyncCollectionChoice | CollectionMode,
  externalId: string | null,
): CollectionMode {
  if (typeof choice !== 'string') return choice;
  // A whole feed (IG saved, X bookmarks) maps into no collection.
  return choice === 'auto' && externalId !== null ? { mode: 'auto' } : { mode: 'none' };
}

function viewOf(run: Run): SyncView {
  return {
    runId: run.id,
    serverId: run.serverId,
    tabId: run.tabId,
    platform: run.platform,
    trigger: run.trigger,
    listingKey: run.listingKey,
    listingName: run.listing.name,
    state: run.state,
    phase: run.phase,
    stopReason: run.stopReason,
    errorCode: run.errorCode,
    skipped: run.skipped,
    incremental: run.incremental,
    pages: run.pages,
    replayPages: run.replayPages,
    steps: run.steps,
    scanned: run.scanned,
    inserted: run.inserted,
    known: run.known,
    queued: run.queued,
    startedAt: run.createdAt,
    endedAt: run.endedAt,
  };
}
