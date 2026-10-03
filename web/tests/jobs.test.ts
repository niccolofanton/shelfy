// The web client's JobsApi (web/src/api/jobs.ts) on `/api/v1/jobs*` and
// `/api/v1/queues/*` (P4-09).
import { describe, it, expect, vi } from 'vitest';
import { createJobsApi } from '../src/api/jobs';
import type { ServerEventData, ServerEventName } from '../src/api/events';
import type { Http, SendOptions } from '../src/api/http';

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

interface Sent {
  method: string;
  path: string;
  body: unknown;
  options: SendOptions | undefined;
}

function fakeHttp(answers: Record<string, unknown> = {}) {
  const sent: Sent[] = [];
  const gets: { path: string; query?: URLSearchParams }[] = [];
  const http: Http = {
    get: vi.fn(async (path: string, query?: URLSearchParams) => {
      gets.push({ path, query });
      return answers[`GET ${path}`];
    }) as Http['get'],
    send: vi.fn(async (method: string, path: string, body?: unknown, options?: SendOptions) => {
      sent.push({ method, path, body, options });
      const answer = answers[`${method} ${path}`];
      return answer === undefined ? new Response(null, { status: 204 }) : json(answer);
    }) as Http['send'],
    onUnauthorized: () => () => {},
    sessionEnded: () => {},
    onReauthRequired: () => () => {},
  };
  return { http, sent, gets };
}

function fakeEvents() {
  const listeners = new Map<string, ((data: unknown) => void)[]>();
  return {
    on<N extends ServerEventName>(name: N, listener: (data: ServerEventData<N>) => void) {
      const list = listeners.get(name) ?? [];
      list.push(listener as (data: unknown) => void);
      listeners.set(name, list);
      return () =>
        listeners.set(
          name,
          (listeners.get(name) ?? []).filter((l) => l !== listener),
        );
    },
    emit(name: string, data: unknown) {
      for (const listener of listeners.get(name) ?? []) listener(data);
    },
  };
}

const JOB = {
  id: 7,
  kind: 'capture.site',
  state: 'running' as const,
  progress: 0.5,
  stage: null,
  postKey: 'web_abc',
  errorCode: null,
  attempts: 0,
  maxAttempts: 2,
  runAt: 0,
  createdAt: 1,
  updatedAt: 2,
  finishedAt: null,
};

const QUEUE = {
  kind: 'capture.site',
  paused: false,
  queued: 1,
  running: 1,
  succeeded: 2,
  failed: 0,
  cancelled: 0,
};

describe('web jobs api', () => {
  it('lists jobs, repeating kind/state and paging by cursor', async () => {
    const { http, gets } = fakeHttp({ 'GET /api/v1/jobs': { items: [JOB], nextCursor: 'jobs.6' } });
    const jobs = createJobsApi(http, { events: fakeEvents() });
    const page = await jobs.list(
      { kind: ['capture.site', 'import'], state: ['failed'] },
      { limit: 500, cursor: 'jobs.9' },
    );
    expect(page).toEqual({ items: [JOB], nextCursor: 'jobs.6' });
    expect(gets[0].path).toBe('/api/v1/jobs');
    const query = gets[0].query!;
    expect(query.getAll('kind')).toEqual(['capture.site', 'import']);
    expect(query.getAll('state')).toEqual(['failed']);
    expect(query.get('limit')).toBe('200'); // clamped to the server's cap
    expect(query.get('cursor')).toBe('jobs.9');
  });

  it('sends no kind/state when the filter is empty', async () => {
    const { http, gets } = fakeHttp({ 'GET /api/v1/jobs': { items: [], nextCursor: null } });
    const jobs = createJobsApi(http, { events: fakeEvents() });
    await jobs.list({}, { limit: 50 });
    const query = gets[0].query!;
    expect(query.has('kind')).toBe(false);
    expect(query.has('state')).toBe(false);
    expect(query.has('cursor')).toBe(false);
  });

  it('reads the queues summary', async () => {
    const { http } = fakeHttp({ 'GET /api/v1/jobs/summary': { queues: [QUEUE] } });
    const jobs = createJobsApi(http, { events: fakeEvents() });
    await expect(jobs.summary()).resolves.toEqual([QUEUE]);
  });

  it('cancels a job with no Idempotency-Key', async () => {
    const { http, sent } = fakeHttp({
      'POST /api/v1/jobs/7/cancel': { ...JOB, state: 'cancelled' },
    });
    const jobs = createJobsApi(http, { events: fakeEvents() });
    const job = await jobs.cancel(7);
    expect(job.state).toBe('cancelled');
    expect(sent).toEqual([
      { method: 'POST', path: '/api/v1/jobs/7/cancel', body: undefined, options: undefined },
    ]);
  });

  it('retries a job with a fresh Idempotency-Key each call, unless one is supplied', async () => {
    const { http, sent } = fakeHttp({ 'POST /api/v1/jobs/7/retry': { ...JOB, state: 'queued' } });
    let n = 0;
    const jobs = createJobsApi(http, {
      events: fakeEvents(),
      newIdempotencyKey: () => `key-${++n}`,
    });
    await jobs.retry(7);
    await jobs.retry(7);
    expect(sent[0].options).toEqual({ idempotencyKey: 'key-1' });
    expect(sent[1].options).toEqual({ idempotencyKey: 'key-2' });
  });

  it('generates a real Idempotency-Key by default', async () => {
    const { http, sent } = fakeHttp({ 'POST /api/v1/jobs/7/retry': JOB });
    const jobs = createJobsApi(http, { events: fakeEvents() });
    await jobs.retry(7);
    expect(sent[0].options?.idempotencyKey).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/,
    );
  });

  it('pauses, resumes, cancels and clears a queue by kind', async () => {
    const { http, sent } = fakeHttp({
      'POST /api/v1/queues/capture.site/pause': { queue: { ...QUEUE, paused: true }, affected: 1 },
      'POST /api/v1/queues/capture.site/resume': { queue: QUEUE, affected: 1 },
      'POST /api/v1/queues/capture.site/cancel-all': { queue: QUEUE, affected: 2 },
      'POST /api/v1/queues/capture.site/clear-finished': { queue: QUEUE, affected: 3 },
    });
    const jobs = createJobsApi(http, { events: fakeEvents() });
    await expect(jobs.pauseQueue('capture.site')).resolves.toMatchObject({ affected: 1 });
    await expect(jobs.resumeQueue('capture.site')).resolves.toMatchObject({ affected: 1 });
    await expect(jobs.cancelQueue('capture.site')).resolves.toMatchObject({ affected: 2 });
    await expect(jobs.clearFinishedQueue('capture.site')).resolves.toMatchObject({ affected: 3 });
    expect(sent.map((s) => s.path)).toEqual([
      '/api/v1/queues/capture.site/pause',
      '/api/v1/queues/capture.site/resume',
      '/api/v1/queues/capture.site/cancel-all',
      '/api/v1/queues/capture.site/clear-finished',
    ]);
  });

  it('forwards job.updated as onUpdate, and unsubscribes', async () => {
    const events = fakeEvents();
    const jobs = createJobsApi(fakeHttp().http, { events });
    const received: unknown[] = [];
    const off = jobs.onUpdate((update) => received.push(update));
    const update = {
      id: 7,
      kind: 'capture.site',
      state: 'succeeded' as const,
      progress: 1,
      stage: null,
      postKey: 'web_abc',
      errorCode: null,
    };
    events.emit('job.updated', update);
    expect(received).toEqual([update]);
    off();
    events.emit('job.updated', update);
    expect(received).toHaveLength(1);
  });
});
