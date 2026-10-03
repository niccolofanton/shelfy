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
  signal?: AbortSignal;
  reauthenticate?: boolean;
  // Let the request outlive the page (fetch `keepalive`): crash reports.
  keepalive?: boolean;
  // Sent as `Idempotency-Key`: a repeated request (a double click, or this
  // same call resent after a re-authentication) has one effect server-side.
  idempotencyKey?: string;
  // Extra headers (e.g. `Idempotency-Key` on a job-creating POST, P1-11).
  // Merged after the CSRF/content-type headers; never overrides them.
  headers?: Record<string, string>;
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
  // Asks the onReauthRequired handler to confirm the user's identity without
  // a refused request: a page that knows its action needs a re-authentication
  // asks first and sends the action once (F10, /device). Resolves to false
  // when cancelled or when no handler is registered.
  reauthenticate(): Promise<boolean>;
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
      idempotencyKey?: string;
      headers?: Record<string, string>;
      reauthenticate?: boolean;
      // How many re-authentications this request asked for already.
      reauthRounds?: number;
    } = {},
  ): Promise<Response> {
    const qs = init.query?.toString();
    const headers: Record<string, string> = { Accept: 'application/json', ...init.headers };
    if (method !== 'GET') headers[CLIENT_HEADER] = CLIENT_WEB;
    if (init.body !== undefined) headers['Content-Type'] = 'application/json';
    if (init.idempotencyKey) headers['Idempotency-Key'] = init.idempotencyKey;
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
      init.reauthenticate !== false &&
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
      request(method, path, {
        body,
        keepalive: options?.keepalive,
        idempotencyKey: options?.idempotencyKey,
        headers: options?.headers,
        signal: options?.signal,
        reauthenticate: options?.reauthenticate,
      }),
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
    async reauthenticate() {
      const confirm = reauth;
      if (!confirm) return false;
      try {
        return await confirm({ again: false });
      } catch {
        return false;
      }
    },
  };
}
