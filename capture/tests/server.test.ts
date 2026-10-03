import { describe, it, expect, afterEach } from 'vitest';
import type { AddressInfo } from 'net';
import type http from 'http';
import { createCaptureServer, type CaptureServerOptions } from '../src/server';
import { ENV } from '../src/env';
import type { CaptureLine } from '../src/protocol';

const TOKEN = 'test-internal-token-0123456789abcdef';
const VALID = {
  captureId: '01J0000000000000000000000X',
  url: 'https://example.com/',
  maxPages: 6,
  singlePage: false,
  video: true,
  workDir: '/work/01J0000000000000000000000X',
};

const servers: http.Server[] = [];
afterEach(() => {
  for (const s of servers.splice(0)) s.close();
});

async function start(opts: CaptureServerOptions): Promise<{ url: string }> {
  const server = createCaptureServer({ env: { ...ENV, internalToken: TOKEN, slots: 1 }, ...opts });
  servers.push(server);
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const port = (server.address() as AddressInfo).port;
  return { url: `http://127.0.0.1:${port}` };
}

function post(url: string, body: unknown, token: string | null) {
  return fetch(`${url}/v1/captures`, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      ...(token ? { 'x-shelfy-internal-token': token } : {}),
    },
    body: JSON.stringify(body),
  });
}

const okRun: NonNullable<CaptureServerOptions['run']> = async (_req, ctx) => {
  ctx.emit({ type: 'event', kind: 'read', code: 'site.opening', params: { url: _req.url } });
  ctx.emit({ type: 'page', index: 0, url: _req.url, pageType: 'home', assets: [] });
  ctx.emit({ type: 'done', manifest: 'manifest.json', durationMs: 1, peakRssBytes: 1, bytes: 1 });
};

describe('GET /health', () => {
  it('reports ok, a free slot, browser and version', async () => {
    const { url } = await start({ run: okRun });
    const res = await fetch(`${url}/health`);
    expect(res.status).toBe(200);
    const body = await res.json();
    expect(body.ok).toBe(true);
    expect(body.slotsFree).toBe(1);
    expect(body).toHaveProperty('browser');
    expect(body).toHaveProperty('version');
  });
});

describe('POST /v1/captures — auth and validation', () => {
  it('401 without a token and with a wrong token', async () => {
    const { url } = await start({ run: okRun });
    expect((await post(url, VALID, null)).status).toBe(401);
    expect((await post(url, VALID, 'wrong')).status).toBe(401);
  });

  it('400 on an invalid body (workDir mismatch)', async () => {
    const { url } = await start({ run: okRun });
    const res = await post(url, { ...VALID, workDir: '/tmp/evil' }, TOKEN);
    expect(res.status).toBe(400);
  });

  it('streams NDJSON ending in done for a valid request', async () => {
    const { url } = await start({ run: okRun });
    const res = await post(url, VALID, TOKEN);
    expect(res.status).toBe(200);
    expect(res.headers.get('content-type')).toContain('application/x-ndjson');
    const text = await res.text();
    const lines = text
      .trim()
      .split('\n')
      .map((l) => JSON.parse(l) as CaptureLine);
    expect(lines[0].type).toBe('event');
    expect(lines.some((l) => l.type === 'page')).toBe(true);
    expect(lines.at(-1)?.type).toBe('done');
  });
});

describe('POST /v1/captures — one slot', () => {
  it('answers 503 busy while a capture holds the only slot', async () => {
    let release = (): void => {};
    const gate = new Promise<void>((r) => (release = r));
    const { url } = await start({
      run: async (_req, ctx) => {
        ctx.emit({ type: 'event', kind: 'info', code: 'busy.holding' });
        await gate;
        ctx.emit({
          type: 'done',
          manifest: 'manifest.json',
          durationMs: 1,
          peakRssBytes: 1,
          bytes: 1,
        });
      },
    });
    const first = post(url, VALID, TOKEN); // acquires the slot, then waits
    // Give the first request time to acquire the slot.
    await new Promise((r) => setTimeout(r, 100));
    const second = await post(url, VALID, TOKEN);
    expect(second.status).toBe(503);
    expect((await second.json()).code).toBe('busy');
    release();
    await (await first).text();
  });
});

describe('POST /v1/captures — caps', () => {
  it('drops events past the 250 cap and oversized event lines', async () => {
    const { url } = await start({
      env: { ...ENV, internalToken: TOKEN, slots: 1, maxEvents: 5, maxLineBytes: 200 },
      run: async (_req, ctx) => {
        for (let i = 0; i < 20; i++) ctx.emit({ type: 'event', kind: 'info', code: `e${i}` });
        // An oversized event line (big params) is dropped by the wire guard.
        ctx.emit({ type: 'event', kind: 'info', code: 'big', params: { x: 'y'.repeat(500) } });
        ctx.emit({
          type: 'done',
          manifest: 'manifest.json',
          durationMs: 1,
          peakRssBytes: 1,
          bytes: 1,
        });
      },
    });
    const text = await (await post(url, VALID, TOKEN)).text();
    const lines = text
      .trim()
      .split('\n')
      .map((l) => JSON.parse(l) as CaptureLine);
    const events = lines.filter((l) => l.type === 'event');
    expect(events.length).toBeLessThanOrEqual(5);
    expect(events.some((e) => e.type === 'event' && e.code === 'big')).toBe(false);
    expect(lines.at(-1)?.type).toBe('done');
  });
});

describe('POST /v1/captures — abort on stream close', () => {
  it('aborts the capture within a few seconds when the client disconnects', async () => {
    let aborted = false;
    const { url } = await start({
      run: (_req, ctx) =>
        new Promise<void>((resolve) => {
          ctx.emit({ type: 'event', kind: 'info', code: 'started' });
          ctx.signal.addEventListener('abort', () => {
            aborted = true;
            resolve();
          });
        }),
    });
    const ac = new AbortController();
    const res = await fetch(`${url}/v1/captures`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'x-shelfy-internal-token': TOKEN },
      body: JSON.stringify(VALID),
      signal: ac.signal,
    });
    const reader = res.body!.getReader();
    await reader.read(); // the "started" line
    ac.abort(); // client disconnects
    await reader.cancel().catch(() => {});
    // The server's res 'close' fires the AbortController within a few seconds.
    for (let i = 0; i < 50 && !aborted; i++) await new Promise((r) => setTimeout(r, 100));
    expect(aborted).toBe(true);
  });
});
