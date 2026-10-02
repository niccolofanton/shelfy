import React, { useEffect, useRef, useState } from 'react';
import {
  ArrowLeft,
  ExternalLink,
  Maximize2,
  Trash2,
  Archive,
  ShieldAlert,
  Loader2,
} from 'lucide-react';
import { useT, useLang, localeTag } from '../../i18n';
import { assetUrl } from '../../lib/asset';
import ImageLightbox from '../../components/ImageLightbox';
import type { AiJobView, SiteView, WebJob } from './model';
import { ACTIVE_STATUSES, stripColors } from './model';
import { useVocab } from './vocab';
import { ACCENT, PaletteStrip, SiteFavicon } from './ui';
import OverviewTab from './detail/OverviewTab';
import PagesTab from './detail/PagesTab';
import SectionsTab from './detail/SectionsTab';
import DesignTab from './detail/DesignTab';
import SimilarTab from './detail/SimilarTab';
import CaptureTab from './detail/CaptureTab';
import type { VersionEntry } from './detail/CaptureTab';

// Full-panel detail of one reference, layered over the library grid (Esc or
// the back arrow returns to it). Tabs: Overview · Pages · Sections · Design ·
// Similar · Capture.

export type DetailTab = 'overview' | 'pages' | 'sections' | 'design' | 'similar' | 'capture';
const TABS: DetailTab[] = ['overview', 'pages', 'sections', 'design', 'similar', 'capture'];

interface SiteDetailProps {
  site: SiteView;
  job: WebJob | null;
  aiJob: AiJobView | null;
  modelReady: boolean;
  now: number;
  initialTab?: DetailTab;
  versions: VersionEntry[];
  activeVersionId: number | null;
  onSelectVersion: (id: number | null) => void;
  onDeleteSnapshot: (id: number) => void;
  onClose: () => void;
  onOpenSite: (id: string) => void;
  onApplyFacet: (facet: string, value: string) => void;
  onRecapture: () => void;
  onReanalyse: () => void;
  onDelete: () => void;
  onOpenPost?: () => void;
  onCancel: (key: string) => void;
  onRetry: (key: string) => void;
  onUnblock: (key: string) => void;
  onAiCancel: (key: string) => void;
  onAiRetry: (key: string) => void;
  unblocking: boolean;
}

export default function SiteDetail(props: SiteDetailProps): React.ReactElement {
  const {
    site,
    job,
    aiJob,
    modelReady,
    now,
    initialTab = 'overview',
    versions,
    activeVersionId,
    onClose,
    onOpenSite,
    onApplyFacet,
    onDelete,
    onOpenPost,
  } = props;
  const t = useT('aiWebsites');
  const tc = useT('common');
  const vocab = useVocab();
  const { lang } = useLang();
  const [tab, setTab] = useState<DetailTab>(initialTab);
  const [shot, setShot] = useState<number | null>(null);
  const scroller = useRef<HTMLDivElement | null>(null);
  const tabRefs = useRef<Partial<Record<DetailTab, HTMLButtonElement | null>>>({});

  // A different site (Similar → open) starts on its own first tab, at the top.
  useEffect(() => {
    setTab(initialTab);
    setShot(null);
    if (scroller.current) scroller.current.scrollTop = 0;
  }, [site.id, initialTab]);
  useEffect(() => {
    if (scroller.current) scroller.current.scrollTop = 0;
  }, [tab]);

  // Esc returns to the grid (the lightbox swallows its own Esc first).
  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      if (e.key !== 'Escape' || e.defaultPrevented) return;
      const tgt = e.target as HTMLElement | null;
      if (tgt && (tgt.tagName === 'INPUT' || tgt.tagName === 'TEXTAREA')) return;
      onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  const sectionsCount = site.pages.reduce((n, p) => n + p.sections.length, 0);
  const status = job?.status;
  const running = !!status && ACTIVE_STATUSES.has(status);
  const isSnapshot = activeVersionId !== null;
  const snapshotDate = (() => {
    const v = versions.find((x) => x.id === activeVersionId);
    if (!v?.capturedAt) return '';
    try {
      return new Date(v.capturedAt).toLocaleDateString(localeTag(lang), {
        day: '2-digit',
        month: 'short',
        year: 'numeric',
      });
    } catch {
      return '';
    }
  })();
  const colors = stripColors(site.palette, 6);
  const count: Partial<Record<DetailTab, number>> = {
    pages: site.pages.length,
    sections: sectionsCount,
  };

  const onTabKey = (e: React.KeyboardEvent): void => {
    if (e.key !== 'ArrowRight' && e.key !== 'ArrowLeft') return;
    e.preventDefault();
    const i = TABS.indexOf(tab);
    const next = TABS[(i + (e.key === 'ArrowRight' ? 1 : -1) + TABS.length) % TABS.length];
    setTab(next);
    tabRefs.current[next]?.focus();
  };

  return (
    <div
      data-testid="aiweb-detail"
      role="dialog"
      aria-modal="false"
      aria-label={site.name}
      className="absolute inset-0 z-30 flex flex-col bg-[#0f0f0f] u-fade-in"
    >
      {/* ── Header ─────────────────────────────────────────────────────── */}
      <header className="shrink-0 border-b border-[#212121] bg-[#0f0f0f]/95">
        <div className="flex items-center gap-3 px-6 pt-4 pb-3">
          <button
            type="button"
            data-testid="aiweb-detail-close"
            onClick={onClose}
            title={t('backToLibrary')}
            aria-label={t('backToLibrary')}
            className="flex items-center justify-center w-8 h-8 rounded-lg text-[#9a9a9a] hover:text-white hover:bg-[#1c1c1c] u-press"
          >
            <ArrowLeft size={17} />
          </button>
          <SiteFavicon path={site.favicon} name={site.name} tint={colors[1]} size={30} />
          <div className="min-w-0 flex flex-col">
            <div className="flex items-center gap-2 min-w-0">
              <h2
                className="truncate text-[19px] font-semibold text-white font-display"
                data-testid="aiweb-detail-title"
              >
                {site.name}
              </h2>
              {site.ai?.siteType && site.ai.siteType !== 'other' && (
                <span className="shrink-0 rounded-md bg-[#1d1d1d] px-1.5 py-0.5 text-[11px] text-[#a8a8a8]">
                  {vocab.label('siteType', site.ai.siteType)}
                </span>
              )}
              {site.ai?.industry && site.ai.industry !== 'other' && (
                <span className="shrink-0 rounded-md bg-[#1d1d1d] px-1.5 py-0.5 text-[11px] text-[#a8a8a8]">
                  {vocab.label('industry', site.ai.industry)}
                </span>
              )}
            </div>
            <button
              type="button"
              onClick={() => site.url && window.electronAPI?.openExternal?.(site.url)}
              className="self-start inline-flex items-center gap-1 text-[12px] text-[#7a7a7a] hover:text-white u-press"
              title={site.url}
            >
              {site.domain || site.url} <ExternalLink size={11} />
            </button>
          </div>
          <div className="ml-3 hidden lg:block">
            <PaletteStrip colors={colors} size={16} />
          </div>
          <div className="ml-auto flex items-center gap-1.5">
            {status === 'blocked' && (
              <button
                type="button"
                onClick={() => setTab('capture')}
                className="flex items-center gap-1.5 h-8 px-3 rounded-lg bg-[#f0b429] text-black text-[12.5px] font-semibold u-press"
              >
                <ShieldAlert size={13} /> {t('blockedBadge')}
              </button>
            )}
            {running && job && (
              <button
                type="button"
                onClick={() => setTab('capture')}
                className="flex items-center gap-1.5 h-8 px-3 rounded-lg bg-[#1b1630] text-[#c9bcff] text-[12.5px] u-press"
              >
                <Loader2 size={13} className="u-spin" /> {t(`status.${job.status}`)} ·{' '}
                <span className="tabular-nums">{Math.round(job.progress * 100)}%</span>
              </button>
            )}
            <button
              type="button"
              data-testid="aiweb-open-site"
              onClick={() => site.url && window.electronAPI?.openExternal?.(site.url)}
              className="flex items-center gap-1.5 h-8 px-3 rounded-lg text-white text-[12.5px] font-medium u-press hover:brightness-110"
              style={{ background: ACCENT }}
            >
              <ExternalLink size={13} /> {t('openSite')}
            </button>
            {onOpenPost && (
              <button
                type="button"
                onClick={onOpenPost}
                title={t('openReferenceTitle')}
                aria-label={t('openReferenceTitle')}
                className="flex items-center justify-center w-8 h-8 rounded-lg text-[#9a9a9a] hover:text-white hover:bg-[#1c1c1c] u-press"
              >
                <Maximize2 size={14} />
              </button>
            )}
            <button
              type="button"
              data-testid="aiweb-detail-delete"
              onClick={onDelete}
              title={tc('delete')}
              aria-label={tc('delete')}
              className="flex items-center justify-center w-8 h-8 rounded-lg text-[#9a9a9a] hover:text-[#ef5350] hover:bg-[#2a1515] u-press"
            >
              <Trash2 size={14} />
            </button>
          </div>
        </div>

        {isSnapshot && (
          <div className="mx-6 mb-3 flex items-center gap-2 rounded-lg bg-[#1a1a1a] border border-[#2a2a2a] px-3 py-1.5 text-[12px] text-[#bdbdbd]">
            <Archive size={13} className="text-[#8a8a8a]" />
            {t('viewingSnapshot', { date: snapshotDate })}
            <button
              type="button"
              onClick={() => props.onSelectVersion(null)}
              className="ml-auto text-[#a593ff] hover:text-white u-press"
            >
              {t('backToCurrent')}
            </button>
          </div>
        )}

        <div
          role="tablist"
          aria-label={site.name}
          className="flex items-center gap-1 px-5"
          onKeyDown={onTabKey}
        >
          {TABS.map((k) => {
            const active = tab === k;
            const n = count[k];
            const dot = k === 'capture' && (running || status === 'blocked' || status === 'error');
            return (
              <button
                key={k}
                ref={(el) => {
                  tabRefs.current[k] = el;
                }}
                type="button"
                role="tab"
                aria-selected={active}
                tabIndex={active ? 0 : -1}
                data-testid={`aiweb-tab-${k}-btn`}
                onClick={() => setTab(k)}
                className={`relative flex items-center gap-1.5 h-10 px-3 text-[13px] u-transition ${
                  active ? 'text-white' : 'text-[#8a8a8a] hover:text-[#d0d0d0]'
                }`}
              >
                {t(`tab.${k}`)}
                {typeof n === 'number' && n > 0 && (
                  <span className="text-[11px] tabular-nums text-[#5f5f5f]">{n}</span>
                )}
                {dot && (
                  <span
                    className="w-1.5 h-1.5 rounded-full"
                    style={{
                      background:
                        status === 'blocked' ? '#f0b429' : status === 'error' ? '#ef5350' : ACCENT,
                    }}
                  />
                )}
                {active && (
                  <span
                    className="absolute left-2 right-2 -bottom-px h-[2px] rounded-full"
                    style={{ background: ACCENT }}
                  />
                )}
              </button>
            );
          })}
        </div>
      </header>

      {/* ── Body ───────────────────────────────────────────────────────── */}
      <div
        ref={scroller}
        className="flex-1 min-h-0 overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e]"
      >
        <div className="mx-auto w-full max-w-[1240px] px-8 py-7" role="tabpanel">
          {tab === 'overview' && (
            <OverviewTab
              site={site}
              aiJob={aiJob}
              modelReady={modelReady}
              onApplyFacet={onApplyFacet}
              onAnalyse={props.onReanalyse}
              onOpenHero={() => site.pages.length && setShot(0)}
            />
          )}
          {tab === 'pages' && <PagesTab pages={site.pages} onOpenPage={setShot} />}
          {tab === 'sections' && <SectionsTab pages={site.pages} />}
          {tab === 'design' && <DesignTab site={site} onApplyFacet={onApplyFacet} />}
          {tab === 'similar' && <SimilarTab id={site.id} onOpen={onOpenSite} />}
          {tab === 'capture' && (
            <CaptureTab
              site={site}
              job={job}
              aiJob={aiJob}
              modelReady={modelReady}
              now={now}
              versions={versions}
              activeVersionId={activeVersionId}
              onSelectVersion={props.onSelectVersion}
              onDeleteSnapshot={props.onDeleteSnapshot}
              onRecapture={props.onRecapture}
              onReanalyse={props.onReanalyse}
              onCancel={props.onCancel}
              onRetry={props.onRetry}
              onUnblock={props.onUnblock}
              onAiCancel={props.onAiCancel}
              onAiRetry={props.onAiRetry}
              unblocking={props.unblocking}
            />
          )}
        </div>
      </div>

      {shot !== null && site.pages.length > 0 && (
        <ImageLightbox
          images={site.pages.map((p) => ({
            src: assetUrl(p.hero?.path || p.chunks[0]?.path || null) ?? '',
            // The whole page as stacked, lazy bands (falls back to the hero).
            chunks: p.chunks.length
              ? p.chunks.map((c) => assetUrl(c.path)).filter((c): c is string => Boolean(c))
              : undefined,
            label: `${p.pageType ? `${vocab.label('pageType', p.pageType)} · ` : ''}${site.domain}`,
            href: p.url,
          }))}
          index={shot}
          onClose={() => setShot(null)}
          onIndexChange={setShot}
        />
      )}
    </div>
  );
}
