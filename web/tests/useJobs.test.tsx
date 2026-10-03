// useJobs (F19): a single-job cancel/retry refetches the queue summary, so
// the queue bar's counts (and "clear finished") follow the job.
import { describe, it, expect, vi } from 'vitest';
import React from 'react';
import { renderHook, waitFor, act } from '@testing-library/react';
import { useJobs } from '@ui/hooks/useJobs';
import { ShelfyProvider } from '@ui/api/ShelfyProvider';
import type { ShelfyClient } from '@ui/api/ShelfyClient';
import type { Job, JobsApi, QueueSummary } from '@ui/api/jobs';

function job(state: Job['state']): Job {
  return {
    id: 1,
    kind: 'purge',
    state,
    progress: null,
    stage: null,
    postKey: null,
    errorCode: null,
    attempts: 0,
    maxAttempts: 3,
    runAt: 0,
    createdAt: 0,
    updatedAt: 0,
    finishedAt: null,
  };
}

function queue(cancelled: number, queued: number): QueueSummary {
  return { kind: 'purge', paused: false, queued, running: 0, succeeded: 0, failed: 0, cancelled };
}

function setup() {
  const summary = vi
    .fn()
    .mockResolvedValueOnce([queue(0, 1)])
    .mockResolvedValueOnce([queue(1, 0)])
    .mockResolvedValueOnce([queue(0, 1)]);
  const api = {
    list: vi.fn().mockResolvedValue({ items: [job('queued')], nextCursor: null }),
    summary,
    cancel: vi.fn().mockResolvedValue(job('cancelled')),
    retry: vi.fn().mockResolvedValue(job('queued')),
    onUpdate: vi.fn().mockReturnValue(() => undefined),
  } as unknown as JobsApi;
  const client = { jobs: api, on: () => () => undefined } as unknown as ShelfyClient;
  const wrapper = ({ children }: { children: React.ReactNode }) => (
    <ShelfyProvider client={client}>{children}</ShelfyProvider>
  );
  return renderHook(() => useJobs(), { wrapper });
}

describe('useJobs single-job actions', () => {
  it('refreshes the summary after a cancel and after a retry', async () => {
    const { result } = setup();
    await waitFor(() => expect(result.current.summary[0]?.queued).toBe(1));

    await act(() => result.current.cancel(1));
    await waitFor(() => expect(result.current.summary[0]?.cancelled).toBe(1));
    expect(result.current.jobs[0]?.state).toBe('cancelled');

    await act(() => result.current.retry(1));
    await waitFor(() => expect(result.current.summary[0]?.queued).toBe(1));
    expect(result.current.summary[0]?.cancelled).toBe(0);
  });
});
