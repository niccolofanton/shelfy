import { useState, useEffect, useCallback, useRef, startTransition } from 'react';
import { useT } from '../i18n';
import { toApiFilters } from '../lib/postFilters';
import { useShelfy } from '../api/ShelfyProvider';
import { errorMessageKey } from '../api/errors';
import { colsForWidth } from '../components/VirtualPostGrid';
import { applyStep } from './useGridSize';

// UI-side filter bag accepted by the Gallery surfaces. Mirrors the fields
// toApiFilters reads; pagination `limit` is the only window control here.
export interface PostFilters {
  platform?: string;
  source?: string;
  mediaType?: string;
  downloadStatus?: string;
  search?: string;
  collectionId?: number | null;
  category?: string;
  contentType?: string;
  tag?: string;
  aiTagged?: string;
  concepts?: string[];
  conceptMode?: string;
  sortOrder?: string;
  limit?: number;
}

export interface UsePostsOptions {
  // false while the kept-alive view is hidden; live updates are then deferred to
  // a single reload on reactivation.
  active?: boolean;
}

export interface UsePostsResult {
  posts: Shelfy.Post[];
  loading: boolean;
  refreshing: boolean;
  error: string | null;
  total: number;
  reload: () => void;
}

// Deep value equality limited to the JSON-shaped data a post row carries
// (scalars, arrays, plain objects). Used to decide whether a re-fetched row is
// actually different from the one already rendered.
export function postsEqual(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (Array.isArray(a)) {
    if (!Array.isArray(b) || a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) {
      if (!postsEqual(a[i], b[i])) return false;
    }
    return true;
  }
  if (a && b && typeof a === 'object' && typeof b === 'object') {
    const objA = a as Record<string, unknown>;
    const objB = b as Record<string, unknown>;
    const keysA = Object.keys(objA);
    if (keysA.length !== Object.keys(objB).length) return false;
    for (const k of keysA) {
      if (!postsEqual(objA[k], objB[k])) return false;
    }
    return true;
  }
  return false;
}

// Reconcile a freshly fetched page against the rendered list: rows whose
// content is unchanged keep their previous object identity, so PostCard's
// React.memo keeps skipping them across live refreshes. Returns `prev` itself
// when nothing changed at all (no grid re-render).
export function reconcilePosts(prev: Shelfy.Post[], next: Shelfy.Post[]): Shelfy.Post[] {
  if (!prev.length) return next;
  const prevById = new Map<string, Shelfy.Post>();
  for (const p of prev) prevById.set(p.id, p);
  let unchanged = next.length === prev.length;
  const out = next.map((p, i) => {
    const old = prevById.get(p.id);
    if (old && postsEqual(old, p)) {
      if (unchanged && prev[i] !== old) unchanged = false;
      return old;
    }
    unchanged = false;
    return p;
  });
  return unchanged ? prev : out;
}

// Windowing (plan §2.19 "the loaded list is windowed: keep ±1,000 items
// around the viewport"): Gallery's infinite scroll only ever appends forward,
// so the viewport sits at the loaded window's tail — bounding the window to
// 2,000 (±1,000 either side of that tail) caps how much of a 20k-post library
// a long scroll session keeps in memory/DOM-adjacent arrays, without ever
// touching rows still on or near screen.
export const MAX_LOADED_POSTS = 2000;

// Columns in the CURRENT grid layout, so a trim lands on a row boundary (a
// trim that split a row would reshuffle which posts share it). Mirrors
// VirtualPostGrid's own breakpoint math; `applyStep` with no second argument
// reads the live shared zoom step, so this needn't subscribe to it like a
// component would.
function estimatedColumns(): number {
  const width = typeof window !== 'undefined' ? window.innerWidth : 1280;
  return Math.max(1, applyStep(colsForWidth(width)));
}

// Drops posts from the FRONT of `list` down to at most `max`, rounded to a
// whole `cols`-wide row. The front is always the furthest-scrolled-past end
// during forward infinite scroll, so this never touches the viewport itself —
// only what's comfortably above it. Returns `list` unchanged (same
// reference) when it's already within budget, so a reconciled-but-unchanged
// page (see reconcilePosts) still bails out of re-rendering downstream.
export function windowPosts(list: Shelfy.Post[], max: number, cols: number): Shelfy.Post[] {
  const over = list.length - max;
  if (over <= 0) return list;
  const safeCols = Math.max(1, Math.floor(cols) || 1);
  const trim = Math.ceil(over / safeCols) * safeCols;
  return trim >= list.length ? [] : list.slice(trim);
}

/**
 * Custom hook for fetching and managing the post list in the Gallery view.
 *
 * @param {Object} filters
 * @param {string}  filters.platform  - 'all' | 'instagram' | 'twitter'
 * @param {string}  filters.mediaType - 'all' | specific media type
 * @param {string}  filters.search    - free-text search string
 * @param {number}  filters.limit     - page size (Gallery increments this for infinite scroll)
 * @param {Object}  [options]
 * @param {boolean} [options.active=true] - false while the kept-alive view is hidden;
 *   live updates are then deferred to a single reload on reactivation.
 *
 * @returns {{ posts: Array, loading: boolean, refreshing: boolean, error: string|null, total: number, reload: Function }}
 */
export function usePosts(filters: PostFilters, options: UsePostsOptions = {}): UsePostsResult {
  const active = options.active !== false;
  const t = useT('errors');
  const client = useShelfy();
  const [posts, setPosts] = useState<Shelfy.Post[]>([]);
  // `loading` covers user-driven fetches (initial load, filter changes, manual
  // reload, infinite-scroll pages); `refreshing` covers background live reloads
  // so they never flash the spinner or move the scroll sentinel.
  const [loading, setLoading] = useState<boolean>(false);
  const [refreshing, setRefreshing] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  const [total, setTotal] = useState<number>(0);

  // A counter that, when bumped, forces a re-fetch even if filters haven't changed.
  const [reloadCounter, setReloadCounter] = useState<number>(0);

  // Track the AbortController for any in-flight request so we can cancel it.
  const abortRef = useRef<AbortController | null>(null);

  // Where the next page starts: the cursor of the last page fetched (null once
  // the last page is loaded: an append then has nothing to fetch), and how many
  // rows the backend has served for the current query. Both are server-side
  // positions, updated as soon as a page lands: the rendered list catches up
  // later (startTransition), and sizing an append from it would overshoot.
  const nextCursorRef = useRef<string | null>(null);
  const fetchedRef = useRef<number>(0);

  // Set true for the *next* fetch only when it's triggered by a background
  // live-update (download/analyze/newPosts), so that fetch reports through
  // `refreshing` and reconciles by id instead of replacing the list.
  const liveReloadRef = useRef<boolean>(false);

  // Mirrors read by the once-only subscription effect below.
  const filtersRef = useRef<PostFilters>(filters);
  filtersRef.current = filters;
  const activeRef = useRef<boolean>(active);
  activeRef.current = active;
  // Live events that arrive while the view is hidden set this flag; reactivation
  // performs a single reload instead of one every ~2s in the background.
  const dirtyRef = useRef<boolean>(false);

  // Stable reload function — increments the counter to trigger the effect.
  const reload = useCallback((): void => {
    liveReloadRef.current = false;
    setReloadCounter((n) => n + 1);
  }, []);

  // Stable string key for the concepts array so the effect doesn't re-run on
  // every render when a non-memoized array (same contents, new identity) is
  // passed in by the caller.
  const conceptsKey = filters.concepts && filters.concepts.length ? filters.concepts.join('|') : '';

  // Signature of every filter except the pagination window; a change means the
  // result set itself changed and the list must be re-fetched from offset 0.
  const filtersSig = JSON.stringify([
    filters.platform,
    filters.source,
    filters.mediaType,
    filters.downloadStatus,
    filters.search,
    filters.collectionId,
    filters.category,
    filters.contentType,
    filters.tag,
    filters.aiTagged,
    conceptsKey,
    filters.conceptMode,
    filters.sortOrder,
  ]);
  const prevSigRef = useRef<string | null>(null);
  const prevReloadRef = useRef<number>(reloadCounter);
  const prevLimitRef = useRef<number | undefined>(filters.limit);

  // -----------------------------------------------------------------------
  // Core fetch effect. Three kinds of run:
  //   • replace — filters changed / manual reload: fetch the window from the start.
  //   • append  — only `limit` grew (infinite scroll): fetch just the missing
  //     page after the last one (its cursor) and concatenate, instead of
  //     re-querying (and re-transferring) the whole already-loaded window on
  //     every scroll step.
  //   • live    — background event: replace + reconcile by id under `refreshing`.
  // -----------------------------------------------------------------------
  useEffect(() => {
    const limit = filters.limit || 50;
    const sigChanged = filtersSig !== prevSigRef.current;
    const reloadBumped = reloadCounter !== prevReloadRef.current;
    const limitGrew = limit > (prevLimitRef.current || 50);
    prevSigRef.current = filtersSig;
    prevReloadRef.current = reloadCounter;
    prevLimitRef.current = limit;

    // A live reload only counts as such when no user action landed in the same
    // run (a filter change or scroll growth always wins).
    const isLive = liveReloadRef.current && !sigChanged && !limitGrew;
    liveReloadRef.current = false;

    const fetched = fetchedRef.current;
    const isAppend = !sigChanged && !reloadBumped && limitGrew && fetched > 0;

    const query = toApiFilters(filters);
    const pageLimit = isAppend ? limit - fetched : limit;
    const cursor = isAppend ? nextCursorRef.current : null;
    // Nothing more to append (the last page is loaded): leave any in-flight
    // request alone.
    if (isAppend && (pageLimit <= 0 || !cursor)) return;

    // Cancel any previous in-flight request; the client drops its fetch.
    abortRef.current?.abort();
    const thisRequest = new AbortController();
    abortRef.current = thisRequest;
    // A new query: until this run lands, an append has nothing valid to
    // continue from. (A live reload keeps the shown list, and its position,
    // until it lands.)
    if (!isAppend && !isLive) {
      nextCursorRef.current = null;
      fetchedRef.current = 0;
    }

    const setBusy = isLive ? setRefreshing : setLoading;
    // This run just aborted any in-flight request of the other kind; clear its
    // flag too so an aborted fetch can't leave a stale spinner behind.
    const setOther = isLive ? setLoading : setRefreshing;

    (async () => {
      setOther(false);
      setBusy(true);
      if (!isLive) setError(null);

      try {
        const result = await client.listPosts(query, {
          limit: pageLimit,
          cursor,
          signal: thisRequest.signal,
        });

        // Bail out if a newer request has started since this one was fired.
        if (thisRequest.signal.aborted) return;

        // Defensive: a malformed result must not set posts to undefined — that
        // crashes downstream consumers (posts.map / posts.length) at render
        // time, which the awaited try/catch can't recover.
        const page = Array.isArray(result?.posts) ? result.posts : [];
        nextCursorRef.current = result.nextCursor;
        fetchedRef.current = (isAppend ? fetched : 0) + page.length;
        // startTransition: a page landing (infinite scroll) or a live refresh is a
        // non-urgent list update — marking it lets React keep an in-progress scroll
        // responsive instead of blocking a frame on the reconciliation/commit.
        if (isAppend) {
          // Dedupe by id: rows inserted at the top since the previous page can
          // shift the offset window and re-serve already-loaded posts.
          startTransition(() => {
            setPosts((prev) => {
              const seen = new Set(prev.map((p) => p.id));
              const fresh = page.filter((p) => !seen.has(p.id));
              const next = fresh.length ? [...prev, ...fresh] : prev;
              return windowPosts(next, MAX_LOADED_POSTS, estimatedColumns());
            });
          });
        } else {
          startTransition(() =>
            setPosts((prev) =>
              windowPosts(reconcilePosts(prev, page), MAX_LOADED_POSTS, estimatedColumns()),
            ),
          );
        }
        // A later page may come without the total: keep the first page's.
        if (typeof result.total === 'number') setTotal(result.total);
      } catch (err) {
        if (thisRequest.signal.aborted) return;
        console.error('[usePosts] fetch error:', err);
        // A web API failure says what went wrong with its problem code; the
        // desktop's IPC errors have no code and keep their message.
        const codeKey = errorMessageKey(err);
        if (!isLive) {
          setError(
            codeKey ? t(codeKey) : ((err instanceof Error ? err.message : null) ?? t('loadPosts')),
          );
        }
      } finally {
        if (!thisRequest.signal.aborted) {
          setBusy(false);
        }
      }
    })();
    // No cleanup: the next run that fetches cancels this request (see above),
    // and the unmount effect below cancels it on unmount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filtersSig, filters.limit, reloadCounter, t, client]);

  // Unmount: drop the in-flight request so its result is ignored.
  useEffect(() => () => abortRef.current?.abort(), []);

  // Reactivation of a kept-alive view: if live events were deferred while the
  // view was hidden, reconcile once now.
  useEffect(() => {
    if (active && dirtyRef.current) {
      dirtyRef.current = false;
      liveReloadRef.current = true;
      setReloadCounter((n) => n + 1);
    }
  }, [active]);

  // -----------------------------------------------------------------------
  // Keep the grid live while background work runs. Four client events feed it
  // (on the desktop: interceptor:newPosts, download:progress, analyze:progress;
  // on the web: the SSE stream's posts.changed and resync):
  //   • posts.changed — scraping / sync / web placeholders / edits change rows
  //   • post.stored   — a post's media landed (local asset path written)
  //   • post.analyzed — a completed analysis wrote ai_tags / ai_status
  //   • resync        — live events were lost: the list may be stale
  // so the gallery reflects new media and tags as they land, not only at the end.
  //
  // Finished jobs carry the post id, so when the active filters can't change
  // the row's membership in the result set we patch that single row in place
  // (getPostsByIds) instead of re-fetching the whole window.
  // Everything else funnels into a coalesced reload: a plain trailing debounce
  // would *never* fire during a continuous burst (each event resets the timer),
  // so we add a maxWait — the reload still coalesces a flurry of events (QUIET
  // window) but is guaranteed to fire at least once every MAX_WAIT.
  // -----------------------------------------------------------------------
  useEffect(() => {
    const QUIET = 400; // settle window after the last event
    const MAX_WAIT = 2000; // force a refresh at least this often during a burst
    let quietTimer: ReturnType<typeof setTimeout> | null = null;
    let maxTimer: ReturnType<typeof setTimeout> | null = null;

    const fire = (): void => {
      if (quietTimer) clearTimeout(quietTimer);
      if (maxTimer) clearTimeout(maxTimer);
      quietTimer = null;
      maxTimer = null;
      // Mark this reload as live so the core fetch reconciles under `refreshing`.
      liveReloadRef.current = true;
      setReloadCounter((n) => n + 1);
    };
    const schedule = (): void => {
      if (!activeRef.current) {
        dirtyRef.current = true;
        return;
      }
      if (quietTimer) clearTimeout(quietTimer);
      quietTimer = setTimeout(fire, QUIET);
      if (!maxTimer) maxTimer = setTimeout(fire, MAX_WAIT);
    };

    const patchPost = async (postId: string): Promise<void> => {
      try {
        const rows = await client.getPostsByIds([postId]);
        const fresh = Array.isArray(rows) ? rows[0] : null;
        if (!fresh) return;
        setPosts((prev) => {
          const i = prev.findIndex((p) => p.id === fresh.id);
          if (i === -1 || postsEqual(prev[i], fresh)) return prev;
          const next = prev.slice();
          next[i] = fresh;
          return next;
        });
      } catch (err) {
        console.error('[usePosts] patch error:', err);
        schedule(); // fall back to the coalesced reload
      }
    };

    const offNew = client.on('posts.changed', () => schedule());
    const offResync = client.on('resync', () => schedule());
    // Only finished jobs change what listPosts returns (the client drops the
    // mid-progress ticks, which would just thrash the grid).
    const offDownload = client.on('post.stored', (job) => {
      if (!activeRef.current) {
        dirtyRef.current = true;
        return;
      }
      const f = filtersRef.current;
      // A finished download can move the row in/out of a downloadStatus filter;
      // only then is the full reload needed.
      const membershipMayChange = f.downloadStatus && f.downloadStatus !== 'all';
      if (job.postId != null && !membershipMayChange) {
        patchPost(job.postId);
      } else {
        schedule();
      }
    });
    const offAnalyze = client.on('post.analyzed', (job) => {
      if (!activeRef.current) {
        dirtyRef.current = true;
        return;
      }
      const f = filtersRef.current;
      // Fresh AI fields can change the row's membership in any of these filters.
      const membershipMayChange = !!(
        (f.aiTagged && f.aiTagged !== 'all') ||
        f.tag ||
        f.category ||
        f.contentType ||
        f.search ||
        (f.concepts && f.concepts.length)
      );
      if (job.postId != null && !membershipMayChange) {
        patchPost(job.postId);
      } else {
        schedule();
      }
    });

    return () => {
      if (quietTimer) clearTimeout(quietTimer);
      if (maxTimer) clearTimeout(maxTimer);
      offNew();
      offResync();
      offDownload();
      offAnalyze();
    };
  }, [client]); // the client is stable: we only subscribe once

  return { posts, loading, refreshing, error, total, reload };
}
