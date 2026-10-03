import React, { useState, useEffect, useRef, useMemo, useCallback, Suspense, lazy } from 'react';
import { Construction, FileQuestion, Loader2 } from 'lucide-react';
import Sidebar from './components/Sidebar';
import BottomNav, { type BottomNavTarget } from './components/BottomNav';
import WindowControls from './components/WindowControls';
import Gallery from './views/Gallery';
import Browser from './views/Browser';
import { useAiSetupStatus } from './hooks/useAiSetup';
import PostModal from './components/PostModal';
import ErrorBoundary, { ErrorPanel } from './components/ErrorBoundary';
import { ConsentGate } from './components/DisclaimerGate';
import { useCollections } from './hooks/useCollections';
import { useDownloads } from './hooks/useDownloads';
import { useWebJobs } from './hooks/useWebJobs';
import { AnalysisProvider, useAnalysis, analysisSummary } from './hooks/useAnalysis';
import { ActivityProvider } from './hooks/useActivity';
import type { SourceSyncApi, SyncTarget } from './hooks/useSourceSync';
import { useT, useLang, localeTag, withMessages } from './i18n';
import { useShelfy } from './api/ShelfyProvider';
import {
  DEFAULT_SETTINGS_SECTION,
  useNavigation,
  type AppRoute,
  type CurrentRoute,
} from './api/navigation';
import { buildTime } from 'virtual:build-time';

// AI, Websites, Settings, Downloads, Jobs (plan §2.19 "code-split AI,
// Websites, Settings and Admin"; Admin has no view of its own — out of
// scope, E4): on the web these are capability-gated off until P3/P1-20 (see
// `caps.ai` / `caps.websites` / `caps.settings` below), but a static import
// still ships their JS in the initial bundle for every visitor.
// `views/websites/model.ts` alone (the biggest of these view trees) is
// ~25 KB uncompressed, and AiWebsites also pulls in the rest of
// `views/websites/**`. React.lazy defers all of it to its own chunk, fetched
// only the first time `mountedViews` (below) adds that view — which
// capability-gated navigation on the web never does.
//
// F14: withMessages() (src/i18n) wraps each factory so the view's own i18n
// namespace(s) load in the same Promise as its code — Suspense below waits
// on both, so the view never flashes an untranslated "ns.key" on first
// render. A view that cites a namespace only through another component it
// renders (e.g. AiTagsQueue quoting 'aiTags' strings) lists that namespace
// here too. This is the pattern later lanes follow for a new lazy view.
const SettingsView = lazy(
  withMessages(() => import('./views/Settings'), 'settings', 'language', 'importModal'),
);
const AiTags = lazy(withMessages(() => import('./views/AiTags'), 'aiTags'));
const AiTagsQueue = lazy(withMessages(() => import('./views/AiTagsQueue'), 'aiTags', 'aiQueue'));
const AiWebsites = lazy(
  // 'lightbox': views/websites/SiteDetail.tsx and detail/SectionsTab.tsx
  // render the same ImageLightbox PostModal.tsx uses, statically — not
  // through PostModal's own local lazy wrapper, so it needs listing here too.
  withMessages(() => import('./views/AiWebsites'), 'aiWebsites', 'webVocab', 'lightbox'),
);
const AiSearch = lazy(
  withMessages(() => import('./views/AiSearch'), 'aiSearch', 'dictation', 'chip'),
);
const AiOnboarding = lazy(withMessages(() => import('./views/AiOnboarding'), 'aiOnboarding'));
const DownloadsView = lazy(withMessages(() => import('./views/Downloads'), 'downloads'));
// P4-09: the Jobs view is web-only (PG18), so it is always lazy — there is no
// "ships in the initial bundle" baseline to improve on, unlike Downloads.
const JobsView = lazy(withMessages(() => import('./views/Jobs'), 'jobs'));

// Modals and a banner that aren't on the first screen — opened from a toolbar
// action, or (RemoteAiBanner) shown only once `caps.ai` arrives — so each is
// its own chunk too, following the same withMessages() pattern.
const AddSiteModal = lazy(withMessages(() => import('./components/AddSiteModal'), 'addSite'));
const AddBookmarkModal = lazy(
  withMessages(() => import('./components/AddBookmarkModal'), 'addBookmark'),
);
const CollectionModal = lazy(
  withMessages(() => import('./components/CollectionModal'), 'collectionModal'),
);
const RemoteAiBanner = lazy(withMessages(() => import('./components/RemoteAiBanner'), 'remoteAi'));

// Suspense fallback for the lazy views above: a view only ever takes a beat
// to fetch its chunk the first time it's visited (or never, on the web while
// its capability is off), so a small centered spinner is enough — it never
// has layout/content to mirror the way PostGridSkeleton does for the gallery.
function ViewLoading(): React.JSX.Element {
  return (
    <div className="flex items-center justify-center w-full h-full">
      <Loader2 size={20} className="text-[#555] animate-spin" strokeWidth={1.5} />
    </div>
  );
}

// The non-browser view identifiers, in render order. `trash` exists only as
// a web route so far; `jobs` (P4-09) is web-only for good (PG18) — the
// desktop keeps Downloads, gated by the opposite capability.
type ViewId =
  | 'gallery'
  | 'downloads'
  | 'jobs'
  | 'aitags'
  | 'aiqueue'
  | 'aiweb'
  | 'aisearch'
  | 'settings'
  | 'trash';
// All navigable views, including the always-mounted browser.
type View = ViewId | 'browser';
// The three browser-backed platforms that own a sidebar badge.
type BrowserPlatform = 'instagram' | 'twitter' | 'pinterest';

// The selected library source feeding the gallery filter.
interface ActiveSource {
  type: 'platform' | 'collection';
  value: string | number;
}

// Open-state of the collection create/edit modal (null when closed).
interface CollectionModalState {
  initial?: Shelfy.Collection;
}

// Tag handoff from the global AI modal into the Tags Explorer.
interface AiTagsInitial {
  tag: string | null;
  nonce: number;
}

// The initial (pre-fetch) stats shape: only the counters the sidebar reads
// before getStats() lands the full Shelfy.Stats payload.
interface InitialStats {
  total: number;
  byPlatform: Record<BrowserPlatform, number>;
}

// Per-platform live source-sync job reported up by the Browser. Mirrors the
// shape produced by useSourceSync (status machine + counters).
interface SourceSyncJob {
  platform: BrowserPlatform;
  status: 'navigating' | 'syncing' | 'done' | 'stopped' | 'error';
  error: string | null;
  stepIndex: number;
  stepCount: number;
  currentLabel: string | null;
  currentCollectionId: number | null;
  scanned: number;
  fresh: number;
  skipped: string[];
  startedAt: number;
  finishedAt: number | null;
}

// Batch-save summary emitted by the Browser when an import finishes.
interface BrowserSaved {
  count: number;
  platform: BrowserPlatform;
}

// Just-finished save record surfaced to the Activity center.
interface LastSave {
  count: number;
  platform: BrowserPlatform;
  ts: number;
}

// Navigation options carried alongside a target view (e.g. which sub-tab).
interface NavigateOpts {
  platform?: BrowserPlatform;
}

// Item payload accompanying an Activity-center queue action.
interface ActivityActionItem {
  id?: string;
  platform?: BrowserPlatform;
}

// Collection create/save payload from the modal.
interface CollectionDraft {
  name: string;
  color: string;
}

// Patch applied by the global AI modal (currently carries a tag chip).
interface AiFilterPatch {
  tag?: string | null;
}

// Non-browser views, in render order. Each is kept alive once visited (see the
// keep-alive overlay in AppInner) so switching back is instant.
const VIEW_IDS: ViewId[] = [
  'gallery',
  'downloads',
  'jobs',
  'aitags',
  'aiqueue',
  'aiweb',
  'aisearch',
  'settings',
  'trash',
];

// ── Routes (web) ──────────────────────────────────────────────────────────────
// On the web the address bar decides the view, the folder and the open post
// (src/api/navigation.tsx); these map routes to App's state and back. The
// desktop has no addresses and never calls them.

const ALL_SOURCE: ActiveSource = { type: 'platform', value: 'all' };

// The view a route shows. An address with nothing behind it keeps the gallery
// mounted under its panel.
function viewOfRoute(route: CurrentRoute): ViewId {
  if (route.name === 'trash') return 'trash';
  if (route.name === 'jobs') return 'jobs';
  if (route.name === 'settings') return 'settings';
  return 'gallery';
}

// The library source of a route: `/c/:id` names a folder, `/` the whole
// library (where a platform picked in the sidebar stays), and the other routes
// leave the library behind them as it is.
function sourceOfRoute(route: CurrentRoute, current: ActiveSource): ActiveSource {
  if (route.name === 'collection') return { type: 'collection', value: route.collectionId };
  if (route.name === 'library' && current.type === 'collection') return ALL_SOURCE;
  return current;
}

function sameSource(a: ActiveSource, b: ActiveSource): boolean {
  return a.type === b.type && a.value === b.value;
}

// A folder has an address; the platforms live on the library's.
function routeOfSource(source: ActiveSource): AppRoute {
  return source.type === 'collection'
    ? { name: 'collection', collectionId: Number(source.value) }
    : { name: 'library' };
}

// The route that shows a view, if the web has it.
function routeOfView(view: View, source: ActiveSource): AppRoute | null {
  if (view === 'gallery') return routeOfSource(source);
  if (view === 'trash') return { name: 'trash' };
  if (view === 'jobs') return { name: 'jobs', kind: [], state: [] };
  if (view === 'settings') return { name: 'settings', section: DEFAULT_SETTINGS_SECTION };
  return null;
}

// The AI tabs gated by the first-run onboarding: until the local pipeline is
// fully configured they show the setup wizard instead of their own content.
const AI_VIEW_IDS: ViewId[] = ['aitags', 'aiqueue', 'aiweb', 'aisearch'];

// BottomNav (P1-02, under 900px): which tab (if any) highlights for the
// current view. Mirrors the sidebar's own grouping — the four AI sub-tabs all
// light up the single "AI" tab, since the bar has no room for sub-tabs.
function bottomNavTargetOfView(view: View): BottomNavTarget | null {
  if (view === 'gallery') return 'library';
  if (view === 'settings') return 'settings';
  if (view === 'jobs') return 'jobs';
  if (AI_VIEW_IDS.includes(view as ViewId)) return 'ai';
  return null;
}

// Memo wrappers for the always-/keep-alive-mounted children: App re-renders on
// every coalesced progress flush, so each view must only reconcile when its own
// (stabilized) props change. Sidebar and Downloads are also memoized at definition.
const GalleryMemo = React.memo(Gallery);
const BrowserMemo = React.memo(Browser);
const AiTagsMemo = React.memo(AiTags);
const AiTagsQueueMemo = React.memo(AiTagsQueue);
const AiWebsitesMemo = React.memo(AiWebsites);
const AiSearchMemo = React.memo(AiSearch);
const SettingsMemo = React.memo(SettingsView);
const DownloadsMemo = React.memo(DownloadsView);
const JobsMemo = React.memo(JobsView);

// Web-capture statuses counted as "active" for the sidebar badge.
const WEB_ACTIVE_STATUS: string[] = [
  'pending',
  'discovering',
  'capturing',
  'extracting',
  'analyzing',
];

export default function App(): React.JSX.Element {
  // The last resort for the shell itself (sidebar, modals); each view has its own.
  return (
    <ErrorBoundary view="app" layout="page">
      <AnalysisProvider>
        <AppInner />
      </AnalysisProvider>
    </ErrorBoundary>
  );
}

function AppInner(): React.JSX.Element {
  const t = useT('app');
  const te = useT('errors');
  const { lang } = useLang();
  // The backend seam: what this client can do decides which surfaces exist
  // (the web app has no browser, downloads, AI… yet; see ShelfyCapabilities).
  const client = useShelfy();
  const caps = client.capabilities;
  // The address bar, on the web: its route decides the view, the folder and
  // the open post, and moving around changes the address. The desktop has
  // none (`route` is null) and keeps its view state.
  const nav = useNavigation();
  const route = nav?.route ?? null;
  const navigate = nav?.navigate;
  const [stateView, setStateView] = useState<View>('gallery');
  const view: View = route ? viewOfRoute(route) : stateView;
  // Views that have been opened at least once — kept mounted thereafter so
  // returning to them is instant (no re-fetch / skeleton). Seeded with the
  // initial view; 'browser' is always mounted separately and never listed here.
  const [mountedViews, setMountedViews] = useState<Set<ViewId>>(
    () => new Set<ViewId>([route ? viewOfRoute(route) : 'gallery']),
  );
  const [browserTab, setBrowserTab] = useState<BrowserPlatform>('instagram');
  const [collectionModal, setCollectionModal] = useState<CollectionModalState | null>(null); // null | { initial?: Collection }
  const [stats, setStats] = useState<Shelfy.Stats | InitialStats>({
    total: 0,
    byPlatform: { instagram: 0, twitter: 0, pinterest: 0 },
  });
  // Posts intercepted in the current session, per source — drives the per-sub-tab badge.
  const [newPostsAlert, setNewPostsAlert] = useState<Record<BrowserPlatform, number>>({
    instagram: 0,
    twitter: 0,
    pinterest: 0,
  });
  // Live sync state per source, reported up by Browser so the sidebar can show an indicator.
  const [browserSyncing, setBrowserSyncing] = useState<Record<BrowserPlatform, boolean>>({
    instagram: false,
    twitter: false,
    pinterest: false,
  });
  const [activeSource, setActiveSource] = useState<ActiveSource>(() =>
    route ? sourceOfRoute(route, ALL_SOURCE) : ALL_SOURCE,
  );
  const activeSourceRef = useRef<ActiveSource>(activeSource);
  activeSourceRef.current = activeSource;
  const [gallerySourceNonce, setGallerySourceNonce] = useState<number>(0); // bumped on each Sources click

  // Web: when the address changes under the app (the back button, a link),
  // take the folder it names and have the gallery apply it.
  useEffect(() => {
    if (!route) return;
    const next = sourceOfRoute(route, activeSourceRef.current);
    if (sameSource(next, activeSourceRef.current)) return;
    setActiveSource(next);
    setGallerySourceNonce((n) => n + 1);
  }, [route]);

  // Shows a view: on the web through its address (a view without one does
  // not exist there), on the desktop by state.
  const setView = useCallback(
    (next: View): void => {
      if (!navigate) {
        setStateView(next);
        return;
      }
      const to = routeOfView(next, activeSourceRef.current);
      if (to) navigate(to);
    },
    [navigate],
  );

  // BottomNav (narrow only): maps its fixed tab set onto the existing views.
  // "Search" has no view/route of its own yet (P1-04 carry-over) — the search
  // field already lives in the gallery's own toolbar, so it opens the library.
  const handleBottomNav = useCallback(
    (target: BottomNavTarget): void => {
      if (target === 'search' || target === 'library') setView('gallery');
      else if (target === 'ai') setView('aiqueue');
      else if (target === 'jobs') setView('jobs');
      else setView('settings');
    },
    [setView],
  );
  const [devBarVisible, setDevBarVisible] = useState<boolean>(false);
  const devBarMounted = useRef<boolean>(false);

  // Refs mirror the current view/sub-tab for the (mount-once) onNewPosts listener,
  // so it can avoid flagging the source the user is actively looking at.
  const viewRef = useRef<View>(view);
  const browserTabRef = useRef<BrowserPlatform>(browserTab);
  const browserSyncingRef = useRef<Record<BrowserPlatform, boolean>>(browserSyncing);
  viewRef.current = view;
  browserTabRef.current = browserTab;
  browserSyncingRef.current = browserSyncing;

  // Register each non-browser view the first time it becomes active, so the
  // keep-alive overlay mounts it once and then just toggles its visibility.
  useEffect(() => {
    if (view === 'browser') return;
    setMountedViews((prev) => (prev.has(view) ? prev : new Set(prev).add(view)));
  }, [view]);

  // ── First-run AI onboarding gate ────────────────────────────────────────────
  // Latched (not live-derived): it turns on when the pipeline is found incomplete
  // and stays on until the wizard itself dismisses it — so the success screen
  // isn't yanked away the instant the last download lands. "Salta per ora" mutes
  // the gate for this session only; it returns at next launch until complete.
  const aiSetup = useAiSetupStatus();
  const [aiGate, setAiGate] = useState<boolean>(false);
  const aiGateSkipped = useRef<boolean>(false);
  useEffect(() => {
    // A working remote AI node replaces the local setup: never show the wizard.
    if (!caps.ai || aiSetup.status?.remoteReady) setAiGate(false);
    else if (!aiGateSkipped.current && aiSetup.status && !aiSetup.complete) setAiGate(true);
  }, [caps.ai, aiSetup.status, aiSetup.complete]);
  const dismissAiGate = (skip: boolean): void => {
    if (skip) aiGateSkipped.current = true;
    setAiGate(false);
  };

  const clearAlert = useCallback(
    (platform: BrowserPlatform) =>
      setNewPostsAlert((prev) => (prev[platform] ? { ...prev, [platform]: 0 } : prev)),
    [],
  );

  // ── Source-sync (sync in background dalle righe della Libreria) ──────────────
  // Il Browser (sempre montato) registra qui la sua API imperativa; i job per
  // piattaforma risalgono via onSourceSyncJobs e alimentano sidebar + Attività.
  const sourceSyncApiRef = useRef<SourceSyncApi | null>(null);
  const registerSourceSyncApi = useCallback((api: SourceSyncApi | null) => {
    sourceSyncApiRef.current = api;
  }, []);
  const [sourceSyncJobs, setSourceSyncJobs] = useState<Record<string, SourceSyncJob>>({});
  const sourceSyncJobsRef = useRef<Record<string, SourceSyncJob>>(sourceSyncJobs);
  sourceSyncJobsRef.current = sourceSyncJobs;
  // Un solo entry-point per i bottoni della sidebar: se quella piattaforma sta
  // già sincronizzando il click diventa uno stop; un errore residuo (es. login)
  // viene sgomberato prima di ripartire.
  const handleSyncSource = useCallback((target: SyncTarget | null | undefined) => {
    const api = sourceSyncApiRef.current;
    if (!api || !target?.platform) return;
    const job = sourceSyncJobsRef.current[target.platform];
    if (job && (job.status === 'navigating' || job.status === 'syncing')) {
      api.stop(target.platform);
      return;
    }
    if (job) api.dismiss(target.platform);
    api.start(target);
  }, []);

  const {
    collections,
    reload: reloadCollections,
    create: createCollection,
    remove: removeCollection,
    rename: renameCollection,
  } = useCollections();

  // Local VLM analysis activity — surfaced in the sidebar like the browser-tab sync.
  const {
    jobs: analysisJobs,
    modelStatus,
    modelProgress,
    concurrency: analysisConcurrency,
    isPaused: analysisPaused,
    pauseAll: pauseAnalyze,
    resumeAll: resumeAnalyze,
    cancelAll: cancelAllAnalyze,
  } = useAnalysis();
  // Sidebar badge routing: analyzer jobs for web references (platform === 'web')
  // belong to the "Websites" tab — the AI Tags queue view already filters them out
  // the same way — so the aiqueue badge counts only social jobs, while the web
  // analyzer jobs feed the aiweb badge below, alongside the capture jobs.
  const socialAnalysisJobs = useMemo(
    () => analysisJobs.filter((j) => j.platform !== 'web'),
    [analysisJobs],
  );
  const webAnalysisJobs = useMemo(
    () => analysisJobs.filter((j) => j.platform === 'web'),
    [analysisJobs],
  );
  const analysis = useMemo(
    () => analysisSummary(socialAnalysisJobs, analysisConcurrency),
    [socialAnalysisJobs, analysisConcurrency],
  );
  const webAnalysis = useMemo(
    () => analysisSummary(webAnalysisJobs, analysisConcurrency),
    [webAnalysisJobs, analysisConcurrency],
  );

  // True while the Browser is importing a hand-picked selection of posts; lastSave
  // carries the just-finished batch ({count, platform, ts}) so the Activity center
  // can log "N post salvati".
  const [saving, setSaving] = useState<boolean>(false);
  const [lastSave, setLastSave] = useState<LastSave | null>(null);
  // Only the at-a-glance counts feed the "AI Tags" nav badge; the rich live
  // detail is rendered by <ActivityCenter> straight from the activity context.
  const { active: analysisActive, done: analysisDone, total: analysisTotal } = analysis;

  // Post detail modal opened from the AI activity strip / AI Tags queue. Lives at
  // App level so a job row anywhere can pop it; we fetch the full post by id since
  // analysis jobs only carry the id + a thumbnail.
  const [aiModalPost, setAiModalPost] = useState<Shelfy.Post | null>(null);
  const [showAddSite, setShowAddSite] = useState<boolean>(false);
  const [showAddBookmark, setShowAddBookmark] = useState<boolean>(false);
  // Carries a tag from a clicked chip in the global AI modal over to the Tags
  // Explorer; the bumped nonce lets that view re-apply it even for the same tag.
  const [aiTagsInitial, setAiTagsInitial] = useState<AiTagsInitial>({ tag: null, nonce: 0 });
  const openAiPost = useCallback(
    async (postId: string): Promise<void> => {
      if (!postId) return;
      try {
        const [post] = await client.getPostsByIds([postId]);
        if (post) setAiModalPost(post);
      } catch (e) {
        console.error('[App] openAiPost failed:', e);
      }
    },
    [client],
  );
  const openAddSite = useCallback(() => setShowAddSite(true), []);
  const openAddBookmark = useCallback(() => setShowAddBookmark(true), []);
  // The Jobs view's post thumbnail (P4-09): a real link to `/p/:key`, not the
  // local-state modal of openAiPost — the Jobs view is web-only, so `navigate`
  // always exists by the time a user can reach it.
  const openJobPost = useCallback(
    (key: string): void => navigate?.({ name: 'post', key }),
    [navigate],
  );

  // Download queue — lifted here (App is always mounted) so the sidebar can show
  // live progress regardless of the current view, and the Downloads view shares
  // the same single instance via props.
  const downloads = useDownloads();
  const webJobs = useWebJobs();
  // Minimal memoized projections (one pass per queue) so the sidebar badges only
  // change identity when a count actually moves, not on every progress flush.
  const { downloadTotal, downloadDone, downloadActive } = useMemo(() => {
    let done = 0;
    let active = false;
    for (const j of downloads.jobs) {
      if (j.status === 'done') done++;
      else if (j.status === 'downloading' || j.status === 'pending') active = true;
    }
    return { downloadTotal: downloads.jobs.length, downloadDone: done, downloadActive: active };
  }, [downloads.jobs]);

  // Live web-capture summary → badge on the "Websites" AI sub-tab. Counts both
  // the capture jobs AND the analyzer jobs of web posts (e.g. a tag regeneration
  // from a web post's modal), so the activity icon lands on the tab where the
  // process is actually visible instead of on "AI Tags".
  const { webTotal, webDone, webActive } = useMemo(() => {
    let done = 0;
    let active = false;
    for (const j of webJobs.jobs) {
      if (j.status === 'done') done++;
      else if (WEB_ACTIVE_STATUS.includes(j.status)) active = true;
    }
    return {
      webTotal: webJobs.jobs.length + webAnalysis.total,
      webDone: done + webAnalysis.done,
      webActive: active || webAnalysis.active,
    };
  }, [webJobs.jobs, webAnalysis.total, webAnalysis.done, webAnalysis.active]);

  // Queue controls for the Activity center popover: deps are the individual
  // (stable) hook callbacks, not the whole hook objects, so the handler identity
  // survives progress flushes (the handler itself is defined after the browser
  // navigation callbacks it closes over).
  const {
    isPaused: downloadsPaused,
    pauseAll: pauseDownloads,
    resumeAll: resumeDownloads,
    cancelAll: cancelAllDownloads,
  } = downloads;
  const { cancelJob: cancelWebJob, retryJob: retryWebJob } = webJobs;

  const refreshStats = useCallback(
    () =>
      client
        .getStats()
        .then(setStats)
        .catch((e) => console.error('[App] getStats failed:', e)),
    [client],
  );

  // From a web post's modal: jump to the Websites panel, optionally re-running
  // the whole capture+analysis with overwrite so the bad screenshots/data are
  // regenerated and the live process is visible there.
  const goOpenInWebsites = useCallback(() => setView('aiweb'), [setView]);
  const goReanalyzeWeb = useCallback(
    async (p: Shelfy.Post): Promise<void> => {
      const url = p?.webUrl || p?.webFinalUrl || p?.postUrl;
      if (!url) return;
      try {
        // singlePage is deliberately left undefined: the orchestrator replays the
        // persisted capture mode (web_meta_json → webSinglePage). Passing the
        // renderer's copy here proved unreliable — the gallery post can be stale
        // (e.g. opened while the first capture was still a placeholder), which
        // silently turned a single-page reference into a full sitemap crawl.
        await window.electronAPI.addWebReference(url, undefined, true);
      } catch (e) {
        console.error('[App] reanalyze web failed:', e);
      }
      setView('aiweb');
    },
    [setView],
  );

  const handleSelectSource = useCallback(
    (source: ActiveSource) => {
      setActiveSource(source);
      setGallerySourceNonce((n) => n + 1);
      // Web: the source's address, which shows the gallery.
      if (navigate) navigate(routeOfSource(source));
      else setStateView('gallery');
    },
    [navigate],
  );

  const handleSelectBrowserTab = useCallback(
    (tabId: BrowserPlatform) => {
      setBrowserTab(tabId);
      setView('browser');
      clearAlert(tabId);
    },
    [clearAlert, setView],
  );

  // Navigazione dalla sidebar e dal Centro Attività. Quando una notifica di sync
  // chiede il Browser porta anche la piattaforma, così apriamo il sotto-tab giusto.
  const handleNavigate = useCallback(
    (nextView: View, opts?: NavigateOpts) => {
      if (nextView === 'browser' && opts?.platform) {
        handleSelectBrowserTab(opts.platform);
        return;
      }
      setView(nextView);
    },
    [handleSelectBrowserTab, setView],
  );

  // Queue controls fired from the Activity center popover (pause / cancel-all).
  const handleActivityAction = useCallback(
    (id: string, item?: ActivityActionItem) => {
      switch (id) {
        case 'analysis-toggle':
          (analysisPaused ? resumeAnalyze : pauseAnalyze)?.();
          break;
        case 'analysis-cancel':
          cancelAllAnalyze?.();
          break;
        case 'download-toggle':
          (downloadsPaused ? resumeDownloads : pauseDownloads)?.();
          break;
        case 'download-cancel':
          cancelAllDownloads?.();
          break;
        case 'web-cancel':
          cancelWebJob?.(item?.id);
          break;
        case 'web-retry':
          retryWebJob?.(item?.id);
          break;
        case 'sourcesync-stop':
          if (item?.platform) sourceSyncApiRef.current?.stop(item.platform);
          break;
        case 'sourcesync-open':
          // CTA del fallimento per login: porta al connettore giusto per accedere
          // e archivia l'errore (il prossimo sync ripartirà pulito).
          if (item?.platform) {
            sourceSyncApiRef.current?.dismiss(item.platform);
            handleSelectBrowserTab(item.platform);
          }
          break;
        case 'sourcesync-dismiss':
          if (item?.platform) sourceSyncApiRef.current?.dismiss(item.platform);
          break;
        default:
          break;
      }
    },
    [
      analysisPaused,
      resumeAnalyze,
      pauseAnalyze,
      cancelAllAnalyze,
      downloadsPaused,
      resumeDownloads,
      pauseDownloads,
      cancelAllDownloads,
      cancelWebJob,
      retryWebJob,
      handleSelectBrowserTab,
    ],
  );

  const handleCreateCollection = useCallback(
    async ({ name, color }: CollectionDraft) => {
      const created = await createCollection(name, color);
      refreshStats();
      return created;
    },
    [createCollection, refreshStats],
  );

  // Stable identities for the props of the memoized Sidebar / Browser / Settings.
  const openNewCollection = useCallback(() => setCollectionModal({}), []);
  const openEditCollection = useCallback(
    (c: Shelfy.Collection) => setCollectionModal({ initial: c }),
    [],
  );
  const handleBrowserSaved = useCallback(
    ({ count, platform }: BrowserSaved) => setLastSave({ count, platform, ts: Date.now() }),
    [],
  );
  const handleCollectionsChanged = useCallback(() => {
    reloadCollections();
    refreshStats();
  }, [reloadCollections, refreshStats]);
  const handleDataCleared = useCallback(() => {
    refreshStats();
    setNewPostsAlert({ instagram: 0, twitter: 0, pinterest: 0 });
    reloadCollections();
  }, [refreshStats, reloadCollections]);

  const handleSaveCollection = async ({ name, color }: CollectionDraft) => {
    if (collectionModal?.initial) {
      await renameCollection(collectionModal.initial.id, { name, color });
      refreshStats();
      return;
    }
    return handleCreateCollection({ name, color });
  };

  const handleDeleteCollection = async (id: number, opts: { deletePosts?: boolean } = {}) => {
    const res = await removeCollection(id, opts);
    if (activeSource.type === 'collection' && activeSource.value === id) {
      setActiveSource({ type: 'platform', value: 'all' });
      // Web: the folder's address is gone too.
      navigate?.({ name: 'library' }, { replace: true });
    }
    // Re-apply the source filter so the grid reflects the removed tag (and, when
    // its posts were deleted too, drops them library-wide). Refresh counters when
    // posts were actually removed.
    setGallerySourceNonce((n) => n + 1);
    if (opts.deletePosts) refreshStats();
    // Surface the IPC result ({ ok, deletedPosts, errors }) so the modal can warn
    // on partial-delete failures instead of looking like a clean success.
    return res;
  };

  const currentBuild = buildTime;
  useEffect(() => {
    if (!devBarMounted.current) {
      devBarMounted.current = true;
      return;
    }
    setDevBarVisible(true);
    const t = setTimeout(() => setDevBarVisible(false), 1000);
    return () => clearTimeout(t);
  }, [currentBuild]);

  // Read by the mount-once subscriptions below.
  const reloadCollectionsRef = useRef(reloadCollections);
  reloadCollectionsRef.current = reloadCollections;

  // Load stats on mount and keep them live: `stats.changed` says the counters
  // moved (the web stream's event; on the desktop, every new-posts push).
  useEffect(() => {
    refreshStats();
    // A sync can emit hundreds of new-post events in a burst; getStats runs several
    // COUNT(*) scans, so coalesce into at most one refresh per window (immediate on
    // the first event, then a trailing refresh to capture the final totals).
    let lastStats = 0;
    let statsTimer: ReturnType<typeof setTimeout> | null = null;
    const STATS_WINDOW = 800;
    const bumpStats = (): void => {
      const now = Date.now();
      if (now - lastStats >= STATS_WINDOW) {
        lastStats = now;
        refreshStats();
      } else if (!statsTimer) {
        statsTimer = setTimeout(
          () => {
            statsTimer = null;
            lastStats = Date.now();
            refreshStats();
          },
          STATS_WINDOW - (now - lastStats),
        );
      }
    };
    const offStats = client.on('stats.changed', bumpStats);
    // Live changes were lost (the web stream fell behind): reload what the
    // shell shows. Each view reloads its own (usePosts).
    const offResync = client.on('resync', () => {
      bumpStats();
      void reloadCollectionsRef.current();
    });
    const unsub = client.on('posts.changed', (event) => {
      // The event doubles as a generic "reload the list" signal (the 'thumb-blur'
      // backfill, web placeholders): those payloads carry no valid count and must
      // not touch the badge. The counters follow 'stats.changed' (above), so leave
      // newPostsAlert alone here.
      const n = event.count ?? NaN;
      if (!Number.isFinite(n) || n <= 0) return;
      // Solo le tre piattaforme browser hanno un badge in sidebar. Altre sorgenti
      // (es. 'web' dalla pipeline di cattura sito, 'manual') non devono inquinare
      // newPostsAlert con una chiave illimitata e mai azzerata. Niente default a
      // 'instagram': un segnale non etichettato non deve mai incrementare il badge.
      const platform = event.platform;
      if (platform !== 'instagram' && platform !== 'twitter' && platform !== 'pinterest') {
        return;
      }
      // Don't flag the source the user is currently viewing — nothing "new" to notice there.
      // Exception: during an active sync on that platform, keep the badge counting so it
      // shows the live running total of captured posts next to the spinner.
      const focused = viewRef.current === 'browser' && browserTabRef.current === platform;
      const syncing = !!browserSyncingRef.current[platform];
      if (focused && !syncing) return;
      setNewPostsAlert((prev) => ({ ...prev, [platform]: (prev[platform] || 0) + n }));
    });
    return () => {
      if (statsTimer) clearTimeout(statsTimer);
      offStats();
      offResync();
      unsub();
    };
  }, [client, refreshStats]);

  // ActiveSource.value is the shared string|number slot, but the discriminant
  // fixes which it is at runtime: a 'platform' source always carries a string id
  // ('all' | platform key), a 'collection' source always a numeric Collection.id
  // (see every setActiveSource/onSelectSource call site). Narrow accordingly.
  const galleryPlatform: string =
    activeSource.type === 'platform' ? (activeSource.value as string) : 'all';
  const galleryCollectionId: number | null =
    activeSource.type === 'collection' ? (activeSource.value as number) : null;
  const galleryCollection = galleryCollectionId
    ? collections.find((c) => c.id === galleryCollectionId)
    : null;
  const galleryResetKey = `${activeSource.type}:${activeSource.value}`;

  // Pages a route has nothing for. `route-back` returns to the library.
  const backToLibrary = { label: te('backToLibrary'), testId: 'route-back', primary: true };
  const unavailablePanel = (
    <ErrorPanel
      testId="route-unavailable"
      icon={Construction}
      title={te('unavailableTitle')}
      message={te('unavailable')}
      actions={[{ ...backToLibrary, onClick: () => handleSelectSource(ALL_SOURCE) }]}
    />
  );
  // An address this app has nothing for at all. A `/p/:key` whose post is
  // missing is Gallery's own not-found panel now (P1-06): the gallery is what
  // resolves the route to a post, so it is what finds out there isn't one.
  const routePanel: React.ReactNode =
    route?.name === 'notFound' ? (
      <ErrorPanel
        testId="route-not-found"
        icon={FileQuestion}
        title={te('notFoundTitle')}
        message={te('notFound')}
        actions={[
          { ...backToLibrary, onClick: () => navigate?.({ name: 'library' }, { replace: true }) },
        ]}
      />
    ) : null;

  const lastUpdate = new Date(buildTime).toLocaleTimeString(localeTag(lang), {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });

  // Le prop passate al provider sono memoizzate sulle rispettive dipendenze
  // primitive/array: object literal freschi ad ogni render invaliderebbero i
  // useMemo interni del provider (e quindi tutto il context value) inutilmente.
  const analysisSrc = useMemo(
    () => ({ jobs: analysisJobs, concurrency: analysisConcurrency, paused: analysisPaused }),
    [analysisJobs, analysisConcurrency, analysisPaused],
  );
  const downloadsSrc = useMemo(
    () => ({ jobs: downloads.jobs, isPaused: downloads.isPaused }),
    [downloads.jobs, downloads.isPaused],
  );
  const modelSrc = useMemo(
    () => ({ status: modelStatus, progress: modelProgress }),
    [modelStatus, modelProgress],
  );
  const syncSrc = useMemo(
    () => ({ syncing: browserSyncing, counts: newPostsAlert, jobs: sourceSyncJobs }),
    [browserSyncing, newPostsAlert, sourceSyncJobs],
  );
  const saveSrc = useMemo(() => ({ active: saving, lastSave }), [saving, lastSave]);
  // Stable identity for the web slice too, so the ActivityProvider's liveSources
  // memo doesn't re-fire on every unrelated App render (a fresh object literal here
  // would defeat the per-slice memoization the provider relies on).
  const webSrc = useMemo(() => ({ jobs: webJobs.jobs }), [webJobs.jobs]);

  return (
    <ActivityProvider
      analysis={analysisSrc}
      downloads={downloadsSrc}
      model={modelSrc}
      sync={syncSrc}
      save={saveSrc}
      web={webSrc}
    >
      <div className="relative flex narrow:flex-col h-screen w-screen overflow-hidden bg-[#0f0f0f]">
        {/* Frameless window controls — Windows/Linux only (macOS uses its native
          traffic lights). Floats over the top-right corner of the shell; the
          gallery toolbar reserves room for it (see Gallery's needsWinControls). */}
        {caps.windowControls && (
          <div className="absolute top-0 right-0 z-50 no-drag">
            <WindowControls />
          </div>
        )}
        {import.meta.env.DEV && devBarVisible && (
          <div className="u-fade-in-down fixed bottom-0 left-0 right-0 z-50 bg-yellow-400/90 text-black text-xs text-center py-1 px-3 font-mono">
            {t('devUpdated', { time: lastUpdate })}
          </div>
        )}
        <Sidebar
          currentView={view}
          onNavigate={handleNavigate}
          stats={stats}
          newPostsAlert={newPostsAlert}
          browserSyncing={browserSyncing}
          onClearAlert={clearAlert}
          browserTab={browserTab}
          onSelectBrowserTab={handleSelectBrowserTab}
          onAddSite={openAddSite}
          onAddBookmark={openAddBookmark}
          onSelectSource={handleSelectSource}
          collections={collections}
          activeSource={activeSource}
          onAddCollection={openNewCollection}
          onEditCollection={openEditCollection}
          analysisActive={analysisActive}
          analysisDone={analysisDone}
          analysisTotal={analysisTotal}
          downloadActive={downloadActive}
          downloadDone={downloadDone}
          downloadTotal={downloadTotal}
          webActive={webActive}
          webDone={webDone}
          webTotal={webTotal}
          onActivityAction={handleActivityAction}
        />
        <main className="flex-1 narrow:min-h-0 overflow-hidden relative">
          {caps.ai && (
            <Suspense fallback={null}>
              <RemoteAiBanner />
            </Suspense>
          )}
          {/* Browser is always mounted so its webviews keep syncing in the background,
            even when another view is on screen; an opaque overlay covers it meanwhile.
            zIndex:0 makes this an isolated stacking context so the Browser's own
            loading bar (zIndex:10) can't poke through the overlay (zIndex:2) below. */}
          {caps.browser && (
            <div style={{ position: 'absolute', inset: 0, zIndex: 0 }}>
              <ErrorBoundary view="browser">
                <BrowserMemo
                  activeTab={browserTab}
                  onSyncingChange={setBrowserSyncing}
                  onSavingChange={setSaving}
                  onSaved={handleBrowserSaved}
                  collections={collections}
                  onCreateCollection={createCollection}
                  onCollectionsChanged={handleCollectionsChanged}
                  registerSourceSyncApi={registerSourceSyncApi}
                  onSourceSyncJobs={setSourceSyncJobs as (jobs: unknown) => void}
                />
              </ErrorBoundary>
            </div>
          )}
          {/* Keep-alive: every visited (non-browser) view stays mounted in its own
            layer and is shown/hidden via `visibility` instead of being unmounted.
            Switching back is instant — no re-fetch, no skeleton, preserved scroll
            and state. `visibility:hidden` (not display:none) keeps each layer's
            dimensions, so the virtualized grids keep windowing correctly while
            off-screen instead of falling back to rendering everything. The whole
            overlay is hidden (revealing the always-mounted Browser) on the
            'browser' tab. Each layer fades in once, on first mount only. */}
          <div
            style={{
              position: 'absolute',
              inset: 0,
              zIndex: 2,
              background: '#0f0f0f',
              visibility: view === 'browser' ? 'hidden' : 'visible',
            }}
          >
            {VIEW_IDS.filter((v) => mountedViews.has(v)).map((v) => {
              const visible = view === v;
              return (
                <div
                  key={v}
                  // Keep-alive cross-fade: each layer stays mounted (state/scroll
                  // preserved) and is shown/hidden via visibility. Re-animating with a
                  // keyframe would require a remount, which would defeat keep-alive — so
                  // instead we drive opacity with a CSS transition (u-transition, dur-3)
                  // that fires on every activation, giving a coherent cross-fade in/out
                  // without ever unmounting the view.
                  style={{
                    position: 'absolute',
                    inset: 0,
                    visibility: visible ? 'visible' : 'hidden',
                    opacity: visible ? 1 : 0,
                    transition: 'opacity var(--dur-3) var(--ease-out)',
                  }}
                >
                  {/* A crashed view renders again when it comes back on screen
                    (the gallery also when its source changes). */}
                  <ErrorBoundary
                    view={v}
                    resetKey={visible && (v === 'gallery' ? galleryResetKey : true)}
                  >
                    {v === 'gallery' && (
                      <GalleryMemo
                        active={view === 'gallery'}
                        platform={galleryPlatform}
                        collectionId={galleryCollectionId}
                        collectionLabel={galleryCollection?.name}
                        collectionColor={galleryCollection?.color}
                        sourceNonce={gallerySourceNonce}
                        collections={collections}
                        stats={stats as Shelfy.Stats | Record<string, never>}
                        activeSource={activeSource}
                        onSelectSource={handleSelectSource}
                        onCreateCollection={handleCreateCollection}
                        onAssigned={reloadCollections}
                        onStatsChanged={refreshStats}
                        onOpenInWebsites={caps.websites ? goOpenInWebsites : undefined}
                        onReanalyzeWeb={caps.websites ? goReanalyzeWeb : undefined}
                        sourceSyncJobs={sourceSyncJobs}
                        onSyncSource={caps.browser ? handleSyncSource : undefined}
                      />
                    )}
                    {v === 'downloads' && (
                      <Suspense fallback={<ViewLoading />}>
                        <DownloadsMemo downloads={downloads} />
                      </Suspense>
                    )}
                    {v === 'jobs' &&
                      (caps.jobs ? (
                        <Suspense fallback={<ViewLoading />}>
                          <JobsMemo onOpenPost={openJobPost} />
                        </Suspense>
                      ) : (
                        unavailablePanel
                      ))}
                    {v === 'aitags' && (
                      <Suspense fallback={<ViewLoading />}>
                        <AiTagsMemo
                          active={view === 'aitags'}
                          initialTag={aiTagsInitial.tag}
                          initialTagNonce={aiTagsInitial.nonce}
                          onOpenInWebsites={goOpenInWebsites}
                          onReanalyzeWeb={goReanalyzeWeb}
                        />
                      </Suspense>
                    )}
                    {v === 'aiqueue' && (
                      <Suspense fallback={<ViewLoading />}>
                        <AiTagsQueueMemo onOpenPost={openAiPost} />
                      </Suspense>
                    )}
                    {v === 'aiweb' && (
                      <Suspense fallback={<ViewLoading />}>
                        <AiWebsitesMemo
                          webJobs={webJobs as React.ComponentProps<typeof AiWebsites>['webJobs']}
                          onAddSite={openAddSite}
                          onOpenPost={openAiPost}
                        />
                      </Suspense>
                    )}
                    {v === 'aisearch' && (
                      <Suspense fallback={<ViewLoading />}>
                        <AiSearchMemo
                          onOpenInWebsites={goOpenInWebsites}
                          onReanalyzeWeb={goReanalyzeWeb}
                        />
                      </Suspense>
                    )}
                    {v === 'settings' &&
                      (caps.settings ? (
                        <Suspense fallback={<ViewLoading />}>
                          <SettingsMemo onDataCleared={handleDataCleared} />
                        </Suspense>
                      ) : (
                        unavailablePanel
                      ))}
                    {/* The trash view arrives with P1-14. */}
                    {v === 'trash' && unavailablePanel}
                  </ErrorBoundary>
                </div>
              );
            })}

            {/* AI onboarding gate: a keep-alive overlay above the AI tabs. Stays
                mounted while the gate is latched (so an in-flight install keeps
                its progress when the user wanders off) and is only visible while
                an AI tab is active — every other view is untouched. */}
            {aiGate && (
              <div
                data-testid="ai-onboarding-gate"
                style={{
                  position: 'absolute',
                  inset: 0,
                  zIndex: 5,
                  background: '#0f0f0f',
                  visibility: AI_VIEW_IDS.includes(view as ViewId) ? 'visible' : 'hidden',
                  opacity: AI_VIEW_IDS.includes(view as ViewId) ? 1 : 0,
                  transition: 'opacity var(--dur-3) var(--ease-out)',
                }}
              >
                <Suspense fallback={<ViewLoading />}>
                  <AiOnboarding
                    onDone={() => dismissAiGate(false)}
                    onSkip={() => dismissAiGate(true)}
                    onOpenSettings={() => setView('settings')}
                  />
                </Suspense>
              </div>
            )}

            {/* Web: an address with nothing behind it, over the views. */}
            {routePanel && (
              <div style={{ position: 'absolute', inset: 0, zIndex: 6, background: '#0f0f0f' }}>
                {routePanel}
              </div>
            )}
          </div>
        </main>
        {/* Bottom navigation (narrow only): a normal flex-column sibling of
          `<main>`, not a floating overlay — App's `narrow:flex-col` reserves
          its height instead of it covering the last row of content. */}
        <BottomNav
          active={bottomNavTargetOfView(view)}
          onNavigate={handleBottomNav}
          aiVisible={caps.ai}
          jobsVisible={caps.jobs}
        />
        {collectionModal && (
          <Suspense fallback={null}>
            <CollectionModal
              initial={collectionModal.initial}
              collections={collections}
              onClose={() => setCollectionModal(null)}
              onSave={handleSaveCollection}
              onDelete={handleDeleteCollection}
            />
          </Suspense>
        )}
        {showAddSite && (
          <Suspense fallback={null}>
            <AddSiteModal
              onClose={() => setShowAddSite(false)}
              onAdded={() => {
                refreshStats();
                setShowAddSite(false);
                setView('aiweb');
              }}
            />
          </Suspense>
        )}
        {showAddBookmark && (
          <Suspense fallback={null}>
            <AddBookmarkModal
              onClose={() => setShowAddBookmark(false)}
              onAdded={() => {
                refreshStats();
                setShowAddBookmark(false);
                // Jump to the "all posts" gallery (re-applies the source → reloads the
                // grid) so the freshly-added bookmark is visible right away even if a
                // platform filter was active.
                handleSelectSource({ type: 'platform', value: 'all' });
              }}
            />
          </Suspense>
        )}
        {aiModalPost && (
          <ErrorBoundary
            view="postModal"
            layout="dialog"
            resetKey={aiModalPost.id}
            onDismiss={() => setAiModalPost(null)}
          >
            <PostModal
              post={aiModalPost}
              onClose={() => setAiModalPost(null)}
              onApplyAiFilter={(patch: AiFilterPatch) => {
                setAiModalPost(null);
                if (patch?.tag) {
                  setAiTagsInitial((s) => ({ tag: patch.tag ?? null, nonce: s.nonce + 1 }));
                  setView('aitags');
                }
              }}
              onPostUpdated={(postId: string, fields: Partial<Shelfy.Post>) =>
                setAiModalPost((prev) =>
                  prev && prev.id === postId ? { ...prev, ...fields } : prev,
                )
              }
              onAssigned={reloadCollections}
              onOpenInWebsites={() => {
                setAiModalPost(null);
                goOpenInWebsites();
              }}
              onReanalyzeWeb={(p: Shelfy.Post) => {
                setAiModalPost(null);
                goReanalyzeWeb(p);
              }}
            />
          </ErrorBoundary>
        )}
        {/* Web: the post of a `/p/:key` address now opens inside Gallery itself
          (P1-06), the same modal a card click opens — so it's rendered there,
          not here. This file still renders `routePanel` (above) when the
          address is `notFound`, and the folder-create/edit modal below. */}
        {/* First-run legal gate: blocks the app until the current notices are
          accepted (localStorage on the desktop, the account on the web). */}
        <ConsentGate />
      </div>
    </ActivityProvider>
  );
}
