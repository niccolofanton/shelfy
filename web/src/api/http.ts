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

export interface SendOptions {
  // Let the request outlive the page (fetch `keepalive`): crash reports.
  keepalive?: boolean;
}

export interface Http {
  // GET `path` (with `query`) and parse the JSON answer.
  get<T>(path: string, query?: URLSearchParams, signal?: AbortSignal): Promise<T>;
  // A state-changing request with an optional JSON body; resolves on 2xx.
  send(
    method: UnsafeMethod,
    path: string,
    body?: unknown,
    options?: SendOptions,
  ): Promise<Response>;
  // Called whenever the server answers 401: the session is gone. Returns the
  // unsubscribe function.
  onUnauthorized(listener: () => void): () => void;
  // Tells the onUnauthorized listeners that the session is gone without a 401:
  // the user signed out.
  sessionEnded(): void;
  // Who confirms the user's identity when the server answers 403
  // `reauth_required` (a sensitive action needs a sign-in from the last 5
  // minutes): the re-authentication dialog (web/src/auth/ReauthDialog.tsx).
  // It resolves to true once the user confirmed, and the refused request is
  // then sent again; false (cancelled) rejects it with the 403. A request
  // still refused asks again (`again`), up to MAX_REAUTH_ROUNDS times. One
  // handler at a time; returns the function that removes it.
  onReauthRequired(handler: ReauthHandler): () => void;
}

export type ReauthHandler = (context: { again: boolean }) => Promise<boolean>;

// How many times one request asks for a re-authentication.
export const MAX_REAUTH_ROUNDS = 3;

export interface HttpOptions {
  // The fetch to use (tests); defaults to the browser's.
  fetch?: typeof fetch;
}

// The code of an error answer that is not a problem document (a proxy's page).
function codeOfStatus(status: number): ErrorCode {
  if (status === 401) return 'unauthorized';
  if (status === 403) return 'forbidden';
  if (status === 404) return 'not_found';
  if (status === 423) return 'user_locked';
  if (status === 429) return 'rate_limited';
  return status >= 500 ? 'unavailable' : 'bad_request';
}

async function errorOf(res: Response): Promise<ApiError> {
  let problem: Partial<Problem> = {};
  try {
    problem = (await res.json()) as Partial<Problem>;
  } catch {
    /* not a problem document */
  }
  const retry = Number(res.headers.get('Retry-After'));
  return new ApiError(
    res.status,
    problem.code ?? codeOfStatus(res.status),
    problem.detail,
    Number.isFinite(retry) && retry > 0 ? retry : null,
  );
}

// The routes that re-authenticate: their answers never open the dialog.
const REAUTH_PREFIX = '/api/v1/auth/reauth/';

export function createHttp({ fetch: fetchImpl }: HttpOptions = {}): Http {
  // Called through a wrapper: a detached `window.fetch` throws "Illegal invocation".
  const doFetch: typeof fetch = fetchImpl ?? ((input, init) => fetch(input, init));
  const unauthorized = new Set<() => void>();
  let reauth: ReauthHandler | null = null;

  async function request(
    method: 'GET' | UnsafeMethod,
    path: string,
    init: {
      query?: URLSearchParams;
      body?: unknown;
      signal?: AbortSignal;
      keepalive?: boolean;
      // How many re-authentications this request asked for already.
      reauthRounds?: number;
    } = {},
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
        ...(init.keepalive ? { keepalive: true } : {}),
      });
    } catch (err) {
      if (init.signal?.aborted) throw err;
      throw new ApiError(0, 'network', err instanceof Error ? err.message : undefined);
    }
    if (res.status === 401) unauthorized.forEach((listener) => listener());
    if (res.ok) return res;
    const error = await errorOf(res);
    const confirm = reauth;
    const rounds = init.reauthRounds ?? 0;
    if (
      error.code === 'reauth_required' &&
      confirm &&
      rounds < MAX_REAUTH_ROUNDS &&
      !path.startsWith(REAUTH_PREFIX) &&
      !init.signal?.aborted
    ) {
      let confirmed = false;
      try {
        confirmed = await confirm({ again: rounds > 0 });
      } catch {
        /* a failing dialog cancels */
      }
      if (confirmed) return request(method, path, { ...init, reauthRounds: rounds + 1 });
    }
    throw error;
  }

  return {
    async get<T>(path: string, query?: URLSearchParams, signal?: AbortSignal): Promise<T> {
      const res = await request('GET', path, { query, signal });
      return (await res.json()) as T;
    },
    send: (method, path, body, options) =>
      request(method, path, { body, keepalive: options?.keepalive }),
    onUnauthorized(listener) {
      unauthorized.add(listener);
      return () => {
        unauthorized.delete(listener);
      };
    },
    sessionEnded() {
      unauthorized.forEach((listener) => listener());
    },
    onReauthRequired(handler) {
      reauth = handler;
      return () => {
        if (reauth === handler) reauth = null;
      };
    },
  };
}
