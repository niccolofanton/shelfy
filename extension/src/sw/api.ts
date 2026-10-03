// The worker's HTTP client for the Shelfy API (contract C1). Every request:
// - goes to the Shelfy origin only, with `credentials: "include"` (the browser's Cloudflare
//   Access cookie, E5) — a URL on any other origin is refused here;
// - carries `X-Shelfy-Extension: <version>`, `Authorization: Bearer shx_…` when the route takes
//   the token, and the Access service-token headers when the panel has them (P2-22 decides
//   which of the two passes Access);
// - uses `redirect: "manual"`, so an Access redirect to its sign-in page comes back as an
//   `opaqueredirect` response instead of being followed.
// Failures are returned, never thrown; sw/errors.ts decides what to do with them.

import type { AccessHeaders } from '../shared/protocol';
import { isRecord } from '../shared/protocol';
import { parseRetryAfter, type ApiFailure } from './errors';

export const EXTENSION_HEADER = 'X-Shelfy-Extension';
export const ACCESS_ID_HEADER = 'CF-Access-Client-Id';
export const ACCESS_SECRET_HEADER = 'CF-Access-Client-Secret';
const DEFAULT_TIMEOUT_MS = 45_000;

export interface ApiCredentials {
  token: string | null;
  access: AccessHeaders | null;
}

export interface ApiDeps {
  origin: string;
  version: string;
  fetch: (input: string, init: RequestInit) => Promise<Response>;
  credentials(): Promise<ApiCredentials>;
  now(): number;
}

export interface RequestOptions {
  /** `token`: the route takes the extension token (C1); `none`: public (pairing, health). */
  auth: 'token' | 'none';
  body?: unknown;
  idempotencyKey?: string;
  etag?: string | null;
  timeoutMs?: number;
  /** Leave the Access service-token headers out (the cookie-only probe of "Check connection"). */
  omitAccessHeaders?: boolean;
}

export type ApiResponse =
  | { ok: true; status: number; data: unknown; etag: string | null; replayed: boolean }
  | { ok: false; failure: ApiFailure };

/** Builds the fetch arguments of one request; throws for a URL off the Shelfy origin. */
export function buildRequest(
  origin: string,
  method: string,
  path: string,
  options: RequestOptions,
  credentials: ApiCredentials,
  version: string,
): { url: string; init: RequestInit } {
  const url = new URL(path, origin);
  if (url.origin !== origin)
    throw new Error(`refusing a request off the Shelfy origin: ${url.origin}`);
  const headers: Record<string, string> = {
    Accept: 'application/json',
    [EXTENSION_HEADER]: version,
  };
  if (options.auth === 'token' && credentials.token)
    headers.Authorization = `Bearer ${credentials.token}`;
  if (credentials.access && !options.omitAccessHeaders) {
    headers[ACCESS_ID_HEADER] = credentials.access.clientId;
    headers[ACCESS_SECRET_HEADER] = credentials.access.clientSecret;
  }
  if (options.idempotencyKey) headers['Idempotency-Key'] = options.idempotencyKey;
  if (options.etag) headers['If-None-Match'] = options.etag;
  let body: string | undefined;
  if (options.body !== undefined) {
    headers['Content-Type'] = 'application/json';
    body = JSON.stringify(options.body);
  }
  return {
    url: url.href,
    init: {
      method,
      headers,
      body,
      // Only ever the Shelfy origin (checked above): its Access cookie may ride along.
      credentials: 'include',
      redirect: 'manual',
      cache: 'no-store',
    },
  };
}

function isJson(response: Response): boolean {
  return /\bjson\b/i.test(response.headers.get('content-type') ?? '');
}

async function readJson(response: Response): Promise<unknown> {
  const text = await response.text();
  if (!text) return null;
  return JSON.parse(text) as unknown;
}

/** Turns a fetch response into an ApiResponse (exported for tests). */
export async function readResponse(response: Response, now: number): Promise<ApiResponse> {
  if (response.status === 304)
    return {
      ok: true,
      status: 304,
      data: null,
      etag: response.headers.get('etag'),
      replayed: false,
    };
  // Access answers an unauthenticated request with a redirect to its sign-in page.
  if (response.type === 'opaqueredirect' || (response.status >= 300 && response.status < 400))
    return { ok: false, failure: { kind: 'access_redirect' } };
  if (response.ok) {
    let data: unknown = null;
    try {
      data = await readJson(response);
    } catch {
      return {
        ok: false,
        failure: {
          kind: 'http',
          status: response.status,
          code: 'bad_response',
          retryAfterMs: null,
          fields: [],
        },
      };
    }
    return {
      ok: true,
      status: response.status,
      data,
      etag: response.headers.get('etag'),
      replayed: response.headers.get('idempotent-replayed') === 'true',
    };
  }
  // A 401/403 without our problem+json body comes from Access in front of the server.
  if ((response.status === 401 || response.status === 403) && !isJson(response))
    return { ok: false, failure: { kind: 'access_redirect' } };
  let code: string | null = null;
  const fields: string[] = [];
  if (isJson(response)) {
    try {
      const problem = await readJson(response);
      if (isRecord(problem)) {
        if (typeof problem.code === 'string') code = problem.code.slice(0, 64);
        if (Array.isArray(problem.errors))
          for (const error of problem.errors)
            if (isRecord(error) && typeof error.field === 'string') fields.push(error.field);
      }
    } catch {
      /* an unreadable problem keeps its status */
    }
  }
  return {
    ok: false,
    failure: {
      kind: 'http',
      status: response.status,
      code,
      retryAfterMs: parseRetryAfter(response.headers.get('retry-after'), now),
      fields,
    },
  };
}

export class ApiClient {
  constructor(private readonly deps: ApiDeps) {}

  get origin(): string {
    return this.deps.origin;
  }

  async request(method: string, path: string, options: RequestOptions): Promise<ApiResponse> {
    const credentials = await this.deps.credentials();
    if (options.auth === 'token' && !credentials.token)
      return { ok: false, failure: { kind: 'unpaired' } };
    const { url, init } = buildRequest(
      this.deps.origin,
      method,
      path,
      options,
      credentials,
      this.deps.version,
    );
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), options.timeoutMs ?? DEFAULT_TIMEOUT_MS);
    try {
      const response = await this.deps.fetch(url, { ...init, signal: controller.signal });
      return await readResponse(response, this.deps.now());
    } catch (err) {
      const detail = err instanceof Error ? `${err.name}: ${err.message}` : String(err);
      return { ok: false, failure: { kind: 'network', detail: detail.slice(0, 200) } };
    } finally {
      clearTimeout(timer);
    }
  }

  get(path: string, options: Omit<RequestOptions, 'body'>): Promise<ApiResponse> {
    return this.request('GET', path, options);
  }

  post(path: string, body: unknown, options: Omit<RequestOptions, 'body'>): Promise<ApiResponse> {
    return this.request('POST', path, { ...options, body });
  }

  patch(path: string, body: unknown, options: Omit<RequestOptions, 'body'>): Promise<ApiResponse> {
    return this.request('PATCH', path, { ...options, body });
  }
}
