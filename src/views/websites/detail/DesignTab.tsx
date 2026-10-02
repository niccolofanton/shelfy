import React, { useEffect, useRef, useState } from 'react';
import { Award, Check, Cpu, Palette, Ruler, Type, Layers, ExternalLink } from 'lucide-react';
import { useT } from '../../../i18n';
import type { FontView, SiteView, Swatch, TechEntry } from '../model';
import { contrastGrade, fontStack, inkOn } from '../model';
import { useVocab } from '../vocab';
import { BlockTitle, Muted, TagChip } from '../ui';

// Design: the measured system — palette (names, roles, coverage) with the
// body-text contrast, type specimens + the type scale, the tech stack by
// category, layout/motion traits as facts, and awards.

type FilterBy = (facet: string, value: string) => (() => void) | undefined;

interface DesignTabProps {
  site: SiteView;
  onApplyFacet: (facet: string, value: string) => void;
}

export default function DesignTab({ site, onApplyFacet }: DesignTabProps): React.ReactElement {
  const t = useT('aiWebsites');
  const { palette, fonts, tech, awards, meta } = site;
  // Only values the catalog actually indexed are clickable (a filter on a value
  // that is not in post_facets would just empty the grid).
  const indexed = new Set(
    Object.entries(site.ai?.facets || {}).flatMap(([f, vs]) =>
      (Array.isArray(vs) ? vs : []).map((v) => `${f}:${String(v).toLowerCase()}`),
    ),
  );
  const filterBy = (facet: string, value: string): (() => void) | undefined =>
    indexed.has(`${facet}:${value.toLowerCase()}`) ? () => onApplyFacet(facet, value) : undefined;
  return (
    <div className="flex flex-col gap-10" data-testid="aiweb-tab-design">
      <PaletteBlock palette={palette} contrast={meta.contrast} filterBy={filterBy} />
      <TypeBlock
        fonts={fonts}
        scale={meta.typeScale}
        baseSize={meta.baseSize}
        ratio={meta.scaleRatio}
        filterBy={filterBy}
      />
      <TraitsBlock traits={meta.traits} />
      <TechBlock tech={tech} filterBy={filterBy} />
      {awards.length > 0 && (
        <section>
          <BlockTitle icon={Award} count={awards.length}>
            {t('sectionAwards')}
          </BlockTitle>
          <ul className="grid grid-cols-1 lg:grid-cols-2 gap-2">
            {awards.map((a, i) => (
              <li
                key={`${a.platform}-${i}`}
                className="flex items-center gap-3 rounded-xl bg-[#151515] border border-[#242424] px-3.5 py-2.5"
              >
                <Award size={16} className="shrink-0 text-[#f0b429]" />
                <div className="min-w-0 flex-1">
                  <div className="text-[13px] text-white">
                    {a.platform}
                    {a.level && <span className="text-[#a0a0a0]"> · {a.level}</span>}
                  </div>
                  {(a.evidence || a.date) && (
                    <div className="text-[11.5px] text-[#6f6f6f] truncate">
                      {[a.date, a.evidence].filter(Boolean).join(' · ')}
                    </div>
                  )}
                </div>
                {/^https?:\/\//.test(a.profileUrl) && (
                  <button
                    type="button"
                    onClick={() => window.electronAPI?.openExternal?.(a.profileUrl)}
                    title={t('openAwardProfile')}
                    aria-label={t('openAwardProfile')}
                    className="shrink-0 flex items-center justify-center w-7 h-7 rounded-md text-[#8a8a8a] hover:text-white hover:bg-[#222] u-press"
                  >
                    <ExternalLink size={13} />
                  </button>
                )}
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}

// ── Palette ─────────────────────────────────────────────────────────────────
function PaletteBlock({
  palette,
  contrast,
  filterBy,
}: {
  palette: Swatch[];
  contrast: SiteView['meta']['contrast'];
  filterBy: FilterBy;
}): React.ReactElement {
  const t = useT('aiWebsites');
  if (!palette.length && !contrast) {
    return (
      <section>
        <BlockTitle icon={Palette}>{t('sectionPalette')}</BlockTitle>
        <Muted>{t('paletteEmpty')}</Muted>
      </section>
    );
  }
  const covered = palette.filter((s) => (s.coverage ?? 0) > 0);
  const total = covered.reduce((n, s) => n + (s.coverage ?? 0), 0);
  const grade = contrast ? contrastGrade(contrast.ratio) : null;
  return (
    <section data-testid="aiweb-palette">
      <BlockTitle icon={Palette} count={palette.length}>
        {t('sectionPalette')}
      </BlockTitle>
      {total > 0 && (
        <div className="flex h-3 w-full rounded-full overflow-hidden mb-4" aria-hidden>
          {covered.map((s, i) => (
            <span
              key={`${s.hex}-${i}`}
              title={`${s.hex} · ${Math.round(((s.coverage ?? 0) / total) * 100)}%`}
              style={{ background: s.hex, flexGrow: s.coverage ?? 0 }}
            />
          ))}
        </div>
      )}
      <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-4 2xl:grid-cols-5 gap-3">
        {palette.map((s, i) => (
          <SwatchCard
            key={`${s.hex}-${i}`}
            swatch={s}
            onFilter={s.name ? filterBy('color', s.name) : undefined}
          />
        ))}
        {contrast && grade && (
          <div
            data-testid="aiweb-contrast"
            className="rounded-xl overflow-hidden border border-[#242424] bg-[#151515] flex flex-col"
          >
            <div
              className="h-20 flex items-center justify-center text-[28px] font-semibold"
              style={{ background: contrast.background || '#fff', color: contrast.text || '#000' }}
            >
              Aa
            </div>
            <div className="px-3 py-2 flex flex-col gap-0.5">
              <span className="flex items-center gap-1.5 text-[12.5px] text-white tabular-nums">
                {contrast.ratio.toFixed(1)}:1
                <span
                  className={`rounded px-1 text-[10px] font-semibold ${
                    grade === 'fail' ? 'bg-[#3a1515] text-[#ef5350]' : 'bg-[#13301b] text-[#4caf50]'
                  }`}
                >
                  {t(`contrast.${grade}`)}
                </span>
              </span>
              <span className="text-[11px] text-[#6f6f6f]">{t('contrastLabel')}</span>
            </div>
          </div>
        )}
      </div>
      {palette.some((s) => s.name) && (
        <p className="mt-2 text-[11px] text-[#5f5f5f]">{t('paletteHint')}</p>
      )}
    </section>
  );
}

function SwatchCard({
  swatch,
  onFilter,
}: {
  swatch: Swatch;
  onFilter?: () => void;
}): React.ReactElement {
  const t = useT('aiWebsites');
  const vocab = useVocab();
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => clearTimeout(timer.current ?? undefined), []);
  const copy = async (): Promise<void> => {
    try {
      await navigator.clipboard.writeText(swatch.hex);
      setCopied(true);
      clearTimeout(timer.current ?? undefined);
      timer.current = setTimeout(() => setCopied(false), 1100);
    } catch {
      /* clipboard unavailable */
    }
  };
  const ink = inkOn(swatch.hex);
  return (
    <div
      className="group rounded-xl overflow-hidden border border-[#242424] bg-[#151515] flex flex-col"
      data-testid="aiweb-swatch"
    >
      <button
        type="button"
        onClick={copy}
        title={t('copyHex', { hex: swatch.hex })}
        className="relative h-20 u-press"
        style={{ background: swatch.hex }}
      >
        <span
          className="absolute right-2 top-2 text-[10.5px] font-medium opacity-0 group-hover:opacity-100 u-transition flex items-center gap-1"
          style={{ color: ink }}
        >
          {copied ? (
            <>
              <Check size={11} strokeWidth={3} /> {t('copied')}
            </>
          ) : (
            t('copy')
          )}
        </span>
      </button>
      <div className="px-3 py-2 flex flex-col gap-0.5 min-w-0">
        <span className="flex items-center gap-1.5 min-w-0">
          <span className="text-[12.5px] text-white tabular-nums uppercase">{swatch.hex}</span>
          {swatch.coverage !== null && swatch.coverage > 0 && (
            <span className="ml-auto text-[11px] text-[#6f6f6f] tabular-nums">
              {Math.max(1, Math.round(swatch.coverage * 100))}%
            </span>
          )}
        </span>
        <span className="flex items-center gap-1 text-[11px] text-[#8a8a8a] min-w-0">
          {swatch.name && onFilter ? (
            <button
              type="button"
              onClick={onFilter}
              title={t('applyFilterTitle')}
              className="truncate hover:text-white hover:underline underline-offset-2"
            >
              {vocab.color(swatch.name)}
            </button>
          ) : swatch.name ? (
            <span className="truncate">{vocab.color(swatch.name)}</span>
          ) : null}
          {swatch.name && swatch.role && <span className="text-[#444]">·</span>}
          {swatch.role && (
            <span className="truncate text-[#6f6f6f]">{vocab.label('role', swatch.role)}</span>
          )}
        </span>
      </div>
    </div>
  );
}

// ── Typography ──────────────────────────────────────────────────────────────
function TypeBlock({
  fonts,
  scale,
  baseSize,
  ratio,
  filterBy,
}: {
  fonts: FontView[];
  scale: { size: number; share: number }[];
  baseSize: number | null;
  ratio: number | null;
  filterBy: FilterBy;
}): React.ReactElement {
  const t = useT('aiWebsites');
  const vocab = useVocab();
  const sizes = [...scale].sort((a, b) => b.size - a.size);
  const maxShare = Math.max(0.0001, ...sizes.map((s) => s.share));
  return (
    <section data-testid="aiweb-type">
      <BlockTitle icon={Type} count={fonts.length}>
        {t('sectionFonts')}
      </BlockTitle>
      {fonts.length === 0 ? (
        <Muted>{t('fontsEmpty')}</Muted>
      ) : (
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-3">
          {fonts.map((f, i) => {
            const roles = Array.from(new Set([f.role, ...f.roles].filter(Boolean)));
            const onFamily = filterBy('font', f.family);
            return (
              <article
                key={`${f.family}-${i}`}
                data-testid="aiweb-font"
                className="rounded-xl border border-[#242424] bg-[#151515] px-4 py-4 flex flex-col gap-3 min-w-0"
              >
                <div className="flex items-start gap-4 min-w-0">
                  <span
                    className="shrink-0 text-[44px] leading-none text-white"
                    style={{ fontFamily: fontStack(f), fontStyle: f.italic ? 'italic' : undefined }}
                    aria-hidden
                  >
                    Aa
                  </span>
                  <div className="min-w-0 flex-1">
                    {onFamily ? (
                      <button
                        type="button"
                        onClick={onFamily}
                        title={t('applyFilterTitle')}
                        className="block max-w-full truncate text-left text-[15px] font-semibold text-white hover:underline underline-offset-2"
                      >
                        {f.family}
                      </button>
                    ) : (
                      <span className="block truncate text-[15px] font-semibold text-white">
                        {f.family}
                      </span>
                    )}
                    <div className="mt-1 flex flex-wrap gap-1">
                      {Array.from(
                        new Set([
                          ...roles.map((r) => vocab.label('fontRole', r)),
                          ...(f.classification ? [vocab.label('fontClass', f.classification)] : []),
                        ]),
                      ).map((label) => (
                        <TagChip key={label} size="xs">
                          {label}
                        </TagChip>
                      ))}
                    </div>
                  </div>
                  {f.share !== null && (
                    <span
                      className="shrink-0 text-[11px] tabular-nums text-[#6f6f6f]"
                      title={t('fontShareTitle')}
                    >
                      {Math.max(1, Math.round(f.share * 100))}%
                    </span>
                  )}
                </div>
                {f.sample && (
                  <p
                    className="text-[19px] leading-snug text-[#dcdcdc] line-clamp-2 break-words"
                    style={{ fontFamily: fontStack(f), fontStyle: f.italic ? 'italic' : undefined }}
                  >
                    {f.sample}
                  </p>
                )}
                <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-[11.5px]">
                  {f.provider && (
                    <>
                      <dt className="text-[#6f6f6f]">{t('fontProvider')}</dt>
                      <dd className="text-[#bdbdbd]">{vocab.label('provider', f.provider)}</dd>
                    </>
                  )}
                  {f.weights.length > 0 && (
                    <>
                      <dt className="text-[#6f6f6f]">{t('fontWeights')}</dt>
                      <dd className="flex flex-wrap gap-x-2 text-[#bdbdbd]">
                        {f.weights.map((w) => (
                          <span
                            key={w}
                            className="tabular-nums"
                            style={{ fontFamily: fontStack(f), fontWeight: w }}
                          >
                            {w}
                          </span>
                        ))}
                      </dd>
                    </>
                  )}
                  {f.sizes.length > 0 && (
                    <>
                      <dt className="text-[#6f6f6f]">{t('fontSizes')}</dt>
                      <dd className="text-[#bdbdbd] tabular-nums">
                        {f.sizes.map((s) => `${s}`).join(' · ')} px
                      </dd>
                    </>
                  )}
                </dl>
              </article>
            );
          })}
        </div>
      )}

      {sizes.length > 0 && (
        <div className="mt-6" data-testid="aiweb-typescale">
          <BlockTitle
            icon={Ruler}
            right={
              <span className="text-[11px] text-[#6f6f6f] tabular-nums">
                {[
                  baseSize ? t('typeBase', { px: baseSize }) : '',
                  ratio ? t('typeRatio', { r: ratio.toFixed(2) }) : '',
                ]
                  .filter(Boolean)
                  .join(' · ')}
              </span>
            }
          >
            {t('typeScale')}
          </BlockTitle>
          <div className="flex flex-col gap-1.5 rounded-xl border border-[#242424] bg-[#151515] px-4 py-3">
            {sizes.map((s) => (
              <div key={s.size} className="flex items-center gap-3">
                <span className="w-12 shrink-0 text-right text-[11.5px] tabular-nums text-[#8a8a8a]">
                  {s.size}px
                </span>
                <span className="flex-1 h-2 rounded-full bg-[#1f1f1f] overflow-hidden">
                  <span
                    className="block h-full rounded-full"
                    style={{
                      width: `${Math.max(2, (s.share / maxShare) * 100)}%`,
                      background: '#7B5CFF',
                    }}
                  />
                </span>
                <span className="w-10 shrink-0 text-right text-[11px] tabular-nums text-[#5f5f5f]">
                  {Math.round(s.share * 100)}%
                </span>
              </div>
            ))}
          </div>
        </div>
      )}
    </section>
  );
}

// ── Traits ──────────────────────────────────────────────────────────────────
const BOOL_TRAITS = [
  'fixedHeader',
  'scrollJacked',
  'webgl',
  'videoBackground',
  'marquee',
  'horizontalScroll',
  'glass',
  'blendModes',
  'textGradient',
  'customCursor',
  'shadows',
  'grid',
];
const VALUE_TRAITS = [
  'smoothScroll',
  'pageTransitions',
  'containerMaxWidth',
  'cornerRadius',
  'stickyElements',
];

function TraitsBlock({ traits }: { traits: Record<string, unknown> }): React.ReactElement | null {
  const t = useT('aiWebsites');
  const vocab = useVocab();
  const on = BOOL_TRAITS.filter((k) => traits[k] === true);
  const valued = VALUE_TRAITS.map((k): [string, string] => {
    const v = traits[k];
    return [k, typeof v === 'string' || (typeof v === 'number' && v > 0) ? String(v) : ''];
  }).filter(([, v]) => v);
  const heroMedia = Array.isArray(traits.heroMedia)
    ? traits.heroMedia.filter((x): x is string => typeof x === 'string')
    : [];
  if (!on.length && !valued.length && !heroMedia.length) return null;
  return (
    <section data-testid="aiweb-traits">
      <BlockTitle icon={Layers}>{t('traitsTitle')}</BlockTitle>
      <div className="flex flex-wrap gap-1.5">
        {on.map((k) => (
          <TagChip key={k} icon={Check}>
            {vocab.label('trait', k)}
          </TagChip>
        ))}
        {valued.map(([k, v]) => (
          <TagChip key={k}>
            {vocab.label('trait', k)} <span className="text-white tabular-nums">{v}</span>
          </TagChip>
        ))}
        {heroMedia.length > 0 && (
          <TagChip>
            {vocab.label('trait', 'heroMedia')}{' '}
            <span className="text-white">{heroMedia.join(', ')}</span>
          </TagChip>
        )}
      </div>
    </section>
  );
}

// ── Tech ────────────────────────────────────────────────────────────────────
function TechBlock({
  tech,
  filterBy,
}: {
  tech: TechEntry[];
  filterBy: FilterBy;
}): React.ReactElement | null {
  const t = useT('aiWebsites');
  const vocab = useVocab();
  if (!tech.length) return null;
  const groups = new Map<string, TechEntry[]>();
  for (const x of tech) {
    const k = x.category || 'other';
    groups.set(k, [...(groups.get(k) || []), x]);
  }
  return (
    <section data-testid="aiweb-tech">
      <BlockTitle icon={Cpu} count={tech.length}>
        {t('sectionTech')}
      </BlockTitle>
      <dl className="grid grid-cols-1 lg:grid-cols-2 gap-x-8 gap-y-3">
        {[...groups.entries()].map(([cat, items]) => (
          <div key={cat} className="flex items-baseline gap-3 min-w-0">
            <dt className="w-32 shrink-0 text-[11.5px] text-[#6f6f6f]">
              {vocab.label('techCat', cat)}
            </dt>
            <dd className="flex flex-wrap gap-1.5 min-w-0">
              {items.map((x) => (
                <TagChip
                  key={x.name}
                  size="xs"
                  title={filterBy('tech', x.name) ? t('applyFilterTitle') : undefined}
                  onClick={filterBy('tech', x.name)}
                >
                  {x.name}
                  {x.version && <span className="opacity-60 tabular-nums">{x.version}</span>}
                </TagChip>
              ))}
            </dd>
          </div>
        ))}
      </dl>
    </section>
  );
}
