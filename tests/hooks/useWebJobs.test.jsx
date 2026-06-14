import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import { useWebJobs } from '../../src/hooks/useWebJobs.js';

// The shared setup.js mock doesn't cover the web-capture IPC surface; the hook
// reads it via optional chaining, so each test wires exactly what it needs.
let capturedCallback;

beforeEach(() => {
  capturedCallback = undefined;
  window.electronAPI.getWebStatus = vi.fn().mockResolvedValue([]);
  window.electronAPI.onWebProgress = vi.fn((cb) => {
    capturedCallback = cb;
    return () => {};
  });
  window.electronAPI.clearCompletedWeb = vi.fn().mockResolvedValue(undefined);
});

const doneJob = (id) => ({ postId: id, status: 'done', url: `https://${id}.test` });

// ─── Initial snapshot ─────────────────────────────────────────────────────────

describe('useWebJobs — initial snapshot', () => {
  it('loads jobs from getWebStatus on mount', async () => {
    window.electronAPI.getWebStatus.mockResolvedValue([
      { postId: 'p1', status: 'capturing', url: 'https://a.test' },
    ]);

    const { result } = renderHook(() => useWebJobs());

    await waitFor(() => expect(result.current.jobs).toHaveLength(1));
    expect(result.current.jobs[0].postId).toBe('p1');
  });

  it('merges live progress fields over the snapshot instead of clobbering them', async () => {
    window.electronAPI.getWebStatus.mockResolvedValue([
      { postId: 'p1', status: 'capturing', domain: 'a.test' },
    ]);

    const { result } = renderHook(() => useWebJobs());
    await waitFor(() => expect(result.current.jobs).toHaveLength(1));

    act(() => {
      capturedCallback({ postId: 'p1', status: 'extracting' });
    });

    await waitFor(() => expect(result.current.jobs[0].status).toBe('extracting'));
    expect(result.current.jobs[0].domain).toBe('a.test');
  });
});

// ─── Coalescing — bursts flush throttled ──────────────────────────────────────

describe('useWebJobs — progress event coalescing', () => {
  beforeEach(() =>
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval'] }),
  );
  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  it('publishes the first event immediately and the rest of the burst in one trailing flush', async () => {
    const { result } = renderHook(() => useWebJobs());
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    // Leading edge: first event of a quiet spell renders right away.
    act(() => {
      capturedCallback({ postId: 'p1', status: 'discovering' });
    });
    expect(result.current.jobs[0].status).toBe('discovering');

    // Burst inside the window: buffered.
    act(() => {
      capturedCallback({ postId: 'p1', status: 'capturing' });
      capturedCallback({ postId: 'p1', status: 'extracting' });
    });
    expect(result.current.jobs[0].status).toBe('discovering');

    // Trailing flush publishes the latest state.
    await act(async () => {
      vi.advanceTimersByTime(100);
    });
    expect(result.current.jobs[0].status).toBe('extracting');
  });
});

// ─── Pruning — bounded completed backlog ──────────────────────────────────────

describe('useWebJobs — completed jobs pruning', () => {
  it('caps done/cancelled jobs at 200 when the queue is idle, dropping the oldest', async () => {
    const snapshot = Array.from({ length: 250 }, (_, i) => doneJob(`p${i + 1}`));
    window.electronAPI.getWebStatus.mockResolvedValue(snapshot);

    const { result } = renderHook(() => useWebJobs());

    await waitFor(() => expect(result.current.jobs).toHaveLength(200));
    expect(result.current.jobs[0].postId).toBe('p51');
    expect(result.current.jobs.at(-1).postId).toBe('p250');
  });

  it('never prunes error jobs (they carry the retry action)', async () => {
    const snapshot = [
      { postId: 'e1', status: 'error', error: 'boom' },
      ...Array.from({ length: 250 }, (_, i) => doneJob(`p${i + 1}`)),
    ];
    window.electronAPI.getWebStatus.mockResolvedValue(snapshot);

    const { result } = renderHook(() => useWebJobs());

    await waitFor(() => expect(result.current.jobs).toHaveLength(201));
    expect(result.current.jobs.some((j) => j.postId === 'e1')).toBe(true);
  });

  it('does not prune while a job is still active (badge counters stay honest)', async () => {
    const snapshot = [
      ...Array.from({ length: 250 }, (_, i) => doneJob(`p${i + 1}`)),
      { postId: 'live', status: 'capturing' },
    ];
    window.electronAPI.getWebStatus.mockResolvedValue(snapshot);

    const { result } = renderHook(() => useWebJobs());

    await waitFor(() => expect(result.current.jobs).toHaveLength(251));
  });
});

// ─── clearCompleted ───────────────────────────────────────────────────────────

describe('useWebJobs — clearCompleted', () => {
  it('drops done/error/cancelled entries and keeps active ones', async () => {
    window.electronAPI.getWebStatus.mockResolvedValue([
      doneJob('p1'),
      { postId: 'p2', status: 'error' },
      { postId: 'p3', status: 'capturing' },
    ]);

    const { result } = renderHook(() => useWebJobs());
    await waitFor(() => expect(result.current.jobs).toHaveLength(3));

    await act(async () => {
      await result.current.clearCompleted();
    });

    expect(window.electronAPI.clearCompletedWeb).toHaveBeenCalled();
    expect(result.current.jobs).toHaveLength(1);
    expect(result.current.jobs[0].postId).toBe('p3');
  });
});
