import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { isFinalJobState, type Job, type JobState, type QueueSummary } from '../api/jobs';
import { useShelfy } from '../api/ShelfyProvider';

// Drives the Jobs view (P4-09): one page of the user's jobs at a time
// (newest first, kept live by `job.updated`), and the per-kind queues of
// `GET /jobs/summary`. General on purpose (EXECUTION.md L14) — nothing here
// is specific to the view's own layout, so P2-08's Activity center can reuse
// the same shape for its own, smaller slice (summary + onUpdate) later.

export interface UseJobsFilter {
  kind: string[];
  state: JobState[];
}

export const NO_JOBS_FILTER: UseJobsFilter = { kind: [], state: [] };

const PAGE_SIZE = 50;

interface JobsState {
  byId: Map<number, Job>;
  // Fetched order (newest first); a page is appended, never re-sorted — a
  // live update patches a row in place but never moves it.
  order: number[];
  nextCursor: string | null;
}

const EMPTY_STATE: JobsState = { byId: new Map(), order: [], nextCursor: null };

function withoutFinishedOfKind(state: JobsState, kind: string): JobsState {
  const byId = new Map(state.byId);
  const order = state.order.filter((id) => {
    const job = byId.get(id);
    const drop = !!job && job.kind === kind && isFinalJobState(job.state);
    if (drop) byId.delete(id);
    return !drop;
  });
  return { ...state, byId, order };
}

function replaceQueue(summary: QueueSummary[], queue: QueueSummary): QueueSummary[] {
  const i = summary.findIndex((q) => q.kind === queue.kind);
  if (i === -1) return [...summary, queue];
  const next = [...summary];
  next[i] = queue;
  return next;
}

function without<T>(set: ReadonlySet<T>, value: T): Set<T> {
  const next = new Set(set);
  next.delete(value);
  return next;
}

export interface UseJobs {
  // The current filter's jobs, newest first.
  jobs: Job[];
  // Every queue of the user, independent of the list's own filter.
  summary: QueueSummary[];
  // The first page of the current filter is loading.
  loading: boolean;
  loadingMore: boolean;
  hasMore: boolean;
  error: unknown;
  loadMore(): void;
  // Refetches the list (from the start) and the summary.
  refresh(): void;
  // Ids with a cancel/retry call in flight: disable their buttons.
  busyIds: ReadonlySet<number>;
  cancel(id: number): Promise<void>;
  retry(id: number): Promise<void>;
  // Kinds with a queue action in flight: disable their buttons.
  busyKinds: ReadonlySet<string>;
  pauseQueue(kind: string): Promise<void>;
  resumeQueue(kind: string): Promise<void>;
  cancelQueue(kind: string): Promise<void>;
  clearFinishedQueue(kind: string): Promise<void>;
}

export function useJobs(filter: UseJobsFilter = NO_JOBS_FILTER): UseJobs {
  const client = useShelfy();
  const api = client.jobs;
  // Keyed on content, not identity: a caller that rebuilds `{kind, state}`
  // every render (a fresh array of the same values) must not re-fetch.
  const kindKey = filter.kind.join('\u0000');
  const stateKey = filter.state.join('\u0000');

  const [state, setState] = useState<JobsState>(EMPTY_STATE);
  const [loading, setLoading] = useState<boolean>(!!api);
  const [loadingMore, setLoadingMore] = useState<boolean>(false);
  const [error, setError] = useState<unknown>(null);
  const [summary, setSummary] = useState<QueueSummary[]>([]);
  const [busyIds, setBusyIds] = useState<Set<number>>(new Set());
  const [busyKinds, setBusyKinds] = useState<Set<string>>(new Set());

  // Guards a fetch whose filter changed (or that lost a race) before it
  // resolved: only the latest request may commit its page.
  const requestIdRef = useRef(0);

  const loadFirstPage = useCallback((): void => {
    if (!api) return;
    const requestId = ++requestIdRef.current;
    setLoading(true);
    setError(null);
    api
      .list({ kind: filter.kind, state: filter.state }, { limit: PAGE_SIZE })
      .then((page) => {
        if (requestIdRef.current !== requestId) return;
        setState({
          byId: new Map(page.items.map((j) => [j.id, j])),
          order: page.items.map((j) => j.id),
          nextCursor: page.nextCursor,
        });
        setLoading(false);
      })
      .catch((err: unknown) => {
        if (requestIdRef.current !== requestId) return;
        setError(err);
        setLoading(false);
      });
    // `filter.kind`/`filter.state` may be a fresh array every render; kindKey/
    // stateKey (their content) are the real dependency — see the comment above.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [api, kindKey, stateKey]);

  const loadSummary = useCallback((): void => {
    api
      ?.summary()
      .then(setSummary)
      .catch(() => {
        /* the queue bar just keeps its last known counts */
      });
  }, [api]);

  useEffect(() => {
    loadFirstPage();
  }, [loadFirstPage]);
  useEffect(() => {
    loadSummary();
  }, [loadSummary]);

  // Live updates: patch a row already on screen. Never inserts one the
  // caller has not fetched yet (a resync, `refresh()`, or the next page,
  // does) — see src/api/jobs.ts's JobsApi.onUpdate.
  useEffect(() => {
    if (!api) return undefined;
    return api.onUpdate((update) => {
      setState((prev) => {
        const existing = prev.byId.get(update.id);
        if (!existing) return prev;
        const byId = new Map(prev.byId);
        byId.set(update.id, {
          ...existing,
          state: update.state,
          progress: update.progress,
          stage: update.stage,
          postKey: update.postKey,
          errorCode: update.errorCode,
        });
        return { ...prev, byId };
      });
    });
  }, [api]);

  // Events were lost (the stream fell behind, or the server restarted):
  // reload both the list and the queues.
  useEffect(
    () =>
      client.on('resync', () => {
        loadFirstPage();
        loadSummary();
      }),
    [client, loadFirstPage, loadSummary],
  );

  const loadMore = useCallback((): void => {
    if (!api || !state.nextCursor || loadingMore || loading) return;
    const requestId = requestIdRef.current;
    const cursor = state.nextCursor;
    setLoadingMore(true);
    api
      .list({ kind: filter.kind, state: filter.state }, { limit: PAGE_SIZE, cursor })
      .then((page) => {
        if (requestIdRef.current !== requestId) return;
        setState((prev) => {
          const byId = new Map(prev.byId);
          for (const job of page.items) byId.set(job.id, job);
          return {
            byId,
            order: [...prev.order, ...page.items.map((j) => j.id)],
            nextCursor: page.nextCursor,
          };
        });
        setLoadingMore(false);
      })
      .catch((err: unknown) => {
        if (requestIdRef.current !== requestId) return;
        setError(err);
        setLoadingMore(false);
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [api, state.nextCursor, loadingMore, loading, kindKey, stateKey]);

  const refresh = useCallback((): void => {
    loadFirstPage();
    loadSummary();
  }, [loadFirstPage, loadSummary]);

  const cancel = useCallback(
    async (id: number): Promise<void> => {
      if (!api) return;
      setBusyIds((prev) => new Set(prev).add(id));
      try {
        const job = await api.cancel(id);
        setState((prev) =>
          prev.byId.has(id) ? { ...prev, byId: new Map(prev.byId).set(id, job) } : prev,
        );
        // The queue's counts moved too (a cancel adds to `cancelled`, a retry
        // re-queues): the single-job call returns only the job, so refetch.
        loadSummary();
      } finally {
        setBusyIds((prev) => without(prev, id));
      }
    },
    [api, loadSummary],
  );

  const retry = useCallback(
    async (id: number): Promise<void> => {
      if (!api) return;
      setBusyIds((prev) => new Set(prev).add(id));
      try {
        const job = await api.retry(id);
        setState((prev) =>
          prev.byId.has(id) ? { ...prev, byId: new Map(prev.byId).set(id, job) } : prev,
        );
        // The queue's counts moved too (a cancel adds to `cancelled`, a retry
        // re-queues): the single-job call returns only the job, so refetch.
        loadSummary();
      } finally {
        setBusyIds((prev) => without(prev, id));
      }
    },
    [api, loadSummary],
  );

  // Every queue action shares the busy-kind bookkeeping; only
  // `clearFinishedQueue` also drops rows from the list (no event tells us a
  // finished job's row was deleted server-side).
  const runQueueAction = useCallback(
    async (
      kind: string,
      method: 'pauseQueue' | 'resumeQueue' | 'cancelQueue' | 'clearFinishedQueue',
    ): Promise<void> => {
      if (!api) return;
      setBusyKinds((prev) => new Set(prev).add(kind));
      try {
        const result = await api[method](kind);
        setSummary((prev) => replaceQueue(prev, result.queue));
        if (method === 'clearFinishedQueue') {
          setState((prev) => withoutFinishedOfKind(prev, kind));
        }
      } finally {
        setBusyKinds((prev) => without(prev, kind));
      }
    },
    [api],
  );

  const pauseQueue = useCallback(
    (kind: string) => runQueueAction(kind, 'pauseQueue'),
    [runQueueAction],
  );
  const resumeQueue = useCallback(
    (kind: string) => runQueueAction(kind, 'resumeQueue'),
    [runQueueAction],
  );
  const cancelQueue = useCallback(
    (kind: string) => runQueueAction(kind, 'cancelQueue'),
    [runQueueAction],
  );
  const clearFinishedQueue = useCallback(
    (kind: string) => runQueueAction(kind, 'clearFinishedQueue'),
    [runQueueAction],
  );

  const jobs = useMemo(
    () => state.order.map((id) => state.byId.get(id)).filter((j): j is Job => !!j),
    [state],
  );

  return {
    jobs,
    summary,
    loading,
    loadingMore,
    hasMore: state.nextCursor != null,
    error,
    loadMore,
    refresh,
    busyIds,
    cancel,
    retry,
    busyKinds,
    pauseQueue,
    resumeQueue,
    cancelQueue,
    clearFinishedQueue,
  };
}
