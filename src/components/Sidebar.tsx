import React, { useEffect, useRef, useState } from 'react';
import {
  Grid3X3,
  Globe,
  Download,
  Settings,
  Plus,
  Pencil,
  RefreshCw,
  Sparkles,
  Loader2,
  ChevronDown,
  Instagram,
  Twitter,
  MessageSquare,
  Bookmark,
  FolderPlus,
} from 'lucide-react';
import PinterestIcon from './PinterestIcon';
import Logo from './Logo';
import ActivityCenter from './ActivityCenter';
import FeedbackModal from './FeedbackModal';
import { useT } from '../i18n';

// Translator returned by useT — namespaced key + optional interpolation vars.
type Translate = (key: string, vars?: Record<string, string | number>) => string;

// Lucide-compatible icon component: the subset of props the sidebar passes to
// icons held in the platform/browser arrays (rendered with size + className
// only). Covers both lucide-react icons and the local PinterestIcon glyph.
type IconComponent = (props: { size?: number; className?: string }) => React.ReactNode;

// Navigable views (mirrors App's View union).
type NavView =
  | 'gallery'
  | 'downloads'
  | 'aitags'
  | 'aiqueue'
  | 'aiweb'
  | 'aisearch'
  | 'settings'
  | 'browser';

// The three browser-backed platforms that own a sidebar badge.
type BrowserPlatform = 'instagram' | 'twitter' | 'pinterest';

// Extra routing params handed alongside a view (platform for sync, postId for web).
interface NavParams {
  platform?: BrowserPlatform;
  postId?: string | null;
}

// The minimal item shape carried by a delegated activity action (queue controls).
interface ActivityActionItem {
  id?: string;
  platform?: BrowserPlatform;
}

// A library source selection routed back up to App.
interface SourceSelection {
  type: 'platform' | 'collection';
  value: string | number;
  label?: string;
  color?: string;
}

// The currently-active source driving the highlight.
interface ActiveSource {
  type: 'platform' | 'collection';
  value: string | number;
}

// One AI sub-tab descriptor.
interface AiTab {
  id: NavView;
  key: string;
}

// One platform-stats row descriptor (verbatim label or localized via `key`).
interface PlatformStat {
  id: string;
  label?: string;
  key?: string;
  Icon: IconComponent;
}

// One social browser sub-tab descriptor.
interface BrowserTab {
  id: BrowserPlatform;
  label: string;
  Icon: IconComponent;
}

// Persisted collapse-state maps (keyed by group / platform id).
type ExpandedGroups = { browser: boolean; bookmarks: boolean; ai: boolean };
type ExpandedPlatforms = Record<string, boolean>;

// The header total + per-platform counts the sidebar reads. Loose enough to
// accept both the full Shelfy.Stats and App's pre-fetch InitialStats shape.
interface SidebarStats {
  total?: number;
  byPlatform?: Partial<Record<string, number>>;
}

interface SidebarProps {
  currentView: NavView | string;
  onNavigate: (view: NavView, params?: NavParams) => void;
  stats?: SidebarStats;
  newPostsAlert?: Partial<Record<BrowserPlatform, number>>;
  browserSyncing?: Partial<Record<BrowserPlatform, boolean>>;
  // Passed by App but unused here (the per-tab alert is cleared on tab select up
  // in App); kept on the props so the JSX call site stays type-clean.
  onClearAlert?: (platform: BrowserPlatform) => void;
  browserTab?: BrowserPlatform;
  onSelectBrowserTab?: (tab: BrowserPlatform) => void;
  onAddSite?: () => void;
  onAddBookmark?: () => void;
  onSelectSource?: (source: SourceSelection) => void;
  collections?: Shelfy.Collection[];
  activeSource?: ActiveSource;
  onAddCollection?: () => void;
  onEditCollection?: (collection: Shelfy.Collection) => void;
  analysisActive?: boolean;
  analysisDone?: number;
  analysisTotal?: number;
  downloadActive?: boolean;
  downloadDone?: number;
  downloadTotal?: number;
  webActive?: boolean;
  webDone?: number;
  webTotal?: number;
  onActivityAction?: (id: string, item?: ActivityActionItem) => void;
}

// The live-analysis badge rides on the "Auto-tag" queue tab (aiqueue), since
// that's where the running queue lives; "Tags Explorer" stays for browsing.
// `key` resolves to a `sidebar` i18n message at render (hooks can't run at
// module scope); `id` stays stable for navigation/test ids.
const AI_TABS: AiTab[] = [
  { id: 'aiqueue', key: 'aiqueue' },
  { id: 'aiweb', key: 'aiweb' },
  { id: 'aisearch', key: 'aisearch' },
  { id: 'aitags', key: 'aitags' },
];

// Brand icons (lucide) instead of coloured dots. Neutral tint: they inherit the
// row's text colour (grey idle, white when active) like the other nav icons.
// Brand names render verbatim; only the 'web' label is localized via `key`.
const PLATFORM_STATS: PlatformStat[] = [
  { id: 'instagram', label: 'Instagram', Icon: Instagram },
  { id: 'twitter', label: 'X / Twitter', Icon: Twitter },
  { id: 'pinterest', label: 'Pinterest', Icon: PinterestIcon },
  { id: 'web', key: 'web', Icon: Globe },
];

const BROWSER_TABS: BrowserTab[] = [
  { id: 'instagram', label: 'Instagram', Icon: Instagram },
  { id: 'twitter', label: 'X / Twitter', Icon: Twitter },
  { id: 'pinterest', label: 'Pinterest', Icon: PinterestIcon },
];

// Persist collapse state across remounts (dev hot-reload) and app restarts so a
// user who collapses noisy groups/platform folders doesn't have to re-do it.
const LS_GROUPS_KEY = 'shelfy.sidebar.expandedGroups';
const LS_PLATFORMS_KEY = 'shelfy.sidebar.expandedPlatforms';

function loadPersisted<T extends Record<string, unknown>>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return fallback;
    const parsed: unknown = JSON.parse(raw);
    return parsed && typeof parsed === 'object'
      ? { ...fallback, ...(parsed as Partial<T>) }
      : fallback;
  } catch {
    return fallback;
  }
}

function savePersisted(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* localStorage unavailable / quota — non-fatal, state stays in-memory */
  }
}

function formatCount(n: number | null | undefined): string {
  if (n == null) return '0';
  return n.toLocaleString();
}

// Badge count: show the real number up to five digits, then "99999+".
function formatBadge(n: number): string {
  return n > 99999 ? '99999+' : String(n);
}

// Memoized (see export below): App re-renders on every queue/progress flush; the
// sidebar only needs to follow its own (mostly primitive) props.
function Sidebar({
  currentView,
  onNavigate,
  stats,
  newPostsAlert = {},
  browserSyncing = {},
  browserTab,
  onSelectBrowserTab,
  onAddSite,
  onAddBookmark,
  onSelectSource,
  collections = [],
  activeSource,
  onAddCollection,
  onEditCollection,
  // Live-analysis summary still drives the per-tab badge on "AI Tags"; the full
  // background-activity surface now lives in <ActivityCenter> (sidebar footer).
  analysisActive = false,
  analysisDone = 0,
  analysisTotal = 0,
  downloadActive = false,
  downloadDone = 0,
  downloadTotal = 0,
  // Live web-capture summary → small badge on the "Websites" AI sub-tab.
  webActive = false,
  webDone = 0,
  webTotal = 0,
  // Queue controls (pause/cancel) routed from the Activity center popover.
  onActivityAction,
}: SidebarProps): React.JSX.Element {
  const t: Translate = useT('sidebar');
  const total = stats?.total ?? 0;
  const byPlatform: Partial<Record<string, number>> = stats?.byPlatform ?? {};

  // Feedback modal (mailto verso lo sviluppatore), aperto dal footer sopra Attività.
  const [feedbackOpen, setFeedbackOpen] = useState<boolean>(false);

  // Collapsible nav groups (Connections, Library, AI). Default expanded; toggled
  // independently. `browser`/`bookmarks` are kept as the group keys for
  // backwards-compat with persisted state / tests. Hydrated from localStorage so
  // the choice survives remounts and restarts.
  const [expandedGroups, setExpandedGroups] = useState<ExpandedGroups>(() =>
    loadPersisted<ExpandedGroups>(LS_GROUPS_KEY, { browser: true, bookmarks: true, ai: true }),
  );
  const toggleGroup = (id: keyof ExpandedGroups): void =>
    setExpandedGroups((prev) => ({ ...prev, [id]: !prev[id] }));

  // Per-platform expansion of nested folder-tags (e.g. Instagram saved folders).
  // Default expanded — undefined is treated as open. Persisted like the groups.
  const [expandedPlatforms, setExpandedPlatforms] = useState<ExpandedPlatforms>(() =>
    loadPersisted<ExpandedPlatforms>(LS_PLATFORMS_KEY, {}),
  );
  const togglePlatform = (id: string): void =>
    setExpandedPlatforms((prev) => ({ ...prev, [id]: prev[id] === false }));

  useEffect(() => savePersisted(LS_GROUPS_KEY, expandedGroups), [expandedGroups]);
  useEffect(() => savePersisted(LS_PLATFORMS_KEY, expandedPlatforms), [expandedPlatforms]);

  // Gate the staggered entrance animation to first mount + genuinely-new rows,
  // so a reload (after every assign/create/rename/delete) doesn't replay the
  // whole list's stagger. `seenIds` holds the ids present at the last commit;
  // a row animates only when its id wasn't there yet. First commit: animate all.
  const seenIds = useRef<Set<number> | null>(null);
  const animatedIds = seenIds.current; // null on first render → animate everything
  useEffect(() => {
    seenIds.current = new Set(collections.map((c) => c.id));
  }, [collections]);
  const isNewRow = (id: number): boolean => animatedIds == null || !animatedIds.has(id);

  const isActive = (type: ActiveSource['type'], value: string | number): boolean =>
    currentView === 'gallery' && activeSource?.type === type && activeSource?.value === value;

  // One custom-source / folder-tag row. `nested` indents it under a platform.
  // Active-row indicator: a small rounded accent bar pinned to the left edge.
  const accentBar = (color = '#7B5CFF'): React.JSX.Element => (
    <span
      aria-hidden
      className="u-bar-in absolute left-0 inset-y-0 my-auto h-4 w-[3px] rounded-r-full"
      style={{ backgroundColor: color, transformOrigin: 'center' }}
    />
  );

  const renderCollectionRow = (
    c: Shelfy.Collection,
    i: number,
    { nested = false, isLast = false }: { nested?: boolean; isLast?: boolean } = {},
  ): React.JSX.Element => {
    const active = isActive('collection', c.id);
    const animate = isNewRow(c.id);
    return (
      <div
        key={c.id}
        data-testid={`source-collection-${c.id}`}
        style={animate ? { animationDelay: Math.min(i, 8) * 30 + 'ms' } : undefined}
        className={[
          'u-press group relative w-full flex items-center py-1.5 text-sm rounded-md cursor-pointer transition-colors mx-2',
          animate ? 'u-fade-in-up' : '',
          nested ? 'pl-14 pr-2' : 'pl-9 pr-2',
          active
            ? 'bg-[#1e1e1e] text-white'
            : 'text-gray-400 hover:bg-[#1a1a1a] hover:text-gray-200',
        ].join(' ')}
        onClick={() =>
          onSelectSource?.({ type: 'collection', value: c.id, label: c.name, color: c.color })
        }
      >
        {active && accentBar(c.color)}
        {/* Tree guide: the vertical trunk runs full-height between siblings but
            stops at the tick for the LAST child (┗), so it doesn't dangle below. */}
        {nested && (
          <>
            <span
              aria-hidden
              className={[
                'absolute left-[2.7rem] top-0 w-px bg-[#2a2a2a]',
                isLast ? 'h-1/2' : 'bottom-0',
              ].join(' ')}
            />
            <span
              aria-hidden
              className="absolute left-[2.7rem] top-1/2 -translate-y-1/2 h-px w-3 bg-[#2a2a2a] group-hover:bg-[#3a3a3a] transition-colors"
            />
          </>
        )}
        <span
          className={[
            'w-2 h-2 rounded-full shrink-0 transition-shadow',
            active ? 'u-pop-in' : '',
          ].join(' ')}
          style={{
            backgroundColor: c.color,
            boxShadow: active ? `0 0 0 3px ${c.color}33` : 'none',
          }}
        />
        <span className="ml-2.5 flex-1 truncate">{c.name}</span>
        <button
          data-testid={`edit-collection-${c.id}`}
          onClick={(e: React.MouseEvent<HTMLButtonElement>) => {
            e.stopPropagation();
            onEditCollection?.(c);
          }}
          title={t('editSource')}
          className="u-press u-fade-in hidden group-hover:flex items-center justify-center w-5 h-5 rounded text-gray-500 hover:text-white transition-colors"
        >
          <Pencil size={13} />
        </button>
        <span className="ml-1.5 text-[11px] text-gray-500 tabular-nums group-hover:hidden">
          {formatCount(c.count ?? 0)}
        </span>
        {/* Keeps the count aligned in the same column as rows that DO have a chevron. */}
        <span aria-hidden className="w-5 ml-1 shrink-0" />
      </div>
    );
  };

  // Shared chevron used by the collapsible group headers.
  const groupChevron = (open: boolean): React.JSX.Element => (
    <ChevronDown
      size={14}
      className={[
        'shrink-0 text-gray-500 transition-transform duration-200',
        open ? '' : '-rotate-90',
      ].join(' ')}
    />
  );

  return (
    <aside
      data-testid="sidebar"
      className="flex flex-col w-[240px] min-w-[240px] h-full bg-[#111111] border-r border-[#2e2e2e] overflow-hidden select-none"
    >
      {/* App header — pinned above the single scrolling menu */}
      <div className="u-fade-in flex items-center gap-2.5 px-4 h-14 shrink-0">
        <Logo size={20} />
        <div className="flex flex-col leading-tight">
          <span className="font-display text-white text-[15px] font-semibold tracking-wide">
            SHELFY
          </span>
          <span className="text-gray-500 text-[11px] mt-0.5">
            {t('postsCount', { n: formatCount(total) })}
          </span>
        </div>
      </div>

      {/* One single scrollable menu: Connections, Library (with Downloads), AI, then the
          footer actions (Feedback / Attività / Impostazioni). Everything lives in
          the same scroll flow — when it runs out of vertical room it scrolls,
          nothing is pinned as an overlay on top. */}
      <div
        data-testid="sidebar-scroll"
        className="flex-1 min-h-0 overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e] scrollbar-track-transparent flex flex-col"
      >
        <nav className="flex flex-col gap-0.5 mt-1">
          {/* ===================== CONNECTIONS (ex Sources/Browser) ===================== */}
          {(() => {
            const open = expandedGroups.browser;
            return (
              <>
                <button
                  data-testid="nav-browser"
                  aria-expanded={open}
                  onClick={() => toggleGroup('browser')}
                  className="u-press flex items-center gap-3 px-4 py-2.5 rounded-md mx-2 text-sm text-gray-400 hover:bg-[#1a1a1a] hover:text-gray-200 transition-colors text-left cursor-pointer"
                >
                  <Globe size={16} strokeWidth={1.75} className="shrink-0" />
                  <span className="flex-1">{t('sources')}</span>
                  {groupChevron(open)}
                </button>

                {open && (
                  <div className="flex flex-col gap-0.5 mb-0.5">
                    {BROWSER_TABS.map((tab, i) => {
                      const subActive = currentView === 'browser' && browserTab === tab.id;
                      const count = newPostsAlert?.[tab.id] || 0;
                      const syncing = !!browserSyncing?.[tab.id];
                      const TabIcon = tab.Icon;
                      return (
                        <button
                          key={tab.id}
                          data-testid={`browser-tab-${tab.id}`}
                          aria-current={subActive ? 'page' : undefined}
                          onClick={() => onSelectBrowserTab?.(tab.id)}
                          style={{ animationDelay: i * 30 + 'ms' }}
                          className={[
                            'u-press u-fade-in-down flex items-center gap-2 pl-9 pr-4 py-1.5 rounded-md mx-2 cursor-pointer text-sm transition-colors text-left',
                            subActive
                              ? 'bg-[#1e1e1e] text-white'
                              : 'text-gray-400 hover:bg-[#1a1a1a] hover:text-gray-200',
                          ].join(' ')}
                        >
                          <TabIcon size={15} className="shrink-0" />
                          <span className="flex-1">{tab.label}</span>
                          {syncing && (
                            <RefreshCw
                              size={12}
                              data-testid={`browser-tab-${tab.id}-syncing`}
                              className="shrink-0 text-amber-400 u-spin"
                            />
                          )}
                          {count > 0 && (
                            <span
                              data-testid={`browser-tab-${tab.id}-badge`}
                              className="u-pop-in flex items-center justify-center min-w-[18px] h-[18px] px-1 rounded-full bg-[#7B5CFF] text-white text-[10px] font-semibold leading-none tabular-nums"
                            >
                              {formatBadge(count)}
                            </span>
                          )}
                        </button>
                      );
                    })}

                    {/* Action row inside the Connections group: opens the "add web
                        reference" modal (sites via URL, alongside the social tabs
                        that import via webview). Not a view → no active state.
                        Action rows read lighter (gray-500) than nav rows and share
                        the Plus affordance, so they don't pass for destinations. */}
                    <button
                      data-testid="browser-tab-add-site"
                      onClick={() => onAddSite?.()}
                      title={t('addSite')}
                      style={{ animationDelay: BROWSER_TABS.length * 30 + 'ms' }}
                      className="u-press u-fade-in-down flex items-center gap-2 pl-9 pr-4 py-1.5 rounded-md mx-2 cursor-pointer text-sm text-gray-500 hover:bg-[#1a1a1a] hover:text-gray-200 transition-colors text-left"
                    >
                      <Plus size={15} className="shrink-0" />
                      <span className="flex-1">{t('website')}</span>
                    </button>

                    {/* Manual bookmark: add local files (images/videos/pdf/any) +
                        note + tags. Sits alongside "Add website" as an add-content
                        action; not a view → no active state. */}
                    <button
                      data-testid="browser-tab-add-bookmark"
                      onClick={() => onAddBookmark?.()}
                      title={t('addBookmark')}
                      style={{ animationDelay: (BROWSER_TABS.length + 1) * 30 + 'ms' }}
                      className="u-press u-fade-in-down flex items-center gap-2 pl-9 pr-4 py-1.5 rounded-md mx-2 cursor-pointer text-sm text-gray-500 hover:bg-[#1a1a1a] hover:text-gray-200 transition-colors text-left"
                    >
                      <Plus size={15} className="shrink-0" />
                      <span className="flex-1">{t('manualBookmark')}</span>
                    </button>
                  </div>
                )}
              </>
            );
          })()}

          {/* ===================== LIBRARY (ex Bookmarks/Gallery) ===================== */}
          {(() => {
            const open = expandedGroups.bookmarks;
            return (
              <div data-testid="sidebar-bookmarks" className="flex flex-col mt-2">
                {/* Group header: toggles the section. Adding a custom source now
                    lives as an explicit "New folder" row at the bottom of the list
                    (mirrors the "Add website" action in the Connections group). */}
                <button
                  data-testid="nav-bookmarks"
                  aria-expanded={open}
                  onClick={() => toggleGroup('bookmarks')}
                  className="u-press flex items-center gap-3 px-4 py-2.5 rounded-md mx-2 text-sm text-gray-400 hover:bg-[#1a1a1a] hover:text-gray-200 transition-colors text-left cursor-pointer"
                >
                  <Bookmark size={16} strokeWidth={1.75} className="shrink-0" />
                  <span className="flex-1 truncate">{t('bookmarks')}</span>
                  {groupChevron(open)}
                </button>

                {open && (
                  <div className="flex flex-col gap-0.5 mb-0.5 mt-0.5">
                    {/* Downloads — lives under Bookmarks as the first sub-row. */}
                    <button
                      data-testid="nav-downloads"
                      aria-current={currentView === 'downloads' ? 'page' : undefined}
                      onClick={() => onNavigate('downloads')}
                      className={[
                        'u-press group relative w-full flex items-center pl-9 pr-2 py-1.5 text-sm rounded-md mx-2 cursor-pointer transition-colors text-left',
                        currentView === 'downloads'
                          ? 'bg-[#1e1e1e] text-white'
                          : 'text-gray-400 hover:bg-[#1a1a1a] hover:text-gray-200',
                      ].join(' ')}
                    >
                      {currentView === 'downloads' && accentBar()}
                      <Download size={15} className="shrink-0 mr-2.5" />
                      <span className="flex-1 truncate">{t('downloads')}</span>
                      {downloadActive && (
                        <>
                          <Loader2
                            size={14}
                            data-testid="nav-downloads-active"
                            className="shrink-0 text-[#7B5CFF] u-spin"
                          />
                          <span
                            data-testid="nav-downloads-badge"
                            className="u-pop-in ml-1.5 flex items-center justify-center min-w-[18px] h-[18px] px-1 rounded-full bg-[#7B5CFF] text-white text-[10px] font-semibold leading-none tabular-nums"
                          >
                            {downloadDone}/{downloadTotal}
                          </span>
                        </>
                      )}
                      {/* Empty chevron slot so the badge/label aligns with counted rows. */}
                      <span aria-hidden className="w-5 ml-1 shrink-0" />
                    </button>

                    {/* All posts */}
                    <button
                      data-testid="source-all"
                      aria-current={isActive('platform', 'all') ? 'page' : undefined}
                      onClick={() => onSelectSource?.({ type: 'platform', value: 'all' })}
                      className={[
                        'u-press group relative w-full flex items-center pl-9 pr-2 py-1.5 text-sm rounded-md mx-2 cursor-pointer transition-colors',
                        isActive('platform', 'all')
                          ? 'bg-[#1e1e1e] text-white'
                          : 'text-gray-400 hover:bg-[#1a1a1a] hover:text-gray-200',
                      ].join(' ')}
                    >
                      {isActive('platform', 'all') && accentBar()}
                      <Grid3X3 size={15} className="shrink-0 mr-2.5" />
                      <span className="flex-1 text-left truncate">{t('allPosts')}</span>
                      <span className="text-[11px] text-gray-500 tabular-nums shrink-0">
                        {formatCount(total)}
                      </span>
                      {/* Empty chevron slot so the count aligns with rows that have a chevron. */}
                      <span aria-hidden className="w-5 ml-1 shrink-0" />
                    </button>

                    {PLATFORM_STATS.map(({ id, label, key, Icon: PIcon }) => {
                      // Brand rows carry a verbatim `label`; the localized 'web' row
                      // resolves its label from a `sidebar` i18n key at render.
                      const platformLabel = key ? t(key) : label;
                      // Folder-tags belonging to this platform nest underneath it (e.g.
                      // Instagram saved folders), turning the platform row into a dropdown.
                      const children = collections.filter((c) => c.platform === id);
                      const platformOpen = expandedPlatforms[id] !== false; // default expanded
                      return (
                        <React.Fragment key={id}>
                          <div
                            className={[
                              'group relative w-full flex items-center pr-2 pl-2 py-1.5 text-sm rounded-md mx-2 cursor-pointer transition-colors',
                              isActive('platform', id)
                                ? 'bg-[#1e1e1e] text-white'
                                : 'text-gray-400 hover:bg-[#1a1a1a] hover:text-gray-200',
                            ].join(' ')}
                          >
                            {isActive('platform', id) && accentBar()}
                            {/* Disclosure chevron lives in the left gutter (w-7 = pl-9 indent),
                                so platform icons stay aligned with chevron-less rows. */}
                            <span className="w-7 shrink-0 flex justify-center">
                              {children.length > 0 && (
                                <button
                                  data-testid={`source-${id}-toggle`}
                                  aria-expanded={platformOpen}
                                  onClick={(e: React.MouseEvent<HTMLButtonElement>) => {
                                    e.stopPropagation();
                                    togglePlatform(id);
                                  }}
                                  title={platformOpen ? t('collapse') : t('expand')}
                                  className="u-press flex items-center justify-center w-5 h-5 rounded text-gray-500 hover:text-gray-200 hover:bg-[#262626] transition-colors"
                                >
                                  <ChevronDown
                                    size={13}
                                    className={[
                                      'transition-transform duration-200',
                                      platformOpen ? '' : '-rotate-90',
                                    ].join(' ')}
                                  />
                                </button>
                              )}
                            </span>
                            <button
                              data-testid={`source-${id}`}
                              onClick={() => onSelectSource?.({ type: 'platform', value: id })}
                              className="u-press flex-1 flex justify-between items-center min-w-0 text-left"
                            >
                              <span className="flex items-center gap-2.5 min-w-0">
                                <PIcon size={15} className="shrink-0" />
                                <span className="truncate">{platformLabel}</span>
                              </span>
                              <span className="text-[11px] text-gray-500 tabular-nums shrink-0 ml-2">
                                {formatCount(byPlatform[id] ?? 0)}
                              </span>
                            </button>
                            {/* Empty slot so counts stay column-aligned with rows that have one. */}
                            <span aria-hidden className="w-5 ml-1 shrink-0" />
                          </div>
                          {children.length > 0 && platformOpen && (
                            <div data-testid={`source-${id}-children`} className="flex flex-col">
                              {children.map((c, i) =>
                                renderCollectionRow(c, i, {
                                  nested: true,
                                  isLast: i === children.length - 1,
                                }),
                              )}
                            </div>
                          )}
                        </React.Fragment>
                      );
                    })}

                    {/* Custom sources: any collection not nested under a shown
                        platform row. Covers both platform-less folders (`!platform`)
                        and orphans whose `platform` is a value outside PLATFORM_STATS
                        (legacy/unknown ids), so no collection is ever invisible. */}
                    {collections.some((c) => !PLATFORM_STATS.some((p) => p.id === c.platform)) && (
                      <div data-testid="custom-sources" className="mt-1.5">
                        {collections
                          .filter((c) => !PLATFORM_STATS.some((p) => p.id === c.platform))
                          .map((c, i) => renderCollectionRow(c, i))}
                      </div>
                    )}

                    {/* Action row: opens the modal to create a custom source
                        (a "folder/label" for organising bookmarks). Not a view →
                        no active state. Reads lighter (gray-500) like the add
                        rows in Connections, with the same "+" affordance. */}
                    <button
                      data-testid="add-source-btn"
                      onClick={onAddCollection}
                      title={t('addCollection')}
                      className="u-press u-fade-in-down flex items-center gap-2 pl-9 pr-4 py-1.5 rounded-md mx-2 cursor-pointer text-sm text-gray-500 hover:bg-[#1a1a1a] hover:text-gray-200 transition-colors text-left"
                    >
                      <FolderPlus size={15} className="shrink-0" />
                      <span className="flex-1">{t('newFolder')}</span>
                    </button>
                  </div>
                )}
              </div>
            );
          })()}

          {/* ===================== AI ===================== */}
          {(() => {
            const open = expandedGroups.ai;
            return (
              <>
                <button
                  data-testid="nav-ai"
                  aria-expanded={open}
                  onClick={() => toggleGroup('ai')}
                  className="u-press flex items-center gap-3 px-4 py-2.5 rounded-md mx-2 mt-2 text-sm text-gray-400 hover:bg-[#1a1a1a] hover:text-gray-200 transition-colors text-left cursor-pointer"
                >
                  <Sparkles size={16} strokeWidth={1.75} className="shrink-0" />
                  <span className="flex-1">{t('ai')}</span>
                  {groupChevron(open)}
                </button>

                {open && (
                  <div className="flex flex-col gap-0.5 mb-0.5">
                    {AI_TABS.map((tab, i) => {
                      const subActive = currentView === tab.id;
                      const showAnalysisOnTab = tab.id === 'aiqueue' && analysisActive;
                      const showWebOnTab = tab.id === 'aiweb' && webActive;
                      return (
                        <button
                          key={tab.id}
                          data-testid={`nav-${tab.id}`}
                          aria-current={subActive ? 'page' : undefined}
                          onClick={() => onNavigate(tab.id)}
                          style={{ animationDelay: i * 30 + 'ms' }}
                          className={[
                            'u-press u-fade-in-down flex items-center gap-2 pl-11 pr-4 py-1.5 rounded-md mx-2 cursor-pointer text-sm transition-colors text-left',
                            subActive
                              ? 'bg-[#1e1e1e] text-white'
                              : 'text-gray-400 hover:bg-[#1a1a1a] hover:text-gray-200',
                          ].join(' ')}
                        >
                          <span className="flex-1">{t(tab.key)}</span>
                          {showAnalysisOnTab && (
                            <>
                              <Sparkles
                                size={14}
                                data-testid="nav-aiqueue-analyzing"
                                className="shrink-0 text-[#7B5CFF] animate-pulse"
                              />
                              <span
                                data-testid="nav-aiqueue-badge"
                                className="u-pop-in flex items-center justify-center min-w-[18px] h-[18px] px-1 rounded-full bg-[#7B5CFF] text-white text-[10px] font-semibold leading-none tabular-nums"
                              >
                                {analysisDone}/{analysisTotal}
                              </span>
                            </>
                          )}
                          {showWebOnTab && (
                            <>
                              <Globe
                                size={14}
                                data-testid="nav-aiweb-active"
                                className="shrink-0 text-[#7B5CFF] animate-pulse"
                              />
                              <span
                                data-testid="nav-aiweb-badge"
                                className="u-pop-in flex items-center justify-center min-w-[18px] h-[18px] px-1 rounded-full bg-[#7B5CFF] text-white text-[10px] font-semibold leading-none tabular-nums"
                              >
                                {webDone}/{webTotal}
                              </span>
                            </>
                          )}
                        </button>
                      );
                    })}
                  </div>
                )}
              </>
            );
          })()}
        </nav>

        {/* Spacer: pushes the footer actions to the bottom when there's spare room,
            but lets the whole column scroll once content overflows. */}
        <div className="flex-1 min-h-[12px]" />

        {/* Footer actions — same scroll flow, set apart by a divider. */}
        <div className="pt-2 pb-3 flex flex-col gap-0.5 border-t border-[#222]">
          {/* Feedback — apre un modal che invia una mail allo sviluppatore. */}
          <button
            data-testid="nav-feedback"
            onClick={() => setFeedbackOpen(true)}
            className="u-press flex items-center gap-3 px-4 py-2.5 rounded-md mx-2 cursor-pointer text-sm transition-colors text-left text-gray-400 hover:bg-[#1a1a1a] hover:text-gray-200"
          >
            <MessageSquare size={16} strokeWidth={1.75} className="shrink-0" />
            <span>{t('feedback')}</span>
          </button>

          {/* Activity center — aggregates every background task + recent events. */}
          <ActivityCenter onAction={onActivityAction} onNavigate={onNavigate} />

          <button
            data-testid="nav-settings"
            onClick={() => onNavigate('settings')}
            className={[
              'u-press flex items-center gap-3 px-4 py-2.5 rounded-md mx-2 cursor-pointer text-sm transition-colors text-left',
              currentView === 'settings'
                ? 'bg-[#1e1e1e] text-white'
                : 'text-gray-400 hover:bg-[#1a1a1a] hover:text-gray-200',
            ].join(' ')}
          >
            <Settings size={16} strokeWidth={1.75} className="shrink-0" />
            <span>{t('settings')}</span>
          </button>
        </div>
      </div>

      {feedbackOpen && <FeedbackModal onClose={() => setFeedbackOpen(false)} />}
    </aside>
  );
}

export default React.memo(Sidebar);
