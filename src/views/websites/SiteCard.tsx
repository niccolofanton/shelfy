import React, { memo, useEffect, useRef, useState } from 'react';
import { Check, Globe, Loader2, Play, ShieldAlert, AlertTriangle } from 'lucide-react';
import { useT } from '../../i18n';
import { assetThumbUrl, assetUrl } from '../../lib/asset';
import type { SiteView, WebJob } from './model';
import { ACTIVE_STATUSES, stripColors } from './model';
import { useVocab } from './vocab';
import { ACCENT, PaletteStrip, SiteFavicon } from './ui';

// One site in the library grid: the untouched first viewport (16:10), the
// site's identity (favicon, name, domain), its catalog type/industry and the
// palette strip. Hovering plays the short sped-up preview recording — the
// <video> only mounts on hover, so the grid never loads media up front.

interface SiteCardProps {
  site: SiteView;
  job?: WebJob | null;
  selectMode?: boolean;
  checked?: boolean;
  onOpen: (id: string) => void;
  onToggle?: (id: string, e: React.MouseEvent) => void;
}

const HOVER_DELAY_MS = 180;

function SiteCardImpl({
  site,
  job,
  selectMode = false,
  checked = false,
  onOpen,
  onToggle,
}: SiteCardProps): React.ReactElement {
  const t = useT('aiWebsites');
  const vocab = useVocab();
  const [hover, setHover] = useState(false);
  const [videoReady, setVideoReady] = useState(false);
  const [imgLoaded, setImgLoaded] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => clearTimeout(timer.current ?? undefined), []);

  const preview = site.meta.video?.preview || '';
  const colors = stripColors(site.palette);
  const tint = colors.find((c) => c !== colors[0]) || colors[0];
  const running = !!job && ACTIVE_STATUSES.has(job.status);
  const blocked = job?.status === 'blocked';
  const failed = job?.status === 'error';
  const siteType = site.ai?.siteType && site.ai.siteType !== 'other' ? site.ai.siteType : '';
  const industry = site.ai?.industry && site.ai.industry !== 'other' ? site.ai.industry : '';

  const enter = (): void => {
    if (!preview) return;
    clearTimeout(timer.current ?? undefined);
    timer.current = setTimeout(() => setHover(true), HOVER_DELAY_MS);
  };
  const leave = (): void => {
    clearTimeout(timer.current ?? undefined);
    setHover(false);
    setVideoReady(false);
  };

  const handleClick = (e: React.MouseEvent): void => {
    if (selectMode) onToggle?.(site.id, e);
    else onOpen(site.id);
  };

  return (
    <div
      data-testid="aiweb-card"
      data-site={site.domain}
      className="group flex flex-col gap-2.5 min-w-0 u-fade-in"
      onMouseEnter={enter}
      onMouseLeave={leave}
    >
      <button
        type="button"
        onClick={handleClick}
        aria-label={site.name}
        className="relative w-full overflow-hidden rounded-xl bg-[#161616] u-clip-aa text-left outline-none focus-visible:ring-2 focus-visible:ring-[#7B5CFF]"
        style={{ aspectRatio: '16 / 10' }}
      >
        {site.cover ? (
          <img
            src={assetThumbUrl(site.cover.path, 800) ?? undefined}
            alt=""
            loading="lazy"
            decoding="async"
            draggable={false}
            onLoad={() => setImgLoaded(true)}
            className="absolute inset-0 w-full h-full object-cover object-top transition-transform duration-500 ease-out group-hover:scale-[1.015]"
            style={{
              opacity: imgLoaded ? 1 : 0,
              transition: 'opacity 240ms ease-out, transform 500ms ease-out',
            }}
          />
        ) : (
          <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 text-[#4a4a4a]">
            <Globe size={28} strokeWidth={1.25} />
            <span className="text-[11px]">{site.domain}</span>
          </div>
        )}

        {hover && preview && (
          <video
            src={assetUrl(preview) ?? undefined}
            muted
            loop
            playsInline
            autoPlay
            preload="auto"
            onPlaying={() => setVideoReady(true)}
            className="absolute inset-0 w-full h-full object-cover object-top"
            style={{ opacity: videoReady ? 1 : 0, transition: 'opacity 200ms ease-out' }}
          />
        )}

        {/* Bottom scrim so the overlays stay legible on light heroes. */}
        {(running || blocked || failed) && (
          <div className="absolute inset-0 bg-gradient-to-t from-black/70 via-black/10 to-transparent" />
        )}

        {preview && !hover && !selectMode && (
          <span
            className="absolute right-2.5 bottom-2.5 flex items-center justify-center w-6 h-6 rounded-full bg-black/55 text-white opacity-0 group-hover:opacity-100 u-transition"
            aria-hidden
          >
            <Play size={11} fill="currentColor" />
          </span>
        )}

        {selectMode && (
          <span
            aria-hidden
            className="absolute left-2.5 top-2.5 flex items-center justify-center w-5 h-5 rounded-md u-transition"
            style={{
              background: checked ? ACCENT : 'rgba(0,0,0,0.45)',
              boxShadow: checked ? 'none' : 'inset 0 0 0 1.5px rgba(255,255,255,0.7)',
            }}
          >
            {checked && <Check size={13} color="#fff" strokeWidth={3} />}
          </span>
        )}

        {running && job && (
          <div
            className="absolute left-3 right-3 bottom-3 flex flex-col gap-1.5"
            data-testid="aiweb-card-progress"
          >
            <span className="flex items-center gap-1.5 text-[11px] font-medium text-white">
              <Loader2 size={11} className="u-spin" />
              {t(`status.${job.status}`)}
              <span className="ml-auto tabular-nums text-white/70">
                {Math.round(job.progress * 100)}%
              </span>
            </span>
            <span className="h-[3px] rounded-full bg-white/15 overflow-hidden">
              <span
                className="block h-full rounded-full u-progress"
                style={{ width: `${Math.max(3, job.progress * 100)}%`, background: ACCENT }}
              />
            </span>
          </div>
        )}
        {blocked && (
          <span className="absolute left-3 bottom-3 inline-flex items-center gap-1.5 rounded-full bg-[#f0b429] text-black px-2 py-0.5 text-[11px] font-semibold">
            <ShieldAlert size={12} /> {t('blockedBadge')}
          </span>
        )}
        {failed && (
          <span className="absolute left-3 bottom-3 inline-flex items-center gap-1.5 rounded-full bg-[#ef5350] text-white px-2 py-0.5 text-[11px] font-semibold">
            <AlertTriangle size={12} /> {t('status.error')}
          </span>
        )}
        {/* Hairline frame drawn INSIDE the box (the clip mask would cut an
            outer box-shadow). */}
        <span
          aria-hidden
          className="pointer-events-none absolute inset-0 rounded-xl"
          style={{
            boxShadow: checked
              ? `inset 0 0 0 2px ${ACCENT}`
              : 'inset 0 0 0 1px rgba(255,255,255,0.07)',
          }}
        />
      </button>

      <div className="flex items-center gap-2 min-w-0 px-0.5">
        <SiteFavicon path={site.favicon} name={site.name} tint={tint} size={18} />
        <div className="min-w-0 flex-1 flex items-baseline gap-1.5">
          <span className="truncate text-[13px] font-medium text-[#ededed]">{site.name}</span>
          {site.domain && site.domain.toLowerCase() !== site.name.toLowerCase() && (
            <span className="truncate text-[11.5px] text-[#6b6b6b]">{site.domain}</span>
          )}
        </div>
        <PaletteStrip colors={colors} size={11} />
      </div>

      {(siteType || industry) && (
        <div className="flex items-center gap-1 min-w-0 px-0.5 -mt-1 overflow-hidden">
          {siteType && (
            <span className="truncate rounded-md bg-[#1b1b1b] px-1.5 py-[1px] text-[10.5px] text-[#9a9a9a]">
              {vocab.label('siteType', siteType)}
            </span>
          )}
          {industry && (
            <span className="truncate rounded-md bg-[#1b1b1b] px-1.5 py-[1px] text-[10.5px] text-[#9a9a9a]">
              {vocab.label('industry', industry)}
            </span>
          )}
        </div>
      )}
    </div>
  );
}

const SiteCard = memo(SiteCardImpl);
export default SiteCard;
