import React, {
  useState,
  useEffect,
  useCallback,
  useRef,
  createContext,
  useContext,
  createElement,
  type ReactNode,
} from 'react';
import type { QueuedResult } from '../../types/electron-api';

// The analyzer's runtime job record (analyze:getJobs → onAnalyzeProgress). It has
// no Shelfy.* domain type (it's an analyzer-internal shape, not the persisted
// `jobs` row — see types/electron-api.d.ts), so the fields actually read across
// the app are described here. Display-only; the backend owns the authoritative
// state. Bridge channels return these as `unknown`, narrowed at the boundary.
export interface AnalyzeJob {
  key: string;
  postId?: string;
  platform?: Shelfy.Platform;
  status: 'pending' | 'extracting' | 'analyzing' | 'done' | 'error' | 'cancelled';
  error?: string | null;
  authorUsername?: string | null;
  description?: string | null;
  tags?: string[] | null;
  stage?: string | null;
  mediaType?: Shelfy.MediaType | null;
  model?: string | null;
  thumbnailPath?: string | null;
  imagePath?: string | null;
  thumbnailUrl?: string | null;
  streamText?: string | null;
  startedAt?: number | null;
  finishedAt?: number | null;
  queuedAt?: number | null;
  durationMs?: number | null;
  phaseProgress?: number | null;
}

// VLM model status (getModelStatus → analyzer-internal shape, `unknown` on the
// bridge). Only these fields are read across the app.
export interface ModelStatus {
  ready?: boolean;
  downloading?: boolean;
  files?: unknown;
  name?: string;
  [key: string]: unknown;
}

// Model-download progress event (onModelProgress payload is `unknown`).
export interface ModelProgress {
  progress: number;
  label?: string;
  [key: string]: unknown;
}

// The at-a-glance reducer output shared by the queue view and activity strip.
export interface AnalysisSummary {
  activeJobs: AnalyzeJob[];
  doneJobs: AnalyzeJob[];
  errorJobs: AnalyzeJob[];
  running: AnalyzeJob | null;
  runningJobs: AnalyzeJob[];
  concurrency: number;
  active: boolean;
  done: number;
  total: number;
  current: string;
  remaining: number;
  lastDurationMs: number | null;
  avgDurationMs: number | null;
  etaMs: number | null;
}

// The shared analysis instance shape (provider value / standalone fallback).
export interface AnalysisInstance {
  jobs: AnalyzeJob[];
  jobFor: (postId: string) => AnalyzeJob | null;
  modelStatus: ModelStatus | null;
  modelProgress: ModelProgress | null;
  isPaused: boolean;
  concurrency: number;
  refreshModel: () => Promise<ModelStatus | null>;
  analyzePost: (postId: string) => Promise<QueuedResult>;
  analyzeAll: () => Promise<QueuedResult>;
  cancelJob: (key: string) => Promise<unknown>;
  cancelAll: () => Promise<unknown>;
  clearAll: () => Promise<void>;
  clearCompleted: () => Promise<void>;
  pauseAll: () => Promise<void>;
  resumeAll: () => Promise<void>;
  retryJob: (key: string) => Promise<unknown>;
  downloadModel: () => Promise<void>;
  updatePostAiAnalysis: (id: string, fields: unknown) => Promise<void>;
  updatePostUserContent: (
    id: string,
    fields: { note?: string | null; manualTags?: string[] | null },
  ) => Promise<void>;
  clearPostDescriptions: (ids: string[]) => Promise<number>;
}

// Narrow an unknown bridge value to an AnalyzeJob (must carry a string `key`).
function asAnalyzeJob(v: unknown): AnalyzeJob | null {
  if (v && typeof v === 'object' && typeof (v as { key?: unknown }).key === 'string') {
    return v as AnalyzeJob;
  }
  return null;
}

// Narrow an unknown bridge list to AnalyzeJob[].
function asAnalyzeJobs(v: unknown): AnalyzeJob[] {
  if (!Array.isArray(v)) return [];
  const out: AnalyzeJob[] = [];
  for (const item of v) {
    const job = asAnalyzeJob(item);
    if (job) out.push(job);
  }
  return out;
}

// Narrow an unknown bridge value to a ModelStatus.
function asModelStatus(v: unknown): ModelStatus | null {
  return v && typeof v === 'object' ? (v as ModelStatus) : null;
}

// Narrow an onModelProgress payload to a ModelProgress.
function asModelProgress(v: unknown): ModelProgress | null {
  if (v && typeof v === 'object' && typeof (v as { progress?: unknown }).progress === 'number') {
    return v as ModelProgress;
  }
  return null;
}

// Snapshot recorded when a job key is forgotten by clearAll.
interface ClearedKeyInfo {
  stamp: number;
  queuedAt: number | null;
}

// The shared analysis state, populated by AnalysisProvider. Null when no provider
// is mounted (e.g. a standalone renderHook in tests) — consumers fall back to a
// local standalone instance in that case.
const AnalysisContext = createContext<AnalysisInstance | null>(null);

// analyze:progress is throttled per job in the main process, but N parallel
// slots still multiply it into a steady event stream; publishing the derived
// array on every event re-renders every consumer of the analysis context.
// Events are upserted into a Map immediately and published with a leading +
// trailing throttle (same policy as useDownloads/useWebJobs).
const FLUSH_WINDOW = 100;

// Core analysis logic: per-post job state, plus first-run model download.
// Used both by the provider (shared, single subscription) and as a standalone
// fallback for consumers without a provider.
function useAnalysisStandalone(enabled = true): AnalysisInstance {
  const [jobs, setJobs] = useState<AnalyzeJob[]>([]);
  const [modelStatus, setModelStatus] = useState<ModelStatus | null>(null); // { ready, downloading, files, name }
  const [modelProgress, setModelProgress] = useState<ModelProgress | null>(null); // { progress, label } during download
  const [isPaused, setIsPaused] = useState(false); // queue paused (mirrors main process)
  const [concurrency, setConcurrency] = useState(1); // parallel inference slots (mirrors main process)

  // Keys forgotten by the last clearAll. A progress event for one of THESE keys
  // that arrives after a clear is a stale emit that was already in flight over IPC
  // when the backend dropped the job — we drop it instead of re-inserting a zombie
  // row the backend will never finish. Snapshotting the exact keys (rather than
  // blanket-dropping every unknown key for a fixed window) lets a genuinely new
  // job re-enqueued right after the clear show its first progress event without
  // delay. Entries auto-expire after CLEAR_GRACE_MS so a later reuse of the same
  // post id isn't suppressed.
  const clearedKeysRef = useRef<Map<string, ClearedKeyInfo>>(new Map()); // key -> stamp
  const CLEAR_GRACE_MS = 1500;

  // While a VLM download is in flight, poll the authoritative status. analyzer's
  // downloadModel emits NO terminal progress=1 on failure/pause/cancel (and a
  // download may be started from the Settings picker, not via our own action), so
  // without this the banner could spin on "Download in corso…" forever. Mirrors
  // useAiSearch.startModelStatusPoll.
  const modelPollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Source of truth for progress updates: a Map keyed by job.key keeps per-event
  // upserts O(1) (instead of findIndex + array copy per event). The `jobs` array
  // is derived from it on each throttled flush; untouched job objects keep their
  // identity across publishes, so memoized consumers skip re-rendering.
  const jobsMapRef = useRef<Map<string, AnalyzeJob>>(new Map());
  const publishJobs = useCallback((): void => {
    setJobs(Array.from(jobsMapRef.current.values()));
  }, []);
  const flushTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lastFlushRef = useRef(0);
  const scheduleFlush = useCallback((): void => {
    if (flushTimerRef.current) return;
    const elapsed = Date.now() - lastFlushRef.current;
    if (elapsed >= FLUSH_WINDOW) {
      lastFlushRef.current = Date.now();
      publishJobs();
      return;
    }
    flushTimerRef.current = setTimeout(() => {
      flushTimerRef.current = null;
      lastFlushRef.current = Date.now();
      publishJobs();
    }, FLUSH_WINDOW - elapsed);
  }, [publishJobs]);
  const syncJobs = useCallback(
    (list: AnalyzeJob[]): void => {
      const map = new Map<string, AnalyzeJob>();
      for (const j of list || []) map.set(j.key, j);
      jobsMapRef.current = map;
      publishJobs();
    },
    [publishJobs],
  );

  const refreshModel = useCallback(async (): Promise<ModelStatus | null> => {
    const s = asModelStatus(await window.electronAPI.getModelStatus());
    setModelStatus(s);
    return s;
  }, []);

  const stopModelPoll = useCallback((): void => {
    if (modelPollRef.current) {
      clearInterval(modelPollRef.current);
      modelPollRef.current = null;
    }
  }, []);
  // Self-healing for a download that ends without a terminal progress=1 (pause /
  // cancel / failure): poll the authoritative status and clear the banner once the
  // backend reports it's no longer downloading.
  const startModelPoll = useCallback((): void => {
    if (modelPollRef.current) return;
    modelPollRef.current = setInterval(async () => {
      const s = await refreshModel();
      if (!s?.downloading) {
        stopModelPoll();
        setModelProgress(null);
      }
    }, 2000);
  }, [refreshModel, stopModelPoll]);

  useEffect(() => {
    // When a provider already owns the shared instance, the fallback copy stays
    // dormant to avoid duplicate IPC subscriptions and divergent state.
    if (!enabled) return undefined;
    window.electronAPI.getAnalyzeStatus().then((j) => syncJobs(asAnalyzeJobs(j)));
    window.electronAPI.getAnalyzeIsPaused?.().then(setIsPaused);
    // Mirror the parallel-slot setting so ETA math matches how the queue actually
    // drains. Refreshed below whenever Settings changes it.
    const readConcurrency = (): void => {
      window.electronAPI
        .getAnalyzeConcurrency?.()
        .then((r) => {
          if (r && typeof r.value === 'number') setConcurrency(Math.max(1, r.value));
        })
        .catch(() => {});
    };
    readConcurrency();
    refreshModel();

    const unsubJob = window.electronAPI.onAnalyzeProgress((raw) => {
      const job = asAnalyzeJob(raw);
      if (!job) return;
      const map = jobsMapRef.current;
      // Stale event for a job the last clearAll just forgot: it was still in
      // flight over IPC when the backend dropped it. Drop ONLY events for keys we
      // actually cleared (within the grace window) so the optimistic empty list
      // isn't undone by a zombie row — a brand-new job re-enqueued right after the
      // clear has a fresh key and surfaces immediately.
      if (!map.has(job.key)) {
        const cleared = clearedKeysRef.current.get(job.key);
        if (cleared != null) {
          // Drop only a late event from the SAME instance we cleared (same queuedAt)
          // and only within the grace window. A job re-enqueued right after the clear
          // reuses the key but gets a FRESH queuedAt, so its early progress events
          // surface immediately instead of being suppressed for the whole grace.
          const sameInstance =
            cleared.queuedAt == null || job.queuedAt == null || job.queuedAt === cleared.queuedAt;
          if (sameInstance && Date.now() - cleared.stamp < CLEAR_GRACE_MS) return;
          clearedKeysRef.current.delete(job.key);
        }
      }
      map.set(job.key, job);
      scheduleFlush();
    });

    const unsubModel = window.electronAPI.onModelProgress((raw) => {
      const p = asModelProgress(raw);
      if (!p) return;
      if (p.progress >= 1) {
        // Terminal success: stop any fallback poll and clear the banner.
        stopModelPoll();
        setModelProgress(null);
        refreshModel();
        return;
      }
      // Reflect download progress immediately (synchronously) so the banner tracks
      // the percentage without waiting on an IPC round-trip.
      setModelProgress(p);
      // Guard against a stuck banner if this download fails/pauses/cancels without
      // ever emitting a terminal progress event: poll the authoritative status and
      // clear the banner once the backend reports it's no longer downloading.
      startModelPoll();
    });

    // The Settings view can switch the active model; re-read its status so the
    // ready/not-ready gating across the app reflects the new selection.
    const onModelChanged = (): void => {
      refreshModel();
    };
    window.addEventListener('ai-model-changed', onModelChanged);

    // Settings can change the parallel-slot count at runtime; re-read it so the
    // ETA reflects the new throughput without a reload.
    const onConcurrencyChanged = (): void => readConcurrency();
    window.addEventListener('ai-concurrency-changed', onConcurrencyChanged);

    return () => {
      if (typeof unsubJob === 'function') unsubJob();
      if (typeof unsubModel === 'function') unsubModel();
      if (flushTimerRef.current) clearTimeout(flushTimerRef.current);
      flushTimerRef.current = null;
      stopModelPoll();
      window.removeEventListener('ai-model-changed', onModelChanged);
      window.removeEventListener('ai-concurrency-changed', onConcurrencyChanged);
    };
  }, [refreshModel, enabled, syncJobs, scheduleFlush, startModelPoll, stopModelPoll]);

  const jobFor = useCallback(
    (postId: string): AnalyzeJob | null => jobs.find((j) => j.key === `${postId}:analyze`) || null,
    [jobs],
  );

  const analyzePost = useCallback(
    (postId: string): Promise<QueuedResult> => window.electronAPI.analyzePost(postId),
    [],
  );
  const analyzeAll = useCallback((): Promise<QueuedResult> => window.electronAPI.analyzeAll(), []);
  const cancelJob = useCallback(
    (key: string): Promise<unknown> => window.electronAPI.cancelAnalyzeJob(key),
    [],
  );
  const cancelAll = useCallback((): Promise<unknown> => window.electronAPI.cancelAllAnalyze(), []);
  const retryJob = useCallback(
    (key: string): Promise<unknown> => window.electronAPI.retryAnalyzeJob(key),
    [],
  );

  // Flip the UI optimistically so the button/badge react instantly, then confirm
  // with the main process. If the IPC fails, roll back so the UI can't lie about
  // a state the backend never reached.
  const pauseAll = useCallback(async (): Promise<void> => {
    setIsPaused(true);
    try {
      await window.electronAPI.pauseAnalyze();
    } catch {
      setIsPaused(false);
    }
  }, []);
  const resumeAll = useCallback(async (): Promise<void> => {
    setIsPaused(false);
    try {
      await window.electronAPI.resumeAnalyze();
    } catch {
      setIsPaused(true);
    }
  }, []);
  // Cancel in-flight/queued work and empty the list (the queue's "clear").
  // Snapshot the keys we're forgetting (with a timestamp) so the progress reducer
  // drops late stale events for THOSE jobs — still in flight over IPC when the
  // backend forgot them — instead of re-adding a zombie row, while letting a
  // brand-new job re-enqueued right after the clear (different key) show through.
  // We snapshot both before and after the await to cover keys upserted meanwhile.
  const clearAll = useCallback(async (): Promise<void> => {
    const stampCleared = (): void => {
      const now = Date.now();
      // Prune entries whose grace already lapsed so the snapshot Map stays bounded.
      for (const [k, info] of clearedKeysRef.current) {
        if (now - info.stamp >= CLEAR_GRACE_MS) clearedKeysRef.current.delete(k);
      }
      // Record each forgotten key WITH the instance's queuedAt, so the reducer can
      // tell a stale late event (same queuedAt) from a re-enqueued job (fresh one).
      for (const [k, job] of jobsMapRef.current) {
        clearedKeysRef.current.set(k, { stamp: now, queuedAt: job?.queuedAt ?? null });
      }
    };
    stampCleared();
    await window.electronAPI.clearAllAnalyze();
    stampCleared();
    jobsMapRef.current = new Map();
    publishJobs();
    setIsPaused(false);
  }, [publishJobs]);

  // Prune only the terminal (done/cancelled) rows, leaving in-flight and errored
  // jobs in place. Mirrors the backend analyzer.clearCompleted, which otherwise
  // grows jobsMap/jobstore/renderer state unbounded across the app's lifetime.
  const clearCompleted = useCallback(async (): Promise<void> => {
    await window.electronAPI.clearCompletedAnalyze?.();
    const map = jobsMapRef.current;
    for (const [k, j] of map) {
      if (j.status === 'done' || j.status === 'cancelled') map.delete(k);
    }
    publishJobs();
  }, [publishJobs]);

  // Persist manual edits to a post's AI fields (description / tags / saveReason).
  const updatePostAiAnalysis = useCallback(
    (id: string, fields: unknown): Promise<void> =>
      window.electronAPI.updatePostAiAnalysis(id, fields),
    [],
  );

  // Persist the user-authored layer (personal note + manual tags), distinct from
  // the AI fields so it survives an analysis regeneration.
  const updatePostUserContent = useCallback(
    (id: string, fields: { note?: string | null; manualTags?: string[] | null }): Promise<void> =>
      window.electronAPI.updatePostUserContent(id, fields),
    [],
  );

  // Delete the AI-generated description for one or more posts (tags kept).
  const clearPostDescriptions = useCallback(
    (ids: string[]): Promise<number> => window.electronAPI.clearPostDescriptions(ids),
    [],
  );

  const downloadModel = useCallback(async (): Promise<void> => {
    setModelProgress({ progress: 0, label: 'modello' });
    try {
      // No model id: the analyzer downloads the currently-selected/default model.
      // The .d.ts (frozen) types `id` as required, but the IPC handler defaults it
      // to undefined, so passing nothing is the intended runtime behavior. Cast to
      // satisfy the required-parameter type without altering the call.
      await (window.electronAPI.downloadModel as () => Promise<unknown>)();
    } finally {
      stopModelPoll();
      setModelProgress(null);
      refreshModel();
    }
  }, [refreshModel, stopModelPoll]);

  return {
    jobs,
    jobFor,
    modelStatus,
    modelProgress,
    isPaused,
    concurrency,
    refreshModel,
    analyzePost,
    analyzeAll,
    cancelJob,
    cancelAll,
    clearAll,
    clearCompleted,
    pauseAll,
    resumeAll,
    retryJob,
    downloadModel,
    updatePostAiAnalysis,
    updatePostUserContent,
    clearPostDescriptions,
  };
}

// Derives the at-a-glance analysis stats shared by the sidebar activity strip and
// the AI Tags queue view from the raw job list: counts, the running job's label,
// and timing (last completed duration + ETA from a rolling average). Pure — kept
// out of components so both surfaces compute identical numbers.
//
// `concurrency` is how many inferences run in parallel: with N slots the queue
// drains ~N× faster, so the ETA divides the remaining work by the slots actually
// in use. Defaults to 1 (serial) so callers that don't pass it stay correct.
const ACTIVE_STATES = ['pending', 'extracting', 'analyzing'];
const RUNNING_STATES = ['extracting', 'analyzing'];
export function analysisSummary(jobs: AnalyzeJob[], concurrency = 1): AnalysisSummary {
  const list = Array.isArray(jobs) ? jobs : [];
  const activeJobs = list.filter((j) => ACTIVE_STATES.includes(j.status));
  const doneJobs = list.filter((j) => j.status === 'done');
  const errorJobs = list.filter((j) => j.status === 'error');
  // Every in-flight job, not just the first — with parallel slots there can be
  // several at once, and each one's elapsed time feeds the live countdown.
  const runningJobs = list.filter((j) => RUNNING_STATES.includes(j.status));
  const running = runningJobs[0] || null;

  const done = doneJobs.length;
  // Errored jobs count toward the denominator so a drained queue with failures
  // never reads as 100% complete (e.g. '7 / 10' when 3 errored, not '7 / 7').
  // The FIFO/ETA math below stays on active + done only.
  const total = activeJobs.length + doneJobs.length + errorJobs.length;
  const current = running ? running.description || running.authorUsername || '' : '';

  // Completed jobs with a measured duration, most-recently-finished first.
  const timed = doneJobs
    .filter(
      (j): j is AnalyzeJob & { durationMs: number } =>
        typeof j.durationMs === 'number' && j.durationMs > 0,
    )
    .sort((a, b) => (b.finishedAt || 0) - (a.finishedAt || 0));
  const lastDurationMs = timed.length ? timed[0].durationMs : null;
  // Average over the last 10 to smooth out per-post variance for a stable ETA.
  const sample = timed.slice(0, 10);
  const avgDurationMs = sample.length
    ? Math.round(sample.reduce((sum, j) => sum + j.durationMs, 0) / sample.length)
    : null;
  const remaining = activeJobs.length; // pending + in-flight
  // Slots actually busy can't exceed the work left: 3 jobs on 10 slots run 3-wide.
  const slots = Math.min(Math.max(1, concurrency || 1), remaining) || 1;
  // Per-job durations already include the slowdown from sharing the GPU across
  // slots, so dividing the serial estimate by the slots in use gives wall-clock.
  const etaMs =
    avgDurationMs != null && remaining > 0 ? Math.round((avgDurationMs * remaining) / slots) : null;

  return {
    activeJobs,
    doneJobs,
    errorJobs,
    running,
    runningJobs,
    concurrency: Math.max(1, concurrency || 1),
    active: activeJobs.length > 0,
    done,
    total,
    current,
    remaining,
    lastDurationMs,
    avgDurationMs,
    etaMs,
  };
}

// Live countdown to an empty queue, parallel-aware. Spreads the work still left —
// each in-flight job's remaining slice (avg minus what it's already run) plus a
// full avg per queued job — across the slots in use. With one serial slot this is
// just `etaMs - elapsed`; with N slots it tracks every in-flight job at once so
// the estimate doesn't jump when slots finish at different times.
export function liveRemainingMs(summary: AnalysisSummary | null, now: number): number | null {
  if (!summary) return null;
  const { avgDurationMs, runningJobs = [], remaining = 0, concurrency = 1 } = summary;
  if (avgDurationMs == null || remaining <= 0) return null;

  const inflight = runningJobs.length;
  const pending = Math.max(0, remaining - inflight);
  let workMs = pending * avgDurationMs;
  for (const j of runningJobs) {
    const elapsed = j.startedAt ? Math.max(0, now - j.startedAt) : 0;
    workMs += Math.max(0, avgDurationMs - elapsed);
  }
  const slots = Math.min(Math.max(1, concurrency || 1), remaining) || 1;
  return Math.max(0, Math.round(workMs / slots));
}

interface AnalysisProviderProps {
  children: ReactNode;
}

// Mounts a single shared analysis instance and exposes it to all descendants,
// so App and PostModal share the same jobs/modelStatus/subscriptions.
export function AnalysisProvider({ children }: AnalysisProviderProps): React.JSX.Element {
  const value = useAnalysisStandalone();
  return createElement(AnalysisContext.Provider, { value }, children);
}

// Drives local VLM analysis. Reads the shared context when a provider is present;
// falls back to a standalone instance otherwise (keeps renderHook tests working).
export function useAnalysis(): AnalysisInstance {
  const ctx = useContext(AnalysisContext);
  // The standalone instance is only active (subscribes/loads) when no provider
  // supplies the shared one — keeps hook order stable without double work.
  const standalone = useAnalysisStandalone(ctx == null);
  return ctx ?? standalone;
}
