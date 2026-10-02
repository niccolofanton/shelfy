#!/usr/bin/env node
/**
 * SPIKE-2 report: compares two `cdn-probe.mjs` runs of the same sample.
 *
 * --ref is the run that defines "the URL works" (a residential connection);
 * --test is the run under evaluation (the datacenter IP). For each platform it
 * prints, as Markdown: how many fresh URLs work from --ref, how many of those
 * also return 2xx media from --test (the SPIKE-2 ratio, pass at >= 98 %), the
 * status and block/challenge breakdown, byte-identity of the bodies, latency
 * and size percentiles, and throttling signals. Aggregates only; no URLs.
 *
 * Usage:
 *   node scripts/spikes/cdn-compare.mjs --ref mac.jsonl --test vps.jsonl [--json summary.json]
 */
import { readFileSync, writeFileSync } from 'node:fs';
import { parseArgs } from 'node:util';

const { values: args } = parseArgs({
  options: {
    ref: { type: 'string' },
    test: { type: 'string' },
    json: { type: 'string' },
  },
});
if (!args.ref || !args.test) {
  console.error('usage: cdn-compare.mjs --ref mac.jsonl --test vps.jsonl [--json out.json]');
  process.exit(2);
}

const PASS_RATIO = 0.98;

function load(path) {
  const rows = readFileSync(path, 'utf8')
    .split('\n')
    .filter(Boolean)
    .map((line) => JSON.parse(line));
  return new Map(rows.map((r) => [r.id, r]));
}

/** A complete 2xx media body: no challenge, not cut short (timeout, size cap, reset). */
const ok = (r) =>
  r &&
  r.status >= 200 &&
  r.status < 300 &&
  !r.error &&
  /^(image|video)\//.test(r.contentType ?? '') &&
  !r.challenge;

function pct(values, p) {
  if (!values.length) return null;
  const sorted = values.slice().sort((a, b) => a - b);
  const rank = Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1));
  return sorted[rank];
}

function dist(values) {
  return {
    n: values.length,
    p50: pct(values, 50),
    p90: pct(values, 90),
    p95: pct(values, 95),
    p99: pct(values, 99),
    max: values.length ? Math.max(...values) : null,
  };
}

function countBy(rows, key) {
  const counts = {};
  for (const r of rows) {
    const k = key(r);
    counts[k] = (counts[k] ?? 0) + 1;
  }
  return counts;
}

const statusKey = (r) => (r ? (r.error ? `error:${r.error}` : String(r.status)) : 'missing');
const fmtCounts = (counts) =>
  Object.entries(counts)
    .sort((a, b) => b[1] - a[1])
    .map(([k, v]) => `${k} ×${v}`)
    .join(', ') || '—';
const pctStr = (num, den) => (den ? `${((100 * num) / den).toFixed(1)} %` : 'n/a');
const kb = (b) => (b == null ? '—' : `${(b / 1024).toFixed(0)} KB`);
const ms = (v) => (v == null ? '—' : `${v} ms`);

const ref = load(args.ref);
const test = load(args.test);
const platforms = [...new Set([...ref.values()].map((r) => r.platform))];
const summary = {};

for (const platform of platforms) {
  const ids = [...ref.keys()].filter((id) => ref.get(id).platform === platform);
  const fresh = ids.filter((id) => ref.get(id).freshness !== 'expired');
  const controls = ids.filter((id) => ref.get(id).freshness === 'expired');
  const refOk = fresh.filter((id) => ok(ref.get(id)));
  const bothOk = refOk.filter((id) => ok(test.get(id)));
  const testOkAll = fresh.filter((id) => ok(test.get(id)));
  const refFail = fresh.filter((id) => !ok(ref.get(id)));
  const testOnlyFail = refOk.filter((id) => !ok(test.get(id)));
  const sameBytes = bothOk.filter((id) => ref.get(id).sha === test.get(id).sha);
  const ratio = refOk.length ? bothOk.length / refOk.length : null;
  const decision =
    refOk.length < 30
      ? 'insufficient sample'
      : ratio >= PASS_RATIO
        ? 'server'
        : ratio >= 0.5
          ? 'auto'
          : 'client';

  const testRows = fresh.map((id) => test.get(id)).filter(Boolean);
  const refRows = fresh.map((id) => ref.get(id));
  const okTest = bothOk.map((id) => test.get(id));
  const okRef = bothOk.map((id) => ref.get(id));
  // Throttling: compare the first and the last third of the run, in order.
  const ordered = testRows.slice().sort((a, b) => a.seq - b.seq);
  const third = Math.floor(ordered.length / 3);
  const early = ordered.slice(0, third);
  const late = ordered.slice(-third);
  const okShare = (rows) => (rows.length ? rows.filter(ok).length / rows.length : null);

  const s = {
    sampled: ids.length,
    fresh: fresh.length,
    controls: controls.length,
    refOk: refOk.length,
    testOk: testOkAll.length,
    bothOk: bothOk.length,
    ratio,
    decision,
    refStatus: countBy(refRows, statusKey),
    testStatus: countBy(testRows, statusKey),
    refFailReasons: countBy(
      refFail.map((id) => ref.get(id)),
      (r) => `${statusKey(r)}${r.challenge ? `/${r.challenge}` : ''}`,
    ),
    testOnlyFailReasons: countBy(
      testOnlyFail.map((id) => test.get(id)),
      (r) => `${statusKey(r)}${r?.challenge ? `/${r.challenge}` : ''}`,
    ),
    testChallenges: countBy(
      testRows.filter((r) => r.challenge),
      (r) => r.challenge,
    ),
    sameBytes: sameBytes.length,
    redirectsTest: testRows.filter((r) => r.redirects?.length).length,
    contentTypesTest: countBy(okTest, (r) => r.contentType),
    ttfbTest: dist(okTest.map((r) => r.ttfbMs)),
    totalTest: dist(okTest.map((r) => r.totalMs)),
    ttfbRef: dist(okRef.map((r) => r.ttfbMs)),
    totalRef: dist(okRef.map((r) => r.totalMs)),
    bytes: dist(okTest.map((r) => r.bytes)),
    bytesSum: okTest.reduce((a, r) => a + r.bytes, 0),
    status429Test: testRows.filter((r) => r.status === 429).length,
    retryAfterTest: testRows.filter((r) => r.retryAfter).length,
    okShareEarly: okShare(early),
    okShareLate: okShare(late),
    ttfbEarlyP50: pct(
      early.filter(ok).map((r) => r.ttfbMs),
      50,
    ),
    ttfbLateP50: pct(
      late.filter(ok).map((r) => r.ttfbMs),
      50,
    ),
    controlsRef: countBy(
      controls.map((id) => ref.get(id)),
      (r) => `${statusKey(r)}${r.challenge ? `/${r.challenge}` : ''}`,
    ),
    controlsTest: countBy(
      controls.map((id) => test.get(id)),
      (r) => `${statusKey(r)}${r?.challenge ? `/${r.challenge}` : ''}`,
    ),
  };
  summary[platform] = s;

  console.log(`\n## ${platform}\n`);
  console.log('| Measure | Value |');
  console.log('|---|---|');
  console.log(
    `| URLs sampled (fresh + expired controls) | ${s.sampled} (${s.fresh} + ${s.controls}) |`,
  );
  console.log(`| Fresh URLs that work from --ref | ${s.refOk} / ${s.fresh} |`);
  console.log(
    `| ...that also work from --test (SPIKE-2 ratio) | ${s.bothOk} / ${s.refOk} = ${pctStr(s.bothOk, s.refOk)} |`,
  );
  console.log(`| Fresh URLs that work from --test (any) | ${s.testOk} / ${s.fresh} |`);
  console.log(`| Suggested mode (>= 98 % → server) | ${s.decision} |`);
  console.log(`| --ref status | ${fmtCounts(s.refStatus)} |`);
  console.log(`| --test status | ${fmtCounts(s.testStatus)} |`);
  console.log(`| Fails on --ref (excluded) | ${fmtCounts(s.refFailReasons)} |`);
  console.log(`| Fails only on --test | ${fmtCounts(s.testOnlyFailReasons)} |`);
  console.log(`| Block/challenge answers on --test | ${fmtCounts(s.testChallenges)} |`);
  console.log(`| Identical bytes (both 2xx) | ${s.sameBytes} / ${s.bothOk} |`);
  console.log(`| Redirected on --test | ${s.redirectsTest} |`);
  console.log(`| Content types on --test (2xx) | ${fmtCounts(s.contentTypesTest)} |`);
  console.log(
    `| TTFB --test p50 / p95 / p99 / max | ${ms(s.ttfbTest.p50)} / ${ms(s.ttfbTest.p95)} / ${ms(s.ttfbTest.p99)} / ${ms(s.ttfbTest.max)} |`,
  );
  console.log(
    `| Total --test p50 / p95 / p99 / max | ${ms(s.totalTest.p50)} / ${ms(s.totalTest.p95)} / ${ms(s.totalTest.p99)} / ${ms(s.totalTest.max)} |`,
  );
  console.log(`| TTFB --ref p50 / p95 | ${ms(s.ttfbRef.p50)} / ${ms(s.ttfbRef.p95)} |`);
  console.log(`| Total --ref p50 / p95 | ${ms(s.totalRef.p50)} / ${ms(s.totalRef.p95)} |`);
  console.log(
    `| Size p50 / p90 / p99 / max (sum) | ${kb(s.bytes.p50)} / ${kb(s.bytes.p90)} / ${kb(s.bytes.p99)} / ${kb(s.bytes.max)} (${(s.bytesSum / 1048576).toFixed(1)} MB) |`,
  );
  console.log(`| 429 / Retry-After on --test | ${s.status429Test} / ${s.retryAfterTest} |`);
  console.log(
    `| 2xx share first vs last third (--test) | ${pctStr(s.okShareEarly ?? 0, 1)} vs ${pctStr(s.okShareLate ?? 0, 1)} |`,
  );
  console.log(
    `| TTFB p50 first vs last third (--test) | ${ms(s.ttfbEarlyP50)} vs ${ms(s.ttfbLateP50)} |`,
  );
  if (s.controls) {
    console.log(`| Expired controls on --ref | ${fmtCounts(s.controlsRef)} |`);
    console.log(`| Expired controls on --test | ${fmtCounts(s.controlsTest)} |`);
  }
}

if (args.json) writeFileSync(args.json, JSON.stringify(summary, null, 2));
