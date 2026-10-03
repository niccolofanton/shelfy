// How the worker reacts to a failed API call (contract C1, plan §2.16), as pure functions:
//
// | Failure                                   | Action                                         |
// |-------------------------------------------|------------------------------------------------|
// | 401 (problem+json), or no token           | unpair: forget the token, keep the queue       |
// | 426 extension_outdated                    | outdated: stop sending, "update the extension" |
// | 3xx to the Access sign-in (opaqueredirect),| access: "Sign in to Shelfy in this browser"   |
// | or a 401/403 that is not our problem+json |                                                |
// | 429, 503, 423                             | wait for Retry-After (default 30 s, 60 s)      |
// | 409 source_disabled                       | drop the batch, refresh the config             |
// | 404 sync_run_not_found                    | open the run again                             |
// | 413                                       | split the batch in two                         |
// | 422 on Idempotency-Key                    | send the batch again under a new key           |
// | 400, 410, 422                             | drop: the server will never take this request  |
// | 403, other 404, 405                       | hold: keep everything, retry in 10 min          |
// | other 5xx, 408, 409, network, timeout     | backoff: 2 s × 2ⁿ with jitter, at most 5 min   |

export type ApiFailure =
  /** No token: the extension is not paired (the request was not sent). */
  | { kind: 'unpaired' }
  | { kind: 'network'; detail: string }
  | { kind: 'access_redirect' }
  | {
      kind: 'http';
      status: number;
      /** The problem's `code`, when the body was problem+json. */
      code: string | null;
      retryAfterMs: number | null;
      /** Fields of a `validation_failed` problem. */
      fields: string[];
    };

export type FailureAction =
  | { action: 'unpair' }
  | { action: 'outdated' }
  | { action: 'access' }
  | { action: 'wait'; ms: number }
  | { action: 'backoff' }
  | { action: 'drop'; code: string }
  | { action: 'disable_source' }
  | { action: 'recreate_run' }
  | { action: 'split' }
  | { action: 'rekey' }
  | { action: 'hold'; ms: number; code: string };

export const DEFAULT_RETRY_AFTER_MS = 30_000;
export const LOCKED_RETRY_AFTER_MS = 60_000;
export const HOLD_MS = 10 * 60_000;
export const BACKOFF_BASE_MS = 2_000;
export const BACKOFF_MAX_MS = 5 * 60_000;
const MAX_RETRY_AFTER_MS = 60 * 60_000;

export function classifyFailure(failure: ApiFailure): FailureAction {
  switch (failure.kind) {
    case 'unpaired':
      return { action: 'unpair' };
    case 'network':
      return { action: 'backoff' };
    case 'access_redirect':
      return { action: 'access' };
    case 'http':
      break;
  }
  const { status, code } = failure;
  const retry = (fallback: number): FailureAction => ({
    action: 'wait',
    ms: failure.retryAfterMs ?? fallback,
  });
  switch (status) {
    case 401:
      return { action: 'unpair' };
    case 426:
      return { action: 'outdated' };
    case 429:
    case 503:
      return retry(DEFAULT_RETRY_AFTER_MS);
    case 423:
      return retry(LOCKED_RETRY_AFTER_MS);
    case 409:
      return code === 'source_disabled' ? { action: 'disable_source' } : { action: 'backoff' };
    case 404:
      return code === 'sync_run_not_found'
        ? { action: 'recreate_run' }
        : { action: 'hold', ms: HOLD_MS, code: code ?? 'not_found' };
    case 413:
      return { action: 'split' };
    case 422:
      return failure.fields.includes('Idempotency-Key')
        ? { action: 'rekey' }
        : { action: 'drop', code: code ?? 'validation_failed' };
    case 400:
    case 410:
      return { action: 'drop', code: code ?? (status === 400 ? 'bad_request' : 'gone') };
    case 403:
    case 405:
      return {
        action: 'hold',
        ms: HOLD_MS,
        code: code ?? (status === 403 ? 'forbidden' : 'method_not_allowed'),
      };
    default:
      return status >= 500 || status === 408
        ? { action: 'backoff' }
        : { action: 'hold', ms: HOLD_MS, code: code ?? `http_${status}` };
  }
}

/**
 * The code the panel shows for a failure (`error.<code>` in the extension's messages, or the
 * server's problem code, which falls back to `error.unknown`).
 */
export function failureCode(failure: ApiFailure): string {
  switch (failure.kind) {
    case 'unpaired':
      return 'unauthorized';
    case 'network':
      return 'network';
    case 'access_redirect':
      return 'access_redirect';
    case 'http':
      if (failure.status === 401) return 'unauthorized';
      if (failure.status === 426) return 'outdated';
      if (failure.status === 429) return 'rate_limited';
      if (failure.status === 503) return 'unavailable';
      if (failure.status >= 500) return 'server';
      return failure.code ?? `http_${failure.status}`;
  }
}

/**
 * Exponential backoff with "equal jitter": after the n-th consecutive failure, a delay between
 * half and all of min(5 min, 2 s × 2ⁿ⁻¹). `random` returns a number in [0, 1).
 */
export function backoffMs(failures: number, random: () => number = Math.random): number {
  const exponent = Math.min(Math.max(failures, 1) - 1, 20);
  const ceiling = Math.min(BACKOFF_MAX_MS, BACKOFF_BASE_MS * 2 ** exponent);
  return Math.round(ceiling / 2 + random() * (ceiling / 2));
}

/** `Retry-After` in milliseconds: delta-seconds or an HTTP date; null when absent or unreadable. */
export function parseRetryAfter(value: string | null, now: number): number | null {
  if (!value) return null;
  const text = value.trim();
  let ms: number;
  if (/^\d{1,9}$/.test(text)) ms = Number(text) * 1000;
  else {
    const date = Date.parse(text);
    if (Number.isNaN(date)) return null;
    ms = date - now;
  }
  return Math.min(Math.max(ms, 1000), MAX_RETRY_AFTER_MS);
}
