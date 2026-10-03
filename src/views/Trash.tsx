import React, { useState, useEffect, useCallback, useRef, useMemo } from 'react';
import { Trash2, RotateCcw, CheckSquare, X, ListChecks, AlertCircle } from 'lucide-react';
import MenuButton from '../components/MenuButton';
import {
  Button,
  EmptyState,
  IconButton,
  NARROW_QUERY,
  PageHeader,
  ToastHost,
  useMediaQuery,
} from '../components/ui';
import VirtualPostGrid from '../components/VirtualPostGrid';
import PostGridSkeleton from '../components/PostGridSkeleton';
import { useRangeSelect } from '../hooks/useRangeSelect';
import { useToasts } from '../hooks/useToast';
import { useShelfy } from '../api/ShelfyProvider';
import type { BulkJob } from '../api/ShelfyClient';
import { useT } from '../i18n';

// The Trash view (P1-11/P1-14): a plain, newest-deleted-first list — no
// search or facets (trashed posts carry no FTS rows, review L6 on P1-11, so a
// query would never match) — with selection, bulk restore and "empty trash".
// Reuses VirtualPostGrid (shared with the Gallery) for the windowed grid, and
// useRangeSelect's selectAllMatching for a selection that can span more posts
// than are loaded (the same pattern Gallery.tsx uses for its own select-all).

const LOAD_BATCH = 50;
const PAGE_BATCH = 250;
const PREFETCH_PX = 1200;

interface TrashProps {
  // False while the keep-alive view is hidden (mirrors Gallery's `active`).
  active?: boolean;
  // The library changed (posts restored or purged): let the caller refresh
  // its own stats/sidebar counts.
  onLibraryChanged?: () => void;
}

export default function Trash({ active = true, onLibraryChanged }: TrashProps): React.JSX.Element {
  const t = useT('trash');
  const client = useShelfy();

  const [posts, setPosts] = useState<Shelfy.Post[]>([]);
  const [total, setTotal] = useState<number>(0);
  const [retentionDays, setRetentionDays] = useState<number>(30);
  const [loading, setLoading] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  const [reloadNonce, setReloadNonce] = useState<number>(0);
  const reload = useCallback(() => setReloadNonce((n) => n + 1), []);

  const nextCursorRef = useRef<string | null>(null);
  const fetchedRef = useRef<number>(0);
  const [limit, setLimit] = useState<number>(LOAD_BATCH);
  const abortRef = useRef<AbortController | null>(null);

  // Core fetch: a plain replace on mount / manual reload, an incremental
  // append when `limit` grows (infinite scroll) — mirrors usePosts.ts.
  useEffect(() => {
    if (!active) return undefined;
    const isAppend = limit > fetchedRef.current && fetchedRef.current > 0 && reloadNonce === 0;
    abortRef.current?.abort();
    const controller = new AbortController();
    abortRef.current = controller;
    if (!isAppend) {
      fetchedRef.current = 0;
      nextCursorRef.current = null;
    }
    const cursor = isAppend ? nextCursorRef.current : null;
    if (isAppend && !cursor) return undefined; // last page already loaded
    setLoading(true);
    setError(null);
    client
      .listTrash({
        limit: limit - (isAppend ? fetchedRef.current : 0),
        cursor,
        signal: controller.signal,
      })
      .then((page) => {
        if (controller.signal.aborted) return;
        setTotal(page.total);
        setRetentionDays(page.retentionDays);
        nextCursorRef.current = page.nextCursor;
        fetchedRef.current = (isAppend ? fetchedRef.current : 0) + page.posts.length;
        setPosts((prev) => (isAppend ? [...prev, ...page.posts] : page.posts));
      })
      .catch((err: unknown) => {
        if (controller.signal.aborted) return;
        console.error('[Trash] listTrash error:', err);
        setError(t('loadError'));
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, limit, active, reloadNonce]);
  useEffect(() => () => abortRef.current?.abort(), []);

  // ── Selection (mirrors Gallery.tsx's select-all-matching) ────────────────
  const [selectMode, setSelectMode] = useState<boolean>(false);
  const {
    selected,
    setSelected,
    toggleAt,
    clearSelection,
    selectAllMatching,
    setSelectAllMatching,
    isSelected,
  } = useRangeSelect(posts, (p: Shelfy.Post) => p.id);
  const toasts = useToasts();
  const narrow = useMediaQuery(NARROW_QUERY);
  const showFeedback = useCallback(
    (message: string, variant: 'success' | 'error' | 'neutral' = 'neutral') =>
      toasts.show(message, { variant, testId: 'trash-feedback-toast' }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [toasts.show],
  );
  const [confirmEmpty, setConfirmEmpty] = useState<boolean>(false);
  const [confirmRestore, setConfirmRestore] = useState<boolean>(false);
  const [pendingJob, setPendingJob] = useState<BulkJob | null>(null);
  const [jobProgress, setJobProgress] = useState<number | null>(null);

  useEffect(() => {
    if (!pendingJob) return undefined;
    const off = client.on('job.updated', (evt) => {
      if (evt.id !== pendingJob.id) return;
      setJobProgress(evt.progress);
      if (evt.state === 'succeeded' || evt.state === 'failed' || evt.state === 'cancelled') {
        setPendingJob(null);
        setJobProgress(null);
        reload();
        onLibraryChanged?.();
      }
    });
    const offChanged = client.on('posts.changed', () => {
      setPendingJob(null);
      setJobProgress(null);
    });
    return () => {
      off();
      offChanged();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pendingJob, client]);

  // The purge/restore job's progress: one toast, updated in place and dismissed
  // when the job ends (or posts.changed says the library moved on).
  const toastsShow = toasts.show;
  const toastsDismiss = toasts.dismiss;
  useEffect(() => {
    if (!pendingJob) {
      toastsDismiss('trash-job');
      return;
    }
    toastsShow(
      jobProgress != null
        ? t('jobProgress', { pct: Math.round(jobProgress * 100) })
        : t('jobRunning'),
      { id: 'trash-job', variant: 'progress', duration: null, testId: 'trash-job-toast' },
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pendingJob, jobProgress, toastsShow, toastsDismiss]);

  const exitSelectMode = useCallback(() => {
    setSelectMode(false);
    clearSelection();
    setConfirmEmpty(false);
    setConfirmRestore(false);
  }, [clearSelection]);

  const selectedCount = selectAllMatching ? Math.max(0, total - selected.size) : selected.size;
  const allSelected = selectAllMatching ? selected.size === 0 : total > 0 && selectedCount >= total;
  const visibleSelected = useMemo(
    () =>
      selectAllMatching
        ? new Set(posts.filter((p) => isSelected(p.id)).map((p) => p.id))
        : selected,
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [selectAllMatching, selected, posts],
  );

  const handleSelectAll = async (): Promise<void> => {
    if (allSelected) {
      clearSelection();
      return;
    }
    const loadedIds = posts.map((p) => p.id);
    setSelectAllMatching(false);
    setSelected(new Set(loadedIds));
    if (total > posts.length) {
      setSelectAllMatching(true);
      setSelected(new Set());
    }
    showFeedback(t('selectedCount', { n: total > posts.length ? total : loadedIds.length }));
  };

  const handleCardOpen = useCallback(
    (post: Shelfy.Post, event?: React.SyntheticEvent) => {
      const index = posts.findIndex((p) => p.id === post.id);
      const shiftKey =
        event && 'shiftKey' in event ? (event as unknown as { shiftKey: boolean }).shiftKey : false;
      if (!selectMode) setSelectMode(true);
      toggleAt(post.id, index, shiftKey);
    },
    [posts, selectMode, toggleAt],
  );

  // Restores the selection: by filter (select-all-matching) or by explicit
  // keys (chunked at 200, consistent with Gallery's bulk actions).
  const handleRestore = async (): Promise<void> => {
    if (!selectAllMatching && selected.size === 0) return;
    try {
      let changed = 0;
      let job: BulkJob | null = null;
      if (selectAllMatching) {
        const res = await client.restoreFromTrash({
          filter: { trash: true },
          exceptKeys: [...selected],
        });
        changed = res.changed ?? 0;
        job = res.job;
      } else {
        const ids = [...selected];
        for (let i = 0; i < ids.length; i += 200) {
          const res = await client.restoreFromTrash({ keys: ids.slice(i, i + 200) });
          changed += res.changed ?? 0;
          if (res.job) job = res.job;
        }
      }
      if (job) {
        setPendingJob(job);
        setJobProgress(null);
      } else {
        showFeedback(t('fbRestored', { n: changed }), 'success');
      }
      exitSelectMode();
      reload();
      onLibraryChanged?.();
    } catch (err) {
      console.error('[Trash] restore error:', err);
      showFeedback(t('fbRestoreError'), 'error');
      setConfirmRestore(false);
    }
  };

  const handleEmpty = async (): Promise<void> => {
    try {
      const res = await client.emptyTrash();
      if (res.job) {
        setPendingJob(res.job);
        setJobProgress(null);
      }
      showFeedback(t('fbEmptyQueued'), 'success');
      exitSelectMode();
      reload();
      onLibraryChanged?.();
    } catch (err) {
      console.error('[Trash] emptyTrash error:', err);
      showFeedback(t('fbEmptyError'), 'error');
    } finally {
      setConfirmEmpty(false);
    }
  };

  // ── Infinite scroll (mirrors Gallery.tsx) ─────────────────────────────────
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const sentinelRef = useRef<HTMLDivElement | null>(null);
  const observerRef = useRef<IntersectionObserver | null>(null);
  const loadingRef = useRef(loading);
  const postsLenRef = useRef(posts.length);
  const totalRef = useRef(total);
  loadingRef.current = loading;
  postsLenRef.current = posts.length;
  totalRef.current = total;

  const maybeLoadMore = useCallback(() => {
    if (loadingRef.current) return;
    if (postsLenRef.current >= totalRef.current) return;
    const scroller = scrollRef.current;
    const sentinel = sentinelRef.current;
    if (!scroller || !sentinel || scroller.clientHeight === 0) return;
    const distance = sentinel.getBoundingClientRect().top - scroller.getBoundingClientRect().bottom;
    if (distance < PREFETCH_PX) setLimit((l) => l + PAGE_BATCH);
  }, []);

  useEffect(() => {
    observerRef.current = new IntersectionObserver(maybeLoadMore, {
      root: scrollRef.current,
      rootMargin: `${PREFETCH_PX}px 0px`,
    });
    if (sentinelRef.current) observerRef.current.observe(sentinelRef.current);
    return () => observerRef.current?.disconnect();
  }, [maybeLoadMore]);
  useEffect(() => {
    if (!loading) maybeLoadMore();
  }, [loading, posts.length, total, maybeLoadMore]);

  const isEmpty = !loading && posts.length === 0;

  const selectAllLabel = allSelected
    ? t('deselectAll')
    : total > posts.length
      ? t('selectAllN', { n: total.toLocaleString() })
      : t('selectAll');
  const restoreLabel = confirmRestore
    ? narrow
      ? t('restoreConfirmShort', { n: selectedCount })
      : t('restoreSelectedConfirm', { n: selectedCount })
    : t('restoreSelected');

  const selectionCount = (
    <span
      data-testid="trash-selection-count"
      className="text-sm font-medium tabular-nums text-primary"
    >
      {t('selectedCount', { n: selectedCount.toLocaleString() })}
    </span>
  );
  const selectAllButton = total > 0 && (
    <Button
      data-testid="trash-select-all"
      variant="ghost"
      size="sm"
      icon={ListChecks}
      onClick={handleSelectAll}
      className="text-accent"
    >
      {selectAllLabel}
    </Button>
  );
  const restoreButton = (
    <Button
      data-testid="trash-restore"
      variant="primary"
      size={narrow ? 'lg' : 'sm'}
      icon={RotateCcw}
      disabled={selectedCount === 0}
      onClick={() => (confirmRestore ? handleRestore() : setConfirmRestore(true))}
    >
      {restoreLabel}
    </Button>
  );
  const cancelSelectButton = (
    <IconButton
      data-testid="trash-select-cancel"
      label={t('exitSelectionTitle')}
      icon={X}
      onClick={exitSelectMode}
    />
  );

  let actions: React.ReactNode = null;
  if (!selectMode) {
    if (total > 0) {
      actions = narrow ? (
        <>
          <IconButton
            data-testid="trash-select-toggle"
            label={t('select')}
            icon={CheckSquare}
            onClick={() => setSelectMode(true)}
          />
          <IconButton
            data-testid="trash-empty"
            label={t('emptyTrash')}
            icon={Trash2}
            tone="danger"
            aria-expanded={confirmEmpty}
            onClick={() => setConfirmEmpty((v) => !v)}
          />
        </>
      ) : (
        <>
          <Button
            data-testid="trash-select-toggle"
            variant="ghost"
            size="sm"
            icon={CheckSquare}
            title={t('selectTitle')}
            onClick={() => setSelectMode(true)}
          >
            {t('select')}
          </Button>
          <Button
            data-testid="trash-empty"
            variant="ghost"
            size="sm"
            icon={Trash2}
            title={t('emptyTrashHint')}
            aria-expanded={confirmEmpty}
            onClick={() => setConfirmEmpty((v) => !v)}
            className="text-error hover:text-error"
          >
            {t('emptyTrash')}
          </Button>
        </>
      );
    }
  } else if (!narrow) {
    actions = (
      <>
        {selectionCount}
        {selectAllButton}
        {restoreButton}
        {cancelSelectButton}
      </>
    );
  }

  return (
    <div data-testid="trash-view" className="flex h-full flex-col overflow-hidden">
      <ToastHost toasts={toasts} />

      <PageHeader
        className="border-b border-subtle"
        leading={<MenuButton />}
        title={t('title')}
        count={
          <span data-testid="trash-count">{t('postsCount', { n: total.toLocaleString() })}</span>
        }
        actions={actions}
      />

      {confirmEmpty && !selectMode && total > 0 && (
        <div
          data-testid="trash-empty-confirm-bar"
          role="alert"
          className="flex flex-wrap items-center gap-2 border-b border-subtle bg-secondary px-4 py-2 narrow:px-3"
        >
          <p className="min-w-0 flex-1 text-sm text-primary">{t('emptyTrashConfirm')}</p>
          <Button size="sm" variant="ghost" onClick={() => setConfirmEmpty(false)}>
            {t('cancel')}
          </Button>
          <Button
            data-testid="trash-empty-confirm"
            size="sm"
            variant="danger"
            icon={Trash2}
            onClick={handleEmpty}
          >
            {t('emptyTrashForever')}
          </Button>
        </div>
      )}

      {total > 0 && (
        <p className="border-b border-subtle px-4 py-2 text-caption text-muted narrow:px-3">
          {t('retention', { days: retentionDays })}
        </p>
      )}

      <div
        ref={scrollRef}
        className={`flex-1 overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e] scrollbar-track-transparent ${
          selectMode && narrow ? 'pb-24' : ''
        }`}
      >
        {posts.length > 0 && (
          <VirtualPostGrid
            testId="trash-grid"
            posts={posts}
            scrollRef={scrollRef}
            onOpen={handleCardOpen}
            selectable={selectMode}
            selected={visibleSelected}
          />
        )}

        {loading && posts.length === 0 && <PostGridSkeleton />}

        {isEmpty &&
          (error ? (
            <EmptyState
              testId="trash-empty-state"
              className="h-full min-h-[60vh]"
              icon={AlertCircle}
              title={error}
              action={{ label: t('retry'), onClick: reload }}
            />
          ) : (
            <EmptyState
              testId="trash-empty-state"
              className="h-full min-h-[60vh]"
              icon={Trash2}
              title={t('emptyStateTitle')}
              body={t('emptyStateHint', { days: retentionDays })}
            />
          ))}

        <div ref={sentinelRef} className="h-1" aria-hidden="true" />
      </div>

      {/* Narrow select mode: the actions sit in a bottom bar over the BottomNav
        (TR-5, as the gallery's GAL-11): [× 44] [N selected] [Select all] [Restore]. */}
      {selectMode && narrow && (
        <div
          data-testid="trash-selection-bar"
          className="fixed inset-x-0 bottom-0 z-drawer flex items-center gap-2 border-t border-strong bg-elevated px-3 py-2 u-fade-in-up"
          style={{ paddingBottom: 'calc(0.5rem + env(safe-area-inset-bottom))' }}
        >
          {cancelSelectButton}
          {selectionCount}
          <div className="flex-1" />
          {selectAllButton}
          {restoreButton}
        </div>
      )}
    </div>
  );
}
