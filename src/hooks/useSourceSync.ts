import { useEffect, useRef, useState } from 'react';
import { SAVED_PATTERNS, LOGIN_PATTERNS, IG_SAVED_URL_KEY, isAllowedUrl } from '../lib/browserUrls';
import { IG_GET_USERNAME, findIgFolderHref } from '../lib/browserScripts';

// Background "source sync" launched from the Library sidebar: drives the
// always-mounted Browser webviews to the right listing page (IG all-posts / IG
// saved folder / X bookmarks / Pinterest board) and runs the existing
// startSync/intercept pipeline there, with no Browser view on screen.
//
// A run is a SEQUENCE of steps (platform-level sync = all-posts + every native
// folder, one at a time; Pinterest = every board) and is interruptible at any
// point via stop(). One run per platform at a time — the underlying sync
// machinery (syncing flags, collection target, counters) is per-tab.
//
// Job state (one per platform) is reported up to App, which feeds it to the
// Activity Center (live progress + completion/error log) and to the sidebar
// buttons (spinner on the row being synced).

// The platforms this hook can drive. Mirrors the Browser's webview tabs.
export type SyncPlatform = 'instagram' | 'twitter' | 'pinterest';

// The Electron <webview> guest surface this hook touches. Declared structurally
// so the hook doesn't depend on the Browser view's local element type.
export interface SyncWebview {
  getURL(): string;
  loadURL(url: string): Promise<void>;
  reload(): void;
  executeJavaScript(code: string): Promise<unknown>;
}

// The hook only reads `.current`, so the container is covariant on the (wider)
// element type the Browser actually stores — `readonly` keeps it assignable.
type WebviewRef = { readonly current: SyncWebview | null };

// Per-tab counters accumulated by the in-page sync (scanned/fresh).
export interface SyncCounts {
  scanned: number;
  fresh: number;
}

// One planned step of a run. ig-all / tw-bookmarks carry no collection; folder
// and board steps target a specific collection.
export type SyncStep =
  | { type: 'ig-all' }
  | { type: 'tw-bookmarks' }
  | { type: 'ig-folder'; collection: Shelfy.Collection }
  | { type: 'pin-board'; collection: Shelfy.Collection };

// The job-status state machine reported per platform.
export type SyncJobStatus = 'navigating' | 'syncing' | 'done' | 'stopped' | 'error';

export interface SyncJob {
  platform: SyncPlatform;
  status: SyncJobStatus;
  error: string | null;
  stepIndex: number;
  stepCount: number;
  currentLabel: string | null;
  currentCollectionId: number | null;
  scanned: number;
  fresh: number;
  skipped: string[];
  startedAt: number;
  finishedAt: number | null;
}

// A request to start a per-platform background sync from a sidebar row.
export interface SyncTarget {
  platform?: SyncPlatform;
  type?: 'collection' | 'platform';
  collectionId?: number;
}

// The imperative API this hook registers up to App.
export interface SourceSyncApi {
  start: (target: SyncTarget) => boolean;
  stop: (platform: SyncPlatform) => void;
  dismiss: (platform: SyncPlatform) => void;
}

// While a run is in flight: an abort flag the async loop polls.
interface RunHandle {
  aborted: boolean;
}

// Per-platform error code thrown by a step. 'login' aborts the whole run.
type StepErrorCode = 'nav' | 'login' | 'aborted' | 'scripts' | 'start' | 'not-found';

interface StepError extends Error {
  code: StepErrorCode;
}

export interface UseSourceSyncOptions {
  webviewRefs: Partial<Record<SyncPlatform, WebviewRef>>;
  syncingRef: { readonly current: Partial<Record<SyncPlatform, boolean>> };
  startSync: (platform?: SyncPlatform, collectionId?: number | null) => boolean;
  stopSync: (platform?: SyncPlatform) => void;
  syncScriptPromiseRefs?: Partial<
    Record<SyncPlatform, { readonly current: Promise<unknown> | null }>
  >;
  syncCountsRefs?: Partial<Record<SyncPlatform, { readonly current: SyncCounts | null }>>;
  scriptsStatus: string;
  injectedForLoadRefs: Partial<Record<SyncPlatform, { readonly current: boolean }>>;
  syncScanned: Partial<Record<SyncPlatform, number>>;
  syncNew: Partial<Record<SyncPlatform, number>>;
  collections: Shelfy.Collection[];
  registerApi?: (api: SourceSyncApi | null) => void;
  onJobsChange?: (jobs: Record<string, SyncJob>) => void;
}

export interface UseSourceSyncResult {
  jobs: Record<string, SyncJob>;
  runRef: { current: Record<string, RunHandle> };
  stop: (platform: SyncPlatform) => void;
}

const TW_BOOKMARKS_URL = 'https://x.com/i/bookmarks';

// Error taxonomy for a failed step. 'login' aborts the whole run (every later
// step needs the same session); the others skip just that step.
const err = (code: StepErrorCode): StepError =>
  Object.assign(new Error(code), { code }) as StepError;

// Pure step planner, exported for tests. A native source is a collection that
// carries the platform's own folder/board id (externalId); custom folders
// (platform == null) and "all posts" are never syncable.
export function buildSyncSteps(
  target: SyncTarget | null | undefined,
  collections: Shelfy.Collection[] = [],
): SyncStep[] {
  const native = (p: SyncPlatform): Shelfy.Collection[] =>
    collections.filter((c) => c.platform === p && c.externalId != null);
  if (target?.type === 'collection') {
    const c = collections.find((x) => x.id === target.collectionId);
    if (!c || c.externalId == null) return [];
    if (c.platform === 'instagram') return [{ type: 'ig-folder', collection: c }];
    if (c.platform === 'pinterest') return [{ type: 'pin-board', collection: c }];
    return [];
  }
  switch (target?.platform) {
    case 'instagram':
      return [
        { type: 'ig-all' },
        ...native('instagram').map((c): SyncStep => ({ type: 'ig-folder', collection: c })),
      ];
    case 'twitter':
      return [{ type: 'tw-bookmarks' }];
    case 'pinterest':
      return native('pinterest').map((c): SyncStep => ({ type: 'pin-board', collection: c }));
    default:
      return [];
  }
}

export default function useSourceSync({
  webviewRefs,
  syncingRef,
  startSync,
  stopSync,
  syncScriptPromiseRefs,
  syncCountsRefs,
  scriptsStatus,
  injectedForLoadRefs,
  syncScanned,
  syncNew,
  collections,
  registerApi,
  onJobsChange,
}: UseSourceSyncOptions): UseSourceSyncResult {
  const [jobs, setJobs] = useState<Record<string, SyncJob>>({}); // platform → job
  const runRef = useRef<Record<string, RunHandle>>({}); // platform → { aborted } while a run is in flight
  const accumRef = useRef<Record<string, SyncCounts>>({}); // platform → counters accumulated over FINISHED steps
  const igUserRef = useRef<string>(''); // cached IG username (cleared on login failure)

  // Mirrors so the async run loop (mount-once closures) reads live values.
  const scriptsStatusRef = useRef<string>(scriptsStatus);
  useEffect(() => {
    scriptsStatusRef.current = scriptsStatus;
  }, [scriptsStatus]);
  const collectionsRef = useRef<Shelfy.Collection[]>(collections);
  useEffect(() => {
    collectionsRef.current = collections;
  }, [collections]);
  const onJobsChangeRef = useRef<UseSourceSyncOptions['onJobsChange']>(onJobsChange);
  useEffect(() => {
    onJobsChangeRef.current = onJobsChange;
  }, [onJobsChange]);

  useEffect(() => {
    onJobsChangeRef.current?.(jobs);
  }, [jobs]);

  // Live counters: while a step is syncing, the job shows the run total =
  // accumulated finished steps + the current step's per-tab counters.
  useEffect(() => {
    setJobs((prev) => {
      let changed = false;
      const next = { ...prev };
      for (const p of Object.keys(prev)) {
        const j = prev[p];
        if (!j || j.status !== 'syncing') continue;
        const acc = accumRef.current[p] || { scanned: 0, fresh: 0 };
        const scanned = acc.scanned + (syncScanned[p as SyncPlatform] || 0);
        const fresh = acc.fresh + (syncNew[p as SyncPlatform] || 0);
        if (scanned !== j.scanned || fresh !== j.fresh) {
          next[p] = { ...j, scanned, fresh };
          changed = true;
        }
      }
      return changed ? next : prev;
    });
  }, [syncScanned, syncNew]);

  const patchJob = (platform: SyncPlatform, patch: Partial<SyncJob>): void =>
    setJobs((prev) =>
      prev[platform] ? { ...prev, [platform]: { ...prev[platform], ...patch } } : prev,
    );

  const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms));
  const waitFor = async <T>(
    fn: () => T,
    { timeout = 15000, interval = 300 }: { timeout?: number; interval?: number } = {},
  ): Promise<T | null> => {
    const t0 = Date.now();
    for (;;) {
      const v = fn();
      if (v) return v;
      if (Date.now() - t0 >= timeout) return null;
      await sleep(interval);
    }
  };
  const getUrl = (wv: SyncWebview): string => {
    try {
      return wv.getURL();
    } catch {
      return '';
    }
  };

  // Loads `url` (if not already there) and waits until the webview settles on a
  // URL matching `expected` — or on the platform's login page, which means the
  // session expired. The settle loop, not loadURL's promise, is the arbiter:
  // these SPAs routinely redirect client-side after the load "finishes".
  const navigateTo = async (
    platform: SyncPlatform,
    url: string,
    run: RunHandle,
    expected: RegExp,
  ): Promise<void> => {
    const wv = webviewRefs[platform]?.current;
    if (!wv || !isAllowedUrl(platform, url)) throw err('nav');
    if (getUrl(wv) !== url) {
      try {
        await wv.loadURL(url);
      } catch {
        /* ERR_ABORTED on SPA redirects is routine — judged by the settle loop */
      }
    }
    const landed = await waitFor<'aborted' | 'login' | 'ok' | null>(
      () => {
        if (run.aborted) return 'aborted';
        const u = getUrl(wv);
        if (LOGIN_PATTERNS[platform]?.test(u)) return 'login';
        if (expected.test(u)) return 'ok';
        return null;
      },
      { timeout: 45000, interval: 400 },
    );
    if (landed === 'aborted' || run.aborted) throw err('aborted');
    if (landed === 'login') throw err('login');
    if (landed !== 'ok') throw err('nav');
  };

  // A sync can only capture once the MAIN-world hook is installed for the
  // current load. The script string is fetched async at mount: if both
  // dom-ready and did-finish-load fired before it was available, this load has
  // no hook — one reload re-runs the injection path now that the string exists.
  const ensureCapture = async (platform: SyncPlatform, run: RunHandle): Promise<void> => {
    const ready = await waitFor(() => run.aborted || scriptsStatusRef.current === 'ready', {
      timeout: 20000,
    });
    if (run.aborted) throw err('aborted');
    if (!ready) throw err('scripts');
    const injectedNow = (): boolean =>
      run.aborted || injectedForLoadRefs[platform]?.current === true;
    let injected = await waitFor(injectedNow, { timeout: 8000, interval: 250 });
    if (run.aborted) throw err('aborted');
    if (!injected) {
      const wv = webviewRefs[platform]?.current;
      if (!wv) throw err('nav');
      try {
        wv.reload();
      } catch {
        /* judged by the wait below */
      }
      injected = await waitFor(injectedNow, { timeout: 20000 });
      if (run.aborted) throw err('aborted');
      if (!injected) throw err('scripts');
    }
  };

  // Starts the per-tab sync toward `collectionId` and waits for it to finish.
  // A manual sync left running on that tab is taken over (the user's Library
  // click is the most recent intent). The 35-minute cap is a safety net above
  // the scroll scripts' own 30-minute ceiling.
  const beginStep = async (
    platform: SyncPlatform,
    collectionId: number | null,
    run: RunHandle,
  ): Promise<void> => {
    await ensureCapture(platform, run);
    // Takeover: stop the previous sync and wait for its in-page scripts to
    // actually settle (i.e. observe __syncStop), not a guessed sleep — a still
    // running scroll loop would double-produce batches into the new step. The
    // race caps the wait in case the webview is wedged mid-script.
    if (syncingRef.current[platform]) stopSync(platform);
    const prior = syncScriptPromiseRefs?.[platform]?.current;
    if (prior) await Promise.race([prior, sleep(10000)]);
    if (run.aborted) throw err('aborted');
    if (!startSync(platform, collectionId)) throw err('start');
    patchJob(platform, { status: 'syncing' });
    await waitFor(() => run.aborted || !syncingRef.current[platform], {
      timeout: 35 * 60 * 1000,
      interval: 500,
    });
    if (syncingRef.current[platform]) stopSync(platform); // abort or timeout: tear down
  };

  const resolveIgUsername = async (run: RunHandle): Promise<string> => {
    if (igUserRef.current) return igUserRef.current;
    // Cheapest source: the persisted saved-page URL (anyone who imported a
    // folder has been there). Fallback: ask IG itself from the page origin.
    const saved = localStorage.getItem(IG_SAVED_URL_KEY) || '';
    const m = /instagram\.com\/([^/?#]+)\/saved(?:\/|$)/.exec(saved);
    if (m && m[1]) {
      igUserRef.current = m[1];
      return m[1];
    }
    const wv = webviewRefs.instagram?.current;
    if (!wv) throw err('nav');
    if (!/instagram\.com/.test(getUrl(wv))) {
      await navigateTo('instagram', 'https://www.instagram.com/', run, /instagram\.com/);
    }
    let user = '';
    try {
      user = String((await wv.executeJavaScript(IG_GET_USERNAME)) || '').trim();
    } catch {
      user = '';
    }
    if (run.aborted) throw err('aborted');
    if (!user) throw err('login');
    igUserRef.current = user;
    return user;
  };

  const runStep = async (platform: SyncPlatform, step: SyncStep, run: RunHandle): Promise<void> => {
    if (step.type === 'tw-bookmarks') {
      await navigateTo('twitter', TW_BOOKMARKS_URL, run, SAVED_PATTERNS.twitter);
      await beginStep('twitter', null, run);
      return;
    }
    if (step.type === 'pin-board') {
      // externalId is "<user>/<board-slug>" — exactly the board's URL path.
      const extId = String(step.collection.externalId || '');
      const path = extId.split('/').filter(Boolean).map(encodeURIComponent).join('/');
      await navigateTo(
        'pinterest',
        `https://www.pinterest.com/${path}/`,
        run,
        SAVED_PATTERNS.pinterest,
      );
      await beginStep('pinterest', step.collection.id, run);
      return;
    }
    const user = await resolveIgUsername(run);
    const base = `https://www.instagram.com/${encodeURIComponent(user)}/saved/`;
    if (step.type === 'ig-all') {
      await navigateTo('instagram', `${base}all-posts/`, run, SAVED_PATTERNS.instagram);
      await beginStep('instagram', null, run);
      return;
    }
    // ig-folder: collections persist only the rename-safe numeric folder id, not
    // the URL slug — discover the folder's real href from the saved index page.
    await navigateTo('instagram', base, run, SAVED_PATTERNS.instagram);
    const wv = webviewRefs.instagram?.current;
    const href = wv ? await findIgFolderHref(wv, step.collection.externalId) : null;
    if (run.aborted) throw err('aborted');
    if (!href) throw err('not-found');
    const folderUrl = new URL(href, 'https://www.instagram.com/').toString();
    await navigateTo('instagram', folderUrl, run, SAVED_PATTERNS.instagram);
    await beginStep('instagram', step.collection.id, run);
  };

  const start = (target: SyncTarget): boolean => {
    const platform = target?.platform;
    if (!platform || !webviewRefs[platform]) return false;
    if (runRef.current[platform]) return false; // one run per platform
    const steps = buildSyncSteps(target, collectionsRef.current);
    if (!steps.length) return false;
    const run: RunHandle = { aborted: false };
    runRef.current[platform] = run;
    accumRef.current[platform] = { scanned: 0, fresh: 0 };
    const firstStep = steps[0];
    const firstCollection = 'collection' in firstStep ? firstStep.collection : null;
    setJobs((prev) => ({
      ...prev,
      [platform]: {
        platform,
        status: 'navigating',
        error: null,
        stepIndex: 0,
        stepCount: steps.length,
        currentLabel: firstCollection?.name ?? null,
        currentCollectionId: firstCollection?.id ?? null,
        scanned: 0,
        fresh: 0,
        skipped: [],
        startedAt: Date.now(),
        finishedAt: null,
      },
    }));
    (async () => {
      const skipped: string[] = [];
      let loginError = false;
      for (let i = 0; i < steps.length && !run.aborted; i++) {
        const step = steps[i];
        const stepCollection = 'collection' in step ? step.collection : null;
        patchJob(platform, {
          status: 'navigating',
          stepIndex: i,
          currentLabel: stepCollection?.name ?? null,
          currentCollectionId: stepCollection?.id ?? null,
        });
        try {
          await runStep(platform, step, run);
        } catch (e) {
          const code = (e as Partial<StepError>)?.code;
          if (code === 'aborted') break;
          if (code === 'login') {
            // A stale cached username can also land here — drop it so the next
            // attempt re-resolves instead of failing forever.
            igUserRef.current = '';
            loginError = true;
            break;
          }
          skipped.push(stepCollection?.name ?? step.type);
          continue; // pre-start failure: counters never ran for this step
        }
        // The step ran (possibly partially, on abort): fold its counters in.
        // ingestBatch counts `scanned` synchronously (before its IPC save), so the
        // run total includes the terminal page even though that page's save may still
        // be settling when syncingRef flips off. (`fresh` trails by the save.)
        const acc = accumRef.current[platform];
        const counts = syncCountsRefs?.[platform]?.current;
        acc.scanned += counts?.scanned || 0;
        acc.fresh += counts?.fresh || 0;
      }
      delete runRef.current[platform];
      const acc = accumRef.current[platform] || { scanned: 0, fresh: 0 };
      const failedAll = !run.aborted && !loginError && skipped.length >= steps.length;
      const status: SyncJobStatus = run.aborted
        ? 'stopped'
        : loginError || failedAll
          ? 'error'
          : 'done';
      patchJob(platform, {
        status,
        error: status === 'error' ? (loginError ? 'login' : 'failed') : null,
        scanned: acc.scanned,
        fresh: acc.fresh,
        skipped,
        currentLabel: null,
        currentCollectionId: null,
        finishedAt: Date.now(),
      });
      // done/stopped jobs leave the map shortly after the Activity log effect
      // has seen the terminal state, so later MANUAL browser syncs fall back to
      // the legacy logging path. Error jobs persist as a live row (with the
      // login CTA) until dismissed or replaced by a new run.
      if (status !== 'error') {
        setTimeout(() => {
          setJobs((prev) => {
            const j = prev[platform];
            if (!j || (j.status !== 'done' && j.status !== 'stopped')) return prev;
            const next = { ...prev };
            delete next[platform];
            return next;
          });
        }, 4000);
      }
    })();
    return true;
  };

  const stop = (platform: SyncPlatform): void => {
    const run = runRef.current[platform];
    if (run) run.aborted = true;
    if (syncingRef.current[platform]) stopSync(platform);
  };

  const dismiss = (platform: SyncPlatform): void =>
    setJobs((prev) => {
      if (!prev[platform] || runRef.current[platform]) return prev;
      const next = { ...prev };
      delete next[platform];
      return next;
    });

  // Imperative API for App (sidebar buttons + Activity actions). Registered
  // once: start/stop/dismiss read only refs and stable setters, and the
  // captured startSync/stopSync are the first-render instances, which — like
  // everywhere else in the Browser hooks — read only refs/stable setters too.
  useEffect(() => {
    registerApi?.({ start, stop, dismiss });
    return () => registerApi?.(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return { jobs, runRef, stop };
}
