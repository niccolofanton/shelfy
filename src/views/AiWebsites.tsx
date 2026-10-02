import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  CheckCircle2,
  Globe,
  Loader2,
  Plus,
  RefreshCw,
  Search,
  Settings2,
  SlidersHorizontal,
  Trash2,
  X,
} from 'lucide-react';
import { useAnalysis } from '../hooks/useAnalysis';
import { useRangeSelect } from '../hooks/useRangeSelect';
import { useToast } from '../hooks/useToast';
import { useT } from '../i18n';
import { normalizeInputUrl } from '../components/AddSiteModal';
import type { AiJobView, FacetCounts, FacetSelection, SiteView, WebJob } from './websites/model';
import {
  ACTIVE_STATUSES,
  selectionSize,
  snapshotToPost,
  toAiJob,
  toSiteView,
  toWebJob,
  toggleFacet,
} from './websites/model';
import { useVocab } from './websites/vocab';
import SiteCard from './websites/SiteCard';
import FacetPanel from './websites/FacetPanel';
import ColorFilter from './websites/ColorFilter';
import JobQueue from './websites/JobQueue';
import type { QueueItem } from './websites/JobQueue';
import SiteDetail from './websites/SiteDetail';
import type { DetailTab } from './websites/SiteDetail';
import type { VersionEntry } from './websites/detail/CaptureTab';

// ════════════════════════════════════════════════════════════════════════════
//  Websites — a design-reference library (Godly / Refero style) on top of the
//  web-capture pipeline.
//
//  • Library: a responsive grid of site cards (first viewport, identity, type,
//    palette; hover plays the preview recording), driven by queryWebReferences
//    with text search, colour search, sort and catalog facet filters
//    (getWebFacets; OR within a facet, AND across facets). Paged in batches of
//    PAGE_SIZE with an infinite-scroll sentinel.
//  • Live work: a compact queue above the grid — captures in progress, sites
//    stuck on an anti-bot check ("pass the check" opens a visible browser),
//    failures, and AI catalogs being streamed.
//  • Detail: a full panel per site (Overview, Pages, Sections, Design,
//    Similar, Capture) with versions, re-capture/re-analyse and delete.
// ════════════════════════════════════════════════════════════════════════════

const PAGE_SIZE = 60;
type Sort = 'recent' | 'name' | 'color';

interface WebJobsApi {
  jobs?: unknown[];
  cancelJob?: (key: string) => unknown;
  cancelAll?: () => unknown;
  retryJob?: (key: string) => unknown;
  clearCompleted?: () => unknown;
}
interface AiWebsitesProps {
  webJobs?: WebJobsApi;
  onAddSite?: () => void;
  onOpenPost?: (postId: string) => void;
}

export default function AiWebsites({
  webJobs,
  onAddSite,
  onOpenPost,
}: AiWebsitesProps): React.ReactElement {
  const t = useT('aiWebsites');
  const tc = useT('common');
  const vocab = useVocab();
  const { toast, toastClosing, showToast } = useToast();
  const analysis = useAnalysis();
  const modelReady = !!analysis?.modelStatus?.ready;
  const { cancelJob, retryJob, clearCompleted } = webJobs || {};

  // ── Live capture jobs (normalised: strings only, never raw objects) ─────────
  const rawJobs = webJobs?.jobs;
  const jobs = useMemo<WebJob[]>(
    () =>
      (Array.isArray(rawJobs) ? rawJobs : []).map(toWebJob).filter((j): j is WebJob => j !== null),
    [rawJobs],
  );
  const jobByPost = useMemo(() => {
    const m = new Map<string, WebJob>();
    for (const j of jobs) if (j.postId) m.set(j.postId, j);
    return m;
  }, [jobs]);

  // AI catalog jobs of web references (the shared analyzer queue).
  const aiJobs = useMemo(() => {
    const m = new Map<string, AiJobView>();
    for (const raw of analysis?.jobs || []) {
      const pid = raw.postId || '';
      if (raw.platform !== 'web' && !pid.startsWith('web:')) continue;
      const view = toAiJob(raw);
      if (view && pid) m.set(pid, view);
    }
    return m;
  }, [analysis?.jobs]);

  // ── Query state ─────────────────────────────────────────────────────────────
  const [searchInput, setSearchInput] = useState('');
  const [q, setQ] = useState('');
  const [facets, setFacets] = useState<FacetSelection>({});
  const [color, setColor] = useState<string | null>(null);
  const [sort, setSort] = useState<Sort>('recent');
  const [filtersOpen, setFiltersOpen] = useState(true);
  useEffect(() => {
    const timer = setTimeout(() => setQ(searchInput.trim()), 250);
    return () => clearTimeout(timer);
  }, [searchInput]);
  // Colour proximity is a sort only while a colour is set.
  const effectiveSort: Sort = sort === 'color' && !color ? 'recent' : sort;
  const filterKey = JSON.stringify([q, facets, color, effectiveSort]);
  const hasFilters = !!q || !!color || selectionSize(facets) > 0;

  // ── Results (paged) ─────────────────────────────────────────────────────────
  const [posts, setPosts] = useState<Shelfy.Post[]>([]);
  const [total, setTotal] = useState(0);
  const [loading, setLoading] = useState(true);
  const [loadedOnce, setLoadedOnce] = useState(false);
  const [counts, setCounts] = useState<FacetCounts>({});
  const [reloadSig, setReloadSig] = useState(0);
  const reload = useCallback(() => setReloadSig((n) => n + 1), []);
  const reqSeq = useRef(0);
  const postsLenRef = useRef(0);
  postsLenRef.current = posts.length;

  // Data signatures: a capture finishing, a new placeholder appearing, or an AI
  // catalog landing all change what the grid / facet counts should show.
  const dataSig = useMemo(
    () =>
      [
        jobs
          .map(
            (j) =>
              `${j.key}:${j.status === 'done' ? 'd' : ACTIVE_STATUSES.has(j.status) ? 'a' : j.status}`,
          )
          .join(','),
        [...aiJobs.entries()]
          .filter(([, j]) => j.status === 'done' || j.status === 'error')
          .map(([k, j]) => `${k}:${j.status}`)
          .join(','),
      ].join('|'),
    [jobs, aiJobs],
  );

  const fetchPage = useCallback(
    async (offset: number, limit: number, replace: boolean): Promise<void> => {
      const seq = ++reqSeq.current;
      setLoading(true);
      try {
        const res = await window.electronAPI?.queryWebReferences?.({
          q: q || undefined,
          facets: selectionSize(facets) ? facets : undefined,
          color: color || undefined,
          sort: effectiveSort,
          limit,
          offset,
        });
        if (seq !== reqSeq.current) return;
        const list = Array.isArray(res?.posts) ? res.posts : [];
        setTotal(typeof res?.total === 'number' ? res.total : list.length);
        setPosts((prev) =>
          replace ? list : [...prev, ...list.filter((p) => !prev.some((x) => x.id === p.id))],
        );
      } catch {
        if (seq === reqSeq.current && replace) {
          setPosts([]);
          setTotal(0);
        }
      } finally {
        if (seq === reqSeq.current) {
          setLoading(false);
          setLoadedOnce(true);
        }
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [filterKey],
  );

  // Filters changed → back to the first page.
  useEffect(() => {
    void fetchPage(0, PAGE_SIZE, true);
  }, [fetchPage]);

  // Data changed → refresh what's loaded in place (keeps the scroll position).
  const firstData = useRef(true);
  useEffect(() => {
    if (firstData.current) {
      firstData.current = false;
      return;
    }
    void fetchPage(0, Math.max(PAGE_SIZE, postsLenRef.current), true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dataSig, reloadSig]);

  useEffect(() => {
    let alive = true;
    // Counts follow the active search (a facet's own selection is ignored by the
    // backend when counting that facet, so OR alternatives stay visible).
    Promise.resolve(
      window.electronAPI?.getWebFacets?.({
        q: q || undefined,
        facets: selectionSize(facets) ? facets : undefined,
        color: color || undefined,
      }),
    )
      .then((res) => {
        if (alive && res && typeof res === 'object') setCounts(res);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [dataSig, reloadSig, q, facets, color]);

  const hasMore = posts.length < total;
  const sentinel = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const el = sentinel.current;
    if (!el || !hasMore) return undefined;
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting) && !loading)
          void fetchPage(postsLenRef.current, PAGE_SIZE, false);
      },
      { rootMargin: '600px' },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [hasMore, loading, fetchPage]);

  const sites = useMemo(() => posts.map(toSiteView), [posts]);
  const postById = useMemo(() => new Map(posts.map((p) => [p.id, p])), [posts]);
  // Every site seen so far (any filter): the queue keeps naming a site even
  // when the current filters hide it from the grid.
  const siteCache = useRef(new Map<string, SiteView>());
  const siteById = useMemo(() => {
    for (const s of sites) siteCache.current.set(s.id, s);
    return new Map(siteCache.current);
  }, [sites]);

  // ── 1s ticker while something runs (elapsed time in the Capture tab) ────────
  const anyActive = jobs.some((j) => ACTIVE_STATUSES.has(j.status));
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!anyActive) return undefined;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [anyActive]);

  // ── Detail ──────────────────────────────────────────────────────────────────
  const [openId, setOpenId] = useState<string | null>(null);
  const [openTab, setOpenTab] = useState<DetailTab>('overview');
  // Posts opened from outside the loaded page (Similar, queue) are fetched.
  const [extraPost, setExtraPost] = useState<Shelfy.Post | null>(null);
  const openSite = useCallback(
    (id: string, tab: DetailTab = 'overview') => {
      setOpenTab(tab);
      setOpenId(id);
      if (!postById.has(id)) {
        Promise.resolve(window.electronAPI?.getPostsByIds?.([id]))
          .then((list) => setExtraPost(Array.isArray(list) && list[0] ? list[0] : null))
          .catch(() => setExtraPost(null));
      }
    },
    [postById],
  );
  const openFromCard = useCallback((id: string) => openSite(id, 'overview'), [openSite]);
  const closeDetail = useCallback(() => setOpenId(null), []);
  // Keep the fetched copy fresh when the same site reloads.
  const openPost: Shelfy.Post | null = openId
    ? postById.get(openId) || (extraPost?.id === openId ? extraPost : null)
    : null;
  useEffect(() => {
    if (!openId || postById.has(openId) || extraPost?.id !== openId) return;
    Promise.resolve(window.electronAPI?.getPostsByIds?.([openId]))
      .then((list) => Array.isArray(list) && list[0] && setExtraPost(list[0]))
      .catch(() => {});
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dataSig]);

  // Snapshot versions of the open site.
  const [snapshots, setSnapshots] = useState<Shelfy.WebSnapshot[]>([]);
  const [snapshotsFor, setSnapshotsFor] = useState<string | null>(null);
  const [activeSnapshotId, setActiveSnapshotId] = useState<number | null>(null);
  useEffect(() => {
    setActiveSnapshotId(null);
    if (!openId) {
      setSnapshots([]);
      setSnapshotsFor(null);
      return undefined;
    }
    let alive = true;
    Promise.resolve(window.electronAPI?.getWebSnapshots?.(openId))
      .then((list) => {
        if (!alive) return;
        setSnapshots(Array.isArray(list) ? list : []);
        setSnapshotsFor(openId);
      })
      .catch(() => {
        if (!alive) return;
        setSnapshots([]);
        setSnapshotsFor(openId);
      });
    return () => {
      alive = false;
    };
  }, [openId, reloadSig]);
  const snapshotsValid = snapshotsFor === openId;
  const versions = useMemo<VersionEntry[]>(() => {
    const current: VersionEntry = {
      id: null,
      capturedAt: openPost?.webCapturedAt ? openPost.webCapturedAt * 1000 : null,
      isCurrent: true,
    };
    const archived: VersionEntry[] = snapshotsValid
      ? snapshots.map((s) => ({
          id: s.id,
          capturedAt: (s.capturedAt || 0) * 1000,
          isCurrent: false,
        }))
      : [];
    return [current, ...archived];
  }, [openPost?.webCapturedAt, snapshots, snapshotsValid]);
  const activeSnapshot =
    snapshotsValid && activeSnapshotId !== null
      ? snapshots.find((s) => s.id === activeSnapshotId) || null
      : null;
  const detailSite: SiteView | null = useMemo(() => {
    if (!openPost) return null;
    return toSiteView(activeSnapshot ? snapshotToPost(activeSnapshot, openPost) : openPost);
  }, [openPost, activeSnapshot]);

  const handleDeleteSnapshot = useCallback(
    async (snapshotId: number) => {
      try {
        await window.electronAPI?.deleteWebSnapshot?.(snapshotId);
      } catch {
        /* surfaced via reload below */
      }
      if (activeSnapshotId === snapshotId) setActiveSnapshotId(null);
      reload();
    },
    [activeSnapshotId, reload],
  );

  // ── Actions ─────────────────────────────────────────────────────────────────
  const [unblocking, setUnblocking] = useState<Set<string>>(() => new Set());
  const handleUnblock = useCallback(
    async (key: string) => {
      setUnblocking((s) => new Set(s).add(key));
      showToast(t('unblockOpened'));
      try {
        const res = await window.electronAPI?.unblockWebJob?.(key);
        if (res && !res.ok && res.reason === 'not-passed') showToast(t('unblockNotPassed'));
      } catch {
        showToast(tc('genericError'));
      } finally {
        setUnblocking((s) => {
          const n = new Set(s);
          n.delete(key);
          return n;
        });
      }
    },
    [showToast, t, tc],
  );

  const handleRecapture = useCallback(async () => {
    const url = openPost?.webUrl || openPost?.webFinalUrl || openPost?.postUrl;
    if (!url) return;
    try {
      // singlePage left undefined: the orchestrator replays the persisted mode.
      await window.electronAPI?.addWebReference?.(url, undefined, true);
      setActiveSnapshotId(null);
      showToast(t('recaptureQueued'));
    } catch {
      showToast(tc('genericError'));
    }
  }, [openPost, showToast, t, tc]);

  const handleReanalyse = useCallback(async () => {
    if (!openId) return;
    try {
      const res = await window.electronAPI?.recatalogWebReferences?.({ ids: [openId] });
      showToast(res?.queued ? t('reanalyseQueued') : t('reanalyseNotQueued'));
    } catch {
      showToast(tc('genericError'));
    }
  }, [openId, showToast, t, tc]);

  const [recataloguing, setRecataloguing] = useState(false);
  const handleRecatalogOutdated = useCallback(async () => {
    setRecataloguing(true);
    try {
      const res = await window.electronAPI?.recatalogWebReferences?.({ outdatedOnly: true });
      const n = res?.queued ?? 0;
      showToast(n > 0 ? t('recatalogQueued', { count: n, n }) : t('recatalogNone'));
    } catch {
      showToast(tc('genericError'));
    } finally {
      setRecataloguing(false);
    }
  }, [showToast, t, tc]);

  const applyFacet = useCallback((facet: string, value: string) => {
    setFacets((sel) =>
      (sel[facet] || []).includes(value.toLowerCase()) ? sel : toggleFacet(sel, facet, value),
    );
    setFiltersOpen(true);
    setOpenId(null);
  }, []);
  const onToggleFacet = useCallback(
    (facet: string, value: string) => setFacets((s) => toggleFacet(s, facet, value)),
    [],
  );
  const clearAll = useCallback(() => {
    setFacets({});
    setColor(null);
    setSearchInput('');
    setQ('');
  }, []);

  // ── Add site (inline) ───────────────────────────────────────────────────────
  const [addUrl, setAddUrl] = useState('');
  const [adding, setAdding] = useState(false);
  const addNormalized = normalizeInputUrl(addUrl);
  const submitAdd = async (e: React.FormEvent): Promise<void> => {
    e.preventDefault();
    if (!addNormalized || adding) return;
    setAdding(true);
    try {
      await window.electronAPI?.addWebReference?.(addNormalized);
      setAddUrl('');
      showToast(t('addQueued', { host: new URL(addNormalized).hostname }));
      reload();
    } catch (err) {
      showToast(err instanceof Error && err.message ? err.message : tc('genericError'));
    } finally {
      setAdding(false);
    }
  };

  // ── Multi-select + delete ───────────────────────────────────────────────────
  const [selectMode, setSelectMode] = useState(false);
  const {
    selected: selectedIds,
    toggleAt,
    clearSelection,
    resetAnchor,
  } = useRangeSelect(sites, (s: SiteView) => s.id);
  const sitesRef = useRef<SiteView[]>(sites);
  sitesRef.current = sites;
  useEffect(() => {
    resetAnchor();
  }, [sites, resetAnchor]);
  const exitSelectMode = useCallback(() => {
    setSelectMode(false);
    clearSelection();
  }, [clearSelection]);
  useEffect(() => {
    clearSelection();
  }, [filterKey, clearSelection]);
  const handleToggle = useCallback(
    (id: string, evt: React.MouseEvent) => {
      const index = sitesRef.current.findIndex((s) => s.id === id);
      toggleAt(id, index, evt?.shiftKey);
    },
    [toggleAt],
  );
  const [deleteDialog, setDeleteDialog] = useState<{ ids: string[] } | null>(null);
  const [busy, setBusy] = useState(false);
  const runDelete = useCallback(
    async (mode: 'complete' | 'report') => {
      const ids = deleteDialog?.ids || [];
      if (!ids.length) {
        setDeleteDialog(null);
        return;
      }
      setBusy(true);
      try {
        if (mode === 'complete') await window.electronAPI?.deleteWebSites?.(ids);
        else await window.electronAPI?.deleteWebLatestReport?.(ids);
      } catch {
        /* surfaced via reload below */
      }
      setBusy(false);
      setDeleteDialog(null);
      exitSelectMode();
      if (mode === 'complete' && openId && ids.includes(openId)) setOpenId(null);
      reload();
    },
    [deleteDialog, exitSelectMode, openId, reload],
  );

  // ── Queue items ─────────────────────────────────────────────────────────────
  const queueItems = useMemo<QueueItem[]>(() => {
    const items = new Map<string, QueueItem>();
    for (const j of jobs) {
      if (j.status === 'done' || j.status === 'cancelled') continue;
      items.set(j.key, {
        key: j.key,
        postId: j.postId,
        job: j,
        ai: j.postId ? aiJobs.get(j.postId) || null : null,
        site: j.postId ? siteById.get(j.postId) || null : null,
      });
    }
    for (const [pid, ai] of aiJobs) {
      if (!['pending', 'extracting', 'analyzing'].includes(ai.status)) continue;
      const key = `web:${pid}`;
      if (items.has(key)) continue;
      const j = jobByPost.get(pid);
      if (j && j.status !== 'done') continue;
      items.set(key, { key, postId: pid, job: null, ai, site: siteById.get(pid) || null });
    }
    return [...items.values()];
  }, [jobs, aiJobs, siteById, jobByPost]);
  const canClear = jobs.some(
    (j) => j.status === 'done' || j.status === 'error' || j.status === 'cancelled',
  );

  const activeChips = useMemo(
    () =>
      Object.entries(facets).flatMap(([f, vals]) =>
        vals.map((v) => ({ facet: f, value: v, label: vocab.label(f, v), group: vocab.facet(f) })),
      ),
    [facets, vocab],
  );
  const filterCount = selectionSize(facets);
  const libraryEmpty =
    loadedOnce && !loading && total === 0 && !hasFilters && queueItems.length === 0;
  const detailJob = openId ? jobByPost.get(openId) || null : null;
  const detailAi = openId ? aiJobs.get(openId) || null : null;

  return (
    <div data-testid="aiweb-view" className="relative flex flex-col h-full bg-[#0f0f0f]">
      {/* ── Header ──────────────────────────────────────────────────────── */}
      <div className="shrink-0 flex items-center gap-3 px-6 h-14 border-b border-[#1f1f1f]">
        <Globe size={18} style={{ color: 'var(--accent)' }} />
        <h1 className="text-[15px] font-semibold text-white font-display">{t('headerTitle')}</h1>
        {loadedOnce && (
          <span className="text-[12px] tabular-nums text-[#6b6b6b]" data-testid="aiweb-count">
            {hasFilters
              ? t('countFiltered', { n: total })
              : t('countAll', { count: total, n: total })}
          </span>
        )}
        <form
          onSubmit={submitAdd}
          className="ml-auto flex items-center gap-1.5"
          data-testid="aiweb-add-form"
        >
          <div className="relative">
            <Globe
              size={13}
              className="absolute left-2.5 top-1/2 -translate-y-1/2 text-[#5f5f5f] pointer-events-none"
            />
            <input
              data-testid="aiweb-add-input"
              value={addUrl}
              onChange={(e) => setAddUrl(e.target.value)}
              placeholder={t('addPlaceholder')}
              spellCheck={false}
              className="w-[280px] h-8 rounded-lg bg-[#161616] border border-[#2a2a2a] pl-8 pr-2.5 text-[12.5px] text-white placeholder:text-[#5a5a5a] outline-none focus:border-[#7B5CFF] u-transition"
            />
          </div>
          <button
            type="submit"
            data-testid="aiweb-add"
            disabled={!addNormalized || adding}
            className="flex items-center gap-1.5 h-8 px-3 rounded-lg text-[12.5px] font-medium text-white u-press disabled:opacity-40 hover:brightness-110"
            style={{ background: 'var(--accent)' }}
          >
            {adding ? <Loader2 size={13} className="u-spin" /> : <Plus size={14} />}
            {t('addSite')}
          </button>
          <button
            type="button"
            data-testid="aiweb-add-options"
            onClick={() => onAddSite?.()}
            title={t('addOptionsTitle')}
            aria-label={t('addOptionsTitle')}
            className="flex items-center justify-center w-8 h-8 rounded-lg text-[#9a9a9a] hover:text-white hover:bg-[#1c1c1c] u-press"
          >
            <Settings2 size={15} />
          </button>
        </form>
      </div>

      {/* ── Toolbar ─────────────────────────────────────────────────────── */}
      {!libraryEmpty && (
        <div className="shrink-0 flex items-center gap-2 px-6 py-2.5 border-b border-[#1f1f1f]">
          <button
            type="button"
            data-testid="aiweb-filters-toggle"
            onClick={() => setFiltersOpen((o) => !o)}
            aria-pressed={filtersOpen}
            className={`flex items-center gap-1.5 h-8 px-2.5 rounded-lg text-[12.5px] u-press border ${
              filtersOpen
                ? 'bg-[#1f1f1f] border-[#2e2e2e] text-white'
                : 'bg-[#171717] border-[#2a2a2a] text-[#bdbdbd] hover:text-white'
            }`}
          >
            <SlidersHorizontal size={14} /> {t('filters')}
            {filterCount > 0 && (
              <span className="rounded-full bg-[#7B5CFF] px-1.5 text-[10.5px] font-semibold leading-[16px] text-white tabular-nums">
                {filterCount}
              </span>
            )}
          </button>
          <div className="relative flex-1 max-w-[420px]">
            <Search
              size={14}
              className="absolute left-2.5 top-1/2 -translate-y-1/2 text-[#5f5f5f] pointer-events-none"
            />
            <input
              data-testid="aiweb-search"
              type="text"
              value={searchInput}
              onChange={(e) => setSearchInput(e.target.value)}
              placeholder={t('searchPlaceholder')}
              className="w-full h-8 rounded-lg bg-[#161616] border border-[#2a2a2a] pl-8 pr-7 text-[12.5px] text-white placeholder:text-[#5a5a5a] outline-none focus:border-[#7B5CFF] u-transition"
            />
            {searchInput && (
              <button
                type="button"
                onClick={() => setSearchInput('')}
                title={t('clearSearchTitle')}
                aria-label={t('clearSearchTitle')}
                className="absolute right-1.5 top-1/2 -translate-y-1/2 flex items-center justify-center w-5 h-5 rounded text-[#7a7a7a] hover:text-white"
              >
                <X size={13} />
              </button>
            )}
          </div>
          <ColorFilter
            value={color}
            onChange={(hex) => {
              setColor(hex);
              if (hex) setSort('color');
            }}
          />
          <div
            role="radiogroup"
            aria-label={t('sortLabel')}
            className="flex items-center gap-0.5 rounded-lg bg-[#161616] border border-[#2a2a2a] p-0.5"
          >
            {(['recent', 'name', 'color'] as Sort[])
              .filter((s) => s !== 'color' || color)
              .map((s) => (
                <button
                  key={s}
                  type="button"
                  role="radio"
                  aria-checked={effectiveSort === s}
                  data-testid={`aiweb-sort-${s}`}
                  onClick={() => setSort(s)}
                  className={`h-7 px-2.5 rounded-md text-[12px] u-press ${
                    effectiveSort === s
                      ? 'bg-[#2a2a2a] text-white'
                      : 'text-[#9a9a9a] hover:text-white'
                  }`}
                >
                  {t(`sort.${s}`)}
                </button>
              ))}
          </div>
          <div className="ml-auto flex items-center gap-1.5">
            <button
              type="button"
              data-testid="aiweb-recatalog"
              onClick={handleRecatalogOutdated}
              disabled={recataloguing}
              title={t('recatalogTitle')}
              className="flex items-center gap-1.5 h-8 px-2.5 rounded-lg text-[12.5px] text-[#bdbdbd] hover:text-white hover:bg-[#1c1c1c] u-press disabled:opacity-50"
            >
              <RefreshCw size={13} className={recataloguing ? 'u-spin' : ''} /> {t('recatalog')}
            </button>
            {selectMode ? (
              <button
                type="button"
                data-testid="aiweb-select-done"
                onClick={exitSelectMode}
                className="flex items-center gap-1.5 h-8 px-2.5 rounded-lg text-[12.5px] text-white bg-[#1f1f1f] border border-[#2e2e2e] u-press"
              >
                <X size={14} /> {tc('cancel')}
              </button>
            ) : (
              <button
                type="button"
                data-testid="aiweb-select"
                onClick={() => setSelectMode(true)}
                title={t('selectMultipleTitle')}
                className="flex items-center gap-1.5 h-8 px-2.5 rounded-lg text-[12.5px] text-[#bdbdbd] hover:text-white hover:bg-[#1c1c1c] u-press"
              >
                <CheckCircle2 size={14} /> {t('selectAction')}
              </button>
            )}
          </div>
        </div>
      )}

      {/* ── Active filter chips ─────────────────────────────────────────── */}
      {hasFilters && (
        <div
          className="shrink-0 flex flex-wrap items-center gap-1.5 px-6 py-2 border-b border-[#1f1f1f]"
          data-testid="aiweb-active-filters"
        >
          {q && (
            <ActiveChip
              label={`“${q}”`}
              onRemove={() => setSearchInput('')}
              removeLabel={t('removeFilter')}
            />
          )}
          {color && (
            <ActiveChip
              label={color.toUpperCase()}
              swatch={color}
              group={t('colorButton')}
              onRemove={() => setColor(null)}
              removeLabel={t('removeFilter')}
            />
          )}
          {activeChips.map((c) => (
            <ActiveChip
              key={`${c.facet}:${c.value}`}
              group={c.group}
              label={c.label}
              onRemove={() => onToggleFacet(c.facet, c.value)}
              removeLabel={t('removeFilter')}
            />
          ))}
          <button
            type="button"
            data-testid="aiweb-clear-filters"
            onClick={clearAll}
            className="ml-1 text-[12px] text-[#8b74ff] hover:text-[#a593ff] u-press"
          >
            {t('clearFilters')}
          </button>
        </div>
      )}

      {/* ── Selection toolbar ───────────────────────────────────────────── */}
      {selectMode && (
        <div
          data-testid="aiweb-select-toolbar"
          className="shrink-0 flex items-center gap-3 px-6 py-2 border-b border-[#1f1f1f] bg-[#151515] u-fade-in"
        >
          <span className="text-[13px] font-medium text-white">
            {selectedIds.size === 1
              ? t('siteSelectedOne', { n: selectedIds.size })
              : t('siteSelectedOther', { n: selectedIds.size })}
          </span>
          <span className="text-[12px] text-[#6b6b6b]">{t('shiftRangeHint')}</span>
          <button
            type="button"
            data-testid="aiweb-delete-selected"
            disabled={selectedIds.size === 0 || busy}
            onClick={() =>
              setDeleteDialog({
                ids: [...selectedIds].filter((id): id is string => typeof id === 'string'),
              })
            }
            className="ml-auto flex items-center gap-1.5 h-8 px-3 rounded-lg text-[12.5px] text-white bg-[#ef5350] u-press disabled:opacity-40"
          >
            <Trash2 size={14} /> {tc('delete')}
          </button>
        </div>
      )}

      {/* ── Body ────────────────────────────────────────────────────────── */}
      <div className="relative flex-1 min-h-0 flex">
        {filtersOpen && !libraryEmpty && (
          <aside className="w-[244px] shrink-0 border-r border-[#1f1f1f]">
            <FacetPanel counts={counts} selection={facets} onToggle={onToggleFacet} />
          </aside>
        )}
        <div
          className="flex-1 min-w-0 overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e]"
          data-testid="aiweb-scroll"
        >
          <JobQueue
            items={queueItems}
            unblocking={unblocking}
            onOpen={(pid) => openSite(pid, 'capture')}
            onCancel={(k) => void cancelJob?.(k)}
            onRetry={(k) => void retryJob?.(k)}
            onUnblock={handleUnblock}
            onAiCancel={(k) => void analysis?.cancelJob?.(k)}
            onClear={() => void clearCompleted?.()}
            canClear={canClear}
          />

          {libraryEmpty ? (
            <div
              data-testid="aiweb-empty"
              className="flex flex-col items-center justify-center gap-4 px-6 py-24 text-center u-fade-in-up"
            >
              <Globe size={42} strokeWidth={1.25} className="text-[#4a4a4a]" />
              <div>
                <p className="text-[15px] font-medium text-white">{t('emptyTitle')}</p>
                <p className="mt-1 max-w-md text-[13px] leading-relaxed text-[#6b6b6b]">
                  {t('emptyHint')}
                </p>
              </div>
            </div>
          ) : loadedOnce && sites.length === 0 && !loading ? (
            <div
              data-testid="aiweb-no-results"
              className="flex flex-col items-center justify-center gap-3 px-6 py-24 text-center u-fade-in"
            >
              <Search size={26} strokeWidth={1.25} className="text-[#4a4a4a]" />
              <p className="text-[13px] text-[#8a8a8a]">{t('noResults')}</p>
              {hasFilters && (
                <button
                  type="button"
                  onClick={clearAll}
                  className="text-[12.5px] text-[#8b74ff] hover:text-[#a593ff] u-press"
                >
                  {t('clearFilters')}
                </button>
              )}
            </div>
          ) : (
            <div
              data-testid="aiweb-grid"
              className="grid gap-x-6 gap-y-9 px-6 pt-6 pb-10"
              style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(290px, 1fr))' }}
            >
              {sites.map((s) => (
                <SiteCard
                  key={s.id}
                  site={s}
                  job={jobByPost.get(s.id) || null}
                  selectMode={selectMode}
                  checked={selectedIds.has(s.id)}
                  onOpen={openFromCard}
                  onToggle={handleToggle}
                />
              ))}
              {!loadedOnce &&
                Array.from({ length: 6 }, (_, i) => (
                  <div key={`sk-${i}`} className="flex flex-col gap-2.5">
                    <span
                      className="ai-skel-static block w-full rounded-xl"
                      style={{ aspectRatio: '16 / 10' }}
                    />
                    <span className="ai-skel-static block h-3 w-1/2 rounded" />
                  </div>
                ))}
            </div>
          )}
          {hasMore && (
            <div ref={sentinel} className="flex justify-center pb-10 text-[12px] text-[#6b6b6b]">
              {loading && <Loader2 size={16} className="u-spin" />}
            </div>
          )}
        </div>
      </div>

      {/* Detail: a full panel over the whole view (Esc / back returns). */}
      {openId && detailSite && (
        <SiteDetail
          site={detailSite}
          job={detailJob}
          aiJob={activeSnapshot ? null : detailAi}
          modelReady={modelReady}
          now={now}
          initialTab={openTab}
          versions={versions}
          activeVersionId={activeSnapshotId}
          onSelectVersion={setActiveSnapshotId}
          onDeleteSnapshot={handleDeleteSnapshot}
          onClose={closeDetail}
          onOpenSite={openFromCard}
          onApplyFacet={applyFacet}
          onRecapture={handleRecapture}
          onReanalyse={handleReanalyse}
          onDelete={() => setDeleteDialog({ ids: [openId] })}
          onOpenPost={onOpenPost ? () => onOpenPost(openId) : undefined}
          onCancel={(k) => void cancelJob?.(k)}
          onRetry={(k) => void retryJob?.(k)}
          onUnblock={handleUnblock}
          onAiCancel={(k) => void analysis?.cancelJob?.(k)}
          onAiRetry={(k) => void analysis?.retryJob?.(k)}
          unblocking={!!detailJob && unblocking.has(detailJob.key)}
        />
      )}

      {/* ── Toast ───────────────────────────────────────────────────────── */}
      {toast && (
        <div className="pointer-events-none absolute bottom-5 inset-x-0 z-50 flex justify-center px-4">
          <div
            role="status"
            data-testid="aiweb-toast"
            className={`${toastClosing ? 'u-fade-out' : 'u-fade-in-up'} rounded-lg border border-[#2e2e2e] bg-[#1c1c1c] px-3.5 py-2 text-[12.5px] text-white shadow-2xl`}
          >
            {toast}
          </div>
        </div>
      )}

      {/* ── Delete confirmation: complete vs report-only ────────────────── */}
      {deleteDialog && (
        <div
          data-testid="aiweb-delete-dialog"
          className="fixed inset-0 z-[200] flex items-center justify-center p-6 bg-black/60 u-backdrop-in"
          onClick={() => !busy && setDeleteDialog(null)}
        >
          <div
            role="dialog"
            aria-modal="true"
            className="w-full max-w-md rounded-xl border border-[#2e2e2e] bg-[#1a1a1a] p-5 u-dialog-in"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-start gap-3">
              <span className="shrink-0 flex items-center justify-center w-9 h-9 rounded-lg bg-[#2a2a2a] text-[#ef5350]">
                <Trash2 size={18} />
              </span>
              <div className="min-w-0">
                <h3 className="text-sm font-semibold text-white">
                  {deleteDialog.ids.length === 1
                    ? t('deleteTitleOne', { n: 1 })
                    : t('deleteTitleOther', { n: deleteDialog.ids.length })}
                </h3>
                <p className="text-xs mt-0.5 text-[#7a7a7a]">{t('deleteSubtitle')}</p>
              </div>
            </div>
            <div className="mt-4 flex flex-col gap-2">
              <button
                type="button"
                data-testid="aiweb-delete-report"
                disabled={busy}
                onClick={() => runDelete('report')}
                className="text-left rounded-lg border border-[#2e2e2e] bg-[#222] px-3 py-2.5 u-press disabled:opacity-50 hover:bg-[#262626]"
              >
                <div className="text-sm font-medium text-white">{t('deleteReportTitle')}</div>
                <div className="text-xs mt-0.5 text-[#7a7a7a]">{t('deleteReportHint')}</div>
              </button>
              <button
                type="button"
                data-testid="aiweb-delete-complete"
                disabled={busy}
                onClick={() => runDelete('complete')}
                className="text-left rounded-lg border border-[#ef5350]/70 bg-[#222] px-3 py-2.5 u-press disabled:opacity-50 hover:bg-[#2a1a1a]"
              >
                <div className="text-sm font-medium text-[#ef5350]">{t('deleteCompleteTitle')}</div>
                <div className="text-xs mt-0.5 text-[#7a7a7a]">{t('deleteCompleteHint')}</div>
              </button>
            </div>
            <div className="mt-3 flex justify-end">
              <button
                type="button"
                disabled={busy}
                onClick={() => setDeleteDialog(null)}
                className="px-3 py-1.5 rounded text-sm text-[#a0a0a0] hover:text-white u-press disabled:opacity-50"
              >
                {tc('cancel')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function ActiveChip({
  label,
  group,
  swatch,
  onRemove,
  removeLabel,
}: {
  label: string;
  group?: string;
  swatch?: string;
  onRemove: () => void;
  removeLabel: string;
}): React.ReactElement {
  return (
    <span
      data-testid="aiweb-active-chip"
      className="inline-flex items-center gap-1.5 h-7 pl-2.5 pr-1 rounded-full bg-[#7B5CFF]/10 border border-[#7B5CFF]/35 text-[12px] text-white"
    >
      {swatch && <span className="w-3 h-3 rounded-full" style={{ background: swatch }} />}
      {group && <span className="text-[#a593ff]">{group}:</span>}
      {label}
      <button
        type="button"
        onClick={onRemove}
        aria-label={`${removeLabel} ${label}`}
        className="flex items-center justify-center w-5 h-5 rounded-full text-[#bdb3ff] hover:text-white hover:bg-white/10 u-press"
      >
        <X size={12} />
      </button>
    </span>
  );
}
