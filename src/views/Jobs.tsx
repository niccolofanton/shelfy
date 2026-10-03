import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { RefreshCw } from 'lucide-react';
import { useLang, useT } from '../i18n';
import { useShelfy } from '../api/ShelfyProvider';
import { useNavigation } from '../api/navigation';
import type { JobState } from '../api/jobs';
import { NO_JOBS_FILTER, useJobs, type UseJobsFilter } from '../hooks/useJobs';
import { jobStateLabel } from './jobs/labels';
import JobRow from './jobs/JobRow';
import QueueBar from './jobs/QueueBar';

// The Jobs view (P4-09, plan §2.19, §2.12 Controls): the web's background-job
// manager, which replaces the desktop's Downloads there (PG18). Filters live
// in the address (`/jobs?kind&state`) so they survive a reload and can be
// shared — src/api/navigation.tsx's `jobs` route carries them, unvalidated;
// this view is the one place that knows which values are valid.

const STATE_ORDER: JobState[] = ['queued', 'running', 'failed', 'succeeded', 'cancelled'];

function sanitizeStates(values: readonly string[]): JobState[] {
  return values.filter((v): v is JobState => (STATE_ORDER as string[]).includes(v));
}

export interface JobsProps {
  // Opens the job's post (a link to `/p/:key`); App wires this to the shared
  // navigation. A no-op default keeps the view usable in isolation (tests).
  onOpenPost?: (key: string) => void;
}

export default function Jobs({ onOpenPost }: JobsProps): React.JSX.Element {
  const t = useT('jobs');
  const tc = useT('common');
  const { lang } = useLang();
  const client = useShelfy();
  const nav = useNavigation();
  const openPost = onOpenPost ?? (() => {});

  // The address is the source of truth when one exists (always, on the web);
  // local state only backs this view when it runs without a Navigation
  // provider (a component test).
  const routeFilter = nav && nav.route.name === 'jobs' ? nav.route : null;
  const [localFilter, setLocalFilter] = useState<UseJobsFilter>(NO_JOBS_FILTER);
  // Memoized so it keeps its identity across renders the route (or the local
  // state) didn't actually change — toggleKind/toggleState below stay stable too.
  const filter = useMemo<UseJobsFilter>(
    () =>
      routeFilter
        ? { kind: routeFilter.kind, state: sanitizeStates(routeFilter.state) }
        : localFilter,
    [routeFilter, localFilter],
  );

  const setFilter = useCallback(
    (next: UseJobsFilter): void => {
      if (nav)
        nav.navigate({ name: 'jobs', kind: next.kind, state: next.state }, { replace: true });
      else setLocalFilter(next);
    },
    [nav],
  );
  const toggleKind = useCallback(
    (kind: string): void => {
      const has = filter.kind.includes(kind);
      setFilter({
        kind: has ? filter.kind.filter((k) => k !== kind) : [...filter.kind, kind],
        state: filter.state,
      });
    },
    [filter, setFilter],
  );
  const toggleState = useCallback(
    (state: JobState): void => {
      const has = filter.state.includes(state);
      setFilter({
        kind: filter.kind,
        state: has ? filter.state.filter((s) => s !== state) : [...filter.state, state],
      });
    },
    [filter, setFilter],
  );
  const clearFilters = useCallback(() => setFilter(NO_JOBS_FILTER), [setFilter]);
  const filtered = filter.kind.length > 0 || filter.state.length > 0;

  const jobs = useJobs(filter);

  // Posts of the jobs on screen, for their thumbnail and author (batched,
  // fetched once per key). `null` once resolved-but-missing (the post is
  // gone); absent while still loading.
  const [posts, setPosts] = useState<Map<string, Shelfy.Post | null>>(new Map());
  const postsRef = useRef(posts);
  postsRef.current = posts;
  useEffect(() => {
    const keys = Array.from(
      new Set(jobs.jobs.map((j) => j.postKey).filter((k): k is string => !!k)),
    );
    const missing = keys.filter((k) => !postsRef.current.has(k));
    if (!missing.length) return undefined;
    let alive = true;
    client
      .getPostsByIds(missing)
      .then((found) => {
        if (!alive) return;
        setPosts((prev) => {
          const next = new Map(prev);
          for (const key of missing) next.set(key, found.find((p) => p.id === key) ?? null);
          return next;
        });
      })
      .catch(() => {
        if (!alive) return;
        setPosts((prev) => {
          const next = new Map(prev);
          for (const key of missing) if (!next.has(key)) next.set(key, null);
          return next;
        });
      });
    return () => {
      alive = false;
    };
  }, [jobs.jobs, client]);

  const tileUrlOf = useCallback(
    (post: Shelfy.Post | null | undefined): string | null => {
      if (!post) return null;
      return client.media.tile(post.thumbnailPath, 80) ?? post.thumbnailUrl ?? null;
    },
    [client],
  );

  return (
    <div data-testid="jobs-view" className="flex flex-col h-full overflow-hidden bg-[#0f0f0f]">
      <header className="flex items-center gap-3 px-4 h-14 shrink-0 border-b border-[#222]">
        <h1 className="flex-1 text-[15px] font-semibold text-white">{t('title')}</h1>
        <button
          type="button"
          data-testid="jobs-refresh"
          title={t('refresh')}
          aria-label={t('refresh')}
          onClick={jobs.refresh}
          className="u-press flex items-center justify-center w-8 h-8 rounded-md text-[#9a9a9a] hover:text-white hover:bg-[#1f1f1f]"
        >
          <RefreshCw size={16} />
        </button>
      </header>

      <QueueBar
        queues={jobs.summary}
        selectedKinds={filter.kind}
        busyKinds={jobs.busyKinds}
        onToggleKind={toggleKind}
        onPause={jobs.pauseQueue}
        onResume={jobs.resumeQueue}
        onCancelAll={jobs.cancelQueue}
        onClearFinished={jobs.clearFinishedQueue}
      />

      <div className="flex items-center gap-2 px-4 py-2.5 border-b border-[#222] flex-wrap">
        <span className="text-[11px] uppercase tracking-wide text-[#7a7a7a]">
          {t('stateFilterLabel')}
        </span>
        {STATE_ORDER.map((state) => {
          const selected = filter.state.includes(state);
          return (
            <button
              key={state}
              type="button"
              data-testid={`jobs-state-filter-${state}`}
              aria-pressed={selected}
              onClick={() => toggleState(state)}
              className={[
                'u-press px-2.5 h-7 rounded-full text-[12px] border',
                selected
                  ? 'border-[#7B5CFF] bg-[#7B5CFF22] text-white'
                  : 'border-[#2a2a2a] text-[#9a9a9a] hover:text-white hover:bg-[#1a1a1a]',
              ].join(' ')}
            >
              {jobStateLabel(lang, state)}
            </button>
          );
        })}
        {filtered && (
          <button
            type="button"
            data-testid="jobs-clear-filters"
            onClick={clearFilters}
            className="u-press ml-1 px-2.5 h-7 rounded-full text-[12px] text-[#9a9a9a] hover:text-white hover:bg-[#1a1a1a]"
          >
            {t('clearFilters')}
          </button>
        )}
      </div>

      <div className="flex-1 min-h-0 overflow-y-auto">
        {jobs.loading ? (
          <div className="p-6 text-center text-[13px] text-[#7a7a7a]">{tc('loading')}</div>
        ) : jobs.jobs.length === 0 ? (
          <div data-testid="jobs-empty" className="p-6 text-center text-[13px] text-[#7a7a7a]">
            {filtered ? t('emptyFiltered') : t('empty')}
          </div>
        ) : (
          <>
            {jobs.jobs.map((job) => {
              const post = job.postKey ? posts.get(job.postKey) : null;
              return (
                <JobRow
                  key={job.id}
                  job={job}
                  post={post}
                  tileUrl={tileUrlOf(post)}
                  busy={jobs.busyIds.has(job.id)}
                  onOpenPost={openPost}
                  onCancel={jobs.cancel}
                  onRetry={jobs.retry}
                />
              );
            })}
            {jobs.hasMore && (
              <div className="p-4 flex justify-center">
                <button
                  type="button"
                  data-testid="jobs-load-more"
                  disabled={jobs.loadingMore}
                  onClick={jobs.loadMore}
                  className="u-press px-3 h-8 rounded-md bg-[#1f1f1f] text-[12px] text-[#ddd] hover:bg-[#272727] disabled:opacity-60"
                >
                  {jobs.loadingMore ? tc('loading') : t('loadMore')}
                </button>
              </div>
            )}
          </>
        )}
      </div>
    </div>
  );
}
