import React, { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import VirtualPostGrid from '../components/VirtualPostGrid';
import PostGridSkeleton from '../components/PostGridSkeleton';
import PostModal from '../components/PostModal';
import FilterBar from '../components/FilterBar';
import FilterDrawer from '../components/FilterDrawer';
import CollectionModal from '../components/CollectionModal';
import Popover from '../components/Popover';
import { usePosts } from '../hooks/usePosts';
import { useToast } from '../hooks/useToast';
import { toApiFilters } from '../lib/postFilters';
import { useDownloadPrefs } from '../hooks/useDownloadPrefs';
import { useRangeSelect } from '../hooks/useRangeSelect';
import { useT } from '../i18n';
import {
  RefreshCw,
  ImageOff,
  CheckSquare,
  X,
  FolderPlus,
  Plus,
  Check,
  Sparkles,
  ListChecks,
  Download,
  Trash2,
  ArrowDownUp,
  ChevronDown,
  Eraser,
  TagsIcon,
  Info,
  CloudDownload,
  Loader2,
} from 'lucide-react';

const LOAD_BATCH = 50;
// Infinite scroll PREFETCHES: the window grows by PAGE_BATCH whenever the
// sentinel comes within PREFETCH_PX of the scroller's bottom edge, so the next
// page loads while the bottom is still screens away. PAGE_BATCH is sized so a
// single batch is taller than the prefetch zone at every grid density (worst
// case 10 cols × ~136px rows ≈ 3400px) — each load pushes the sentinel back out
// of the zone, keeping observer events flowing.
const PAGE_BATCH = 250;
const PREFETCH_PX = 3000;

// Date-sort options — surfaced directly above the list (not in the filters menu).
// Labels are resolved at render time from the `gallery` namespace.
const SORT_OPTIONS = [
  { value: 'newest', labelKey: 'sortNewest' },
  { value: 'oldest', labelKey: 'sortOldest' },
];

export default function Gallery({
  platform = 'all',
  collectionId = null,
  collectionLabel,
  collectionColor,
  sourceNonce = 0,
  collections = [],
  // Sidebar-mirrored source picker shown inside the filters drawer. `stats` feeds
  // the per-source counts, `activeSource` drives the highlight, `onSelectSource`
  // routes a pick back up to App (same path as the sidebar).
  stats = {},
  activeSource,
  onSelectSource,
  onCreateCollection,
  onAssigned,
  // Refresh App-level stats (sidebar grand total + per-platform counts). onAssigned
  // only reloads collections, so a plain gallery delete must call this to avoid
  // stale sidebar counts. Wire it to App's refreshStats (see crossFile note).
  onStatsChanged,
  // Web-post actions routed up to App (navigate to the Websites panel / re-run
  // capture+analysis with overwrite). Undefined for non-web contexts.
  onOpenInWebsites,
  onReanalyzeWeb,
  // Background source-sync: per-platform job map + the start/stop handler for
  // the toolbar button (shown only when the current source maps to a connector).
  sourceSyncJobs = {},
  onSyncSource,
  // Kept-alive views stay mounted but hidden; `active` is false while off-screen
  // so the infinite-scroll sentinel doesn't keep paging in the background.
  active = true,
}) {
  const t = useT('gallery');
  const tc = useT('common');
  const [filters, setFilters] = useState({
    platform,
    source: 'all',
    collectionId: collectionId ?? undefined,
    mediaType: 'all',
    downloadStatus: 'all',
    aiTagged: 'all',
    search: '',
    category: undefined,
    contentType: undefined,
    tag: undefined,
    concepts: [],
    conceptMode: 'or',
    sortOrder: 'newest',
    limit: LOAD_BATCH,
  });

  // Apply the source chosen from the sidebar. Keyed on sourceNonce so re-clicking
  // the same source re-applies it; skipped on initial mount since the source is
  // already set in the initial filters above.
  const firstRun = useRef(true);
  useEffect(() => {
    if (firstRun.current) {
      firstRun.current = false;
      return;
    }
    // The post array is about to change source; a stored range-select anchor
    // would point into the old array, so reset it. (`resetAnchor` is declared
    // below with the multi-select hook; effects run post-render, so the
    // binding is initialized by then.)
    resetAnchor();
    setFilters((prev) => ({
      ...prev,
      platform,
      collectionId: collectionId ?? undefined,
      limit: LOAD_BATCH,
    }));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sourceNonce]);

  const { posts, loading, total, reload, error } = usePosts(filters, { active });

  // Multi-select set + Shift+click range logic, shared with the Websites view.
  // Declared right after usePosts (it tracks the loaded array) and before the
  // callbacks/effects below that reset the anchor or clear the selection.
  const { selected, setSelected, toggleAt, resetAnchor, clearSelection } = useRangeSelect(
    posts,
    (p) => p.id,
  );

  // Asset-type download preferences (shared with Settings / the Downloads view),
  // used to scope the bulk "Scarica" action to the file types the user wants.
  const { selectedTypes } = useDownloadPrefs();

  const [activePost, setActivePost] = useState(null);

  // Right-hand filters drawer (sources + media/download/AI filters). Toggled by
  // the Filtri button in the FilterBar; stays open while picking a source.
  const [drawerOpen, setDrawerOpen] = useState(false);

  // ── AI suggested filter tags ──────────────────────────────────────────────
  // When the text filter is non-empty, the local model proposes a few related
  // filter tags (e.g. "AirPods" → cuffie, Apple, musica, design) shown as
  // clickable chips below the search bar. Clicking one adds it to the query as a
  // concept filter (OR by default; the AND/OR toggle narrows).
  const [suggestedTags, setSuggestedTags] = useState([]);
  const [suggestLoading, setSuggestLoading] = useState(false);
  const aiReqIdRef = useRef(0);

  useEffect(() => {
    const query = (filters.search || '').trim();
    // A changed query invalidates the old suggestions and any active concepts.
    setSuggestedTags([]);
    setFilters((prev) =>
      prev.concepts && prev.concepts.length ? { ...prev, concepts: [] } : prev,
    );
    if (!query) {
      setSuggestLoading(false);
      return undefined;
    }
    const reqId = ++aiReqIdRef.current;
    setSuggestLoading(true);
    // `cancelled` complements the reqId race guard: reqId only advances when a
    // NEW non-empty query schedules a request, so on its own it can't stop an
    // in-flight response from landing after the query was cleared or the view
    // unmounted. The cleanup flips the flag so a late resolution never setStates.
    let cancelled = false;
    const timer = setTimeout(async () => {
      try {
        const res = await window.electronAPI.suggestSearch(query);
        // Ignore responses superseded by a newer query (race guard) or
        // belonging to a torn-down effect run.
        if (cancelled || reqId !== aiReqIdRef.current) return;
        setSuggestedTags(res?.tags || []);
      } catch {
        // Model not ready or transient failure — fail silently.
        if (!cancelled && reqId === aiReqIdRef.current) setSuggestedTags([]);
      } finally {
        if (!cancelled && reqId === aiReqIdRef.current) setSuggestLoading(false);
      }
    }, 600);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [filters.search]);

  const activeConcepts = filters.concepts || [];
  const conceptMode = filters.conceptMode || 'or';

  // Toggle a suggested tag in/out of the active concept filter set. Resets the
  // page size and the range-select anchor since the post array is about to change.
  const toggleConcept = useCallback(
    (t) => {
      resetAnchor();
      setFilters((prev) => {
        const cur = prev.concepts || [];
        const next = cur.includes(t) ? cur.filter((x) => x !== t) : [...cur, t];
        return { ...prev, concepts: next, limit: LOAD_BATCH };
      });
    },
    [resetAnchor],
  );

  const changeConceptMode = useCallback((m) => {
    setFilters((prev) => (prev.conceptMode === m ? prev : { ...prev, conceptMode: m }));
  }, []);

  const showSuggestBar =
    !!(filters.search || '').trim() && (suggestLoading || suggestedTags.length > 0);

  // ── Multi-select / assignment ─────────────────────────────────────────────
  const [selectMode, setSelectMode] = useState(false);
  const selectModeRef = useRef(selectMode);
  selectModeRef.current = selectMode;
  // Optimistic collection membership: postId -> Set<collectionId> just assigned
  // this session, merged into allInByCollection so the assign popover's green
  // check updates instantly without re-fetching the whole grid.
  const [assignedOverlay, setAssignedOverlay] = useState(() => new Map());
  const [showCreate, setShowCreate] = useState(false);
  // Single "Azioni" menu holding the bulk actions (analyze, download,
  // assign-to-source, the cleanup actions and the destructive delete);
  // select-all sits in the toolbar next to the selection counter.
  const [actionsOpen, setActionsOpen] = useState(false);
  // Two-step confirmation for the destructive bulk actions (live inside the menu).
  const [confirmClearDesc, setConfirmClearDesc] = useState(false);
  const [confirmClearTags, setConfirmClearTags] = useState(false);
  const [confirmDeletePosts, setConfirmDeletePosts] = useState(false);
  // Ids of selected posts whose media isn't on disk yet: surfaced as an actionable
  // banner after a bulk analyze so the user can download them before retrying.
  const [downloadSuggest, setDownloadSuggest] = useState(null);
  const actionsRef = useRef(null);

  // Inline feedback for bulk actions (analyze / export) — shared toast hook
  // (exit animation unused here: the banner disappears without a motion step).
  const { toast: feedback, showToast: showFeedback } = useToast();

  // PostCard's onOpen(post) doesn't forward the click event, so we capture the
  // most recent pointer event here (set on the wrapping div) to read shiftKey.
  const lastEventRef = useRef(null);

  // Normalised filters matching usePosts (shared mapper), without limit/offset —
  // used to resolve the full id set behind the current view (select-all-matching).
  const buildApiFilters = useCallback(() => toApiFilters(filters), [filters]);

  const exitSelectMode = useCallback(() => {
    setSelectMode(false);
    clearSelection();
    // Drop the optimistic membership overlay: out of bulk-edit, a stale overlay
    // would force "already in source" checks that may no longer match the DB, and
    // it would otherwise grow unbounded for the lifetime of the kept-alive view.
    setAssignedOverlay(new Map());
    // A post snapshot left in activePost must not silently re-open the modal when
    // select mode exits (the snapshot can be stale vs. background reloads).
    setActivePost(null);
    setActionsOpen(false);
    setConfirmClearDesc(false);
    setConfirmClearTags(false);
    setConfirmDeletePosts(false);
    setDownloadSuggest(null);
  }, [clearSelection]);

  // Reset the two-step confirmations whenever the actions menu closes so a
  // primed "confirm" state never lingers across re-opens.
  useEffect(() => {
    if (!actionsOpen) {
      setConfirmClearDesc(false);
      setConfirmClearTags(false);
      setConfirmDeletePosts(false);
    }
  }, [actionsOpen]);

  // Signature of everything that defines the current result set EXCEPT pagination
  // (limit) and ordering (sortOrder) — both leave the matching id set unchanged.
  // Changing any real filter invalidates a selection made in the previous view.
  const querySignature = useMemo(() => {
    const { limit, sortOrder, ...rest } = filters;
    return JSON.stringify(rest);
  }, [filters]);

  // Exit select mode whenever the defining filters change (search, source,
  // collection, tags, media type, …). Otherwise the mode stays armed over a
  // different result set and ids selected in the old view linger invisibly —
  // "0 visibili ma N selezionati". Skipped on mount and on pure pagination/sort
  // changes (signature unchanged).
  const querySigRef = useRef(querySignature);
  useEffect(() => {
    if (querySigRef.current === querySignature) return;
    querySigRef.current = querySignature;
    if (selectModeRef.current) {
      exitSelectMode();
      return;
    }
    // Outside select mode there's no selection to drop, but reconcile the rest:
    // the banner's ids belong to the old view, and a stale optimistic membership
    // overlay could mask the freshly-loaded posts' real collection membership
    // (and would otherwise grow unbounded across filter changes). Keep the empty
    // identity: a fresh Map on every settled search would re-render the grid.
    clearSelection();
    setDownloadSuggest(null);
    setAssignedOverlay((prev) => (prev.size ? new Map() : prev));
  }, [querySignature, clearSelection, exitSelectMode]);

  // Leaving the tab (the kept-alive view flips `active` off) also exits select
  // mode — coming back to a still-armed selection over a possibly-changed
  // library is surprising.
  useEffect(() => {
    if (!active && selectModeRef.current) exitSelectMode();
  }, [active, exitSelectMode]);

  // id → index map for the loaded posts, used to resolve a card's position from
  // its post object (range-select) without threading an index through props.
  const postIndexById = useMemo(() => {
    const m = new Map();
    posts.forEach((p, i) => m.set(p.id, i));
    return m;
  }, [posts]);
  // Keep a ref so the stable handleCardOpen can read the latest map.
  const postIndexByIdRef = useRef(postIndexById);
  postIndexByIdRef.current = postIndexById;
  const postsRef = useRef(posts);
  postsRef.current = posts;
  const selectedRef = useRef(selected);
  selectedRef.current = selected;

  // ── Drag-select (iPhone-Photos-like) ─────────────────────────────────────
  // In select mode, pressing on a card and sweeping the mouse over others
  // selects (or deselects, if the first card was already selected) the whole
  // range between the press point and the card currently under the pointer.
  // Moving back shrinks the range: each update re-applies the operation onto a
  // snapshot of the selection taken at press time. A sweep that actually
  // crossed onto another card suppresses the click that fires on mouseup, so
  // the start card isn't immediately toggled back.
  const dragRef = useRef(null); // { anchorIndex, mode, snapshot, lastIndex, swept }
  const dragSweptRef = useRef(false);

  const dragIndexFromEvent = (e) => {
    const el = e.target?.closest?.('[data-post-index]');
    if (!el) return null;
    const idx = Number(el.dataset.postIndex);
    return Number.isInteger(idx) && idx >= 0 ? idx : null;
  };

  // Stable identities (everything is read via refs): the memoized
  // VirtualPostGrid must not re-render on every Gallery render just because of
  // fresh inline handlers.
  const resetAnchorRef = useRef(resetAnchor);
  resetAnchorRef.current = resetAnchor;

  const handleGridMouseDown = useCallback((e) => {
    // PostCard's onOpen(post) doesn't forward the click event, so keep the most
    // recent pointer event readable for shiftKey (range-select fallback path).
    lastEventRef.current = e;
    dragSweptRef.current = false;
    // Shift+press keeps the existing anchor→click range semantics.
    if (!selectModeRef.current || e.button !== 0 || e.shiftKey) return;
    const idx = dragIndexFromEvent(e);
    if (idx == null) return;
    const post = postsRef.current[idx];
    if (!post) return;
    dragRef.current = {
      anchorIndex: idx,
      mode: selectedRef.current.has(post.id) ? 'deselect' : 'select',
      snapshot: new Set(selectedRef.current),
      lastIndex: null,
      swept: false,
    };
  }, []);

  const handleGridMouseOver = useCallback(
    (e) => {
      const drag = dragRef.current;
      if (!drag) return;
      const idx = dragIndexFromEvent(e);
      if (idx == null || idx === drag.lastIndex) return;
      // Still hovering the press card and never left it: not a sweep yet — a
      // plain click must keep its normal toggle behavior.
      if (!drag.swept && idx === drag.anchorIndex) return;
      drag.swept = true;
      drag.lastIndex = idx;
      const [lo, hi] = drag.anchorIndex <= idx ? [drag.anchorIndex, idx] : [idx, drag.anchorIndex];
      const next = new Set(drag.snapshot);
      const list = postsRef.current;
      for (let i = lo; i <= hi; i++) {
        const p = list[i];
        if (!p) continue;
        if (drag.mode === 'select') next.add(p.id);
        else next.delete(p.id);
      }
      setSelected(next);
    },
    [setSelected],
  );

  // End the sweep on mouseup anywhere (the pointer may be off-grid by then).
  // The suppression flag must be consumed by the *immediate* trailing click (if
  // any) and never linger: when the sweep ends away from a card (toolbar, off
  // window) no card `click` fires, so handleCardOpen never clears it. Clearing
  // it on the next rAF — after the synchronous trailing click has already run —
  // keeps it from swallowing a later, unrelated keyboard activation.
  useEffect(() => {
    const onUp = () => {
      if (dragRef.current?.swept) {
        dragSweptRef.current = true;
        requestAnimationFrame(() => {
          dragSweptRef.current = false;
        });
        // The sweep applied the selection by setSelected() directly, bypassing
        // useRangeSelect — so its Shift+click anchor (lastIndexRef) still points
        // at the last plain-clicked card, NOT where the drag ended. There's no
        // exported setter to advance it to drag.lastIndex (see crossFile note),
        // so drop it: a following Shift+click then falls back to a plain toggle
        // from a fresh anchor instead of spanning a stale, unrelated range.
        resetAnchorRef.current();
      }
      dragRef.current = null;
    };
    window.addEventListener('mouseup', onUp);
    return () => window.removeEventListener('mouseup', onUp);
  }, []);

  // ── Modal navigation ──────────────────────────────────────────────────────
  // Derive the active post's index from the existing id→index map instead of two
  // inline O(n) findIndex calls per render, and keep onPrev/onNext stable across
  // renders (they read postsRef) so PostModal's keydown listener isn't re-bound
  // on every Gallery render.
  const activeIndex = useMemo(
    () => (activePost ? (postIndexById.get(activePost.id) ?? -1) : -1),
    [activePost, postIndexById],
  );
  const hasPrev = activeIndex > 0;
  const hasNext = activeIndex >= 0 && activeIndex < posts.length - 1;
  const handlePrev = useCallback(() => {
    setActivePost((cur) => {
      if (!cur) return cur;
      const list = postsRef.current;
      const i = list.findIndex((p) => p.id === cur.id);
      return i > 0 ? list[i - 1] : cur;
    });
  }, []);
  const handleNext = useCallback(() => {
    setActivePost((cur) => {
      if (!cur) return cur;
      const list = postsRef.current;
      const i = list.findIndex((p) => p.id === cur.id);
      return i >= 0 && i < list.length - 1 ? list[i + 1] : cur;
    });
  }, []);

  // Stable across renders: reads volatile state via refs so PostCard's
  // React.memo isn't defeated by a fresh callback identity each render. PostCard
  // forwards the originating event as `onOpen(post, event)` so we can read
  // shiftKey directly (works for keyboard/touch, not just captured mouse events).
  // Range/toggle semantics live in useRangeSelect's toggleAt.
  const handleCardOpen = useCallback(
    (post, event) => {
      // A drag-select sweep just ended on this card: the selection was already
      // applied during the sweep, so the trailing (mouse) click must not toggle
      // it back. Only mouse activations can be the sweep's trailing click — a
      // keyboard Enter/Space activation must never be suppressed, so leave the
      // flag untouched for it (the rAF in the mouseup handler clears it anyway).
      const isPointerActivation = !event || event.type === 'click' || event.type === 'mouseup';
      if (dragSweptRef.current && isPointerActivation) {
        dragSweptRef.current = false;
        return;
      }
      const index = postIndexByIdRef.current.get(post.id) ?? -1;
      const evt = event ?? lastEventRef.current;
      if (selectModeRef.current) {
        toggleAt(post.id, index, evt?.shiftKey);
      } else {
        setActivePost(post);
      }
    },
    [toggleAt],
  );

  // Quick-select (Google-Photos-like): the hover checkbox on a card arms select
  // mode directly with that post selected, skipping the toolbar's Select button.
  // In select mode the card click already toggles, so this is a no-op there.
  const handleQuickSelect = useCallback(
    (post) => {
      if (selectModeRef.current) return; // in select mode the click on the card already toggles
      resetAnchor();
      setSelectMode(true);
      setSelected(new Set([post.id]));
    },
    [resetAnchor, setSelected],
  );

  // Optimistic: reflect the membership instantly (green check) and refresh the
  // sidebar counts in the background, without the blocking full-grid refetch the
  // old flow did. Roll the overlay back if the IPC write fails.
  const assignTo = async (cid) => {
    const ids = [...selected];
    if (ids.length === 0) return;
    // Snapshot which ids this call actually ADDS `cid` to (i.e. the overlay
    // didn't already carry it). The rollback must undo only those: an id whose
    // overlay already contained `cid` (a prior successful assignTo this session)
    // owns its green check independently, and a blind delete would falsely strip
    // it — leaving the post shown as NOT in the collection though it really is.
    const addedTo = ids.filter((id) => !assignedOverlay.get(id)?.has(cid));
    setAssignedOverlay((prev) => {
      const next = new Map(prev);
      for (const id of ids) {
        const s = new Set(next.get(id));
        s.add(cid);
        next.set(id, s);
      }
      return next;
    });
    try {
      await window.electronAPI.addPostsToCollections(ids, [cid]);
      onAssigned?.(); // sidebar source counts — cheap, non-blocking for the grid
    } catch (err) {
      console.error('[Gallery] assignTo error:', err);
      setAssignedOverlay((prev) => {
        const next = new Map(prev);
        for (const id of addedTo) {
          const s = new Set(next.get(id));
          s.delete(cid);
          if (s.size) next.set(id, s);
          else next.delete(id);
        }
        return next;
      });
      showFeedback(t('fbAssignError'));
    }
  };

  const handleCreateAndAssign = async ({ name, color }) => {
    const created = await onCreateCollection?.({ name, color });
    if (created?.id) await assignTo(created.id);
  };

  // ── Bulk actions: analyze, select-all-matching ───────────────────────────
  const handleAnalyzeSelected = async () => {
    const ids = [...selected];
    if (ids.length === 0) return;
    // Gate on model readiness (mirrors PostModal / AiTagsQueue): without a model on
    // disk every enqueued job throws MODEL_NOT_READY and is silently marked 'error'.
    try {
      const status = await window.electronAPI.getModelStatus?.();
      if (status && !status.ready) {
        showFeedback(t('fbModelFirst'));
        return;
      }
    } catch {
      // Status probe failed — fall through and let the enqueue path report below.
    }
    // The local VLM reads media off disk, so posts whose media isn't downloaded
    // can't be analyzed visually yet. Split the selection: enqueue only what's on
    // disk (or text-only bookmarks), and surface the rest as a download suggestion.
    let analyzable = ids;
    let needsDownload = [];
    try {
      const split = await window.electronAPI.splitForAnalysis?.(ids);
      if (split) {
        analyzable = split.analyzable ?? ids;
        needsDownload = split.needsDownload ?? [];
      }
    } catch (err) {
      console.error('[Gallery] splitForAnalysis error:', err);
      // Fall back to enqueuing the whole selection (the backend canAnalyze still
      // gates each post, so nothing un-analyzable slips through).
    }
    setDownloadSuggest(needsDownload.length ? needsDownload : null);
    if (analyzable.length === 0) {
      // Nothing on disk to analyze — point the user at the download step.
      showFeedback(t('fbAnalyzeNoneLocal', { n: needsDownload.length }));
      return;
    }
    try {
      const res = await window.electronAPI.analyzePosts(analyzable);
      const queued = res?.queued ?? analyzable.length;
      showFeedback(
        needsDownload.length
          ? t('fbQueuedPartial', { n: queued, skipped: needsDownload.length })
          : t('fbQueued', { n: queued }),
      );
    } catch (err) {
      console.error('[Gallery] analyzePosts error:', err);
      showFeedback(t('fbAnalyzeError'));
    }
  };

  // Download the remote-only posts surfaced by the last analyze (the banner's
  // suggested action), so they can be analyzed on the next pass.
  const handleDownloadSuggested = async () => {
    const ids = downloadSuggest || [];
    if (ids.length === 0) return;
    const types = selectedTypes();
    if (!types.length) {
      showFeedback(t('fbNoFileTypes'));
      return;
    }
    try {
      const res = await window.electronAPI.downloadPosts(ids, types);
      showFeedback(t('fbDownloading', { n: res?.queued ?? ids.length }));
      setDownloadSuggest(null);
    } catch (err) {
      console.error('[Gallery] downloadPosts (suggested) error:', err);
      showFeedback(t('fbDownloadError'));
    }
  };

  // Enqueue downloads for the selected posts (missing assets only, per the
  // backend default) using the user's asset-type preferences.
  const handleDownloadSelected = async () => {
    const ids = [...selected];
    if (ids.length === 0) return;
    const types = selectedTypes();
    if (!types.length) {
      showFeedback(t('fbNoFileTypes'));
      return;
    }
    try {
      const res = await window.electronAPI.downloadPosts(ids, types);
      showFeedback(t('fbDownloading', { n: res?.queued ?? ids.length }));
    } catch (err) {
      console.error('[Gallery] downloadPosts error:', err);
      showFeedback(t('fbDownloadError'));
    }
  };

  const handleClearDescriptions = async () => {
    const ids = [...selected];
    if (ids.length === 0) return;
    try {
      const n = await window.electronAPI.clearPostDescriptions(ids);
      showFeedback(t('fbDescriptionsCleared', { n: n ?? ids.length }));
      // The cleared posts may no longer match the active aiTagged filter and
      // vanish from the grid; reconcile the selection so we don't leave
      // "N selezionati" with 0 matching cards (the querySignature effect won't
      // fire — the filters are unchanged).
      clearSelection();
      reload();
    } catch (err) {
      console.error('[Gallery] clearPostDescriptions error:', err);
      showFeedback(t('fbClearDescriptionsError'));
    } finally {
      setConfirmClearDesc(false);
      setActionsOpen(false);
    }
  };

  const handleClearAiTags = async () => {
    const ids = [...selected];
    if (ids.length === 0) return;
    try {
      const n = await window.electronAPI.clearPostAiTags(ids);
      showFeedback(t('fbTagsCleared', { n: n ?? ids.length }));
      // See handleClearDescriptions: cleared posts may drop out of the active
      // aiTagged filter, so reconcile the now-stale selection.
      clearSelection();
      reload();
    } catch (err) {
      console.error('[Gallery] clearPostAiTags error:', err);
      showFeedback(t('fbClearTagsError'));
    } finally {
      setConfirmClearTags(false);
      setActionsOpen(false);
    }
  };

  // Permanently delete the selected posts (DB rows + on-disk files), then exit
  // selection and refresh the grid.
  const handleDeletePosts = async () => {
    const ids = [...selected];
    if (ids.length === 0) return;
    try {
      const res = await window.electronAPI.deletePosts(ids);
      // Surface the non-fatal file-unlink errors the handler returns instead of
      // dropping them silently — but in the SAME toast as the deleted count:
      // showFeedback replaces the toast state synchronously, so two back-to-back
      // calls would clobber the "N eliminati" confirmation and leave only the
      // file-error line, reading like a pure failure.
      const deletedMsg = t('fbPostsDeleted', { n: res?.deleted ?? ids.length });
      const fileErrors = Array.isArray(res?.errors) ? res.errors.length : 0;
      showFeedback(
        fileErrors
          ? `${deletedMsg} · ${t('fbFilesNotRemoved', { n: fileErrors })}`
          : deletedMsg,
      );
      exitSelectMode();
    } catch (err) {
      console.error('[Gallery] deletePosts error:', err);
      showFeedback(t('fbDeleteError'));
      setConfirmDeletePosts(false);
      setActionsOpen(false);
    } finally {
      // Always reconcile the grid with the DB: on a partial failure some rows may
      // already be deleted, so the grid would otherwise keep showing gone rows.
      reload();
      onAssigned?.(); // refresh sidebar source counts
      onStatsChanged?.(); // refresh App-level stats (sidebar total + per-platform)
    }
  };

  // Select-all toggle. When everything matching is already selected it clears the
  // selection (staying in select mode); otherwise it selects every matching post.
  // The loaded ids are applied synchronously so every visible card checks the
  // instant you click — then, when more posts match than are loaded, the full set
  // (across pagination) is resolved via getPostIds and merged in. Without the
  // optimistic step the IPC round trip leaves the click feeling unresponsive on
  // large libraries ("ci mette un po'").
  const handleSelectAll = async () => {
    if (allSelected) {
      setSelected(new Set());
      return;
    }
    const loadedIds = posts.map((p) => p.id);
    setSelected(new Set(loadedIds)); // instant feedback on the visible cards
    if (total > posts.length) {
      // Snapshot the query at click time. If the filters change during the await
      // (the querySignature effect clears the selection), a late getPostIds
      // resolution must not re-populate the selection with the OLD filter's ids.
      const sigAtCall = querySigRef.current;
      try {
        const ids = await window.electronAPI.getPostIds(buildApiFilters());
        if (querySigRef.current !== sigAtCall) return; // stale: filters changed
        const full = ids && ids.length ? ids : loadedIds;
        setSelected(new Set(full));
        showFeedback(t('fbSelectionDone', { n: full.length }));
      } catch (err) {
        console.error('[Gallery] getPostIds error:', err);
        if (querySigRef.current !== sigAtCall) return;
        showFeedback(t('fbSelectionError')); // keep the optimistic loaded selection
      }
    } else {
      showFeedback(t('fbSelectionDone', { n: loadedIds.length }));
    }
  };

  // ── Infinite scroll ─────────────────────────────────────────────────────────
  // The observer + handler read the volatile loading / posts.length / total
  // through refs, so a slow stream of post/total updates doesn't tear down and
  // recreate the IntersectionObserver each render.
  const scrollRef = useRef(null);
  const sentinelRef = useRef(null);
  const observerRef = useRef(null);
  const loadingRef = useRef(loading);
  const postsLenRef = useRef(posts.length);
  const totalRef = useRef(total);
  const activeRef = useRef(active);
  loadingRef.current = loading;
  postsLenRef.current = posts.length;
  totalRef.current = total;
  activeRef.current = active;

  // Grow the window when the sentinel sits within PREFETCH_PX of the scroller's
  // viewport. Shared by the observer and the post-load re-check below.
  // `activeRef` gates background paging while the view is kept-alive but hidden
  // (visibility:hidden keeps the sentinel's geometry, so it would otherwise keep
  // firing and load the whole library off-screen).
  const maybeLoadMore = useCallback(() => {
    if (!activeRef.current || loadingRef.current) return;
    if (postsLenRef.current >= totalRef.current) return;
    const scroller = scrollRef.current;
    const sentinel = sentinelRef.current;
    // clientHeight 0 → unmeasured layout (first frame, jsdom under test):
    // geometry would read "everything at the top" and chain-load the whole
    // library; let the observer's real intersection events drive paging instead.
    if (!scroller || !sentinel || scroller.clientHeight === 0) return;
    const distance = sentinel.getBoundingClientRect().top - scroller.getBoundingClientRect().bottom;
    if (distance < PREFETCH_PX) {
      setFilters((prev) => ({ ...prev, limit: prev.limit + PAGE_BATCH }));
    }
  }, []);

  useEffect(() => {
    // root = the inner scroll container. With the default (window) root the
    // sentinel stays clipped by this scroller until it's actually visible, so
    // any rootMargin was effectively ignored and paging always raced the user —
    // a fast fling hit the bottom and waited on the fetch.
    observerRef.current = new IntersectionObserver(maybeLoadMore, {
      root: scrollRef.current,
      rootMargin: `${PREFETCH_PX}px 0px`,
    });
    if (sentinelRef.current) observerRef.current.observe(sentinelRef.current);
    return () => observerRef.current?.disconnect();
  }, [maybeLoadMore]);

  // Observer callbacks only fire on intersection *changes*: a trigger discarded
  // while a fetch was in flight (fast fling), or a batch too short to push the
  // sentinel out of the prefetch zone, would stall paging until the next manual
  // scroll. Re-check after every load settles (and on re-activation) so
  // prefetching chains until the window outruns the zone or the library is done.
  useEffect(() => {
    if (!loading) maybeLoadMore();
  }, [loading, posts.length, total, active, maybeLoadMore]);

  // Reset limit when filter params (excluding limit) change. Preserve the
  // active collection filter (it lives outside the FilterBar UI).
  const handleFilterChange = useCallback(
    (newFilters) => {
      // Filters change the post array; drop any stored range-select anchor so a
      // following shift+click can't span the stale array.
      resetAnchor();
      setFilters((prev) => ({ ...newFilters, collectionId: prev.collectionId, limit: LOAD_BATCH }));
    },
    [resetAnchor],
  );

  const isEmpty = !loading && posts.length === 0;
  const selectedCount = selected.size;
  // True once every matching post (across pagination, not just the loaded page)
  // is selected — flips the select-all toggle to "Deseleziona tutti".
  const allSelected = total > 0 && selectedCount >= total;

  // Precompute, once per (posts, selected, collections) change, which collections
  // already contain ALL selected posts — instead of the O(N·S·P) inline scan that
  // ran for every collection on every render of the assign popover.
  const postsById = useMemo(() => {
    const m = new Map();
    for (const p of posts) m.set(p.id, p);
    return m;
  }, [posts]);
  const allInByCollection = useMemo(() => {
    const out = {};
    // Only the open Azioni menu reads this; skip the O(collections·selected)
    // scan while it's closed (selection changes during drag-select, etc.).
    if (!actionsOpen || selected.size === 0) return out;
    const ids = [...selected];
    for (const c of collections) {
      out[c.id] = ids.every(
        (id) =>
          postsById.get(id)?.collectionIds?.includes(c.id) || assignedOverlay.get(id)?.has(c.id),
      );
    }
    return out;
  }, [actionsOpen, collections, selected, postsById, assignedOverlay]);

  return (
    <div data-testid="gallery-view" className="flex h-full overflow-hidden">
      {/* Bulk-action feedback also has to survive leaving select mode: a successful
        bulk delete calls exitSelectMode() in the same commit it sets the toast, which
        unmounts the in-toolbar feedback span. This root-level overlay shows it once
        we're back in browse mode, so the "N eliminati" confirmation is never lost. */}
      {!selectMode && feedback && (
        <div
          key={feedback}
          data-testid="bulk-feedback-toast"
          className="fixed bottom-6 left-1/2 -translate-x-1/2 z-50 px-4 py-2 rounded-lg bg-[#1a1a1a] border border-[#2e2e2e] text-xs text-[#7B5CFF] tabular-nums whitespace-nowrap u-pop-in shadow-lg"
        >
          {feedback}
        </div>
      )}
      <div className="flex flex-col flex-1 min-w-0 overflow-hidden">
        {/* Unified sticky toolbar (~52px) — one strip for everything above the grid.
          Two distinct modes:
          • browse — the FilterBar: search | sort toggle | active source chip | tag
            chip | total count | grid zoom | Filtri | refresh (icon) | "Seleziona".
          • select — the same strip swaps to the contextual action bar: feedback +
            selection count + select-all + the single "Azioni" menu + exit (✕). */}
        <div
          className="sticky top-0 z-10 bg-[#0f0f0f] border-b border-[#2e2e2e]"
          style={{ minHeight: 52 }}
        >
          {!selectMode ? (
            <FilterBar
              filters={filters}
              onChange={handleFilterChange}
              total={total}
              drawerOpen={drawerOpen}
              onToggleDrawer={() => setDrawerOpen((o) => !o)}
              leading={
                <>
                  {/* Sort by date — a single toggle: tap it to flip between
                    "Più recenti" and "Meno recenti" (no second label / no menu). */}
                  {(() => {
                    const order = filters.sortOrder ?? 'newest';
                    const next = order === 'newest' ? 'oldest' : 'newest';
                    const labelKey = SORT_OPTIONS.find((o) => o.value === order)?.labelKey;
                    const label = labelKey ? t(labelKey) : '';
                    return (
                      <button
                        data-testid="sort-toggle"
                        data-order={order}
                        title={t('sortToggleTitle')}
                        onClick={() => handleFilterChange({ ...filters, sortOrder: next })}
                        className="flex items-center gap-1.5 px-2.5 py-1 rounded-md text-sm text-gray-300 hover:text-white hover:bg-[#1a1a1a] u-press transition-colors whitespace-nowrap shrink-0"
                      >
                        <ArrowDownUp size={13} className="text-gray-500" />
                        {label}
                      </button>
                    );
                  })()}

                  {collectionId && collectionLabel && (
                    <span
                      data-testid="active-collection-chip"
                      className="flex items-center gap-1.5 text-sm text-gray-300 whitespace-nowrap shrink-0 u-fade-in"
                    >
                      <span
                        className="w-2 h-2 rounded-full"
                        style={{ backgroundColor: collectionColor || '#7B5CFF' }}
                      />
                      {collectionLabel}
                    </span>
                  )}
                </>
              }
              trailing={
                <>
                  {/* Source-sync: fetches new posts from the source's connector in
                    background (Activity Center shows the progress). Only sources
                    that map to a connector listing get the button: IG / X
                    platforms, Pinterest once it has boards, native folders.
                    CloudDownload (not RefreshCw — taken by the grid refresh
                    beside it); while the run is in flight it becomes a spinner
                    and clicking it stops the run. */}
                  {(() => {
                    if (!onSyncSource) return null;
                    const c =
                      collectionId != null ? collections.find((x) => x.id === collectionId) : null;
                    const target = c
                      ? c.platform && c.externalId != null
                        ? { type: 'collection', platform: c.platform, collectionId: c.id }
                        : null
                      : platform === 'instagram' || platform === 'twitter'
                        ? { type: 'platform', platform }
                        : platform === 'pinterest' &&
                            collections.some(
                              (x) => x.platform === 'pinterest' && x.externalId != null,
                            )
                          ? { type: 'platform', platform }
                          : null;
                    if (!target) return null;
                    const job = sourceSyncJobs?.[target.platform];
                    const running =
                      !!job && (job.status === 'navigating' || job.status === 'syncing');
                    return (
                      <button
                        data-testid="gallery-sync-source"
                        onClick={() => onSyncSource(target)}
                        title={running ? t('syncStopTitle') : t('syncSourceTitle')}
                        aria-label={running ? t('syncStopTitle') : t('syncSourceTitle')}
                        className={[
                          'flex items-center justify-center w-7 h-7 rounded-md u-press shrink-0',
                          running
                            ? 'text-amber-400 hover:bg-[#1a1a1a]'
                            : 'text-gray-400 hover:text-white hover:bg-[#1a1a1a]',
                        ].join(' ')}
                      >
                        {running ? (
                          <Loader2 size={15} className="animate-spin" />
                        ) : (
                          <CloudDownload size={15} />
                        )}
                      </button>
                    );
                  })()}

                  <button
                    data-testid="gallery-refresh"
                    onClick={() => reload()}
                    disabled={loading}
                    title={t('refreshTitle')}
                    aria-label={t('refreshTitle')}
                    className="flex items-center justify-center w-7 h-7 rounded-md text-gray-400 hover:text-white hover:bg-[#1a1a1a] disabled:opacity-50 disabled:cursor-not-allowed u-press shrink-0"
                  >
                    <RefreshCw size={15} className={loading ? 'animate-spin' : undefined} />
                  </button>

                  <button
                    data-testid="select-toggle"
                    onClick={() => setSelectMode(true)}
                    title={t('selectTitle')}
                    className="flex items-center gap-1.5 px-3 py-1 rounded-md text-sm text-gray-400 hover:text-white hover:bg-[#1a1a1a] u-press whitespace-nowrap shrink-0"
                  >
                    <CheckSquare size={15} />
                    {t('select')}
                  </button>
                </>
              }
            />
          ) : (
            <div className="flex items-center gap-2 w-full px-4 h-[52px] u-fade-in-down">
              {/* Everything is pushed to the right: an empty flexer fills the left
                gutter so the count, the single "Azioni" menu and the exit (✕) sit
                together as one right-aligned cluster. */}
              <div className="flex-1" />

              {feedback && (
                <span
                  key={feedback}
                  data-testid="bulk-feedback"
                  className="text-xs text-[#7B5CFF] tabular-nums u-pop-in whitespace-nowrap"
                >
                  {feedback}
                </span>
              )}

              <span
                data-testid="selection-count"
                className="text-sm font-medium text-gray-200 tabular-nums whitespace-nowrap shrink-0"
              >
                {t('selectedCount', { n: selectedCount.toLocaleString() })}
              </span>

              {/* Select-all toggle — lives next to the counter (not in the actions
                menu) so the most common bulk gesture is one click away. */}
              {total > 0 && (
                <button
                  data-testid="select-all-matching"
                  onClick={handleSelectAll}
                  title={allSelected ? t('deselectAllTitle') : t('selectAllTitle')}
                  className="u-press flex items-center gap-1.5 px-2.5 py-1 rounded-md text-sm text-[#b9a6ff] hover:bg-[#7B5CFF]/15 whitespace-nowrap shrink-0 transition-colors"
                >
                  <ListChecks size={15} className="shrink-0" />
                  {allSelected
                    ? t('deselectAll')
                    : total > posts.length
                      ? t('selectAllN', { n: total.toLocaleString() })
                      : t('selectAll')}
                </button>
              )}

              <div className="h-5 w-px bg-[#2e2e2e] shrink-0" />

              {/* Single "Azioni" menu: holds every bulk action (analyze/download,
                assign-to-source, cleanup and the destructive delete) — select-all
                lives next to the counter above. The trigger stays enabled with
                nothing selected; the action-requiring rows disable themselves. */}
              <div className="relative shrink-0" ref={actionsRef}>
                <button
                  data-testid="bulk-actions"
                  onClick={() => setActionsOpen((o) => !o)}
                  aria-haspopup="menu"
                  aria-expanded={actionsOpen}
                  title={t('actionsTitle')}
                  className="flex items-center gap-1.5 px-3 py-1 rounded-md text-sm font-medium text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] u-press transition-colors"
                >
                  <Sparkles size={15} />
                  {t('actions')}
                  <ChevronDown
                    size={14}
                    className={['transition-transform', actionsOpen ? 'rotate-180' : ''].join(' ')}
                  />
                </button>

                <Popover
                  anchorRef={actionsRef}
                  open={actionsOpen}
                  align="right"
                  onRequestClose={() => setActionsOpen(false)}
                  data-testid="bulk-actions-menu"
                  className="w-72 bg-[#1a1a1a] border border-[#2e2e2e] rounded-lg shadow-2xl py-1 u-fade-in-down origin-top-right"
                >
                  {/* ── Azioni primarie ───────────────────────────────────── */}
                  <button
                    data-testid="bulk-analyze"
                    disabled={selectedCount === 0}
                    onClick={() => {
                      setActionsOpen(false);
                      handleAnalyzeSelected();
                    }}
                    className="u-press w-full flex items-center gap-2.5 px-3 py-2 text-[13px] text-left text-gray-300 hover:bg-[#2a2a2a] hover:text-white disabled:opacity-40 disabled:pointer-events-none transition-colors"
                  >
                    <Sparkles size={15} className="shrink-0" />
                    <span className="flex-1">{t('analyze')}</span>
                  </button>

                  <button
                    data-testid="bulk-download"
                    disabled={selectedCount === 0}
                    onClick={() => {
                      setActionsOpen(false);
                      handleDownloadSelected();
                    }}
                    className="u-press w-full flex items-center gap-2.5 px-3 py-2 text-[13px] text-left text-gray-300 hover:bg-[#2a2a2a] hover:text-white disabled:opacity-40 disabled:pointer-events-none transition-colors"
                  >
                    <Download size={15} className="shrink-0" />
                    <span className="flex-1 min-w-0">
                      <span className="block">{tc('download')}</span>
                      {/* The backend enqueues missing assets only — say so, or a
                        full-library selection looks like it re-downloads everything. */}
                      <span className="block text-[11px] text-gray-500 leading-snug">
                        {t('downloadOnlyMissingHint')}
                      </span>
                    </span>
                  </button>

                  <div className="my-1 border-t border-[#2e2e2e]" />

                  {/* ── Aggiungi a source ─────────────────────────────────── */}
                  <div
                    className={[
                      'transition-opacity',
                      selectedCount === 0 ? 'opacity-40 pointer-events-none' : '',
                    ].join(' ')}
                  >
                    <p className="px-3 pt-1.5 pb-1 text-[10px] font-semibold uppercase tracking-widest text-gray-600 flex items-center gap-1.5">
                      <FolderPlus size={12} className="shrink-0" />
                      {t('addToSource')}
                    </p>
                    <div className="max-h-44 overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e]">
                      {collections.length === 0 && (
                        <p className="px-3 py-1.5 text-xs text-gray-500">{t('noSources')}</p>
                      )}
                      {collections.map((c) => {
                        const allIn = selectedCount > 0 && !!allInByCollection[c.id];
                        return (
                          <button
                            key={c.id}
                            data-testid={`assign-to-${c.id}`}
                            onClick={() => assignTo(c.id)}
                            className="u-press w-full flex items-center gap-2.5 px-3 py-1.5 text-[13px] text-gray-300 hover:bg-[#2a2a2a] hover:text-white transition-colors"
                          >
                            <span
                              className="w-2 h-2 rounded-full shrink-0"
                              style={{ backgroundColor: c.color }}
                            />
                            <span className="flex-1 truncate text-left">{c.name}</span>
                            {allIn && (
                              <Check size={14} className="u-pop-in text-green-400 shrink-0" />
                            )}
                          </button>
                        );
                      })}
                    </div>
                    <button
                      data-testid="assign-create-new"
                      onClick={() => {
                        setActionsOpen(false);
                        setShowCreate(true);
                      }}
                      className="u-press w-full flex items-center gap-2.5 px-3 py-1.5 text-[13px] text-gray-300 hover:bg-[#2a2a2a] hover:text-white transition-colors"
                    >
                      <Plus size={15} className="shrink-0" />
                      <span className="flex-1 text-left">{t('createNewSource')}</span>
                    </button>
                  </div>

                  <div className="my-1 border-t border-[#2e2e2e]" />

                  {/* ── Pulizia + distruttiva (two-step) ──────────────────── */}
                  <button
                    data-testid={
                      confirmClearDesc
                        ? 'bulk-clear-descriptions-confirm'
                        : 'bulk-clear-descriptions'
                    }
                    disabled={selectedCount === 0}
                    onClick={() => {
                      if (confirmClearDesc) handleClearDescriptions();
                      else {
                        setConfirmClearDesc(true);
                        setConfirmClearTags(false);
                        setConfirmDeletePosts(false);
                      }
                    }}
                    className={[
                      'u-press w-full flex items-center gap-2.5 px-3 py-2 text-[13px] text-left transition-colors disabled:opacity-40 disabled:pointer-events-none',
                      confirmClearDesc
                        ? 'bg-amber-500/15 text-amber-300 hover:bg-amber-500/25'
                        : 'text-gray-300 hover:bg-[#2a2a2a] hover:text-white',
                    ].join(' ')}
                  >
                    <Eraser size={15} className="shrink-0" />
                    <span key={confirmClearDesc ? 'c' : 'i'} className="u-fade-in">
                      {confirmClearDesc
                        ? t('clearDescriptionsConfirm', { n: selectedCount })
                        : t('clearDescriptions')}
                    </span>
                  </button>

                  <button
                    data-testid={confirmClearTags ? 'bulk-clear-tags-confirm' : 'bulk-clear-tags'}
                    disabled={selectedCount === 0}
                    onClick={() => {
                      if (confirmClearTags) handleClearAiTags();
                      else {
                        setConfirmClearTags(true);
                        setConfirmClearDesc(false);
                        setConfirmDeletePosts(false);
                      }
                    }}
                    className={[
                      'u-press w-full flex items-center gap-2.5 px-3 py-2 text-[13px] text-left transition-colors disabled:opacity-40 disabled:pointer-events-none',
                      confirmClearTags
                        ? 'bg-amber-500/15 text-amber-300 hover:bg-amber-500/25'
                        : 'text-gray-300 hover:bg-[#2a2a2a] hover:text-white',
                    ].join(' ')}
                  >
                    <TagsIcon size={15} className="shrink-0" />
                    <span key={confirmClearTags ? 'c' : 'i'} className="u-fade-in">
                      {confirmClearTags
                        ? t('clearTagsConfirm', { n: selectedCount })
                        : t('clearTags')}
                    </span>
                  </button>

                  <div className="my-1 border-t border-[#2e2e2e]" />

                  <button
                    data-testid={
                      confirmDeletePosts ? 'bulk-delete-posts-confirm' : 'bulk-delete-posts'
                    }
                    disabled={selectedCount === 0}
                    onClick={() => {
                      if (confirmDeletePosts) handleDeletePosts();
                      else {
                        setConfirmDeletePosts(true);
                        setConfirmClearDesc(false);
                        setConfirmClearTags(false);
                      }
                    }}
                    className={[
                      'u-press w-full flex items-center gap-2.5 px-3 py-2 text-[13px] text-left transition-colors disabled:opacity-40 disabled:pointer-events-none',
                      confirmDeletePosts
                        ? 'bg-red-500/20 text-red-300 hover:bg-red-500/30'
                        : 'text-red-400/90 hover:bg-red-500/10 hover:text-red-300',
                    ].join(' ')}
                  >
                    <Trash2 size={15} className="shrink-0" />
                    <span key={confirmDeletePosts ? 'c' : 'i'} className="u-fade-in">
                      {confirmDeletePosts
                        ? t('deletePostsConfirm', { n: selectedCount })
                        : t('deletePosts')}
                    </span>
                  </button>
                  {confirmDeletePosts && (
                    <p className="px-3 pt-1 pb-1.5 text-[10.5px] leading-snug text-gray-500">
                      {t('deleteHint')}
                    </p>
                  )}
                </Popover>
              </div>

              <button
                data-testid="select-cancel"
                onClick={exitSelectMode}
                title={t('exitSelectionTitle')}
                className="flex items-center justify-center w-7 h-7 rounded-md text-gray-400 hover:text-white hover:bg-[#1a1a1a] u-press shrink-0"
              >
                <X size={16} />
              </button>
            </div>
          )}
        </div>

        {/* AI-suggested filter tags — a separate strip below the toolbar while a
          query is active */}
        {showSuggestBar && (
          <div
            data-testid="suggested-tags-bar"
            className="flex items-center gap-2 px-3 py-2 shrink-0 border-b border-[#1e1e1e] bg-[#0f0f0f] overflow-x-auto scrollbar-thin scrollbar-thumb-[#2e2e2e] scrollbar-track-transparent u-fade-in-down"
          >
            <span className="flex items-center gap-1.5 text-[11px] text-gray-500 shrink-0">
              <Sparkles size={13} className="text-[#7B5CFF]" />
              {t('suggestedFilters')}
            </span>

            {/* AND/OR toggle — meaningful only with 2+ active concepts */}
            <div className="inline-flex rounded-md border border-[#2e2e2e] overflow-hidden shrink-0">
              {['or', 'and'].map((m) => (
                <button
                  key={m}
                  data-testid={`concept-mode-${m}`}
                  onClick={() => changeConceptMode(m)}
                  disabled={activeConcepts.length < 2}
                  className={[
                    'px-2 py-0.5 text-[10px] uppercase u-press disabled:opacity-40',
                    conceptMode === m
                      ? 'bg-[#7B5CFF] text-white'
                      : 'text-gray-400 hover:bg-[#1a1a1a]',
                  ].join(' ')}
                >
                  {m}
                </button>
              ))}
            </div>

            {suggestedTags.map((t, i) => {
              const active = activeConcepts.includes(t);
              return (
                <button
                  key={t}
                  data-testid="suggested-tag"
                  onClick={() => toggleConcept(t)}
                  style={{ animationDelay: i * 30 + 'ms' }}
                  className={[
                    'shrink-0 whitespace-nowrap px-2.5 py-0.5 rounded-full text-[12px] u-press u-pop-in',
                    active
                      ? 'bg-[#7B5CFF] text-white'
                      : 'bg-[#7B5CFF]/15 text-[#b9a6ff] hover:bg-[#7B5CFF]/25',
                  ].join(' ')}
                >
                  {active ? '' : '#'}
                  {t}
                </button>
              );
            })}

            {suggestLoading && (
              <RefreshCw
                size={13}
                className="text-[#7B5CFF] animate-spin shrink-0"
                strokeWidth={1.5}
              />
            )}
          </div>
        )}

        {/* Suggested-download banner — shown after a bulk analyze when part of the
          selection had remote-only media that must be downloaded before it can be
          analyzed. Offers to queue exactly those downloads. */}
        {selectMode && downloadSuggest && downloadSuggest.length > 0 && (
          <div
            data-testid="analyze-download-suggest"
            className="flex items-center gap-2 px-3 py-1.5 shrink-0 border-b border-[#1e1e1e] bg-amber-500/10 u-fade-in-down"
          >
            <Info size={14} className="text-amber-300 shrink-0" />
            <span className="flex-1 text-xs text-amber-200/90 leading-snug">
              {t('analyzeNeedsDownload', { n: downloadSuggest.length })}
            </span>
            <button
              data-testid="analyze-download-suggest-action"
              onClick={handleDownloadSuggested}
              title={t('downloadOnlyMissingHint')}
              className="u-press flex items-center gap-1.5 px-2.5 py-1 rounded-md text-xs font-medium text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] shrink-0 transition-colors"
            >
              <Download size={13} />
              {t('analyzeDownloadMissing', { n: downloadSuggest.length })}
            </button>
            <button
              data-testid="analyze-download-suggest-dismiss"
              onClick={() => setDownloadSuggest(null)}
              title={t('analyzeSuggestDismiss')}
              className="flex items-center justify-center w-6 h-6 rounded-md text-amber-200/70 hover:text-white hover:bg-white/10 u-press shrink-0"
            >
              <X size={14} />
            </button>
          </div>
        )}

        {/* Scrollable content area */}
        <div
          ref={scrollRef}
          className="flex-1 overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e] scrollbar-track-transparent"
        >
          {/* Grid (row-virtualized via the shared VirtualPostGrid) — only the rows
            in (or near) the viewport are mounted, so off-screen PostCard
            images/videos are never created. A single onMouseDownCapture on the
            container records the last pointer event (shiftKey for range-select)
            and arms the drag-select sweep; onGridMouseOver extends the sweep. */}
          <VirtualPostGrid
            testId="post-grid"
            posts={posts}
            scrollRef={scrollRef}
            onOpen={handleCardOpen}
            selectable={selectMode}
            selected={selected}
            onQuickSelect={handleQuickSelect}
            onGridMouseDownCapture={handleGridMouseDown}
            onGridMouseOver={handleGridMouseOver}
          />

          {/* Initial load → skeleton grid (content-shaped, no empty flash). */}
          {loading && posts.length === 0 && <PostGridSkeleton />}

          {/* Mid-scroll load (more pages) → small spinner under the grid. */}
          {loading && posts.length > 0 && (
            <div className="flex items-center justify-center py-10 u-fade-in">
              <RefreshCw size={20} className="text-[#555] animate-spin" strokeWidth={1.5} />
            </div>
          )}

          {/* Empty state */}
          {isEmpty && (
            <div
              data-testid="empty-state"
              className="flex flex-col items-center justify-center h-full min-h-[60vh] gap-3 text-center px-6 u-fade-in-up"
            >
              <ImageOff size={36} className="text-[#333]" strokeWidth={1} />
              {error ? (
                <p className="text-red-400 text-sm leading-relaxed max-w-xs">{error}</p>
              ) : (
                <p className="text-[#555] text-sm leading-relaxed max-w-xs font-display">
                  {t('emptyTitle')} <span className="text-[#444]">{t('emptyHint')}</span>
                </p>
              )}
            </div>
          )}

          {/* Sentinel for infinite scroll */}
          <div ref={sentinelRef} className="h-1" aria-hidden="true" />
        </div>
      </div>

      {/* Right-hand filters drawer (lives inside the Gallery page) */}
      <FilterDrawer
        open={drawerOpen}
        onClose={() => setDrawerOpen(false)}
        filters={filters}
        onChange={handleFilterChange}
        collections={collections}
        stats={stats}
        activeSource={activeSource}
        onSelectSource={onSelectSource}
      />

      {activePost && !selectMode && (
        <PostModal
          post={activePost}
          onClose={() => setActivePost(null)}
          onPrev={handlePrev}
          onNext={handleNext}
          hasPrev={hasPrev}
          hasNext={hasNext}
          onApplyAiFilter={(patch) => {
            setFilters((prev) => ({ ...prev, ...patch, limit: LOAD_BATCH }));
            setActivePost(null);
          }}
          onLocalFilesDeleted={() => reload()}
          onPostUpdated={(postId, fields) =>
            setActivePost((prev) => (prev && prev.id === postId ? { ...prev, ...fields } : prev))
          }
          onAssigned={() => onAssigned?.()}
          onPostDeleted={() => {
            showFeedback(t('fbPostDeleted'));
            reload();
            onAssigned?.();
            onStatsChanged?.(); // keep App-level sidebar counts in sync
          }}
          onOpenInWebsites={
            onOpenInWebsites
              ? () => {
                  setActivePost(null);
                  onOpenInWebsites();
                }
              : undefined
          }
          onReanalyzeWeb={
            onReanalyzeWeb
              ? (p) => {
                  setActivePost(null);
                  onReanalyzeWeb(p);
                }
              : undefined
          }
        />
      )}

      {showCreate && (
        <CollectionModal
          collections={collections}
          onClose={() => setShowCreate(false)}
          onSave={handleCreateAndAssign}
        />
      )}
    </div>
  );
}
