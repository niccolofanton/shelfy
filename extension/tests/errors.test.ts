// Error mapping of the API client (contract C1): what each answer becomes (sw/api.ts
// readResponse), what the worker does about it (sw/errors.ts classifyFailure), the panel code,
// the backoff and Retry-After.

import { describe, expect, it } from 'vitest';
import { opaqueRedirect } from '../scripts/fake-api';
import { ApiClient, buildRequest, readResponse } from '../src/sw/api';
import {
  BACKOFF_MAX_MS,
  backoffMs,
  classifyFailure,
  failureCode,
  parseRetryAfter,
  type ApiFailure,
} from '../src/sw/errors';
import { ORIGIN, T0 } from './helpers';

const problem = (status: number, code: string, extra: Record<string, unknown> = {}, headers = {}) =>
  new Response(JSON.stringify({ type: 'about:blank', title: code, status, code, ...extra }), {
    status,
    headers: { 'content-type': 'application/problem+json', ...headers },
  });

const http = (status: number, code: string | null = null, extra: Partial<ApiFailure> = {}) =>
  ({ kind: 'http', status, code, retryAfterMs: null, fields: [], ...extra }) as ApiFailure;

describe('readResponse', () => {
  it('reads JSON answers, 304s and the Idempotent-Replayed header', async () => {
    expect(
      await readResponse(
        new Response('{"a":1}', {
          status: 200,
          headers: {
            'content-type': 'application/json',
            etag: '"v1"',
            'idempotent-replayed': 'true',
          },
        }),
        T0,
      ),
    ).toEqual({ ok: true, status: 200, data: { a: 1 }, etag: '"v1"', replayed: true });
    expect(
      await readResponse(new Response(null, { status: 304, headers: { etag: '"v1"' } }), T0),
    ).toEqual({
      ok: true,
      status: 304,
      data: null,
      etag: '"v1"',
      replayed: false,
    });
    expect(await readResponse(new Response(null, { status: 204 }), T0)).toMatchObject({
      ok: true,
      data: null,
    });
  });

  it('turns an opaque redirect, or a 401/403 that is not our problem+json, into an Access redirect', async () => {
    expect(await readResponse(opaqueRedirect(), T0)).toEqual({
      ok: false,
      failure: { kind: 'access_redirect' },
    });
    expect(await readResponse(new Response('<html>Forbidden</html>', { status: 403 }), T0)).toEqual(
      { ok: false, failure: { kind: 'access_redirect' } },
    );
    expect(await readResponse(problem(401, 'unauthorized'), T0)).toMatchObject({
      ok: false,
      failure: { kind: 'http', status: 401, code: 'unauthorized' },
    });
  });

  it('reads the problem code, the fields and Retry-After', async () => {
    expect(
      await readResponse(
        problem(422, 'validation_failed', {
          errors: [{ field: 'Idempotency-Key', reason: 'reused' }],
        }),
        T0,
      ),
    ).toEqual({
      ok: false,
      failure: {
        kind: 'http',
        status: 422,
        code: 'validation_failed',
        retryAfterMs: null,
        fields: ['Idempotency-Key'],
      },
    });
    expect(
      await readResponse(problem(429, 'rate_limited', {}, { 'retry-after': '12' }), T0),
    ).toMatchObject({
      failure: { status: 429, retryAfterMs: 12_000 },
    });
    expect(await readResponse(new Response('oops', { status: 500 }), T0)).toMatchObject({
      failure: { kind: 'http', status: 500, code: null },
    });
  });
});

describe('classifyFailure (C1)', () => {
  it.each([
    [{ kind: 'unpaired' } as ApiFailure, { action: 'unpair' }],
    [http(401, 'unauthorized'), { action: 'unpair' }],
    [http(426, 'extension_outdated'), { action: 'outdated' }],
    [{ kind: 'access_redirect' } as ApiFailure, { action: 'access' }],
    [http(429, 'rate_limited', { retryAfterMs: 5_000 }), { action: 'wait', ms: 5_000 }],
    [http(429, 'rate_limited'), { action: 'wait', ms: 30_000 }],
    [http(503, 'unavailable'), { action: 'wait', ms: 30_000 }],
    [http(423, 'user_locked'), { action: 'wait', ms: 60_000 }],
    [http(500, 'internal'), { action: 'backoff' }],
    [http(502), { action: 'backoff' }],
    [http(504, 'timeout'), { action: 'backoff' }],
    [http(408), { action: 'backoff' }],
    [
      { kind: 'network', detail: 'TypeError: Failed to fetch' } as ApiFailure,
      { action: 'backoff' },
    ],
    [http(409, 'conflict'), { action: 'backoff' }],
    [http(409, 'source_disabled'), { action: 'disable_source' }],
    [http(404, 'sync_run_not_found'), { action: 'recreate_run' }],
    [http(404, 'not_found'), { action: 'hold', ms: 600_000, code: 'not_found' }],
    [http(413, 'payload_too_large'), { action: 'split' }],
    [http(422, 'validation_failed', { fields: ['Idempotency-Key'] }), { action: 'rekey' }],
    [http(422, 'validation_failed'), { action: 'drop', code: 'validation_failed' }],
    [http(400, 'bad_request'), { action: 'drop', code: 'bad_request' }],
    [http(410), { action: 'drop', code: 'gone' }],
    [http(403, 'forbidden'), { action: 'hold', ms: 600_000, code: 'forbidden' }],
    [http(405), { action: 'hold', ms: 600_000, code: 'method_not_allowed' }],
  ])('%j → %j', (failure, action) => {
    expect(classifyFailure(failure)).toEqual(action);
  });

  it('names failures for the panel', () => {
    expect(failureCode({ kind: 'network', detail: 'x' })).toBe('network');
    expect(failureCode({ kind: 'access_redirect' })).toBe('access_redirect');
    expect(failureCode({ kind: 'unpaired' })).toBe('unauthorized');
    expect(failureCode(http(401, 'unauthorized'))).toBe('unauthorized');
    expect(failureCode(http(426, 'extension_outdated'))).toBe('outdated');
    expect(failureCode(http(429))).toBe('rate_limited');
    expect(failureCode(http(503, 'unavailable'))).toBe('unavailable');
    expect(failureCode(http(500, 'internal'))).toBe('server');
    expect(failureCode(http(409, 'source_disabled'))).toBe('source_disabled');
    expect(failureCode(http(418))).toBe('http_418');
  });
});

describe('backoff and Retry-After', () => {
  it('doubles from 2 s up to 5 min, with jitter between half and all of the step', () => {
    expect([1, 2, 3, 4].map((n) => backoffMs(n, () => 0))).toEqual([1_000, 2_000, 4_000, 8_000]);
    expect([1, 2, 3, 4].map((n) => backoffMs(n, () => 0.999_999))).toEqual([
      2_000, 4_000, 8_000, 16_000,
    ]);
    expect(backoffMs(30, () => 0.999_999)).toBe(BACKOFF_MAX_MS);
    expect(backoffMs(30, () => 0)).toBe(BACKOFF_MAX_MS / 2);
    expect(backoffMs(0, () => 0)).toBe(1_000);
  });

  it('reads delta-seconds and HTTP dates, bounded to [1 s, 1 h]', () => {
    expect(parseRetryAfter('7', T0)).toBe(7_000);
    expect(parseRetryAfter('0', T0)).toBe(1_000);
    expect(parseRetryAfter('99999', T0)).toBe(3_600_000);
    expect(parseRetryAfter(new Date(T0 + 20_000).toUTCString(), T0)).toBe(20_000);
    expect(parseRetryAfter('soon', T0)).toBeNull();
    expect(parseRetryAfter(null, T0)).toBeNull();
  });
});

describe('buildRequest (C1 headers and credentials)', () => {
  const credentials = {
    token: 'shx_synthetictokensynthetictokensynthetictoken0',
    access: { clientId: 'id.access', clientSecret: 'secret' },
  };

  it('sends the token, the version, the Access headers and the key, with the cookie, toward Shelfy only', () => {
    const { url, init } = buildRequest(
      ORIGIN,
      'POST',
      '/api/v1/ingest/batches',
      { auth: 'token', body: { a: 1 }, idempotencyKey: 'KEY' },
      credentials,
      '0.2.0',
    );
    expect(url).toBe(`${ORIGIN}/api/v1/ingest/batches`);
    expect(init).toMatchObject({
      method: 'POST',
      credentials: 'include',
      redirect: 'manual',
      cache: 'no-store',
      body: '{"a":1}',
    });
    expect(init.headers).toEqual({
      Accept: 'application/json',
      'X-Shelfy-Extension': '0.2.0',
      Authorization: `Bearer ${credentials.token}`,
      'CF-Access-Client-Id': 'id.access',
      'CF-Access-Client-Secret': 'secret',
      'Idempotency-Key': 'KEY',
      'Content-Type': 'application/json',
    });
  });

  it('leaves the token off public routes and the Access headers off the cookie probe', () => {
    const { init } = buildRequest(
      ORIGIN,
      'POST',
      '/api/v1/extension/pair',
      { auth: 'none', omitAccessHeaders: true, etag: '"v2"' },
      credentials,
      '0.2.0',
    );
    expect(init.headers).toEqual({
      Accept: 'application/json',
      'X-Shelfy-Extension': '0.2.0',
      'If-None-Match': '"v2"',
    });
  });

  it('refuses any URL off the Shelfy origin, so the cookie never goes elsewhere', () => {
    for (const path of [
      'https://evil.example/api',
      '//evil.example/api',
      'https://scontent.cdninstagram.com/v/a.jpg',
    ])
      expect(() =>
        buildRequest(ORIGIN, 'GET', path, { auth: 'token' }, credentials, '0.2.0'),
      ).toThrow(/off the Shelfy origin/);
  });

  it('a token route without a token is not sent at all', async () => {
    let calls = 0;
    const client = new ApiClient({
      origin: ORIGIN,
      version: '0.2.0',
      fetch: async () => {
        calls += 1;
        return new Response('{}');
      },
      credentials: async () => ({ token: null, access: null }),
      now: () => T0,
    });
    expect(await client.get('/api/v1/extension/config', { auth: 'token' })).toEqual({
      ok: false,
      failure: { kind: 'unpaired' },
    });
    expect(calls).toBe(0);
    expect(await client.get('/health', { auth: 'none' })).toMatchObject({ ok: true });
    expect(calls).toBe(1);
  });

  it('reports a network failure or a timeout as `network`', async () => {
    const client = new ApiClient({
      origin: ORIGIN,
      version: '0.2.0',
      fetch: (_input, init) =>
        new Promise((_resolve, reject) =>
          init.signal?.addEventListener('abort', () =>
            reject(new DOMException('aborted', 'AbortError')),
          ),
        ),
      credentials: async () => ({ token: null, access: null }),
      now: () => T0,
    });
    expect(await client.get('/health', { auth: 'none', timeoutMs: 5 })).toEqual({
      ok: false,
      failure: { kind: 'network', detail: 'AbortError: aborted' },
    });
  });
});
