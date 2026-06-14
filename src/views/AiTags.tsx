import React, { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import VirtualPostGrid from '../components/VirtualPostGrid';
import GridSizeControl from '../components/GridSizeControl';
import PostGridSkeleton from '../components/PostGridSkeleton';
import PostModal from '../components/PostModal';
import { useAiTags } from '../hooks/useAiTags';
import { useAnalysis } from '../hooks/useAnalysis';
import { useToast } from '../hooks/useToast';
import { postsToMarkdown, downloadMarkdown, copyPostLinks } from '../lib/exportMarkdown';
import { useT } from '../i18n';
import {
  Sparkles,
  RefreshCw,
  X,
  Tags,
  Boxes,
  Users,
  GitMerge,
  Pencil,
  FolderPlus,
  ClipboardCopy,
  FileDown,
  Wand2,
  Heart,
  AlertTriangle,
  Check,
  Link2,
  ArrowRight,
} from 'lucide-react';
import type { LucideIcon } from 'lucide-react';

const ACCENT = '#7B5CFF';

// 'and' | 'or' tag combination mode for the results filter.
type TagMode = 'and' | 'or';

// Progress payload emitted by the cluster/alias LLM runs.
interface ProgressState {
  done: number;
  total: number;
}

// ────────────────────────────────────────────────────────────────────────────
// Small presentational helpers
// ────────────────────────────────────────────────────────────────────────────

interface SectionTitleProps {
  icon?: LucideIcon;
  children?: React.ReactNode;
  right?: React.ReactNode;
}

function SectionTitle({ icon: Icon, children, right }: SectionTitleProps) {
  return (
    <div className="flex items-center justify-between px-1 mb-2">
      <div className="flex items-center gap-2 text-[10px] font-semibold uppercase tracking-widest text-gray-500">
        {Icon && <Icon size={13} className="text-gray-500" />}
        {children}
      </div>
      {right}
    </div>
  );
}

interface ChipProps {
  label: string;
  count?: number;
  active?: boolean;
  onClick?: () => void;
  onRemove?: () => void;
  color?: string;
}

function Chip({ label, count, active, onClick, onRemove, color }: ChipProps) {
  const tc = useT('common');
  return (
    <span
      className={[
        'inline-flex items-center gap-1 rounded-full pl-2.5 pr-2 py-1 text-xs u-press',
        active ? 'bg-[#7B5CFF] text-white u-pop-in' : 'bg-[#1a1a1a] text-gray-300 hover:bg-[#222]',
      ].join(' ')}
    >
      {color && <span className="w-1.5 h-1.5 rounded-full" style={{ backgroundColor: color }} />}
      {onClick ? (
        <button onClick={onClick} className="leading-none">
          {label}
        </button>
      ) : (
        <span className="leading-none">{label}</span>
      )}
      {typeof count === 'number' && <span className="tabular-nums opacity-70">{count}</span>}
      {onRemove && (
        <button
          onClick={onRemove}
          className="ml-0.5 opacity-70 hover:opacity-100 u-press"
          title={tc('remove')}
        >
          <X size={12} />
        </button>
      )}
    </span>
  );
}

// ────────────────────────────────────────────────────────────────────────────
// Header dashboard (coverage, distributions, analyze-missing)
// ────────────────────────────────────────────────────────────────────────────

interface DashboardProps {
  overview: Shelfy.AiOverview | null;
  onAnalyzeMissing: () => void;
  onCancelAnalyze: () => void;
  analyzing: boolean;
  analyzeActive?: number;
}

function Dashboard({
  overview,
  onAnalyzeMissing,
  onCancelAnalyze,
  analyzing,
  analyzeActive = 0,
}: DashboardProps) {
  const t = useT('aiTags');
  if (!overview) return null;
  const { total = 0, analyzed = 0, unanalyzed = 0, uniqueTags = 0, taggedPosts = 0 } = overview;
  const coverage = total > 0 ? Math.round((analyzed / total) * 100) : 0;

  return (
    <div
      data-testid="aitags-dashboard"
      className="border-b border-[#2e2e2e] bg-[#0f0f0f] px-4 py-3"
    >
      <div className="flex items-center gap-2 mb-3">
        <Sparkles size={18} style={{ color: ACCENT }} />
        <h1 className="text-white text-base font-semibold">{t('title')}</h1>
        <div className="flex-1" />
        <div className="flex items-center gap-4 text-xs text-gray-500">
          <span>
            <span className="text-gray-300 tabular-nums">{uniqueTags.toLocaleString()}</span>{' '}
            {t('uniqueTags')}
          </span>
          <span>
            <span className="text-gray-300 tabular-nums">{taggedPosts.toLocaleString()}</span>{' '}
            {t('taggedPosts')}
          </span>
        </div>
      </div>

      <div className="grid grid-cols-1 gap-4">
        {/* Coverage */}
        <div className="rounded-lg border border-[#2e2e2e] bg-[#111111] p-3">
          <p className="text-[10px] font-semibold uppercase tracking-widest text-gray-500 mb-2">
            {t('coverage')}
          </p>
          <div className="flex items-baseline gap-1.5 mb-2">
            <span className="text-2xl font-semibold text-white tabular-nums">
              {analyzed.toLocaleString()}
            </span>
            <span className="text-sm text-gray-500">
              {t('analyzedOf', { total: total.toLocaleString() })}
            </span>
          </div>
          <div className="h-2 rounded-full bg-[#1a1a1a] overflow-hidden mb-2">
            <div
              className="h-full rounded-full u-progress"
              style={{ width: `${coverage}%`, backgroundColor: ACCENT }}
            />
          </div>
          {analyzeActive > 0 ? (
            <button
              data-testid="cancel-analyze-btn"
              onClick={onCancelAnalyze}
              title={t('cancelAnalyzeTitle')}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded-md text-sm text-red-300 bg-[#1a1a1a] hover:bg-[#241619] u-press"
            >
              <X size={14} /> {t('cancelAnalyze', { n: analyzeActive.toLocaleString() })}
            </button>
          ) : unanalyzed > 0 ? (
            <button
              data-testid="analyze-missing-btn"
              onClick={onAnalyzeMissing}
              disabled={analyzing}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded-md text-sm text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] disabled:opacity-50 u-press"
            >
              {analyzing ? <RefreshCw size={14} className="animate-spin" /> : <Wand2 size={14} />}
              {t('analyzeMissing', { n: unanalyzed.toLocaleString() })}
            </button>
          ) : null}
        </div>
      </div>
    </div>
  );
}

// ────────────────────────────────────────────────────────────────────────────
// Merge / rename modal
// ────────────────────────────────────────────────────────────────────────────

interface MergeModalProps {
  suggestions: Shelfy.TagMergeSuggestion[];
  onClose: () => void;
  onMerge: (sources: string[], target: string) => void;
  onRename: (from: string, to: string) => Promise<void> | void;
  busy: boolean;
}

function MergeModal({ suggestions, onClose, onMerge, onRename, busy }: MergeModalProps) {
  const t = useT('aiTags');
  const [renameFrom, setRenameFrom] = useState<string>('');
  const [renameTo, setRenameTo] = useState<string>('');

  return (
    <div
      className="fixed inset-0 z-40 flex items-center justify-center bg-black/60 p-4 u-backdrop-in"
      onClick={onClose}
    >
      <div
        className="w-full max-w-lg max-h-[80vh] flex flex-col rounded-xl border border-[#2e2e2e] bg-[#111111] shadow-2xl u-dialog-in"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center gap-2 px-4 py-3 border-b border-[#2e2e2e]">
          <GitMerge size={16} style={{ color: ACCENT }} />
          <h2 className="text-white text-sm font-semibold">{t('mergeTitle')}</h2>
          <div className="flex-1" />
          <button onClick={onClose} className="text-gray-400 hover:text-white u-press">
            <X size={18} />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e] p-4 space-y-4">
          {/* Suggestions */}
          <div>
            <p className="text-[10px] font-semibold uppercase tracking-widest text-gray-500 mb-2">
              {t('suggestions', { n: suggestions.length })}
            </p>
            {suggestions.length === 0 && (
              <p className="text-xs text-gray-600">{t('noMergeSuggestions')}</p>
            )}
            <div className="space-y-2">
              {suggestions.map((s, i) => (
                <div
                  key={s.canonical}
                  className="flex items-center gap-2 rounded-lg border border-[#2e2e2e] bg-[#0f0f0f] p-2.5 u-fade-in-up"
                  style={{ animationDelay: `${Math.min(i, 8) * 30}ms` }}
                >
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-1.5 flex-wrap">
                      {(s.variants || []).map((v) => (
                        <span
                          key={v}
                          className="rounded bg-[#1a1a1a] px-1.5 py-0.5 text-[11px] text-gray-400 line-through"
                        >
                          {v}
                        </span>
                      ))}
                      <span className="text-gray-600 text-xs">→</span>
                      <span
                        className="rounded px-1.5 py-0.5 text-[11px] font-medium text-white"
                        style={{ backgroundColor: ACCENT }}
                      >
                        {s.canonical}
                      </span>
                    </div>
                    <p className="text-[10px] text-gray-600 mt-1 tabular-nums">
                      {t('totalPosts', { n: s.totalCount })}
                    </p>
                  </div>
                  <button
                    onClick={() => onMerge(s.variants || [], s.canonical)}
                    disabled={busy}
                    className="shrink-0 px-3 py-1.5 rounded-md text-xs text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] disabled:opacity-50 u-press"
                  >
                    {t('merge')}
                  </button>
                </div>
              ))}
            </div>
          </div>

          {/* Manual rename */}
          <div className="border-t border-[#2e2e2e] pt-4">
            <p className="text-[10px] font-semibold uppercase tracking-widest text-gray-500 mb-2">
              {t('manualRename')}
            </p>
            <div className="flex items-center gap-2">
              <input
                value={renameFrom}
                onChange={(e: React.ChangeEvent<HTMLInputElement>) => setRenameFrom(e.target.value)}
                placeholder={t('renameFromPlaceholder')}
                className="flex-1 min-w-0 rounded-md bg-[#0f0f0f] border border-[#2e2e2e] px-2.5 py-1.5 text-sm text-gray-200 placeholder-gray-600 focus:border-[#7B5CFF] outline-none"
              />
              <span className="text-gray-600">→</span>
              <input
                value={renameTo}
                onChange={(e: React.ChangeEvent<HTMLInputElement>) => setRenameTo(e.target.value)}
                placeholder={t('renameToPlaceholder')}
                className="flex-1 min-w-0 rounded-md bg-[#0f0f0f] border border-[#2e2e2e] px-2.5 py-1.5 text-sm text-gray-200 placeholder-gray-600 focus:border-[#7B5CFF] outline-none"
              />
              <button
                onClick={async () => {
                  if (!renameFrom.trim() || !renameTo.trim()) return;
                  await onRename(renameFrom.trim(), renameTo.trim());
                  setRenameFrom('');
                  setRenameTo('');
                }}
                disabled={busy || !renameFrom.trim() || !renameTo.trim()}
                className="shrink-0 flex items-center gap-1.5 px-3 py-1.5 rounded-md text-sm text-gray-200 bg-[#1a1a1a] hover:bg-[#222] disabled:opacity-50 u-press"
              >
                <Pencil size={13} /> {t('rename')}
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

// ────────────────────────────────────────────────────────────────────────────
// Main view
// ────────────────────────────────────────────────────────────────────────────

const RESULT_LIMIT = 60;

interface AiTagsProps {
  initialTag?: string | null;
  initialTagNonce?: number;
  active?: boolean;
  onOpenInWebsites?: () => void;
  onReanalyzeWeb?: (post: Shelfy.Post) => void;
}

export default function AiTags({
  initialTag = null,
  initialTagNonce = 0,
  active = true,
  onOpenInWebsites,
  onReanalyzeWeb,
}: AiTagsProps) {
  const {
    overview,
    clusters,
    entityStats,
    health,
    mergeSuggestions,
    aliases,
    loading,
    error,
    refresh,
    renameTag,
    mergeTags,
    analyzeMissing,
    regenerateClusters,
    cancelClusters,
    acceptCluster,
    acceptAllClusters,
    dismissCluster,
    renameCluster,
    removeTagFromCluster,
    proposeAliases,
    cancelAliasProposals,
    acceptAlias,
    dismissAlias,
    acceptAllAliases,
    getPostIdsByTags,
    getTagCooccurrence,
    fetchPosts,
  } = useAiTags({ active });

  const t = useT('aiTags');
  const tc = useT('common');

  // Shared analysis job state — used to surface a cancel control while a global
  // auto-tag run (queued from this screen) is still in flight.
  const { jobs: analysisJobs, cancelAll: cancelAllAnalyze } = useAnalysis();
  // Cheap O(n) derivations — recompute inline rather than paying useMemo's
  // dependency-compare overhead for a single counted filter.
  const analyzeActive = (analysisJobs || []).filter((j) =>
    ['pending', 'extracting', 'analyzing'].includes(j.status),
  ).length;

  // Render every cluster — the column is overflow-y-auto (scrollable), and the
  // "Accetta tutti" action + proposedCount operate on the same full set, so capping
  // the rendered list would hide clusters that stay reachable by those actions.
  const visibleClusters = clusters;
  // Number of proposed (LLM-suggested) clusters still awaiting review.
  const proposedCount = clusters.filter((c) => c.status === 'proposed').length;
  // Number of proposed tag aliases (synonyms) still awaiting review.
  const proposedAliasCount = aliases.filter((a) => a.status === 'proposed').length;

  // ── Active filter state for the results pane ──────────────────────────────
  const [selectedTags, setSelectedTags] = useState<string[]>(initialTag ? [initialTag] : []);
  const [tagMode, setTagMode] = useState<TagMode>('or'); // 'and' | 'or'
  const [entity, setEntity] = useState<string | undefined>(undefined);

  // Apply a tag handed over from the global AI modal. Keyed on the nonce so the
  // same tag re-applies on a fresh click; skips the initial 0 to avoid clobbering
  // any in-view selection on first mount.
  useEffect(() => {
    if (initialTagNonce > 0 && initialTag) {
      setEntity(undefined);
      setSelectedTags([initialTag]);
    }
  }, [initialTagNonce, initialTag]);

  // ── Results ───────────────────────────────────────────────────────────────
  const [results, setResults] = useState<Shelfy.Post[]>([]);
  const [resultTotal, setResultTotal] = useState<number>(0);
  const [resultsLoading, setResultsLoading] = useState<boolean>(false);
  const [resultsNonce, setResultsNonce] = useState<number>(0); // bump to force a refetch

  // ── Post detail modal (opened from the results grid) ──────────────────────
  const [activePost, setActivePost] = useState<Shelfy.Post | null>(null);
  // Scroll container for the (row-virtualized) results grid.
  const resultsScrollRef = useRef<HTMLDivElement | null>(null);

  // ── Related tags ("see also") for a single selected tag ───────────────────
  const [related, setRelated] = useState<Shelfy.TagCount[]>([]);

  // ── UI state ──────────────────────────────────────────────────────────────
  const [mergeOpen, setMergeOpen] = useState<boolean>(false);
  const [analyzing, setAnalyzing] = useState<boolean>(false);
  const [busy, setBusy] = useState<boolean>(false);
  const [clustering, setClustering] = useState<boolean>(false);
  const [clusterProgress, setClusterProgress] = useState<ProgressState | null>(null);
  const [aliasing, setAliasing] = useState<boolean>(false);
  const [aliasProgress, setAliasProgress] = useState<ProgressState | null>(null);
  const { toast, toastClosing, showToast } = useToast();

  const clusterCancelledRef = useRef<boolean>(false);
  const aliasCancelledRef = useRef<boolean>(false);

  const hasFilter = selectedTags.length > 0 || !!entity;

  // ── Fetch results whenever the active filter changes ──────────────────────
  const reqRef = useRef<number>(0);
  useEffect(() => {
    if (!hasFilter) {
      setResults([]);
      setResultTotal(0);
      return;
    }
    const reqId = ++reqRef.current;
    setResultsLoading(true);
    fetchPosts({
      tags: selectedTags,
      tagMode,
      entity,
      limit: RESULT_LIMIT,
      offset: 0,
    })
      .then((res) => {
        if (reqId !== reqRef.current) return;
        setResults(res?.posts ?? []);
        setResultTotal(res?.total ?? 0);
      })
      .catch((err: unknown) => {
        if (reqId !== reqRef.current) return;
        console.error('[AiTags] fetchPosts error:', err);
        setResults([]);
        setResultTotal(0);
      })
      .finally(() => {
        if (reqId === reqRef.current) setResultsLoading(false);
      });
  }, [selectedTags, tagMode, entity, hasFilter, fetchPosts, resultsNonce]);

  // ── Load related tags when exactly one tag is selected ────────────────────
  useEffect(() => {
    if (selectedTags.length === 1) {
      let alive = true;
      getTagCooccurrence(selectedTags[0], 12)
        .then((r: Shelfy.TagCount[]) => {
          if (alive) setRelated(Array.isArray(r) ? r : []);
        })
        .catch(() => {
          if (alive) setRelated([]);
        });
      return () => {
        alive = false;
      };
    }
    setRelated([]);
    return undefined;
  }, [selectedTags, getTagCooccurrence]);

  // ── Filter manipulation ───────────────────────────────────────────────────
  const addTag = useCallback((tag: string) => {
    setEntity(undefined);
    setSelectedTags((prev) => (prev.includes(tag) ? prev : [...prev, tag]));
  }, []);
  const removeTag = useCallback((tag: string) => {
    setSelectedTags((prev) => prev.filter((t) => t !== tag));
  }, []);
  const clearFilters = useCallback(() => {
    setSelectedTags([]);
    setEntity(undefined);
  }, []);

  const selectCluster = useCallback((cluster: Shelfy.TagCluster) => {
    setEntity(undefined);
    setTagMode('or');
    setSelectedTags(cluster.tags || []);
  }, []);

  const selectEntity = useCallback((e: string) => {
    setSelectedTags([]);
    setEntity((prev) => (prev === e ? undefined : e));
  }, []);

  // ── Keyboard activation helper for nav items ──────────────────────────────
  const onKeyActivate = (fn: () => void) => (e: React.KeyboardEvent<HTMLButtonElement>) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      fn();
    }
  };

  // ── Actions ───────────────────────────────────────────────────────────────
  const handleAnalyzeMissing = useCallback(async () => {
    setAnalyzing(true);
    try {
      const res = await analyzeMissing();
      showToast(t('queuedToAnalyze', { n: res?.queued ?? 0 }));
    } catch (err) {
      showToast(t('analyzeStartError'));
    } finally {
      setAnalyzing(false);
    }
  }, [analyzeMissing, showToast, t]);

  const handleMerge = useCallback(
    async (sources: string[], target: string) => {
      setBusy(true);
      try {
        const res = await mergeTags(sources, target);
        showToast(t('mergedRefs', { n: res?.updated ?? 0, target }));
      } catch (err) {
        showToast(t('mergeError'));
      } finally {
        setBusy(false);
      }
    },
    [mergeTags, showToast, t],
  );

  const handleRename = useCallback(
    async (from: string, to: string) => {
      setBusy(true);
      try {
        const res = await renameTag(from, to);
        showToast(t('renamedRefs', { n: res?.updated ?? 0, from, to }));
        // keep filter in sync if the renamed tag was selected
        setSelectedTags((prev) => prev.map((tag) => (tag === from ? to : tag)));
      } catch (err) {
        showToast(t('renameError'));
      } finally {
        setBusy(false);
      }
    },
    [renameTag, showToast, t],
  );

  // ── Cluster review ──────────────────────────────────────────────────────────
  const handleRegenerate = useCallback(async () => {
    clusterCancelledRef.current = false;
    setClustering(true);
    setClusterProgress({ done: 0, total: 0 });
    try {
      const res = await regenerateClusters((p: unknown) => setClusterProgress(p as ProgressState));
      const summary = res as { count?: number; candidates?: number } | undefined;
      showToast(
        t('proposedClusters', {
          count: summary?.count ?? 0,
          candidates: summary?.candidates ?? 0,
        }),
      );
    } catch (err) {
      if (clusterCancelledRef.current) showToast(t('regenCancelled'));
      else
        showToast(
          String((err as { message?: string } | null)?.message ?? '').includes('MODEL_NOT_READY')
            ? t('modelNotReady')
            : t('regenError'),
        );
    } finally {
      setClustering(false);
      setClusterProgress(null);
    }
  }, [regenerateClusters, showToast, t]);

  const handleCancelClusters = useCallback(async () => {
    clusterCancelledRef.current = true;
    try {
      await cancelClusters();
    } catch {
      /* ignore */
    }
  }, [cancelClusters]);

  const handleCancelAnalyze = useCallback(async () => {
    // cancelAllAnalyze wipes the ENTIRE shared analyze queue (every in-flight or
    // queued job across the app, not just this screen's analyze-missing batch),
    // so confirm before destroying the user's whole backlog.
    if (!window.confirm(t('confirmCancelAnalyze'))) {
      return;
    }
    try {
      await cancelAllAnalyze();
      showToast(t('analyzeCancelled'));
    } catch {
      showToast(t('cancelError'));
    }
  }, [cancelAllAnalyze, showToast, t]);

  const handleAcceptCluster = useCallback(
    async (id: number) => {
      setBusy(true);
      try {
        await acceptCluster(id);
        showToast(t('clusterAccepted'));
      } catch {
        showToast(tc('error'));
      } finally {
        setBusy(false);
      }
    },
    [acceptCluster, showToast, t, tc],
  );

  const handleAcceptAllClusters = useCallback(async () => {
    setBusy(true);
    try {
      const res = await acceptAllClusters();
      showToast(t('clustersAccepted', { n: res?.accepted ?? 0 }));
    } catch {
      showToast(tc('error'));
    } finally {
      setBusy(false);
    }
  }, [acceptAllClusters, showToast, t, tc]);

  const handleDismissCluster = useCallback(
    async (id: number) => {
      setBusy(true);
      try {
        await dismissCluster(id);
        showToast(t('clusterDismissed'));
      } catch {
        showToast(tc('error'));
      } finally {
        setBusy(false);
      }
    },
    [dismissCluster, showToast, t, tc],
  );

  const handleRenameCluster = useCallback(
    (id: number, label: string) => renameCluster(id, label),
    [renameCluster],
  );
  const handleRemoveTagFromCluster = useCallback(
    (tag: string, id: number) => removeTagFromCluster(tag, id),
    [removeTagFromCluster],
  );

  // ── Alias / synonym review ────────────────────────────────────────────────────
  const handleProposeAliases = useCallback(async () => {
    aliasCancelledRef.current = false;
    setAliasing(true);
    setAliasProgress({ done: 0, total: 0 });
    try {
      const res = await proposeAliases((p: unknown) => setAliasProgress(p as ProgressState));
      showToast(t('proposedAliases', { n: res?.proposed ?? 0 }));
    } catch (err) {
      if (aliasCancelledRef.current) showToast(t('aliasProposalCancelled'));
      else
        showToast(
          String((err as { message?: string } | null)?.message ?? '').includes('MODEL_NOT_READY')
            ? t('modelNotReady')
            : t('aliasProposalError'),
        );
    } finally {
      setAliasing(false);
      setAliasProgress(null);
    }
  }, [proposeAliases, showToast, t]);

  const handleCancelAliases = useCallback(async () => {
    aliasCancelledRef.current = true;
    try {
      await cancelAliasProposals();
    } catch {
      /* ignore */
    }
  }, [cancelAliasProposals]);

  const handleAcceptAlias = useCallback(
    async (aliasNorm: string) => {
      setBusy(true);
      try {
        await acceptAlias(aliasNorm);
        showToast(t('aliasApplied'));
      } catch {
        showToast(tc('error'));
      } finally {
        setBusy(false);
      }
    },
    [acceptAlias, showToast, t, tc],
  );

  const handleDismissAlias = useCallback(
    async (aliasNorm: string) => {
      setBusy(true);
      try {
        await dismissAlias(aliasNorm);
        showToast(t('aliasDismissed'));
      } catch {
        showToast(tc('error'));
      } finally {
        setBusy(false);
      }
    },
    [dismissAlias, showToast, t, tc],
  );

  const handleAcceptAllAliases = useCallback(async () => {
    setBusy(true);
    try {
      const res = await acceptAllAliases();
      showToast(t('aliasesApplied', { n: res?.accepted ?? 0 }));
    } catch {
      showToast(tc('error'));
    } finally {
      setBusy(false);
    }
  }, [acceptAllAliases, showToast, t, tc]);

  // ── Export / promote ──────────────────────────────────────────────────────
  const collectableIds = useMemo(() => results.map((p) => p.id), [results]);

  const handleCopyLinks = useCallback(async () => {
    const { status, n } = await copyPostLinks(results);
    if (status === 'empty') showToast(t('noLinksToCopy'));
    else if (status === 'copied') showToast(t('linksCopied', { n }));
    else showToast(t('copyFailed'));
  }, [results, showToast, t]);

  const handleExportMarkdown = useCallback(() => {
    if (results.length === 0) {
      showToast(t('noResultsToExport'));
      return;
    }
    downloadMarkdown(postsToMarkdown(results), `shelfy-aitags-${Date.now()}.md`);
    showToast(t('exportedToMarkdown', { n: results.length }));
  }, [results, showToast, t]);

  const handlePromoteCollection = useCallback(async () => {
    let ids = collectableIds;
    setBusy(true);
    try {
      // Prefer the full set matching the active filter when available — `results`
      // is only the first RESULT_LIMIT page, so collecting `collectableIds` alone
      // would silently truncate filters that match more posts than the page size.
      if (selectedTags.length > 0) {
        const full = await getPostIdsByTags(selectedTags, tagMode);
        if (Array.isArray(full) && full.length) ids = full;
      } else if (entity) {
        // No dedicated entity helper exists; reuse the uncapped getPostIds path,
        // which feeds the same buildPostFilter `entity` branch as the results grid.
        const full = await window.electronAPI.getPostIds({ entity });
        if (Array.isArray(full) && full.length) ids = full;
      }
      if (ids.length === 0) {
        showToast(t('noPostsToCollect'));
        return;
      }
      const name = window.prompt(
        t('newCollectionPrompt'),
        defaultCollectionName(selectedTags, entity, t('defaultCollectionName')),
      );
      if (!name) return;
      const created = await window.electronAPI.createCollection(name, ACCENT);
      if (!created?.id) {
        showToast(t('collectionCreateError'));
        return;
      }
      const res = await window.electronAPI.addPostsToCollections(ids, [created.id]);
      // Trust the backend's reported count; only show the number when it's a real
      // value (don't pretend all ids were added when `added` is missing).
      showToast(
        typeof res?.added === 'number'
          ? t('collectionCreatedWith', { name, n: res.added })
          : t('collectionCreated', { name }),
      );
    } catch (err) {
      console.error('[AiTags] promote error:', err);
      showToast(t('collectionCreateError'));
    } finally {
      setBusy(false);
    }
  }, [collectableIds, selectedTags, tagMode, entity, getPostIdsByTags, showToast, t]);

  const isEmpty = !loading && overview && overview.analyzed === 0;

  // ── Render ────────────────────────────────────────────────────────────────
  if (loading) {
    return (
      <div data-testid="aitags-view" className="flex items-center justify-center h-full">
        <RefreshCw size={22} className="text-[#555] animate-spin" strokeWidth={1.5} />
      </div>
    );
  }

  if (error) {
    return (
      <div
        data-testid="aitags-view"
        className="flex flex-col items-center justify-center h-full gap-3 px-6 text-center u-fade-in"
      >
        <AlertTriangle size={36} className="text-red-400/70" strokeWidth={1} />
        <p className="text-red-400 text-sm max-w-sm">{error}</p>
        <button
          onClick={refresh}
          className="px-3 py-1.5 rounded-md text-sm text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] u-press"
        >
          {tc('retry')}
        </button>
      </div>
    );
  }

  if (isEmpty) {
    return (
      <div data-testid="aitags-view" className="flex flex-col h-full">
        <div className="flex flex-col items-center justify-center flex-1 gap-4 px-6 text-center u-fade-in-up">
          <Sparkles size={40} style={{ color: ACCENT }} strokeWidth={1.25} />
          <div>
            <p className="text-gray-200 text-base font-medium">{t('noPostsAnalyzed')}</p>
            <p className="text-gray-500 text-sm mt-1 max-w-sm">{t('emptyHint')}</p>
          </div>
          <button
            data-testid="empty-analyze-btn"
            onClick={handleAnalyzeMissing}
            disabled={analyzing}
            className="flex items-center gap-1.5 px-4 py-2 rounded-md text-sm text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] disabled:opacity-50 u-press"
          >
            {analyzing ? <RefreshCw size={15} className="animate-spin" /> : <Wand2 size={15} />}
            {t('analyzeUnanalyzed')}
          </button>
        </div>
        {toast && <Toast closing={toastClosing}>{toast}</Toast>}
      </div>
    );
  }

  return (
    <div data-testid="aitags-view" className="flex flex-col h-full overflow-hidden">
      <Dashboard
        overview={overview}
        analyzing={analyzing}
        analyzeActive={analyzeActive}
        onAnalyzeMissing={handleAnalyzeMissing}
        onCancelAnalyze={handleCancelAnalyze}
      />

      {/* Two-column body */}
      <div className="flex-1 flex overflow-hidden">
        {/* ── Left: navigation (clusters / cloud / entities / health) ───────── */}
        <div className="w-[340px] min-w-[340px] border-r border-[#2e2e2e] overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e] p-3 space-y-5">
          {/* 1. Clusters */}
          <section>
            <SectionTitle
              icon={Boxes}
              right={
                <div className="flex items-center gap-2">
                  {proposedCount > 0 && (
                    <button
                      data-testid="accept-all-clusters-btn"
                      onClick={handleAcceptAllClusters}
                      disabled={busy}
                      className="flex items-center gap-1 text-[10px] text-gray-500 hover:text-gray-200 u-press disabled:opacity-50"
                      title={t('acceptAllClustersTitle')}
                    >
                      <Check size={12} /> {t('acceptAll', { n: proposedCount })}
                    </button>
                  )}
                  {clustering ? (
                    <button
                      onClick={handleCancelClusters}
                      className="flex items-center gap-1 text-[10px] text-red-300 hover:text-red-200 u-press"
                      title={t('stopRegenTitle')}
                    >
                      <X size={12} /> {t('stop')}
                    </button>
                  ) : (
                    <button
                      onClick={handleRegenerate}
                      className="flex items-center gap-1 text-[10px] text-gray-500 hover:text-gray-200 u-press"
                      title={t('regenerateTitle')}
                    >
                      <Wand2 size={12} /> {t('regenerate')}
                    </button>
                  )}
                  <button
                    onClick={() => setMergeOpen(true)}
                    className="flex items-center gap-1 text-[10px] text-gray-500 hover:text-gray-200 u-press"
                    title={t('manageTitle')}
                  >
                    <GitMerge size={12} /> {t('manage')}
                  </button>
                </div>
              }
            >
              {t('clusters')}
            </SectionTitle>

            {clustering && clusterProgress && (
              <div className="px-1 mb-2">
                <div className="h-1.5 rounded-full bg-[#1a1a1a] overflow-hidden">
                  <div
                    className="h-full rounded-full u-progress"
                    style={{
                      width: `${clusterProgress.total ? Math.round((clusterProgress.done / clusterProgress.total) * 100) : 5}%`,
                      backgroundColor: ACCENT,
                    }}
                  />
                </div>
                <p className="text-[10px] text-gray-500 mt-1 tabular-nums">
                  {t('analyzingGroups', {
                    done: clusterProgress.done,
                    total: clusterProgress.total || '…',
                  })}
                </p>
              </div>
            )}

            <div className="space-y-1.5">
              {clusters.length === 0 && !clustering && (
                <div className="px-1 py-2">
                  <p className="text-xs text-gray-600 mb-2">{t('noClusters')}</p>
                  <button
                    onClick={handleRegenerate}
                    className="flex items-center gap-1.5 px-2.5 py-1.5 rounded-md text-xs text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] u-press"
                  >
                    <Wand2 size={13} /> {t('regenerateClusters')}
                  </button>
                </div>
              )}
              {visibleClusters.map((cl) => (
                <ClusterCard
                  key={cl.id}
                  cluster={cl}
                  busy={busy}
                  onSelect={selectCluster}
                  onAccept={handleAcceptCluster}
                  onDismiss={handleDismissCluster}
                  onRename={handleRenameCluster}
                  onRemoveTag={handleRemoveTagFromCluster}
                  onKeyActivate={onKeyActivate}
                />
              ))}
            </div>
          </section>

          {/* 2. Alias / synonyms */}
          <section>
            <SectionTitle
              icon={Link2}
              right={
                <div className="flex items-center gap-2">
                  {proposedAliasCount > 0 && (
                    <button
                      data-testid="accept-all-aliases-btn"
                      onClick={handleAcceptAllAliases}
                      disabled={busy}
                      className="flex items-center gap-1 text-[10px] text-gray-500 hover:text-gray-200 u-press disabled:opacity-50"
                      title={t('acceptAllAliasesTitle')}
                    >
                      <Check size={12} /> {t('acceptAll', { n: proposedAliasCount })}
                    </button>
                  )}
                  {aliasing ? (
                    <button
                      onClick={handleCancelAliases}
                      className="flex items-center gap-1 text-[10px] text-red-300 hover:text-red-200 u-press"
                      title={t('stopAliasTitle')}
                    >
                      <X size={12} /> {t('stop')}
                    </button>
                  ) : (
                    <button
                      onClick={handleProposeAliases}
                      className="flex items-center gap-1 text-[10px] text-gray-500 hover:text-gray-200 u-press"
                      title={t('generateProposalsTitle')}
                    >
                      <Wand2 size={12} /> {t('generateProposals')}
                    </button>
                  )}
                </div>
              }
            >
              {t('aliases')}
            </SectionTitle>

            <p className="px-1 mb-2 text-[10px] leading-snug text-gray-600">
              {t('aliasHintPre')}
              <span className="text-gray-500">{t('aliasHintEm')}</span>
              {t('aliasHintPost')}
            </p>

            {aliasing && aliasProgress && (
              <div className="px-1 mb-2">
                <div className="h-1.5 rounded-full bg-[#1a1a1a] overflow-hidden">
                  <div
                    className="h-full rounded-full u-progress"
                    style={{
                      width: `${aliasProgress.total ? Math.round((aliasProgress.done / aliasProgress.total) * 100) : 5}%`,
                      backgroundColor: ACCENT,
                    }}
                  />
                </div>
                <p className="text-[10px] text-gray-500 mt-1 tabular-nums">
                  {t('analyzingTags', {
                    done: aliasProgress.done,
                    total: aliasProgress.total || '…',
                  })}
                </p>
              </div>
            )}

            <div className="space-y-1.5">
              {aliases.length === 0 && !aliasing && (
                <div className="px-1 py-2">
                  <p className="text-xs text-gray-600 mb-2">{t('noAliases')}</p>
                  <button
                    onClick={handleProposeAliases}
                    className="flex items-center gap-1.5 px-2.5 py-1.5 rounded-md text-xs text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] u-press"
                  >
                    <Wand2 size={13} /> {t('generateProposals')}
                  </button>
                </div>
              )}
              {aliases.map((a) => (
                <AliasCard
                  key={a.aliasNorm}
                  alias={a}
                  busy={busy}
                  onAccept={handleAcceptAlias}
                  onDismiss={handleDismissAlias}
                />
              ))}
            </div>
          </section>

          {/* 7. Entity index */}
          <section>
            <SectionTitle icon={Users}>{t('entities')}</SectionTitle>
            <div className="flex flex-wrap gap-1.5 px-1">
              {entityStats.length === 0 && (
                <p className="text-xs text-gray-600">{t('noEntities')}</p>
              )}
              {entityStats.map((e) => (
                <Chip
                  key={e.entity}
                  label={e.entity}
                  count={e.count}
                  active={entity === e.entity}
                  onClick={() => selectEntity(e.entity)}
                />
              ))}
            </div>
          </section>

          {/* 9. Health */}
          {health && (
            <section>
              <SectionTitle icon={Heart}>{t('tagsToFix')}</SectionTitle>
              <div className="grid grid-cols-2 gap-2 px-1 mb-2">
                <HealthStat label={t('untagged')} value={health.untaggedPosts} />
                <HealthStat label={t('unanalyzed')} value={health.unanalyzedPosts} />
                <HealthStat label={t('rareTags')} value={health.rareTags} />
                <HealthStat label={t('orphans')} value={(health.orphanTags || []).length} />
              </div>
              {(health.orphanTags || []).length > 0 && (
                <div className="flex flex-wrap gap-1.5 px-1">
                  {health.orphanTags.map((o) => (
                    <Chip key={o.tag} label={o.tag} count={o.count} onClick={() => addTag(o.tag)} />
                  ))}
                </div>
              )}
            </section>
          )}
        </div>

        {/* ── Right: filters + results grid ─────────────────────────────────── */}
        <div className="flex-1 flex flex-col overflow-hidden">
          {/* Filter bar */}
          <div className="border-b border-[#2e2e2e] bg-[#0f0f0f] px-3 py-2.5 space-y-2">
            <div className="flex items-center gap-2 flex-wrap">
              {/* AND/OR toggle */}
              <div className="inline-flex rounded-md border border-[#2e2e2e] overflow-hidden">
                {(['and', 'or'] as TagMode[]).map((m) => (
                  <button
                    key={m}
                    onClick={() => setTagMode(m)}
                    disabled={selectedTags.length < 2}
                    className={[
                      'px-2.5 py-1 text-xs uppercase u-press disabled:opacity-40',
                      tagMode === m
                        ? 'bg-[#7B5CFF] text-white'
                        : 'text-gray-400 hover:bg-[#1a1a1a]',
                    ].join(' ')}
                  >
                    {m}
                  </button>
                ))}
              </div>

              {/* Active filter chips */}
              {selectedTags.map((tag) => (
                <Chip key={tag} label={tag} active onRemove={() => removeTag(tag)} />
              ))}
              {entity && (
                <Chip
                  label={t('entityChip', { name: entity })}
                  active
                  onRemove={() => setEntity(undefined)}
                />
              )}

              {hasFilter && (
                <button
                  onClick={clearFilters}
                  className="text-xs text-gray-500 hover:text-gray-200 u-press"
                >
                  {t('clearFilters')}
                </button>
              )}

              <div className="flex-1" />

              <span data-testid="result-count" className="text-xs text-gray-400 tabular-nums">
                {hasFilter ? t('resultsCount', { n: resultTotal.toLocaleString() }) : t('noFilter')}
              </span>

              {hasFilter && results.length > 0 && <GridSizeControl />}
            </div>

            {/* 5. Related tags */}
            {related.length > 0 && (
              <div className="flex items-center gap-1.5 flex-wrap">
                <span className="text-[10px] uppercase tracking-widest text-gray-600 mr-1">
                  {t('related')}
                </span>
                {related.map((r) => (
                  <Chip key={r.tag} label={r.tag} count={r.count} onClick={() => addTag(r.tag)} />
                ))}
              </div>
            )}

            {/* Result actions */}
            {hasFilter && results.length > 0 && (
              <div className="flex items-center gap-2 flex-wrap">
                <button
                  data-testid="promote-collection-btn"
                  onClick={handlePromoteCollection}
                  disabled={busy}
                  className="flex items-center gap-1.5 px-2.5 py-1 rounded-md text-xs text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] disabled:opacity-50 u-press"
                >
                  <FolderPlus size={13} /> {t('createCollection')}
                </button>
                <button
                  onClick={handleCopyLinks}
                  className="flex items-center gap-1.5 px-2.5 py-1 rounded-md text-xs text-gray-300 bg-[#1a1a1a] hover:bg-[#222] u-press"
                >
                  <ClipboardCopy size={13} /> {t('copyLinks')}
                </button>
                <button
                  onClick={handleExportMarkdown}
                  className="flex items-center gap-1.5 px-2.5 py-1 rounded-md text-xs text-gray-300 bg-[#1a1a1a] hover:bg-[#222] u-press"
                >
                  <FileDown size={13} /> {t('exportMarkdown')}
                </button>
              </div>
            )}
          </div>

          {/* Results grid */}
          <div
            ref={resultsScrollRef}
            className="flex-1 overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e]"
          >
            {!hasFilter && (
              <div className="flex flex-col items-center justify-center h-full min-h-[40vh] gap-2 text-center px-6 u-fade-in">
                <Tags size={32} className="text-[#333]" strokeWidth={1} />
                <p className="text-[#555] text-sm max-w-xs">{t('pickPrompt')}</p>
              </div>
            )}

            {hasFilter && resultsLoading && results.length === 0 && <PostGridSkeleton />}

            {hasFilter && !resultsLoading && results.length === 0 && (
              <div className="flex flex-col items-center justify-center h-full min-h-[40vh] gap-2 text-center px-6 u-fade-in">
                <X size={32} className="text-[#333]" strokeWidth={1} />
                <p className="text-[#555] text-sm">{t('noPostsForFilter')}</p>
                {/* AND across many tags (e.g. a whole cluster) almost never matches:
                    a single post rarely carries every tag — hint at switching to OR. */}
                {tagMode === 'and' && selectedTags.length > 3 && (
                  <button
                    onClick={() => setTagMode('or')}
                    className="text-xs text-[#7B5CFF] hover:text-[#9b85ff] u-press"
                  >
                    {t('switchToOr')}
                  </button>
                )}
              </div>
            )}

            {results.length > 0 && (
              <VirtualPostGrid
                testId="aitags-grid"
                posts={results}
                scrollRef={resultsScrollRef}
                onOpen={setActivePost}
              />
            )}
          </div>
        </div>
      </div>

      {mergeOpen && (
        <MergeModal
          suggestions={mergeSuggestions}
          busy={busy}
          onClose={() => setMergeOpen(false)}
          onMerge={handleMerge}
          onRename={handleRename}
        />
      )}

      {activePost && (
        <PostModal
          post={activePost}
          onClose={() => setActivePost(null)}
          onPrev={() =>
            setActivePost((cur) => {
              if (!cur) return cur;
              const i = results.findIndex((p) => p.id === cur.id);
              return i > 0 ? results[i - 1] : cur;
            })
          }
          onNext={() =>
            setActivePost((cur) => {
              if (!cur) return cur;
              const i = results.findIndex((p) => p.id === cur.id);
              return i >= 0 && i < results.length - 1 ? results[i + 1] : cur;
            })
          }
          hasPrev={results.findIndex((p) => p.id === activePost.id) > 0}
          hasNext={(() => {
            const i = results.findIndex((p) => p.id === activePost.id);
            return i >= 0 && i < results.length - 1;
          })()}
          onApplyAiFilter={(patch: { tag?: string }) => {
            if (patch?.tag) {
              setEntity(undefined);
              setSelectedTags([patch.tag]);
            }
            setActivePost(null);
          }}
          onLocalFilesDeleted={() => setResultsNonce((n) => n + 1)}
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
              ? (p: Shelfy.Post) => {
                  setActivePost(null);
                  onReanalyzeWeb(p);
                }
              : undefined
          }
        />
      )}

      {toast && <Toast closing={toastClosing}>{toast}</Toast>}
    </div>
  );
}

// ────────────────────────────────────────────────────────────────────────────
// Tiny leaf components
// ────────────────────────────────────────────────────────────────────────────

interface ClusterCardProps {
  cluster: Shelfy.TagCluster;
  busy: boolean;
  onSelect: (cluster: Shelfy.TagCluster) => void;
  onAccept: (id: number) => void;
  onDismiss: (id: number) => void;
  onRename: (id: number, label: string) => void;
  onRemoveTag: (tag: string, id: number) => void;
  onKeyActivate: (fn: () => void) => (e: React.KeyboardEvent<HTMLButtonElement>) => void;
}

// A single cluster in the left nav. Accepted clusters are clickable to filter
// the results; proposed clusters (LLM suggestions awaiting review) get an
// editable name, removable tag chips, and accept/dismiss controls.
function ClusterCard({
  cluster,
  busy,
  onSelect,
  onAccept,
  onDismiss,
  onRename,
  onRemoveTag,
  onKeyActivate,
}: ClusterCardProps) {
  const t = useT('aiTags');
  const proposed = cluster.status === 'proposed';
  const [editing, setEditing] = useState<boolean>(false);
  // The optimistic rename in useAiTags writes `cluster.label`; only the DB reload
  // re-maps it onto `topTag`. Prefer `label` so the renamed name shows immediately
  // instead of flickering back to the stale `topTag` until the silent reload lands.
  const clusterName = cluster.label || cluster.topTag || '';
  const [label, setLabel] = useState<string>(clusterName);
  useEffect(() => {
    setLabel(clusterName);
  }, [clusterName]);

  const submitRename = (): void => {
    const v = label.trim();
    setEditing(false);
    if (v && v !== clusterName) onRename(cluster.id, v);
    else setLabel(clusterName);
  };

  return (
    <div
      className={[
        'rounded-lg border p-2.5 transition-colors u-fade-in-up',
        proposed
          ? 'border-dashed border-[#3a3a5e] bg-[#13101f]'
          : 'border-[#2e2e2e] bg-[#111111] hover:border-[#3a3a5e]',
      ].join(' ')}
    >
      <div className="flex items-center justify-between mb-1.5 gap-2">
        {editing ? (
          <input
            autoFocus
            value={label}
            onChange={(e: React.ChangeEvent<HTMLInputElement>) => setLabel(e.target.value)}
            onBlur={submitRename}
            onKeyDown={(e: React.KeyboardEvent<HTMLInputElement>) => {
              if (e.key === 'Enter') submitRename();
              if (e.key === 'Escape') {
                setEditing(false);
                setLabel(clusterName);
              }
            }}
            className="flex-1 min-w-0 rounded bg-[#0f0f0f] border border-[#2e2e2e] px-1.5 py-0.5 text-sm text-gray-200 focus:border-[#7B5CFF] outline-none"
          />
        ) : (
          <button
            onClick={() => (proposed ? setEditing(true) : onSelect(cluster))}
            onKeyDown={proposed ? undefined : onKeyActivate(() => onSelect(cluster))}
            className="flex items-center gap-1 min-w-0 text-sm font-medium text-gray-200 text-left focus:outline-none"
            title={proposed ? t('clickToRename') : t('filterClusterPosts')}
          >
            <span className="truncate">{clusterName}</span>
            {proposed && <Pencil size={11} className="text-gray-600 shrink-0" />}
          </button>
        )}
        <span className="text-[10px] tabular-nums text-gray-500 shrink-0">
          {t('clusterPostCount', { n: cluster.postCount })}
        </span>
      </div>

      <div className="flex flex-wrap gap-1">
        {(cluster.tags || []).slice(0, 12).map((tag) =>
          proposed ? (
            <span
              key={tag}
              className="inline-flex items-center gap-1 rounded bg-[#1a1a1a] px-1.5 py-0.5 text-[10px] text-gray-400"
            >
              {tag}
              <button
                onClick={() => onRemoveTag(tag, cluster.id)}
                className="opacity-60 hover:opacity-100"
                title={t('removeFromCluster')}
              >
                <X size={10} />
              </button>
            </span>
          ) : (
            <span
              key={tag}
              className="rounded bg-[#1a1a1a] px-1.5 py-0.5 text-[10px] text-gray-400"
            >
              {tag}
            </span>
          ),
        )}
      </div>

      {proposed && (
        <div className="flex items-center gap-1.5 mt-2">
          <button
            onClick={() => onAccept(cluster.id)}
            disabled={busy}
            className="flex items-center gap-1 px-2 py-1 rounded text-[11px] text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] disabled:opacity-50 u-press"
          >
            <Check size={12} /> {t('accept')}
          </button>
          <button
            onClick={() => onDismiss(cluster.id)}
            disabled={busy}
            className="flex items-center gap-1 px-2 py-1 rounded text-[11px] text-gray-300 bg-[#1a1a1a] hover:bg-[#222] disabled:opacity-50 u-press"
          >
            <X size={12} /> {t('dismiss')}
          </button>
          <button
            onClick={() => onSelect(cluster)}
            className="ml-auto text-[10px] text-gray-500 hover:text-gray-300 u-press"
          >
            {t('preview')}
          </button>
        </div>
      )}
    </div>
  );
}

interface AliasCardProps {
  alias: Shelfy.TagAlias;
  busy: boolean;
  onAccept: (aliasNorm: string) => void;
  onDismiss: (aliasNorm: string) => void;
}

// A single tag alias (synonym) proposal in the left nav. Shows
// "aliasForm → canonicalForm" with the number of affected posts; proposed
// aliases get accept/dismiss controls. Accepting rewrites post tags, so the
// proposed state uses the dashed style shared with proposed clusters.
function AliasCard({ alias, busy, onAccept, onDismiss }: AliasCardProps) {
  const t = useT('aiTags');
  const proposed = alias.status === 'proposed';
  return (
    <div
      className={[
        'rounded-lg border p-2.5 transition-colors u-fade-in-up',
        proposed ? 'border-dashed border-[#3a3a5e] bg-[#13101f]' : 'border-[#2e2e2e] bg-[#111111]',
      ].join(' ')}
    >
      <div className="flex items-center justify-between gap-2 mb-0.5">
        <div className="flex items-center gap-1.5 min-w-0">
          <Chip label={alias.aliasForm} />
          <ArrowRight size={12} className="text-gray-600 shrink-0" />
          <Chip label={alias.canonicalForm} color={ACCENT} />
        </div>
        {typeof alias.count === 'number' && (
          <span className="text-[10px] tabular-nums text-gray-500 shrink-0">
            {t('aliasPostCount', { n: alias.count })}
          </span>
        )}
      </div>

      {proposed && (
        <div className="flex items-center gap-1.5 mt-2">
          <button
            onClick={() => onAccept(alias.aliasNorm)}
            disabled={busy}
            className="flex items-center gap-1 px-2 py-1 rounded text-[11px] text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] disabled:opacity-50 u-press"
          >
            <Check size={12} /> {t('accept')}
          </button>
          <button
            onClick={() => onDismiss(alias.aliasNorm)}
            disabled={busy}
            className="flex items-center gap-1 px-2 py-1 rounded text-[11px] text-gray-300 bg-[#1a1a1a] hover:bg-[#222] disabled:opacity-50 u-press"
          >
            <X size={12} /> {t('dismiss')}
          </button>
        </div>
      )}
    </div>
  );
}

interface HealthStatProps {
  label: string;
  value: number;
}

function HealthStat({ label, value }: HealthStatProps) {
  return (
    <div className="rounded-md border border-[#2e2e2e] bg-[#111111] px-2 py-1.5">
      <div className="text-base font-semibold text-gray-200 tabular-nums">
        {(value ?? 0).toLocaleString()}
      </div>
      <div className="text-[10px] text-gray-500">{label}</div>
    </div>
  );
}

interface ToastProps {
  children?: React.ReactNode;
  closing: boolean;
}

function Toast({ children, closing }: ToastProps) {
  return (
    <div
      data-testid="aitags-toast"
      className={[
        'fixed bottom-6 left-1/2 -translate-x-1/2 z-50 flex items-center gap-2 rounded-lg border border-[#2e2e2e] bg-[#1a1a1a] px-4 py-2.5 text-sm text-gray-200 shadow-2xl',
        closing ? 'u-fade-out' : 'u-fade-in-up',
      ].join(' ')}
    >
      <Check size={15} className="text-green-400" />
      {children}
    </div>
  );
}

function defaultCollectionName(
  tags: string[],
  entity: string | undefined,
  fallback: string,
): string {
  if (tags?.length) return tags.slice(0, 3).join(' + ');
  if (entity) return entity;
  return fallback;
}
