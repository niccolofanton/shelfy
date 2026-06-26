import React, { useState, useEffect, useRef } from 'react';
import { Search, SlidersHorizontal, X } from 'lucide-react';
import GridSizeControl from './GridSizeControl';
import { useT } from '../i18n';

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
}

// The Gallery's single browse-mode toolbar: search + active-tag chip + total
// post count + the grid zoom control + the "Filtri" drawer toggle. The source /
// platform selection and the media/download/AI-tag filters live in the
// right-hand <FilterDrawer> (opened by the Filtri button). Gallery-owned
// controls slot in via `leading` (sort toggle + active-collection chip, right
// after the search) and `trailing` (refresh + select, at the far right) so this
// component stays namespace-pure while the whole strip reads as one bar.
export default function FilterBar<F extends FilterBarFilters>({
  filters,
  onChange: onFiltersChange,
  total,
  onToggleDrawer,
  drawerOpen,
  leading = null,
  trailing = null,
}: FilterBarProps<F>): React.JSX.Element {
  const t: Translate = useT('filterBar');
  const [searchValue, setSearchValue] = useState<string>(filters.search ?? '');

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

  const mediaType = filters.mediaType ?? 'all';
  const downloadStatus = filters.downloadStatus ?? 'all';
  const aiTagged = filters.aiTagged ?? 'all';
  // Sort lives in the `leading` slot (Gallery-owned), not in the filters drawer.
  const activeCount =
    (mediaType !== 'all' ? 1 : 0) +
    (downloadStatus !== 'all' ? 1 : 0) +
    (aiTagged !== 'all' ? 1 : 0);

  // Apple-Maps-style floating control "island": a translucent, blurred, rounded
  // capsule with a hairline ring + soft shadow. There is NO toolbar background —
  // each group floats over the grid as its own pill, so the posts show through the
  // gaps. `pointer-events-auto` re-enables interaction (the header strip that
  // hosts the pills is pointer-events-none so clicks in the gaps reach the grid).
  const PILL =
    'pointer-events-auto flex items-center rounded-full bg-[#1c1c1e]/85 backdrop-blur-xl ring-1 ring-white/10 shadow-[0_6px_20px_-6px_rgba(0,0,0,0.6)]';

  return (
    // The row itself is pointer-events-none so the gaps between the floating pills
    // let clicks/scrolls reach the post grid behind them; each pill re-enables
    // pointer events. No background — this is just the layout for the islands.
    <div className="relative w-full h-[52px]">
      <div className="pointer-events-none flex items-center h-full px-3 gap-2">
        {/* ── Pill 1 · Search ─────────────────────────────────────────────── */}
        <div className={`${PILL} h-9 pl-3 pr-1.5 w-[300px] max-w-[30vw] min-w-[150px]`}>
          <Search size={15} className="text-gray-400 shrink-0 pointer-events-none" />
          <input
            type="text"
            value={searchValue}
            onChange={(e: React.ChangeEvent<HTMLInputElement>) => setSearchValue(e.target.value)}
            placeholder={t('searchPlaceholder')}
            aria-label={t('searchAria')}
            className="flex-1 min-w-0 bg-transparent border-0 outline-none px-2 text-sm text-gray-100 placeholder-gray-500"
          />
          {searchValue && (
            <button
              type="button"
              data-testid="search-clear"
              onClick={() => setSearchValue('')}
              title={t('clearSearch')}
              aria-label={t('clearSearch')}
              className="flex items-center justify-center w-5 h-5 rounded-full text-gray-400 hover:text-white hover:bg-white/10 transition-colors u-press shrink-0"
            >
              <X size={12} />
            </button>
          )}
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
          <button
            data-testid="filters-toggle"
            aria-expanded={!!drawerOpen}
            title={t('filtersTitle')}
            onClick={() => onToggleDrawer?.()}
            className={`relative flex items-center gap-1.5 whitespace-nowrap px-3 py-1.5 rounded-full text-sm cursor-pointer transition-colors u-press shrink-0 ${
              drawerOpen || activeCount > 0
                ? 'bg-white/15 text-white'
                : 'text-gray-300 hover:bg-white/10'
            }`}
          >
            <SlidersHorizontal size={14} />
            {t('filters')}
            {activeCount > 0 && (
              <span className="flex items-center justify-center min-w-[16px] h-4 px-1 rounded-full bg-[#7B5CFF] text-white text-[10px] font-medium tabular-nums u-pop-in">
                {activeCount}
              </span>
            )}
          </button>

          {/* Gallery-owned trailing controls (refresh + select) */}
          {trailing}
        </div>
      </div>
    </div>
  );
}
