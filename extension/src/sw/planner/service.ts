import { LOGIN_PATTERNS } from '../../../../src/lib/browserUrls';
import { platformForUrl } from '../../shared/hosts';
import { syncTarget } from '../../shared/listing';
import {
  PLATFORMS,
  type BridgePong,
  type ExternalAnswer,
  type Platform,
  type SyncTarget,
} from '../../shared/protocol';
import type { StorageArea, Pairing } from '../settings';
import type { SyncService } from '../sync/service';
import {
  DEFAULT_SCHEDULE,
  StepError,
  buildSyncSteps,
  parseSchedule,
  stepErrorAction,
  type ExtensionSource,
  type ScheduleSettings,
  type SyncStep,
} from './model';

export const PLANNER_KEY = 'shelfy.planner';
export const SCHEDULE_KEY = 'shelfy.schedule';
export const INSTAGRAM_BACKLOG_DAY_KEY = 'shelfy.instagramBacklogSyncDay';
export const PLANNER_ALARM = {
  reminder: 'shelfy.sync.reminder',
  taskPoll: 'shelfy.tasks.poll',
} as const;
export const STEP_TIMEOUT_MS = 35 * 60_000;
export const NAVIGATION_TIMEOUT_MS = 45_000;
export const BRIDGE_TIMEOUT_MS = 20_000;
export const REMINDER_NOTIFICATION = 'shelfy.sync.now';
export interface PlannerJob {
  accountKey?: string;
  platform: Platform;
  status: 'navigating' | 'syncing' | 'done' | 'stopped' | 'error';
  step: number;
  total: number;
  skipped: number;
  code: string | null;
  tabId: number | null;
  windowId: number | null;
  startedAt: number;
}
export interface PlannerBrowser {
  open(
    platform: Platform,
    previousWindowId: number | null,
    minimized: boolean,
  ): Promise<{ tabId: number; windowId: number }>;
  existing(tabId: number): Promise<{ tabId: number; windowId: number }>;
  get(tabId: number): Promise<{ url?: string; status?: string }>;
  navigate(tabId: number, url: string): Promise<void>;
  reload(tabId: number): Promise<void>;
  ping(tabId: number): Promise<BridgePong | null>;
  instagramUsername(tabId: number): Promise<{ username: string | null; login: boolean }>;
  folderLinks(tabId: number): Promise<string[]>;
}
export interface PlannerDeps {
  storage: StorageArea;
  browser: PlannerBrowser;
  sync: Pick<SyncService, 'start' | 'stopPlatform' | 'syncing' | 'views'>;
  eligibility(platform: Platform): Promise<string | null>;
  sources(pairing?: Pairing): Promise<ExtensionSource[]>;
  now(): number;
  sleep(ms: number): Promise<void>;
  changed(): void;
  log(where: string, error: unknown): void;
  afterSync?(): Promise<void>;
  accountKey?(): Promise<string | null>;
  pairing?(): Promise<Pairing | null>;
}
interface RunHandle {
  stopped: boolean;
  full: boolean;
  existingTabId?: number;
  minimized: boolean;
  pairing?: Pairing;
}
export class PlannerService {
  private active = new Map<Platform, RunHandle>();
  private jobs: Partial<Record<Platform, PlannerJob>> = {};
  private writes: Promise<unknown> = Promise.resolve();
  private loaded: Promise<void> | null = null;
  constructor(private readonly deps: PlannerDeps) {}
  private load(): Promise<void> {
    this.loaded ??= this.deps.storage.get(PLANNER_KEY).then((stored) => {
      const value = stored[PLANNER_KEY];
      if (value && typeof value === 'object') this.jobs = value as typeof this.jobs;
    });
    return this.loaded;
  }
  private async patch(platform: Platform, patch: Partial<PlannerJob>): Promise<void> {
    this.jobs[platform] = { ...this.jobs[platform]!, ...patch };
    const snapshot = structuredClone(this.jobs);
    this.writes = this.writes
      .catch(() => undefined)
      .then(() => this.deps.storage.set({ [PLANNER_KEY]: snapshot }));
    await this.writes;
    this.deps.changed();
  }
  async snapshot(): Promise<{ jobs: PlannerJob[]; schedule: ScheduleSettings }> {
    await this.load();
    const pairing = await this.deps.pairing?.();
    return {
      jobs: PLATFORMS.flatMap((p) =>
        this.jobs[p] && (!this.deps.pairing || this.jobs[p]?.accountKey === pairing?.tokenId)
          ? [this.jobs[p]!]
          : [],
      ),
      schedule: await this.schedule(),
    };
  }
  async schedule(): Promise<ScheduleSettings> {
    return (
      parseSchedule((await this.deps.storage.get(SCHEDULE_KEY))[SCHEDULE_KEY]) ?? {
        ...DEFAULT_SCHEDULE,
      }
    );
  }
  async setSchedule(schedule: ScheduleSettings): Promise<void> {
    await this.deps.storage.set({ [SCHEDULE_KEY]: schedule });
    this.deps.changed();
  }
  async syncing(): Promise<Record<Platform, boolean>> {
    const running = await this.deps.sync.syncing();
    return Object.fromEntries(
      PLATFORMS.map((p) => [p, this.active.has(p) || running[p]]),
    ) as Record<Platform, boolean>;
  }
  async start(
    target: SyncTarget,
    options: {
      trigger?: 'web' | 'scheduled';
      full?: boolean;
      existingTabId?: number;
      minimized?: boolean;
    } = {},
  ): Promise<ExternalAnswer> {
    const platform = target.platform;
    if (this.active.has(platform)) return { ok: false, code: 'busy' };
    const handle: RunHandle = {
      stopped: false,
      full: options.full === true,
      existingTabId: options.existingTabId,
      minimized: options.minimized === true,
    };
    this.active.set(platform, handle);
    try {
      if (this.deps.pairing) {
        const pairing = await this.deps.pairing();
        if (!pairing) {
          this.active.delete(platform);
          return { ok: false, code: 'not_paired' };
        }
        handle.pairing = pairing;
      }
      await this.load();
      await this.check(handle);
      const refused = await this.deps.eligibility(platform);
      await this.check(handle);
      if (refused) {
        this.active.delete(platform);
        return { ok: false, code: refused };
      }
      const syncing = await this.deps.sync.syncing();
      await this.check(handle);
      if (syncing[platform]) {
        this.active.delete(platform);
        return { ok: false, code: 'busy' };
      }
      const sources = await this.deps.sources(handle.pairing);
      await this.check(handle);
      const steps = buildSyncSteps(target, sources);
      if (!steps.length) {
        this.active.delete(platform);
        return { ok: false, code: 'no_sources' };
      }
      if (handle.stopped) {
        this.active.delete(platform);
        return { ok: false, code: 'cancelled' };
      }
      const previousWindowId = this.jobs[platform]?.windowId ?? null;
      this.jobs[platform] = {
        platform,
        accountKey: handle.pairing?.tokenId,
        status: 'navigating',
        step: 0,
        total: steps.length,
        skipped: 0,
        code: null,
        tabId: null,
        windowId: previousWindowId,
        startedAt: this.deps.now(),
      };
      await this.patch(platform, {});
      await this.check(handle);
      void this.run(platform, steps, handle, options.trigger ?? 'web').catch((err: unknown) =>
        this.deps.log('planner', err),
      );
      return { ok: true };
    } catch (error) {
      this.active.delete(platform);
      this.deps.log('planner start', error);
      return { ok: false, code: error instanceof StepError ? error.code : 'sources_unavailable' };
    }
  }
  async stop(platform: Platform): Promise<ExternalAnswer> {
    const handle = this.active.get(platform);
    if (handle) handle.stopped = true;
    await this.deps.sync.stopPlatform(platform);
    return { ok: true };
  }
  async startAll(
    trigger: 'web' | 'scheduled' = 'web',
    minimized = false,
  ): Promise<ExternalAnswer[]> {
    // Each platform has its own visible window and one active run; an error or
    // login wall on one platform never stops the others.
    return Promise.all(
      PLATFORMS.map((platform) => this.start({ platform }, { trigger, minimized })),
    );
  }
  /** P2-17's only backlog seam. The caller must pass an already-open IG tab;
   * this method never creates a task tab/window. A full walk ignores both the
   * incremental known-run stop and the old resume cursor, once per local day. */
  async requestInstagramBacklogSync({
    backlog,
    tabId,
  }: {
    backlog: number;
    tabId: number;
  }): Promise<ExternalAnswer> {
    if (!Number.isSafeInteger(backlog) || backlog < 200)
      return { ok: false, code: 'below_threshold' };
    const tab = await this.deps.browser.get(tabId).catch(() => null);
    if (!tab?.url || platformForUrl(tab.url) !== 'instagram')
      return { ok: false, code: 'no_instagram_tab' };
    const day = new Date(this.deps.now()).toDateString();
    const accountKey = (await this.deps.accountKey?.()) ?? null;
    if (this.deps.accountKey && !accountKey) return { ok: false, code: 'not_paired' };
    const previous = (await this.deps.storage.get(INSTAGRAM_BACKLOG_DAY_KEY))[
      INSTAGRAM_BACKLOG_DAY_KEY
    ];
    if (
      typeof previous === 'object' &&
      previous !== null &&
      'accountKey' in previous &&
      previous.accountKey === accountKey &&
      'day' in previous &&
      previous.day === day
    )
      return { ok: false, code: 'daily_limit' };
    const answer = await this.start(
      { platform: 'instagram' },
      { trigger: 'scheduled', full: true, existingTabId: tabId },
    );
    if (answer.ok && (!this.deps.accountKey || (await this.deps.accountKey()) === accountKey))
      await this.deps.storage.set({ [INSTAGRAM_BACKLOG_DAY_KEY]: { accountKey, day } });
    return answer;
  }
  /** Worker restart: an interrupted plan never resumes an unattended walk. */
  async recover(): Promise<void> {
    await this.load();
    for (const platform of PLATFORMS) {
      const job = this.jobs[platform];
      if (job && ['navigating', 'syncing'].includes(job.status) && !this.active.has(platform)) {
        await this.deps.sync.stopPlatform(platform, job.accountKey);
        await this.patch(platform, { status: 'stopped', code: 'worker_restarted' });
      }
    }
  }
  private async check(handle: RunHandle): Promise<void> {
    if (handle.stopped) throw new StepError('cancelled');
    if (handle.pairing && this.deps.pairing) {
      const current = await this.deps.pairing();
      if (
        !current ||
        current.tokenId !== handle.pairing.tokenId ||
        current.token !== handle.pairing.token
      )
        throw new StepError('cancelled');
    }
  }
  private async until<T>(
    handle: RunHandle,
    timeout: number,
    read: () => Promise<T | null>,
  ): Promise<T | null> {
    const start = this.deps.now();
    for (;;) {
      await this.check(handle);
      const result = await read();
      await this.check(handle);
      if (result) return result;
      if (this.deps.now() - start >= timeout) return null;
      await this.deps.sleep(500);
    }
  }
  private async navigate(
    platform: Platform,
    tabId: number,
    url: string,
    handle: RunHandle,
  ): Promise<void> {
    await this.check(handle);
    await this.deps.browser.navigate(tabId, url).catch(() => undefined);
    const settled = await this.until(handle, NAVIGATION_TIMEOUT_MS, async () => {
      const tab = await this.deps.browser.get(tabId).catch(() => null);
      if (tab?.url && LOGIN_PATTERNS[platform].test(tab.url)) throw new StepError('login_required');
      if (!tab?.url || tab.status === 'loading') return null;
      return tab.url.replace(/\/$/, '') === url.replace(/\/$/, '') ? true : null;
    });
    if (!settled) throw new StepError('navigation');
  }
  private async bridge(platform: Platform, tabId: number, handle: RunHandle): Promise<void> {
    for (let attempt = 0; attempt < 2; attempt++) {
      const ready = await this.until(handle, BRIDGE_TIMEOUT_MS, async () => {
        const tab = await this.deps.browser.get(tabId);
        await this.check(handle);
        if (tab.url && LOGIN_PATTERNS[platform].test(tab.url))
          throw new StepError('login_required');
        const pong = await this.deps.browser.ping(tabId).catch(() => null);
        return pong?.ok === true ? pong : null;
      });
      if (ready?.ok) return;
      if (attempt === 0) {
        await this.check(handle);
        await this.deps.browser.reload(tabId);
      }
    }
    throw new StepError('bridge');
  }
  private async run(
    platform: Platform,
    steps: SyncStep[],
    handle: RunHandle,
    trigger: 'web' | 'scheduled',
  ): Promise<void> {
    try {
      await this.check(handle);
      const window =
        handle.existingTabId !== undefined
          ? await this.deps.browser.existing(handle.existingTabId)
          : await this.deps.browser.open(
              platform,
              this.jobs[platform]?.windowId ?? null,
              handle.minimized,
            );
      await this.check(handle);
      await this.patch(platform, { ...window });
      await this.check(handle);
      const tabId = window.tabId;
      let username = '';
      let folders: string[] = [];
      if (platform === 'instagram') {
        await this.navigate(platform, tabId, 'https://www.instagram.com/', handle);
        const viewer = await this.deps.browser.instagramUsername(tabId);
        await this.check(handle);
        if (viewer.login) throw new StepError('login_required');
        if (!viewer.username) throw new StepError('navigation');
        username = viewer.username;
        if (steps.some((step) => step.kind === 'ig-folder')) {
          await this.navigate(
            platform,
            tabId,
            `https://www.instagram.com/${username}/saved/`,
            handle,
          );
          for (let attempt = 0; attempt < 12; attempt++) {
            await this.check(handle);
            folders = await this.deps.browser.folderLinks(tabId);
            await this.check(handle);
            if (
              steps
                .filter((step) => step.kind === 'ig-folder')
                .every((step) =>
                  folders.some(
                    (href) => syncTarget('instagram', href)?.externalId === step.externalId,
                  ),
                )
            )
              break;
            await this.deps.sleep(500);
          }
        }
      }
      for (let i = 0; i < steps.length; i++) {
        await this.check(handle);
        const stepStartedAt = this.deps.now();
        const step = steps[i];
        await this.patch(platform, { step: i + 1, status: 'navigating', code: null });
        try {
          const refused = await this.deps.eligibility(platform);
          await this.check(handle);
          if (refused) {
            await this.patch(platform, { status: 'error', code: refused });
            return;
          }
          const url =
            step.kind === 'ig-all'
              ? `https://www.instagram.com/${username}/saved/all-posts/`
              : step.kind === 'x-bookmarks'
                ? 'https://x.com/i/bookmarks'
                : step.kind === 'pin-board'
                  ? `https://www.pinterest.com/${step.externalId}/`
                  : folders.find(
                      (href) =>
                        syncTarget('instagram', href)?.externalId === step.externalId &&
                        new URL(href).pathname.startsWith(`/${username}/saved/`),
                    );
          if (!url) throw new StepError('folder_missing');
          await this.navigate(platform, tabId, url, handle);
          await this.bridge(platform, tabId, handle);
          await this.check(handle);
          const started = await this.deps.sync.start({
            tabId,
            trigger,
            collection: step.collection,
            name: step.name,
            full: handle.full,
            expectedPairing: handle.pairing,
          });
          await this.check(handle);
          if (!started.ok) {
            if (['not_paired', 'disabled', 'outdated', 'busy'].includes(started.code)) {
              await this.patch(platform, { status: 'error', code: started.code });
              return;
            }
            throw new StepError('start');
          }
          await this.patch(platform, { status: 'syncing' });
          const remaining = Math.max(0, STEP_TIMEOUT_MS - (this.deps.now() - stepStartedAt));
          const ended = await this.until(handle, remaining, async () => {
            const view = (await this.deps.sync.views()).find(
              (view) => view.runId === started.runId,
            );
            return view?.state === 'ended' ? view : null;
          });
          if (!ended) {
            await this.deps.sync.stopPlatform(platform, handle.pairing?.tokenId);
            throw new StepError('step_timeout');
          }
          if (ended.stopReason === 'login_required' || ended.errorCode === 'login_required')
            throw new StepError('login_required');
          if (ended.stopReason === 'error' || ended.errorCode) throw new StepError('start');
        } catch (error) {
          const code = error instanceof StepError ? error.code : 'navigation';
          const action = stepErrorAction(code);
          if (action !== 'skip') throw new StepError(code);
          await this.patch(platform, { skipped: (this.jobs[platform]?.skipped ?? 0) + 1, code });
        }
      }
      await this.patch(platform, { status: 'done' });
    } catch (error) {
      const code = error instanceof StepError ? error.code : 'navigation';
      await this.deps.sync.stopPlatform(platform, handle.pairing?.tokenId).catch(() => undefined);
      await this.patch(platform, { status: code === 'cancelled' ? 'stopped' : 'error', code });
    } finally {
      this.active.delete(platform);
      await this.deps.afterSync?.().catch((error: unknown) => this.deps.log('after sync', error));
    }
  }
}
