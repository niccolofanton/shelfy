import { API, parseIngestResult, parseSyncRunCreated, type ExtensionConfig } from '../contracts';
import type { ApiClient } from '../api';
import type { ApiFailure } from '../errors';
import { failureCode } from '../errors';
import { prefilterBatch } from '../prefilter';
import type { SettingsStore, StorageArea, Pairing } from '../settings';
import type { PlannerService } from '../planner/service';
import { PLATFORMS, type Platform } from '../../shared/protocol';
import { TASK_API, TaskError, parseTasks, type ExtensionTask, type TaskOutcome } from './contracts';
import type { ImageUploadDeps } from './upload';
import { uploadImage } from './upload';
import type { TaskInstagram } from './instagram';

export const TASK_STATE_KEY = 'shelfy.tasks.state';
export const TASK_SESSION_KEY = 'shelfy.tasks.sessions';
export const REFRESH_GAP_MS = 700;
export interface TasksState {
  waiting: Record<Platform, number>;
  running: boolean;
  code: string | null;
  completed: number;
  lastPolledAt: number | null;
}
interface Session {
  count: number;
  lastAt: number;
  blocked: string | null;
}
interface Context {
  pairing: Pairing;
  api: ApiClient;
  abort: AbortController;
}
export interface TasksDeps {
  store: Pick<SettingsStore, 'pairing' | 'status'>;
  session: StorageArea;
  config(): Promise<ExtensionConfig>;
  apiFor(pairing: Pairing): ApiClient;
  uploadDeps(pairing: Pairing): Omit<ImageUploadDeps, 'guard'>;
  instagram: TaskInstagram;
  planner: Pick<PlannerService, 'requestInstagramBacklogSync' | 'syncing'>;
  now(): number;
  sleep(ms: number): Promise<void>;
  id(): string;
  changed(): void;
  failure(failure: ApiFailure): Promise<void>;
  client: { ext: string; parser: string };
}
const emptyState = (): TasksState => ({
  waiting: { instagram: 0, twitter: 0, pinterest: 0 },
  running: false,
  code: null,
  completed: 0,
  lastPolledAt: null,
});

/** Leases and pending completions deliberately never survive a worker restart.
 * Every wake polls fresh; a 409 discards the old batch before repolling. */
export class TasksService {
  private state = emptyState();
  private sessions: Record<number, Session> = {};
  private accountKey: string | null = null;
  private work: Promise<void> | null = null;
  private context: Context | null = null;
  constructor(private readonly deps: TasksDeps) {}
  private async account(pairing: Pairing | null): Promise<void> {
    const key = pairing?.tokenId ?? null;
    if (key === this.accountKey) return;
    this.state = emptyState();
    this.sessions = {};
    this.accountKey = key;
    if (key) {
      const stored = await this.deps.session.get([TASK_SESSION_KEY, TASK_STATE_KEY]);
      const record = stored[TASK_SESSION_KEY] as
        | { accountKey?: string; sessions?: Record<number, Session> }
        | undefined;
      if (record?.accountKey === key && record.sessions) this.sessions = record.sessions;
      const state = stored[TASK_STATE_KEY] as
        | { accountKey?: string; state?: TasksState }
        | undefined;
      if (state?.accountKey === key && state.state) this.state = { ...state.state, running: false };
    }
    await this.save();
  }
  private async save(): Promise<void> {
    await this.deps.session.set({
      [TASK_SESSION_KEY]: { accountKey: this.accountKey, sessions: this.sessions },
      [TASK_STATE_KEY]: { accountKey: this.accountKey, state: this.state },
    });
    this.deps.changed();
  }
  async snapshot(): Promise<TasksState> {
    await this.account(await this.deps.store.pairing());
    return structuredClone(this.state);
  }
  async reset(): Promise<void> {
    this.context?.abort.abort();
    this.accountKey = null;
    this.sessions = {};
    this.state = emptyState();
    await this.deps.session.remove([TASK_SESSION_KEY, TASK_STATE_KEY]);
    this.deps.changed();
  }
  async tabRemoved(tabId: number): Promise<void> {
    delete this.sessions[tabId];
    await this.save();
  }
  poll(): Promise<void> {
    if (this.work) return this.work;
    this.work = this.drain().finally(() => {
      this.work = null;
    });
    return this.work;
  }
  private async guard(ctx: Context, task?: ExtensionTask): Promise<void> {
    const [pairing, status] = await Promise.all([
      this.deps.store.pairing(),
      this.deps.store.status(),
    ]);
    if (
      ctx.abort.signal.aborted ||
      pairing?.tokenId !== ctx.pairing.tokenId ||
      pairing.token !== ctx.pairing.token
    )
      throw new TaskError('cancelled');
    if (status.outdated || status.blockedUntil > this.deps.now()) throw new TaskError('disabled');
    if (task && task.leaseUntil <= this.deps.now()) throw new TaskError('lease_expired');
    if (task) {
      const config = await this.deps.config();
      const modes = config.platforms[task.platform];
      if (
        (!modes.passive && !modes.replay && !modes.scroll) ||
        (task.kind !== 'upload_media' && config.refreshPerSession === 0)
      )
        throw new TaskError('disabled');
    }
  }
  private async complete(
    ctx: Context,
    task: ExtensionTask,
    outcome: TaskOutcome,
    extra: { uploadId?: string; errorCode?: string } = {},
  ): Promise<void> {
    await this.guard(ctx, task);
    const answer = await ctx.api.post(
      TASK_API.complete(task.id),
      {
        leaseId: task.leaseId,
        outcome,
        uploadId: extra.uploadId ?? null,
        errorCode: extra.errorCode ?? null,
      },
      { auth: 'token', expectedToken: ctx.pairing.token },
    );
    await this.guard(ctx);
    if (!answer.ok) {
      if (answer.failure.kind === 'http' && answer.failure.status === 409)
        throw new TaskError('stale_lease');
      await this.deps.failure(answer.failure);
      throw new TaskError('completion_unknown');
    }
    await this.guard(ctx);
    this.state.completed++;
    this.state.waiting[task.platform] = Math.max(0, this.state.waiting[task.platform] - 1);
  }
  private async ingest(ctx: Context, task: ExtensionTask, raw: unknown[]): Promise<void> {
    const items = prefilterBatch(raw, 'instagram')
      .items.filter((item) => item.id.split('_')[0] === task.nativeId)
      .slice(0, 1);
    if (!items.length || items.every((item) => !item.thumbnailUrl && !item.media.length))
      throw new TaskError('empty_refresh');
    await this.guard(ctx, task);
    const created = await ctx.api.post(
      API.syncRuns,
      {
        platform: 'instagram',
        trigger: 'refresh',
        listing: { kind: 'ig_saved', externalId: null, name: null },
        collection: { mode: 'none' },
      },
      { auth: 'token', expectedToken: ctx.pairing.token },
    );
    await this.guard(ctx);
    if (!created.ok) {
      await this.deps.failure(created.failure);
      throw new TaskError('ingest_unavailable');
    }
    const run = parseSyncRunCreated(created.data);
    if (!run) throw new TaskError('bad_response');
    try {
      await this.guard(ctx, task);
      const result = await ctx.api.post(
        API.ingestBatches,
        {
          syncRunId: run.id,
          platform: 'instagram',
          source: 'refresh',
          hasNextPage: null,
          client: this.deps.client,
          items,
        },
        { auth: 'token', expectedToken: ctx.pairing.token, idempotencyKey: this.deps.id() },
      );
      await this.guard(ctx);
      if (!result.ok) {
        await this.deps.failure(result.failure);
        throw new TaskError('ingest_unavailable');
      }
      const merged = parseIngestResult(result.data);
      if (!merged.results.some((entry) => entry.key === task.postKey))
        throw new TaskError('refresh_rejected');
      await this.guard(ctx);
      // A media-info reply never proves that a saved feed reached its end.
      await ctx.api.patch(
        API.syncRun(run.id),
        {
          state: 'done',
          pages: 1,
          scanned: items.length,
          stopReason: null,
          resumeCursor: null,
          errorCode: null,
        },
        { auth: 'token', expectedToken: ctx.pairing.token },
      );
    } catch (error) {
      if (
        await this.deps.store.pairing().then((pairing) => pairing?.tokenId === ctx.pairing.tokenId)
      )
        await ctx.api.patch(
          API.syncRun(run.id),
          {
            state: 'failed',
            pages: 0,
            scanned: 0,
            stopReason: 'error',
            resumeCursor: null,
            errorCode: 'refresh_failed',
          },
          { auth: 'token', expectedToken: ctx.pairing.token },
        );
      throw error;
    }
  }
  private async drain(): Promise<void> {
    const pairing = await this.deps.store.pairing();
    await this.account(pairing);
    if (!pairing) return;
    const ctx: Context = { pairing, api: this.deps.apiFor(pairing), abort: new AbortController() };
    this.context = ctx;
    try {
      await this.guard(ctx);
      const initial = await this.deps.config();
      if (
        !PLATFORMS.some((platform) => {
          const modes = initial.platforms[platform];
          return modes.passive || modes.replay || modes.scroll;
        })
      )
        return;
      this.state.running = true;
      this.state.code = null;
      await this.save();
      for (let round = 0; round < 10; round++) {
        await this.guard(ctx);
        const response = await ctx.api.get(round === 0 ? TASK_API.poll : TASK_API.next, {
          auth: 'token',
          expectedToken: ctx.pairing.token,
          timeoutMs: 35_000,
          signal: ctx.abort.signal,
        });
        await this.guard(ctx);
        if (!response.ok) {
          await this.deps.failure(response.failure);
          this.state.code = failureCode(response.failure);
          break;
        }
        await this.guard(ctx);
        const batch = parseTasks(response.data);
        if (!batch) {
          this.state.code = 'bad_response';
          break;
        }
        this.state.waiting = batch.waiting;
        this.state.lastPolledAt = this.deps.now();
        await this.save();
        if (!batch.tasks.length) break;
        const config = await this.deps.config();
        const unavailable = Object.entries(this.sessions)
          .filter(([, session]) => session.blocked || session.count >= config.refreshPerSession)
          .map(([id]) => Number(id));
        const tab = await this.deps.instagram.find(unavailable);
        let igBusy = (await this.deps.planner.syncing()).instagram;
        if (tab && !igBusy && batch.waiting.instagram >= 200 && config.refreshPerSession > 0) {
          const full = await this.deps.planner.requestInstagramBacklogSync({
            backlog: batch.waiting.instagram,
            tabId: tab.id,
          });
          igBusy = full.ok || (!full.ok && full.code === 'busy');
          if (full.ok) this.state.code = 'full_sync';
        }
        let progressed = false;
        let stale = false;
        for (const task of batch.tasks) {
          try {
            await this.guard(ctx, task);
            if (task.kind === 'upload_media') {
              const uploadId = await uploadImage(
                task,
                { ...this.deps.uploadDeps(pairing), guard: () => this.guard(ctx, task) },
                ctx.abort.signal,
              );
              await this.complete(ctx, task, 'uploaded', { uploadId });
              progressed = true;
            } else {
              if (igBusy || !tab) {
                this.state.code ??= unavailable.length ? 'session_limit' : 'no_instagram_tab';
                continue;
              }
              const session = (this.sessions[tab.id] ??= { count: 0, lastAt: 0, blocked: null });
              const current = await this.deps.config();
              if (session.blocked || session.count >= current.refreshPerSession) {
                this.state.code = session.blocked ?? 'session_limit';
                continue;
              }
              const delay = Math.max(0, REFRESH_GAP_MS - (this.deps.now() - session.lastAt));
              if (delay) await this.deps.sleep(delay);
              await this.guard(ctx, task);
              session.lastAt = this.deps.now();
              session.count++;
              await this.save();
              let answer;
              try {
                answer = await this.deps.instagram.refresh(tab, task, this.deps.id());
              } finally {
                session.lastAt = this.deps.now();
                await this.save();
              }
              await this.guard(ctx, task);
              if (answer.outcome === 'blocked') {
                session.blocked = answer.code;
                this.state.code = answer.code;
                await this.save();
                continue;
              }
              if (answer.outcome === 'gone') await this.complete(ctx, task, 'gone');
              else if (answer.outcome === 'refreshed') {
                await this.ingest(ctx, task, answer.items);
                await this.complete(ctx, task, 'refreshed');
              } else {
                if (answer.code === 'reload_tab') {
                  this.state.code = answer.code;
                  continue;
                }
                throw new TaskError(answer.code);
              }
              progressed = true;
            }
          } catch (error) {
            const code = error instanceof TaskError ? error.code : 'task_network';
            if (['cancelled', 'completion_unknown', 'ingest_unavailable'].includes(code))
              throw error;
            if (code === 'stale_lease' || code === 'lease_expired') {
              stale = true;
              break;
            }
            if (code === 'disabled') {
              this.state.code = code;
              continue;
            }
            try {
              await this.complete(ctx, task, 'failed', { errorCode: code });
              progressed = true;
            } catch (error) {
              if (error instanceof TaskError && error.code === 'stale_lease') {
                stale = true;
                break;
              }
              throw error;
            }
          }
          await this.save();
        }
        await this.save();
        if (!progressed && !stale) break;
      }
    } catch (error) {
      if (error instanceof TaskError && error.code !== 'cancelled') this.state.code = error.code;
      else if (!(error instanceof TaskError)) this.state.code = 'task_network';
    } finally {
      this.context = null;
      const current = await this.deps.store.pairing();
      if (current?.tokenId !== pairing.tokenId) {
        await this.account(current);
      }
      this.state.running = false;
      await this.save();
    }
  }
}
