import { useState, useEffect, useCallback, useMemo, useRef } from 'react';

// Backend job keys are `${postId}:${assetType}` or, for carousel slides,
// `${postId}:${assetType}:${position}`. This fallback mirrors that exact format
// (`:` separators, position-aware) so it can never collapse two slides of the
// same post into one Map entry. In practice job.key is always present.
const fallbackKey = (j) => `${j.postId}:${j.assetType}:${j.mediaPosition ?? 0}`;

// Progress events can arrive at 10-30+/s per active download; publishing the
// derived array on every one re-renders the whole app tree. Events are upserted
// into the Map immediately and published with a leading + trailing throttle.
const FLUSH_WINDOW = 100;
// getStats runs several COUNT(*) aggregations — coalesce the per-completion
// refresh into at most one call per window (same policy as onNewPosts in App).
const STATS_WINDOW = 800;

export function useDownloads() {
  const [jobs, setJobs] = useState([]);
  const [stats, setStats] = useState({ total: 0, thumbnails: 0, images: 0, videos: 0 });
  const [isPaused, setIsPaused] = useState(false);

  // Source of truth for progress updates: a Map keyed by job.key keeps per-event
  // upserts O(1) (instead of findIndex + array copy → O(n²) over a burst). The
  // `jobs` array is derived from it whenever it changes. Insertion order is
  // preserved by Map, matching the previous append-on-new-key behaviour.
  const jobsMapRef = useRef(new Map());

  // Derive and publish the array from the Map. Untouched job objects keep their
  // identity across publishes, so memoized rows skip re-rendering.
  const publishJobs = useCallback(() => {
    setJobs(Array.from(jobsMapRef.current.values()));
  }, []);

  // Throttled publish for the event stream: the first event after a quiet spell
  // renders immediately (snappy single updates), a burst coalesces into one
  // trailing flush per window. Direct publishes (refresh/clear) bypass the
  // throttle clock so they never delay the next live event.
  const flushTimerRef = useRef(null);
  const lastFlushRef = useRef(0);
  const scheduleFlush = useCallback(() => {
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

  // Rebuild the Map from a freshly-fetched array and publish the derived list.
  const syncJobs = useCallback(
    (list) => {
      const map = new Map();
      for (const j of list || []) map.set(j.key ?? fallbackKey(j), j);
      jobsMapRef.current = map;
      publishJobs();
    },
    [publishJobs],
  );

  const refreshStats = useCallback(async () => {
    const s = await window.electronAPI.getStats();
    setStats({
      total: s.total,
      thumbnails: s.downloadedByType?.thumbnails || 0,
      images: s.downloadedByType?.images || 0,
      videos: s.downloadedByType?.videos || 0,
    });
  }, []);

  // Leading + trailing coalescing for the per-completion stats refresh: a drain
  // of N done events costs at most one getStats per window plus a trailing one
  // that captures the final totals.
  const statsTimerRef = useRef(null);
  const lastStatsRef = useRef(0);
  const bumpStats = useCallback(() => {
    const now = Date.now();
    if (now - lastStatsRef.current >= STATS_WINDOW) {
      lastStatsRef.current = now;
      refreshStats();
    } else if (!statsTimerRef.current) {
      statsTimerRef.current = setTimeout(
        () => {
          statsTimerRef.current = null;
          lastStatsRef.current = Date.now();
          refreshStats();
        },
        STATS_WINDOW - (now - lastStatsRef.current),
      );
    }
  }, [refreshStats]);

  const refreshJobs = useCallback(async () => {
    const j = await window.electronAPI.getDownloadStatus();
    syncJobs(j || []);
  }, [syncJobs]);

  useEffect(() => {
    refreshJobs();
    refreshStats();
    window.electronAPI.getDownloadIsPaused?.().then(setIsPaused);

    const unsub = window.electronAPI.onDownloadProgress((job) => {
      const key = job.key ?? fallbackKey(job);
      // O(1) upsert into the Map, then publish via the throttled flush.
      jobsMapRef.current.set(key, job);
      scheduleFlush();
      if (job.status === 'done') bumpStats();
    });

    return () => {
      if (typeof unsub === 'function') unsub();
      if (flushTimerRef.current) {
        clearTimeout(flushTimerRef.current);
        flushTimerRef.current = null;
      }
      if (statsTimerRef.current) {
        clearTimeout(statsTimerRef.current);
        statsTimerRef.current = null;
      }
    };
  }, [refreshJobs, refreshStats, scheduleFlush, bumpStats]);

  // Stats polling: getStats runs several COUNT(*) aggregations, so only poll
  // while the queue is actually active. The 'done' progress handler already
  // refreshes stats on each completion; this interval just covers slow drains.
  // When the queue empties the effect re-runs and clears the timer, so an idle
  // app (mounted in App.jsx for its whole lifetime) does no background churn.
  const hasQueue = jobs.some((j) => j.status === 'pending' || j.status === 'downloading');
  useEffect(() => {
    if (!hasQueue) return;
    const interval = setInterval(refreshStats, 5000);
    return () => clearInterval(interval);
  }, [hasQueue, refreshStats]);

  const refresh = useCallback(() => {
    refreshJobs();
    refreshStats();
  }, [refreshJobs, refreshStats]);

  // ── Actions ──────────────────────────────────────────────────────────────────

  const pauseAll = useCallback(async () => {
    await window.electronAPI.pauseDownloads();
    setIsPaused(true);
  }, []);

  const resumeAll = useCallback(async () => {
    await window.electronAPI.resumeDownloads();
    setIsPaused(false);
  }, []);

  const cancelAll = useCallback(async () => {
    await window.electronAPI.cancelAllDownloads();
    refreshJobs();
  }, [refreshJobs]);

  const clearCompleted = useCallback(async () => {
    await window.electronAPI.clearCompletedDownloads();
    for (const [key, j] of jobsMapRef.current) {
      if (j.status === 'done' || j.status === 'cancelled') jobsMapRef.current.delete(key);
    }
    publishJobs();
  }, [publishJobs]);

  // Cancel everything in flight/queued, then drop all entries from the list.
  // cancelAll() flips downloading/pending AND error jobs to 'cancelled', so the
  // clearCompleted() that follows purges every terminal state from the backend
  // jobsMap/jobstore — the local wipe below then stays in sync (error jobs no
  // longer reappear on the next refresh or after a restart).
  const clearAll = useCallback(async () => {
    await window.electronAPI.cancelAllDownloads();
    await window.electronAPI.clearCompletedDownloads();
    jobsMapRef.current = new Map();
    setJobs([]);
    setIsPaused(false);
  }, []);

  const cancelJob = useCallback(async (key) => {
    await window.electronAPI.cancelDownloadJob(key);
  }, []);

  const retryJob = useCallback(async (key) => {
    await window.electronAPI.retryDownloadJob(key);
  }, []);

  // Stable object identity: consumers (memoized views, effect deps) only see a
  // new reference when one of the slices actually changes.
  return useMemo(
    () => ({
      jobs,
      stats,
      isPaused,
      refresh,
      pauseAll,
      resumeAll,
      cancelAll,
      clearCompleted,
      clearAll,
      cancelJob,
      retryJob,
    }),
    [
      jobs,
      stats,
      isPaused,
      refresh,
      pauseAll,
      resumeAll,
      cancelAll,
      clearCompleted,
      clearAll,
      cancelJob,
      retryJob,
    ],
  );
}
