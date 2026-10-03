import React, { useState, useEffect, useCallback, useRef, useMemo } from 'react';
import { Trash2, RotateCcw, CheckSquare, X, ListChecks, Loader2, ImageOff } from 'lucide-react';
import VirtualPostGrid from '../components/VirtualPostGrid';
import PostGridSkeleton from '../components/PostGridSkeleton';
import { useRangeSelect } from '../hooks/useRangeSelect';
import { useToast } from '../hooks/useToast';
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
  const { toast: feedback, showToast: showFeedback } = useToast();
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
        showFeedback(t('fbRestored', { n: changed }));
      }
      exitSelectMode();
      reload();
      onLibraryChanged?.();
    } catch (err) {
      console.error('[Trash] restore error:', err);
      showFeedback(t('fbRestoreError'));
      setConfirmRestore(false);
    }
  };

  const handleEmpty = async (): Promise<void> => {
    if (!confirmEmpty) {
      setConfirmEmpty(true);
      return;
    }
    try {
      const res = await client.emptyTrash();
      if (res.job) {
        setPendingJob(res.job);
        setJobProgress(null);
      }
      showFeedback(t('fbEmptyQueued'));
      exitSelectMode();
      reload();
      onLibraryChanged?.();
    } catch (err) {
      console.error('[Trash] emptyTrash error:', err);
      showFeedback(t('fbEmptyError'));
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

  return (
    <div data-testid="trash-view" className="flex h-full flex-col overflow-hidden">
      {!selectMode && feedback && (
        <div
          key={feedback}
          data-testid="trash-feedback-toast"
          className="fixed bottom-6 left-1/2 -translate-x-1/2 z-50 px-4 py-2 rounded-lg bg-[#1a1a1a] border border-[#2e2e2e] text-xs text-[#7B5CFF] tabular-nums whitespace-nowrap u-pop-in shadow-lg"
        >
          {feedback}
        </div>
      )}
      {pendingJob && (
        <div
          data-testid="trash-job-toast"
          className="fixed bottom-6 left-1/2 -translate-x-1/2 z-50 flex items-center gap-2 px-4 py-2 rounded-lg bg-[#1a1a1a] border border-[#2e2e2e] text-xs text-gray-300 whitespace-nowrap u-pop-in shadow-lg"
        >
          <Loader2 size={13} className="animate-spin text-[#7B5CFF]" />
          {jobProgress != null
            ? t('jobProgress', { pct: Math.round(jobProgress * 100) })
            : t('jobRunning')}
        </div>
      )}

      <div className="flex items-center gap-3 px-4 h-14 shrink-0 border-b border-[#2e2e2e]">
        <Trash2 size={18} className="text-gray-400 shrink-0" />
        <h1 className="text-[15px] font-semibold text-white font-display">{t('title')}</h1>
        <span data-testid="trash-count" className="text-sm text-gray-500 tabular-nums">
          {t('postsCount', { n: total.toLocaleString() })}
        </span>
        <div className="flex-1" />
        {!selectMode ? (
          <>
            {total > 0 && (
              <button
                data-testid="trash-select-toggle"
                onClick={() => setSelectMode(true)}
                title={t('selectTitle')}
                className="u-press flex items-center gap-1.5 px-3 py-1.5 rounded-md text-sm text-gray-400 hover:text-white hover:bg-[#1a1a1a] transition-colors"
              >
                <CheckSquare size={15} />
                {t('select')}
              </button>
            )}
            <button
              data-testid="trash-empty"
              disabled={total === 0}
              onClick={handleEmpty}
              title={t('emptyTrashHint')}
              className={[
                'u-press flex items-center gap-1.5 px-3 py-1.5 rounded-md text-sm transition-colors disabled:opacity-40 disabled:pointer-events-none',
                confirmEmpty
                  ? 'bg-red-500/20 text-red-300 hover:bg-red-500/30'
                  : 'text-red-400/90 hover:bg-red-500/10 hover:text-red-300',
              ].join(' ')}
            >
              <Trash2 size={15} />
              {confirmEmpty ? t('emptyTrashConfirm') : t('emptyTrash')}
            </button>
          </>
        ) : (
          <>
            <span
              data-testid="trash-selection-count"
              className="text-sm font-medium text-gray-200 tabular-nums"
            >
              {t('selectedCount', { n: selectedCount.toLocaleString() })}
            </span>
            {total > 0 && (
              <button
                data-testid="trash-select-all"
                onClick={handleSelectAll}
                className="u-press flex items-center gap-1.5 px-2.5 py-1 rounded-md text-sm text-[#b9a6ff] hover:bg-[#7B5CFF]/15 transition-colors"
              >
                <ListChecks size={15} />
                {allSelected
                  ? t('deselectAll')
                  : total > posts.length
                    ? t('selectAllN', { n: total.toLocaleString() })
                    : t('selectAll')}
              </button>
            )}
            <button
              data-testid="trash-restore"
              disabled={selectedCount === 0}
              onClick={() => (confirmRestore ? handleRestore() : setConfirmRestore(true))}
              className="u-press flex items-center gap-1.5 px-3 py-1.5 rounded-md text-sm font-medium text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] transition-colors disabled:opacity-40 disabled:pointer-events-none"
            >
              <RotateCcw size={15} />
              {confirmRestore
                ? t('restoreSelectedConfirm', { n: selectedCount })
                : t('restoreSelected')}
            </button>
            <button
              data-testid="trash-select-cancel"
              onClick={exitSelectMode}
              title={t('exitSelectionTitle')}
              className="u-press flex items-center justify-center w-8 h-8 rounded-md text-gray-400 hover:text-white hover:bg-white/10"
            >
              <X size={16} />
            </button>
          </>
        )}
      </div>

      <p className="px-4 py-2 text-[11px] text-gray-500 border-b border-[#1e1e1e]">
        {t('retention', { days: retentionDays })}
      </p>

      <div
        ref={scrollRef}
        className="flex-1 overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e] scrollbar-track-transparent"
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

        {isEmpty && (
          <div
            data-testid="trash-empty-state"
            className="flex flex-col items-center justify-center h-full min-h-[60vh] gap-3 text-center px-6"
          >
            <ImageOff size={36} className="text-[#333]" strokeWidth={1} />
            {error ? (
              <p className="text-red-400 text-sm leading-relaxed max-w-xs">{error}</p>
            ) : (
              <p className="text-[#555] text-sm leading-relaxed max-w-xs font-display">
                {t('emptyStateTitle')} <span className="text-[#444]">{t('emptyStateHint')}</span>
              </p>
            )}
          </div>
        )}

        <div ref={sentinelRef} className="h-1" aria-hidden="true" />
      </div>
    </div>
  );
}
