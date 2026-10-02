import { describe, it, expect, vi } from 'vitest';
import { ApiError, createHttp, isApiError } from '../src/api/http';

function json(status: number, body: unknown, headers: Record<string, string> = {}): Response {
  return new Response(body === undefined ? null : JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json', ...headers },
  });
}

describe('http', () => {
  it('sends safe requests same-origin, without the client header', async () => {
    const fetch = vi.fn().mockResolvedValue(json(200, { ok: true }));
    const http = createHttp({ fetch });
    const body = await http.get('/api/v1/stats', new URLSearchParams({ a: '1' }));
    expect(body).toEqual({ ok: true });
    const [url, init] = fetch.mock.calls[0];
    expect(url).toBe('/api/v1/stats?a=1');
    expect(init.method).toBe('GET');
    expect(init.credentials).toBe('same-origin');
    expect(init.headers['X-Shelfy-Client']).toBeUndefined();
  });

  it('marks every state-changing request for the CSRF guard', async () => {
    const fetch = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
    const http = createHttp({ fetch });
    await http.send('POST', '/api/v1/auth/magic-links/redeem', { token: 't' });
    await http.send('POST', '/api/v1/auth/logout');
    for (const [, init] of fetch.mock.calls) {
      expect(init.headers['X-Shelfy-Client']).toBe('web');
    }
    expect(fetch.mock.calls[0][1].headers['Content-Type']).toBe('application/json');
    expect(fetch.mock.calls[0][1].body).toBe('{"token":"t"}');
    expect(fetch.mock.calls[1][1].body).toBeUndefined();
  });

  it('turns a problem into an ApiError with its code', async () => {
    const fetch = vi
      .fn()
      .mockResolvedValue(
        json(
          429,
          { type: 'about:blank', title: 'Too Many Requests', status: 429, code: 'rate_limited' },
          { 'Retry-After': '30' },
        ),
      );
    const err = await createHttp({ fetch })
      .send('POST', '/api/v1/auth/magic-links', { email: 'a@b.test' })
      .catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect(isApiError(err, 'rate_limited')).toBe(true);
    expect((err as ApiError).status).toBe(429);
    expect((err as ApiError).retryAfter).toBe(30);
  });

  it('maps an answer that is not a problem by its status', async () => {
    const page = (status: number) => new Response('<html>proxy page</html>', { status });
    const fetch = vi.fn();
    for (const status of [502, 401, 403, 404, 429, 400]) fetch.mockResolvedValueOnce(page(status));
    const http = createHttp({ fetch });
    const codes = [];
    for (let i = 0; i < 6; i++) {
      codes.push(await http.get('/api/v1/stats').catch((e: ApiError) => e.code));
    }
    expect(codes).toEqual([
      'unavailable',
      'unauthorized',
      'forbidden',
      'not_found',
      'rate_limited',
      'bad_request',
    ]);
  });

  it('reports a request that got no answer as a network error', async () => {
    const fetch = vi.fn().mockRejectedValue(new TypeError('Failed to fetch'));
    const err = await createHttp({ fetch })
      .get('/api/v1/me')
      .catch((e: unknown) => e);
    expect(isApiError(err, 'network')).toBe(true);
  });

  it('lets an aborted request reject with the abort itself', async () => {
    const controller = new AbortController();
    controller.abort();
    const abort = new DOMException('Aborted', 'AbortError');
    const fetch = vi.fn().mockRejectedValue(abort);
    const err = await createHttp({ fetch })
      .get('/api/v1/posts', undefined, controller.signal)
      .catch((e: unknown) => e);
    expect(err).toBe(abort);
  });

  it('tells the listeners when the session is gone', async () => {
    const fetch = vi.fn().mockResolvedValue(
      json(401, {
        type: 'about:blank',
        title: 'Unauthorized',
        status: 401,
        code: 'unauthorized',
      }),
    );
    const http = createHttp({ fetch });
    const listener = vi.fn();
    const off = http.onUnauthorized(listener);
    await http.get('/api/v1/posts').catch(() => {});
    expect(listener).toHaveBeenCalledTimes(1);
    off();
    await http.get('/api/v1/posts').catch(() => {});
    expect(listener).toHaveBeenCalledTimes(1);
  });
});
