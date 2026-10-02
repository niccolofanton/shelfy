import React, { useEffect, useState } from 'react';
import { Loader2 } from 'lucide-react';
import { useT } from '../../../i18n';
import type { SiteView } from '../model';
import { toSiteView } from '../model';
import { useVocab } from '../vocab';
import SiteCard from '../SiteCard';
import { Muted } from '../ui';

// Similar: the closest references by shared catalog facets (+ palette
// proximity), with the values they share — so "why is this similar" is legible.

interface SimilarEntry {
  site: SiteView;
  score: number;
  shared: { facet: string; value: string }[];
}

interface SimilarTabProps {
  id: string;
  onOpen: (id: string) => void;
}

export default function SimilarTab({ id, onOpen }: SimilarTabProps): React.ReactElement {
  const t = useT('aiWebsites');
  const vocab = useVocab();
  const [items, setItems] = useState<SimilarEntry[] | null>(null);

  useEffect(() => {
    let alive = true;
    setItems(null);
    Promise.resolve(window.electronAPI?.getSimilarWebReferences?.(id, 12))
      .then((res) => {
        if (!alive) return;
        const list = Array.isArray(res) ? res : [];
        setItems(
          list
            .filter((r) => r && r.post && typeof r.post.id === 'string')
            .map((r) => ({
              site: toSiteView(r.post),
              score: typeof r.score === 'number' ? r.score : 0,
              // Prefer facet-qualified values (exact localized label); fall back
              // to bare values from older backends.
              shared: Array.isArray(r.sharedFacets)
                ? r.sharedFacets
                    .filter((s) => s && typeof s.facet === 'string' && typeof s.value === 'string')
                    .map((s) => ({ facet: s.facet, value: s.value }))
                : Array.isArray(r.shared)
                  ? r.shared
                      .filter((s): s is string => typeof s === 'string')
                      .map((value) => ({ facet: '', value }))
                  : [],
            })),
        );
      })
      .catch(() => alive && setItems([]));
    return () => {
      alive = false;
    };
  }, [id]);

  if (items === null) {
    return (
      <div className="flex items-center gap-2 text-[12.5px] text-[#6b6b6b]">
        <Loader2 size={14} className="u-spin" /> {t('similarLoading')}
      </div>
    );
  }
  if (!items.length) return <Muted>{t('similarEmpty')}</Muted>;

  return (
    <div
      data-testid="aiweb-tab-similar"
      className="grid gap-x-5 gap-y-7"
      style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(260px, 1fr))' }}
    >
      {items.map((it) => (
        <div key={it.site.id} className="flex flex-col gap-2 min-w-0" data-testid="aiweb-similar">
          <SiteCard site={it.site} onOpen={onOpen} />
          {it.shared.length > 0 && (
            <p className="px-0.5 text-[11px] leading-relaxed text-[#6f6f6f]">
              <span className="text-[#8a8a8a]">{t('similarShared')} </span>
              {it.shared
                .map((s) => (s.facet ? vocab.label(s.facet, s.value) : vocab.any(s.value)))
                .join(' · ')}
            </p>
          )}
        </div>
      ))}
    </div>
  );
}
