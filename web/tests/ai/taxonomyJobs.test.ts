import { afterEach, describe, expect, it, vi } from 'vitest';
import { createTaxonomyJobs } from '../../src/api/ai/taxonomyJobs';
import { ApiError, type Http } from '../../src/api/http';
import type { EventStream } from '../../src/api/events';
import type { components } from '../../src/api/schema';
type Job = components['schemas']['Job'];
const row = (patch: Partial<Job> = {}): Job => ({
  id: 7,
  kind: 'ai.run',
  state: 'queued',
  progress: null,
  stage: null,
  postKey: null,
  errorCode: null,
  attempts: 0,
  maxAttempts: 1,
  runAt: 0,
  createdAt: 0,
  updatedAt: 0,
  finishedAt: null,
  ...patch,
});
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
function fixture() {
  const listeners = new Map<string, (data: Job) => void>();
  const off = vi.fn();
  const on = vi.fn((name: string, callback: (data: Job) => void) => {
    listeners.set(name, callback);
    return () => {
      listeners.delete(name);
      off(name);
    };
  });
  const send = vi.fn(async () => new Response(JSON.stringify(row())));
  const get = vi.fn(
    async (
      _path: string,
      _query?: URLSearchParams,
    ): Promise<{ items: Job[]; nextCursor: string | null }> => ({
      items: [row()],
      nextCursor: null,
    }),
  );
  const jobs = createTaxonomyJobs({ get, send } as unknown as Http, {
    events: { on } as unknown as Pick<EventStream, 'on'>,
    newIdempotencyKey: () => 'fresh-key',
    pollIntervalMs: 100,
  });
  return {
    jobs,
    send,
    get,
    off,
    emit: (name: string, data = row()) => listeners.get(name)?.(data),
  };
}
afterEach(() => {
  vi.useRealTimers();
});
describe('durable taxonomy job transport', () => {
  it('captures completion before POST returns and ignores unrelated jobs', async () => {
    const f = fixture();
    const accepted = deferred<Response>();
    f.send.mockReturnValueOnce(accepted.promise);
    const progress = vi.fn();
    const run = f.jobs.run('clusters', progress);
    f.emit('job.updated', row({ id: 8, state: 'failed' }));
    f.emit('job.updated', row({ state: 'succeeded', progress: 1, stage: 'complete:2/2' }));
    accepted.resolve(new Response(JSON.stringify(row())));
    expect((await run).state).toBe('succeeded');
    expect(progress).toHaveBeenLastCalledWith(
      expect.objectContaining({ done: 2, total: 2, jobId: 7 }),
    );
    expect(f.get).not.toHaveBeenCalled();
    expect(f.off).toHaveBeenCalledTimes(2);
    expect(f.send).toHaveBeenCalledWith('POST', '/api/v1/tag-clusters/regenerate', undefined, {
      idempotencyKey: 'fresh-key',
    });
  });
  it('polls all pages on reconnect and reports waiting from authoritative runAt', async () => {
    const f = fixture();
    f.send.mockResolvedValueOnce(new Response(JSON.stringify(row({ runAt: Date.now() + 60000 }))));
    const progress = vi.fn();
    const run = f.jobs.run('aliases', progress);
    await vi.waitFor(() => expect(f.get).toHaveBeenCalledTimes(1));
    expect(progress).toHaveBeenCalledWith(
      expect.objectContaining({ waiting: true, state: 'queued' }),
    );
    f.get
      .mockResolvedValueOnce({ items: [row({ id: 8 })], nextCursor: 'older' })
      .mockResolvedValueOnce({ items: [row({ state: 'succeeded' })], nextCursor: null });
    f.emit('resync');
    await run;
    expect(f.get.mock.calls.at(-1)?.[1]?.get('cursor')).toBe('older');
  });
  it('survives network interruption and finishes through polling without SSE', async () => {
    vi.useFakeTimers();
    const f = fixture();
    f.get
      .mockRejectedValueOnce(new ApiError(0, 'network'))
      .mockResolvedValueOnce({ items: [row({ state: 'succeeded' })], nextCursor: null });
    const run = f.jobs.run('clusters');
    await vi.advanceTimersByTimeAsync(101);
    expect((await run).state).toBe('succeeded');
    expect(vi.getTimerCount()).toBe(0);
  });
  it('waits for acceptance before cancelling exactly that job', async () => {
    const f = fixture();
    const accepted = deferred<Response>();
    f.send
      .mockReturnValueOnce(accepted.promise)
      .mockResolvedValueOnce(new Response(JSON.stringify(row({ state: 'cancelled' }))));
    const run = f.jobs.run('aliases').catch((error: unknown) => error);
    const cancel = f.jobs.cancel('aliases');
    expect(f.send).toHaveBeenCalledTimes(1);
    expect(await f.jobs.cancel('clusters')).toEqual({ cancelled: false });
    accepted.resolve(new Response(JSON.stringify(row())));
    expect(await cancel).toEqual({ cancelled: true });
    expect(await run).toMatchObject({ code: 'cancelled' });
    expect(f.send.mock.calls[1]).toEqual(['POST', '/api/v1/jobs/7/cancel']);
    expect(f.off).toHaveBeenCalledTimes(2);
  });
  it('ignores a stale poll after SSE failure and cleans listeners', async () => {
    const f = fixture();
    const page = deferred<{ items: Job[]; nextCursor: null }>();
    f.get.mockReturnValueOnce(page.promise);
    const run = f.jobs.run('clusters').catch((error: unknown) => error);
    await vi.waitFor(() => expect(f.get).toHaveBeenCalledTimes(1));
    f.emit('job.updated', row({ state: 'failed', errorCode: 'invalid_schema' }));
    expect(await run).toMatchObject({ code: 'invalid_schema' });
    page.resolve({ items: [row({ state: 'succeeded' })], nextCursor: null });
    expect(await f.jobs.cancel('clusters')).toEqual({ cancelled: false });
    expect(f.off).toHaveBeenCalledTimes(2);
  });
  it.each([0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1])(
    'rejects invalid job ID %s before any cancel or poll',
    async (id) => {
      const f = fixture();
      f.send.mockResolvedValueOnce(new Response(JSON.stringify(row({ id }))));
      await expect(f.jobs.run('clusters')).rejects.toMatchObject({ code: 'internal' });
      expect(f.get).not.toHaveBeenCalled();
      expect(await f.jobs.cancel('clusters')).toEqual({ cancelled: false });
      expect(f.off).toHaveBeenCalledTimes(2);
    },
  );
});
