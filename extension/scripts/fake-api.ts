// A fake Shelfy API for the extension's tests: the routes of contracts C1–C5
// (docs/web-port/phases/P2.md) with the behaviour the extension relies on — pairing codes used
// once, bearer tokens, the version gate (426), the config's ETag, sync runs, and ingest with
// Idempotency-Key replay. It runs in memory: `handle()` takes a request and returns a response,
// `fetchHandler()` wraps it as a fetch function (vitest) and smoke.ts serves it over HTTP.
// Tokens and pairing codes are random per run and never printed.

import { randomBytes } from 'node:crypto';
import { canonicalIdentity } from '../src/shared/identity';
import { isPlatform, isRecord, type Platform } from '../src/shared/protocol';
import { compareVersions } from '../src/shared/version';

export interface FakeRequest {
  method: string;
  /** Path and query, e.g. `/api/v1/sync-runs`. */
  path: string;
  headers: Record<string, string>;
  body: string | null;
}

export interface FakeResponse {
  status: number;
  headers: Record<string, string>;
  body: string;
}

export interface FakeRun {
  id: string;
  platform: Platform;
  trigger: string;
  listing: { kind: string; externalId: string | null; name: string | null };
  collection: unknown;
  state: string;
  stopReason: string | null;
  pages: number;
  scanned: number;
  batches: number;
}

export interface IngestRecord {
  key: string;
  runId: string;
  platform: Platform;
  source: string;
  client: unknown;
  hasNextPage: boolean | null;
  count: number;
  /** True when this was a replay of an earlier request with the same key. */
  replayed: boolean;
}

export interface FakeApiOptions {
  minVersion?: string;
}

const random = (bytes: number): string => randomBytes(bytes).toString('base64url');

const problem = (
  status: number,
  code: string,
  extra: Record<string, unknown> = {},
  headers: Record<string, string> = {},
): FakeResponse => ({
  status,
  headers: { 'content-type': 'application/problem+json', ...headers },
  body: JSON.stringify({ type: 'about:blank', title: code, status, code, ...extra }),
});

const json = (
  status: number,
  value: unknown,
  headers: Record<string, string> = {},
): FakeResponse => ({
  status,
  headers: { 'content-type': 'application/json', ...headers },
  body: JSON.stringify(value),
});

export class FakeShelfyApi {
  /** Every request, in order (method, path, the headers the tests look at). */
  readonly log: Array<{
    method: string;
    path: string;
    authorization: string | null;
    extension: string | null;
    idempotencyKey: string | null;
    accessId: string | null;
    status: number;
  }> = [];
  readonly runs = new Map<string, FakeRun>();
  /** P2-15 native listings returned by the source planner endpoint. */
  sources: Array<{
    platform: Platform;
    listing: { kind: string; externalId: string | null; name: string | null };
    collectionId: number | null;
  }> = [];
  /** Ingest requests the server acted on (a replay is listed with `replayed: true`). */
  readonly ingests: IngestRecord[] = [];
  /** Canonical keys of every post ingested, with how many times each was ingested. */
  readonly posts = new Map<string, number>();
  /** Items of each accepted batch, by Idempotency-Key. */
  readonly batchItems = new Map<string, unknown[]>();
  readonly lookups: Array<{ platform: Platform; keys: string[] }> = [];
  readonly patches: Array<{ id: string; body: unknown }> = [];
  minVersion: string;
  /** Platforms whose passive source is killed (409 source_disabled). */
  readonly killed = new Set<Platform>();
  /** Answer every request with this status (problem+json), e.g. 503; null = normal. */
  failWith: { status: number; code: string; retryAfter?: string } | null = null;
  /** Process the next ingest, then answer 502 as if the response was lost on the way back. */
  loseNextIngestResponse = false;
  /** Pretend the server forgot every run (404 sync_run_not_found on the next ingest). */
  forgetRuns = false;
  /** Largest batch the fake takes; more items answer 413 (C5: ≤ 500 items, ≤ 8 MiB). */
  maxBatchItems = 500;
  /** Require these Access headers on every request (header mode). */
  requireAccess: { clientId: string; clientSecret: string } | null = null;
  /** Config values per platform over the defaults (e.g. a killed mode, a lower page cap). */
  readonly platformConfig: Partial<Record<Platform, Record<string, unknown>>> = {};
  /** What the next POST /sync-runs answers (P2-13: incremental, resume cursor), then cleared. */
  nextRunAnswer: {
    incremental?: boolean;
    stopAfterKnown?: number;
    resumeCursor?: string | null;
    collectionId?: number | null;
  } | null = null;
  private readonly codes = new Set<string>();
  private readonly tokens = new Map<string, { installId: string; tokenId: string }>();
  private readonly idempotency = new Map<string, { fingerprint: string; response: FakeResponse }>();
  private configVersion = 1;
  private runSeq = 0;

  constructor(options: FakeApiOptions = {}) {
    this.minVersion = options.minVersion ?? '0.2.0';
  }

  /** A fresh single-use pairing code (C2: 43 base64url characters). */
  issuePairingCode(): string {
    const code = random(32);
    this.codes.add(code);
    return code;
  }

  /** Mints a token directly (tests that start paired). */
  mintToken(installId = 'test-install'): string {
    const token = `shx_${random(32)}`;
    this.tokens.set(token, { installId, tokenId: `tok-${this.tokens.size + 1}` });
    return token;
  }

  revokeAll(): void {
    this.tokens.clear();
  }

  /** Changes the config (a new ETag), e.g. to kill a source. */
  bumpConfig(): void {
    this.configVersion += 1;
  }

  config(): Record<string, unknown> {
    const platform = (name: Platform, base: Record<string, unknown>) => ({
      passive: !this.killed.has(name),
      scroll: true,
      ...base,
      ...this.platformConfig[name],
    });
    return {
      minVersion: this.minVersion,
      platforms: {
        instagram: platform('instagram', {
          replay: true,
          stopAfterKnown: 10,
          replayGapMs: 700,
          replayMaxPages: 100,
          scrollSettleMs: 650,
        }),
        twitter: platform('twitter', { stopAfterKnown: 20, scrollSettleMs: 750 }),
        pinterest: platform('pinterest', { stopAfterKnown: 25, scrollSettleMs: 650 }),
      },
      maxSteps: 16000,
      maxRunMs: 1800000,
      taskPollMinutes: 5,
      refreshPerSession: 200,
    };
  }

  handle(request: FakeRequest): FakeResponse {
    const response = this.route(request);
    const header = (name: string): string | null => request.headers[name.toLowerCase()] ?? null;
    this.log.push({
      method: request.method,
      path: request.path,
      authorization: header('authorization'),
      extension: header('x-shelfy-extension'),
      idempotencyKey: header('idempotency-key'),
      accessId: header('cf-access-client-id'),
      status: response.status,
    });
    return response;
  }

  private route(request: FakeRequest): FakeResponse {
    const url = new URL(request.path, 'http://fake.invalid');
    const path = url.pathname;
    const header = (name: string): string | null => request.headers[name.toLowerCase()] ?? null;
    if (this.requireAccess) {
      const ok =
        header('cf-access-client-id') === this.requireAccess.clientId &&
        header('cf-access-client-secret') === this.requireAccess.clientSecret;
      if (!ok)
        return {
          status: 302,
          headers: { location: 'https://team.cloudflareaccess.com/cdn-cgi/access/login' },
          body: '',
        };
    }
    if (request.method === 'GET' && path === '/health')
      return json(200, { status: 'ok', version: 'fake-0.2.0', checks: { controlDb: 'ok' } });
    if (this.failWith)
      return problem(
        this.failWith.status,
        this.failWith.code,
        {},
        this.failWith.retryAfter ? { 'retry-after': this.failWith.retryAfter } : {},
      );
    let body: unknown = null;
    if (request.body) {
      try {
        body = JSON.parse(request.body);
      } catch {
        return problem(400, 'bad_request');
      }
    }
    if (request.method === 'POST' && path === '/api/v1/extension/pair') return this.pair(body);

    // Token routes (C1).
    const auth = header('authorization');
    const token = auth?.startsWith('Bearer ') ? auth.slice(7) : null;
    if (!token || !this.tokens.has(token)) return problem(401, 'unauthorized');
    if (!(request.method === 'GET' && path === '/api/v1/extension/config')) {
      const version = header('x-shelfy-extension');
      const order = version ? compareVersions(version, this.minVersion) : null;
      if (order === null || order < 0) return problem(426, 'extension_outdated');
    }
    if (request.method === 'GET' && path === '/api/v1/extension/config') {
      const etag = `"config-${this.configVersion}"`;
      if (header('if-none-match') === etag) return { status: 304, headers: { etag }, body: '' };
      return json(200, this.config(), { etag });
    }
    if (request.method === 'GET' && path === '/api/v1/extension/sources')
      return json(200, { items: this.sources });
    if (request.method === 'POST' && path === '/api/v1/posts/lookup') {
      if (
        !isRecord(body) ||
        !isPlatform(body.platform) ||
        !Array.isArray(body.keys) ||
        body.keys.length > 1000 ||
        body.keys.some((key) => typeof key !== 'string')
      )
        return problem(422, 'validation_failed');
      const keys = body.keys as string[];
      this.lookups.push({ platform: body.platform, keys });
      return json(200, {
        items: keys.flatMap((key) => {
          const identity = canonicalIdentity(body.platform as Platform, {
            ids: [key],
            shortcode: body.platform === 'instagram' ? key : undefined,
          });
          return identity && this.posts.has(identity.key)
            ? [{ key, postKey: identity.key, trashed: false }]
            : [];
        }),
      });
    }
    if (request.method === 'POST' && path === '/api/v1/sync-runs') return this.createRun(body);
    const patch = /^\/api\/v1\/sync-runs\/([^/]+)$/.exec(path);
    if (request.method === 'PATCH' && patch)
      return this.patchRun(decodeURIComponent(patch[1]), body);
    if (request.method === 'POST' && path === '/api/v1/ingest/batches')
      return this.ingest(header('idempotency-key'), request.body ?? '', body);
    return problem(404, 'not_found');
  }

  private pair(body: unknown): FakeResponse {
    if (!isRecord(body) || typeof body.code !== 'string' || typeof body.installId !== 'string')
      return problem(422, 'validation_failed');
    if (!this.codes.delete(body.code)) return problem(400, 'invalid_pairing_code');
    // Re-pairing the same installation revokes its previous token (P2-G16).
    for (const [token, owner] of this.tokens)
      if (owner.installId === body.installId) this.tokens.delete(token);
    const token = `shx_${random(32)}`;
    const tokenId = `tok-${this.tokens.size + 1}`;
    this.tokens.set(token, { installId: body.installId, tokenId });
    return json(201, { token, tokenId, scopes: ['ingest', 'tasks', 'uploads', 'lookup'] });
  }

  private createRun(body: unknown): FakeResponse {
    if (
      !isRecord(body) ||
      !isPlatform(body.platform) ||
      typeof body.trigger !== 'string' ||
      !isRecord(body.listing) ||
      typeof body.listing.kind !== 'string'
    )
      return problem(422, 'validation_failed');
    const id = `run-${++this.runSeq}`;
    this.runs.set(id, {
      id,
      platform: body.platform,
      trigger: body.trigger,
      listing: {
        kind: body.listing.kind,
        externalId: typeof body.listing.externalId === 'string' ? body.listing.externalId : null,
        name: typeof body.listing.name === 'string' ? body.listing.name : null,
      },
      collection: body.collection,
      state: 'running',
      stopReason: null,
      pages: 0,
      scanned: 0,
      batches: 0,
    });
    const answer = this.nextRunAnswer ?? {};
    this.nextRunAnswer = null;
    return json(201, {
      id,
      incremental: answer.incremental ?? false,
      stopAfterKnown: answer.stopAfterKnown ?? 10,
      collectionId: answer.collectionId ?? null,
      resumeCursor: answer.resumeCursor ?? null,
    });
  }

  private patchRun(id: string, body: unknown): FakeResponse {
    const run = this.runs.get(id);
    if (!run) return problem(404, 'sync_run_not_found');
    if (!isRecord(body) || typeof body.state !== 'string') return problem(422, 'validation_failed');
    run.state = body.state;
    run.stopReason = typeof body.stopReason === 'string' ? body.stopReason : null;
    run.pages = typeof body.pages === 'number' ? body.pages : run.pages;
    run.scanned = typeof body.scanned === 'number' ? body.scanned : run.scanned;
    this.patches.push({ id, body });
    return json(200, run);
  }

  private ingest(key: string | null, raw: string, body: unknown): FakeResponse {
    if (!key) return problem(400, 'bad_request');
    const seen = this.idempotency.get(key);
    if (seen) {
      if (seen.fingerprint !== raw)
        return problem(422, 'validation_failed', {
          errors: [{ field: 'Idempotency-Key', reason: 'reused with another request' }],
        });
      const record = this.ingests.find((entry) => entry.key === key);
      if (record) this.ingests.push({ ...record, replayed: true });
      return {
        ...seen.response,
        headers: { ...seen.response.headers, 'idempotent-replayed': 'true' },
      };
    }
    if (this.forgetRuns) {
      this.forgetRuns = false;
      this.runs.clear();
    }
    if (!isRecord(body) || !isPlatform(body.platform) || !Array.isArray(body.items))
      return problem(422, 'validation_failed');
    const run = typeof body.syncRunId === 'string' ? this.runs.get(body.syncRunId) : undefined;
    if (!run) {
      const response = problem(404, 'sync_run_not_found');
      this.idempotency.set(key, { fingerprint: raw, response });
      return response;
    }
    if (run.platform !== body.platform) return problem(422, 'validation_failed');
    if (this.killed.has(body.platform) && body.source === 'passive')
      return problem(409, 'source_disabled');
    if (body.items.length > this.maxBatchItems) return problem(413, 'payload_too_large');
    const platform = body.platform;
    let inserted = 0;
    let known = 0;
    const results: unknown[] = [];
    const rejected: unknown[] = [];
    body.items.forEach((item, index) => {
      const identity = isRecord(item)
        ? canonicalIdentity(platform, {
            ids: [String(item.id ?? '')],
            shortcode: typeof item.shortcode === 'string' ? item.shortcode : undefined,
            postUrl: typeof item.postUrl === 'string' ? item.postUrl : undefined,
          })
        : null;
      if (!identity) {
        rejected.push({ index, code: 'bad_id' });
        return;
      }
      const count = this.posts.get(identity.key) ?? 0;
      this.posts.set(identity.key, count + 1);
      if (count) known += 1;
      else inserted += 1;
      results.push({
        index,
        key: identity.key,
        outcome: count ? 'known' : 'inserted',
        changed: false,
      });
    });
    run.batches += 1;
    this.batchItems.set(key, body.items);
    this.ingests.push({
      key,
      runId: run.id,
      platform,
      source: String(body.source),
      client: body.client,
      hasNextPage: typeof body.hasNextPage === 'boolean' ? body.hasNextPage : null,
      count: body.items.length,
      replayed: false,
    });
    const response = json(200, { inserted, updated: 0, known, results, rejected });
    this.idempotency.set(key, { fingerprint: raw, response });
    if (this.loseNextIngestResponse) {
      this.loseNextIngestResponse = false;
      return problem(502, 'internal');
    }
    return response;
  }

  /** A fetch function over this API, for code that runs in Node (vitest). */
  fetchHandler(origin: string): (input: string, init: RequestInit) => Promise<Response> {
    return async (input, init) => {
      const url = new URL(input);
      if (url.origin !== origin) throw new TypeError(`fake API: unexpected origin ${url.origin}`);
      const headers: Record<string, string> = {};
      new Headers(init.headers).forEach((value, name) => (headers[name.toLowerCase()] = value));
      const result = this.handle({
        method: init.method ?? 'GET',
        path: url.pathname + url.search,
        headers,
        body: typeof init.body === 'string' ? init.body : null,
      });
      if (REDIRECTS.has(result.status) && init.redirect === 'manual') return opaqueRedirect();
      return new Response(result.status === 304 ? null : result.body || null, {
        status: result.status,
        headers: result.headers,
      });
    };
  }
}

const REDIRECTS = new Set([301, 302, 303, 307, 308]);

/** What fetch(…, {redirect: 'manual'}) returns for a redirect in a browser. */
export function opaqueRedirect(): Response {
  return {
    type: 'opaqueredirect',
    status: 0,
    ok: false,
    headers: new Headers(),
    text: async () => '',
    json: async () => null,
  } as unknown as Response;
}
