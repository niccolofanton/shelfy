// Sends the queue to the server, oldest batch first, one request at a time:
//
// 1. seal the groups that are due (sw/queue/queue.ts);
// 2. stop while unpaired or outdated, or before `blockedUntil` (Retry-After, backoff, Access);
// 3. per batch: make sure its run exists on the server (POST /sync-runs, C4), then
//    POST /ingest/batches with the batch's Idempotency-Key (C5); react to failures per
//    sw/errors.ts;
// 4. close the ended runs whose items have all been sent (PATCH /sync-runs/{id}).
//
// A flush never runs twice at once: a call during a flush asks it to go round once more.

import {
  failureCode,
  backoffMs,
  classifyFailure,
  type ApiFailure,
  type FailureAction,
} from './errors';
import type { ApiClient } from './api';
import type { ConfigService } from './config';
import {
  API,
  parseIngestResult,
  parseSyncRunCreated,
  type IngestBatchBody,
  type RunState,
  type SyncRunPatch,
} from './contracts';
import type { Queue } from './queue/queue';
import type { Batch, Run } from './queue/types';
import type { SettingsStore } from './settings';

const INGEST_TIMEOUT_MS = 60_000;
/** An Access redirect waits at least this long: the user has to sign in first. */
const ACCESS_MIN_WAIT_MS = 60_000;

export interface UploaderDeps {
  queue: Queue;
  api: ApiClient;
  store: SettingsStore;
  config: ConfigService;
  now(): number;
  random(): number;
  /** Wakes the worker at `at` to flush again (a timer, and an alarm for long waits). */
  wakeAt(at: number): void;
  /** Something the panel shows changed. */
  changed(): void;
}

type Step = 'next' | 'stop';

/** The closing PATCH of a run (C4). Passive runs end `done`, never claiming the end of the feed. */
export function closingPatch(run: Run): SyncRunPatch {
  const reason = run.stopReason ?? 'user';
  const state: RunState =
    reason === 'error' || reason === 'login_required'
      ? 'failed'
      : reason === 'user' && run.trigger !== 'passive'
        ? 'stopped'
        : 'done';
  return {
    state,
    pages: run.pages,
    scanned: run.scanned,
    stopReason: reason,
    resumeCursor: null,
    errorCode: null,
  };
}

export class Uploader {
  private running = false;
  private again = false;
  private forceAgain = false;

  constructor(private readonly deps: UploaderDeps) {}

  /** Seals what is due and sends what the gate lets through. `force` ignores the retry gate. */
  async flush(options: { force?: boolean } = {}): Promise<void> {
    if (this.running) {
      this.again = true;
      this.forceAgain ||= !!options.force;
      return;
    }
    this.running = true;
    let force = !!options.force;
    try {
      do {
        this.again = false;
        await this.flushOnce(force);
        force = this.forceAgain;
        this.forceAgain = false;
      } while (this.again);
    } finally {
      this.running = false;
    }
  }

  private async flushOnce(force: boolean): Promise<void> {
    const { queue, store, now } = this.deps;
    const sealed = await queue.sealDue(now());
    if (sealed.nextDueAt !== null) this.deps.wakeAt(sealed.nextDueAt);
    if (sealed.sealed) this.deps.changed();

    const [pairing, status] = await Promise.all([store.pairing(), store.status()]);
    if (!pairing || status.outdated) return;
    if (!force && status.blockedUntil > now()) {
      this.deps.wakeAt(status.blockedUntil);
      return;
    }
    // Batches whose run was recreated or whose key was renewed once in this flush: a second
    // time means the server refuses them for good.
    const retried = new Set<string>();
    for (;;) {
      const batch = await queue.nextBatch();
      if (!batch) break;
      if ((await this.sendBatch(batch, retried)) === 'stop') return;
    }
    for (const run of await queue.runsToClose()) if ((await this.closeRun(run)) === 'stop') return;
  }

  private async sendBatch(batch: Batch, retried: Set<string>): Promise<Step> {
    const { queue, api, now } = this.deps;
    const run = await queue.getRun(batch.runId);
    if (!run) {
      await queue.refuse(batch.id);
      return 'next';
    }
    let serverId = run.serverId;
    if (!serverId) {
      const created = await this.createRun(run);
      if ('step' in created) return created.step;
      serverId = created.serverId;
    }
    const prepared = await queue.prepareSend(batch.id, serverId);
    if (!prepared) return 'next';
    const body: IngestBatchBody = {
      syncRunId: serverId,
      platform: prepared.platform,
      source: prepared.source,
      hasNextPage: prepared.hasNextPage,
      client: prepared.client,
      items: prepared.items,
    };
    const response = await api.post(API.ingestBatches, body, {
      auth: 'token',
      idempotencyKey: prepared.key,
      timeoutMs: INGEST_TIMEOUT_MS,
    });
    if (response.ok) {
      await queue.complete(batch.id, parseIngestResult(response.data), now());
      await this.succeeded();
      return 'next';
    }
    const action = classifyFailure(response.failure);
    switch (action.action) {
      case 'drop':
        await queue.refuse(batch.id);
        await this.noteError(failureCode(response.failure));
        return 'next';
      case 'disable_source':
        // The server killed this source: the batch will never be taken. Learn the new switches.
        await queue.refuse(batch.id);
        await this.noteError('source_disabled');
        await this.deps.config.refresh(true);
        return 'next';
      case 'recreate_run':
      case 'rekey':
        if (retried.has(batch.id)) {
          await queue.refuse(batch.id);
          await this.noteError(failureCode(response.failure));
          return 'next';
        }
        retried.add(batch.id);
        if (action.action === 'recreate_run') await queue.setRunServerId(run.id, null);
        else await queue.rekey(batch.id);
        return 'next';
      case 'split':
        if (!(await queue.split(batch.id))) await queue.refuse(batch.id);
        return 'next';
      default:
        await this.block(response.failure, action);
        return 'stop';
    }
  }

  /** Opens the run on the server: its id, or how to go on when that failed. */
  private async createRun(run: Run): Promise<{ serverId: string } | { step: Step }> {
    const { queue, api } = this.deps;
    const response = await api.post(
      API.syncRuns,
      {
        platform: run.platform,
        trigger: run.trigger,
        listing: run.listing,
        collection: run.collection,
      },
      { auth: 'token' },
    );
    if (response.ok) {
      const created = parseSyncRunCreated(response.data);
      if (created) {
        await queue.setRunServerId(run.id, created.id);
        return { serverId: created.id };
      }
      await this.block(
        {
          kind: 'http',
          status: response.status,
          code: 'bad_response',
          retryAfterMs: null,
          fields: [],
        },
        { action: 'backoff' },
      );
      return { step: 'stop' };
    }
    const action = classifyFailure(response.failure);
    if (action.action === 'drop' || action.action === 'disable_source') {
      // The server will never open this run: its items cannot be sent.
      await queue.dropRun(run.id);
      await this.noteError(failureCode(response.failure));
      if (action.action === 'disable_source') await this.deps.config.refresh(true);
      this.deps.changed();
      return { step: 'next' };
    }
    await this.block(
      response.failure,
      action.action === 'recreate_run' ? { action: 'backoff' } : action,
    );
    return { step: 'stop' };
  }

  private async closeRun(run: Run): Promise<Step> {
    const { queue, api } = this.deps;
    if (!run.serverId) {
      await queue.forgetRun(run.id);
      return 'next';
    }
    const response = await api.patch(API.syncRun(run.serverId), closingPatch(run), {
      auth: 'token',
    });
    if (response.ok) {
      await queue.forgetRun(run.id);
      return 'next';
    }
    const action = classifyFailure(response.failure);
    if (action.action === 'drop' || action.action === 'recreate_run') {
      // The server no longer knows the run, or refuses the patch: nothing left to report.
      await queue.forgetRun(run.id);
      return 'next';
    }
    await this.block(response.failure, action);
    return 'stop';
  }

  private async succeeded(): Promise<void> {
    const now = this.deps.now();
    await this.deps.store.patchStatus((status) =>
      status.failures || status.blockedUntil || status.lastError
        ? { failures: 0, blockedUntil: 0, lastError: null, lastOkAt: now }
        : { lastOkAt: now },
    );
    this.deps.changed();
  }

  private async noteError(code: string): Promise<void> {
    const at = this.deps.now();
    await this.deps.store.patchStatus({ lastError: { code, at } });
    this.deps.changed();
  }

  /** Stops sending after a failure, and records why and until when. */
  async block(failure: ApiFailure, action: FailureAction): Promise<void> {
    const { store, now, random } = this.deps;
    const at = now();
    const code = failureCode(failure);
    if (action.action === 'unpair') await store.setPairing(null);
    const status = await store.patchStatus((current) => {
      const patch: Partial<typeof current> = { lastError: { code, at } };
      switch (action.action) {
        case 'outdated':
          patch.outdated = true;
          break;
        case 'wait':
          patch.blockedUntil = at + action.ms;
          break;
        case 'hold':
          patch.blockedUntil = at + action.ms;
          break;
        case 'access':
          patch.failures = current.failures + 1;
          patch.blockedUntil = at + Math.max(ACCESS_MIN_WAIT_MS, backoffMs(patch.failures, random));
          break;
        case 'unpair':
          // The token is gone (above): nothing is sent until the next pairing.
          break;
        default:
          // `backoff`, and any action a caller could not act on itself.
          patch.failures = current.failures + 1;
          patch.blockedUntil = at + backoffMs(patch.failures, random);
          break;
      }
      return patch;
    });
    if (status.blockedUntil > at) this.deps.wakeAt(status.blockedUntil);
    this.deps.changed();
  }
}
