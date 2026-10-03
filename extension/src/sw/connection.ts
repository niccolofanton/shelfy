// "Check connection" (side panel; the P2-22 Access probe). Three requests from the worker:
//
// 1. `GET /health` with the browser's cookies and no service-token headers: does the browser's
//    Cloudflare Access cookie get the worker through Access ("cookie mode")?
// 2. `GET /health` with the saved service-token headers and no cookies ("header mode"), when
//    headers are saved;
// 3. `GET /extension/config` with the extension token, when paired: is the token accepted?
//
// An Access redirect shows as an `opaqueredirect` response (redirect: "manual"). The result is
// stored for the panel; a passing check also lifts a wait that an Access redirect had set.

import { ACCESS_ID_HEADER, ACCESS_SECRET_HEADER, EXTENSION_HEADER, type ApiClient } from './api';
import { API } from './contracts';
import { failureCode } from './errors';
import type { ConnectionCheck, Probe, SettingsStore, TokenOutcome } from './settings';
import { isRecord } from '../shared/protocol';

export interface ConnectionDeps {
  origin: string;
  version: string;
  api: ApiClient;
  store: SettingsStore;
  fetch: (input: string, init: RequestInit) => Promise<Response>;
  now(): number;
}

const PROBE_TIMEOUT_MS = 15_000;

async function probeHealth(
  deps: ConnectionDeps,
  mode: 'cookie' | 'headers',
  headers: Record<string, string>,
): Promise<Probe> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), PROBE_TIMEOUT_MS);
  try {
    const response = await deps.fetch(new URL(API.health, deps.origin).href, {
      method: 'GET',
      headers: { Accept: 'application/json', [EXTENSION_HEADER]: deps.version, ...headers },
      credentials: mode === 'cookie' ? 'include' : 'omit',
      redirect: 'manual',
      cache: 'no-store',
      signal: controller.signal,
    });
    if (response.type === 'opaqueredirect' || (response.status >= 300 && response.status < 400))
      return { outcome: 'access_redirect', status: null, detail: null, version: null };
    const isJson = /\bjson\b/i.test(response.headers.get('content-type') ?? '');
    if (!isJson && (response.status === 401 || response.status === 403))
      return { outcome: 'access_redirect', status: response.status, detail: null, version: null };
    let version: string | null = null;
    if (isJson) {
      const body: unknown = await response.json().catch(() => null);
      if (isRecord(body) && typeof body.version === 'string') version = body.version.slice(0, 64);
    }
    // /health answers 503 when its database check fails: Access passed all the same.
    if (response.ok || (response.status === 503 && isJson))
      return { outcome: 'ok', status: response.status, detail: null, version };
    return {
      outcome: 'failed',
      status: response.status,
      detail: `HTTP ${response.status}`,
      version,
    };
  } catch (err) {
    const detail = err instanceof Error ? err.name : 'network';
    return { outcome: 'failed', status: null, detail, version: null };
  } finally {
    clearTimeout(timer);
  }
}

async function probeToken(deps: ConnectionDeps): Promise<ConnectionCheck['token']> {
  if (!(await deps.store.pairing())) return { outcome: 'unpaired', detail: null };
  const response = await deps.api.get(API.config, { auth: 'token' });
  if (response.ok) return { outcome: 'ok', detail: null };
  const failure = response.failure;
  let outcome: TokenOutcome = 'failed';
  if (failure.kind === 'unpaired' || (failure.kind === 'http' && failure.status === 401))
    outcome = 'unauthorized';
  else if (failure.kind === 'http' && failure.status === 426) outcome = 'outdated';
  return { outcome, detail: outcome === 'failed' ? failureCode(failure) : null };
}

export async function checkConnection(deps: ConnectionDeps): Promise<ConnectionCheck> {
  const settings = await deps.store.settings();
  const cookie = await probeHealth(deps, 'cookie', {});
  const headers = settings.access
    ? await probeHealth(deps, 'headers', {
        [ACCESS_ID_HEADER]: settings.access.clientId,
        [ACCESS_SECRET_HEADER]: settings.access.clientSecret,
      })
    : null;
  const token = await probeToken(deps);
  const check: ConnectionCheck = { at: deps.now(), origin: deps.origin, cookie, headers, token };
  const passes = cookie.outcome === 'ok' || headers?.outcome === 'ok';
  await deps.store.patchStatus((status) => ({
    connection: check,
    // A wait set by an Access redirect ends once Access lets the worker through.
    ...(passes && status.lastError?.code === 'access_redirect'
      ? { blockedUntil: 0, failures: 0, lastError: null }
      : {}),
  }));
  return check;
}
