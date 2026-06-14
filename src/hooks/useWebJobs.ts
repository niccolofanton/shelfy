import { useState, useEffect, useCallback, useMemo, useRef } from 'react';

// Runtime web-capture job record as pushed by onWebProgress / returned by
// getWebStatus (weborchestrator-internal, not a DB row). IPC types these
// channels as `unknown`, so we describe the fields the hook and its consumers
// read and narrow at the boundary.
export interface WebJobRecord {
  key?: string;
  postId?: string;
  status: string; // 'pending' | 'discovering' | … | 'done' | 'error' | 'cancelled'
  domain?: string;
  url?: string;
  [key: string]: unknown;
}

export interface UseWebJobs {
  jobs: WebJobRecord[];
  cancelJob: (key?: string) => Promise<unknown> | undefined;
  cancelAll: () => Promise<unknown> | undefined;
  retryJob: (key?: string) => Promise<unknown> | undefined;
  clearCompleted: () => Promise<void>;
}

// Tracks the web-reference capture queue (F10 orchestrator). Mirrors useDownloads:
// an initial getWebStatus() snapshot + live web:progress upserts keyed by job key
// (`web:${postId}`). Feeds the Activity center so the user can follow the
// behind-the-scenes phases (discovering → capturing → extracting → analyzing).
const keyOf = (j: WebJobRecord | null | undefined): string | undefined =>
  j?.postId ? `web:${j.postId}` : j?.key;

// Narrow the `unknown` IPC payload to a job record.
const asJob = (data: unknown): WebJobRecord => data as WebJobRecord;

const ACTIVE_STATUS = new Set(['pending', 'discovering', 'capturing', 'extracting', 'analyzing']);

// Progress events stream per-phase per-job; publishing the derived array on
// every one re-renders the whole app tree. Same leading + trailing throttle as
// useDownloads.
const FLUSH_WINDOW = 100;

// done/cancelled jobs already live on as persisted `web` posts (AiWebsites) and
// in the Activity log (the done/error transition is pushed when it happens), so
// the in-memory backlog only needs a bounded recent window. Error jobs are kept:
// they carry the retry action and may have no persisted post yet. Pruning only
// runs while the queue is idle so the done/total badge counters stay honest
// during an active batch.
const MAX_COMPLETED = 200;

export function useWebJobs(): UseWebJobs {
  const [jobs, setJobs] = useState<WebJobRecord[]>([]);
  const mapRef = useRef<Map<string, WebJobRecord>>(new Map());

  const publish = useCallback((): void => {
    const map = mapRef.current;
    let active = false;
    for (const j of map.values()) {
      if (ACTIVE_STATUS.has(j.status)) {
        active = true;
        break;
      }
    }
    if (!active) {
      let completed = 0;
      for (const j of map.values()) {
        if (j.status === 'done' || j.status === 'cancelled') completed++;
      }
      let toDrop = completed - MAX_COMPLETED;
      if (toDrop > 0) {
        // Map preserves insertion order → the oldest enqueued go first.
        for (const [k, j] of map) {
          if (toDrop <= 0) break;
          if (j.status === 'done' || j.status === 'cancelled') {
            map.delete(k);
            toDrop--;
          }
        }
      }
    }
    setJobs(Array.from(map.values()));
  }, []);

  const flushTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lastFlushRef = useRef<number>(0);
  const scheduleFlush = useCallback((): void => {
    if (flushTimerRef.current) return;
    const elapsed = Date.now() - lastFlushRef.current;
    if (elapsed >= FLUSH_WINDOW) {
      lastFlushRef.current = Date.now();
      publish();
      return;
    }
    flushTimerRef.current = setTimeout(() => {
      flushTimerRef.current = null;
      lastFlushRef.current = Date.now();
      publish();
    }, FLUSH_WINDOW - elapsed);
  }, [publish]);

  const sync = useCallback(
    (list: WebJobRecord[]): void => {
      // Merge per-key (like onWebProgress) instead of replacing the whole map: if
      // getWebStatus is called again mid-session (re-subscribe/reload) a snapshot
      // shouldn't clobber fields that live progress events already filled in.
      const m = new Map(mapRef.current);
      for (const j of list || []) {
        const k = keyOf(j);
        if (k) m.set(k, { ...(m.get(k) || {}), ...j });
      }
      mapRef.current = m;
      publish();
    },
    [publish],
  );

  useEffect(() => {
    window.electronAPI
      ?.getWebStatus?.()
      .then((j) => sync((j || []).map(asJob)))
      .catch(() => {});
    const unsub = window.electronAPI?.onWebProgress?.((data) => {
      const job = asJob(data);
      if (!job) return;
      const k = keyOf(job);
      if (!k) return;
      // Merge so partial progress events don't drop fields (domain/url) set earlier.
      mapRef.current.set(k, { ...(mapRef.current.get(k) || {}), ...job });
      scheduleFlush();
    });
    return () => {
      if (typeof unsub === 'function') unsub();
      if (flushTimerRef.current) {
        clearTimeout(flushTimerRef.current);
        flushTimerRef.current = null;
      }
    };
  }, [sync, scheduleFlush]);

  // key is passed straight through (callers may hand an Activity item's optional
  // id); the optional-chained IPC call mirrors the original runtime exactly.
  const cancelJob = useCallback(
    (key?: string) => window.electronAPI?.cancelWebJob?.(key as string),
    [],
  );
  const cancelAll = useCallback(() => window.electronAPI?.cancelAllWeb?.(), []);
  const retryJob = useCallback(
    (key?: string) => window.electronAPI?.retryWebJob?.(key as string),
    [],
  );
  const clearCompleted = useCallback(async (): Promise<void> => {
    await window.electronAPI?.clearCompletedWeb?.();
    for (const [k, j] of mapRef.current) {
      if (['done', 'error', 'cancelled'].includes(j.status)) mapRef.current.delete(k);
    }
    publish();
  }, [publish]);

  // Stable object identity so memoized consumers only re-render on real changes.
  return useMemo(
    () => ({ jobs, cancelJob, cancelAll, retryJob, clearCompleted }),
    [jobs, cancelJob, cancelAll, retryJob, clearCompleted],
  );
}
