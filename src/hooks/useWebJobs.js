import { useState, useEffect, useCallback, useMemo, useRef } from 'react';

// Tracks the web-reference capture queue (F10 orchestrator). Mirrors useDownloads:
// an initial getWebStatus() snapshot + live web:progress upserts keyed by job key
// (`web:${postId}`). Feeds the Activity center so the user can follow the
// behind-the-scenes phases (discovering → capturing → extracting → analyzing).
const keyOf = (j) => (j?.postId ? `web:${j.postId}` : j?.key);

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

export function useWebJobs() {
  const [jobs, setJobs] = useState([]);
  const mapRef = useRef(new Map());

  const publish = useCallback(() => {
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

  const flushTimerRef = useRef(null);
  const lastFlushRef = useRef(0);
  const scheduleFlush = useCallback(() => {
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
    (list) => {
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
      .then((j) => sync(j || []))
      .catch(() => {});
    const unsub = window.electronAPI?.onWebProgress?.((job) => {
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

  const cancelJob = useCallback((key) => window.electronAPI?.cancelWebJob?.(key), []);
  const cancelAll = useCallback(() => window.electronAPI?.cancelAllWeb?.(), []);
  const retryJob = useCallback((key) => window.electronAPI?.retryWebJob?.(key), []);
  const clearCompleted = useCallback(async () => {
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
