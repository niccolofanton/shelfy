// The web app's transport: same-origin `fetch` with the session cookie, the
// headers the server's CSRF guard requires, and errors as ApiError.
import type { components } from './schema';

export type Problem = components['schemas']['Problem'];
export type ErrorCode = components['schemas']['ErrorCode'];

// Header every state-changing request carries (crates/server/src/auth/csrf.rs).
export const CLIENT_HEADER = 'X-Shelfy-Client';
export const CLIENT_WEB = 'web';

// A failed request. `code` is the server's stable problem code, or `network`
// when no answer came back (offline, server down, aborted…).
export class ApiError extends Error {
  readonly status: number;
  readonly code: ErrorCode | 'network';
  // Seconds to wait before retrying, from `Retry-After` (429).
  readonly retryAfter: number | null;

  constructor(
    status: number,
    code: ErrorCode | 'network',
    detail?: string,
    retryAfter: number | null = null,
  ) {
    super(detail ? `${code}: ${detail}` : code);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
    this.retryAfter = retryAfter;
  }
}

export function isApiError(err: unknown, code?: ApiError['code']): err is ApiError {
  return err instanceof ApiError && (code == null || err.code === code);
}

type UnsafeMethod = 'POST' | 'PUT' | 'PATCH' | 'DELETE';

export interface Http {
  // GET `path` (with `query`) and parse the JSON answer.
  get<T>(path: string, query?: URLSearchParams, signal?: AbortSignal): Promise<T>;
  // A state-changing request with an optional JSON body; resolves on 2xx.
  send(method: UnsafeMethod, path: string, body?: unknown): Promise<Response>;
  // Called whenever the server answers 401: the session is gone. Returns the
  // unsubscribe function.
  onUnauthorized(listener: () => void): () => void;
}

export interface HttpOptions {
  // The fetch to use (tests); defaults to the browser's.
  fetch?: typeof fetch;
}

async function errorOf(res: Response): Promise<ApiError> {
  let problem: Partial<Problem> = {};
  try {
    problem = (await res.json()) as Partial<Problem>;
  } catch {
    /* not a problem document (a proxy error page…) */
  }
  const retry = Number(res.headers.get('Retry-After'));
  return new ApiError(
    res.status,
    problem.code ?? (res.status >= 500 ? 'unavailable' : 'bad_request'),
    problem.detail,
    Number.isFinite(retry) && retry > 0 ? retry : null,
  );
}

export function createHttp({ fetch: fetchImpl }: HttpOptions = {}): Http {
  // Called through a wrapper: a detached `window.fetch` throws "Illegal invocation".
  const doFetch: typeof fetch = fetchImpl ?? ((input, init) => fetch(input, init));
  const unauthorized = new Set<() => void>();

  async function request(
    method: 'GET' | UnsafeMethod,
    path: string,
    init: { query?: URLSearchParams; body?: unknown; signal?: AbortSignal } = {},
  ): Promise<Response> {
    const qs = init.query?.toString();
    const headers: Record<string, string> = { Accept: 'application/json' };
    if (method !== 'GET') headers[CLIENT_HEADER] = CLIENT_WEB;
    if (init.body !== undefined) headers['Content-Type'] = 'application/json';
    let res: Response;
    try {
      res = await doFetch(qs ? `${path}?${qs}` : path, {
        method,
        headers,
        body: init.body !== undefined ? JSON.stringify(init.body) : undefined,
        credentials: 'same-origin',
        signal: init.signal,
      });
    } catch (err) {
      if (init.signal?.aborted) throw err;
      throw new ApiError(0, 'network', err instanceof Error ? err.message : undefined);
    }
    if (res.status === 401) unauthorized.forEach((listener) => listener());
    if (!res.ok) throw await errorOf(res);
    return res;
  }

  return {
    async get<T>(path: string, query?: URLSearchParams, signal?: AbortSignal): Promise<T> {
      const res = await request('GET', path, { query, signal });
      return (await res.json()) as T;
    },
    send: (method, path, body) => request(method, path, { body }),
    onUnauthorized(listener) {
      unauthorized.add(listener);
      return () => {
        unauthorized.delete(listener);
      };
    },
  };
}
