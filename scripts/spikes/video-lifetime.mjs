#!/usr/bin/env node
/**
 * SPIKE-9 Q1: lifetime of the signed Instagram CDN URLs in an extension capture.
 *
 * Reads a SPIKE-3 export (`shelfy-spike3-capture-*.json`, format version 1),
 * where every Instagram media slot carries its URL, its capture time and the
 * `oe` expiry. Prints, per URL kind (`image`, `poster`, `video`), the
 * distribution of `oe − capturedAt` in hours and how many URLs are still valid
 * now, and writes the still-valid unique URLs as a `cdn-probe.mjs` sample so
 * the fetch can be tested from the VPS and from a residential connection.
 *
 * The capture and the sample hold the owner's personal data (saved media
 * URLs): keep both outside the repo and delete the sample after the run. The
 * printed summary is aggregate only.
 *
 * Usage:
 *   node scripts/spikes/video-lifetime.mjs --capture <capture.json>
 *     [--out <sample.tsv>] [--min-ttl-h 2]
 */
import { readFileSync, writeFileSync } from 'node:fs';
import { parseArgs } from 'node:util';
import { pct } from './video-common.mjs';

const { values: args } = parseArgs({
  options: {
    capture: { type: 'string' },
    out: { type: 'string' },
    'min-ttl-h': { type: 'string', default: '2' },
  },
});
if (!args.capture) {
  console.error('usage: video-lifetime.mjs --capture <capture.json> [--out sample.tsv]');
  process.exit(2);
}

const capture = JSON.parse(readFileSync(args.capture, 'utf8'));
if (capture.format !== 'shelfy-spike3-capture' || capture.version !== 1) {
  console.error('not a shelfy-spike3-capture v1 export');
  process.exit(2);
}

const nowMs = Date.now();
const minTtlMs = Number(args['min-ttl-h']) * 3600_000;
/** url → { kind, capturedAt, expiresAt } (latest capture of each unique URL). */
const urls = new Map();
const slots = { total: 0, byKind: {} };
for (const item of capture.items ?? []) {
  if (item.platform !== 'instagram') continue;
  for (const media of item.media ?? []) {
    slots.total++;
    const kind = media.urlKind ?? media.type ?? 'image';
    slots.byKind[kind] = (slots.byKind[kind] ?? 0) + 1;
    let expiresAt = media.expiresAt;
    if (!expiresAt) {
      const oe = new URL(media.url).searchParams.get('oe');
      expiresAt = oe ? parseInt(oe, 16) * 1000 : null;
    }
    if (!expiresAt || !media.capturedAt) continue;
    const prev = urls.get(media.url);
    // A URL can sit in two slots (the cover of a video post is its poster).
    const merged = prev ? `${prev.kind}+${kind}` : kind;
    const capturedAt = Math.max(prev?.capturedAt ?? 0, media.capturedAt);
    urls.set(media.url, {
      kind: prev && prev.kind !== kind ? merged : kind,
      capturedAt,
      expiresAt,
    });
  }
}

const hours = (ms) => ms / 3600_000;
const fmt = (v) => (v == null ? '—' : v.toFixed(1));
const rows = [...urls.values()];
const kinds = [...new Set(rows.map((r) => r.kind))];
const capturedAts = rows.map((r) => r.capturedAt);

console.log(`# Instagram URL lifetime in ${args.capture.split('/').pop()}\n`);
console.log(
  `Exported ${capture.exportedAt}; captures from ${new Date(Math.min(...capturedAts)).toISOString()} ` +
    `to ${new Date(Math.max(...capturedAts)).toISOString()}; evaluated at ${new Date(nowMs).toISOString()}.`,
);
console.log(
  `Instagram media slots: ${slots.total} (${Object.entries(slots.byKind)
    .map(([k, v]) => `${k} ${v}`)
    .join(', ')}); unique signed URLs: ${rows.length}.\n`,
);
console.log(
  '| URL kind | Unique URLs | oe − capture, h: min / p5 / p25 / p50 / p75 / p95 / max | Valid now | Valid > min TTL |',
);
console.log('|---|---|---|---|---|');
for (const kind of [...kinds, 'all']) {
  const list = kind === 'all' ? rows : rows.filter((r) => r.kind === kind);
  const ttl = list.map((r) => hours(r.expiresAt - r.capturedAt));
  const stats = [
    Math.min(...ttl),
    pct(ttl, 5),
    pct(ttl, 25),
    pct(ttl, 50),
    pct(ttl, 75),
    pct(ttl, 95),
    Math.max(...ttl),
  ];
  const valid = list.filter((r) => r.expiresAt > nowMs).length;
  const fresh = list.filter((r) => r.expiresAt - nowMs > minTtlMs).length;
  console.log(`| ${kind} | ${list.length} | ${stats.map(fmt).join(' / ')} | ${valid} | ${fresh} |`);
}

const left = rows.map((r) => hours(r.expiresAt - nowMs));
console.log(
  `\nHours left now: min ${fmt(Math.min(...left))}, p50 ${fmt(pct(left, 50))}, max ${fmt(Math.max(...left))}.`,
);

if (args.out) {
  const lines = ['#id\tplatform\tkind\tfreshness\turl'];
  let n = 0;
  for (const [url, r] of urls) {
    if (r.expiresAt - nowMs <= minTtlMs) continue;
    n++;
    lines.push([`cap-${String(n).padStart(4, '0')}`, 'instagram', r.kind, 'fresh', url].join('\t'));
  }
  writeFileSync(args.out, lines.join('\n') + '\n', { mode: 0o600 });
  console.log(`\nWrote ${n} still-valid URLs for cdn-probe.mjs.`);
}
