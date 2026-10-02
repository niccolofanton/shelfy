#!/usr/bin/env node
// scripts/live/budgets.mjs — P1-26: the §6.2 server budgets from
// VictoriaMetrics (docs/web-port/IMPLEMENTATION-PLAN.md §6.2; route names
// and bucket bounds from the P1-26 carry-over note, P1-15).
//
// Route p95 (and, for `list`, p99) comes from
// `shelfy_http_request_duration_seconds_bucket{route="…"}` (bucket bounds at
// 5, 15, 40, 60 and 100 ms: crates/server/src/telemetry/metrics.rs,
// DURATION_BUCKETS). `route` is the exact axum route template — no
// `method` label on this histogram (only the `_total` counter has one), so
// never filter on method here. `g480` size comes from
// `shelfy_rendition_bytes_bucket{variant="g480"}` the same way.
//
// Usage:
//   node budgets.mjs --vm <url> [--window 1h] [--time <unix-seconds>] [--json]
//   node budgets.mjs --fixture <file> [--json]       (offline, see below)
//
// `--vm` is the VictoriaMetrics (or any Prometheus HTTP API-compatible)
// base URL the lead tunnels to from the VPS; this tool only ever runs an
// instant `/api/v1/query`, read-only.
//
// `--fixture <file>` replaces the live queries with canned responses for a
// dry run, or for CI-less verification of the table and pass/fail logic:
// a JSON object keyed by this file's row ids (`list_p95`, `g480_p95`, …),
// each value the exact JSON `/api/v1/query` itself would return (the
// Prometheus HTTP API vector format). A row missing from the file reports
// "no data", the same as a live query with an empty result.
//
// Prints a markdown table to stdout (or the same data as JSON with
// --json), then the §6.2 budgets these tools cannot measure, with the
// command the lead should run for each. Exit code is always 0: this is a
// report, not a gate (unlike `admin bench --strict`).

import { readFileSync } from 'node:fs';
import { parseArgs } from 'node:util';

import { round, usageError } from './lib.mjs';

const ROUTES = {
  list: '/api/v1/posts',
  search: '/api/v1/search',
  detail: '/api/v1/posts/{key}',
  media: '/media/{file}',
};

/** One row of the table: an id (the fixture key), a label, the PromQL
 * builder, the unit to display the value in, and the budget to compare
 * against — in the metric's *native* unit (seconds for a duration
 * histogram, however the `ms` display unit renders it), since `measured`
 * comes straight off `histogram_quantile` in that unit and the comparison
 * must not mix units. `note` surfaces a caveat next to the row. */
const ROWS = [
  {
    id: 'list_p95',
    label: 'list (`GET /posts`) p95',
    unit: 'ms',
    budget: 0.04,
    query: (w) =>
      quantileQuery('shelfy_http_request_duration_seconds', `{route="${ROUTES.list}"}`, 0.95, w),
  },
  {
    id: 'list_p99',
    label: 'list (`GET /posts`) p99',
    unit: 'ms',
    budget: 0.1,
    query: (w) =>
      quantileQuery('shelfy_http_request_duration_seconds', `{route="${ROUTES.list}"}`, 0.99, w),
  },
  {
    id: 'search_p95',
    label: 'search (`GET /search`) p95',
    unit: 'ms',
    budget: 0.06,
    query: (w) =>
      quantileQuery('shelfy_http_request_duration_seconds', `{route="${ROUTES.search}"}`, 0.95, w),
  },
  {
    id: 'detail_p95',
    label: 'detail (`GET /posts/{key}`) p95',
    unit: 'ms',
    budget: 0.015,
    query: (w) =>
      quantileQuery('shelfy_http_request_duration_seconds', `{route="${ROUTES.detail}"}`, 0.95, w),
  },
  {
    id: 'media_p95',
    label: 'media rendition (`GET /media/{file}`) p95',
    unit: 'ms',
    budget: 0.005,
    note: 'this route serves every media variant, not only `g480` — see "Gaps" in the README',
    query: (w) =>
      quantileQuery('shelfy_http_request_duration_seconds', `{route="${ROUTES.media}"}`, 0.95, w),
  },
  {
    id: 'g480_p50',
    label: '`g480` rendition size p50',
    unit: 'bytes',
    budget: 35_000,
    query: (w) => quantileQuery('shelfy_rendition_bytes', '{variant="g480"}', 0.5, w),
  },
  {
    id: 'g480_p95',
    label: '`g480` rendition size p95',
    unit: 'bytes',
    budget: 60_000,
    query: (w) => quantileQuery('shelfy_rendition_bytes', '{variant="g480"}', 0.95, w),
  },
];

/** `histogram_quantile(q, sum by (le) (increase(<metric>_bucket<selector>[window])))`.
 * `increase()`, not `rate()`, to match the P1-26 carry-over note's own
 * `g480` query exactly; `histogram_quantile` only cares about the bucket
 * proportions, which `rate()` would not have changed anyway. */
function quantileQuery(metric, selector, quantile, window) {
  return `histogram_quantile(${quantile}, sum by (le) (increase(${metric}_bucket${selector}[${window}])))`;
}

/** The scalar of a Prometheus/VictoriaMetrics instant-query vector
 * response, or `null` for no data (an empty result, or a failed query). */
function extractScalar(response) {
  if (response?.status !== 'success') return null;
  const result = response.data?.result ?? [];
  if (!result.length) return null;
  const value = Number(result[0].value?.[1]);
  return Number.isFinite(value) ? value : null;
}

async function queryVm(vmBase, promql, time) {
  const url = new URL('/api/v1/query', vmBase);
  url.searchParams.set('query', promql);
  if (time) url.searchParams.set('time', time);
  const res = await fetch(url);
  if (!res.ok) {
    throw new Error(`VictoriaMetrics query failed: HTTP ${res.status} for ${promql}`);
  }
  return res.json();
}

function formatValue(value, unit) {
  if (value == null) return 'no data';
  if (unit === 'ms') return `${round(value * 1000, 1)} ms`;
  if (unit === 'bytes') return `${round(value / 1000, 1)} KB`;
  return String(value);
}

function verdict(measured, budget) {
  if (measured == null) return 'NO DATA';
  return measured <= budget ? 'PASS' : 'FAIL';
}

function renderMarkdown(rows, window, source) {
  const lines = [];
  lines.push(`§6.2 budgets, from \`${source}\`, window \`${window}\`.`, '');
  lines.push('| Budget | Target | Measured | Result |', '|---|---|---|---|');
  for (const row of rows) {
    lines.push(
      `| ${row.label} | ≤ ${formatValue(row.budget, row.unit)} | ${formatValue(row.measured, row.unit)} | ${row.verdict} |`,
    );
  }
  const notes = rows.filter((r) => r.note);
  if (notes.length) {
    lines.push('', 'Notes:');
    for (const row of notes) lines.push(`- ${row.label}: ${row.note}`);
  }
  lines.push('', '<details><summary>Queries used</summary>', '', '```');
  for (const row of rows) lines.push(`${row.id}: ${row.promql}`);
  lines.push('```', '', '</details>');
  lines.push(
    '',
    '## Lead-run measurements',
    '',
    'The §6.2 budgets these tools cannot measure, and the command to run each:',
    '',
    '- **TTFB through Cloudflare, p95 ≤ 250 ms** (`/api/v1/version` and `index.html`, from the EU, ' +
      'through Access with the service token): a k6 run in a container; no k6 script lives in this ' +
      'repo yet (out of scope for this lane — scripts and docs only).',
    '- **A restart becomes healthy within 3 s:** the deploy log timestamps (compose/orchestrator), ' +
      'not a metric query.',
    '- **Client budgets** (gallery LCP ≤ 2.5 s mobile/slow 4G, initial JS ≤ 220 KB gzip, grid scroll ' +
      '≥ 55 fps median): taken from the P1-08 and P1-21 CI runs; for a one-off check against the ' +
      'live host, run Lighthouse CI against `https://refs.niccolofanton.dev` with the Moto G Power / ' +
      'slow 4G profile once P1-08/P1-21 land their config.',
    '- **An authenticated read-route sweep at volume** (list/search/detail/media, with rate limits ' +
      'off, at a quiet time): `just shelfy-admin bench --user <owner id> --requests 1000 --strict`. ' +
      'The very first request after a fresh deploy pays the library v2 migration (the trigram infix ' +
      'index), about 0.4 s for 6k posts (P1-05) — ignore an outlier first sample, or run it twice.',
  );
  return lines.join('\n');
}

async function main() {
  const { values } = parseArgs({
    options: {
      vm: { type: 'string' },
      window: { type: 'string', default: '1h' },
      time: { type: 'string' },
      fixture: { type: 'string' },
      json: { type: 'boolean', default: false },
    },
  });
  if (!values.vm && !values.fixture) {
    usageError(
      'budgets.mjs --vm <url> [--window 1h] [--time <unix-seconds>] [--json]  |  --fixture <file> [--json]',
    );
  }
  if (values.vm && values.fixture) {
    usageError('pass --vm or --fixture, not both');
  }

  const fixtureData = values.fixture ? JSON.parse(readFileSync(values.fixture, 'utf8')) : null;
  const source = values.fixture ? `--fixture ${values.fixture}` : values.vm;

  const rows = [];
  for (const row of ROWS) {
    const promql = row.query(values.window);
    let measured;
    if (fixtureData) {
      measured = Object.hasOwn(fixtureData, row.id) ? extractScalar(fixtureData[row.id]) : null;
    } else {
      // Sequential: a handful of reads against the lead's tunnel, no need
      // for concurrency here.
      const response = await queryVm(values.vm, promql, values.time);
      measured = extractScalar(response);
    }
    rows.push({ ...row, promql, measured, verdict: verdict(measured, row.budget) });
  }

  if (values.json) {
    console.log(JSON.stringify({ window: values.window, source, rows }, null, 2));
  } else {
    console.log(renderMarkdown(rows, values.window, source));
  }
}

main().catch((err) => {
  console.error(`error: ${err.message}`);
  process.exitCode = 1;
});
