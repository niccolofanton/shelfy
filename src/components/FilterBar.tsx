import React, { useState, useEffect, useRef } from 'react';
import { Search, SlidersHorizontal, X } from 'lucide-react';
import GridSizeControl from './GridSizeControl';
import { useT } from '../i18n';
import { NARROW_QUERY, useMediaQuery } from './ui/useMediaQuery';

// Translator returned by useT — namespaced key + optional interpolation vars.
type Translate = (key: string, vars?: Record<string, string | number>) => string;

// The slice of the Gallery filter state this bar reads. The component is generic
// over the full filter object (F) so it hands the same shape back through
// onChange — extra Gallery-owned fields ride along untouched in the spread.
interface FilterBarFilters {
  search?: string;
  mediaType?: string;
  downloadStatus?: string;
  aiTagged?: string;
  aiStatus?: string;
  category?: string;
  contentType?: string;
  tag?: string;
}

interface FilterBarProps<F extends FilterBarFilters> {
  filters: F;
  onChange: (filters: F) => void;
  total: number;
  onToggleDrawer?: () => void;
  drawerOpen?: boolean;
  leading?: React.ReactNode;
  trailing?: React.ReactNode;
  // The narrow (<900px) menu button (UX-1), rendered first in the row so the
  // top strip reads [menu] [search] [filters] [select]. Hidden at ≥900px by the
  // button itself; passed by Gallery so this component needn't import the shell.
  menuButton?: React.ReactNode;
  // Mirrors FilterDrawer's own flag, so the "Filtri" badge counts the same
  // active facets the drawer shows (AI status is web-only, see there).
  showAiStatus?: boolean;
}

// The Gallery's single browse-mode toolbar. At ≥900px: search + Gallery-owned
// leading controls (sort / active source) + total count + grid zoom + the
// "Filtri" toggle + Gallery-owned trailing controls (refresh / select). Under
// 900px the row collapses to what fits a phone (GAL audit GAL-1): [menu]
// [search, flex-1, h-11] [filters icon + count badge] [the trailing slot, a
// single Select icon]; the count, view mode, sort, density and refresh move
// into the filter sheet (FilterDrawer's "View" section and its "Show N posts"
// footer). The source / platform selection and the media/download/AI-tag
// filters live in the <FilterDrawer> the Filtri button opens.
export default function FilterBar<F extends FilterBarFilters>({
  filters,
  onChange: onFiltersChange,
  total,
  onToggleDrawer,
  drawerOpen,
  leading = null,
  trailing = null,
  menuButton = null,
  showAiStatus = false,
}: FilterBarProps<F>): React.JSX.Element {
  const t: Translate = useT('filterBar');
  const narrow = useMediaQuery(NARROW_QUERY);
  const [searchValue, setSearchValue] = useState<string>(filters.search ?? '');
  // On narrow, the search pill grows to the full row while it has focus so the
  // query and the on-screen keyboard have room (audit SH-11): the Filtri and
  // Select icons step aside until blur.
  const [searchFocused, setSearchFocused] = useState<boolean>(false);
  const inputRef = useRef<HTMLInputElement>(null);

  // Mirror filters/onFiltersChange in a ref so the debounce effect can read the
  // latest values without depending on their (unstable) identities — otherwise
  // every parent render would reset the timer and defeat the debounce.
  const latestRef = useRef<{
    filters: F;
    onFiltersChange: (filters: F) => void;
  }>({ filters, onFiltersChange });
  latestRef.current = { filters, onFiltersChange };

  useEffect(() => {
    setSearchValue(filters.search ?? '');
  }, [filters.search]);

  useEffect(() => {
    const timer = setTimeout(() => {
      const { filters: f, onFiltersChange: onChange } = latestRef.current;
      if (searchValue !== f.search) {
        onChange({ ...f, search: searchValue });
      }
    }, 300);
    return () => clearTimeout(timer);
  }, [searchValue]);

  // The BottomNav "Search" tab (SH-11) dispatches a cancelable
  // `shelfy:focus-search`: own it (focus the field, select its text) and
  // preventDefault so App doesn't also run its interim fallback focus.
  useEffect(() => {
    const onFocusSearch = (e: Event): void => {
      const input = inputRef.current;
      if (!input) return;
      e.preventDefault();
      input.focus();
      input.select();
    };
    window.addEventListener('shelfy:focus-search', onFocusSearch);
    return () => window.removeEventListener('shelfy:focus-search', onFocusSearch);
  }, []);

  const mediaType = filters.mediaType ?? 'all';
  const downloadStatus = filters.downloadStatus ?? 'all';
  const aiTagged = filters.aiTagged ?? 'all';
  // Sort lives in the `leading` slot (Gallery-owned), not in the filters drawer.
  const activeCount =
    (mediaType !== 'all' ? 1 : 0) +
    (downloadStatus !== 'all' ? 1 : 0) +
    (aiTagged !== 'all' ? 1 : 0) +
    (filters.category ? 1 : 0) +
    (filters.contentType ? 1 : 0) +
    (showAiStatus && filters.aiStatus ? 1 : 0);

  // Apple-Maps-style floating control "island": a translucent, blurred, rounded
  // capsule with a hairline ring + soft shadow. There is NO toolbar background —
  // each group floats over the grid as its own pill, so the posts show through the
  // gaps. `pointer-events-auto` re-enables interaction (the header strip that
  // hosts the pills is pointer-events-none so clicks in the gaps reach the grid).
  const PILL =
    'pointer-events-auto flex items-center rounded-full bg-[#1c1c1e]/85 backdrop-blur-xl ring-1 ring-white/10 shadow-pill';

  // The search field + its clear button; identical in both layouts apart from
  // the pill's own width/height. `bg-transparent` on the input is what the
  // global focus rule rings on its parent pill (index.css), so keep it.
  const searchField = (
    <>
      <Search size={15} className="text-gray-400 shrink-0 pointer-events-none" />
      <input
        ref={inputRef}
        type="search"
        value={searchValue}
        onChange={(e: React.ChangeEvent<HTMLInputElement>) => setSearchValue(e.target.value)}
        onFocus={() => setSearchFocused(true)}
        onBlur={() => setSearchFocused(false)}
        placeholder={t('searchPlaceholder')}
        aria-label={t('searchAria')}
        enterKeyHint="search"
        autoCapitalize="none"
        autoCorrect="off"
        spellCheck={false}
        className="flex-1 min-w-0 appearance-none bg-transparent border-0 outline-none px-2 text-sm narrow:text-base text-gray-100 placeholder-gray-500 [&::-webkit-search-cancel-button]:appearance-none"
      />
      {searchValue && (
        <button
          type="button"
          data-testid="search-clear"
          onClick={() => setSearchValue('')}
          title={t('clearSearch')}
          aria-label={t('clearSearch')}
          className="flex items-center justify-center w-5 h-5 rounded-full text-gray-400 hover:text-white hover:bg-white/10 transition-colors u-press shrink-0 narrow:u-hit"
        >
          <X size={12} />
        </button>
      )}
    </>
  );

  // The Filtri toggle (shared by both layouts): a labelled pill at ≥900px, an
  // icon-only 44px button under it, each with the active-count badge.
  const filtersButton = (
    <button
      data-testid="filters-toggle"
      aria-expanded={!!drawerOpen}
      aria-label={narrow ? t('filters') : undefined}
      title={t('filtersTitle')}
      onClick={() => onToggleDrawer?.()}
      className={
        narrow
          ? `${PILL} relative justify-center w-11 h-11 shrink-0 cursor-pointer u-press ${
              drawerOpen || activeCount > 0 ? 'text-white' : 'text-gray-300'
            }`
          : `relative flex items-center gap-1.5 whitespace-nowrap px-3 py-1.5 rounded-full text-sm cursor-pointer transition-colors u-press shrink-0 ${
              drawerOpen || activeCount > 0
                ? 'bg-white/15 text-white'
                : 'text-gray-300 hover:bg-white/10'
            }`
      }
    >
      <SlidersHorizontal size={narrow ? 18 : 14} />
      {!narrow && t('filters')}
      {activeCount > 0 && (
        <span
          className={`flex items-center justify-center min-w-[16px] h-4 px-1 rounded-full bg-accent text-white text-2xs font-medium tabular-nums u-pop-in ${
            narrow ? 'absolute -top-0.5 -right-0.5' : ''
          }`}
        >
          {activeCount}
        </span>
      )}
    </button>
  );

  if (narrow) {
    // Phone layout (audit GAL-1 / §4): one row that fits 390px. The search pill
    // takes the width; focusing it hides the trailing icons so the field (and
    // the keyboard it raises) get the whole row.
    return (
      <div className="relative w-full h-[52px]">
        <div className="pointer-events-none flex items-center h-full px-3 gap-2">
          {menuButton}
          <div className={`${PILL} h-11 min-w-0 flex-1 pl-3 pr-1.5`}>{searchField}</div>
          {!searchFocused && (
            <>
              {/* On narrow the `leading` slot carries only the active-folder chip
                (view mode, sort and density moved to the filter sheet). */}
              {leading}
              {filtersButton}
              {trailing}
            </>
          )}
        </div>
      </div>
    );
  }

  return (
    // The row itself is pointer-events-none so the gaps between the floating pills
    // let clicks/scrolls reach the post grid behind them; each pill re-enables
    // pointer events. No background — this is just the layout for the islands.
    <div className="relative w-full h-[52px]">
      {menuButton}
      <div className="pointer-events-none flex items-center h-full px-3 gap-2">
        {/* ── Pill 1 · Search ─────────────────────────────────────────────── */}
        <div className={`${PILL} h-9 pl-3 pr-1.5 w-[300px] max-w-[30vw] min-w-[150px]`}>
          {searchField}
        </div>

        {/* ── Pill 2 · View + order (Gallery-owned leading controls) ───────── */}
        {leading && <div className={`${PILL} h-9 px-1.5 gap-0.5 shrink-0`}>{leading}</div>}

        {/* Active tag chip (set from the AI panel) — its own floating chip */}
        {filters.tag && (
          <div
            data-testid="tag-filter-chip"
            className={`${PILL} h-9 pl-3 pr-1.5 gap-1.5 text-violet-200 text-sm u-pop-in shrink-0`}
          >
            <span className="whitespace-nowrap">#{filters.tag}</span>
            <button
              onClick={() => onFiltersChange({ ...filters, tag: undefined })}
              title={t('removeTagFilter')}
              className="flex items-center justify-center w-5 h-5 rounded-full text-violet-200/80 hover:text-white hover:bg-violet-500/30 transition-colors u-press"
            >
              <X size={12} />
            </button>
          </div>
        )}

        <div className="flex-1" />

        {/* ── Pill 3 · Count · zoom · Filters · refresh · select ──────────── */}
        <div className={`${PILL} h-9 pl-3 pr-1.5 gap-1.5 shrink-0`}>
          {/* Post count — total only (the old "mostrando N" strip is gone) */}
          <span className="text-sm text-gray-400 shrink-0 tabular-nums whitespace-nowrap">
            {t('postsCount', { n: total.toLocaleString() })}
          </span>

          {/* Grid zoom (shared density preference, ⌘/Ctrl +/- shortcuts) */}
          <GridSizeControl className="shrink-0" />

          {/* Divider */}
          <div className="h-5 w-px bg-white/10 shrink-0" />

          {/* Filters drawer toggle */}
          {filtersButton}

          {/* Gallery-owned trailing controls (refresh + select) */}
          {trailing}
        </div>
      </div>
    </div>
  );
}
