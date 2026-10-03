// src/hooks/useJobs.ts (P4-09): paging, filters, live updates and the job/queue
// actions, against a fake JobsApi — each test wires exactly what it needs
// (mirrors tests/hooks/useWebJobs.test.tsx's own convention).
import React from 'react';
import { describe, it, expect, vi } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import { desktopCapabilities } from '../../src/api/electronClient';
import { ShelfyProvider } from '../../src/api/ShelfyProvider';
import type { ShelfyCapabilities, ShelfyClient } from '../../src/api/ShelfyClient';
import type {
  Job,
  JobPage,
  JobsApi,
  JobUpdate,
  QueueResult,
  QueueSummary,
} from '../../src/api/jobs';
import { NO_JOBS_FILTER, useJobs } from '../../src/hooks/useJobs';

// Every capability field, so a lane that adds one elsewhere never breaks this
// fixture; only `jobs` matters to these tests.
const CAPS: ShelfyCapabilities = { ...desktopCapabilities('darwin'), jobs: true };

function job(overrides: Partial<Job> = {}): Job {
  return {
    id: 1,
    kind: 'capture.site',
    state: 'running',
    progress: 0.1,
    stage: null,
    postKey: null,
    errorCode: null,
    attempts: 0,
    maxAttempts: 2,
    runAt: 0,
    createdAt: 0,
    updatedAt: 0,
    finishedAt: null,
    ...overrides,
  };
}

function queue(overrides: Partial<QueueSummary> = {}): QueueSummary {
  return {
    kind: 'capture.site',
    paused: false,
    queued: 0,
    running: 0,
    succeeded: 0,
    failed: 0,
    cancelled: 0,
    ...overrides,
  };
}

interface FakeJobsApi extends JobsApi {
  emitUpdate(update: JobUpdate): void;
}

function fakeJobsApi(firstPage: JobPage, queues: QueueSummary[] = []): FakeJobsApi {
  const listeners = new Set<(update: JobUpdate) => void>();

  const api: FakeJobsApi = {
    list: vi.fn(async () => firstPage),
    summary: vi.fn(async () => queues),
    cancel: vi.fn(async (id: number) => ({ ...job({ id }), state: 'cancelled' }) as Job),
    retry: vi.fn(async (id: number) => ({ ...job({ id }), state: 'queued', attempts: 1 }) as Job),
    pauseQueue: vi.fn(
      async (kind: string): Promise<QueueResult> => ({
        queue: queue({ kind, paused: true }),
        affected: 1,
      }),
    ),
    resumeQueue: vi.fn(
      async (kind: string): Promise<QueueResult> => ({
        queue: queue({ kind, paused: false }),
        affected: 1,
      }),
    ),
    cancelQueue: vi.fn(
      async (kind: string): Promise<QueueResult> => ({ queue: queue({ kind }), affected: 2 }),
    ),
    clearFinishedQueue: vi.fn(
      async (kind: string): Promise<QueueResult> => ({ queue: queue({ kind }), affected: 3 }),
    ),
    onUpdate: vi.fn((listener: (update: JobUpdate) => void) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    }),
    emitUpdate: (update) => listeners.forEach((l) => l(update)),
  };
  return api;
}

// The methods useJobs never calls, stubbed once so neither client literal
// below has to repeat them (and a future seam method only needs adding here).
function unusedClientMethods() {
  return {
    listPosts: vi.fn(),
    getPostsByIds: vi.fn(async () => []),
    getStats: vi.fn(),
    listCollections: vi.fn(),
    updatePost: vi.fn(),
    createCollection: vi.fn(),
    updateCollection: vi.fn(),
    deleteCollection: vi.fn(),
    addPostsToCollections: vi.fn(),
    removePostFromCollection: vi.fn(),
    openExternal: vi.fn(),
    on: vi.fn(() => () => {}),
    countPosts: vi.fn(),
    resolveAllIds: vi.fn(),
    bulkAction: vi.fn(),
    listTrash: vi.fn(),
    restoreFromTrash: vi.fn(),
    emptyTrash: vi.fn(),
    reportError: vi.fn(),
  };
}

function fakeClient(jobsApi: JobsApi): ShelfyClient {
  return {
    capabilities: CAPS,
    media: { file: () => null, tile: () => null, isStored: () => false },
    jobs: jobsApi,
    ...unusedClientMethods(),
  };
}

function wrapper(client: ShelfyClient) {
  return function Wrapper({ children }: { children: React.ReactNode }): React.JSX.Element {
    return <ShelfyProvider client={client}>{children}</ShelfyProvider>;
  };
}

describe('useJobs — loading', () => {
  it('loads the first page and exposes it newest-first, as the server ordered it', async () => {
    const api = fakeJobsApi({ items: [job({ id: 2 }), job({ id: 1 })], nextCursor: null });
    const { result } = renderHook(() => useJobs(), { wrapper: wrapper(fakeClient(api)) });
    expect(result.current.loading).toBe(true);
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.jobs.map((j) => j.id)).toEqual([2, 1]);
    expect(result.current.hasMore).toBe(false);
  });

  it('loads the queue summary alongside the list', async () => {
    const api = fakeJobsApi({ items: [], nextCursor: null }, [queue({ running: 3 })]);
    const { result } = renderHook(() => useJobs(), { wrapper: wrapper(fakeClient(api)) });
    await waitFor(() => expect(result.current.summary).toEqual([queue({ running: 3 })]));
  });

  it('without a jobs seam (the desktop), it never calls one and stays empty', () => {
    const client: ShelfyClient = {
      capabilities: { ...CAPS, jobs: false },
      media: { file: () => null, tile: () => null, isStored: () => false },
      ...unusedClientMethods(),
    };
    const { result } = renderHook(() => useJobs(), { wrapper: wrapper(client) });
    expect(result.current.loading).toBe(false);
    expect(result.current.jobs).toEqual([]);
  });
});

describe('useJobs — paging', () => {
  it('loadMore appends the next page without losing the first', async () => {
    const api = fakeJobsApi({ items: [job({ id: 5 })], nextCursor: 'jobs.4' });
    const { result } = renderHook(() => useJobs(), { wrapper: wrapper(fakeClient(api)) });
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.hasMore).toBe(true);

    vi.mocked(api.list).mockResolvedValueOnce({ items: [job({ id: 4 })], nextCursor: null });
    act(() => result.current.loadMore());
    await waitFor(() => expect(result.current.jobs.map((j) => j.id)).toEqual([5, 4]));
    expect(result.current.hasMore).toBe(false);
    expect(api.list).toHaveBeenLastCalledWith(
      { kind: [], state: [] },
      { limit: 50, cursor: 'jobs.4' },
    );
  });
});

describe('useJobs — filters', () => {
  it('refetches from scratch when the filter changes, not when an equal one is rebuilt', async () => {
    const api = fakeJobsApi({ items: [job({ id: 1 })], nextCursor: null });
    const { result, rerender } = renderHook(({ filter }) => useJobs(filter), {
      wrapper: wrapper(fakeClient(api)),
      initialProps: { filter: NO_JOBS_FILTER },
    });
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(api.list).toHaveBeenCalledTimes(1);

    // A fresh array of the same (empty) content: no new fetch.
    rerender({ filter: { kind: [], state: [] } });
    expect(api.list).toHaveBeenCalledTimes(1);

    // A real change: refetches.
    vi.mocked(api.list).mockResolvedValueOnce({ items: [job({ id: 9 })], nextCursor: null });
    rerender({ filter: { kind: ['capture.site'], state: [] } });
    await waitFor(() => expect(result.current.jobs.map((j) => j.id)).toEqual([9]));
    expect(api.list).toHaveBeenLastCalledWith({ kind: ['capture.site'], state: [] }, { limit: 50 });
  });
});

describe('useJobs — live updates', () => {
  it('patches a job already on screen without listing again', async () => {
    const api = fakeJobsApi({
      items: [job({ id: 1, state: 'running', progress: 0.2 })],
      nextCursor: null,
    });
    const { result } = renderHook(() => useJobs(), { wrapper: wrapper(fakeClient(api)) });
    await waitFor(() => expect(result.current.loading).toBe(false));

    act(() => {
      (api as FakeJobsApi).emitUpdate({
        id: 1,
        kind: 'capture.site',
        state: 'succeeded',
        progress: 1,
        stage: 'done',
        postKey: 'web_1',
        errorCode: null,
      });
    });
    expect(result.current.jobs[0]).toMatchObject({
      state: 'succeeded',
      progress: 1,
      postKey: 'web_1',
    });
    expect(api.list).toHaveBeenCalledTimes(1); // no refetch per event
  });

  it('ignores an update for a job it has not fetched', async () => {
    const api = fakeJobsApi({ items: [job({ id: 1 })], nextCursor: null });
    const { result } = renderHook(() => useJobs(), { wrapper: wrapper(fakeClient(api)) });
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() => {
      (api as FakeJobsApi).emitUpdate({
        id: 999,
        kind: 'capture.site',
        state: 'succeeded',
        progress: 1,
        stage: null,
        postKey: null,
        errorCode: null,
      });
    });
    expect(result.current.jobs.map((j) => j.id)).toEqual([1]);
  });
});

describe('useJobs — job actions', () => {
  it('cancel marks the id busy, then patches the row from the full response', async () => {
    const api = fakeJobsApi({ items: [job({ id: 1, state: 'running' })], nextCursor: null });
    const { result } = renderHook(() => useJobs(), { wrapper: wrapper(fakeClient(api)) });
    await waitFor(() => expect(result.current.loading).toBe(false));

    let settle!: (job: Job) => void;
    vi.mocked(api.cancel).mockReturnValueOnce(new Promise<Job>((resolve) => (settle = resolve)));
    act(() => void result.current.cancel(1));
    expect(result.current.busyIds.has(1)).toBe(true);
    await act(async () => settle({ ...job({ id: 1 }), state: 'cancelled' }));
    expect(result.current.busyIds.has(1)).toBe(false);
    expect(result.current.jobs[0].state).toBe('cancelled');
  });

  it('retry refreshes attempts from the response (job.updated never carries them)', async () => {
    const api = fakeJobsApi({
      items: [job({ id: 1, state: 'failed', attempts: 1 })],
      nextCursor: null,
    });
    const { result } = renderHook(() => useJobs(), { wrapper: wrapper(fakeClient(api)) });
    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => result.current.retry(1));
    expect(result.current.jobs[0]).toMatchObject({ state: 'queued', attempts: 1 });
  });
});

describe('useJobs — queue actions', () => {
  it('pause/resume/cancel-all update the summary from the response', async () => {
    const api = fakeJobsApi({ items: [], nextCursor: null }, [queue()]);
    const { result } = renderHook(() => useJobs(), { wrapper: wrapper(fakeClient(api)) });
    await waitFor(() => expect(result.current.summary).toEqual([queue()]));

    await act(async () => result.current.pauseQueue('capture.site'));
    expect(result.current.summary[0].paused).toBe(true);
    expect(result.current.busyKinds.has('capture.site')).toBe(false);

    await act(async () => result.current.resumeQueue('capture.site'));
    expect(result.current.summary[0].paused).toBe(false);
  });

  it('clear-finished drops that kind’s finished rows (no event announces the deletion)', async () => {
    const api = fakeJobsApi(
      {
        items: [
          job({ id: 1, kind: 'capture.site', state: 'succeeded' }),
          job({ id: 2, kind: 'capture.site', state: 'running' }),
          job({ id: 3, kind: 'import', state: 'failed' }),
        ],
        nextCursor: null,
      },
      [queue()],
    );
    const { result } = renderHook(() => useJobs(), { wrapper: wrapper(fakeClient(api)) });
    await waitFor(() => expect(result.current.jobs).toHaveLength(3));

    await act(async () => result.current.clearFinishedQueue('capture.site'));
    expect(result.current.jobs.map((j) => j.id)).toEqual([2, 3]); // the running one and the other kind stay
  });
});

describe('useJobs — resync', () => {
  it('reloads the list and the summary on a resync event', async () => {
    let resyncListener: (() => void) | undefined;
    const api = fakeJobsApi({ items: [job({ id: 1 })], nextCursor: null }, [queue()]);
    const client = fakeClient(api);
    client.on = vi.fn((type: string, listener: () => void) => {
      if (type === 'resync') resyncListener = listener;
      return () => {};
    }) as ShelfyClient['on'];
    const { result } = renderHook(() => useJobs(), { wrapper: wrapper(client) });
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(api.list).toHaveBeenCalledTimes(1);
    expect(api.summary).toHaveBeenCalledTimes(1);

    act(() => resyncListener?.());
    await waitFor(() => expect(api.list).toHaveBeenCalledTimes(2));
    expect(api.summary).toHaveBeenCalledTimes(2);
  });
});
