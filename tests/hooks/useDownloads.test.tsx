import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import { useDownloads } from '../../src/hooks/useDownloads';

type ProgressCallback = (data: unknown) => void;

// ─── 1. Initial load (real timers — waitFor works normally) ───────────────────

describe('useDownloads — initial load', () => {
  it('calls getDownloadStatus and getStats on mount', async () => {
    vi.mocked(window.electronAPI.getDownloadStatus).mockResolvedValue([]);
    vi.mocked(window.electronAPI.getStats).mockResolvedValue({
      total: 5,
      downloaded: { thumbnails: 2, images: 1, videos: 0 },
    } as unknown as Shelfy.Stats);

    renderHook(() => useDownloads());

    await waitFor(() => {
      expect(window.electronAPI.getDownloadStatus).toHaveBeenCalled();
      expect(window.electronAPI.getStats).toHaveBeenCalled();
    });
  });

  it('populates jobs and stats from API responses', async () => {
    const fakeJobs = [{ postId: 'p1', assetType: 'thumbnail', status: 'done', progress: 1 }];
    vi.mocked(window.electronAPI.getDownloadStatus).mockResolvedValue(fakeJobs);
    vi.mocked(window.electronAPI.getStats).mockResolvedValue({
      total: 10,
      downloadedByType: { thumbnails: 3, images: 2, videos: 1 },
    } as unknown as Shelfy.Stats);

    const { result } = renderHook(() => useDownloads());

    await waitFor(() => expect(result.current.jobs).toEqual(fakeJobs));
    expect(result.current.stats).toEqual({ total: 10, thumbnails: 3, images: 2, videos: 1 });
  });

  it('defaults stats fields to 0 when downloaded sub-keys are missing', async () => {
    vi.mocked(window.electronAPI.getStats).mockResolvedValue({
      total: 2,
    } as unknown as Shelfy.Stats);

    const { result } = renderHook(() => useDownloads());

    await waitFor(() => expect(result.current.stats.total).toBe(2));
    expect(result.current.stats.thumbnails).toBe(0);
    expect(result.current.stats.images).toBe(0);
    expect(result.current.stats.videos).toBe(0);
  });
});

// ─── 2. onDownloadProgress — new job ─────────────────────────────────────────

describe('useDownloads — onDownloadProgress new job', () => {
  it('adds a new entry to jobs when postId/assetType is unknown', async () => {
    vi.mocked(window.electronAPI.getDownloadStatus).mockResolvedValue([]);

    let capturedCallback: ProgressCallback | undefined;
    vi.mocked(window.electronAPI.onDownloadProgress).mockImplementation((cb) => {
      capturedCallback = cb;
      return () => {};
    });

    const { result } = renderHook(() => useDownloads());
    await waitFor(() => expect(result.current.jobs).toEqual([]));

    const newEvent = {
      postId: 'p99',
      assetType: 'thumbnail',
      status: 'downloading',
      progress: 0.3,
    };
    act(() => {
      capturedCallback!(newEvent);
    });

    expect(result.current.jobs).toHaveLength(1);
    expect(result.current.jobs[0]).toEqual(newEvent);
  });
});

// ─── 3. onDownloadProgress — update existing job ──────────────────────────────

describe('useDownloads — onDownloadProgress update existing job', () => {
  it('updates the matching job rather than adding a duplicate', async () => {
    const existingJob = { postId: 'p1', assetType: 'video', status: 'downloading', progress: 0.1 };
    vi.mocked(window.electronAPI.getDownloadStatus).mockResolvedValue([existingJob]);

    let capturedCallback: ProgressCallback | undefined;
    vi.mocked(window.electronAPI.onDownloadProgress).mockImplementation((cb) => {
      capturedCallback = cb;
      return () => {};
    });

    const { result } = renderHook(() => useDownloads());
    await waitFor(() => expect(result.current.jobs).toHaveLength(1));

    act(() => {
      capturedCallback!({ postId: 'p1', assetType: 'video', status: 'downloading', progress: 0.7 });
    });

    expect(result.current.jobs).toHaveLength(1);
    expect(result.current.jobs[0].progress).toBe(0.7);
  });

  it('adds a new entry when assetType differs', async () => {
    // The hook dedupes by job.key, so distinct asset types of the same post
    // carry distinct keys and coexist as separate entries.
    const existingJob = {
      key: 'p1:thumbnail',
      postId: 'p1',
      assetType: 'thumbnail',
      status: 'done',
      progress: 1,
    };
    vi.mocked(window.electronAPI.getDownloadStatus).mockResolvedValue([existingJob]);

    let capturedCallback: ProgressCallback | undefined;
    vi.mocked(window.electronAPI.onDownloadProgress).mockImplementation((cb) => {
      capturedCallback = cb;
      return () => {};
    });

    const { result } = renderHook(() => useDownloads());
    await waitFor(() => expect(result.current.jobs).toHaveLength(1));

    act(() => {
      capturedCallback!({
        key: 'p1:video',
        postId: 'p1',
        assetType: 'video',
        status: 'downloading',
        progress: 0.2,
      });
    });

    expect(result.current.jobs).toHaveLength(2);
  });
});

// ─── 4. onDownloadProgress — done event refreshes stats ───────────────────────

describe('useDownloads — onDownloadProgress done event', () => {
  it('calls getStats again when event.status === "done"', async () => {
    vi.mocked(window.electronAPI.getDownloadStatus).mockResolvedValue([]);

    let capturedCallback: ProgressCallback | undefined;
    vi.mocked(window.electronAPI.onDownloadProgress).mockImplementation((cb) => {
      capturedCallback = cb;
      return () => {};
    });

    renderHook(() => useDownloads());
    await waitFor(() => expect(window.electronAPI.getStats).toHaveBeenCalledTimes(1));

    act(() => {
      capturedCallback!({ postId: 'p1', assetType: 'thumbnail', status: 'done', progress: 1 });
    });

    await waitFor(() => expect(window.electronAPI.getStats).toHaveBeenCalledTimes(2));
  });

  it('does NOT call getStats again for non-done events', async () => {
    vi.mocked(window.electronAPI.getDownloadStatus).mockResolvedValue([]);

    let capturedCallback: ProgressCallback | undefined;
    vi.mocked(window.electronAPI.onDownloadProgress).mockImplementation((cb) => {
      capturedCallback = cb;
      return () => {};
    });

    renderHook(() => useDownloads());
    await waitFor(() => expect(window.electronAPI.getStats).toHaveBeenCalledTimes(1));

    act(() => {
      capturedCallback!({
        postId: 'p1',
        assetType: 'thumbnail',
        status: 'downloading',
        progress: 0.5,
      });
    });

    await act(async () => {});

    expect(window.electronAPI.getStats).toHaveBeenCalledTimes(1);
  });
});

// ─── 4b. Coalescing — bursts of progress events flush throttled ───────────────
// Progress events can arrive 10-30+/s per download; the hook publishes the first
// event of a quiet spell immediately (leading) and coalesces the rest of the
// burst into a single trailing flush per ~100ms window.

describe('useDownloads — progress event coalescing', () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval'] });
  });
  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  it('publishes the first event immediately and the rest of the burst in one trailing flush', async () => {
    vi.mocked(window.electronAPI.getDownloadStatus).mockResolvedValue([]);

    let capturedCallback: ProgressCallback | undefined;
    vi.mocked(window.electronAPI.onDownloadProgress).mockImplementation((cb) => {
      capturedCallback = cb;
      return () => {};
    });

    const { result } = renderHook(() => useDownloads());
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    const evt = (progress: number) => ({
      key: 'p1:video',
      postId: 'p1',
      assetType: 'video',
      status: 'downloading',
      progress,
    });

    // Leading edge: the first event renders right away.
    act(() => {
      capturedCallback!(evt(0.1));
    });
    expect(result.current.jobs[0].progress).toBe(0.1);

    // Burst within the window: buffered, no intermediate publish.
    act(() => {
      capturedCallback!(evt(0.2));
      capturedCallback!(evt(0.3));
      capturedCallback!(evt(0.4));
    });
    expect(result.current.jobs[0].progress).toBe(0.1);

    // Trailing flush publishes the latest state.
    await act(async () => {
      vi.advanceTimersByTime(100);
    });
    expect(result.current.jobs[0].progress).toBe(0.4);
  });

  it('coalesces getStats for a burst of done events into leading + trailing calls', async () => {
    vi.mocked(window.electronAPI.getDownloadStatus).mockResolvedValue([]);

    let capturedCallback: ProgressCallback | undefined;
    vi.mocked(window.electronAPI.onDownloadProgress).mockImplementation((cb) => {
      capturedCallback = cb;
      return () => {};
    });

    renderHook(() => useDownloads());
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    const callsAfterMount = vi.mocked(window.electronAPI.getStats).mock.calls.length;

    const done = (id: string) => ({
      key: `${id}:thumbnail`,
      postId: id,
      assetType: 'thumbnail',
      status: 'done',
      progress: 1,
    });
    await act(async () => {
      capturedCallback!(done('p1'));
      capturedCallback!(done('p2'));
      capturedCallback!(done('p3'));
    });

    // Leading refresh only — the rest of the burst is deferred.
    expect(vi.mocked(window.electronAPI.getStats).mock.calls.length).toBe(callsAfterMount + 1);

    // Trailing refresh captures the final totals.
    await act(async () => {
      vi.advanceTimersByTime(800);
      await Promise.resolve();
    });
    expect(vi.mocked(window.electronAPI.getStats).mock.calls.length).toBe(callsAfterMount + 2);
  });
});

// ─── 5. Periodic stats refresh (fake timers for setInterval control) ──────────
// getStats runs several COUNT(*) aggregations, so the hook now polls ONLY while
// the queue is active (status pending/downloading). These tests exercise the
// active-queue contract; the idle case (no polling) is covered explicitly below.

describe('useDownloads — periodic stats refresh', () => {
  // An active job so `hasQueue` is true and the polling interval is armed.
  const activeJob = {
    key: 'p1:video',
    postId: 'p1',
    assetType: 'video',
    status: 'downloading',
    progress: 0.1,
  };

  // Only fake setTimeout/setInterval — leave queueMicrotask intact so React's
  // scheduler (which uses queueMicrotask in React 18) doesn't infinite-loop.
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval'] });
  });
  afterEach(() => {
    vi.clearAllTimers(); // clear without running — vi.runAllTimers would loop on setInterval
    vi.useRealTimers();
  });

  it('polls getStats every 5 seconds while the queue is active', async () => {
    vi.mocked(window.electronAPI.getDownloadStatus).mockResolvedValue([activeJob]);

    renderHook(() => useDownloads());

    // Flush the mount (refreshJobs → setJobs → hasQueue=true arms the interval).
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    const callsBefore = vi.mocked(window.electronAPI.getStats).mock.calls.length;

    await act(async () => {
      vi.advanceTimersByTime(5000);
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(vi.mocked(window.electronAPI.getStats).mock.calls.length).toBeGreaterThan(callsBefore);
  });

  it('polls getStats over multiple 5s intervals while the queue is active', async () => {
    vi.mocked(window.electronAPI.getDownloadStatus).mockResolvedValue([activeJob]);

    renderHook(() => useDownloads());
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    const callsBefore = vi.mocked(window.electronAPI.getStats).mock.calls.length;

    // Advance 3 × 5s one step at a time to avoid running too many timers at once
    for (let i = 0; i < 3; i++) {
      await act(async () => {
        vi.advanceTimersByTime(5000);
        await Promise.resolve();
        await Promise.resolve();
      });
    }

    // Three interval ticks on top of the initial mount call.
    expect(vi.mocked(window.electronAPI.getStats).mock.calls.length).toBeGreaterThanOrEqual(
      callsBefore + 3,
    );
  });

  it('does NOT poll getStats when the queue is idle (empty)', async () => {
    vi.mocked(window.electronAPI.getDownloadStatus).mockResolvedValue([]);

    renderHook(() => useDownloads());
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    const callsBefore = vi.mocked(window.electronAPI.getStats).mock.calls.length;

    await act(async () => {
      vi.advanceTimersByTime(15000);
      await Promise.resolve();
      await Promise.resolve();
    });

    // No active queue → interval never armed → no extra getStats round-trips.
    expect(vi.mocked(window.electronAPI.getStats).mock.calls.length).toBe(callsBefore);
  });
});

// ─── 6. Cleanup on unmount ────────────────────────────────────────────────────

describe('useDownloads — cleanup on unmount', () => {
  it('calls unsub() when unmounted', () => {
    const unsub = vi.fn();
    vi.mocked(window.electronAPI.onDownloadProgress).mockReturnValue(unsub);
    vi.mocked(window.electronAPI.getDownloadStatus).mockResolvedValue([]);

    const { unmount } = renderHook(() => useDownloads());
    unmount();

    expect(unsub).toHaveBeenCalled();
  });

  it('stops calling getStats after unmount (interval cleared)', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval'] });
    vi.mocked(window.electronAPI.getDownloadStatus).mockResolvedValue([]);

    const { unmount } = renderHook(() => useDownloads());

    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    unmount();
    const callsAtUnmount = vi.mocked(window.electronAPI.getStats).mock.calls.length;

    await act(async () => {
      vi.advanceTimersByTime(15000);
      await Promise.resolve();
    });

    expect(vi.mocked(window.electronAPI.getStats).mock.calls.length).toBe(callsAtUnmount);

    vi.runAllTimers();
    vi.useRealTimers();
  });
});

// ─── 7. refresh() ─────────────────────────────────────────────────────────────

describe('useDownloads — refresh()', () => {
  it('calling refresh() calls both getDownloadStatus and getStats again', async () => {
    vi.mocked(window.electronAPI.getDownloadStatus).mockResolvedValue([]);

    const { result } = renderHook(() => useDownloads());

    await waitFor(() => {
      expect(window.electronAPI.getDownloadStatus).toHaveBeenCalledTimes(1);
      expect(window.electronAPI.getStats).toHaveBeenCalledTimes(1);
    });

    await act(async () => {
      result.current.refresh();
    });

    await waitFor(() => {
      expect(window.electronAPI.getDownloadStatus).toHaveBeenCalledTimes(2);
      expect(window.electronAPI.getStats).toHaveBeenCalledTimes(2);
    });
  });
});
