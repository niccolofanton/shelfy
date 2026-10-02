import React, { useState } from 'react';
import { Check, ChevronDown, ChevronRight } from 'lucide-react';
import { useT } from '../../i18n';
import type { FacetCounts, FacetSelection } from './model';
import { FACET_ORDER } from './model';
import { useVocab } from './vocab';

// Left filter panel: every catalog facet with its value counts (getWebFacets).
// Multi-select inside a facet is an OR, facets combine with AND (the query
// contract of queryWebReferences). Counts are library-wide.

const VISIBLE = 6;
const OPEN_BY_DEFAULT = new Set(['siteType', 'industry', 'style', 'theme', 'colorMood', 'layout']);

interface FacetPanelProps {
  counts: FacetCounts;
  selection: FacetSelection;
  onToggle: (facet: string, value: string) => void;
}

export default function FacetPanel({
  counts,
  selection,
  onToggle,
}: FacetPanelProps): React.ReactElement {
  const t = useT('aiWebsites');
  const facets = FACET_ORDER.filter((f) => (counts[f] || []).length > 0);
  return (
    <nav
      data-testid="aiweb-facets"
      aria-label={t('filtersTitle')}
      className="h-full overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e] px-3 py-3 flex flex-col gap-1"
    >
      {facets.length === 0 && (
        <p className="px-2 py-2 text-[12px] leading-relaxed text-[#6b6b6b]">{t('filtersEmpty')}</p>
      )}
      {facets.map((f) => (
        <FacetGroup
          key={f}
          facet={f}
          values={counts[f] || []}
          selected={selection[f] || []}
          onToggle={onToggle}
        />
      ))}
    </nav>
  );
}

interface FacetGroupProps {
  facet: string;
  values: { value: string; count: number }[];
  selected: string[];
  onToggle: (facet: string, value: string) => void;
}

function FacetGroup({ facet, values, selected, onToggle }: FacetGroupProps): React.ReactElement {
  const t = useT('aiWebsites');
  const vocab = useVocab();
  const [open, setOpen] = useState<boolean>(OPEN_BY_DEFAULT.has(facet) || selected.length > 0);
  const [all, setAll] = useState(false);
  // Selected values stay visible even when they fall outside the top slice.
  const shown = all
    ? values
    : values.filter((v, i) => i < VISIBLE || selected.includes(v.value.toLowerCase()));
  return (
    <div className="flex flex-col" data-testid={`aiweb-facet-${facet}`}>
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        className="flex items-center gap-1.5 px-2 py-1.5 rounded-md text-left u-press hover:bg-[#1c1c1c]"
      >
        {open ? (
          <ChevronDown size={13} className="text-[#6b6b6b]" />
        ) : (
          <ChevronRight size={13} className="text-[#6b6b6b]" />
        )}
        <span className="text-[12px] font-semibold text-[#d6d6d6]">{vocab.facet(facet)}</span>
        {selected.length > 0 && (
          <span className="ml-auto rounded-full bg-[#7B5CFF] px-1.5 text-[10px] font-semibold leading-[16px] text-white tabular-nums">
            {selected.length}
          </span>
        )}
      </button>
      {open && (
        <div className="flex flex-col pb-2">
          {shown.map((v) => {
            const on = selected.includes(v.value.toLowerCase());
            return (
              <button
                key={v.value}
                type="button"
                role="checkbox"
                aria-checked={on}
                data-testid="aiweb-facet-value"
                onClick={() => onToggle(facet, v.value)}
                className={`group flex items-center gap-2 pl-[26px] pr-2 py-[5px] rounded-md text-left u-transition ${
                  on ? 'bg-[#7B5CFF]/10' : 'hover:bg-[#1a1a1a]'
                }`}
              >
                <span
                  aria-hidden
                  className="shrink-0 flex items-center justify-center w-3.5 h-3.5 rounded-[4px] u-transition"
                  style={{
                    background: on ? '#7B5CFF' : 'transparent',
                    boxShadow: on ? 'none' : 'inset 0 0 0 1.25px #4a4a4a',
                  }}
                >
                  {on && <Check size={10} color="#fff" strokeWidth={3.25} />}
                </span>
                <span
                  className={`flex-1 min-w-0 truncate text-[12px] ${on ? 'text-white' : 'text-[#a8a8a8] group-hover:text-[#e0e0e0]'}`}
                >
                  {vocab.label(facet, v.value)}
                </span>
                <span className="text-[10.5px] tabular-nums text-[#5c5c5c]">{v.count}</span>
              </button>
            );
          })}
          {values.length > VISIBLE && (
            <button
              type="button"
              onClick={() => setAll((a) => !a)}
              className="self-start ml-[26px] mt-0.5 text-[11px] text-[#8b74ff] hover:text-[#a593ff] u-press"
            >
              {all ? t('facetShowLess') : t('facetShowAll', { n: values.length })}
            </button>
          )}
        </div>
      )}
    </div>
  );
}
