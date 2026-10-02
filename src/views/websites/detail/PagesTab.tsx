import React, { useEffect, useRef, useState } from 'react';
import { ExternalLink, FileText, AlertTriangle, Film, Scissors } from 'lucide-react';
import { useT } from '../../../i18n';
import { assetThumbUrl, assetUrl } from '../../../lib/asset';
import type { PageView } from '../model';
import { pathOf } from '../model';
import { useVocab } from '../vocab';
import { BlockTitle, LazyShot, Muted } from '../ui';

// Pages: the captured pages in a rail; the selected page shows its first
// viewport, the whole page re-assembled from its lazily-loaded bands (a
// filmstrip for scroll-jacked experiences) and the footer.

interface PagesTabProps {
  pages: PageView[];
  onOpenPage: (index: number) => void;
}

export default function PagesTab({ pages, onOpenPage }: PagesTabProps): React.ReactElement {
  const t = useT('aiWebsites');
  const vocab = useVocab();
  const [index, setIndex] = useState(0);
  const scrollTop = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (index >= pages.length) setIndex(0);
  }, [pages.length, index]);

  if (!pages.length) return <Muted>{t('pagesEmpty')}</Muted>;
  const page = pages[Math.min(index, pages.length - 1)];

  return (
    <div className="flex gap-8 items-start" data-testid="aiweb-tab-pages">
      <ol className="sticky top-0 w-[220px] shrink-0 flex flex-col gap-1">
        {pages.map((p, i) => (
          <li key={`${p.url}-${i}`}>
            <button
              type="button"
              data-testid="aiweb-page-item"
              onClick={() => {
                setIndex(i);
                scrollTop.current?.scrollIntoView?.({ block: 'nearest' });
              }}
              aria-current={i === index}
              className={`w-full flex gap-2.5 rounded-lg p-1.5 text-left u-transition ${
                i === index ? 'bg-[#1f1f1f]' : 'hover:bg-[#171717]'
              }`}
            >
              <span
                className="shrink-0 w-[68px] rounded-md overflow-hidden bg-[#161616]"
                style={{
                  aspectRatio: '16 / 10',
                  boxShadow: 'inset 0 0 0 1px rgba(255,255,255,0.06)',
                }}
              >
                {p.hero && (
                  <img
                    src={assetThumbUrl(p.hero.path, 200) ?? undefined}
                    alt=""
                    loading="lazy"
                    className="w-full h-full object-cover object-top"
                  />
                )}
              </span>
              <span className="min-w-0 flex flex-col justify-center gap-0.5">
                <span
                  className={`text-[12px] font-medium truncate ${i === index ? 'text-white' : 'text-[#c8c8c8]'}`}
                >
                  {p.pageType
                    ? vocab.label('pageType', p.pageType)
                    : t('pageFallback', { n: i + 1 })}
                </span>
                <span className="text-[11px] text-[#6b6b6b] truncate">{pathOf(p.url)}</span>
              </span>
            </button>
          </li>
        ))}
      </ol>

      <div className="flex-1 min-w-0 flex flex-col gap-8" ref={scrollTop}>
        <header className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
          <h3 className="text-[15px] font-semibold text-white truncate max-w-full">
            {page.title || pathOf(page.url)}
          </h3>
          <button
            type="button"
            onClick={() => window.electronAPI?.openExternal?.(page.url)}
            className="inline-flex items-center gap-1 text-[12px] text-[#8a8a8a] hover:text-white u-press"
            title={page.url}
          >
            {pathOf(page.url)} <ExternalLink size={11} />
          </button>
          <div className="flex items-center gap-1.5 ml-auto text-[11px] text-[#8a8a8a]">
            {page.heightCss ? (
              <Badge>{t('pageHeight', { px: Math.round(page.heightCss) })}</Badge>
            ) : null}
            {page.jacked && (
              <Badge>
                <Film size={11} /> {t('pageJacked')}
              </Badge>
            )}
            {page.capped && (
              <Badge>
                <Scissors size={11} /> {t('pageCapped')}
              </Badge>
            )}
            {page.qcStatus && page.qcStatus !== 'ok' && (
              <Badge warn title={page.qcReason}>
                <AlertTriangle size={11} /> {vocab.label('qc', page.qcStatus)}
              </Badge>
            )}
          </div>
        </header>

        {page.h1 && (
          <p className="-mt-5 text-[12.5px] leading-relaxed text-[#8a8a8a] line-clamp-2">
            <span className="text-[#5f5f5f]">H1 · </span>
            {page.h1}
          </p>
        )}

        {page.hero && (
          <div>
            <BlockTitle>{t('pageHero')}</BlockTitle>
            <LazyShot
              src={assetUrl(page.hero.path)}
              width={page.hero.width}
              height={page.hero.height}
              className="rounded-xl"
              onClick={() => onOpenPage(index)}
            />
          </div>
        )}

        {page.chunks.length > 0 && (
          <div data-testid="aiweb-fullpage">
            <BlockTitle
              icon={FileText}
              count={page.chunks.length}
              right={
                <button
                  type="button"
                  onClick={() => onOpenPage(index)}
                  className="text-[11.5px] text-[#8b74ff] hover:text-[#a593ff] u-press"
                >
                  {t('openFullscreen')}
                </button>
              }
            >
              {page.jacked ? t('pageFilmstrip') : t('pageFull')}
            </BlockTitle>
            <div
              className={`rounded-xl overflow-hidden bg-[#121212] ${page.jacked ? 'flex flex-col gap-3 p-3' : ''}`}
              style={{ boxShadow: 'inset 0 0 0 1px rgba(255,255,255,0.06)' }}
            >
              {page.chunks.map((c, i) => (
                <LazyShot
                  key={`${c.path}-${i}`}
                  src={assetUrl(c.path)}
                  width={c.width}
                  height={c.height}
                  className={page.jacked ? 'rounded-lg' : ''}
                />
              ))}
            </div>
          </div>
        )}

        {page.footer && (
          <div>
            <BlockTitle>{t('pageFooter')}</BlockTitle>
            <LazyShot
              src={assetUrl(page.footer.path)}
              width={page.footer.width}
              height={page.footer.height}
              className="rounded-xl"
            />
          </div>
        )}

        {!page.hero && !page.chunks.length && <Muted>{t('pageNoShots')}</Muted>}
      </div>
    </div>
  );
}

function Badge({
  children,
  warn = false,
  title,
}: {
  children: React.ReactNode;
  warn?: boolean;
  title?: string;
}): React.ReactElement {
  return (
    <span
      title={title}
      className={`inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 ${
        warn ? 'bg-[#3a2a0a] text-[#f0b429]' : 'bg-[#1c1c1c] text-[#9a9a9a]'
      }`}
    >
      {children}
    </span>
  );
}
