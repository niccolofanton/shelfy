#!/usr/bin/env node
/**
 * SPIKE-9 report: aggregates `video-probe.mjs` runs into Markdown tables.
 *
 * --test is the run under evaluation (the VPS); --ref, optional, is a
 * `--resolve-only` run of the same sample from a residential connection. A
 * post counts as "reachable" for a route when --ref resolved a video on that
 * route, which separates deleted or private posts from datacenter refusals.
 *
 * Tables: video routes (success, timings, bytes, output codec and size,
 * `moov` placement, failure classes), hydration endpoints (fields found and
 * whether caption and author match the library), Instagram `oe` lifetimes,
 * throttling and stops. Aggregates only; the inputs hold no URLs or text.
 *
 * Usage:
 *   node scripts/spikes/video-report.mjs --test vps.jsonl [--ref mac.jsonl] [--json summary.json]
 */
import { readFileSync, writeFileSync } from 'node:fs';
import { parseArgs } from 'node:util';
import { pct } from './video-common.mjs';

const { values: args } = parseArgs({
  options: {
    test: { type: 'string' },
    ref: { type: 'string' },
    json: { type: 'string' },
  },
});
if (!args.test) {
  console.error('usage: video-report.mjs --test vps.jsonl [--ref mac.jsonl] [--json summary.json]');
  process.exit(2);
}

function load(path) {
  return readFileSync(path, 'utf8')
    .split('\n')
    .filter(Boolean)
    .map((line) => JSON.parse(line));
}

const testRows = load(args.test);
const results = testRows.filter((r) => r.type === 'result');
const stops = testRows.filter((r) => r.type === 'stop');
const ref = args.ref ? load(args.ref).filter((r) => r.type === 'result') : [];
const refResolved = new Set(ref.filter((r) => r.fields?.video).map((r) => `${r.id}|${r.route}`));
const refAnyVideo = new Set(ref.filter((r) => r.fields?.video).map((r) => r.id));

const ORDER = {
  instagram: ['ig-embed', 'ig-page', 'ig-graphql', 'ig-ytdlp'],
  x: ['x-syndication', 'x-oembed', 'x-ytdlp'],
  pinterest: ['pin-pidgets', 'pin-resource', 'pin-page', 'pin-oembed', 'pin-ytdlp'],
};
const pctStr = (n, d) => (d ? `${Math.round((100 * n) / d)} %` : '—');
const frac = (n, d) => `${n} / ${d}`;
const secs = (v) => (v == null ? '—' : `${(v / 1000).toFixed(1)}`);
const mb = (v) => (v == null ? '—' : `${(v / 1048576).toFixed(1)}`);
const p5095 = (values, f) => `${f(pct(values, 50))} / ${f(pct(values, 95))}`;
const countBy = (rows, key) => {
  const out = {};
  for (const r of rows) {
    const k = key(r);
    if (k == null) continue;
    out[k] = (out[k] ?? 0) + 1;
  }
  return out;
};
const fmtCounts = (counts) =>
  Object.entries(counts)
    .sort((a, b) => b[1] - a[1])
    .map(([k, v]) => `${k} ×${v}`)
    .join(', ') || '—';
const shortSide = (p) => (p?.w && p?.h ? Math.min(p.w, p.h) : null);
const sizeClass = (p) => {
  const s = shortSide(p);
  if (s == null) return null;
  if (s <= 480) return '≤480p';
  if (s <= 720) return '720p';
  if (s <= 1080) return '1080p';
  return '>1080p';
};

const summary = { video: {}, hydration: {}, lifetime: {}, stops };
const platforms = Object.keys(ORDER).filter((p) => results.some((r) => r.platform === p));

const isFull = (r) => r.download?.mode === 'full' || r.download?.mode === 'ytdlp';
const failuresOf = (rows) =>
  countBy(
    rows.filter((r) => r.outcome !== 'ok'),
    (r) => r.outcome,
  );

console.log('## Video routes\n');
console.log(
  'A route succeeds when its video URL serves a valid video to the VPS: a full download checked with ffprobe, or a 1 KiB range request when an earlier route of the same post already downloaded that file.\n',
);
console.log(
  '| Platform | Route | Posts | Video URL | Serves from the VPS | Among reachable | Resolve s p50 / p95 | Checked by full / range | Failures |',
);
console.log('|---|---|---|---|---|---|---|---|---|');
for (const platform of platforms) {
  for (const route of ORDER[platform]) {
    const rows = results.filter((r) => r.platform === platform && r.route === route);
    if (!rows.length || !rows[0].videoRoute) continue;
    const resolved = rows.filter((r) => r.fields.video);
    const ok = rows.filter((r) => r.outcome === 'ok');
    const reachable = ref.length ? rows.filter((r) => refResolved.has(`${r.id}|${r.route}`)) : [];
    const okReachable = reachable.filter((r) => r.outcome === 'ok');
    const resolveMs = rows
      .filter((r) => r.resolve.ms != null && !r.resolve.error)
      .map((r) => r.resolve.ms);
    const s = {
      posts: rows.length,
      resolved: resolved.length,
      ok: ok.length,
      reachable: reachable.length,
      okReachable: okReachable.length,
      resolveMs: { p50: pct(resolveMs, 50), p95: pct(resolveMs, 95) },
      full: ok.filter(isFull).length,
      range: ok.filter((r) => r.download?.mode === 'range').length,
      failures: failuresOf(rows),
    };
    summary.video[route] = s;
    console.log(
      `| ${platform} | ${route} | ${rows.length} | ${frac(resolved.length, rows.length)} | ${frac(ok.length, rows.length)} (${pctStr(ok.length, rows.length)}) | ${ref.length ? `${frac(okReachable.length, reachable.length)} (${pctStr(okReachable.length, reachable.length)})` : '—'} | ${p5095(resolveMs, secs)} | ${s.full} / ${s.range} | ${fmtCounts(s.failures)} |`,
    );
  }
}

console.log('\n## Fetched files\n');
console.log(
  'Full downloads only. "Direct" is the progressive MP4 a resolver route picked (Instagram `video_versions[0]` or the embed `video_url`, the best X MP4 variant whose short side is at most 1080 px, Pinterest `V_720P`); "yt-dlp" is the output of plan §2.13\'s format string. Fetch time excludes resolving; end-to-end includes it.\n',
);
console.log(
  '| Platform | Path | Files | Output | MB p50 / p95 / max (sum) | Duration s p50 | Fetch s p50 / p95 | End-to-end s p50 / p95 | `moov` first |',
);
console.log('|---|---|---|---|---|---|---|---|---|');
summary.files = {};
for (const platform of platforms) {
  for (const [label, pick] of [
    ['direct', (r) => !r.route.endsWith('-ytdlp')],
    ['yt-dlp', (r) => r.route.endsWith('-ytdlp')],
  ]) {
    const rows = results.filter(
      (r) => r.platform === platform && r.outcome === 'ok' && isFull(r) && pick(r),
    );
    if (!rows.length) continue;
    const bytes = rows.map((r) => r.download.bytes);
    const probes = rows.map((r) => r.download.probe);
    const fetchMs = rows.map((r) => r.download.ms).filter((v) => v != null);
    const e2e = rows.map((r) =>
      r.ytdlp ? r.ytdlp.totalMs : (r.resolve.ms ?? 0) + (r.download.ms ?? 0),
    );
    const durations = probes.map((p) => p?.durS).filter((v) => v != null);
    const output = countBy(probes, (p) => (p?.vcodec ? `${p.vcodec} ${sizeClass(p)}` : null));
    const faststart = rows.filter((r) => r.download.faststart).length;
    summary.files[`${platform}:${label}`] = {
      files: rows.length,
      output,
      bytes: { p50: pct(bytes, 50), p95: pct(bytes, 95), max: Math.max(...bytes) },
      sum: bytes.reduce((a, b) => a + b, 0),
      faststart,
    };
    console.log(
      `| ${platform} | ${label} | ${rows.length} | ${fmtCounts(output)} | ${mb(pct(bytes, 50))} / ${mb(pct(bytes, 95))} / ${mb(Math.max(...bytes))} (${mb(bytes.reduce((a, b) => a + b, 0))}) | ${durations.length ? pct(durations, 50) : '—'} | ${p5095(fetchMs, secs)} | ${p5095(e2e, secs)} | ${frac(faststart, rows.length)} |`,
    );
  }
}

console.log('\n## Hydration endpoints\n');
console.log(
  '| Platform | Endpoint | Answered | Caption | Caption = library | Author | Author = library | Date | Image | Video URL | Errors |',
);
console.log('|---|---|---|---|---|---|---|---|---|---|---|');
for (const platform of platforms) {
  // Pinterest has no library keys: compare with PinResource, the richest answer.
  const anchor = new Map(
    results
      .filter((r) => r.route === 'pin-resource')
      .map((r) => [r.id, { caption: r.captionKey, author: r.authorKey }]),
  );
  for (const route of ORDER[platform]) {
    const rows = results.filter((r) => r.platform === platform && r.route === route);
    if (!rows.length) continue;
    const answered = rows.filter((r) => !r.resolve.error && !r.resolve.block);
    const n = (f) => answered.filter((r) => r.fields[f]).length;
    let capMatch;
    let authMatch;
    if (platform === 'pinterest') {
      const cmp = (field) => {
        const pairs = answered.filter((r) => anchor.get(r.id)?.[field] && r[`${field}Key`]);
        const same = pairs.filter((r) => anchor.get(r.id)[field] === r[`${field}Key`]).length;
        return route === 'pin-resource'
          ? '(reference)'
          : `${frac(same, pairs.length)} vs PinResource`;
      };
      capMatch = cmp('caption');
      authMatch = cmp('author');
    } else {
      const cm = answered.filter((r) => r.captionMatch !== null);
      const am = answered.filter((r) => r.authorMatch !== null);
      capMatch = frac(cm.filter((r) => r.captionMatch).length, cm.length);
      authMatch = frac(am.filter((r) => r.authorMatch).length, am.length);
    }
    const errors = fmtCounts(
      countBy(
        rows.filter((r) => r.resolve.error || r.resolve.block),
        (r) => r.resolve.block ?? r.resolve.error,
      ),
    );
    summary.hydration[route] = {
      posts: rows.length,
      answered: answered.length,
      caption: n('caption'),
      author: n('author'),
      date: n('date'),
      media: n('media'),
      video: n('video'),
      capMatch,
      authMatch,
    };
    console.log(
      `| ${platform} | ${route} | ${frac(answered.length, rows.length)} | ${n('caption')} | ${capMatch} | ${n('author')} | ${authMatch} | ${n('date')} | ${n('media')} | ${n('video')} | ${errors} |`,
    );
  }
}

const igRows = results.filter((r) => r.platform === 'instagram');
if (igRows.length) {
  console.log('\n## Instagram `oe` lifetimes (hours left when the route answered)\n');
  console.log(
    '| Route | Video URLs | Video h: min / p50 / p95 / max | Poster URLs | Poster h: min / p50 / max |',
  );
  console.log('|---|---|---|---|---|');
  const f = (v) => (v == null ? '—' : v.toFixed(1));
  for (const route of ORDER.instagram) {
    const rows = igRows.filter((r) => r.route === route);
    const v = rows.map((r) => r.video?.oeH).filter((x) => x != null);
    const p = rows.map((r) => r.posterOeH).filter((x) => x != null);
    if (!rows.length) continue;
    summary.lifetime[route] = { video: v, poster: p };
    console.log(
      `| ${route} | ${v.length} | ${v.length ? `${f(Math.min(...v))} / ${f(pct(v, 50))} / ${f(pct(v, 95))} / ${f(Math.max(...v))}` : '—'} | ${p.length} | ${p.length ? `${f(Math.min(...p))} / ${f(pct(p, 50))} / ${f(Math.max(...p))}` : '—'} |`,
    );
  }
}

console.log('\n## Throttling and stops\n');
for (const platform of platforms) {
  const rows = results
    .filter((r) => r.platform === platform && r.videoRoute)
    .sort((a, b) => a.tRunMs - b.tRunMs);
  const third = Math.floor(rows.length / 3);
  const share = (list) => pctStr(list.filter((r) => r.outcome === 'ok').length, list.length);
  const blocks = fmtCounts(countBy(rows, (r) => r.resolve.block));
  const span = rows.length ? Math.round((rows.at(-1).tRunMs - rows[0].tRunMs) / 60000) : 0;
  console.log(
    `- **${platform}:** valid fetches ${share(rows.slice(0, third))} in the first third vs ${share(rows.slice(-third))} in the last third; block signals: ${blocks}; about ${span} min.`,
  );
}
for (const s of stops) {
  console.log(`- **stop:** ${s.platform} ${s.route ?? ''} ${s.reason} at post ${s.atSeq + 1}.`);
}
if (!stops.length) console.log('- No route or platform was stopped.');
if (ref.length) {
  console.log(
    `\nReference run: ${ref.length} results; ${refAnyVideo.size} posts resolved a video on at least one route.`,
  );
}

if (args.json) writeFileSync(args.json, JSON.stringify(summary, null, 2));
