import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ListChecks, RefreshCw, SearchX } from 'lucide-react';
import MenuButton from '../components/MenuButton';
import { Button, EmptyState, IconButton, PageHeader, Spinner } from '../components/ui';
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
    <div data-testid="jobs-view" className="flex flex-col h-full overflow-hidden bg-primary">
      <PageHeader
        className="border-b border-subtle"
        leading={<MenuButton />}
        title={t('title')}
        actions={
          <IconButton
            data-testid="jobs-refresh"
            label={t('refresh')}
            icon={RefreshCw}
            onClick={jobs.refresh}
          />
        }
      />

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

      {/* State filter: one scrolling row with an edge fade (JOB-4), chips 36px
        on narrow with a 44px tap area. */}
      <div className="flex items-center gap-2 border-b border-subtle py-2.5 pl-4 narrow:pl-3">
        <span className="shrink-0 text-caption uppercase tracking-wide text-muted">
          {t('stateFilterLabel')}
        </span>
        <div
          data-testid="jobs-state-filters"
          className="u-fade-x flex min-w-0 flex-1 items-center gap-2 overflow-x-auto py-1 pr-6"
        >
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
                  'u-press u-hit relative h-7 shrink-0 rounded-full border px-2.5 text-xs narrow:h-9 narrow:px-3',
                  selected
                    ? 'border-accent bg-accent-fill/20 text-primary'
                    : 'border-subtle text-secondary hover:bg-hover hover:text-primary',
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
              className="u-press u-hit relative h-7 shrink-0 rounded-full px-2.5 text-xs text-secondary hover:bg-hover hover:text-primary narrow:h-9"
            >
              {t('clearFilters')}
            </button>
          )}
        </div>
      </div>

      <div className="flex-1 min-h-0 overflow-y-auto">
        {jobs.loading ? (
          <div className="flex justify-center p-6">
            <Spinner size={18} label={tc('loading')} className="text-muted" />
          </div>
        ) : jobs.jobs.length === 0 ? (
          filtered ? (
            <EmptyState
              testId="jobs-empty"
              className="min-h-[50vh]"
              icon={SearchX}
              title={t('emptyFiltered')}
              action={{ label: t('clearFilters'), onClick: clearFilters }}
            />
          ) : (
            <EmptyState
              testId="jobs-empty"
              className="min-h-[50vh]"
              icon={ListChecks}
              title={t('empty')}
              body={t('emptyBody')}
            />
          )
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
                <Button
                  data-testid="jobs-load-more"
                  variant="secondary"
                  loading={jobs.loadingMore}
                  onClick={jobs.loadMore}
                >
                  {jobs.loadingMore ? tc('loading') : t('loadMore')}
                </Button>
              </div>
            )}
          </>
        )}
      </div>
    </div>
  );
}
