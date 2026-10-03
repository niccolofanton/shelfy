/**
 * SPIKE-9 helpers shared by the `video-*.mjs` scripts. No dependencies.
 *
 * The caption and author keys let the probe report whether a hydrated caption
 * or author matches the desktop library without carrying either text: the
 * sample stores a short hash of the library value, the probe hashes what the
 * endpoint returned the same way, and only the comparison leaves the box.
 */
import { createHash } from 'node:crypto';

const sha = (text) => createHash('sha256').update(text).digest('hex').slice(0, 12);

/** Decodes the HTML entities that platforms leave in captions (`&amp;`, `&#39;`…). */
export function decodeEntities(value) {
  return String(value ?? '').replace(
    /&(#x[0-9a-f]+|#\d+|amp|quot|apos|lt|gt|nbsp|mdash);/gi,
    (_whole, entity) => {
      const e = entity.toLowerCase();
      if (e === 'amp') return '&';
      if (e === 'quot') return '"';
      if (e === 'apos') return "'";
      if (e === 'lt') return '<';
      if (e === 'gt') return '>';
      if (e === 'nbsp') return ' ';
      if (e === 'mdash') return '—';
      const code = e.startsWith('#x') ? parseInt(e.slice(2), 16) : parseInt(e.slice(1), 10);
      return Number.isFinite(code) ? String.fromCodePoint(code) : '';
    },
  );
}

/** Lowercase ASCII letters and digits only, URLs and diacritics removed. */
export function normText(value) {
  return decodeEntities(value)
    .replace(/https?:\/\/\S+/g, ' ')
    .normalize('NFKD')
    .replace(/[\u0300-\u036f]/g, '')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '');
}

/** Hash of the first 40 normalized caption characters; null when too short to compare. */
export function captionKey(value) {
  const norm = normText(value);
  return norm.length >= 8 ? sha(norm.slice(0, 40)) : null;
}

/** Hash of a lowercased username without the leading `@`. */
export function authorKey(value) {
  const norm = String(value ?? '')
    .trim()
    .replace(/^@/, '')
    .toLowerCase();
  return norm ? sha(norm) : null;
}

/** Deterministic PRNG (mulberry32), as in `cdn-sample.mjs`. */
export function rng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export function shuffle(list, rand) {
  const out = list.slice();
  for (let i = out.length - 1; i > 0; i--) {
    const j = Math.floor(rand() * (i + 1));
    [out[i], out[j]] = [out[j], out[i]];
  }
  return out;
}

/** Nearest-rank percentile; null for an empty list. */
export function pct(values, p) {
  if (!values.length) return null;
  const sorted = values.slice().sort((a, b) => a - b);
  const rank = Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1));
  return sorted[rank];
}

/** Seconds from `nowMs` until the `oe` hex expiry of a signed Instagram/Facebook CDN URL. */
export function oeSecondsLeft(url, nowMs = Date.now()) {
  try {
    const oe = new URL(url).searchParams.get('oe');
    if (!oe || !/^[0-9a-f]+$/i.test(oe)) return null;
    return Math.round(parseInt(oe, 16) - nowMs / 1000);
  } catch {
    return null;
  }
}
