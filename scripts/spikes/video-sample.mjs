#!/usr/bin/env node
/**
 * SPIKE-9 sample builder: video posts for `video-probe.mjs`.
 *
 * Opens the desktop library read-only and picks --per-platform Instagram and X
 * posts whose media type is `video`, with a seeded shuffle so a re-run picks
 * the same posts. Pinterest posts come from a list of public pin URLs (one per
 * line, see `video-pins.mjs`), because the reference library has none.
 *
 * Writes a TSV: `id  platform  kind  url  captionKey  authorKey`. The ids are
 * opaque (`ig-01`), so probe results can be joined across runs without URLs.
 * The two keys are short hashes of the library caption and author
 * (`video-common.mjs`); the probe hashes what each endpoint returns the same
 * way and reports only whether they match. Pinterest rows have no keys.
 *
 * The output contains personal data (the owner's saved post URLs): write it
 * outside the repo and delete it after the run.
 *
 * Usage:
 *   node scripts/spikes/video-sample.mjs --db <shelfy.sqlite> --out <sample.tsv>
 *     [--pins pins.txt] [--per-platform 30] [--seed 9]
 */
import { readFileSync, writeFileSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { parseArgs } from 'node:util';
import { authorKey, captionKey, rng, shuffle } from './video-common.mjs';

const { values: args } = parseArgs({
  options: {
    db: { type: 'string' },
    out: { type: 'string' },
    pins: { type: 'string' },
    'per-platform': { type: 'string', default: '30' },
    seed: { type: 'string', default: '9' },
  },
});
if (!args.db || !args.out) {
  console.error(
    'usage: video-sample.mjs --db <shelfy.sqlite> --out <sample.tsv> [--pins pins.txt]',
  );
  process.exit(2);
}

const N = Number(args['per-platform']);
const rand = rng(Number(args.seed));
const db = new DatabaseSync(args.db, { readOnly: true });
const rows = db
  .prepare(
    `SELECT platform, post_url AS url, text, author_username AS author FROM posts
      WHERE platform IN ('instagram', 'twitter') AND media_type = 'video'
        AND post_url LIKE 'https://%'
      ORDER BY id`,
  )
  .all();
db.close();

const lines = ['#id\tplatform\tkind\turl\tcaptionKey\tauthorKey'];
const summary = [];
for (const [platform, prefix] of [
  ['instagram', 'ig'],
  ['twitter', 'x'],
]) {
  const all = rows.filter((r) => r.platform === platform);
  const picked = shuffle(all, rand).slice(0, N);
  picked.forEach((r, i) => {
    const id = `${prefix}-${String(i + 1).padStart(2, '0')}`;
    lines.push(
      [
        id,
        platform === 'twitter' ? 'x' : platform,
        'video',
        r.url,
        captionKey(r.text) ?? '',
        authorKey(r.author) ?? '',
      ].join('\t'),
    );
  });
  summary.push({
    platform,
    videoPosts: all.length,
    sampled: picked.length,
    withCaptionKey: picked.filter((r) => captionKey(r.text)).length,
    withAuthorKey: picked.filter((r) => authorKey(r.author)).length,
  });
}

if (args.pins) {
  const pins = readFileSync(args.pins, 'utf8')
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => /^https:\/\/www\.pinterest\.com\/pin\/\d+\/?$/.test(l));
  const picked = shuffle([...new Set(pins)], rand).slice(0, N);
  picked.forEach((url, i) => {
    lines.push(
      [`pin-${String(i + 1).padStart(2, '0')}`, 'pinterest', 'video', url, '', ''].join('\t'),
    );
  });
  summary.push({ platform: 'pinterest', videoPosts: pins.length, sampled: picked.length });
}

writeFileSync(args.out, lines.join('\n') + '\n', { mode: 0o600 });
console.table(summary);
