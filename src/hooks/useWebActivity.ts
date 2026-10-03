import { useCallback, useEffect, useRef, useState } from 'react';
import type {
  ActivityNotification,
  NotificationPage,
  NotificationReadSelector,
} from '../api/activity';
import type { JobUpdate } from '../api/jobs';
import { useShelfy } from '../api/ShelfyProvider';
import { useJobs } from './useJobs';

const ACTIVITY_FILTER = { kind: [], state: ['queued', 'running', 'failed', 'cancelled'] as const };
const PAGE_SIZE = 60;
const EMPTY: NotificationPage = { items: [], unreadCount: 0, nextCursor: null };

function mergeNotifications(
  items: ActivityNotification[],
  incoming: ActivityNotification[],
): ActivityNotification[] {
  const byId = new Map(items.map((item) => [item.id, item]));
  for (const item of incoming) {
    const previous = byId.get(item.id);
    byId.set(item.id, { ...item, readAt: item.readAt ?? previous?.readAt ?? null });
  }
  return [...byId.values()].sort((a, b) => b.id - a.id);
}

export function useWebActivity() {
  const client = useShelfy();
  const api = client.activity;
  const jobs = useJobs({ kind: ACTIVITY_FILTER.kind, state: [...ACTIVITY_FILTER.state] });
  const [updates, setUpdates] = useState<Map<number, JobUpdate>>(new Map());
  const jobsRef = useRef(jobs);
  jobsRef.current = jobs;
  const [page, setPage] = useState<NotificationPage>(EMPTY);
  const [loading, setLoading] = useState(!!api);
  const [loadingMore, setLoadingMore] = useState(false);
  const [reading, setReading] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const mounted = useRef(false);
  const sessionEpoch = useRef(0);
  const generation = useRef(0);
  const received = useRef(new Map<number, ActivityNotification>());
  const pendingActions = useRef(new Set<string>());

  const refresh = useCallback(() => {
    if (!api) return;
    const request = ++generation.current;
    received.current.clear();
    setLoading(true);
    setLoadingMore(false);
    api
      .list({ limit: PAGE_SIZE })
      .then((result) => {
        if (!mounted.current || request !== generation.current) return;
        const newer = [...received.current.values()].filter(
          (n) => n.id > (result.items[0]?.id ?? 0),
        );
        setPage({
          ...result,
          items: mergeNotifications(result.items, [...received.current.values()]),
          unreadCount: result.unreadCount + newer.filter((n) => n.readAt == null).length,
        });
        setError(null);
        setLoading(false);
      })
      .catch((err: unknown) => {
        if (!mounted.current || request !== generation.current) return;
        setError(err);
        setLoading(false);
      });
  }, [api]);

  useEffect(() => {
    const actions = pendingActions.current;
    mounted.current = true;
    sessionEpoch.current += 1;
    refresh();
    if (!api)
      return () => {
        mounted.current = false;
      };
    const stopNotifications = api.onNotification((notification) => {
      received.current.set(notification.id, notification);
      setPage((previous) => {
        const known = previous.items.some((n) => n.id === notification.id);
        const newer = notification.id > (previous.items[0]?.id ?? 0);
        return {
          ...previous,
          items: mergeNotifications(previous.items, [notification]),
          unreadCount:
            previous.unreadCount + (!known && newer && notification.readAt == null ? 1 : 0),
        };
      });
    });
    const stopRefresh = api.onRefresh(() => {
      refresh();
    });
    const stopResync = client.on('resync', () => setUpdates(new Map()));
    let discoveryTimer: ReturnType<typeof setTimeout> | undefined;
    const stopJobs = client.jobs?.onUpdate((update) => {
      // Keep every parallel job's latest progress, including updates that
      // arrive while its initial list is in flight. Never manufacture a Job
      // from the partial event: discover unknown ids through JobsApi.list.
      setUpdates((previous) => new Map(previous).set(update.id, update));
      if (!jobsRef.current.jobs.some((job) => job.id === update.id) && !discoveryTimer) {
        discoveryTimer = setTimeout(() => {
          discoveryTimer = undefined;
          jobsRef.current.refresh();
        }, 250);
      }
    });
    return () => {
      mounted.current = false;
      sessionEpoch.current += 1;
      actions.clear();
      generation.current += 1;
      stopNotifications();
      stopRefresh();
      stopJobs?.();
      stopResync();
      if (discoveryTimer) clearTimeout(discoveryTimer);
    };
  }, [api, client, refresh]);

  // An old terminal history page must not hide active jobs further down the
  // list. Fetch the full active/retryable slice via the existing jobs hook.
  const {
    hasMore: jobsHasMore,
    loading: jobsLoading,
    loadingMore: jobsLoadingMore,
    error: jobsError,
    loadMore: loadMoreJobs,
  } = jobs;
  useEffect(() => {
    if (jobsHasMore && !jobsLoading && !jobsLoadingMore && !jobsError) loadMoreJobs();
  }, [jobsHasMore, jobsLoading, jobsLoadingMore, jobsError, loadMoreJobs]);

  const loadMore = async () => {
    if (!api || !page.nextCursor || loading || loadingMore) return;
    const request = generation.current;
    setLoadingMore(true);
    try {
      const result = await api.list({ limit: PAGE_SIZE, cursor: page.nextCursor });
      if (!mounted.current || request !== generation.current) return;
      setPage((previous) => ({
        ...previous,
        items: mergeNotifications(previous.items, result.items),
        nextCursor: result.nextCursor,
      }));
    } catch (err) {
      if (mounted.current && request === generation.current) setError(err);
    } finally {
      if (mounted.current && request === generation.current) setLoadingMore(false);
    }
  };

  const read = async (selector: NotificationReadSelector) => {
    if (!api || pendingActions.current.has('read')) return;
    const epoch = sessionEpoch.current;
    pendingActions.current.add('read');
    setReading(true);
    try {
      const result = await api.read(selector);
      if (!mounted.current || epoch !== sessionEpoch.current) return;
      setPage((previous) => ({
        ...previous,
        unreadCount: result.unreadCount,
        items: previous.items.map((n) =>
          (selector.upTo != null ? n.id <= selector.upTo : selector.ids.includes(n.id))
            ? { ...n, readAt: n.readAt ?? Date.now() }
            : n,
        ),
      }));
      refresh();
    } catch (err) {
      if (mounted.current && epoch === sessionEpoch.current) setError(err);
    } finally {
      if (epoch === sessionEpoch.current) pendingActions.current.delete('read');
      if (mounted.current && epoch === sessionEpoch.current) setReading(false);
    }
  };

  const act = async (key: string, action: () => Promise<void>) => {
    if (pendingActions.current.has(key)) return;
    const epoch = sessionEpoch.current;
    pendingActions.current.add(key);
    setError(null);
    try {
      await action();
    } catch (err) {
      if (mounted.current && epoch === sessionEpoch.current) setError(err);
    } finally {
      if (epoch === sessionEpoch.current) pendingActions.current.delete(key);
    }
  };
  const jobAction = (id: number, method: 'cancel' | 'retry') =>
    act(`job:${id}`, async () => {
      setUpdates((previous) => {
        const next = new Map(previous);
        next.delete(id);
        return next;
      });
      await jobs[method](id);
    });

  const visibleJobs = jobs.jobs
    .map((job) => ({ ...job, ...updates.get(job.id) }))
    .filter((job) => job.state !== 'succeeded');
  return {
    jobs: visibleJobs,
    queues: jobs.summary.filter(
      (queue) =>
        queue.paused ||
        visibleJobs.some(
          (job) => job.kind === queue.kind && (job.state === 'running' || job.state === 'queued'),
        ),
    ),
    busyIds: jobs.busyIds,
    busyKinds: jobs.busyKinds,
    cancel: (id: number) => jobAction(id, 'cancel'),
    retry: (id: number) => jobAction(id, 'retry'),
    pauseQueue: (kind: string) => act(`queue:${kind}`, () => jobs.pauseQueue(kind)),
    resumeQueue: (kind: string) => act(`queue:${kind}`, () => jobs.resumeQueue(kind)),
    notifications: page.items,
    unread: page.unreadCount,
    loading: loading || jobs.loading,
    loadingMore,
    reading,
    error: error ?? jobs.error,
    hasMore: page.nextCursor != null,
    loadMore,
    markRead: (id: number) => read({ ids: [id] }),
    markAllRead: () => (page.items[0] ? read({ upTo: page.items[0].id }) : Promise.resolve()),
    refresh: () => {
      refresh();
      jobs.refresh();
    },
  };
}
