#!/usr/bin/env node
/**
 * SPIKE-2 sample builder: picks CDN media URLs from a desktop Shelfy library.
 *
 * Opens the desktop SQLite file read-only and writes a TSV sample for
 * `cdn-probe.mjs`: `id  platform  kind  freshness  url`. The ids are opaque
 * (`ig-0001`), so probe results can be joined across runs without the URLs.
 *
 * - kind: `cover` (posts.thumbnail_url), `slide` (image post_media),
 *   `poster` (video post_media; the desktop stores the poster image URL).
 * - freshness: Instagram URLs carry an `oe` hex expiry. `fresh` = expires more
 *   than --min-ttl-h hours from now; `expired` = already past (kept only as
 *   --controls, to record what an expired signature looks like). Other
 *   platforms have no expiry and are `noexp`.
 *
 * The output contains personal data (the owner's saved media URLs): write it
 * outside the repo and delete it after the run.
 *
 * Usage:
 *   node scripts/spikes/cdn-sample.mjs --db <shelfy.sqlite> --out <sample.tsv>
 *     [--per-platform 300] [--controls 20] [--min-ttl-h 2] [--seed 2]
 */
import { writeFileSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { parseArgs } from 'node:util';

const { values: args } = parseArgs({
  options: {
    db: { type: 'string' },
    out: { type: 'string' },
    'per-platform': { type: 'string', default: '300' },
    controls: { type: 'string', default: '20' },
    'min-ttl-h': { type: 'string', default: '2' },
    seed: { type: 'string', default: '2' },
  },
});
if (!args.db || !args.out) {
  console.error('usage: cdn-sample.mjs --db <shelfy.sqlite> --out <sample.tsv>');
  process.exit(2);
}

const PER_PLATFORM = Number(args['per-platform']);
const CONTROLS = Number(args.controls);
const MIN_TTL_S = Number(args['min-ttl-h']) * 3600;
const PLATFORM = { instagram: 'instagram', twitter: 'x', x: 'x', pinterest: 'pinterest' };
const PREFIX = { instagram: 'ig', x: 'x', pinterest: 'pin' };

/** Deterministic PRNG (mulberry32) so a re-run picks the same sample. */
function rng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function shuffle(list, rand) {
  const out = list.slice();
  for (let i = out.length - 1; i > 0; i--) {
    const j = Math.floor(rand() * (i + 1));
    [out[i], out[j]] = [out[j], out[i]];
  }
  return out;
}

/** Seconds until an Instagram `oe` expiry, or null when the URL has none. */
function igExpiresIn(url, nowS) {
  try {
    const oe = new URL(url).searchParams.get('oe');
    if (!oe || !/^[0-9a-f]+$/i.test(oe)) return null;
    return parseInt(oe, 16) - nowS;
  } catch {
    return null;
  }
}

const db = new DatabaseSync(args.db, { readOnly: true });
const rows = [
  ...db
    .prepare(
      `SELECT platform, 'cover' AS kind, thumbnail_url AS url FROM posts
        WHERE thumbnail_url LIKE 'http%'`,
    )
    .all(),
  ...db
    .prepare(
      `SELECT p.platform AS platform,
              CASE pm.media_type WHEN 'video' THEN 'poster' ELSE 'slide' END AS kind,
              pm.source_url AS url
         FROM post_media pm JOIN posts p ON p.id = pm.post_id
        WHERE pm.source_url LIKE 'http%'`,
    )
    .all(),
];
db.close();

const nowS = Date.now() / 1000;
const byPlatform = new Map();
const seen = new Set();
for (const row of rows) {
  const platform = PLATFORM[row.platform];
  if (!platform || seen.has(row.url)) continue;
  seen.add(row.url);
  let freshness = 'noexp';
  if (platform === 'instagram') {
    const ttl = igExpiresIn(row.url, nowS);
    if (ttl === null) continue;
    if (ttl > MIN_TTL_S) freshness = 'fresh';
    else if (ttl < 0) freshness = 'expired';
    else continue; // about to expire: would blur "expired" vs "blocked"
  }
  if (!byPlatform.has(platform)) byPlatform.set(platform, []);
  byPlatform.get(platform).push({ platform, kind: row.kind, freshness, url: row.url });
}

const rand = rng(Number(args.seed));
const lines = ['#id\tplatform\tkind\tfreshness\turl'];
const summary = [];
for (const platform of ['instagram', 'x', 'pinterest']) {
  const all = byPlatform.get(platform) ?? [];
  const usable = shuffle(
    all.filter((r) => r.freshness !== 'expired'),
    rand,
  );
  const expired = shuffle(
    all.filter((r) => r.freshness === 'expired'),
    rand,
  );
  const controls = platform === 'instagram' ? expired.slice(0, CONTROLS) : [];
  const picked = [...usable.slice(0, PER_PLATFORM - controls.length), ...controls];
  picked.forEach((r, i) => {
    const id = `${PREFIX[platform]}-${String(i + 1).padStart(4, '0')}`;
    lines.push([id, r.platform, r.kind, r.freshness, r.url].join('\t'));
  });
  summary.push({
    platform,
    uniqueUrls: all.length,
    usable: usable.length,
    expired: expired.length,
    sampled: picked.length,
    controls: controls.length,
  });
}

writeFileSync(args.out, lines.join('\n') + '\n', { mode: 0o600 });
console.table(summary);
