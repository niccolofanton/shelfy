import { useState, useEffect, useCallback, useRef, startTransition } from 'react';
import { useT } from '../i18n';
import { toApiFilters } from '../lib/postFilters';

// Deep value equality limited to the JSON-shaped data a post row carries
// (scalars, arrays, plain objects). Used to decide whether a re-fetched row is
// actually different from the one already rendered.
export function postsEqual(a, b) {
  if (a === b) return true;
  if (Array.isArray(a)) {
    if (!Array.isArray(b) || a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) {
      if (!postsEqual(a[i], b[i])) return false;
    }
    return true;
  }
  if (a && b && typeof a === 'object' && typeof b === 'object') {
    const keysA = Object.keys(a);
    if (keysA.length !== Object.keys(b).length) return false;
    for (const k of keysA) {
      if (!postsEqual(a[k], b[k])) return false;
    }
    return true;
  }
  return false;
}

// Reconcile a freshly fetched page against the rendered list: rows whose
// content is unchanged keep their previous object identity, so PostCard's
// React.memo keeps skipping them across live refreshes. Returns `prev` itself
// when nothing changed at all (no grid re-render).
export function reconcilePosts(prev, next) {
  if (!prev.length) return next;
  const prevById = new Map();
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
export function usePosts(filters, options = {}) {
  const active = options.active !== false;
  const t = useT('errors');
  const [posts, setPosts] = useState([]);
  // `loading` covers user-driven fetches (initial load, filter changes, manual
  // reload, infinite-scroll pages); `refreshing` covers background live reloads
  // so they never flash the spinner or move the scroll sentinel.
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState(null);
  const [total, setTotal] = useState(0);

  // A counter that, when bumped, forces a re-fetch even if filters haven't changed.
  const [reloadCounter, setReloadCounter] = useState(0);

  // Track the AbortController for any in-flight request so we can cancel it.
  const abortRef = useRef(null);

  // Set true for the *next* fetch only when it's triggered by a background
  // live-update (download/analyze/newPosts), so that fetch reports through
  // `refreshing` and reconciles by id instead of replacing the list.
  const liveReloadRef = useRef(false);

  // Mirrors read by the once-only subscription effect below.
  const postsRef = useRef(posts);
  postsRef.current = posts;
  const filtersRef = useRef(filters);
  filtersRef.current = filters;
  const activeRef = useRef(active);
  activeRef.current = active;
  // Live events that arrive while the view is hidden set this flag; reactivation
  // performs a single reload instead of one every ~2s in the background.
  const dirtyRef = useRef(false);

  // Stable reload function — increments the counter to trigger the effect.
  const reload = useCallback(() => {
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
  const prevSigRef = useRef(null);
  const prevReloadRef = useRef(reloadCounter);
  const prevLimitRef = useRef(filters.limit);

  // -----------------------------------------------------------------------
  // Core fetch effect. Three kinds of run:
  //   • replace — filters changed / manual reload: fetch the window from 0.
  //   • append  — only `limit` grew (infinite scroll): fetch just the missing
  //     page at `offset: loaded` and concatenate, instead of re-querying (and
  //     re-transferring) the whole already-loaded window on every scroll step.
  //   • live    — background event: replace + reconcile by id under `refreshing`.
  // -----------------------------------------------------------------------
  useEffect(() => {
    // Signal to cancel any previous in-flight request.
    if (abortRef.current) {
      abortRef.current.aborted = true;
    }
    const thisRequest = { aborted: false };
    abortRef.current = thisRequest;

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

    const loaded = postsRef.current.length;
    const isAppend = !sigChanged && !reloadBumped && limitGrew && loaded > 0;

    const base = toApiFilters(filters);
    const apiFilters = isAppend
      ? { ...base, limit: limit - loaded, offset: loaded }
      : { ...base, limit, offset: 0 };
    if (isAppend && apiFilters.limit <= 0) return undefined;

    const setBusy = isLive ? setRefreshing : setLoading;
    // This run just aborted any in-flight request of the other kind; clear its
    // flag too so an aborted fetch can't leave a stale spinner behind.
    const setOther = isLive ? setLoading : setRefreshing;

    (async () => {
      setOther(false);
      setBusy(true);
      if (!isLive) setError(null);

      try {
        const result = await window.electronAPI.getPosts(apiFilters);

        // Bail out if a newer request has started since this one was fired.
        if (thisRequest.aborted) return;

        // Defensive: a malformed / empty IPC result (undefined, or missing
        // `posts`) must not set posts to undefined — that crashes downstream
        // consumers (posts.map / posts.length) at render time, which the awaited
        // try/catch can't recover.
        const page = Array.isArray(result?.posts) ? result.posts : [];
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
              return fresh.length ? [...prev, ...fresh] : prev;
            });
          });
        } else {
          startTransition(() => setPosts((prev) => reconcilePosts(prev, page)));
        }
        setTotal(result?.total ?? 0);
      } catch (err) {
        if (thisRequest.aborted) return;
        console.error('[usePosts] fetch error:', err);
        if (!isLive) setError(err?.message ?? t('loadPosts'));
      } finally {
        if (!thisRequest.aborted) {
          setBusy(false);
        }
      }
    })();

    return () => {
      // Mark the request as aborted so any already-dispatched async call
      // ignores its result.
      thisRequest.aborted = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filtersSig, filters.limit, reloadCounter, t]);

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
  // Keep the grid live while background work runs. Three push channels feed it:
  //   • interceptor:newPosts — scraping / sync / web placeholders insert rows
  //   • download:progress    — a completed download writes the local asset path
  //   • analyze:progress     — a completed analysis writes ai_tags / ai_status
  // so the gallery reflects new media and tags as they land, not only at the end.
  //
  // Completed download/analyze jobs carry the post id, so when the active
  // filters can't change the row's membership in the result set we patch that
  // single row in place (getPostsByIds) instead of re-fetching the whole window.
  // Everything else funnels into a coalesced reload: a plain trailing debounce
  // would *never* fire during a continuous burst (each event resets the timer),
  // so we add a maxWait — the reload still coalesces a flurry of events (QUIET
  // window) but is guaranteed to fire at least once every MAX_WAIT.
  // -----------------------------------------------------------------------
  useEffect(() => {
    const QUIET = 400; // settle window after the last event
    const MAX_WAIT = 2000; // force a refresh at least this often during a burst
    let quietTimer = null;
    let maxTimer = null;

    const fire = () => {
      clearTimeout(quietTimer);
      clearTimeout(maxTimer);
      quietTimer = null;
      maxTimer = null;
      // Mark this reload as live so the core fetch reconciles under `refreshing`.
      liveReloadRef.current = true;
      setReloadCounter((n) => n + 1);
    };
    const schedule = () => {
      if (!activeRef.current) {
        dirtyRef.current = true;
        return;
      }
      clearTimeout(quietTimer);
      quietTimer = setTimeout(fire, QUIET);
      if (!maxTimer) maxTimer = setTimeout(fire, MAX_WAIT);
    };

    const patchPost = async (postId) => {
      try {
        const rows = await window.electronAPI.getPostsByIds([postId]);
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

    const api = window.electronAPI;
    const offNew = api.onNewPosts(() => schedule());
    // Only completed jobs change what getPosts returns; mid-progress ticks would
    // just thrash the grid, so we ignore everything but the terminal 'done'.
    const offDownload = api.onDownloadProgress?.((job) => {
      if (job?.status !== 'done') return;
      if (!activeRef.current) {
        dirtyRef.current = true;
        return;
      }
      const f = filtersRef.current;
      // A finished download can move the row in/out of a downloadStatus filter;
      // only then is the full reload needed.
      const membershipMayChange = f.downloadStatus && f.downloadStatus !== 'all';
      if (job.postId != null && api.getPostsByIds && !membershipMayChange) {
        patchPost(job.postId);
      } else {
        schedule();
      }
    });
    const offAnalyze = api.onAnalyzeProgress?.((job) => {
      if (job?.status !== 'done') return;
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
      if (job.postId != null && api.getPostsByIds && !membershipMayChange) {
        patchPost(job.postId);
      } else {
        schedule();
      }
    });

    return () => {
      clearTimeout(quietTimer);
      clearTimeout(maxTimer);
      offNew?.();
      offDownload?.();
      offAnalyze?.();
    };
  }, []); // intentionally empty — we only subscribe once

  return { posts, loading, refreshing, error, total, reload };
}
