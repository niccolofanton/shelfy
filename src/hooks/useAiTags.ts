import { useState, useEffect, useCallback, useRef } from 'react';
import { useT } from '../i18n';
import type {
  AliasStatusResult,
  CancelledResult,
  ClusterStatusResult,
  MergeTagsResult,
  PostSearchResult,
  ProposeAliasesResult,
  QueuedResult,
  RemoveTagResult,
} from '../../types/electron-api';

// Progress callback shape passed to the long-running cluster/alias LLM jobs. The
// payload is opaque (regenerateClusters/proposeAliases run summaries are typed
// `unknown` on the bridge); callers forward it straight to their UI.
type ProgressCallback = (p: unknown) => void;

// An analyze-progress event (onAnalyzeProgress payload is `unknown`); only
// `status` is read here to decide whether the aggregates changed.
interface AnalyzeProgressJob {
  status?: string;
  [key: string]: unknown;
}

// Internal cluster shape: identical to Shelfy.TagCluster but with the status
// widened to the full review lifecycle so the optimistic dismiss can stamp a
// transient 'dismissed' (the persisted domain type only enumerates
// proposed/accepted). The reload reconciles it away; the hook still EXPOSES
// Shelfy.TagCluster[] to consumers (the two statuses they care about overlap).
type ClusterState = Omit<Shelfy.TagCluster, 'status'> & { status: Shelfy.ClusterStatus };

export interface UseAiTagsOpts {
  active?: boolean;
}

export interface UseAiTagsResult {
  overview: Shelfy.AiOverview | null;
  tagStats: Shelfy.Tag[];
  clusters: Shelfy.TagCluster[];
  entityStats: Shelfy.Entity[];
  health: Shelfy.TagHealth | null;
  mergeSuggestions: Shelfy.TagMergeSuggestion[];
  aliases: Shelfy.TagAlias[];
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
  renameTag: (from: string, to: string) => Promise<MergeTagsResult>;
  mergeTags: (sources: string[], target: string) => Promise<MergeTagsResult>;
  analyzeMissing: () => Promise<QueuedResult>;
  regenerateClusters: (onProgress?: ProgressCallback) => Promise<unknown>;
  cancelClusters: () => Promise<CancelledResult>;
  acceptCluster: (id: number) => Promise<ClusterStatusResult>;
  acceptAllClusters: () => Promise<{ accepted: number }>;
  dismissCluster: (id: number) => Promise<ClusterStatusResult>;
  renameCluster: (id: number, label: string) => Promise<ClusterStatusResult>;
  removeTagFromCluster: (tag: string, clusterId: number) => Promise<RemoveTagResult>;
  proposeAliases: (onProgress?: ProgressCallback) => Promise<ProposeAliasesResult>;
  cancelAliasProposals: () => Promise<CancelledResult>;
  acceptAlias: (aliasNorm: string) => Promise<AliasStatusResult>;
  dismissAlias: (aliasNorm: string) => Promise<AliasStatusResult>;
  acceptAllAliases: () => Promise<{ accepted: number }>;
  getPostIdsByTags: (tags: string[], mode?: 'and' | 'or') => Promise<string[]>;
  getTagCooccurrence: (tag: string, limit?: number) => Promise<Shelfy.TagCount[]>;
  fetchPosts: (filters?: unknown) => Promise<PostSearchResult>;
}

/**
 * Loads and manages all AI-tagging insight data for the AI Tags view.
 *
 * On mount (and on refresh()) it pulls overview, tag stats, clusters, entity
 * stats and tag health in parallel. It also exposes the mutating actions
 * (renameTag, mergeTags, analyzeMissing) and read helpers (getPostIdsByTags,
 * getTagCooccurrence, fetchPosts) used by the view.
 *
 * @param opts — when `active` is false the live analyze-progress refresh is
 *   suppressed. The AiTags view stays mounted for the whole session (App keeps
 *   every visited view alive), so without this gate its 7-query aggregate suite
 *   would re-run roughly once per 800ms FOR THE ENTIRE SESSION during any large
 *   auto-tag run, even while the user browses another view — competing with the
 *   analysis jobs for SQLite/the main thread.
 */
export function useAiTags({ active = true }: UseAiTagsOpts = {}): UseAiTagsResult {
  const [overview, setOverview] = useState<Shelfy.AiOverview | null>(null);
  const [tagStats, setTagStats] = useState<Shelfy.Tag[]>([]);
  const [clusters, setClusters] = useState<ClusterState[]>([]);
  const [entityStats, setEntityStats] = useState<Shelfy.Entity[]>([]);
  const [health, setHealth] = useState<Shelfy.TagHealth | null>(null);
  const [mergeSuggestions, setMergeSuggestions] = useState<Shelfy.TagMergeSuggestion[]>([]);
  const [aliases, setAliases] = useState<Shelfy.TagAlias[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const t = useT('aiTags');

  // Guards against state updates after unmount during in-flight loads.
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  // Mirror `active` into a ref so the long-lived analyze-progress subscription
  // can read the latest visibility without resubscribing on every toggle.
  const activeRef = useRef(active);
  activeRef.current = active;

  // `silent` reloads update the underlying data in place WITHOUT toggling the
  // full-page loading flag. Mutations (accept/dismiss/rename cluster, merge…)
  // and the live analyze-progress refresh use this so the whole view doesn't
  // unmount into the spinner — which would also wipe the user's active filter.
  const load = useCallback(
    async ({ silent = false }: { silent?: boolean } = {}): Promise<void> => {
      if (!silent) setLoading(true);
      setError(null);
      try {
        const api = window.electronAPI;
        const [ov, ts, cl, es, hl, ms, al] = await Promise.all([
          api.getAiOverview(),
          api.getTagStats({ limit: 200 }),
          api.getTagClusters({ maxClusters: 24 }),
          api.getEntityStats({ limit: 60 }),
          api.getTagHealth(),
          api.getTagMergeSuggestions({ limit: 30 }),
          // Tag aliases are LLM proposals awaiting accept/dismiss review; missing
          // API or errors degrade gracefully to an empty list.
          api.getTagAliases?.({ status: 'proposed' }).catch(() => [] as Shelfy.TagAlias[]) ?? [],
        ]);
        if (!mountedRef.current) return;
        setOverview(ov || null);
        setTagStats(Array.isArray(ts) ? ts : []);
        setClusters(Array.isArray(cl) ? cl : []);
        setEntityStats(Array.isArray(es) ? es : []);
        setHealth(hl || null);
        setMergeSuggestions(Array.isArray(ms) ? ms : []);
        setAliases(Array.isArray(al) ? al : []);
      } catch (err) {
        if (!mountedRef.current) return;
        console.error('[useAiTags] load error:', err);
        setError(err instanceof Error ? err.message : t('loadError'));
      } finally {
        if (mountedRef.current && !silent) setLoading(false);
      }
    },
    [t],
  );

  useEffect(() => {
    load();
  }, [load]);

  // The view stays mounted across the session (App keep-alive). When it becomes
  // visible again, do a silent reload to pick up anything that changed while live
  // refresh was suppressed (the initial mount's load() already ran, so skip it).
  const didInitialActiveRef = useRef(false);
  useEffect(() => {
    if (!active) return;
    if (!didInitialActiveRef.current) {
      didInitialActiveRef.current = true;
      return;
    }
    load({ silent: true });
  }, [active, load]);

  // ── Real-time refresh while analysis runs ──────────────────────────────────
  // Subscribe to analyze-progress events and reload the aggregate data with a
  // debounce so overview/tagStats/clusters update live without thrashing.
  const reloadTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    const onProgress = (raw: unknown): void => {
      // Skip the heavy aggregate suite while the view isn't visible — see the
      // hook's `active` doc above. The view does a full load() when it remounts/
      // becomes visible, so nothing is lost by deferring.
      if (!activeRef.current) return;
      const job = (raw && typeof raw === 'object' ? raw : null) as AnalyzeProgressJob | null;
      // Only react to states that change the underlying data.
      if (!job || (job.status !== 'done' && job.status !== 'completed')) return;
      if (reloadTimer.current) clearTimeout(reloadTimer.current);
      reloadTimer.current = setTimeout(() => {
        if (mountedRef.current) load({ silent: true });
      }, 800);
    };
    const unsubscribe = window.electronAPI?.onAnalyzeProgress?.(onProgress);
    return () => {
      if (reloadTimer.current) clearTimeout(reloadTimer.current);
      if (typeof unsubscribe === 'function') unsubscribe();
    };
  }, [load]);

  const refresh = useCallback((): Promise<void> => load(), [load]);

  // ── Mutations — each refreshes the aggregate data afterwards ───────────────
  // Optimistic: reflect the rename in the visible cluster chips immediately, fire
  // the IPC, then reconcile the exact aggregates (counts, related, health) with a
  // background reload. Re-await only on error to restore the truth.
  const renameTag = useCallback(
    async (from: string, to: string): Promise<MergeTagsResult> => {
      setClusters((prev) =>
        prev.map((c) => ({
          ...c,
          tags: (c.tags || []).map((tag) => (tag === from ? to : tag)),
        })),
      );
      try {
        const res = await window.electronAPI.renameTag(from, to);
        load({ silent: true });
        return res;
      } catch (err) {
        await load({ silent: true });
        throw err;
      }
    },
    [load],
  );

  const mergeTags = useCallback(
    async (sources: string[], target: string): Promise<MergeTagsResult> => {
      const srcSet = new Set(sources);
      setClusters((prev) =>
        prev.map((c) => ({
          ...c,
          tags: Array.from(new Set((c.tags || []).map((tag) => (srcSet.has(tag) ? target : tag)))),
        })),
      );
      try {
        const res = await window.electronAPI.mergeTags(sources, target);
        load({ silent: true });
        return res;
      } catch (err) {
        await load({ silent: true });
        throw err;
      }
    },
    [load],
  );

  const analyzeMissing = useCallback(async (): Promise<QueuedResult> => {
    const res = await window.electronAPI.analyzeMissing();
    // Don't refresh immediately — analysis is queued and runs async.
    return res;
  }, []);

  // ── Cluster review actions ──────────────────────────────────────────────────
  // Regeneration is an explicit, long-running LLM job: subscribe to progress for
  // the duration of the call, then reload the (now persisted) clusters.
  const regenerateClusters = useCallback(
    async (onProgress?: ProgressCallback): Promise<unknown> => {
      const unsubscribe = window.electronAPI.onClusterProgress?.((p) => onProgress?.(p));
      try {
        const res = await window.electronAPI.regenerateClusters();
        await load({ silent: true });
        return res;
      } finally {
        if (typeof unsubscribe === 'function') unsubscribe();
      }
    },
    [load],
  );

  // Cluster review actions are optimistic: the card updates instantly, the IPC
  // write fires, and the silent reload runs in the background to reconcile
  // (re-awaited only on error, to restore the truth).
  const acceptCluster = useCallback(
    async (id: number): Promise<ClusterStatusResult> => {
      setClusters((prev) => prev.map((c) => (c.id === id ? { ...c, status: 'accepted' } : c)));
      try {
        const res = await window.electronAPI.acceptCluster(id);
        load({ silent: true });
        return res;
      } catch (err) {
        await load({ silent: true });
        throw err;
      }
    },
    [load],
  );

  // Accept every proposed cluster in one pass, optimistically, then reconcile.
  const acceptAllClusters = useCallback(async (): Promise<{ accepted: number }> => {
    const proposed = clusters.filter((c) => c.status === 'proposed');
    setClusters((prev) =>
      prev.map((c) => (c.status === 'proposed' ? { ...c, status: 'accepted' } : c)),
    );
    try {
      await Promise.all(proposed.map((c) => window.electronAPI.acceptCluster(c.id)));
      load({ silent: true });
    } catch (err) {
      await load({ silent: true });
      throw err;
    }
    return { accepted: proposed.length };
  }, [clusters, load]);

  const dismissCluster = useCallback(
    async (id: number): Promise<ClusterStatusResult> => {
      setClusters((prev) => prev.map((c) => (c.id === id ? { ...c, status: 'dismissed' } : c)));
      try {
        const res = await window.electronAPI.dismissCluster(id);
        load({ silent: true });
        return res;
      } catch (err) {
        await load({ silent: true });
        throw err;
      }
    },
    [load],
  );

  const renameCluster = useCallback(
    async (id: number, label: string): Promise<ClusterStatusResult> => {
      setClusters((prev) => prev.map((c) => (c.id === id ? { ...c, label } : c)));
      try {
        const res = await window.electronAPI.renameCluster(id, label);
        load({ silent: true });
        return res;
      } catch (err) {
        await load({ silent: true });
        throw err;
      }
    },
    [load],
  );

  const removeTagFromCluster = useCallback(
    async (tag: string, clusterId: number): Promise<RemoveTagResult> => {
      setClusters((prev) =>
        prev.map((c) =>
          c.id === clusterId ? { ...c, tags: (c.tags || []).filter((tg) => tg !== tag) } : c,
        ),
      );
      try {
        const res = await window.electronAPI.removeTagFromCluster(tag, clusterId);
        load({ silent: true });
        return res;
      } catch (err) {
        await load({ silent: true });
        throw err;
      }
    },
    [load],
  );

  const cancelClusters = useCallback(
    (): Promise<CancelledResult> => window.electronAPI.cancelClusters(),
    [],
  );

  // ── Alias review actions ────────────────────────────────────────────────────
  // Reload just the proposed aliases (used after a propose run or to reconcile
  // an optimistic mutation). Failures degrade to an empty list.
  const reloadAliases = useCallback(async (): Promise<void> => {
    try {
      const al = await window.electronAPI.getTagAliases?.({ status: 'proposed' });
      if (mountedRef.current) setAliases(Array.isArray(al) ? al : []);
    } catch {
      if (mountedRef.current) setAliases([]);
    }
  }, []);

  // Proposing aliases is an explicit, long-running LLM job: subscribe to progress
  // for the duration of the call, then reload the (now persisted) proposals.
  const proposeAliases = useCallback(
    async (onProgress?: ProgressCallback): Promise<ProposeAliasesResult> => {
      const unsubscribe = window.electronAPI.onAliasProgress?.((p) => onProgress?.(p));
      try {
        const res = await window.electronAPI.proposeAliases();
        await reloadAliases();
        return res;
      } finally {
        if (typeof unsubscribe === 'function') unsubscribe();
      }
    },
    [reloadAliases],
  );

  const cancelAliasProposals = useCallback(
    (): Promise<CancelledResult> => window.electronAPI.cancelAliases(),
    [],
  );

  // Alias review actions are optimistic: the row vanishes instantly, the IPC
  // write fires, and a silent reload reconciles only on error (to restore truth).
  // Accepting/dismissing rewrites post_tags and invalidates the global caches
  // (re-canonicalizing rows onto the root tag), so on success we also fire the
  // background aggregate reload — like the cluster mutations — to refresh
  // overview/tagStats/clusters/health, not just the proposed-alias list.
  const acceptAlias = useCallback(
    async (aliasNorm: string): Promise<AliasStatusResult> => {
      setAliases((prev) => prev.filter((a) => a.aliasNorm !== aliasNorm));
      try {
        const res = await window.electronAPI.acceptAlias(aliasNorm);
        load({ silent: true });
        return res;
      } catch (err) {
        await reloadAliases();
        throw err;
      }
    },
    [load, reloadAliases],
  );

  const dismissAlias = useCallback(
    async (aliasNorm: string): Promise<AliasStatusResult> => {
      setAliases((prev) => prev.filter((a) => a.aliasNorm !== aliasNorm));
      try {
        const res = await window.electronAPI.dismissAlias(aliasNorm);
        load({ silent: true });
        return res;
      } catch (err) {
        await reloadAliases();
        throw err;
      }
    },
    [load, reloadAliases],
  );

  // Accept every proposed alias in one pass, optimistically, then reconcile.
  const acceptAllAliases = useCallback(async (): Promise<{ accepted: number }> => {
    const proposed = aliases.filter((a) => a.status === 'proposed');
    setAliases((prev) => prev.filter((a) => a.status !== 'proposed'));
    try {
      await Promise.all(proposed.map((a) => window.electronAPI.acceptAlias(a.aliasNorm)));
      load({ silent: true });
    } catch (err) {
      // Reload the full aggregates (not just the alias list): a partial Promise.all
      // failure can still have applied some aliases, so overview/tagStats/clusters/
      // health would otherwise be left stale. load() also refetches the aliases.
      await load({ silent: true });
      throw err;
    }
    return { accepted: proposed.length };
  }, [aliases, load]);

  // ── Read helpers ───────────────────────────────────────────────────────────
  const getPostIdsByTags = useCallback(
    (tags: string[], mode: 'and' | 'or' = 'or'): Promise<string[]> =>
      window.electronAPI.getPostIdsByTags(tags, mode),
    [],
  );

  const getTagCooccurrence = useCallback(
    (tag: string, limit = 12): Promise<Shelfy.TagCount[]> =>
      window.electronAPI.getTagCooccurrence(tag, limit),
    [],
  );

  const fetchPosts = useCallback(
    (filters?: unknown): Promise<PostSearchResult> => window.electronAPI.getPosts(filters),
    [],
  );

  return {
    overview,
    tagStats,
    // Expose the persisted domain shape; the transient 'dismissed' status only
    // lives internally between an optimistic dismiss and the reconciling reload.
    clusters: clusters as Shelfy.TagCluster[],
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
  };
}
