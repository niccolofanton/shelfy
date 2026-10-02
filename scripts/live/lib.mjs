// Shared helpers for scripts/live/*.mjs (P1-26 prep): the live-check tools
// for the §6.2 budgets and SSE on the real host (docs/web-port/phases/P1.md
// rule 8, "no owner session in automation" — these tools hold a session the
// lead mints and revokes for the run, never the owner's own browser
// session).
//
// Node ESM, no dependencies: Node 24's global `fetch` only.

import { randomBytes } from 'node:crypto';
import { chmodSync, readFileSync, unlinkSync, writeFileSync } from 'node:fs';
import { setTimeout as sleepMs } from 'node:timers/promises';

/** Prefix every probe folder is named with (P1-26: "creating and then
 * deleting probe folders named `zz-probe-<run>`"). */
export const PROBE_PREFIX = 'zz-probe-';

/** A short, unpredictable id for one run's probe folders and log lines. */
export function newRunId() {
  return `${Date.now().toString(36)}${randomBytes(3).toString('hex')}`;
}

export const sleep = (ms) => sleepMs(ms);

const started = performance.now();

/** Logs to stderr with a time-since-start prefix; stdout stays for the
 * script's actual output (a report, JSON, a markdown table). */
export function log(...args) {
  const ts = ((performance.now() - started) / 1000).toFixed(1).padStart(6, ' ');
  console.error(`[${ts}s]`, ...args);
}

/** Prints `message` to stderr and exits 2 (a usage error, not a run failure). */
export function usageError(message) {
  console.error(`usage: ${message}`);
  process.exit(2);
}

/** Reads a JSON file (a session or token file). Throws with `.code ===
 * 'ENOENT'` when it does not exist, like `fs.readFileSync`. */
export function readJsonFile(path) {
  return JSON.parse(readFileSync(path, 'utf8'));
}

/** Writes `data` as JSON to `path`, 0600 (owner read/write only): a session
 * or token file never readable by another account on the box. Pass secret
 * values here, never to `console.log`. */
export function writeSecretJson(path, data) {
  writeFileSync(path, `${JSON.stringify(data, null, 2)}\n`, { mode: 0o600 });
  // Belt and braces: `writeFileSync`'s mode is subject to umask.
  chmodSync(path, 0o600);
}

/** Deletes `path` if it exists; never throws when it is already gone, so
 * callers stay idempotent. Returns whether a file was actually removed. */
export function removeIfExists(path) {
  if (!path) return false;
  try {
    unlinkSync(path);
    return true;
  } catch (err) {
    if (err.code === 'ENOENT') return false;
    throw err;
  }
}

/**
 * Parses a `SPIKE_HEADERS`-style file (`scripts/spikes/sse-probe.mjs`): one
 * `Name: value` per line, so the Cloudflare Access service-token headers
 * (`CF-Access-Client-Id`, `CF-Access-Client-Secret`) stay off the command
 * line and out of shell history. Blank lines and lines without a `:` are
 * ignored. No path: `{}` (the local test server, behind no Access app).
 */
export function readHeadersFile(path) {
  if (!path) return {};
  const text = readFileSync(path, 'utf8');
  const headers = {};
  for (const rawLine of text.split('\n')) {
    const line = rawLine.trim();
    const at = line.indexOf(':');
    if (at === -1) continue;
    const name = line.slice(0, at).trim();
    const value = line.slice(at + 1).trim();
    if (name) headers[name] = value;
  }
  return headers;
}

/** An API error: a `application/problem+json` body (`crates/server/src/error.rs`)
 * with a non-2xx status. `code` is the stable, machine-readable field
 * (`reauth_required`, `csrf_failed`, …). */
export class ProblemError extends Error {
  constructor(status, problem) {
    const code = problem?.code ?? 'error';
    const detail = problem?.detail ? `: ${problem.detail}` : '';
    super(`${status} ${code}${detail}`);
    this.name = 'ProblemError';
    this.status = status;
    this.code = problem?.code;
    this.problem = problem;
  }
}

/**
 * Calls `base + path`. Always sends the Access headers (`accessHeaders`,
 * from `readHeadersFile`) and `accept: application/json`. `cookie` adds the
 * session cookie; `bearer` adds `Authorization: Bearer …` (the two are
 * mutually exclusive on every real route, like a browser tab vs. the
 * extension). `origin: true` adds `Origin: <base>` and
 * `X-Shelfy-Client: web` — the CSRF headers every unsafe cookie request
 * needs (F1, `crates/server/src/auth/csrf.rs`); skip it for GET/HEAD and
 * for bearer calls, which the CSRF guard never checks.
 *
 * Throws {@link ProblemError} for a non-2xx response. Returns `{status,
 * json, headers}` otherwise (`json` is `null` for an empty body, e.g. a 204).
 */
export async function apiCall(
  base,
  path,
  {
    method = 'GET',
    body,
    cookie,
    bearer,
    origin = false,
    accessHeaders = {},
    extraHeaders = {},
  } = {},
) {
  const headers = { accept: 'application/json', ...accessHeaders, ...extraHeaders };
  if (cookie) headers.cookie = cookie;
  if (bearer) headers.authorization = `Bearer ${bearer}`;
  if (origin) {
    headers.origin = base;
    headers['x-shelfy-client'] = 'web';
  }
  let payload;
  if (body !== undefined) {
    payload = JSON.stringify(body);
    headers['content-type'] = 'application/json';
  }
  const res = await fetch(base + path, { method, headers, body: payload });
  const text = await res.text();
  let json = null;
  if (text) {
    try {
      json = JSON.parse(text);
    } catch {
      // Not JSON (an edge or Access HTML page): `json` stays null; the
      // caller sees `status` and can report the raw text if it cares.
    }
  }
  if (!res.ok) {
    throw new ProblemError(res.status, json);
  }
  return { status: res.status, json, headers: res.headers };
}

/** The nearest-rank `p`-th percentile (0-100) of `values`, ignoring
 * `null`/`undefined`; `null` on an empty input. Matches `admin bench`'s
 * `RouteStats::quantile` (nearest rank, not interpolated). */
export function percentile(values, p) {
  const sorted = values.filter((v) => v != null).sort((a, b) => a - b);
  if (!sorted.length) return null;
  const rank = Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1));
  return sorted[rank];
}

export const round = (n, digits = 1) => {
  if (n == null) return null;
  const f = 10 ** digits;
  return Math.round(n * f) / f;
};
