import React, { useState, useEffect, useRef } from 'react';
import { Search, SlidersHorizontal, X } from 'lucide-react';
import GridSizeControl from './GridSizeControl';
import { useT } from '../i18n';

// The Gallery's single browse-mode toolbar: search + active-tag chip + total
// post count + the grid zoom control + the "Filtri" drawer toggle. The source /
// platform selection and the media/download/AI-tag filters live in the
// right-hand <FilterDrawer> (opened by the Filtri button). Gallery-owned
// controls slot in via `leading` (sort toggle + active-collection chip, right
// after the search) and `trailing` (refresh + select, at the far right) so this
// component stays namespace-pure while the whole strip reads as one bar.
export default function FilterBar({
  filters,
  onChange: onFiltersChange,
  total,
  onToggleDrawer,
  drawerOpen,
  leading = null,
  trailing = null,
}) {
  const t = useT('filterBar');
  const [searchValue, setSearchValue] = useState(filters.search ?? '');

  // Mirror filters/onFiltersChange in a ref so the debounce effect can read the
  // latest values without depending on their (unstable) identities — otherwise
  // every parent render would reset the timer and defeat the debounce.
  const latestRef = useRef({ filters, onFiltersChange });
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

  return (
    <div className="relative w-full h-[52px]">
      <div className="flex items-center h-full px-4 py-2 gap-3 overflow-x-auto scrollbar-thin scrollbar-thumb-[#2e2e2e] scrollbar-track-transparent">
        {/* Search */}
        <div className="relative w-[280px] shrink-0">
          <Search
            size={14}
            className="absolute left-2.5 top-1/2 -translate-y-1/2 text-gray-500 pointer-events-none"
          />
          <input
            type="text"
            value={searchValue}
            onChange={(e) => setSearchValue(e.target.value)}
            placeholder={t('searchPlaceholder')}
            aria-label={t('searchAria')}
            className={`w-full bg-[#1a1a1a] border border-[#2e2e2e] rounded-md pl-7 ${searchValue ? 'pr-8' : 'pr-3'} py-1.5 text-sm text-gray-200 placeholder-gray-600 outline-none focus:border-[#7B5CFF] focus:shadow-[0_0_0_3px_rgba(123,92,255,0.18)] transition-[border-color,box-shadow] u-transition`}
          />
          {searchValue && (
            <button
              type="button"
              data-testid="search-clear"
              onClick={() => setSearchValue('')}
              title={t('clearSearch')}
              aria-label={t('clearSearch')}
              className="absolute right-2 top-1/2 -translate-y-1/2 flex items-center justify-center w-4 h-4 rounded-full text-gray-500 hover:text-white hover:bg-[#2e2e2e] transition-colors u-press"
            >
              <X size={12} />
            </button>
          )}
        </div>

        {/* Gallery-owned leading controls (sort toggle + active-collection chip) */}
        {leading}

        {/* Active tag chip (set from the AI panel) */}
        {filters.tag && (
          <div
            data-testid="tag-filter-chip"
            className="flex items-center gap-1.5 shrink-0 pl-3 pr-1.5 py-1 rounded-full bg-violet-500/15 text-violet-200 text-sm u-pop-in"
          >
            <span className="whitespace-nowrap">#{filters.tag}</span>
            <button
              onClick={() => onFiltersChange({ ...filters, tag: undefined })}
              title={t('removeTagFilter')}
              className="flex items-center justify-center w-4 h-4 rounded-full text-violet-200/80 hover:text-white hover:bg-violet-500/30 transition-colors u-press"
            >
              <X size={12} />
            </button>
          </div>
        )}

        <div className="flex-1" />

        {/* Post count — total only (the old "mostrando N" strip is gone) */}
        <span className="text-sm text-gray-500 shrink-0 tabular-nums whitespace-nowrap">
          {t('postsCount', { n: total.toLocaleString() })}
        </span>

        {/* Grid zoom (shared density preference, ⌘/Ctrl +/- shortcuts) */}
        <GridSizeControl className="shrink-0" />

        {/* Divider */}
        <div className="h-5 w-px bg-[#2e2e2e] shrink-0" />

        {/* Filters drawer toggle */}
        <button
          data-testid="filters-toggle"
          aria-expanded={!!drawerOpen}
          title={t('filtersTitle')}
          onClick={() => onToggleDrawer?.()}
          className={`relative flex items-center gap-1.5 whitespace-nowrap px-3 py-1.5 rounded-md text-sm cursor-pointer transition-colors u-press shrink-0 ${
            drawerOpen || activeCount > 0
              ? 'bg-[#2a2a2a] text-white'
              : 'bg-[#1a1a1a] text-gray-400 hover:bg-[#2a2a2a]'
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
  );
}
