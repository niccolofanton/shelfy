import React, { useState } from 'react';
import {
  Image as ImageIcon,
  Lightbulb,
  Film,
  Sparkles,
  Quote,
  ExternalLink,
  Loader2,
} from 'lucide-react';
import { useT } from '../../../i18n';
import { assetThumbUrl, assetUrl } from '../../../lib/asset';
import type { AiJobView, SiteView } from '../model';
import { catalogFacets, formatDuration, streamPreview } from '../model';
import { useVocab } from '../vocab';
import { ACCENT, BlockTitle, Muted, TagChip } from '../ui';

// Overview: the hero (or the scroll recording), the AI catalog in designer
// terms — summary, description, notable details, what to borrow — and every
// catalog facet as a chip that filters the library by that value.

const DESIGN_FACETS = new Set(['font', 'tech', 'color', 'fontClass', 'scheme', 'award']);

interface OverviewTabProps {
  site: SiteView;
  aiJob: AiJobView | null;
  modelReady: boolean;
  onApplyFacet: (facet: string, value: string) => void;
  onAnalyse: () => void;
  onOpenHero: () => void;
}

export default function OverviewTab({
  site,
  aiJob,
  modelReady,
  onApplyFacet,
  onAnalyse,
  onOpenHero,
}: OverviewTabProps): React.ReactElement {
  const t = useT('aiWebsites');
  const vocab = useVocab();
  const video = site.meta.video;
  const [mode, setMode] = useState<'shot' | 'video'>('shot');
  const ai = site.ai;
  const aiRunning = !!aiJob && ['pending', 'extracting', 'analyzing'].includes(aiJob.status);
  const preview = aiRunning && aiJob ? streamPreview(aiJob.streamText) : null;
  const facets = catalogFacets(ai).filter(([f]) => !DESIGN_FACETS.has(f));
  const summary = ai?.summary || '';
  const description = ai?.description || site.legacy.description;
  const reuse = ai?.referenceFor?.length
    ? ai.referenceFor
    : site.legacy.saveReason
      ? [site.legacy.saveReason]
      : [];
  const notable = ai?.notableDetails || [];
  const hasAnyAi = !!(ai || site.legacy.description || site.legacy.tags.length);

  return (
    <div className="flex flex-col gap-8" data-testid="aiweb-tab-overview">
      {/* ── Hero / scroll recording ─────────────────────────────────────── */}
      <div className="flex flex-col gap-3">
        {video && (
          <div
            role="tablist"
            aria-label={t('heroModeLabel')}
            className="self-start inline-flex items-center gap-0.5 rounded-lg bg-[#171717] border border-[#262626] p-0.5"
          >
            <button
              type="button"
              role="tab"
              aria-selected={mode === 'shot'}
              onClick={() => setMode('shot')}
              className={`flex items-center gap-1.5 h-7 px-2.5 rounded-md text-[12px] u-press ${
                mode === 'shot' ? 'bg-[#2a2a2a] text-white' : 'text-[#9a9a9a] hover:text-white'
              }`}
            >
              <ImageIcon size={13} /> {t('heroModeShot')}
            </button>
            <button
              type="button"
              role="tab"
              data-testid="aiweb-video-tab"
              aria-selected={mode === 'video'}
              onClick={() => setMode('video')}
              className={`flex items-center gap-1.5 h-7 px-2.5 rounded-md text-[12px] u-press ${
                mode === 'video' ? 'bg-[#2a2a2a] text-white' : 'text-[#9a9a9a] hover:text-white'
              }`}
            >
              <Film size={13} /> {t('heroModeVideo')}
              {video.duration > 0 && (
                <span className="tabular-nums text-[#777]">{formatDuration(video.duration)}</span>
              )}
            </button>
          </div>
        )}
        <div
          className="relative w-full overflow-hidden rounded-xl bg-[#121212] u-clip-aa"
          style={{ aspectRatio: '16 / 10' }}
        >
          {mode === 'video' && video ? (
            <video
              key={video.path}
              data-testid="aiweb-video"
              src={assetUrl(video.path) ?? undefined}
              poster={
                (video.poster
                  ? assetUrl(video.poster)
                  : site.cover
                    ? assetThumbUrl(site.cover.path, 1024)
                    : null) ?? undefined
              }
              controls
              autoPlay
              muted
              playsInline
              preload="metadata"
              className="absolute inset-0 w-full h-full object-contain bg-black"
            />
          ) : site.cover ? (
            <button
              type="button"
              onClick={onOpenHero}
              className="absolute inset-0 cursor-zoom-in"
              aria-label={t('openFullPage')}
            >
              <img
                src={assetUrl(site.cover.path) ?? undefined}
                alt=""
                decoding="async"
                draggable={false}
                className="w-full h-full object-cover object-top"
              />
            </button>
          ) : (
            <div className="absolute inset-0 flex items-center justify-center text-[#4a4a4a]">
              <ImageIcon size={32} strokeWidth={1.25} />
            </div>
          )}
          <span
            aria-hidden
            className="pointer-events-none absolute inset-0 rounded-xl"
            style={{ boxShadow: 'inset 0 0 0 1px rgba(255,255,255,0.07)' }}
          />
        </div>
      </div>

      {/* ── Streaming catalog (AI at work right now) ───────────────────── */}
      {aiRunning && (
        <div
          data-testid="aiweb-ai-stream"
          className="rounded-xl border border-[#7B5CFF]/30 bg-[#7B5CFF]/[0.06] px-4 py-3 flex flex-col gap-1.5"
        >
          <span
            className="flex items-center gap-1.5 text-[12px] font-medium"
            style={{ color: '#a593ff' }}
          >
            <Loader2 size={12} className="u-spin" /> {t('aiWorking')}
            {aiJob?.model && <span className="text-[#7d70b8] font-normal">· {aiJob.model}</span>}
          </span>
          {preview?.text ? (
            <p className="text-[12.5px] leading-relaxed text-[#c9c2ea]">
              <span className="text-[#8b74ff]">{t(`streamKey.${preview.key}`)} · </span>
              {preview.text}
              <span className="opacity-60">▋</span>
            </p>
          ) : (
            <p className="text-[12.5px] text-[#8f86b8]">
              {t(`aiStatus.${aiJob?.status || 'pending'}`)}
            </p>
          )}
        </div>
      )}

      <div className="grid grid-cols-1 xl:grid-cols-12 gap-8 xl:gap-10">
        {/* ── Left: the curator's write-up ────────────────────────────── */}
        <div className="xl:col-span-7 flex flex-col gap-7 min-w-0">
          {!hasAnyAi && !aiRunning && (
            <div className="rounded-xl border border-dashed border-[#2e2e2e] px-4 py-4 flex items-center gap-3">
              <Sparkles size={16} className="shrink-0 text-[#6b6b6b]" />
              <p className="flex-1 text-[12.5px] text-[#8a8a8a]">
                {modelReady ? t('aiMissing') : t('aiModelNotReady')}
              </p>
              {modelReady && (
                <button
                  type="button"
                  onClick={onAnalyse}
                  className="shrink-0 h-8 px-3 rounded-lg text-[12.5px] font-medium text-white u-press hover:brightness-110"
                  style={{ background: ACCENT }}
                >
                  {t('analyseNow')}
                </button>
              )}
            </div>
          )}

          {(summary || description) && (
            <div className="flex flex-col gap-3">
              {summary && (
                <p
                  className="text-[17px] leading-[1.55] text-[#f0f0f0] font-display"
                  data-testid="aiweb-summary"
                >
                  {summary}
                </p>
              )}
              {description && (
                <p className="text-[13.5px] leading-[1.7] text-[#a6a6a6] whitespace-pre-line">
                  {description}
                </p>
              )}
            </div>
          )}

          {reuse.length > 0 && (
            <div>
              <BlockTitle icon={Lightbulb}>{t('reuseTitle')}</BlockTitle>
              <ul className="flex flex-col gap-2" data-testid="aiweb-reuse">
                {reuse.map((r, i) => (
                  <li
                    key={i}
                    className="flex gap-3 rounded-lg bg-[#161616] border border-[#232323] px-3.5 py-2.5 text-[13px] leading-relaxed text-[#d4d4d4]"
                  >
                    <span
                      className="mt-[7px] shrink-0 w-1.5 h-1.5 rounded-full"
                      style={{ background: ACCENT }}
                    />
                    <span className="first-letter:uppercase">{r}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}

          {notable.length > 0 && (
            <div>
              <BlockTitle icon={Sparkles}>{t('notableTitle')}</BlockTitle>
              <ul className="flex flex-col gap-1.5">
                {notable.map((n, i) => (
                  <li key={i} className="flex gap-2.5 text-[13px] leading-relaxed text-[#bdbdbd]">
                    <span className="text-[#555] tabular-nums">
                      {String(i + 1).padStart(2, '0')}
                    </span>
                    <span>{n}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}

          {ai?.observations && (
            <details className="group rounded-lg border border-[#232323] bg-[#141414] px-3.5 py-2.5">
              <summary className="cursor-pointer list-none flex items-center gap-2 text-[12px] text-[#9a9a9a] hover:text-white">
                <Quote size={12} /> {t('observationsTitle')}
              </summary>
              <p className="mt-2 text-[12.5px] leading-relaxed text-[#9a9a9a]">{ai.observations}</p>
            </details>
          )}

          {!ai && site.legacy.tags.length > 0 && (
            <div>
              <BlockTitle>{t('legacyTags')}</BlockTitle>
              <div className="flex flex-wrap gap-1.5">
                {site.legacy.tags.map((tag) => (
                  <TagChip key={tag} size="xs">
                    {tag}
                  </TagChip>
                ))}
              </div>
            </div>
          )}
        </div>

        {/* ── Right: facts + clickable catalog ────────────────────────── */}
        <aside className="xl:col-span-5 flex flex-col gap-6 min-w-0">
          <dl className="grid grid-cols-[auto_1fr] gap-x-5 gap-y-2.5 rounded-xl bg-[#141414] border border-[#232323] px-4 py-3.5 text-[12.5px]">
            {ai?.craft && (
              <Fact label={vocab.facet('craft')}>
                <span className="inline-flex items-center gap-1.5">
                  <CraftMeter craft={ai.craft} />
                  {vocab.label('craft', ai.craft)}
                </span>
              </Fact>
            )}
            {ai?.audience && <Fact label={t('factAudience')}>{ai.audience}</Fact>}
            {!ai && site.legacy.contentType && (
              <Fact label={t('metaPurpose')}>{site.legacy.contentType}</Fact>
            )}
            {!ai && site.legacy.category && (
              <Fact label={t('metaSector')}>{site.legacy.category}</Fact>
            )}
            {(site.meta.lang || site.legacy.language) && (
              <Fact label={t('metaLanguage')}>
                {[
                  site.meta.lang || site.legacy.language,
                  ...site.meta.languages.filter((l) => l !== site.meta.lang),
                ]
                  .slice(0, 6)
                  .join(', ')}
              </Fact>
            )}
            {site.meta.scheme && (
              <Fact label={vocab.facet('scheme')}>{vocab.label('scheme', site.meta.scheme)}</Fact>
            )}
            <Fact label={t('factPages')}>{site.pages.length}</Fact>
          </dl>

          {facets.length > 0 && (
            <div className="flex flex-col gap-4" data-testid="aiweb-overview-facets">
              {facets.map(([facet, values]) => (
                <div key={facet}>
                  <div className="mb-1.5 text-[11px] text-[#6f6f6f]">{vocab.facet(facet)}</div>
                  <div className="flex flex-wrap gap-1.5">
                    {values.map((v) => (
                      <TagChip
                        key={v}
                        size="xs"
                        testId="aiweb-facet-chip"
                        title={t('applyFilterTitle')}
                        onClick={() => onApplyFacet(facet, v)}
                      >
                        {vocab.label(facet, v)}
                      </TagChip>
                    ))}
                  </div>
                </div>
              ))}
            </div>
          )}

          {site.meta.social.length > 0 && (
            <div>
              <div className="mb-1.5 text-[11px] text-[#6f6f6f]">{t('socialTitle')}</div>
              <div className="flex flex-wrap gap-1.5">
                {site.meta.social.map((s) => (
                  <TagChip
                    key={s.href}
                    size="xs"
                    icon={ExternalLink}
                    title={s.href}
                    onClick={() => window.electronAPI?.openExternal?.(s.href)}
                  >
                    {s.platform || s.href}
                  </TagChip>
                ))}
              </div>
            </div>
          )}

          {site.meta.credits.length > 0 && (
            <div>
              <div className="mb-1.5 text-[11px] text-[#6f6f6f]">{t('creditsTitle')}</div>
              <ul className="flex flex-col gap-1 text-[12.5px] text-[#bdbdbd]">
                {site.meta.credits.map((c, i) => (
                  <li key={i}>
                    {/^https?:\/\//.test(c.href) ? (
                      <button
                        type="button"
                        onClick={() => window.electronAPI?.openExternal?.(c.href)}
                        className="hover:text-white underline-offset-2 hover:underline"
                      >
                        {c.text}
                      </button>
                    ) : (
                      c.text
                    )}
                  </li>
                ))}
              </ul>
            </div>
          )}

          {!hasAnyAi && !facets.length && <Muted>{t('overviewNoCatalog')}</Muted>}
        </aside>
      </div>
    </div>
  );
}

function Fact({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}): React.ReactElement {
  return (
    <>
      <dt className="text-[#6f6f6f] whitespace-nowrap">{label}</dt>
      <dd className="text-[#dcdcdc] min-w-0">{children}</dd>
    </>
  );
}

const CRAFT_LEVEL: Record<string, number> = { template: 1, solid: 2, polished: 3, exceptional: 4 };
function CraftMeter({ craft }: { craft: string }): React.ReactElement {
  const level = CRAFT_LEVEL[craft] ?? 0;
  return (
    <span className="inline-flex items-center gap-[3px]" aria-hidden>
      {[1, 2, 3, 4].map((i) => (
        <span
          key={i}
          className="w-[5px] h-3 rounded-[2px]"
          style={{ background: i <= level ? ACCENT : '#2e2e2e' }}
        />
      ))}
    </span>
  );
}
