// The capture service HTTP server (plan §2.18): one streaming endpoint behind an
// internal token, plus a health probe. No framework — node:http + zod.
//
//   POST /v1/captures   X-Shelfy-Internal-Token: <secret>   → 200 application/x-ndjson
//   GET  /health                                            → { ok, slotsFree, browser, version }
//
// The API dispatches only when /health reports a free slot; closing the response
// stream cancels the capture (AbortController → the pages, contexts and ffmpeg die).

import http from 'http';
import { timingSafeEqual } from 'crypto';
import { isBrowserConnected } from '../../electron/webcap/browser';
import { ENV, configureCapture, type CaptureEnv } from './env';
import { CaptureRequestSchema, type CaptureLine } from './protocol';
import { runCapture, type EmitLine } from './run';
import { runFake } from './fake';

const VERSION = process.env.CAPTURE_VERSION || 'capture-service';

function constantTimeEqual(a: string, b: string): boolean {
  const ab = Buffer.from(a);
  const bb = Buffer.from(b);
  // timingSafeEqual needs equal lengths; compare against a fixed-length digest-ish
  // padding so length alone does not leak. Different lengths are never equal.
  if (ab.length !== bb.length) {
    // Still run a comparison to keep the timing uniform, then fail.
    timingSafeEqual(ab, ab);
    return false;
  }
  return timingSafeEqual(ab, bb);
}

function readBody(req: http.IncomingMessage, maxBytes = 16 * 1024): Promise<string> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    let n = 0;
    req.on('data', (c: Buffer) => {
      n += c.length;
      if (n > maxBytes) {
        reject(new Error('body too large'));
        req.destroy();
        return;
      }
      chunks.push(c);
    });
    req.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')));
    req.on('error', reject);
  });
}

function sendJson(res: http.ServerResponse, status: number, body: unknown): void {
  const s = JSON.stringify(body);
  res.writeHead(status, {
    'content-type': 'application/json',
    'content-length': Buffer.byteLength(s),
  });
  res.end(s);
}

export interface CaptureServerOptions {
  env?: CaptureEnv;
  // Injectable runner (tests / fake mode). Defaults to run.ts, or fake.ts when
  // env.fake is set.
  run?: (
    req: ReturnType<typeof CaptureRequestSchema.parse>,
    ctx: { emit: EmitLine; signal: AbortSignal },
  ) => Promise<void>;
}

export function createCaptureServer(opts: CaptureServerOptions = {}): http.Server {
  const env = opts.env ?? ENV;
  const run =
    opts.run ??
    ((req, ctx) => (env.fake ? runFake(req, { ...ctx, env }) : runCapture(req, { ...ctx, env })));
  let slotsUsed = 0;

  return http.createServer((req, res) => {
    const url = req.url || '';
    const method = req.method || 'GET';

    if (method === 'GET' && (url === '/health' || url === '/health/')) {
      sendJson(res, 200, {
        ok: true,
        slotsFree: Math.max(0, env.slots - slotsUsed),
        browser: env.fake ? false : isBrowserConnected(),
        version: VERSION,
      });
      return;
    }

    if (method !== 'POST' || url.split('?')[0] !== '/v1/captures') {
      sendJson(res, 404, { code: 'not_found' });
      return;
    }

    // Constant-time internal-token check (401 otherwise).
    const token = String(req.headers['x-shelfy-internal-token'] || '');
    if (!env.internalToken || !constantTimeEqual(token, env.internalToken)) {
      sendJson(res, 401, { code: 'unauthorized' });
      return;
    }

    // One site at a time (plan §2.18): 503 busy without a free slot.
    if (slotsUsed >= env.slots) {
      sendJson(res, 503, { code: 'busy' });
      return;
    }

    void handleCapture(req, res, run, env, {
      acquire: () => {
        slotsUsed++;
      },
      release: () => {
        slotsUsed = Math.max(0, slotsUsed - 1);
      },
    });
  });
}

async function handleCapture(
  req: http.IncomingMessage,
  res: http.ServerResponse,
  run: NonNullable<CaptureServerOptions['run']>,
  env: CaptureEnv,
  slot: { acquire: () => void; release: () => void },
): Promise<void> {
  let body: string;
  try {
    body = await readBody(req);
  } catch {
    sendJson(res, 413, { code: 'body_too_large' });
    return;
  }
  let json: unknown;
  try {
    json = JSON.parse(body);
  } catch {
    sendJson(res, 400, { code: 'bad_json' });
    return;
  }
  const parsed = CaptureRequestSchema.safeParse(json);
  if (!parsed.success) {
    sendJson(res, 400, {
      code: 'invalid_request',
      issues: parsed.error.issues.map((i) => i.path.join('.')),
    });
    return;
  }

  slot.acquire();
  res.writeHead(200, {
    'content-type': 'application/x-ndjson',
    'cache-control': 'no-store',
    'x-accel-buffering': 'no',
  });

  const ac = new AbortController();
  let finished = false;
  // Closing the response stream cancels the capture within 5 s (AbortController).
  res.on('close', () => {
    if (!finished) ac.abort();
  });

  // Wire-level guards (run.ts caps too; the server is the second line of defence):
  // at most env.maxEvents event lines, each at most env.maxLineBytes.
  let events = 0;
  const emit: EmitLine = (line: CaptureLine) => {
    if (res.writableEnded) return;
    if (line.type === 'event') {
      if (events >= env.maxEvents) return;
      events++;
    }
    const s = JSON.stringify(line);
    if (Buffer.byteLength(s) > env.maxLineBytes && line.type === 'event') return;
    res.write(s + '\n');
  };

  try {
    await run(parsed.data, { emit, signal: ac.signal });
  } catch {
    if (!res.writableEnded && !ac.signal.aborted) {
      res.write(JSON.stringify({ type: 'failed', code: 'internal' }) + '\n');
    }
  } finally {
    finished = true;
    slot.release();
    if (!res.writableEnded) res.end();
  }
}

// Configure the shared capture modules and start listening (called by main.ts,
// the bundle entry).
export function main(): http.Server {
  configureCapture(ENV);
  const server = createCaptureServer();
  server.listen(ENV.port, () => {
    console.log(`[capture] listening on :${ENV.port} (fake=${ENV.fake}, slots=${ENV.slots})`);
  });
  return server;
}
