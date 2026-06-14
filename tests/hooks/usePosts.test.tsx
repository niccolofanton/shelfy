import { describe, it, expect, vi } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import { usePosts, type PostFilters } from '../../src/hooks/usePosts';
import type { PostSearchResult } from '../../types/electron-api';

type ProgressCallback = (data: unknown) => void;
type NewPostsCallback = (data?: unknown) => void;

const defaultFilters: PostFilters = {
  platform: 'all',
  mediaType: 'all',
  search: '',
  limit: 50,
};

// ─── 1. Initial fetch (real timers — waitFor works normally) ──────────────────

describe('usePosts — initial fetch', () => {
  it('calls getPosts with correct apiFilters and populates posts', async () => {
    const fakePosts = [
      { id: '1', platform: 'instagram' },
      { id: '2', platform: 'twitter' },
    ];
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue({
      posts: fakePosts,
      total: 2,
    } as unknown as PostSearchResult);

    const { result } = renderHook(() => usePosts(defaultFilters));

    await waitFor(() => expect(result.current.posts).toEqual(fakePosts));

    expect(window.electronAPI.getPosts).toHaveBeenCalledWith({
      platform: undefined,
      mediaType: undefined,
      search: undefined,
      limit: 50,
      offset: 0,
    });
    expect(result.current.error).toBeNull();
  });
});

// ─── 2. Filter conversion ─────────────────────────────────────────────────────

describe('usePosts — filter conversion', () => {
  it('converts "all" platform and mediaType to undefined', async () => {
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue({
      posts: [],
      total: 0,
    } as unknown as PostSearchResult);

    renderHook(() => usePosts({ platform: 'all', mediaType: 'all', search: '', limit: 50 }));

    await waitFor(() => expect(window.electronAPI.getPosts).toHaveBeenCalled());

    const [called] = vi.mocked(window.electronAPI.getPosts).mock.calls[0];
    expect((called as PostFilters).platform).toBeUndefined();
    expect((called as PostFilters).mediaType).toBeUndefined();
  });

  it('passes through a specific platform value', async () => {
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue({
      posts: [],
      total: 0,
    } as unknown as PostSearchResult);

    renderHook(() => usePosts({ platform: 'instagram', mediaType: 'all', search: '', limit: 50 }));

    await waitFor(() => expect(window.electronAPI.getPosts).toHaveBeenCalled());

    const [called] = vi.mocked(window.electronAPI.getPosts).mock.calls[0];
    expect((called as PostFilters).platform).toBe('instagram');
    expect((called as PostFilters).mediaType).toBeUndefined();
  });

  it('passes through a specific mediaType value', async () => {
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue({
      posts: [],
      total: 0,
    } as unknown as PostSearchResult);

    renderHook(() => usePosts({ platform: 'all', mediaType: 'video', search: '', limit: 50 }));

    await waitFor(() => expect(window.electronAPI.getPosts).toHaveBeenCalled());

    expect(window.electronAPI.getPosts).toHaveBeenCalledWith(
      expect.objectContaining({ mediaType: 'video' }),
    );
  });
});

// ─── 3. total from backend ────────────────────────────────────────────────────

describe('usePosts — total from backend', () => {
  it('exposes the real total returned by the backend', async () => {
    const fakePosts = Array.from({ length: 50 }, (_, i) => ({ id: String(i) }));
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue({
      posts: fakePosts,
      total: 200,
    } as unknown as PostSearchResult);

    const { result } = renderHook(() => usePosts({ ...defaultFilters, limit: 50 }));

    await waitFor(() => expect(result.current.posts).toHaveLength(50));
    expect(result.current.total).toBe(200);
  });

  it('keeps total stable across limit bumps (reflects DB count, not page size)', async () => {
    const fakePosts = Array.from({ length: 12 }, (_, i) => ({ id: String(i) }));
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue({
      posts: fakePosts,
      total: 12,
    } as unknown as PostSearchResult);

    const { result } = renderHook(() => usePosts({ ...defaultFilters, limit: 50 }));

    await waitFor(() => expect(result.current.posts).toHaveLength(12));
    expect(result.current.total).toBe(12);
  });
});

// ─── 4. Search handling ───────────────────────────────────────────────────────
// The keystroke debounce lives in FilterBar (the input owner); usePosts fetches
// immediately whatever search value it receives — no second debounce layer.

describe('usePosts — search handling', () => {
  it('fetches immediately when search is set (no extra debounce in the hook)', async () => {
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue({
      posts: [],
      total: 0,
    } as unknown as PostSearchResult);

    renderHook(() => usePosts({ ...defaultFilters, search: 'hello' }));

    expect(window.electronAPI.getPosts).toHaveBeenCalledWith(
      expect.objectContaining({ search: 'hello' }),
    );
  });

  it('fetches immediately when there is no search term', async () => {
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue({
      posts: [],
      total: 0,
    } as unknown as PostSearchResult);

    renderHook(() => usePosts({ ...defaultFilters, search: '' }));

    expect(window.electronAPI.getPosts).toHaveBeenCalled();
  });
});

// ─── 5. Error handling ────────────────────────────────────────────────────────

describe('usePosts — error handling', () => {
  it('sets error to the message when getPosts rejects', async () => {
    vi.mocked(window.electronAPI.getPosts).mockRejectedValue(new Error('DB error'));

    const { result } = renderHook(() => usePosts(defaultFilters));

    await waitFor(() => expect(result.current.error).not.toBeNull());

    expect(result.current.error).toBe('DB error');
    expect(result.current.posts).toEqual([]);
  });

  it('sets a fallback error message when rejection has no message', async () => {
    vi.mocked(window.electronAPI.getPosts).mockRejectedValue(null);

    const { result } = renderHook(() => usePosts(defaultFilters));

    // Wait for the error to be set (don't check loading — it starts as false)
    await waitFor(() => expect(result.current.error).not.toBeNull());

    expect(result.current.error).toBe('Caricamento dei post non riuscito.');
  });
});

// ─── 6. reload() ─────────────────────────────────────────────────────────────

describe('usePosts — reload()', () => {
  it('calling reload() triggers another getPosts call', async () => {
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue({
      posts: [],
      total: 0,
    } as unknown as PostSearchResult);

    const { result } = renderHook(() => usePosts(defaultFilters));

    await waitFor(() => expect(result.current.loading).toBe(false));

    const callsBefore = vi.mocked(window.electronAPI.getPosts).mock.calls.length;

    await act(async () => {
      result.current.reload();
    });

    await waitFor(() =>
      expect(vi.mocked(window.electronAPI.getPosts).mock.calls.length).toBeGreaterThan(callsBefore),
    );
  });
});

// ─── 6b. New filters: category / contentType / tag ───────────────────────────

describe('usePosts — category/contentType/tag filters', () => {
  it('forwards category, contentType and tag to getPosts apiFilters', async () => {
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue({
      posts: [],
      total: 0,
    } as unknown as PostSearchResult);

    renderHook(() =>
      usePosts({
        ...defaultFilters,
        category: 'food',
        contentType: 'recipe',
        tag: 'pasta',
      }),
    );

    await waitFor(() => expect(window.electronAPI.getPosts).toHaveBeenCalled());

    expect(window.electronAPI.getPosts).toHaveBeenCalledWith(
      expect.objectContaining({
        category: 'food',
        contentType: 'recipe',
        tag: 'pasta',
      }),
    );
  });

  it('leaves category/contentType/tag undefined when not provided', async () => {
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue({
      posts: [],
      total: 0,
    } as unknown as PostSearchResult);

    renderHook(() => usePosts(defaultFilters));

    await waitFor(() => expect(window.electronAPI.getPosts).toHaveBeenCalled());

    const [called] = vi.mocked(window.electronAPI.getPosts).mock.calls[0];
    expect((called as PostFilters).category).toBeUndefined();
    expect((called as PostFilters).contentType).toBeUndefined();
    expect((called as PostFilters).tag).toBeUndefined();
  });

  it('re-fetches when category changes', async () => {
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue({
      posts: [],
      total: 0,
    } as unknown as PostSearchResult);

    const { result, rerender } = renderHook((props: PostFilters) => usePosts(props), {
      initialProps: { ...defaultFilters, category: 'food' },
    });

    await waitFor(() => expect(result.current.loading).toBe(false));
    const callsBefore = vi.mocked(window.electronAPI.getPosts).mock.calls.length;

    rerender({ ...defaultFilters, category: 'travel' });

    await waitFor(() =>
      expect(vi.mocked(window.electronAPI.getPosts).mock.calls.length).toBeGreaterThan(callsBefore),
    );
    expect(window.electronAPI.getPosts).toHaveBeenLastCalledWith(
      expect.objectContaining({ category: 'travel' }),
    );
  });

  it('re-fetches when contentType changes', async () => {
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue({
      posts: [],
      total: 0,
    } as unknown as PostSearchResult);

    const { result, rerender } = renderHook((props: PostFilters) => usePosts(props), {
      initialProps: { ...defaultFilters, contentType: 'recipe' },
    });

    await waitFor(() => expect(result.current.loading).toBe(false));
    const callsBefore = vi.mocked(window.electronAPI.getPosts).mock.calls.length;

    rerender({ ...defaultFilters, contentType: 'tutorial' });

    await waitFor(() =>
      expect(vi.mocked(window.electronAPI.getPosts).mock.calls.length).toBeGreaterThan(callsBefore),
    );
    expect(window.electronAPI.getPosts).toHaveBeenLastCalledWith(
      expect.objectContaining({ contentType: 'tutorial' }),
    );
  });

  it('re-fetches when tag changes', async () => {
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue({
      posts: [],
      total: 0,
    } as unknown as PostSearchResult);

    const { result, rerender } = renderHook((props: PostFilters) => usePosts(props), {
      initialProps: { ...defaultFilters, tag: 'pasta' },
    });

    await waitFor(() => expect(result.current.loading).toBe(false));
    const callsBefore = vi.mocked(window.electronAPI.getPosts).mock.calls.length;

    rerender({ ...defaultFilters, tag: 'pizza' });

    await waitFor(() =>
      expect(vi.mocked(window.electronAPI.getPosts).mock.calls.length).toBeGreaterThan(callsBefore),
    );
    expect(window.electronAPI.getPosts).toHaveBeenLastCalledWith(
      expect.objectContaining({ tag: 'pizza' }),
    );
  });
});

// ─── 7. onNewPosts subscription ───────────────────────────────────────────────

describe('usePosts — onNewPosts subscription', () => {
  it('subscribes to onNewPosts on mount', () => {
    renderHook(() => usePosts(defaultFilters));
    expect(window.electronAPI.onNewPosts).toHaveBeenCalled();
  });

  it('calls the unsub function returned by onNewPosts on unmount', () => {
    const unsub = vi.fn();
    vi.mocked(window.electronAPI.onNewPosts).mockReturnValue(unsub);

    const { unmount } = renderHook(() => usePosts(defaultFilters));
    unmount();

    expect(unsub).toHaveBeenCalled();
  });

  it('triggers a reload after 500ms debounce when onNewPosts fires', async () => {
    vi.useFakeTimers();
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue({
      posts: [],
      total: 0,
    } as unknown as PostSearchResult);

    let capturedCallback: NewPostsCallback | undefined;
    vi.mocked(window.electronAPI.onNewPosts).mockImplementation((cb) => {
      capturedCallback = cb as NewPostsCallback;
      return () => {};
    });

    renderHook(() => usePosts(defaultFilters));

    // Drain the initial fetch's Promise chain
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    const callsBefore = vi.mocked(window.electronAPI.getPosts).mock.calls.length;

    // Simulate a new-posts event
    act(() => {
      capturedCallback!();
    });

    // 399ms — quiet window not yet elapsed
    act(() => {
      vi.advanceTimersByTime(399);
    });
    expect(vi.mocked(window.electronAPI.getPosts).mock.calls.length).toBe(callsBefore);

    // 1ms more — quiet window elapsed → setReloadCounter fires and the re-run
    // effect dispatches getPosts immediately (no second debounce layer).
    await act(async () => {
      vi.advanceTimersByTime(1);
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(vi.mocked(window.electronAPI.getPosts).mock.calls.length).toBeGreaterThan(callsBefore);

    vi.runAllTimers();
    vi.useRealTimers();
  });
});

// ─── 8. Append-only pagination ────────────────────────────────────────────────

describe('usePosts — append-only pagination', () => {
  it('fetches only the missing page (offset = loaded) when limit grows', async () => {
    const firstPage = Array.from({ length: 50 }, (_, i) => ({ id: `a${i}` }));
    const secondPage = Array.from({ length: 50 }, (_, i) => ({ id: `b${i}` }));
    vi.mocked(window.electronAPI.getPosts).mockResolvedValueOnce({
      posts: firstPage,
      total: 200,
    } as unknown as PostSearchResult);

    const { result, rerender } = renderHook((props: PostFilters) => usePosts(props), {
      initialProps: { ...defaultFilters, limit: 50 },
    });
    await waitFor(() => expect(result.current.posts).toHaveLength(50));

    vi.mocked(window.electronAPI.getPosts).mockResolvedValueOnce({
      posts: secondPage,
      total: 200,
    } as unknown as PostSearchResult);
    rerender({ ...defaultFilters, limit: 100 });

    await waitFor(() => expect(result.current.posts).toHaveLength(100));
    expect(window.electronAPI.getPosts).toHaveBeenLastCalledWith(
      expect.objectContaining({ limit: 50, offset: 50 }),
    );
    // Append, not replace: the first page keeps its object identities.
    expect(result.current.posts[0]).toBe(firstPage[0]);
    expect(result.current.posts[50]).toBe(secondPage[0]);
  });

  it('deduplicates by id rows re-served by a shifted offset window', async () => {
    const firstPage = [{ id: 'p1' }, { id: 'p2' }];
    vi.mocked(window.electronAPI.getPosts).mockResolvedValueOnce({
      posts: firstPage,
      total: 10,
    } as unknown as PostSearchResult);

    const { result, rerender } = renderHook((props: PostFilters) => usePosts(props), {
      initialProps: { ...defaultFilters, limit: 2 },
    });
    await waitFor(() => expect(result.current.posts).toHaveLength(2));

    // A new row inserted at the top shifts the page: p2 comes back again.
    vi.mocked(window.electronAPI.getPosts).mockResolvedValueOnce({
      posts: [{ id: 'p2' }, { id: 'p3' }],
      total: 10,
    } as unknown as PostSearchResult);
    rerender({ ...defaultFilters, limit: 4 });

    await waitFor(() => expect(result.current.posts).toHaveLength(3));
    expect(result.current.posts.map((p) => p.id)).toEqual(['p1', 'p2', 'p3']);
  });

  it('re-fetches from offset 0 when a filter changes', async () => {
    const firstPage = Array.from({ length: 50 }, (_, i) => ({ id: `a${i}` }));
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue({
      posts: firstPage,
      total: 200,
    } as unknown as PostSearchResult);

    const { result, rerender } = renderHook((props: PostFilters) => usePosts(props), {
      initialProps: { ...defaultFilters, limit: 50 },
    });
    await waitFor(() => expect(result.current.posts).toHaveLength(50));

    rerender({ ...defaultFilters, platform: 'instagram', limit: 50 });

    await waitFor(() =>
      expect(window.electronAPI.getPosts).toHaveBeenLastCalledWith(
        expect.objectContaining({ platform: 'instagram', offset: 0 }),
      ),
    );
  });
});

// ─── 9. Reconciliation by id on reloads ──────────────────────────────────────

describe('usePosts — reload reconciliation', () => {
  it('reuses the previous object identity for unchanged rows', async () => {
    const original = [
      { id: 'p1', aiTags: ['a'], media: [{ position: 0, url: 'u' }] },
      { id: 'p2', aiTags: [], media: [] },
    ];
    vi.mocked(window.electronAPI.getPosts).mockResolvedValueOnce({
      posts: original,
      total: 2,
    } as unknown as PostSearchResult);

    const { result } = renderHook(() => usePosts(defaultFilters));
    await waitFor(() => expect(result.current.posts).toHaveLength(2));

    // Same content, fresh identities (a new IPC payload) + one real change.
    vi.mocked(window.electronAPI.getPosts).mockResolvedValueOnce({
      posts: [
        { id: 'p1', aiTags: ['a'], media: [{ position: 0, url: 'u' }] },
        { id: 'p2', aiTags: ['new'], media: [] },
      ],
      total: 2,
    } as unknown as PostSearchResult);
    await act(async () => {
      result.current.reload();
    });

    await waitFor(() => expect(result.current.posts[1].aiTags).toEqual(['new']));
    expect(result.current.posts[0]).toBe(original[0]);
    expect(result.current.posts[1]).not.toBe(original[1]);
  });
});

// ─── 10. Single-post patch on terminal job events ────────────────────────────

describe('usePosts — single-post patch on done events', () => {
  it('patches just the completed post via getPostsByIds, without refetching the list', async () => {
    let downloadCb: ProgressCallback | undefined;
    vi.mocked(window.electronAPI.onDownloadProgress).mockImplementation((cb) => {
      downloadCb = cb;
      return () => {};
    });
    const original = [
      { id: 'p1', videoPath: null },
      { id: 'p2', videoPath: null },
    ];
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue({
      posts: original,
      total: 2,
    } as unknown as PostSearchResult);

    const { result } = renderHook(() => usePosts(defaultFilters));
    await waitFor(() => expect(result.current.posts).toHaveLength(2));
    const callsBefore = vi.mocked(window.electronAPI.getPosts).mock.calls.length;

    vi.mocked(window.electronAPI.getPostsByIds).mockResolvedValue([
      { id: 'p1', videoPath: '/x.mp4' },
    ] as unknown as Shelfy.Post[]);
    await act(async () => {
      downloadCb!({ status: 'done', postId: 'p1' });
    });

    await waitFor(() => expect(result.current.posts[0].videoPath).toBe('/x.mp4'));
    expect(window.electronAPI.getPostsByIds).toHaveBeenCalledWith(['p1']);
    // The untouched row keeps its identity; no full list refetch happened.
    expect(result.current.posts[1]).toBe(original[1]);
    expect(vi.mocked(window.electronAPI.getPosts).mock.calls.length).toBe(callsBefore);
  });

  it('falls back to a coalesced reload when a downloadStatus filter is active', async () => {
    vi.useFakeTimers();
    let downloadCb: ProgressCallback | undefined;
    vi.mocked(window.electronAPI.onDownloadProgress).mockImplementation((cb) => {
      downloadCb = cb;
      return () => {};
    });
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue({
      posts: [],
      total: 0,
    } as unknown as PostSearchResult);

    renderHook(() => usePosts({ ...defaultFilters, downloadStatus: 'linkonly' }));
    await act(async () => {
      await Promise.resolve();
    });
    const callsBefore = vi.mocked(window.electronAPI.getPosts).mock.calls.length;

    act(() => {
      downloadCb!({ status: 'done', postId: 'p1' });
    });
    await act(async () => {
      vi.advanceTimersByTime(400);
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(window.electronAPI.getPostsByIds).not.toHaveBeenCalled();
    expect(vi.mocked(window.electronAPI.getPosts).mock.calls.length).toBeGreaterThan(callsBefore);

    vi.runAllTimers();
    vi.useRealTimers();
  });
});

// ─── 11. Inactive view defers live reloads ───────────────────────────────────

describe('usePosts — inactive view', () => {
  it('accumulates a dirty flag while inactive and reloads once on reactivation', async () => {
    vi.useFakeTimers();
    let newPostsCb: NewPostsCallback | undefined;
    vi.mocked(window.electronAPI.onNewPosts).mockImplementation((cb) => {
      newPostsCb = cb as NewPostsCallback;
      return () => {};
    });
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue({
      posts: [],
      total: 0,
    } as unknown as PostSearchResult);

    const { rerender } = renderHook(
      (props: { active: boolean }) => usePosts(defaultFilters, props),
      {
        initialProps: { active: false },
      },
    );
    await act(async () => {
      await Promise.resolve();
    });
    const callsBefore = vi.mocked(window.electronAPI.getPosts).mock.calls.length;

    // Events while hidden: nothing fires, even past the maxWait window.
    act(() => {
      newPostsCb!();
      newPostsCb!();
    });
    act(() => {
      vi.advanceTimersByTime(5000);
    });
    expect(vi.mocked(window.electronAPI.getPosts).mock.calls.length).toBe(callsBefore);

    // Reactivation → exactly one catch-up reload.
    await act(async () => {
      rerender({ active: true });
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(vi.mocked(window.electronAPI.getPosts).mock.calls.length).toBe(callsBefore + 1);

    vi.runAllTimers();
    vi.useRealTimers();
  });
});
