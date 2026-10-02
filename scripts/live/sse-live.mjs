#!/usr/bin/env node
// scripts/live/sse-live.mjs — P1-26: the SPIKE-10 SSE probe
// (scripts/spikes/sse-probe.mjs), adapted from its throwaway test server
// (`/api/v1/emit`, a synthetic `probe` event) to the real API. Run after
// `session.mjs redeem` (and `token`, for the bearer phase).
//
// What it measures, against docs/web-port/IMPLEMENTATION-PLAN.md §6.2 and
// the P1-26 carry-over note from P1-23:
//
//   - latency:    write (POST /api/v1/collections) -> the matching
//                 `posts.changed` event on GET /api/v1/events; p50/p95/max
//                 over >=50 events. Budget: p95 <= 300 ms.
//   - long stream: one stream held open >=5 min, with one idle gap >100 s
//                 (Cloudflare's proxied-connection cutoff is ~100 s; the
//                 20 s heartbeat is what should keep it open); heartbeats
//                 counted, no disconnects.
//   - resume:     close the stream, write N events, reconnect with
//                 `Last-Event-ID`, expect all N, in order, no duplicates.
//   - bearer:     the API token, with the extension / iOS Shortcut / CLI
//                 User-Agents, gets no challenge (no 403, no HTML, no
//                 `cf-mitigated`) on a bearer-scoped route.
//
// Writes create, and immediately delete, a collection named
// `zz-probe-<run>` through POST/DELETE /api/v1/collections (never touching
// real posts). Every probe folder this run creates is tracked and deleted
// again on exit, including on error or SIGINT/SIGTERM.
//
// A note on pacing: `posts.changed` is throttled per reason, leading edge,
// at most one event per 2 s (crates/server/src/events/coalesce.rs,
// `POSTS_WINDOW`); create and delete both announce reason `edit`, so two
// writes closer together than that window share one *coalesced* event
// instead of two independent ones, and the second would misreport the
// throttle's delay as network/SSE latency. Every write here is paced at
// INTERVAL_MS (2.2 s) apart for an honest, leading-edge sample every time.
//
// Options:
//   --session <file>   from `session.mjs redeem` (required)
//   --token <file>      from `session.mjs token` (optional: skips the
//                        bearer-call phase when absent)
//   --base <url>        default: the session file's `base`
//   --headers <file>    the Cloudflare Access service-token headers
//                        (`CF-Access-Client-Id` / `-Secret`), one
//                        `Name: value` per line; omit against a local server
//   --quick             a short run for local testing: fewer latency samples
//                        and a short long-stream phase that does NOT reach
//                        the real >=5 min / >100 s idle budget (flagged as
//                        "skipped" in the report, not pass or fail)
//   --json              print the full result as JSON instead of the
//                        human-readable report
//   --out <file>        also write the JSON result to <file>
//
// Exit code: 0 when every budget this run actually measured passed (quick
// mode's shortened long-stream phase is excluded from that verdict); 1
// otherwise, or on error.

import { randomBytes } from 'node:crypto';
import { writeFileSync } from 'node:fs';
import { parseArgs } from 'node:util';

import {
  apiCall,
  log,
  percentile,
  readHeadersFile,
  readJsonFile,
  round,
  sleep,
  usageError,
} from './lib.mjs';

const CHROME_UA =
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 ' +
  '(KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36';
const USER_AGENTS = {
  extension: CHROME_UA,
  ios_shortcut: 'Shortcuts/4046.0.2 CFNetwork/3826.500.131 Darwin/25.0.0',
  cli: 'shelfy-migrate/0.1.0',
};

// Server throttle window of `posts.changed` per reason (POSTS_WINDOW,
// events/mod.rs) is 2 s; pace every write a comfortable margin past it so
// each one lands on the throttle's leading edge.
const INTERVAL_MS = 2200;

const FULL = {
  pairs: 25, // create+delete => 50 latency samples
  totalStreamS: 300, // >= 5 minutes
  minIdleS: 110, // > Cloudflare's ~100 s idle cutoff, with margin
  resumePairs: 5, // 10 events while disconnected
  bearerCalls: 5, // per User-Agent
};
const QUICK = {
  pairs: 3,
  totalStreamS: 30,
  minIdleS: 22, // just long enough to see a 20 s heartbeat
  resumePairs: 2,
  bearerCalls: 2,
};

function parseCli() {
  const { values } = parseArgs({
    options: {
      base: { type: 'string' },
      headers: { type: 'string' },
      session: { type: 'string' },
      token: { type: 'string' },
      quick: { type: 'boolean', default: false },
      json: { type: 'boolean', default: false },
      out: { type: 'string' },
    },
  });
  if (!values.session) {
    usageError(
      'sse-live.mjs --session <file> [--token <file>] [--base <url>] [--headers <file>] [--quick] [--json] [--out <file>]',
    );
  }
  return values;
}

/** A parsed Server-Sent Events frame: `{event:, data:, id:}`, or a bare
 * comment (the heartbeat) with none of those. */
function parseFrame(block) {
  let id = null;
  let type = 'message';
  let comment = null;
  const data = [];
  for (const line of block.split('\n')) {
    if (line.startsWith(':')) {
      comment = line.slice(1).trim();
      continue;
    }
    const i = line.indexOf(':');
    const field = i === -1 ? line : line.slice(0, i);
    let value = i === -1 ? '' : line.slice(i + 1);
    if (value.startsWith(' ')) value = value.slice(1);
    if (field === 'id') id = value;
    else if (field === 'event') type = value;
    else if (field === 'data') data.push(value);
  }
  return { id, type, data, comment };
}

/** One open `GET /api/v1/events` connection: parses frames as they arrive,
 * keeps a FIFO of delivered messages a caller can consume in order
 * (`takeNext`), and counts heartbeats. Adapted from the `Stream` class of
 * scripts/spikes/sse-probe.mjs for the real API (cookie auth, real topic
 * names, no synthetic `probe` event). */
class EventStream {
  constructor(base, { cookie, accessHeaders, topics, lastEventId } = {}) {
    this.base = base;
    this.cookie = cookie;
    this.accessHeaders = accessHeaders ?? {};
    this.topics = topics;
    this.lastEventId = lastEventId ?? null;
    this.messages = [];
    this.heartbeats = [];
    this.cursor = 0;
    this.waiters = [];
    this.ctrl = new AbortController();
    this.openedAt = null;
    this.closedAt = null;
    this.endReason = null;
    this.status = null;
  }

  async open() {
    this.openedAt = performance.now();
    const url = new URL(`${this.base}/api/v1/events`);
    if (this.topics) url.searchParams.set('topics', this.topics);
    const headers = {
      accept: 'text/event-stream',
      ...this.accessHeaders,
      cookie: this.cookie,
    };
    if (this.lastEventId) headers['last-event-id'] = this.lastEventId;
    let res;
    try {
      res = await fetch(url, { headers, signal: this.ctrl.signal });
    } catch (err) {
      this.endReason = `error:${err.cause?.code ?? err.name}`;
      this.closedAt = performance.now();
      this.done = Promise.resolve();
      return this;
    }
    this.status = res.status;
    this.cfMitigated = Boolean(res.headers.get('cf-mitigated'));
    if (!res.ok || !res.body) {
      this.endReason = `http_${res.status}`;
      this.closedAt = performance.now();
      this.done = Promise.resolve();
      return this;
    }
    this.done = this.pump(res.body);
    return this;
  }

  async pump(body) {
    const reader = body.getReader();
    const decoder = new TextDecoder();
    let buf = '';
    try {
      for (;;) {
        const { value, done } = await reader.read();
        if (done) {
          this.endReason = 'eof';
          break;
        }
        const at = performance.now();
        buf += decoder.decode(value, { stream: true }).replace(/\r\n?/g, '\n');
        let idx;
        while ((idx = buf.indexOf('\n\n')) !== -1) {
          this.dispatch(buf.slice(0, idx), at);
          buf = buf.slice(idx + 2);
        }
      }
    } catch (err) {
      this.endReason = this.ctrl.signal.aborted
        ? 'closed_by_client'
        : `error:${err.cause?.code ?? err.name}`;
    } finally {
      this.closedAt = performance.now();
      for (const waiter of this.waiters.splice(0)) {
        waiter.reject(new Error(`stream ended (${this.endReason})`));
      }
    }
  }

  dispatch(block, at) {
    const frame = parseFrame(block);
    if (frame.comment !== null && !frame.data.length && frame.id === null) {
      this.heartbeats.push(at);
      return;
    }
    if (!frame.data.length) return; // e.g. a lone `retry:`
    if (frame.id !== null) this.lastEventId = frame.id;
    let parsed = frame.data.join('\n');
    try {
      parsed = JSON.parse(parsed);
    } catch {
      // Non-JSON data (should not happen on this API): keep the raw text.
    }
    const msg = { id: frame.id, type: frame.type, data: parsed, at };
    this.messages.push(msg);
    const waiter = this.waiters.shift();
    if (waiter) waiter.resolve(msg);
  }

  /** Waits for, and consumes, the next not-yet-seen message (FIFO order). */
  takeNext(timeoutMs) {
    if (this.cursor < this.messages.length) {
      return Promise.resolve(this.messages[this.cursor++]);
    }
    if (this.closedAt) {
      return Promise.reject(new Error(`stream already ended (${this.endReason})`));
    }
    return new Promise((resolve, reject) => {
      const onMsg = (msg) => {
        clearTimeout(timer);
        this.cursor++;
        resolve(msg);
      };
      const timer = setTimeout(() => {
        const i = this.waiters.findIndex((w) => w.resolve === onMsg);
        if (i !== -1) this.waiters.splice(i, 1);
        reject(new Error('timed out waiting for the next event'));
      }, timeoutMs);
      this.waiters.push({
        resolve: onMsg,
        reject: (err) => {
          clearTimeout(timer);
          reject(err);
        },
      });
    });
  }

  /** The first frame of every stream; throws if it is anything else. */
  async waitForHello(timeoutMs) {
    const msg = await this.takeNext(timeoutMs);
    if (msg.type !== 'hello') throw new Error(`expected hello, got ${msg.type}`);
    return msg;
  }

  async close() {
    this.ctrl.abort();
    await this.done;
  }

  openForS() {
    return ((this.closedAt ?? performance.now()) - this.openedAt) / 1000;
  }
}

/** Creates, measures, and deletes one probe folder, pacing the two writes
 * (and their SSE waits) INTERVAL_MS apart so neither lands inside the
 * other's throttle window. Pushes a latency sample (ms, or null on a
 * missed/late event) for each write into `latencies`. */
async function probePair(ctx, stream, latencies) {
  const create = async () => {
    const res = await apiCall(ctx.base, '/api/v1/collections', {
      method: 'POST',
      body: { name: ctx.probeName },
      cookie: ctx.session.cookie,
      origin: true,
      accessHeaders: ctx.accessHeaders,
    });
    ctx.openIds.add(res.json.id);
    return res.json.id;
  };
  const del = async (id) => {
    await apiCall(ctx.base, `/api/v1/collections/${id}`, {
      method: 'DELETE',
      cookie: ctx.session.cookie,
      origin: true,
      accessHeaders: ctx.accessHeaders,
    });
    ctx.openIds.delete(id);
  };

  const pacedWrite = async (fn) => {
    const slotStart = performance.now();
    const returned = await fn();
    if (stream) {
      try {
        const msg = await stream.takeNext(10_000);
        latencies?.push(msg.at - slotStart);
      } catch (err) {
        log(`latency sample missed: ${err.message}`);
        latencies?.push(null);
      }
    }
    const waited = performance.now() - slotStart;
    if (waited < INTERVAL_MS) await sleep(INTERVAL_MS - waited);
    return returned;
  };

  const id = await pacedWrite(create);
  await pacedWrite(() => del(id));
}

async function latencyAndLongStreamPhase(ctx, profile) {
  const stream = new EventStream(ctx.base, {
    cookie: ctx.session.cookie,
    accessHeaders: ctx.accessHeaders,
    topics: 'posts.changed',
  });
  // Tracked so `cleanup()` can abort it too: an open SSE fetch holds a live
  // socket, so an error thrown before the normal `stream.close()` below
  // (e.g. `waitForHello` timing out) would otherwise keep the process
  // alive forever — `main()` never calls `process.exit()` on its own.
  ctx.openStreams.add(stream);
  await stream.open();
  if (stream.status !== 200) {
    throw new Error(`could not open the event stream (http ${stream.status}, ${stream.endReason})`);
  }
  await stream.waitForHello(15_000);
  log('stream open; hello received');

  const latencies = [];
  for (let n = 0; n < profile.pairs; n++) {
    await probePair(ctx, stream, latencies);
  }
  const delivered = latencies.filter((v) => v != null);
  const latency = {
    samples: delivered.length,
    missed: latencies.length - delivered.length,
    p50: round(percentile(latencies, 50)),
    p95: round(percentile(latencies, 95)),
    p99: round(percentile(latencies, 99)),
    max: delivered.length ? round(Math.max(...delivered)) : null,
    meetsBudget: null,
  };
  latency.meetsBudget = latency.p95 != null && latency.p95 <= 300;
  log('latency', latency);

  // Pads the idle gap so the stream both crosses `totalStreamS` in total
  // and sees one gap over `minIdleS` with nothing but heartbeats.
  const elapsedS = stream.openForS();
  const idleTargetS = Math.max(profile.minIdleS, profile.totalStreamS - elapsedS);
  const hbBefore = stream.heartbeats.length;
  log(
    `idling ${Math.round(idleTargetS)}s (no writes) to cross the ${profile.minIdleS}s idle target`,
  );
  await sleep(idleTargetS * 1000);
  const idleHeartbeats = stream.heartbeats.length - hbBefore;

  const longStream = {
    openForS: round(stream.openForS()),
    idleS: round(idleTargetS),
    idleHeartbeats,
    totalHeartbeats: stream.heartbeats.length,
    disconnected: stream.closedAt != null,
    meetsIdleBudget: idleTargetS > 100,
    meetsDurationBudget: stream.openForS() >= 300,
  };
  log('long stream', longStream);

  const lastEventId = stream.lastEventId;
  await stream.close();
  ctx.openStreams.delete(stream);
  return { latency, longStream, lastEventId };
}

async function resumePhase(ctx, profile, lastEventId) {
  log(`resume: writing ${profile.resumePairs * 2} events while disconnected`);
  for (let n = 0; n < profile.resumePairs; n++) {
    await probePair(ctx, null, null);
  }
  const expected = profile.resumePairs * 2;

  const resumed = new EventStream(ctx.base, {
    cookie: ctx.session.cookie,
    accessHeaders: ctx.accessHeaders,
    topics: 'posts.changed',
    lastEventId,
  });
  ctx.openStreams.add(resumed);
  await resumed.open();
  if (resumed.status !== 200) {
    throw new Error(`resume reconnect failed (http ${resumed.status}, ${resumed.endReason})`);
  }
  await resumed.waitForHello(15_000);

  const received = [];
  let resync = null;
  for (let i = 0; i < expected; i++) {
    let msg;
    try {
      msg = await resumed.takeNext(10_000);
    } catch (err) {
      log(`resume: stopped at ${received.length}/${expected}: ${err.message}`);
      break;
    }
    if (msg.type === 'resync') {
      resync = msg.data?.reason ?? true;
      break;
    }
    received.push(msg.id);
  }
  await resumed.close();
  ctx.openStreams.delete(resumed);

  const duplicates = received.length - new Set(received).size;
  const result = {
    expected,
    received: received.length,
    duplicates,
    resync,
    lossless: resync == null && received.length === expected && duplicates === 0,
  };
  log('resume', result);
  return result;
}

function looksLikeChallenge(res, text) {
  if (res.headers.get('cf-mitigated')) return 'cf-mitigated';
  if (res.status === 403) return 'http_403';
  // This route only ever answers JSON; HTML at any status (Access's login
  // page via a followed redirect, a WAF block page, …) is itself the
  // challenge signal, not just an HTML error page.
  const contentType = res.headers.get('content-type') ?? '';
  if (/text\/html/.test(contentType)) return 'html_response';
  if (
    /cf-chl|challenge-platform|just a moment|attention required|captcha/i.test(text.slice(0, 4096))
  ) {
    return 'challenge_page';
  }
  return null;
}

async function bearerPhase(ctx, tokenData, callsPerUa) {
  const byUa = {};
  for (const [name, ua] of Object.entries(USER_AGENTS)) {
    const stats = { calls: 0, ok: 0, challenges: 0, other: {} };
    for (let i = 0; i < callsPerUa; i++) {
      stats.calls++;
      let res;
      let text;
      try {
        res = await fetch(`${ctx.base}/api/v1/posts/lookup`, {
          method: 'POST',
          headers: {
            ...ctx.accessHeaders,
            'user-agent': ua,
            authorization: `Bearer ${tokenData.token}`,
            'content-type': 'application/json',
            accept: 'application/json',
          },
          body: JSON.stringify({ platform: 'instagram', keys: [] }),
        });
        text = await res.text();
      } catch (err) {
        stats.other[`error:${err.cause?.code ?? err.name}`] = (stats.other.error ?? 0) + 1;
        continue;
      }
      const challenge = looksLikeChallenge(res, text);
      if (challenge) stats.challenges++;
      else if (res.status === 200) stats.ok++;
      else stats.other[res.status] = (stats.other[res.status] ?? 0) + 1;
    }
    byUa[name] = stats;
  }
  const challenges = Object.values(byUa).reduce((sum, s) => sum + s.challenges, 0);
  const result = { byUa, challenges, noChallenge: challenges === 0 };
  log('bearer', { calls: Object.values(byUa).reduce((s, v) => s + v.calls, 0), challenges });
  return result;
}

function overallPass(result, quick) {
  const checks = [result.latency.meetsBudget, result.resume.lossless];
  if (!quick) {
    checks.push(
      result.longStream.meetsIdleBudget &&
        result.longStream.meetsDurationBudget &&
        !result.longStream.disconnected,
    );
  }
  if (result.bearer && !result.bearer.skipped) checks.push(result.bearer.noChallenge);
  return checks.every(Boolean);
}

function printHumanReport(result, quick) {
  const lines = [];
  lines.push(`base: ${result.base}`);
  lines.push('');
  lines.push(
    `latency   p50 ${result.latency.p50}ms  p95 ${result.latency.p95}ms  max ${result.latency.max}ms  ` +
      `(${result.latency.samples}/${result.latency.samples + result.latency.missed} events, budget p95<=300ms) ` +
      `${result.latency.meetsBudget ? 'PASS' : 'FAIL'}`,
  );
  if (quick) {
    lines.push(
      `long stream  open ${result.longStream.openForS}s, idle ${result.longStream.idleS}s, ` +
        `${result.longStream.totalHeartbeats} heartbeats  (--quick: below the real >=300s/>100s budget, not scored)`,
    );
  } else {
    lines.push(
      `long stream  open ${result.longStream.openForS}s, idle ${result.longStream.idleS}s, ` +
        `${result.longStream.totalHeartbeats} heartbeats, disconnected=${result.longStream.disconnected}  ` +
        `${result.longStream.meetsIdleBudget && result.longStream.meetsDurationBudget && !result.longStream.disconnected ? 'PASS' : 'FAIL'}`,
    );
  }
  lines.push(
    `resume    expected ${result.resume.expected}, received ${result.resume.received}, duplicates ${result.resume.duplicates}` +
      `${result.resume.resync ? `, resync: ${result.resume.resync}` : ''}  ${result.resume.lossless ? 'PASS' : 'FAIL'}`,
  );
  if (result.bearer?.skipped) {
    lines.push(`bearer    skipped (${result.bearer.skipped})`);
  } else {
    const perUa = Object.entries(result.bearer.byUa)
      .map(([name, s]) => `${name}: ${s.ok}/${s.calls} ok, ${s.challenges} challenged`)
      .join('; ');
    lines.push(`bearer    ${perUa}  ${result.bearer.noChallenge ? 'PASS' : 'FAIL'}`);
  }
  lines.push('');
  lines.push(overallPass(result, quick) ? 'overall: PASS' : 'overall: FAIL (see above)');
  console.log(lines.join('\n'));
}

async function main() {
  const values = parseCli();
  const session = readJsonFile(values.session);
  const base = values.base ?? session.base;
  const accessHeaders = readHeadersFile(values.headers);
  const tokenData = values.token ? readJsonFile(values.token) : null;
  const profile = values.quick ? QUICK : FULL;

  const ctx = {
    base,
    session,
    accessHeaders,
    probeName: newProbeName(),
    openIds: new Set(),
    openStreams: new Set(),
  };

  const cleanup = async () => {
    // Aborts any SSE connection still open (a thrown error before its own
    // `stream.close()` ran): otherwise its live socket keeps the process
    // alive forever, since nothing here calls `process.exit()` on the
    // normal exit path.
    for (const stream of [...ctx.openStreams]) {
      try {
        await stream.close();
      } catch {
        // close() only aborts a fetch and awaits its already-settling
        // promise; nothing meaningful to report if that itself throws.
      }
      ctx.openStreams.delete(stream);
    }
    for (const id of [...ctx.openIds]) {
      try {
        await apiCall(base, `/api/v1/collections/${id}`, {
          method: 'DELETE',
          cookie: session.cookie,
          origin: true,
          accessHeaders,
        });
        log(`cleanup: deleted collection ${id}`);
      } catch (err) {
        log(`cleanup: could not delete collection ${id}: ${err.message}`);
      }
      ctx.openIds.delete(id);
    }
  };

  let exiting = false;
  const onSignal = (sig) => {
    if (exiting) return;
    exiting = true;
    log(`${sig}: cleaning up probe folders before exit`);
    cleanup().finally(() => process.exit(130));
  };
  process.on('SIGINT', () => onSignal('SIGINT'));
  process.on('SIGTERM', () => onSignal('SIGTERM'));

  const result = {
    base,
    quick: values.quick,
    probeName: ctx.probeName,
    startedAt: new Date().toISOString(),
  };
  try {
    const { latency, longStream, lastEventId } = await latencyAndLongStreamPhase(ctx, profile);
    result.latency = latency;
    result.longStream = longStream;
    result.resume = await resumePhase(ctx, profile, lastEventId);
    result.bearer = tokenData
      ? await bearerPhase(ctx, tokenData, profile.bearerCalls)
      : { skipped: 'no --token given' };
  } finally {
    await cleanup();
  }
  result.finishedAt = new Date().toISOString();
  result.pass = overallPass(result, values.quick);

  if (values.out) writeFileSync(values.out, JSON.stringify(result, null, 2));
  if (values.json) console.log(JSON.stringify(result, null, 2));
  else printHumanReport(result, values.quick);

  process.exitCode = result.pass ? 0 : 1;
}

function newProbeName() {
  return `zz-probe-${Date.now().toString(36)}${randomBytes(3).toString('hex')}`;
}

main().catch(async (err) => {
  console.error(`error: ${err.stack ?? err.message}`);
  process.exitCode = 1;
});
