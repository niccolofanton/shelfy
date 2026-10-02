#!/usr/bin/env node
/**
 * SPIKE-10 client: measures `sse-server.mjs` through whatever sits in front of
 * it (nginx, a Cloudflare tunnel). All timings are taken on this machine, so
 * clock skew between client and server does not matter.
 *
 * Phases:
 *   A  latency    --events POSTs to /api/v1/emit, --interval ms apart; latency
 *                 = POST start → the event read from the open stream
 *   B  idle       --idle seconds with no events: the main stream must survive
 *                 on heartbeats alone; a second stream opened with `hb=0` shows
 *                 what the path does to a silent connection (control)
 *   C  tail       --tail more events on the same stream after the idle period
 *   D  resume     close the stream, emit --replay events, reconnect with
 *                 Last-Event-ID: every missed event must arrive once, in order;
 *                 an unknown id must get `resync`
 *   E  bearer     JSON calls with a bearer token and several User-Agents, two
 *                 large uploads, and one call without a token (expects 401)
 *   X  xab        optional (--xab-control): a stream without
 *                 `X-Accel-Buffering: no`, to show what buffering does
 *
 * --phases picks a subset (default A,B,C,D,E). When the path does not deliver
 * the `hello` event within 15 s, A, C and D are skipped and reported as such;
 * B still records how long the path keeps the undelivered stream open. Node's
 * fetch (undici) drops a body that delivers no byte for 300 s
 * (`UND_ERR_BODY_TIMEOUT`); with a 20 s heartbeat that only happens on a path
 * that buffers the stream.
 *
 * A Cloudflare quick tunnel (trycloudflare.com) is such a path: it buffers
 * `text/event-stream` bodies, so only B and E are meaningful through one.
 *
 * Extra request headers (e.g. a Cloudflare Access service token) come from
 * SPIKE_HEADERS, one `Name: value` per line, so secrets stay off the command
 * line. They go on every request, including the no-token check, which only
 * drops the app's bearer token.
 *
 * Usage:
 *   SPIKE_TOKEN=... [SPIKE_HEADERS=...] node scripts/spikes/sse-probe.mjs --base https://host
 *     [--events 60] [--interval 2000] [--idle 240] [--tail 10] [--replay 10]
 *     [--bearer 10] [--upload-mb 8,16] [--xab-control] [--phases A,B,C,D,E]
 *     [--out result.json]
 */
import { createHash, randomBytes } from 'node:crypto';
import { writeFileSync } from 'node:fs';
import { setTimeout as sleep } from 'node:timers/promises';
import { parseArgs } from 'node:util';

const { values: args } = parseArgs({
  options: {
    base: { type: 'string' },
    events: { type: 'string', default: '60' },
    interval: { type: 'string', default: '2000' },
    idle: { type: 'string', default: '240' },
    tail: { type: 'string', default: '10' },
    replay: { type: 'string', default: '10' },
    bearer: { type: 'string', default: '10' },
    'upload-mb': { type: 'string', default: '8,16' },
    'xab-control': { type: 'boolean', default: false },
    phases: { type: 'string', default: 'A,B,C,D,E' },
    out: { type: 'string' },
  },
});
const PHASES = new Set(args.phases.split(',').map((p) => p.trim().toUpperCase()));
const TOKEN = process.env.SPIKE_TOKEN ?? '';
const EXTRA_HEADERS = Object.fromEntries(
  (process.env.SPIKE_HEADERS ?? '')
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.includes(':'))
    .map((line) => {
      const i = line.indexOf(':');
      return [line.slice(0, i).trim().toLowerCase(), line.slice(i + 1).trim()];
    }),
);
if (!args.base || !TOKEN) {
  console.error('usage: SPIKE_TOKEN=... sse-probe.mjs --base https://host [options]');
  process.exit(2);
}

const BASE = args.base.replace(/\/$/, '');
const EVENTS = Number(args.events);
const INTERVAL_MS = Number(args.interval);
const IDLE_S = Number(args.idle);
const TAIL = Number(args.tail);
const REPLAY = Number(args.replay);
const BEARER = Number(args.bearer);
const UPLOAD_MB = args['upload-mb'] ? args['upload-mb'].split(',').map(Number).filter(Boolean) : [];
const CHROME_UA =
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 ' +
  '(KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36';
const USER_AGENTS = {
  chrome: CHROME_UA,
  ios_shortcuts: 'Shortcuts/4046.0.2 CFNetwork/3826.500.131 Darwin/25.0.0',
  cli: 'shelfy-migrate/0.1.0',
  fetch_default: null,
};
const HEADER_KEYS = [
  'content-type',
  'content-encoding',
  'cache-control',
  'x-accel-buffering',
  'server',
  'cf-cache-status',
  'cf-mitigated',
  'transfer-encoding',
  'alt-svc',
];

const now = () => performance.now();
const round = (v) => (v == null ? null : Math.round(v));
const watchdogMs = (EVENTS * INTERVAL_MS + IDLE_S * 1000 + 10 * 60 * 1000) | 0;
setTimeout(() => {
  console.error('watchdog: giving up');
  process.exit(3);
}, watchdogMs).unref();

function pickHeaders(headers) {
  const out = {};
  for (const key of HEADER_KEYS) if (headers.get(key) != null) out[key] = headers.get(key);
  out['cf-ray'] = headers.get('cf-ray') ? 'present' : 'absent';
  return out;
}

function pct(values, p) {
  const v = values.filter((x) => x != null).sort((a, b) => a - b);
  if (!v.length) return null;
  return round(v[Math.min(v.length - 1, Math.max(0, Math.ceil((p / 100) * v.length) - 1))]);
}

function dist(values) {
  const v = values.filter((x) => x != null);
  return {
    n: values.length,
    delivered: v.length,
    p50: pct(v, 50),
    p95: pct(v, 95),
    p99: pct(v, 99),
    max: v.length ? round(Math.max(...v)) : null,
    min: v.length ? round(Math.min(...v)) : null,
  };
}

function looksLikeChallenge(status, contentType, text, headers) {
  if (headers?.get?.('cf-mitigated')) return 'cf-mitigated';
  const body = (text ?? '').slice(0, 4096).toLowerCase();
  if (/cf-chl|challenge-platform|just a moment|attention required|captcha/.test(body))
    return 'challenge_page';
  if (/text\/html/.test(contentType ?? '') && status !== 200) return 'html_error';
  return null;
}

/** A Server-Sent Events reader over fetch, with arrival timestamps. */
class Stream {
  constructor(name, query = '', headers = {}) {
    this.name = name;
    this.url = `${BASE}/api/v1/events${query}`;
    this.extraHeaders = headers;
    this.messages = [];
    this.heartbeats = [];
    this.waiters = new Set();
    this.lastEventId = null;
    this.lastDataAt = null;
    this.closedAt = null;
    this.endReason = null;
    this.bytes = 0;
    this.chunks = 0;
    this.ctrl = new AbortController();
  }

  async open() {
    this.openedAt = now();
    try {
      const res = await fetch(this.url, {
        headers: {
          accept: 'text/event-stream',
          'accept-encoding': 'gzip, deflate, br',
          'user-agent': CHROME_UA,
          cookie: `spike_session=${TOKEN}`,
          ...EXTRA_HEADERS,
          ...this.extraHeaders,
        },
        signal: this.ctrl.signal,
      });
      this.status = res.status;
      this.headers = pickHeaders(res.headers);
      this.headersMs = round(now() - this.openedAt);
      if (!res.ok || !res.body) {
        this.endReason = `http_${res.status}`;
        this.closedAt = now();
        this.done = Promise.resolve();
      } else {
        this.done = this.pump(res.body);
      }
    } catch (err) {
      this.endReason = `error:${err.cause?.code ?? err.name}`;
      this.closedAt = now();
      this.done = Promise.resolve();
    }
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
        const at = now();
        this.bytes += value.length;
        this.chunks++;
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
      this.closedAt = now();
    }
  }

  dispatch(block, at) {
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
    if (comment !== null && !data.length && id === null) {
      this.heartbeats.push(at);
      this.lastDataAt = at;
      return;
    }
    if (!data.length) return; // e.g. a `retry:`-only block
    if (id !== null) this.lastEventId = id;
    let parsed = data.join('\n');
    try {
      parsed = JSON.parse(parsed);
    } catch {}
    const msg = { id: id === null ? null : Number(id), type, data: parsed, at };
    this.messages.push(msg);
    this.lastDataAt = at;
    for (const waiter of this.waiters) waiter(msg);
  }

  waitFor(predicate, timeoutMs) {
    const found = this.messages.find(predicate);
    if (found) return Promise.resolve(found);
    return new Promise((resolve) => {
      const waiter = (msg) => {
        if (!predicate(msg)) return;
        cleanup();
        resolve(msg);
      };
      const timer = setTimeout(() => {
        cleanup();
        resolve(null);
      }, timeoutMs);
      const cleanup = () => {
        clearTimeout(timer);
        this.waiters.delete(waiter);
      };
      this.waiters.add(waiter);
    });
  }

  async close() {
    this.ctrl.abort();
    await this.done;
  }

  summary() {
    const gaps = this.heartbeats.slice(1).map((at, i) => at - this.heartbeats[i]);
    return {
      status: this.status ?? null,
      headers: this.headers ?? null,
      headersMs: this.headersMs ?? null,
      openForS: round(((this.closedAt ?? now()) - this.openedAt) / 1000),
      stillOpen: this.closedAt == null,
      endReason: this.endReason,
      messages: this.messages.length,
      heartbeats: this.heartbeats.length,
      heartbeatGapS: { min: round(Math.min(...gaps) / 1000), max: round(Math.max(...gaps) / 1000) },
      bytes: this.bytes,
      chunks: this.chunks,
    };
  }
}

async function call(method, path, { body, ua = CHROME_UA, token = TOKEN, contentType } = {}) {
  const headers = { accept: 'application/json', ...EXTRA_HEADERS };
  if (ua) headers['user-agent'] = ua;
  if (token) headers.authorization = `Bearer ${token}`;
  let payload;
  if (body !== undefined) {
    payload = Buffer.isBuffer(body) ? body : JSON.stringify(body);
    headers['content-type'] = contentType ?? 'application/json';
  }
  const t0 = now();
  try {
    const res = await fetch(BASE + path, { method, headers, body: payload });
    const text = await res.text();
    const ms = round(now() - t0);
    const ct = res.headers.get('content-type') ?? '';
    let json = null;
    try {
      json = JSON.parse(text);
    } catch {}
    return {
      status: res.status,
      ms,
      contentType: ct,
      json,
      challenge: looksLikeChallenge(res.status, ct, text, res.headers),
      server: res.headers.get('server'),
      cfRay: Boolean(res.headers.get('cf-ray')),
    };
  } catch (err) {
    return { status: null, ms: round(now() - t0), error: err.cause?.code ?? err.name };
  }
}

const emit = (type, data) => call('POST', '/api/v1/emit', { body: { type, data } });

/** Emits one event and measures POST start → arrival on each stream. */
async function timedEmit(streams, phase, seq) {
  const t0 = now();
  const post = await emit('probe', { phase, seq });
  const postMs = round(now() - t0);
  const arrivals = await Promise.all(
    streams.map((s) =>
      s.waitFor(
        (m) => m.type === 'probe' && m.data?.phase === phase && m.data?.seq === seq,
        10_000,
      ),
    ),
  );
  return {
    seq,
    id: post.json?.id ?? null,
    postStatus: post.status,
    postMs,
    latencyMs: arrivals.map((m) => (m ? round(m.at - t0) : null)),
  };
}

const t0Run = now();
const log = (...parts) => console.error(`[${((now() - t0Run) / 1000).toFixed(1)}s]`, ...parts);
const result = {
  base: BASE,
  startedAt: new Date().toISOString(),
  params: { ...args, base: undefined },
};

// ---- open the main stream and the silent control stream -------------------
const streamPhases = ['A', 'B', 'C', 'D'].some((p) => PHASES.has(p));
let delivered = false;
if (streamPhases) {
  const main = await new Stream('main').open();
  const control =
    PHASES.has('B') && IDLE_S > 0 ? await new Stream('control', '?hb=0').open() : null;
  const hello = await main.waitFor((m) => m.type === 'hello', 15_000);
  delivered = Boolean(hello);
  log('main stream', main.status, main.headers, hello ? 'hello received' : 'NO hello');
  result.open = {
    status: main.status,
    headers: main.headers,
    headersMs: main.headersMs,
    helloMs: hello ? round(hello.at - main.openedAt) : null,
    delivered,
  };
  if (control && delivered) await control.waitFor((m) => m.type === 'hello', 15_000);
  if (!delivered) {
    // Phases A, C and D need events to reach the client; B still shows how long
    // the path keeps an undelivered stream open.
    result.skipped = ['A', 'C', 'D'].filter((p) => PHASES.has(p));
    log(
      'events are not delivered on this path',
      result.skipped.length ? `; skipping ${result.skipped}` : '',
    );
  }

  // ---- A: latency ----------------------------------------------------------
  if (delivered && PHASES.has('A')) {
    const phaseA = [];
    for (let seq = 1; seq <= EVENTS; seq++) {
      const slot = now();
      phaseA.push(await timedEmit(control ? [main, control] : [main], 'A', seq));
      await sleep(Math.max(0, INTERVAL_MS - (now() - slot)));
    }
    result.latency = {
      main: dist(phaseA.map((r) => r.latencyMs[0])),
      control: control ? dist(phaseA.map((r) => r.latencyMs[1])) : null,
      post: dist(phaseA.map((r) => r.postMs)),
      postErrors: phaseA.filter((r) => r.postStatus !== 200).length,
    };
    log('A latency', result.latency.main, 'post', result.latency.post);
  }

  // ---- B: idle -------------------------------------------------------------
  if (PHASES.has('B') && IDLE_S > 0) {
    const idleStart = now();
    const hbBefore = main.heartbeats.length;
    log(`B idle for ${IDLE_S} s`);
    while (now() - idleStart < IDLE_S * 1000) {
      await sleep(5000);
      if (main.closedAt) break;
    }
    const controlLastData = control.lastDataAt ?? control.openedAt;
    result.idle = {
      seconds: round((now() - idleStart) / 1000),
      mainStillOpen: main.closedAt == null,
      mainEndReason: main.endReason,
      mainOpenForS: round(((main.closedAt ?? now()) - main.openedAt) / 1000),
      mainHeartbeatsDuringIdle: main.heartbeats.length - hbBefore,
      controlStillOpen: control.closedAt == null,
      controlEndReason: control.endReason,
      controlOpenForS: round(((control.closedAt ?? now()) - control.openedAt) / 1000),
      controlSilentForS: round(((control.closedAt ?? now()) - controlLastData) / 1000),
    };
    log('B idle', result.idle);
  }

  // ---- C: tail after idle --------------------------------------------------
  if (delivered && PHASES.has('C') && TAIL > 0) {
    const phaseC = [];
    const tailStreams = [main, ...(control && control.closedAt == null ? [control] : [])];
    for (let seq = 1; seq <= TAIL; seq++) {
      phaseC.push(await timedEmit(tailStreams, 'C', seq));
      await sleep(1000);
    }
    result.tail = {
      main: dist(phaseC.map((r) => r.latencyMs[0])),
      control: tailStreams.length > 1 ? dist(phaseC.map((r) => r.latencyMs[1])) : null,
    };
    log('C tail', result.tail);
  }
  result.mainStream = main.summary();
  log('main stream', result.mainStream);
  await main.close();
  if (control) await control.close();

  // ---- D: resume with Last-Event-ID ---------------------------------------
  if (delivered && PHASES.has('D')) {
    const lastSeen = Number(main.lastEventId);
    const missed = [];
    for (let seq = 1; seq <= REPLAY; seq++) {
      missed.push((await emit('probe', { phase: 'D', seq })).json?.id ?? null);
    }
    const resumed = await new Stream('resumed', '', { 'last-event-id': String(lastSeen) }).open();
    await resumed.waitFor(
      (m) => m.type === 'probe' && m.data?.phase === 'D' && m.data?.seq === REPLAY,
      15_000,
    );
    const replayedIds = resumed.messages
      .filter((m) => m.type === 'probe' && m.data?.phase === 'D')
      .map((m) => m.id);
    const live = [];
    for (let seq = 1; seq <= 3; seq++) live.push(await timedEmit([resumed], 'D-live', seq));
    await resumed.close();
    const maxId = Math.max(...live.map((r) => r.id ?? 0));
    const unknown = await new Stream('unknown-id', '', {
      'last-event-id': String(maxId + 1000),
    }).open();
    const resync = await unknown.waitFor((m) => m.type === 'resync', 10_000);
    await unknown.close();
    result.resume = {
      lastSeen,
      emittedWhileClosed: missed,
      replayedIds,
      exactReplay: JSON.stringify(replayedIds) === JSON.stringify(missed),
      duplicates: replayedIds.length - new Set(replayedIds).size,
      liveAfterResumeMs: live.map((r) => r.latencyMs[0]),
      unknownIdGotResync: Boolean(resync),
      resyncReason: resync?.data?.reason ?? null,
    };
    log('D resume', result.resume);
  }
}

// ---- E: bearer calls ------------------------------------------------------
async function bearerPhase() {
  const bearer = [];
  for (const [uaName, ua] of Object.entries(USER_AGENTS)) {
    for (let i = 0; i < BEARER; i++)
      bearer.push({ uaName, kind: 'GET me', ...(await call('GET', '/api/v1/me', { ua })) });
    for (let i = 0; i < 3; i++) {
      const body = { items: Array.from({ length: 20 }, (_, n) => ({ n, text: 'x'.repeat(80) })) };
      bearer.push({
        uaName,
        kind: 'POST echo json',
        ...(await call('POST', '/api/v1/echo', { ua, body })),
      });
    }
  }
  const uploads = [];
  for (const mb of UPLOAD_MB) {
    const blob = randomBytes(mb * 1024 * 1024);
    const sha256 = createHash('sha256').update(blob).digest('hex');
    const r = await call('POST', '/api/v1/echo', {
      body: blob,
      contentType: 'application/octet-stream',
    });
    uploads.push({
      mb,
      status: r.status,
      ms: r.ms,
      intact: r.json?.sha256 === sha256,
      challenge: r.challenge,
      error: r.error ?? null,
    });
  }
  const noToken = await call('GET', '/api/v1/me', { token: null });
  const byUa = {};
  for (const r of bearer) {
    const k = `${r.uaName} / ${r.kind}`;
    byUa[k] ??= { calls: 0, ok200Json: 0, challenges: 0, other: {}, ms: [] };
    const b = byUa[k];
    b.calls++;
    b.ms.push(r.ms);
    if (r.status === 200 && /application\/json/.test(r.contentType) && r.json?.ok && !r.challenge)
      b.ok200Json++;
    else b.other[r.error ?? r.status] = (b.other[r.error ?? r.status] ?? 0) + 1;
    if (r.challenge) b.challenges++;
  }
  for (const b of Object.values(byUa)) b.ms = dist(b.ms);
  result.bearer = {
    calls: bearer.length,
    ok200Json: bearer.filter((r) => r.status === 200 && r.json?.ok && !r.challenge).length,
    challenges: bearer.filter((r) => r.challenge).length,
    byUa,
    uploads,
    noToken: {
      status: noToken.status,
      contentType: noToken.contentType,
      body: noToken.json,
      challenge: noToken.challenge,
    },
  };
  log('E bearer', {
    calls: result.bearer.calls,
    ok: result.bearer.ok200Json,
    challenges: result.bearer.challenges,
    uploads,
    noToken: result.bearer.noToken,
  });
}
if (PHASES.has('E')) await bearerPhase();

// ---- X: buffering control -------------------------------------------------
if (args['xab-control']) {
  const unbuffered = await new Stream('xab-off', '?xab=0&hb=0').open();
  const xhello = await unbuffered.waitFor((m) => m.type === 'hello', 10_000);
  const xs = [];
  for (let seq = 1; seq <= 5; seq++) {
    const t0 = now();
    await emit('probe', { phase: 'X', seq });
    xs.push(t0);
    await sleep(1000);
  }
  await sleep(5000);
  const arrived = xs.map((t0, i) => {
    const m = unbuffered.messages.find((msg) => msg.data?.phase === 'X' && msg.data?.seq === i + 1);
    return m ? round(m.at - t0) : null;
  });
  await unbuffered.close();
  result.xabControl = {
    helloMs: xhello ? round(xhello.at - unbuffered.openedAt) : null,
    latencyMs: arrived,
    headers: unbuffered.headers,
  };
  log('X control (no X-Accel-Buffering)', result.xabControl);
}

result.serverStats = (await call('GET', '/api/v1/stats')).json;
result.finishedAt = new Date().toISOString();
result.durationS = round((now() - t0Run) / 1000);
if (args.out) writeFileSync(args.out, JSON.stringify(result, null, 2));
console.log(JSON.stringify(result, null, 2));
