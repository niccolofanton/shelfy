import { useCallback, useMemo } from 'react';
import { translate, useLang } from '../../i18n';
import { FACET_ORDER, RAW_FACETS, titleCase } from './model';

// Localised labels for the closed web vocabularies (see messages/webVocab.ts).
// A value with no entry degrades to a readable version of the raw slug, so a
// vocabulary added server-side never shows up as a dotted i18n key.

const NS = 'webVocab';
const TONES = ['muted dark', 'pale', 'muted', 'light', 'deep', 'vivid'];

export interface Vocab {
  // Label for a value of a facet / closed vocabulary group (facet, pageType,
  // section, role, fontRole, techCat, provider, trait, qc, …).
  label: (group: string, value: string) => string;
  // Facet display name.
  facet: (facet: string) => string;
  // Localised colour name ("deep blue" → "blu scuro").
  color: (name: string) => string;
  // Label for a value whose facet is unknown (similar-site "shared" values).
  any: (value: string) => string;
}

function prettify(v: string): string {
  const s = v.replace(/[-_]+/g, ' ').trim();
  return s ? s.charAt(0).toUpperCase() + s.slice(1) : v;
}

export function useVocab(): Vocab {
  const { lang } = useLang();
  const lookup = useCallback(
    (key: string): string | null => {
      const full = `${NS}.${key}`;
      const v = translate(lang, full);
      return v === full ? null : v;
    },
    [lang],
  );

  const color = useCallback(
    (name: string): string => {
      const raw = (name || '').trim().toLowerCase();
      if (!raw) return '';
      const whole = lookup(`color.${raw}`);
      if (whole) return whole;
      const tone = TONES.find((tn) => raw.startsWith(`${tn} `));
      if (tone) {
        const base = lookup(`color.${raw.slice(tone.length + 1)}`);
        const toneLabel = lookup(`tone.${tone}`);
        if (base && toneLabel) {
          return translate(lang, `${NS}.colorPattern`, { base, tone: toneLabel });
        }
      }
      return raw;
    },
    [lookup, lang],
  );

  const label = useCallback(
    (group: string, value: string): string => {
      const v = String(value ?? '');
      if (!v) return '';
      if (group === 'color') return color(v);
      // Fonts/tech keep their own casing (stored as found); legacy lowercase rows are title-cased.
      if (RAW_FACETS.has(group)) return v === v.toLowerCase() ? titleCase(v) : v;
      return lookup(`${group}.${v}`) ?? lookup(`${group}.${v.toLowerCase()}`) ?? prettify(v);
    },
    [lookup, color],
  );

  const facet = useCallback((f: string): string => lookup(`facet.${f}`) ?? prettify(f), [lookup]);

  const any = useCallback(
    (value: string): string => {
      const v = String(value ?? '').toLowerCase();
      for (const f of FACET_ORDER) {
        if (RAW_FACETS.has(f) || f === 'color') continue;
        const hit = lookup(`${f}.${v}`);
        if (hit) return hit;
      }
      const c = color(v);
      return c !== v ? c : titleCase(v);
    },
    [lookup, color],
  );

  return useMemo(() => ({ label, facet, color, any }), [label, facet, color, any]);
}
