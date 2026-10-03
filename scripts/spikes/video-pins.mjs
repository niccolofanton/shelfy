#!/usr/bin/env node
/**
 * SPIKE-9 Pinterest sample: public video pins found without signing in.
 *
 * The reference library has no Pinterest posts, so the Pinterest sample comes
 * from public brand accounts and boards.
 *
 * Discovery (--out): for each source this reads the public widget feed
 * (`widgets.pinterest.com/v3/pidgets/{users/<u>|boards/<u>/<b>}/pins/`, up to
 * 50 pin ids), then asks `pidgets/pins/info` for those ids in one batch and
 * keeps the pins that carry a video (`videos.video_list`, or a story pin with
 * a video block), at most --per-source per source so the sample spans several
 * accounts. It stops once --want pins are collected and writes one pin URL per
 * line.
 *
 * Images (--from pins.txt --images-out images.tsv): reads the pins' image URLs
 * from `pidgets/pins/info` and writes a `cdn-probe.mjs` sample with three
 * variants per pin, the largest size the widget serves, the `/1200x/` rewrite
 * plan §2.13 archives and the desktop's `/originals/` rewrite, labelled
 * `pinterest-served`, `pinterest-1200x` and `pinterest-originals` so
 * `cdn-compare.mjs` reports them apart. This gives SPIKE-2 its missing
 * Pinterest sample.
 *
 * Requests are anonymous and at most 1 per second.
 *
 * Usage:
 *   node scripts/spikes/video-pins.mjs --out pins.txt [--want 45] [--per-source 8]
 *     [--sources users/tastemade,boards/tastemade/tastemade-recipes,...]
 *   node scripts/spikes/video-pins.mjs --from pins.txt --images-out images.tsv
 */
import { readFileSync, writeFileSync } from 'node:fs';
import { setTimeout as sleep } from 'node:timers/promises';
import { parseArgs } from 'node:util';

const DEFAULT_SOURCES = [
  'users/tastemade',
  'boards/tastemade/tastemade-recipes',
  'users/buzzfeedtasty',
  'users/delish',
  'users/foodnetwork',
  'users/allrecipes',
  'users/thekitchn',
  'users/bhg',
  'users/hgtv',
  'users/marthastewart',
  'users/thesprucecrafts',
  'users/nike',
];

const { values: args } = parseArgs({
  options: {
    out: { type: 'string' },
    want: { type: 'string', default: '45' },
    'per-source': { type: 'string', default: '8' },
    sources: { type: 'string' },
    from: { type: 'string' },
    'images-out': { type: 'string' },
  },
});
if (!args.out && !(args.from && args['images-out'])) {
  console.error(
    'usage: video-pins.mjs --out pins.txt [--want 45] [--per-source 8] [--sources a,b]\n' +
      '       video-pins.mjs --from pins.txt --images-out images.tsv',
  );
  process.exit(2);
}

const UA =
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 ' +
  '(KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36';

async function getJson(url) {
  await sleep(1100);
  const res = await fetch(url, { headers: { 'user-agent': UA, accept: 'application/json' } });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res.json();
}

const pinInfo = (ids) =>
  getJson(`https://widgets.pinterest.com/v3/pidgets/pins/info/?pin_ids=${ids.join(',')}`);

function hasVideo(pin) {
  if (pin?.videos?.video_list && Object.keys(pin.videos.video_list).length) return true;
  const pages = pin?.story_pin_data?.pages ?? [];
  return pages.some((page) => (page.blocks ?? []).some((block) => block?.video?.video_list));
}

if (args.from) {
  const ids = readFileSync(args.from, 'utf8')
    .split('\n')
    .map((line) => line.match(/\/pin\/(\d+)/)?.[1])
    .filter(Boolean);
  const lines = ['#id\tplatform\tkind\tfreshness\turl'];
  let n = 0;
  for (let i = 0; i < ids.length; i += 50) {
    const info = await pinInfo(ids.slice(i, i + 50));
    for (const pin of info.data ?? []) {
      // The widget serves 236x/237x/564x; take the widest.
      const served = Object.entries(pin.images ?? {})
        .map(([key, image]) => ({ width: parseInt(key, 10) || 0, url: image?.url }))
        .filter((image) => image.url)
        .sort((a, b) => b.width - a.width)[0]?.url;
      if (!served || !/\/\d+x(\d+)?\//.test(served)) continue;
      n++;
      const id = String(n).padStart(3, '0');
      for (const [label, url] of [
        ['served', served],
        ['1200x', served.replace(/\/\d+x(?:\d+)?\//, '/1200x/')],
        ['originals', served.replace(/\/\d+x(?:\d+)?\//, '/originals/')],
      ]) {
        lines.push([`pin-${label}-${id}`, `pinterest-${label}`, 'poster', 'noexp', url].join('\t'));
      }
    }
  }
  writeFileSync(args['images-out'], lines.join('\n') + '\n');
  console.log(`${n} pins × 3 image variants written`);
  process.exit(0);
}

const WANT = Number(args.want);
const PER_SOURCE = Number(args['per-source']);
const sources = args.sources ? args.sources.split(',') : DEFAULT_SOURCES;
const found = new Map();
const report = [];
for (const source of sources) {
  if (found.size >= WANT) break;
  const row = { source, feedPins: 0, videoPins: 0, kept: 0, error: null };
  try {
    const feed = await getJson(`https://widgets.pinterest.com/v3/pidgets/${source}/pins/`);
    const ids = (feed.data?.pins ?? []).map((p) => String(p.id)).filter((id) => /^\d+$/.test(id));
    row.feedPins = ids.length;
    if (ids.length) {
      const info = await pinInfo(ids.slice(0, 50));
      for (const pin of info.data ?? []) {
        if (!hasVideo(pin)) continue;
        row.videoPins++;
        if (found.size < WANT && row.kept < PER_SOURCE && !found.has(String(pin.id))) {
          found.set(String(pin.id), source);
          row.kept++;
        }
      }
    }
  } catch (err) {
    row.error = err.message;
  }
  report.push(row);
}

writeFileSync(
  args.out,
  [...found.keys()].map((id) => `https://www.pinterest.com/pin/${id}/`).join('\n') + '\n',
);
console.table(report);
console.log(`${found.size} public video pins written`);
