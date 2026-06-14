import { useEffect, useRef, useState } from 'react';
import { ExternalLink, RotateCw, Globe, Award, Cpu, Check } from 'lucide-react';
import { useT } from '../../i18n';

// ── Web-reference shape helpers (defensive: backend shapes are loose) ────────
// Palette entries may be hex strings OR { hex, role } objects.
function paletteHexes(palette) {
  if (!Array.isArray(palette)) return [];
  return palette
    .map((c) => (typeof c === 'string' ? c : c && typeof c === 'object' ? c.hex : null))
    .filter((h) => typeof h === 'string' && /^#?[0-9a-fA-F]{3,8}$/.test(h.trim()))
    .map((h) => (h.trim().startsWith('#') ? h.trim() : `#${h.trim()}`));
}
// Fonts are objects { family, usage, provider } (NOT strings); tolerate strings.
function fontEntries(fonts) {
  if (!Array.isArray(fonts)) return [];
  return fonts
    .map((f) => (typeof f === 'string' ? { family: f } : f))
    .filter((f) => f && typeof f.family === 'string' && f.family.trim());
}
// Tech is an array of slug strings.
function techList(tech) {
  if (!Array.isArray(tech)) return [];
  return tech.filter((t) => typeof t === 'string' && t.trim());
}
// Awards are objects { platform, level?, date?, profileUrl?, evidence, confidence }.
function awardList(awards) {
  if (!Array.isArray(awards)) return [];
  return awards.filter((a) => a && (a.platform || a.level));
}

// One clickable palette swatch — copies its hex on click, with a transient
// "Copiato" check. A real <button> so it's keyboard-focusable.
function PaletteSwatch({ hex }) {
  const t = useT('postModal');
  const [copied, setCopied] = useState(false);
  const timerRef = useRef(null);
  useEffect(() => () => clearTimeout(timerRef.current), []);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(hex);
      setCopied(true);
      clearTimeout(timerRef.current);
      timerRef.current = setTimeout(() => setCopied(false), 1100);
    } catch {
      /* clipboard unavailable */
    }
  };
  return (
    <button
      onClick={copy}
      title={t('copyHex', { hex })}
      aria-label={t('copyColor', { hex })}
      className="u-press flex flex-col items-center gap-1 group/swatch"
    >
      <span
        className="relative w-7 h-7 rounded-md ring-1 ring-white/10 flex items-center justify-center"
        style={{ backgroundColor: hex }}
      >
        {copied && (
          <Check
            size={14}
            className="u-pop-in text-white drop-shadow-[0_1px_1px_rgba(0,0,0,0.7)]"
            strokeWidth={3}
          />
        )}
      </span>
      <span className="text-[10px] text-[#9a9a9a] tabular-nums">{copied ? t('copied') : hex}</span>
    </button>
  );
}

// Web metadata block (palette / fonts / tech / awards / "Apri sito"). Rendered
// only for platform==='web'; every group is render-conditional so a raw site
// (enrichment still running) shows only what exists.
export default function WebMetaPanel({ post, onOpenInWebsites, onReanalyzeWeb }) {
  const t = useT('postModal');
  const palette = paletteHexes(post.webPalette);
  const fonts = fontEntries(post.webFonts);
  const tech = techList(post.webTech);
  const awards = awardList(post.webAwards);
  const siteUrl = post.postUrl || post.webFinalUrl || post.webUrl;

  return (
    <div className="u-fade-in rounded-lg border border-[#2a2a2a] bg-[#121212] px-3 py-2.5 space-y-3">
      <div className="flex items-center gap-1.5 text-[11px] font-medium uppercase tracking-wide text-[#7a7a7a]">
        <Globe size={12} className="text-[#7B5CFF]" />
        {t('siteMetadata')}
      </div>

      <div className="flex flex-wrap items-center gap-2">
        {siteUrl && (
          <button
            data-testid="web-open-site"
            onClick={() => window.electronAPI.openExternal(siteUrl)}
            className="u-press inline-flex items-center gap-2 px-3 h-8 rounded-md text-[12px] font-medium text-white bg-[#7B5CFF] hover:bg-[#5A3DDE] transition-colors"
          >
            <ExternalLink size={13} />
            {t('openSite')}
          </button>
        )}
        {onOpenInWebsites && (
          <button
            data-testid="web-open-in-websites"
            onClick={() => onOpenInWebsites(post)}
            title={t('openInWebsitesTitle')}
            className="u-press inline-flex items-center gap-2 px-3 h-8 rounded-md text-[12px] font-medium text-gray-200 bg-[#1f1f1f] hover:bg-[#272727] transition-colors"
          >
            <Globe size={13} />
            {t('openInWebsites')}
          </button>
        )}
        {onReanalyzeWeb && (
          <button
            data-testid="web-reanalyze"
            onClick={() => onReanalyzeWeb(post)}
            title={t('reanalyzeTitle')}
            className="u-press inline-flex items-center gap-2 px-3 h-8 rounded-md text-[12px] font-medium text-gray-200 bg-[#1f1f1f] hover:bg-[#272727] transition-colors"
          >
            <RotateCw size={13} />
            {t('reanalyze')}
          </button>
        )}
      </div>

      {palette.length > 0 && (
        <div className="space-y-1.5">
          <span className="text-[10px] uppercase tracking-wide text-[#6a6a6a]">{t('palette')}</span>
          <div className="flex flex-wrap gap-2">
            {palette.map((hex, i) => (
              <PaletteSwatch key={`${hex}-${i}`} hex={hex} />
            ))}
          </div>
        </div>
      )}

      {fonts.length > 0 && (
        <div className="space-y-1.5">
          <span className="text-[10px] uppercase tracking-wide text-[#6a6a6a]">{t('fonts')}</span>
          <div className="flex flex-wrap gap-1.5">
            {fonts.map((f, i) => (
              <span
                key={`${f.family}-${i}`}
                className="px-2 py-0.5 rounded-full bg-[#1f1f1f] text-[#aaa] text-[11px]"
              >
                {f.family}
                {f.usage && <span className="text-[#6a6a6a]"> · {f.usage}</span>}
              </span>
            ))}
          </div>
        </div>
      )}

      {tech.length > 0 && (
        <div className="space-y-1.5">
          <span className="flex items-center gap-1.5 text-[10px] uppercase tracking-wide text-[#6a6a6a]">
            <Cpu size={11} /> {t('technologies')}
          </span>
          <div className="flex flex-wrap gap-1.5">
            {tech.map((t) => (
              <span
                key={t}
                className="px-2 py-0.5 rounded-full bg-[#1f1f1f] text-[#aaa] text-[11px]"
              >
                {t}
              </span>
            ))}
          </div>
        </div>
      )}

      {awards.length > 0 && (
        <div className="space-y-1.5">
          <span className="text-[10px] uppercase tracking-wide text-[#6a6a6a]">{t('awards')}</span>
          <div className="flex flex-wrap gap-1.5">
            {awards.map((a, i) => {
              const label = [a.platform, a.level].filter(Boolean).join(' · ') || t('award');
              const inner = (
                <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full bg-amber-400/90 text-black text-[11px] font-medium">
                  <Award size={11} strokeWidth={2.5} />
                  {label}
                </span>
              );
              return a.profileUrl ? (
                <button
                  key={`${label}-${i}`}
                  onClick={() => window.electronAPI.openExternal(a.profileUrl)}
                  title={a.evidence || t('openAwardProfile')}
                  className="u-press"
                >
                  {inner}
                </button>
              ) : (
                <span key={`${label}-${i}`} title={a.evidence || undefined}>
                  {inner}
                </span>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
