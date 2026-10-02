import React, { useMemo, useState } from 'react';
import { useT } from '../../../i18n';
import { assetThumbUrl, assetUrl } from '../../../lib/asset';
import ImageLightbox from '../../../components/ImageLightbox';
import type { PageView, SectionRef } from '../model';
import { useVocab } from '../vocab';
import { Muted, TagChip } from '../ui';

// Sections: every detected section crop of every page, filterable by kind
// (hero, features, pricing, testimonials, …) — the Refero-style "show me how
// others did a pricing block" view.

const MAX_TILE_RATIO = 1.25; // height/width beyond which a tile is cropped

interface SectionsTabProps {
  pages: PageView[];
}

export default function SectionsTab({ pages }: SectionsTabProps): React.ReactElement {
  const t = useT('aiWebsites');
  const vocab = useVocab();
  const [kind, setKind] = useState<string | null>(null);
  const [open, setOpen] = useState<number | null>(null);

  const all = useMemo<SectionRef[]>(() => pages.flatMap((p) => p.sections), [pages]);
  const kinds = useMemo(() => {
    const m = new Map<string, number>();
    for (const s of all) m.set(s.kind, (m.get(s.kind) || 0) + 1);
    return [...m.entries()].sort((a, b) => b[1] - a[1]);
  }, [all]);
  const shown = kind ? all.filter((s) => s.kind === kind) : all;

  if (!all.length) {
    return <Muted>{pages.some((p) => p.jacked) ? t('sectionsJacked') : t('sectionsEmpty')}</Muted>;
  }

  return (
    <div className="flex flex-col gap-5" data-testid="aiweb-tab-sections">
      <div className="flex flex-wrap items-center gap-1.5">
        <TagChip active={kind === null} onClick={() => setKind(null)}>
          {t('sectionsAll')} <span className="opacity-60 tabular-nums">{all.length}</span>
        </TagChip>
        {kinds.map(([k, n]) => (
          <TagChip
            key={k}
            active={kind === k}
            onClick={() => setKind(kind === k ? null : k)}
            testId="aiweb-section-kind"
          >
            {vocab.label('section', k)} <span className="opacity-60 tabular-nums">{n}</span>
          </TagChip>
        ))}
      </div>

      <div className="columns-1 lg:columns-2 2xl:columns-3 gap-5">
        {shown.map((s, i) => {
          const ratio = s.width && s.height ? s.height / s.width : 0.6;
          const cropped = ratio > MAX_TILE_RATIO;
          const page = pages[s.pageIndex];
          return (
            <figure
              key={`${s.path}-${i}`}
              className="mb-5 break-inside-avoid"
              data-testid="aiweb-section"
            >
              <button
                type="button"
                onClick={() => setOpen(i)}
                className="relative block w-full overflow-hidden rounded-xl bg-[#141414] cursor-zoom-in u-clip-aa"
                style={{ aspectRatio: `1 / ${Math.min(ratio, MAX_TILE_RATIO)}` }}
              >
                <img
                  src={assetThumbUrl(s.path, 1024) ?? undefined}
                  alt=""
                  loading="lazy"
                  decoding="async"
                  draggable={false}
                  className="absolute inset-0 w-full h-full object-cover object-top"
                />
                {cropped && (
                  <span className="pointer-events-none absolute inset-x-0 bottom-0 h-16 bg-gradient-to-t from-black/60 to-transparent" />
                )}
                <span
                  aria-hidden
                  className="pointer-events-none absolute inset-0 rounded-xl"
                  style={{ boxShadow: 'inset 0 0 0 1px rgba(255,255,255,0.07)' }}
                />
              </button>
              <figcaption className="mt-2 flex items-baseline gap-2 min-w-0 px-0.5">
                <span className="shrink-0 text-[11px] font-medium text-[#a593ff]">
                  {vocab.label('section', s.kind)}
                </span>
                <span className="truncate text-[12px] text-[#bdbdbd]">{s.heading}</span>
                {page?.pageType && pages.length > 1 && (
                  <span className="ml-auto shrink-0 text-[11px] text-[#5f5f5f]">
                    {vocab.label('pageType', page.pageType)}
                  </span>
                )}
              </figcaption>
            </figure>
          );
        })}
      </div>

      {open !== null && (
        <ImageLightbox
          images={shown.map((s) => ({
            src: assetUrl(s.path) ?? '',
            label: [vocab.label('section', s.kind), s.heading].filter(Boolean).join(' — '),
            href: pages[s.pageIndex]?.url,
          }))}
          index={open}
          onClose={() => setOpen(null)}
          onIndexChange={setOpen}
        />
      )}
    </div>
  );
}
