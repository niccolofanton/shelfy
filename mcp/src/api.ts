import type { Config } from './config.js';

export class ApiError extends Error {
  constructor(
    public readonly code: string,
    public readonly status?: number,
    public readonly retryAfter?: number,
  ) {
    super(code);
    this.name = 'ApiError';
  }
}
export type Query = Record<string, string | number | boolean | string[] | undefined>;
export class ShelfyApi {
  constructor(private readonly config: Config) {}
  async request(
    method: string,
    path: string,
    options: { query?: Query; body?: unknown; signal?: AbortSignal } = {},
  ): Promise<unknown> {
    // Call sites have fixed routes; never follow an API-supplied media URL,
    // redirect, user URL or cookie with this bearer credential.
    if (!path.startsWith('/api/v1/') || path.includes('?') || path.includes('#'))
      throw new ApiError('invalid_route');
    const url = new URL(path, this.config.url);
    for (const [key, value] of Object.entries(options.query || {})) {
      if (Array.isArray(value)) for (const item of value) url.searchParams.append(key, item);
      else if (value !== undefined) url.searchParams.set(key, String(value));
    }
    if (
      method !== 'GET' &&
      !this.config.write &&
      !['/api/v1/posts/lookup', '/api/v1/posts/batch-get'].includes(path)
    )
      throw new ApiError('write_disabled', 403);
    const abort = AbortSignal.any([
      AbortSignal.timeout(25_000),
      ...(options.signal ? [options.signal] : []),
    ]);
    try {
      const response = await fetch(url, {
        method,
        redirect: 'error',
        credentials: 'omit',
        headers: {
          authorization: `Bearer ${this.config.token}`,
          accept: 'application/json',
          ...(this.config.access
            ? {
                'CF-Access-Client-Id': this.config.access.clientId,
                'CF-Access-Client-Secret': this.config.access.clientSecret,
              }
            : {}),
          ...(options.body !== undefined ? { 'content-type': 'application/json' } : {}),
        },
        body: options.body === undefined ? undefined : JSON.stringify(options.body),
        signal: abort,
      });
      const retry = response.headers.get('retry-after');
      const retryAfter = retry && /^\d{1,8}$/.test(retry) ? Number(retry) : undefined;
      if (!response.ok) {
        await response.body?.cancel();
        // Do not echo server problem details: they can reflect user text,
        // URLs or credentials. The host gets actionable, stable errors.
        throw new ApiError(
          (
            {
              401: 'unauthorized',
              403: 'forbidden',
              404: 'not_found',
              409: 'conflict',
              422: 'validation_failed',
              423: 'user_locked',
              429: 'rate_limited',
            } as Record<number, string>
          )[response.status] || 'server_error',
          response.status,
          retryAfter,
        );
      }
      if (response.status === 204) return { ok: true };
      if (!response.headers.get('content-type')?.toLowerCase().includes('application/json')) {
        await response.body?.cancel();
        throw new ApiError('invalid_response');
      }
      if (!response.body) throw new ApiError('invalid_response');
      const reader = response.body.getReader();
      const chunks: Uint8Array[] = [];
      let size = 0;
      try {
        while (true) {
          const { done, value } = await reader.read();
          if (done) break;
          size += value.byteLength;
          if (size > 2 * 1024 * 1024) {
            await reader.cancel();
            throw new ApiError('response_too_large');
          }
          chunks.push(value);
        }
      } finally {
        reader.releaseLock();
      }
      try {
        return JSON.parse(Buffer.concat(chunks).toString('utf8'));
      } catch {
        throw new ApiError('invalid_response');
      }
    } catch (error) {
      if (error instanceof ApiError) throw error;
      throw new ApiError(abort.aborted ? 'request_cancelled_or_timed_out' : 'server_unreachable');
    }
  }
}
