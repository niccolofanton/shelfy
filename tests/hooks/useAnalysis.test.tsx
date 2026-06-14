import { describe, it, expect, vi } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import { useAnalysis } from '../../src/hooks/useAnalysis';

type ProgressCallback = (data: unknown) => void;

// ─── 1. Initial load ─────────────────────────────────────────────────────────

describe('useAnalysis — initial load', () => {
  it('calls getAnalyzeStatus and getModelStatus on mount', async () => {
    vi.mocked(window.electronAPI.getAnalyzeStatus).mockResolvedValue([]);
    vi.mocked(window.electronAPI.getModelStatus).mockResolvedValue({
      ready: true,
      downloading: false,
    });

    renderHook(() => useAnalysis());

    await waitFor(() => {
      expect(window.electronAPI.getAnalyzeStatus).toHaveBeenCalled();
      expect(window.electronAPI.getModelStatus).toHaveBeenCalled();
    });
  });

  it('populates jobs and modelStatus from API responses', async () => {
    const fakeJobs = [{ key: 'p1:analyze', status: 'queued', progress: 0 }];
    const fakeModel = { ready: false, downloading: true, files: { model: false }, name: 'vlm' };
    vi.mocked(window.electronAPI.getAnalyzeStatus).mockResolvedValue(fakeJobs);
    vi.mocked(window.electronAPI.getModelStatus).mockResolvedValue(fakeModel);

    const { result } = renderHook(() => useAnalysis());

    await waitFor(() => expect(result.current.jobs).toEqual(fakeJobs));
    expect(result.current.modelStatus).toEqual(fakeModel);
  });

  it('defaults jobs to [] when getAnalyzeStatus resolves null', async () => {
    vi.mocked(window.electronAPI.getAnalyzeStatus).mockResolvedValue(null as unknown as unknown[]);

    const { result } = renderHook(() => useAnalysis());

    await waitFor(() => expect(window.electronAPI.getAnalyzeStatus).toHaveBeenCalled());
    expect(result.current.jobs).toEqual([]);
  });
});

// ─── 2. onAnalyzeProgress — new job ──────────────────────────────────────────

describe('useAnalysis — onAnalyzeProgress new job', () => {
  it('adds a new entry to jobs when key is unknown', async () => {
    vi.mocked(window.electronAPI.getAnalyzeStatus).mockResolvedValue([]);

    let capturedCallback: ProgressCallback | undefined;
    vi.mocked(window.electronAPI.onAnalyzeProgress).mockImplementation((cb) => {
      capturedCallback = cb;
      return () => {};
    });

    const { result } = renderHook(() => useAnalysis());
    await waitFor(() => expect(result.current.jobs).toEqual([]));

    const newJob = { key: 'p99:analyze', status: 'running', progress: 0.4 };
    act(() => {
      capturedCallback!(newJob);
    });

    expect(result.current.jobs).toHaveLength(1);
    expect(result.current.jobs[0]).toEqual(newJob);
  });
});

// ─── 3. onAnalyzeProgress — update existing job ──────────────────────────────

describe('useAnalysis — onAnalyzeProgress update existing job', () => {
  it('updates the matching job by key rather than adding a duplicate', async () => {
    const existing = { key: 'p1:analyze', status: 'running', progress: 0.1 };
    vi.mocked(window.electronAPI.getAnalyzeStatus).mockResolvedValue([existing]);

    let capturedCallback: ProgressCallback | undefined;
    vi.mocked(window.electronAPI.onAnalyzeProgress).mockImplementation((cb) => {
      capturedCallback = cb;
      return () => {};
    });

    const { result } = renderHook(() => useAnalysis());
    await waitFor(() => expect(result.current.jobs).toHaveLength(1));

    act(() => {
      capturedCallback!({ key: 'p1:analyze', status: 'done', progress: 1 });
    });

    expect(result.current.jobs).toHaveLength(1);
    expect(result.current.jobs[0].status).toBe('done');
    expect((result.current.jobs[0] as { progress?: number }).progress).toBe(1);
  });

  it('adds a new entry when key differs', async () => {
    const existing = { key: 'p1:analyze', status: 'done', progress: 1 };
    vi.mocked(window.electronAPI.getAnalyzeStatus).mockResolvedValue([existing]);

    let capturedCallback: ProgressCallback | undefined;
    vi.mocked(window.electronAPI.onAnalyzeProgress).mockImplementation((cb) => {
      capturedCallback = cb;
      return () => {};
    });

    const { result } = renderHook(() => useAnalysis());
    await waitFor(() => expect(result.current.jobs).toHaveLength(1));

    act(() => {
      capturedCallback!({ key: 'p2:analyze', status: 'queued', progress: 0 });
    });

    expect(result.current.jobs).toHaveLength(2);
  });
});

// ─── 4. jobFor ───────────────────────────────────────────────────────────────

describe('useAnalysis — jobFor', () => {
  it('returns the job whose key matches `${postId}:analyze`', async () => {
    const jobs = [
      { key: 'p1:analyze', status: 'running', progress: 0.5 },
      { key: 'p2:analyze', status: 'queued', progress: 0 },
    ];
    vi.mocked(window.electronAPI.getAnalyzeStatus).mockResolvedValue(jobs);

    const { result } = renderHook(() => useAnalysis());
    await waitFor(() => expect(result.current.jobs).toHaveLength(2));

    expect(result.current.jobFor('p1')).toEqual(jobs[0]);
    expect(result.current.jobFor('p2')).toEqual(jobs[1]);
  });

  it('returns null when no job matches the postId', async () => {
    vi.mocked(window.electronAPI.getAnalyzeStatus).mockResolvedValue([
      { key: 'p1:analyze', status: 'done' },
    ]);

    const { result } = renderHook(() => useAnalysis());
    await waitFor(() => expect(result.current.jobs).toHaveLength(1));

    expect(result.current.jobFor('zzz')).toBeNull();
  });
});

// ─── 5. Action methods delegate to electronAPI ───────────────────────────────

describe('useAnalysis — actions', () => {
  it('analyzePost calls electronAPI.analyzePost with the postId', async () => {
    const { result } = renderHook(() => useAnalysis());
    await waitFor(() => expect(window.electronAPI.getModelStatus).toHaveBeenCalled());

    act(() => {
      result.current.analyzePost('p1');
    });
    expect(window.electronAPI.analyzePost).toHaveBeenCalledWith('p1');
  });

  it('analyzeAll calls electronAPI.analyzeAll', async () => {
    const { result } = renderHook(() => useAnalysis());
    await waitFor(() => expect(window.electronAPI.getModelStatus).toHaveBeenCalled());

    act(() => {
      result.current.analyzeAll();
    });
    expect(window.electronAPI.analyzeAll).toHaveBeenCalled();
  });

  it('cancelJob calls electronAPI.cancelAnalyzeJob with the key', async () => {
    const { result } = renderHook(() => useAnalysis());
    await waitFor(() => expect(window.electronAPI.getModelStatus).toHaveBeenCalled());

    act(() => {
      result.current.cancelJob('p1:analyze');
    });
    expect(window.electronAPI.cancelAnalyzeJob).toHaveBeenCalledWith('p1:analyze');
  });

  it('retryJob calls electronAPI.retryAnalyzeJob with the key', async () => {
    const { result } = renderHook(() => useAnalysis());
    await waitFor(() => expect(window.electronAPI.getModelStatus).toHaveBeenCalled());

    act(() => {
      result.current.retryJob('p1:analyze');
    });
    expect(window.electronAPI.retryAnalyzeJob).toHaveBeenCalledWith('p1:analyze');
  });

  it('refreshModel calls electronAPI.getModelStatus again and updates modelStatus', async () => {
    vi.mocked(window.electronAPI.getModelStatus).mockResolvedValue({ ready: true });

    const { result } = renderHook(() => useAnalysis());
    await waitFor(() => expect(result.current.modelStatus).toEqual({ ready: true }));

    vi.mocked(window.electronAPI.getModelStatus).mockResolvedValue({ ready: false });
    await act(async () => {
      await result.current.refreshModel();
    });

    expect(result.current.modelStatus).toEqual({ ready: false });
  });

  it('downloadModel calls electronAPI.downloadModel then refreshes the model', async () => {
    vi.mocked(window.electronAPI.downloadModel).mockResolvedValue({ ready: true });

    const { result } = renderHook(() => useAnalysis());
    await waitFor(() => expect(window.electronAPI.getModelStatus).toHaveBeenCalled());

    const modelCallsBefore = vi.mocked(window.electronAPI.getModelStatus).mock.calls.length;
    await act(async () => {
      await result.current.downloadModel();
    });

    expect(window.electronAPI.downloadModel).toHaveBeenCalled();
    expect(vi.mocked(window.electronAPI.getModelStatus).mock.calls.length).toBeGreaterThan(
      modelCallsBefore,
    );
  });
});

// ─── 6. onModelProgress ──────────────────────────────────────────────────────

describe('useAnalysis — onModelProgress', () => {
  it('updates modelProgress while downloading (progress < 1)', async () => {
    let capturedCallback: ProgressCallback | undefined;
    vi.mocked(window.electronAPI.onModelProgress).mockImplementation((cb) => {
      capturedCallback = cb;
      return () => {};
    });

    const { result } = renderHook(() => useAnalysis());
    await waitFor(() => expect(window.electronAPI.onModelProgress).toHaveBeenCalled());

    act(() => {
      capturedCallback!({ progress: 0.5, label: 'modello' });
    });
    expect(result.current.modelProgress).toEqual({ progress: 0.5, label: 'modello' });
  });

  it('clears modelProgress and refreshes the model when progress >= 1', async () => {
    let capturedCallback: ProgressCallback | undefined;
    vi.mocked(window.electronAPI.onModelProgress).mockImplementation((cb) => {
      capturedCallback = cb;
      return () => {};
    });

    const { result } = renderHook(() => useAnalysis());
    await waitFor(() => expect(window.electronAPI.getModelStatus).toHaveBeenCalled());

    const modelCallsBefore = vi.mocked(window.electronAPI.getModelStatus).mock.calls.length;
    await act(async () => {
      capturedCallback!({ progress: 1, label: 'modello' });
    });

    expect(result.current.modelProgress).toBeNull();
    await waitFor(() =>
      expect(vi.mocked(window.electronAPI.getModelStatus).mock.calls.length).toBeGreaterThan(
        modelCallsBefore,
      ),
    );
  });
});

// ─── 7. Cleanup on unmount ───────────────────────────────────────────────────

describe('useAnalysis — cleanup on unmount', () => {
  it('calls both unsub functions on unmount', () => {
    const unsubJob = vi.fn();
    const unsubModel = vi.fn();
    vi.mocked(window.electronAPI.onAnalyzeProgress).mockReturnValue(unsubJob);
    vi.mocked(window.electronAPI.onModelProgress).mockReturnValue(unsubModel);

    const { unmount } = renderHook(() => useAnalysis());
    unmount();

    expect(unsubJob).toHaveBeenCalled();
    expect(unsubModel).toHaveBeenCalled();
  });
});
