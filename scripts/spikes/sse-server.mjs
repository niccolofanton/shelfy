#!/usr/bin/env node
/**
 * SPIKE-10 test server: a dependency-free stand-in for the realtime stream of
 * plan §2.10 and for bearer-token API calls, used to check both through nginx
 * and a Cloudflare tunnel. Runs in `node:24-alpine`.
 *
 * Environment:
 *   SPIKE_TOKEN   required; accepted as `Authorization: Bearer <t>` or as the
 *                 cookie `spike_session=<t>` (the SPA uses a cookie, the
 *                 extension and the Shortcut a bearer token)
 *   PORT          default 8080
 *   HEARTBEAT_MS  comment heartbeat interval, default 20000
 *   RING_SIZE     events kept for Last-Event-ID resume, default 256
 *   RING_TTL_MS   max age of a resumable event, default 300000
 *
 * Routes:
 *   GET  /health          200 {ok}; no auth
 *   GET  /api/v1/events   text/event-stream: `hello` on connect, replay after
 *                         Last-Event-ID (header or ?lastEventId=), `resync`
 *                         when the gap cannot be served, typed events with
 *                         `id:`, a comment heartbeat. Query `hb=0` disables the
 *                         heartbeat and `xab=0` omits `X-Accel-Buffering: no`
 *                         (both are controls for the spike).
 *   POST /api/v1/emit     {type, data} → pushed to every open stream; {id}
 *   GET  /api/v1/me       bearer-protected JSON
 *   POST /api/v1/echo     bearer-protected; returns {bytes, sha256} of the body
 *   GET  /api/v1/stats    open and recently closed streams as the server saw them
 *
 * Logs one JSON line per stream open/close to stdout; never logs the token.
 */
import { createHash, timingSafeEqual } from 'node:crypto';
import http from 'node:http';

const PORT = Number(process.env.PORT ?? 8080);
const TOKEN = process.env.SPIKE_TOKEN ?? '';
const HEARTBEAT_MS = Number(process.env.HEARTBEAT_MS ?? 20000);
const RING_SIZE = Number(process.env.RING_SIZE ?? 256);
const RING_TTL_MS = Number(process.env.RING_TTL_MS ?? 300000);
const MAX_BODY = 20 * 1024 * 1024;

if (TOKEN.length < 16) {
  console.error('SPIKE_TOKEN (>= 16 chars) is required');
  process.exit(2);
}

const ring = [];
let nextId = 1;
let nextConn = 1;
const streams = new Set();
const closed = [];

const log = (event) => console.log(JSON.stringify({ t: new Date().toISOString(), ...event }));

function tokenMatches(candidate) {
  const a = Buffer.from(candidate ?? '');
  const b = Buffer.from(TOKEN);
  return a.length === b.length && timingSafeEqual(a, b);
}

function authorized(req) {
  const auth = req.headers.authorization ?? '';
  if (auth.startsWith('Bearer ') && tokenMatches(auth.slice(7))) return 'bearer';
  const cookie = (req.headers.cookie ?? '')
    .split(';')
    .map((c) => c.trim())
    .find((c) => c.startsWith('spike_session='));
  if (cookie && tokenMatches(cookie.slice('spike_session='.length))) return 'cookie';
  return null;
}

function json(res, status, body) {
  const payload = JSON.stringify(body);
  res.writeHead(status, {
    'Content-Type': 'application/json',
    'Content-Length': Buffer.byteLength(payload),
    'Cache-Control': 'no-store',
  });
  res.end(payload);
}

function pruneRing(now = Date.now()) {
  while (ring.length > RING_SIZE || (ring.length && now - ring[0].at > RING_TTL_MS)) ring.shift();
}

/** Events after `lastId`, or `{resync}` when the ring cannot fill the gap. */
function replayAfter(lastId) {
  pruneRing();
  const maxId = nextId - 1;
  if (!Number.isInteger(lastId) || lastId < 0) return { resync: 'invalid' };
  if (lastId > maxId) return { resync: 'unknown_id' };
  if (lastId === maxId) return { events: [] };
  const oldest = ring.length ? ring[0].id : nextId;
  if (lastId < oldest - 1) return { resync: 'gap' };
  return { events: ring.filter((e) => e.id > lastId) };
}

const frame = (e) => `id: ${e.id}\nevent: ${e.type}\ndata: ${JSON.stringify(e.data)}\n\n`;

function openStream(req, res, url, auth) {
  const heartbeat = url.searchParams.get('hb') !== '0';
  const xab = url.searchParams.get('xab') !== '0';
  const headers = {
    'Content-Type': 'text/event-stream; charset=utf-8',
    'Cache-Control': 'no-store',
    Connection: 'keep-alive',
  };
  if (xab) headers['X-Accel-Buffering'] = 'no';
  res.writeHead(200, headers);
  req.socket.setNoDelay(true);

  const conn = {
    id: nextConn++,
    openedAt: Date.now(),
    auth,
    heartbeat,
    xab,
    heartbeats: 0,
    events: 0,
    replayed: 0,
    res,
    timer: null,
  };
  streams.add(conn);
  res.write('retry: 2000\n\n');
  res.write(
    `event: hello\ndata: ${JSON.stringify({ conn: conn.id, lastId: nextId - 1, heartbeatMs: heartbeat ? HEARTBEAT_MS : 0 })}\n\n`,
  );

  const lastHeader = req.headers['last-event-id'] ?? url.searchParams.get('lastEventId');
  if (lastHeader != null && lastHeader !== '') {
    const replay = replayAfter(Number(lastHeader));
    if (replay.resync) {
      res.write(`event: resync\ndata: ${JSON.stringify({ reason: replay.resync })}\n\n`);
    } else {
      for (const e of replay.events) res.write(frame(e));
      conn.replayed = replay.events.length;
    }
  }
  if (heartbeat) {
    conn.timer = setInterval(() => {
      conn.heartbeats++;
      res.write(`: hb ${conn.heartbeats}\n\n`);
    }, HEARTBEAT_MS);
  }
  log({
    ev: 'stream_open',
    conn: conn.id,
    auth,
    heartbeat,
    xab,
    lastEventId: lastHeader ?? null,
    replayed: conn.replayed,
  });

  req.on('close', () => {
    clearInterval(conn.timer);
    streams.delete(conn);
    const summary = {
      conn: conn.id,
      durationS: Math.round((Date.now() - conn.openedAt) / 1000),
      heartbeat,
      xab,
      heartbeats: conn.heartbeats,
      events: conn.events,
      replayed: conn.replayed,
    };
    closed.push(summary);
    if (closed.length > 50) closed.shift();
    log({ ev: 'stream_close', ...summary });
  });
}

function emit(type, data) {
  const e = { id: nextId++, type, data, at: Date.now() };
  ring.push(e);
  pruneRing(e.at);
  const text = frame(e);
  for (const conn of streams) {
    conn.res.write(text);
    conn.events++;
  }
  return e.id;
}

function readBody(req, limit) {
  return new Promise((resolve, reject) => {
    const hash = createHash('sha256');
    const chunks = [];
    let bytes = 0;
    req.on('data', (chunk) => {
      bytes += chunk.length;
      if (bytes > limit) {
        reject(Object.assign(new Error('too_large'), { status: 413 }));
        req.destroy();
        return;
      }
      hash.update(chunk);
      if (bytes <= 64 * 1024) chunks.push(chunk);
    });
    req.on('end', () =>
      resolve({ bytes, sha256: hash.digest('hex'), head: Buffer.concat(chunks) }),
    );
    req.on('error', reject);
  });
}

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, 'http://spike.local');
  try {
    if (req.method === 'GET' && url.pathname === '/health') return json(res, 200, { ok: true });
    if (!url.pathname.startsWith('/api/v1/')) return json(res, 404, { error: 'not_found' });
    const auth = authorized(req);
    if (!auth) return json(res, 401, { error: 'unauthorized' });

    if (req.method === 'GET' && url.pathname === '/api/v1/events')
      return openStream(req, res, url, auth);
    if (req.method === 'POST' && url.pathname === '/api/v1/emit') {
      const body = await readBody(req, 64 * 1024);
      const { type = 'probe', data = {} } = JSON.parse(body.head.toString('utf8') || '{}');
      return json(res, 200, { id: emit(String(type), data) });
    }
    if (req.method === 'GET' && url.pathname === '/api/v1/me') {
      return json(res, 200, { ok: true, auth, user: 'spike', serverTime: Date.now() });
    }
    if (req.method === 'POST' && url.pathname === '/api/v1/echo') {
      const body = await readBody(req, MAX_BODY);
      return json(res, 200, { ok: true, auth, bytes: body.bytes, sha256: body.sha256 });
    }
    if (req.method === 'GET' && url.pathname === '/api/v1/stats') {
      const now = Date.now();
      return json(res, 200, {
        emitted: nextId - 1,
        ring: ring.length,
        open: [...streams].map((c) => ({
          conn: c.id,
          ageS: Math.round((now - c.openedAt) / 1000),
          heartbeat: c.heartbeat,
          xab: c.xab,
          heartbeats: c.heartbeats,
          events: c.events,
          replayed: c.replayed,
        })),
        closed,
      });
    }
    return json(res, 404, { error: 'not_found' });
  } catch (err) {
    if (!res.headersSent) json(res, err.status ?? 400, { error: err.message });
  }
});

// Streams are long-lived: no per-socket idle timeout on the Node side.
server.requestTimeout = 0;
server.timeout = 0;
server.keepAliveTimeout = 75_000;
server.listen(PORT, () =>
  log({ ev: 'listening', port: PORT, heartbeatMs: HEARTBEAT_MS, ringSize: RING_SIZE }),
);

for (const signal of ['SIGINT', 'SIGTERM']) {
  process.on(signal, () => {
    for (const conn of streams) conn.res.end();
    server.close(() => process.exit(0));
    setTimeout(() => process.exit(0), 2000).unref();
  });
}
