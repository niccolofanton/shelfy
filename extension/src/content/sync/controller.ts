// The sync controller (plan §2.16, P2-13): walks one saved listing in the syncing tab's content
// script, because MV3 stops an idle service worker after ~30 s and the walk takes minutes. The
// worker opens the run (POST /sync-runs) and hands it here as a plan (MSG.syncRun); the hook's
// captures of this document go to that run (sw/capture.ts); this file decides how to walk and
// when to stop, and reports the end (MSG.syncEnd), which the worker turns into the closing PATCH.
//
//   Instagram  the REST replay (main/replay.ts, injected by the worker in the MAIN world, gated
//              here at every page boundary), then the gradual two-pass scroll (scroll.ts).
//   X          the two-pass scroll; X's DOM scan runs on the scroll events (main/passive.ts).
//   Pinterest  the server-rendered first page (read by the hook), then the scroll.
//
// It ends on: the end of the feed (X and Pinterest on `hasNextPage === false`, Instagram when
// the replay and the scroll have both settled); the incremental stop (P2-G1: at a page boundary,
// `stopAfterKnown` consecutive known items, from the ingest results); the replay's page cap,
// with its cursor (P2-G2); the step and time caps; a login wall; the tab leaving the listing;
// the user's stop. A resumed walk (P2-G2) reads the head until the known run, then jumps to the
// stored cursor.

import { syncTarget } from '../../shared/listing';
import {
  MSG,
  REPLAY_GATE_MESSAGE,
  type GateAction,
  type InterceptMessage,
  type ReplayPageMessage,
  type ScopeDetail,
  type ScopeMessage,
  type SyncEndReason,
  type SyncKnownAnswer,
  type SyncMode,
  type SyncPhase,
  type SyncPlan,
} from '../../shared/protocol';
import type { RelayObserver } from '../relay';
import { LAST_TILE_SELECTOR, gradualScroll, type ScrollEnv, type ScrollOutcome } from './scroll';
import { checkPage, reachedKnownRun } from './termination';

/** How often the controller checks where the tab is and reports progress. */
export const WATCH_EVERY_MS = 500;
export const PROGRESS_EVERY_MS = 1_000;
/** The worker must inject the replay within this time, or the replay counts as failed. */
export const REPLAY_START_TIMEOUT_MS = 15_000;
/** After the scroll, the page may still be fetching a last page: wait this long for it. */
const SETTLE_AFTER_SCROLL_MS = 600;
const MAX_SEEN_IDS = 50_000;

export interface ControllerDeps {
  /** chrome.runtime.sendMessage to the worker. */
  send<T = unknown>(message: unknown): Promise<T>;
  /** window.postMessage to the page's MAIN world (the replay's gate answers). */
  postToPage(message: unknown): void;
  href(): string;
  /** The URL this document was loaded at (a Pinterest board's inline first page is this one's). */
  initialHref: string;
  /** Resolves once the bridge delivered every capture relayed so far to the worker. */
  relayIdle(): Promise<void>;
  scroll: Pick<ScrollEnv, 'scrollY' | 'innerHeight' | 'scrollBy' | 'scrollTo' | 'revealLast'>;
  sleep(ms: number): Promise<void>;
  now(): number;
  randomId(): string;
  warn(message: string): void;
}

interface ReplayState {
  id: string;
  started: () => void;
  ended: (detail: ScopeDetail | null) => void;
}

interface ReplayOutcome {
  reason: string;
  status: number | null;
}

export class SyncController implements RelayObserver {
  private plan: SyncPlan | null = null;
  private deadline = 0;
  /** Set by the user's stop, the watcher (login wall, left the listing) or the time cap. */
  private stopReason: SyncEndReason | null = null;
  private knownStop = false;
  private endSignal = false;
  private boundaryPending = false;
  private seen = new Set<string>();
  private lastInterceptAt = 0;
  private phase: SyncPhase = 'starting';
  private steps = 0;
  private replayPages = 0;
  private replay: ReplayState | null = null;
  /** The cursor the replay reported at its last boundary. */
  private cursor: string | null = null;
  /** A resumed walk jumped to the stored cursor. */
  private jumped = false;

  constructor(private readonly deps: ControllerDeps) {}

  /** The run this document syncs, if any. */
  get runId(): string | null {
    return this.plan?.runId ?? null;
  }

  /** Starts walking; false when this document already syncs. */
  start(plan: SyncPlan): boolean {
    if (this.plan) return false;
    this.reset(plan);
    void this.run(plan).catch((err: unknown) => {
      this.deps.warn(`sync failed: ${err instanceof Error ? err.message : String(err)}`);
      void this.finish(plan, 'error', 'controller');
    });
    return true;
  }

  /** Stops the walk at the next step or page boundary. */
  abort(reason: SyncEndReason = 'user'): void {
    if (this.plan && !this.stopReason) this.stopReason = reason;
  }

  // ── What the bridge relays (RelayObserver) ────────────────────────────────

  intercept(message: InterceptMessage): void {
    const plan = this.plan;
    if (!plan || message.platform !== plan.platform) return;
    if (message.items.length || message.hasNextPage === false)
      this.lastInterceptAt = this.deps.now();
    for (const item of message.items) {
      const id = (item as { id?: unknown }).id;
      if ((typeof id === 'string' || typeof id === 'number') && this.seen.size < MAX_SEEN_IDS)
        this.seen.add(String(id));
    }
    if (message.items.length) this.boundaryPending = true;
    // On Instagram the replay's last page says `more_available: false` long before the scroll
    // is done: only the replay's own end counts there (as on the desktop).
    if (message.hasNextPage === false && plan.platform !== 'instagram') this.endSignal = true;
  }

  scope(message: ScopeMessage): void {
    const replay = this.replay;
    if (!replay || message.source !== 'replay' || message.id !== replay.id) return;
    if (message.phase === 'start') replay.started();
    else replay.ended(message.detail);
  }

  replayPage(message: ReplayPageMessage): void {
    const replay = this.replay;
    if (!replay || message.id !== replay.id) return;
    this.replayPages = Math.max(this.replayPages, message.page);
    this.cursor = message.cursor;
    if (message.wait)
      void this.gate().then(
        ({ action, cursor }) =>
          this.deps.postToPage({ type: REPLAY_GATE_MESSAGE, id: replay.id, action, cursor }),
        () => this.deps.postToPage({ type: REPLAY_GATE_MESSAGE, id: replay.id, action: 'stop' }),
      );
  }

  // ── The walk ──────────────────────────────────────────────────────────────

  private reset(plan: SyncPlan): void {
    this.plan = plan;
    this.deadline = this.deps.now() + plan.maxRunMs;
    this.stopReason = null;
    this.knownStop = false;
    this.endSignal = false;
    this.boundaryPending = false;
    this.seen = new Set();
    this.lastInterceptAt = this.deps.now();
    this.phase = 'starting';
    this.steps = 0;
    this.replayPages = 0;
    this.replay = null;
    this.cursor = null;
    this.jumped = false;
  }

  private async run(plan: SyncPlan): Promise<void> {
    let running = true;
    void this.watch(plan, () => running);
    void this.reportProgress(plan, () => running);
    const skipped: SyncMode[] = [];
    let replay: ReplayOutcome | null = null;
    let scroll: ScrollOutcome | null = null;
    try {
      if (plan.platform === 'instagram') {
        if (plan.replay) {
          this.phase = 'replay';
          replay = await this.runReplay(plan);
        } else skipped.push('replay');
      } else if (plan.platform === 'pinterest') {
        this.phase = 'ssr';
        await this.readPinterestSsr(plan);
      }
      if (!this.terminal(replay)) {
        if (plan.scroll) {
          this.phase = 'scroll';
          scroll = await this.runScroll(plan);
        } else skipped.push('scroll');
      }
    } finally {
      running = false;
    }
    const { reason, errorCode } = this.endReason(plan, replay, scroll);
    await this.finish(plan, reason, errorCode, skipped);
  }

  /** Whether the walk must stop before the next phase. */
  private terminal(replay: ReplayOutcome | null): boolean {
    if (this.stopReason || this.knownStop) return true;
    if (!replay) return false;
    return !['end_of_feed', 'page_cap'].includes(replay.reason);
  }

  /** The end reason, by priority: a stop, the known run, a failure, a cap, the end of the feed. */
  private endReason(
    plan: SyncPlan,
    replay: ReplayOutcome | null,
    scroll: ScrollOutcome | null,
  ): { reason: SyncEndReason; errorCode: string | null } {
    if (this.stopReason) return { reason: this.stopReason, errorCode: null };
    if (this.knownStop) return { reason: 'known_run', errorCode: null };
    if (replay && !['end_of_feed', 'page_cap', 'stopped'].includes(replay.reason)) {
      // Instagram answers a signed-out replay with 401 or 403 (assumption, P2-13).
      if (replay.reason === 'http_error' && (replay.status === 401 || replay.status === 403))
        return { reason: 'login_required', errorCode: null };
      return {
        reason: 'error',
        errorCode:
          replay.reason === 'http_error' && replay.status
            ? `replay_http_${replay.status}`
            : `replay_${replay.reason}`,
      };
    }
    if (scroll === 'step_cap' || scroll === 'time_cap')
      return { reason: 'time_cap', errorCode: null };
    if (replay?.reason === 'page_cap') return { reason: 'page_cap', errorCode: null };
    if (plan.platform === 'instagram' ? replay?.reason === 'end_of_feed' : this.endSignal)
      return { reason: 'end_of_feed', errorCode: null };
    return { reason: 'stalled', errorCode: null };
  }

  private async finish(
    plan: SyncPlan,
    reason: SyncEndReason,
    errorCode: string | null,
    skipped: SyncMode[] = [],
  ): Promise<void> {
    if (this.plan !== plan) return;
    this.phase = 'ending';
    // The run takes captures until it ends: deliver what the page relayed first.
    await this.deps.relayIdle();
    const message = {
      kind: MSG.syncEnd,
      runId: plan.runId,
      reason,
      resumeCursor: reason === 'page_cap' ? this.cursor : null,
      errorCode,
      skipped,
      steps: this.steps,
      replayPages: this.replayPages,
    };
    for (let attempt = 1; attempt <= 3; attempt++) {
      try {
        await this.deps.send(message);
        break;
      } catch (err) {
        if (attempt === 3) this.deps.warn(`could not report the end of the sync: ${String(err)}`);
        else await this.deps.sleep(400 * attempt);
      }
    }
    this.plan = null;
    this.replay = null;
  }

  /** Every 500 ms: a login wall, the tab leaving the listing, the time cap. */
  private async watch(plan: SyncPlan, running: () => boolean): Promise<void> {
    while (running() && this.plan === plan) {
      if (!this.stopReason) {
        const page = checkPage(plan.platform, plan.listingKey, this.deps.href());
        if (page === 'login_required') this.stopReason = 'login_required';
        else if (page === 'left') this.stopReason = 'user';
        else if (this.deps.now() >= this.deadline) this.stopReason = 'time_cap';
      }
      await this.deps.sleep(WATCH_EVERY_MS);
    }
  }

  private async reportProgress(plan: SyncPlan, running: () => boolean): Promise<void> {
    while (running() && this.plan === plan) {
      await this.deps
        .send({
          kind: MSG.syncProgress,
          runId: plan.runId,
          phase: this.phase,
          steps: this.steps,
          replayPages: this.replayPages,
        })
        .catch(() => undefined);
      await this.deps.sleep(PROGRESS_EVERY_MS);
    }
  }

  // ── Instagram: the replay ─────────────────────────────────────────────────

  private async runReplay(plan: SyncPlan): Promise<ReplayOutcome> {
    let started!: () => void;
    let ended!: (detail: ScopeDetail | null) => void;
    const startedPromise = new Promise<void>((resolve) => (started = resolve));
    const endedPromise = new Promise<ScopeDetail | null>((resolve) => (ended = resolve));
    const id = `replay-${this.deps.randomId()}`;
    this.replay = { id, started, ended };
    const answer = await this.deps
      .send<{ ok?: boolean }>({ kind: MSG.syncMain, runId: plan.runId, op: 'replay', replayId: id })
      .catch(() => null);
    if (!answer?.ok) return { reason: 'unavailable', status: null };
    const timedOut = Symbol('timeout');
    const first = await Promise.race([
      startedPromise,
      this.deps.sleep(REPLAY_START_TIMEOUT_MS).then(() => timedOut),
    ]);
    if (first === timedOut) return { reason: 'unavailable', status: null };
    const detail = await endedPromise;
    const reason = typeof detail?.reason === 'string' ? detail.reason : 'unknown';
    const status = typeof detail?.status === 'number' ? detail.status : null;
    if (typeof detail?.pages === 'number')
      this.replayPages = Math.max(this.replayPages, detail.pages);
    if (reason === 'end_of_feed') this.cursor = null;
    return { reason, status };
  }

  /**
   * The answer at a page boundary of the replay: stop (a stop or cap, or the known run), jump
   * to the stored cursor (a resumed walk whose head is known), or continue.
   */
  private async gate(): Promise<{ action: GateAction; cursor: string | null }> {
    const plan = this.plan;
    if (!plan) return { action: 'stop', cursor: null };
    if (this.deps.now() >= this.deadline && !this.stopReason) this.stopReason = 'time_cap';
    if (this.stopReason || this.knownStop) return { action: 'stop', cursor: null };
    const head = plan.resumeCursor !== null && !this.jumped;
    if (!plan.incremental && !head) return { action: 'continue', cursor: null };
    if (!reachedKnownRun(await this.known(plan, false), plan.stopAfterKnown))
      return { action: 'continue', cursor: null };
    if (head) {
      // The head is known: continue where the capped walk stopped, counting afresh.
      this.jumped = true;
      await this.known(plan, true);
      return { action: 'jump', cursor: plan.resumeCursor };
    }
    this.knownStop = true;
    return { action: 'stop', cursor: null };
  }

  /** The worker's known-run count once the relayed captures are ingested; null when unknown. */
  private async known(
    plan: SyncPlan,
    reset: boolean,
  ): Promise<{ streak: number; settled: boolean } | null> {
    await this.deps.relayIdle();
    const answer = await this.deps
      .send<SyncKnownAnswer>({ kind: MSG.syncKnown, runId: plan.runId, reset })
      .catch(() => null);
    return answer && answer.ok ? answer : null;
  }

  // ── Pinterest: the inline first page ──────────────────────────────────────

  private async readPinterestSsr(plan: SyncPlan): Promise<void> {
    // The inline blob describes the page the document was loaded at: after a client-side
    // navigation to this board it would be another board's first page, so it is skipped.
    if (syncTarget('pinterest', this.deps.initialHref)?.key !== plan.listingKey) return;
    await this.deps
      .send({ kind: MSG.syncMain, runId: plan.runId, op: 'pinterest_ssr', replayId: null })
      .catch(() => null);
    await this.deps.relayIdle();
  }

  // ── The scroll ────────────────────────────────────────────────────────────

  private async runScroll(plan: SyncPlan): Promise<ScrollOutcome> {
    const env: ScrollEnv = {
      ...this.deps.scroll,
      sleep: (ms) => this.deps.sleep(ms),
      now: () => this.deps.now(),
      captured: () => this.seen.size,
      lastInterceptAt: () => this.lastInterceptAt,
      afterStep: () => this.afterStep(plan),
    };
    const result = await gradualScroll(env, {
      selector: LAST_TILE_SELECTOR[plan.platform],
      settleMs: plan.scrollSettleMs,
      maxSteps: Math.max(0, plan.maxSteps - this.steps),
      deadline: this.deadline,
    });
    if (result.outcome !== 'stopped') {
      // The last step may have started a page fetch: let it land in this run.
      await this.deps.sleep(SETTLE_AFTER_SCROLL_MS);
    }
    return result.outcome;
  }

  /** After each scroll step (counted here, for the progress report and the shared cap). */
  private async afterStep(plan: SyncPlan): Promise<boolean> {
    this.steps += 1;
    if (this.stopReason || this.endSignal) return true;
    if (plan.incremental && this.boundaryPending) {
      this.boundaryPending = false;
      if (reachedKnownRun(await this.known(plan, false), plan.stopAfterKnown)) {
        this.knownStop = true;
        return true;
      }
    }
    return false;
  }
}
