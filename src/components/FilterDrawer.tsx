import React from 'react';
import {
  X,
  RotateCcw,
  Film,
  HardDrive,
  Sparkles,
  Bookmark,
  Grid3X3,
  Tag,
  LayoutTemplate,
  Activity,
  Eye,
} from 'lucide-react';
import { localeTag, useLang, useT } from '../i18n';
import type { LibraryFacet, LibraryFacetsApi } from '../api/facets';
import { useLibraryFacets } from '../hooks/useLibraryFacets';
import { useFailureText } from '../hooks/useFailureText';
import { useDialog } from '../hooks/useDialog';
import { NARROW_QUERY, useMediaQuery } from './ui/useMediaQuery';
import { categoryOptions, contentTypeOptions, aiStatusOptions } from '../lib/facetOptions';
import { PLATFORM_SOURCES } from '../lib/sourceList';

// Translator returned by useT — namespaced key + optional interpolation vars.
type Translate = (key: string, vars?: Record<string, string | number>) => string;

// Lucide-compatible icon component: the subset of props this drawer passes
// (size + className only). Covers both lucide-react icons and PinterestIcon.
type IconComponent = (props: { size?: number; className?: string }) => React.ReactNode;

// The slice of the Gallery filter state this drawer reads. The component is
// generic over the full filter object (F) so it hands the same shape back
// through onChange — extra Gallery-owned fields ride along untouched.
interface FilterDrawerFilters {
  mediaType?: string;
  downloadStatus?: string;
  aiTagged?: string;
  category?: string;
  contentType?: string;
  aiStatus?: string;
  aiLanguage?: string;
}

// A library source selection routed back up to App (same path as the sidebar).
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

interface SegmentedOption {
  value: string;
  label: string;
}

interface SegmentedProps {
  options: SegmentedOption[];
  value: string;
  onChange: (value: string) => void;
  cols: number;
}

interface SectionLabelProps {
  icon: IconComponent;
  children: React.ReactNode;
}

interface FilterDrawerProps<F extends FilterDrawerFilters> {
  open: boolean;
  onClose: () => void;
  filters: F;
  onChange: (filters: F) => void;
  collections?: Shelfy.Collection[];
  stats?: Shelfy.Stats | Record<string, never>;
  activeSource?: ActiveSource;
  onSelectSource?: (source: SourceSelection) => void;
  // AI status (pending/analyzing/done/error, §1.2 #13) is web-only: the
  // desktop's query builder only has the coarser analyzed/unanalyzed split
  // that `aiTagged` already covers. Default false (the desktop build).
  showAiStatus?: boolean;
  facetsApi?: LibraryFacetsApi;
  // The filtered post total (what the toolbar's count shows): the bottom
  // sheet's footer applies/closes with "Show N posts" (audit GAL-1).
  filteredTotal?: number;
  // The Gallery-owned View controls (view mode, sort, density, refresh) shown
  // in the sheet's "View" section on narrow, where they leave the toolbar
  // (audit GAL-1). Null on desktop, where they stay in the toolbar.
  viewControls?: React.ReactNode;
}

interface SelectOption {
  value: string;
  label: string;
}

interface FacetSelectProps {
  value: string;
  onChange: (value: string) => void;
  allLabel: string;
  options: SelectOption[];
  testId: string;
  ariaLabel: string;
}

// A single-value facet picker (category, content type, AI status): a native
// <select> reads better than a Segmented grid once there are a dozen-plus
// options, and needs no extra dependency.
function FacetSelect({
  value,
  onChange,
  allLabel,
  options,
  testId,
  ariaLabel,
}: FacetSelectProps): React.JSX.Element {
  return (
    <select
      data-testid={testId}
      aria-label={ariaLabel}
      value={value || ''}
      onChange={(e) => onChange(e.target.value)}
      className="w-full bg-secondary border border-subtle rounded-md px-2.5 py-2 text-sm narrow:text-base text-primary outline-none focus:border-strong transition-colors"
    >
      <option value="">{allLabel}</option>
      {options.map((opt) => (
        <option key={opt.value} value={opt.value}>
          {opt.label}
        </option>
      ))}
    </select>
  );
}

function formatCount(n: number | null | undefined): string {
  if (n == null) return '0';
  return n.toLocaleString();
}

// Compact segmented control (same look as the old Filtri popover): one bordered
// track with equal-width cells laid out on a grid so long labels get their cell.
function Segmented({ options, value, onChange, cols }: SegmentedProps): React.JSX.Element {
  return (
    <div
      className="grid gap-1 p-1 rounded-md bg-secondary border border-subtle"
      style={{ gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))` }}
    >
      {options.map((opt) => {
        const active = value === opt.value;
        return (
          <button
            key={opt.value}
            onClick={() => onChange(opt.value)}
            aria-pressed={active}
            className={[
              'u-press px-1.5 py-1.5 narrow:py-2 rounded text-xs font-medium text-center leading-tight transition-colors',
              active
                ? 'bg-accent text-white shadow-sm u-pop-in'
                : 'text-secondary hover:text-primary hover:bg-hover',
            ].join(' ')}
          >
            {opt.label}
          </button>
        );
      })}
    </div>
  );
}

function SectionLabel({ icon: Icon, children }: SectionLabelProps): React.JSX.Element {
  return (
    <div className="flex items-center gap-1.5 mb-2">
      <Icon size={12} className="text-muted shrink-0" />
      <span className="text-2xs font-semibold uppercase tracking-wider text-muted">{children}</span>
    </div>
  );
}

// One source/subfolder row's config (consumed by sourceRow below).
interface SourceRowOptions {
  active: boolean;
  onClick: () => void;
  icon?: React.ReactNode;
  dot?: string;
  label: string;
  count: number;
  nested?: boolean;
}

// Right-hand filters drawer that lives inside the Gallery page. Three
// presentations (audit GAL-5, §4): an inline push panel at ≥1280px, an overlay
// panel with a backdrop at 900–1279px (so it stops squashing the grid), and a
// bottom sheet under 900px (a drag handle, a "View" section, the Library source
// list, the facets, and a sticky [Reset] / [Show N posts] footer). The source
// mirror shows only on narrow, where the sidebar is a drawer (GAL-6 / O5);
// Industry and Site type show only for Websites (GAL-6 / O6).
export default function FilterDrawer<F extends FilterDrawerFilters>({
  open,
  onClose,
  filters,
  onChange: onFiltersChange,
  collections = [],
  stats = {},
  activeSource,
  onSelectSource,
  showAiStatus = false,
  facetsApi,
  filteredTotal = 0,
  viewControls = null,
}: FilterDrawerProps<F>): React.JSX.Element | null {
  const t: Translate = useT('filterDrawer');
  const { lang } = useLang();
  const failure = useFailureText();
  const live = useLibraryFacets(facetsApi, open);
  const narrow = useMediaQuery(NARROW_QUERY);
  // Below 1280px (but not narrow) the panel overlays the grid instead of
  // pushing it; narrow is the bottom sheet; ≥1280px is the inline push panel.
  const below1280 = useMediaQuery('(max-width: 1279px)');
  const sheet = narrow;
  const overlay = below1280 && !narrow;
  const push = !below1280;
  // Overlay and sheet are modal dialogs (focus trap, Escape, restore, inert);
  // the inline push panel is part of the page, so it is not.
  const dialogRef = useDialog<HTMLElement>({ open: open && (sheet || overlay), onClose });

  // Full media-type facet (§1.2 #13): every Shelfy.MediaType, not just the
  // four that fit a single-row Segmented control.
  const MEDIA_TYPE_OPTIONS: SegmentedOption[] = [
    { value: 'all', label: t('mediaAll') },
    { value: 'video', label: t('mediaVideo') },
    { value: 'image', label: t('mediaImage') },
    { value: 'images', label: t('mediaImages') },
    { value: 'carousel', label: t('mediaCarousel') },
    { value: 'text', label: t('mediaText') },
    { value: 'website', label: t('mediaWebsite') },
    { value: 'file', label: t('mediaFile') },
  ];

  const DOWNLOAD_OPTIONS: SegmentedOption[] = [
    { value: 'all', label: t('downloadAll') },
    { value: 'downloaded', label: t('downloadDownloaded') },
    { value: 'linkonly', label: t('downloadLinkOnly') },
  ];

  const AI_TAGS_OPTIONS: SegmentedOption[] = [
    { value: 'all', label: t('aiAll') },
    { value: 'tagged', label: t('aiTagged') },
    { value: 'untagged', label: t('aiUntagged') },
  ];

  // Desktop defaults also supply localized labels for known server values.
  // Web choices and their counts always come from the live facets API.
  const CATEGORY_OPTIONS: SelectOption[] = categoryOptions().map((o) => ({
    value: o.value,
    label: t(`facetCategory.${o.key}`),
  }));
  const CONTENT_TYPE_OPTIONS: SelectOption[] = contentTypeOptions().map((o) => ({
    value: o.value,
    label: t(`facetContentType.${o.key}`),
  }));
  const AI_STATUS_OPTIONS: SelectOption[] = aiStatusOptions().map((o) => ({
    value: o.value,
    label: t(`facetAiStatus.${o.key}`),
  }));
  const options = (
    values: LibraryFacet[] | undefined,
    selected: string | undefined,
    label: (value: string) => string,
  ): SelectOption[] => {
    const rows = values ?? [];
    const result = rows.map(({ value, count }) => ({
      value,
      label: `${label(value)} (${formatCount(count)})`,
    }));
    if (selected && !rows.some((row) => row.value === selected))
      result.push({ value: selected, label: `${label(selected)}${values ? ' (0)' : ''}` });
    return result;
  };
  const knownLabel = (value: string, staticOptions: SelectOption[]) =>
    staticOptions.find((option) => option.value === value)?.label ?? value;
  const languageLabel = (value: string): string => {
    try {
      return new Intl.DisplayNames([localeTag(lang)], { type: 'language' }).of(value) ?? value;
    } catch {
      return value;
    }
  };
  const categories = facetsApi
    ? options(live.facets?.category, filters.category, (value) =>
        knownLabel(value, CATEGORY_OPTIONS),
      )
    : CATEGORY_OPTIONS;
  const contentTypes = facetsApi
    ? options(live.facets?.contentType, filters.contentType, (value) =>
        knownLabel(value, CONTENT_TYPE_OPTIONS),
      )
    : CONTENT_TYPE_OPTIONS;
  const statuses = facetsApi
    ? options(live.facets?.status, filters.aiStatus, (value) =>
        value === 'none' ? t('facetAiStatus.none') : knownLabel(value, AI_STATUS_OPTIONS),
      )
    : AI_STATUS_OPTIONS;
  const languages = options(live.facets?.language, filters.aiLanguage, languageLabel);

  // Platform rows: the one list shared with the sidebar (§1.2 #13, P1-06
  // carry-over), so the drawer and the sidebar can never drift apart on
  // order, icons or ids. The 'web' row's label is non-brand and resolved
  // here (the drawer's own namespace), the same way Sidebar resolves it
  // from its own.
  const PLATFORM_ROWS = PLATFORM_SOURCES.map(({ id, label, key, Icon }) => ({
    id,
    label: key ? t(key) : (label ?? id),
    Icon: Icon as IconComponent,
  }));

  // `stats` is the full Shelfy.Stats once loaded, or {} before the first fetch.
  const statsValue: { total?: number; byPlatform?: Partial<Record<string, number>> } = stats;
  const total = statsValue.total ?? 0;
  const byPlatform: Partial<Record<string, number>> = statsValue.byPlatform ?? {};

  const mediaType = filters.mediaType ?? 'all';
  const downloadStatus = filters.downloadStatus ?? 'all';
  const aiTagged = filters.aiTagged ?? 'all';
  const category = filters.category ?? '';
  const contentType = filters.contentType ?? '';
  const aiStatus = filters.aiStatus ?? '';
  const activeCount =
    (mediaType !== 'all' ? 1 : 0) +
    (downloadStatus !== 'all' ? 1 : 0) +
    (aiTagged !== 'all' ? 1 : 0) +
    (category ? 1 : 0) +
    (contentType ? 1 : 0) +
    (showAiStatus && aiStatus ? 1 : 0) +
    (facetsApi && filters.aiLanguage ? 1 : 0);

  // The source mirror duplicates the sidebar tree, so show it only where the
  // sidebar is hidden — narrow (GAL-6 / O5). Industry and Site type apply only
  // to websites (GAL-6 / O6): the source is "Websites" or the media type is
  // Website.
  const showSourceMirror = narrow;
  const showWebFacets = !!facetsApi || activeSource?.value === 'web' || mediaType === 'website';

  const isActive = (type: ActiveSource['type'], value: string | number): boolean =>
    activeSource?.type === type && activeSource?.value === value;

  const customCollections = collections.filter((c) => !c.platform);

  const resetFilters = (): void =>
    onFiltersChange({
      ...filters,
      mediaType: 'all',
      downloadStatus: 'all',
      aiTagged: 'all',
      category: undefined,
      contentType: undefined,
      aiStatus: undefined,
      aiLanguage: undefined,
    });

  // One source/subfolder row. `nested` indents it (collections under a platform).
  const sourceRow = (
    key: string,
    { active, onClick, icon, dot, label, count, nested }: SourceRowOptions,
  ): React.JSX.Element => (
    <button
      key={key}
      data-testid={`drawer-source-${key}`}
      aria-current={active ? 'page' : undefined}
      onClick={onClick}
      className={[
        'u-press relative w-full flex items-center gap-2.5 pr-2 py-2 text-sm rounded-md cursor-pointer transition-colors',
        nested ? 'pl-9' : 'pl-3',
        active ? 'bg-hover text-primary' : 'text-secondary hover:bg-secondary hover:text-primary',
      ].join(' ')}
    >
      {active && (
        <span
          aria-hidden
          className="u-bar-in absolute left-0 inset-y-0 my-auto h-4 w-[3px] rounded-r-full"
          style={{ backgroundColor: dot || '#7B5CFF' }}
        />
      )}
      {dot ? (
        <span
          className="w-2 h-2 rounded-full shrink-0"
          style={{ backgroundColor: dot, boxShadow: active ? `0 0 0 3px ${dot}33` : 'none' }}
        />
      ) : (
        icon
      )}
      <span className="flex-1 truncate text-left">{label}</span>
      <span className="text-caption text-muted tabular-nums shrink-0">{formatCount(count)}</span>
    </button>
  );

  const sourceList = (
    <div className="flex flex-col gap-0.5">
      {sourceRow('all', {
        active: isActive('platform', 'all'),
        onClick: () => onSelectSource?.({ type: 'platform', value: 'all' }),
        icon: <Grid3X3 size={15} className="shrink-0 text-secondary" />,
        label: t('allPosts'),
        count: total,
      })}

      {PLATFORM_ROWS.map(({ id, label, Icon }) => {
        const children = collections.filter((c) => c.platform === id);
        return (
          <React.Fragment key={id}>
            {sourceRow(id, {
              active: isActive('platform', id),
              onClick: () => onSelectSource?.({ type: 'platform', value: id }),
              icon: <Icon size={15} className="shrink-0" />,
              label,
              count: byPlatform[id] ?? 0,
            })}
            {children.map((c) =>
              sourceRow(`c${c.id}`, {
                active: isActive('collection', c.id),
                onClick: () =>
                  onSelectSource?.({
                    type: 'collection',
                    value: c.id,
                    label: c.name,
                    color: c.color,
                  }),
                dot: c.color,
                label: c.name,
                count: c.count ?? 0,
                nested: true,
              }),
            )}
          </React.Fragment>
        );
      })}

      {customCollections.length > 0 && (
        <div className="mt-1.5 flex flex-col gap-0.5">
          {customCollections.map((c) =>
            sourceRow(`c${c.id}`, {
              active: isActive('collection', c.id),
              onClick: () =>
                onSelectSource?.({
                  type: 'collection',
                  value: c.id,
                  label: c.name,
                  color: c.color,
                }),
              dot: c.color,
              label: c.name,
              count: c.count ?? 0,
            }),
          )}
        </div>
      )}
    </div>
  );

  // The scrollable body: the same sections in every presentation, gated by
  // viewport (the View section and the source mirror are narrow-only) and by
  // the active source (the website facets).
  const body = (
    <>
      {/* View controls (narrow only): view mode, sort, density, refresh —
          moved out of the toolbar so it fits a phone (GAL-1). */}
      {sheet && viewControls && (
        <div data-testid="drawer-view">
          <SectionLabel icon={Eye}>{t('view')}</SectionLabel>
          {viewControls}
        </div>
      )}

      {/* Sources — a mirror of the sidebar, shown only where the sidebar is a
          drawer (narrow). Titled "Library", not "Bookmarks" (§3.7). */}
      {showSourceMirror && (
        <div data-testid="drawer-sources">
          <SectionLabel icon={Bookmark}>{t('library')}</SectionLabel>
          {sourceList}
        </div>
      )}

      <div data-testid="drawer-mediatype">
        <SectionLabel icon={Film}>{t('mediaType')}</SectionLabel>
        <Segmented
          cols={4}
          options={MEDIA_TYPE_OPTIONS}
          value={mediaType}
          onChange={(val) => onFiltersChange({ ...filters, mediaType: val })}
        />
      </div>

      <div data-testid="drawer-download">
        <SectionLabel icon={HardDrive}>{t('downloadStatus')}</SectionLabel>
        <Segmented
          cols={3}
          options={DOWNLOAD_OPTIONS}
          value={downloadStatus}
          onChange={(val) => onFiltersChange({ ...filters, downloadStatus: val })}
        />
      </div>

      <div data-testid="drawer-aitags">
        <SectionLabel icon={Sparkles}>{t('aiTags')}</SectionLabel>
        <Segmented
          cols={3}
          options={AI_TAGS_OPTIONS}
          value={aiTagged}
          onChange={(val) => onFiltersChange({ ...filters, aiTagged: val })}
        />
      </div>

      {/* Industry / Site type (GAL-6 / O6): only for Websites — today only web
          references carry either (electron/analyzer.ts's web catalog). Static
          lists: see src/lib/facetOptions.ts. */}
      {showWebFacets && (
        <>
          <div data-testid="drawer-category">
            <SectionLabel icon={Tag}>{t('category')}</SectionLabel>
            <FacetSelect
              testId="drawer-category-select"
              ariaLabel={t('category')}
              allLabel={t('categoryAll')}
              value={category}
              options={categories}
              onChange={(val) => onFiltersChange({ ...filters, category: val || undefined })}
            />
          </div>

          <div data-testid="drawer-contenttype">
            <SectionLabel icon={LayoutTemplate}>{t('contentType')}</SectionLabel>
            <FacetSelect
              testId="drawer-contenttype-select"
              ariaLabel={t('contentType')}
              allLabel={t('contentTypeAll')}
              value={contentType}
              options={contentTypes}
              onChange={(val) => onFiltersChange({ ...filters, contentType: val || undefined })}
            />
          </div>
        </>
      )}

      {/* AI status (web only, see showAiStatus's doc comment above). */}
      {showAiStatus && (
        <div data-testid="drawer-aistatus">
          <SectionLabel icon={Activity}>{t('aiStatus')}</SectionLabel>
          <FacetSelect
            testId="drawer-aistatus-select"
            ariaLabel={t('aiStatus')}
            allLabel={t('aiStatusAll')}
            value={aiStatus}
            options={statuses}
            onChange={(val) => onFiltersChange({ ...filters, aiStatus: val || undefined })}
          />
        </div>
      )}
      {facetsApi && (
        <>
          <div data-testid="drawer-language">
            <SectionLabel icon={Tag}>{t('language')}</SectionLabel>
            <FacetSelect
              testId="drawer-language-select"
              ariaLabel={t('language')}
              allLabel={t('languageAll')}
              value={filters.aiLanguage ?? ''}
              options={languages}
              onChange={(value) => onFiltersChange({ ...filters, aiLanguage: value || undefined })}
            />
          </div>
          <p className="text-xs text-muted">{t('facetCountsHelp')}</p>
          {live.loading && (
            <p role="status" className="text-xs text-muted">
              {t('facetsLoading')}
            </p>
          )}
          {live.error != null && (
            <div role="alert" className="text-xs text-red-400">
              {failure(live.error)}
              <button
                className="u-press ml-2 underline"
                onClick={() => {
                  void live.refresh();
                }}
              >
                {t('facetsRetry')}
              </button>
            </div>
          )}
        </>
      )}
    </>
  );

  // The header bar shared by the push and overlay presentations: title, the
  // reset shortcut (when any facet is set) and the close button.
  const headerBar = (
    <div className="flex items-center justify-between px-4 h-[52px] shrink-0 border-b border-subtle">
      <span className="text-sm font-semibold text-primary">{t('title')}</span>
      <div className="flex items-center gap-1">
        {activeCount > 0 && (
          <button
            data-testid="drawer-reset"
            onClick={resetFilters}
            className="u-press flex items-center gap-1 text-caption text-muted hover:text-primary transition-colors"
          >
            <RotateCcw size={12} />
            {t('reset')}
          </button>
        )}
        <button
          data-testid="drawer-close"
          onClick={onClose}
          title={t('closeTitle')}
          aria-label={t('closeTitle')}
          className="u-press flex items-center justify-center w-8 h-8 rounded-md text-muted hover:text-primary hover:bg-hover transition-colors"
        >
          <X size={16} />
        </button>
      </div>
    </div>
  );

  // ── Inline push panel (≥1280px): the width-animated track, unchanged ──────
  if (push) {
    return (
      <div
        className={`u-drawer-track shrink-0 h-full relative overflow-hidden${
          open ? '' : ' pointer-events-none'
        }`}
        style={{ width: open ? 280 : 0 }}
        aria-hidden={!open}
      >
        <aside
          data-testid="filter-drawer"
          className="absolute top-0 right-0 w-[280px] h-full bg-sidebar border-l border-subtle flex flex-col overflow-hidden"
        >
          {headerBar}
          <div className="flex-1 min-h-0 overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e] scrollbar-track-transparent p-3 space-y-4">
            {body}
          </div>
        </aside>
      </div>
    );
  }

  if (!open) return null;

  // ── Overlay panel (900–1279px): a right panel over the grid + a backdrop ──
  if (overlay) {
    return (
      <div
        className="fixed inset-0 z-drawer"
        onClick={(e) => {
          if (e.target === e.currentTarget) onClose();
        }}
      >
        <div
          data-testid="filter-drawer-backdrop"
          aria-hidden="true"
          className="u-backdrop-in pointer-events-none absolute inset-0 bg-black/50"
        />
        <aside
          ref={dialogRef as React.RefObject<HTMLElement>}
          role="dialog"
          aria-modal="true"
          aria-label={t('title')}
          data-testid="filter-drawer"
          className="u-fade-in absolute top-0 right-0 w-[min(360px,85vw)] h-full bg-sidebar border-l border-strong shadow-2xl flex flex-col overflow-hidden"
        >
          {headerBar}
          <div className="flex-1 min-h-0 overflow-y-auto overscroll-contain scrollbar-thin scrollbar-thumb-[#2e2e2e] scrollbar-track-transparent p-3 space-y-4">
            {body}
          </div>
        </aside>
      </div>
    );
  }

  // ── Bottom sheet (<900px): drag handle, scroll, sticky footer ─────────────
  return (
    <div
      className="fixed inset-0 z-drawer"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        data-testid="filter-drawer-backdrop"
        aria-hidden="true"
        className="u-backdrop-in pointer-events-none absolute inset-0 bg-black/50"
      />
      <aside
        ref={dialogRef as React.RefObject<HTMLElement>}
        role="dialog"
        aria-modal="true"
        aria-label={t('title')}
        data-testid="filter-drawer"
        className="u-sheet u-sheet-in absolute inset-x-0 bottom-0 flex max-h-[85dvh] flex-col rounded-t-xl border-t border-strong bg-elevated shadow-2xl"
        style={{ paddingBottom: 'env(safe-area-inset-bottom)' }}
      >
        <SheetHandle onClose={onClose} label={t('closeTitle')} />
        <div className="flex items-center justify-between px-4 pb-1 shrink-0">
          <span className="text-sm font-semibold text-primary">{t('title')}</span>
          <button
            data-testid="drawer-close"
            onClick={onClose}
            title={t('closeTitle')}
            aria-label={t('closeTitle')}
            className="u-press flex items-center justify-center w-11 h-11 -mr-2 rounded-md text-muted hover:text-primary hover:bg-hover transition-colors"
          >
            <X size={18} />
          </button>
        </div>
        <div className="flex-1 min-h-0 overflow-y-auto overscroll-contain px-4 pt-2 pb-4 space-y-5">
          {body}
        </div>
        <div className="shrink-0 flex items-center gap-2 border-t border-subtle px-4 py-3">
          <button
            data-testid="drawer-reset"
            onClick={resetFilters}
            disabled={activeCount === 0}
            className="u-press h-11 px-4 rounded-md border border-strong text-sm font-medium text-primary hover:bg-hover disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
          >
            {t('reset')}
          </button>
          <button
            data-testid="drawer-apply"
            onClick={onClose}
            className="u-press flex-1 h-11 px-4 rounded-md bg-accent-fill text-sm font-medium text-white hover:bg-accent-hover transition-colors"
          >
            {t('showPosts', { n: filteredTotal.toLocaleString(), count: filteredTotal })}
          </button>
        </div>
      </aside>
    </div>
  );
}

// The sheet's drag handle: tap or drag down past 80px to close (mirrors
// Popover's sheet), writing the finger's offset straight onto the panel.
const SHEET_DISMISS_PX = 80;
function SheetHandle({
  onClose,
  label,
}: {
  onClose: () => void;
  label: string;
}): React.JSX.Element {
  const drag = React.useRef<{ id: number; y: number; dy: number } | null>(null);
  const dragged = React.useRef(false);
  const panelOf = (el: HTMLElement): HTMLElement | null =>
    el.closest<HTMLElement>('[role="dialog"]');
  const settle = (el: HTMLElement): void => {
    const panel = panelOf(el);
    if (panel) {
      panel.style.transition = 'transform var(--dur-2) var(--ease-out)';
      panel.style.transform = '';
    }
  };
  return (
    <button
      type="button"
      data-testid="filter-sheet-handle"
      aria-label={label}
      className="flex h-7 w-full shrink-0 cursor-grab touch-none items-center justify-center"
      onPointerDown={(e) => {
        drag.current = { id: e.pointerId, y: e.clientY, dy: 0 };
        dragged.current = false;
        e.currentTarget.setPointerCapture?.(e.pointerId);
      }}
      onPointerMove={(e) => {
        const d = drag.current;
        if (!d || d.id !== e.pointerId) return;
        d.dy = Math.max(0, e.clientY - d.y);
        if (d.dy > 4) dragged.current = true;
        const panel = panelOf(e.currentTarget);
        if (panel) {
          panel.style.transition = 'none';
          panel.style.transform = d.dy > 0 ? `translateY(${d.dy}px)` : '';
        }
      }}
      onPointerUp={(e) => {
        const d = drag.current;
        drag.current = null;
        if (!d || d.id !== e.pointerId) return;
        if (d.dy > SHEET_DISMISS_PX) onClose();
        else settle(e.currentTarget);
      }}
      onPointerCancel={(e) => {
        drag.current = null;
        settle(e.currentTarget);
      }}
      onClick={() => {
        if (dragged.current) {
          dragged.current = false;
          return;
        }
        onClose();
      }}
    >
      <span aria-hidden="true" className="h-1 w-9 rounded-full bg-[#4a4a4a]" />
    </button>
  );
}
