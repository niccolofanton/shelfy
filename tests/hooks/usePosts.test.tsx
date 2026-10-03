import { describe, it, expect, vi, type Mock } from 'vitest';
import type React from 'react';
import { renderHook, act, waitFor } from '@testing-library/react';
import {
  usePosts,
  windowPosts,
  MAX_LOADED_POSTS,
  type PostFilters,
} from '../../src/hooks/usePosts';
import { ShelfyProvider } from '../../src/api/ShelfyProvider';
import { desktopCapabilities } from '../../src/api/electronClient';
import type { PostPage, ShelfyClient } from '../../src/api/ShelfyClient';
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

// ─── 11. Cursor paging through a ShelfyClient (the web client's paging) ───────

describe('usePosts — cursor paging through a ShelfyClient', () => {
  // A client whose pages are keyset-style: the cursor is opaque and a later
  // page carries no total.
  function cursorClient(pages: PostPage[]): ShelfyClient & { listPosts: Mock } {
    const listPosts = vi.fn();
    for (const p of pages) listPosts.mockResolvedValueOnce(p);
    return {
      capabilities: desktopCapabilities('darwin'),
      media: { file: (r) => r ?? null, tile: (r) => r ?? null, isStored: () => false },
      listPosts,
      getPostsByIds: vi.fn().mockResolvedValue([]),
      getStats: vi.fn(),
      listCollections: vi.fn().mockResolvedValue([]),
      updatePost: vi.fn(),
      createCollection: vi.fn(),
      updateCollection: vi.fn(),
      deleteCollection: vi.fn(),
      addPostsToCollections: vi.fn(),
      removePostFromCollection: vi.fn(),
      openExternal: vi.fn(),
      on: vi.fn(() => () => {}),
      reportError: vi.fn(),
    };
  }
  const ids = (prefix: string, n: number): Shelfy.Post[] =>
    Array.from({ length: n }, (_, i) => ({ id: `${prefix}${i}` }) as Shelfy.Post);
  function providerOf(client: ShelfyClient) {
    return function Provider({ children }: { children: React.ReactNode }) {
      return <ShelfyProvider client={client}>{children}</ShelfyProvider>;
    };
  }

  it('appends from the cursor and keeps the first page total', async () => {
    const client = cursorClient([
      { posts: ids('a', 50), total: 120, nextCursor: 'c1' },
      { posts: ids('b', 50), nextCursor: 'c2' },
    ]);
    const { result, rerender } = renderHook((props: PostFilters) => usePosts(props), {
      initialProps: { ...defaultFilters, limit: 50 },
      wrapper: providerOf(client),
    });
    await waitFor(() => expect(result.current.posts).toHaveLength(50));
    expect(client.listPosts).toHaveBeenLastCalledWith(
      expect.objectContaining({ platform: undefined }),
      expect.objectContaining({ limit: 50, cursor: null }),
    );

    rerender({ ...defaultFilters, limit: 100 });
    await waitFor(() => expect(result.current.posts).toHaveLength(100));
    expect(client.listPosts).toHaveBeenLastCalledWith(
      expect.anything(),
      expect.objectContaining({ limit: 50, cursor: 'c1' }),
    );
    // The append came without a total: the first page's stays.
    expect(result.current.total).toBe(120);
  });

  it('sizes an append from the rows served, not from the rendered list', async () => {
    // The rendered list can be shorter than the rows served (an append drops
    // rows it already shows) or lag behind them (it renders in a transition).
    // Sized from it, an append would not match its cursor: it overshoots the
    // window, and the next scroll finds nothing left to fetch.
    const client = cursorClient([
      { posts: ids('a', 50), total: 1000, nextCursor: 'c1' },
      { posts: [...ids('a', 50).slice(40), ...ids('b', 240)], nextCursor: 'c2' },
      { posts: ids('c', 250), nextCursor: 'c3' },
    ]);
    const { result, rerender } = renderHook((props: PostFilters) => usePosts(props), {
      initialProps: { ...defaultFilters, limit: 50 },
      wrapper: providerOf(client),
    });
    await waitFor(() => expect(result.current.loading).toBe(false));
    rerender({ ...defaultFilters, limit: 300 });
    await waitFor(() => expect(client.listPosts).toHaveBeenCalledTimes(2));
    expect(client.listPosts.mock.calls[1][1]).toMatchObject({ limit: 250, cursor: 'c1' });
    await waitFor(() => expect(result.current.loading).toBe(false));
    await waitFor(() => expect(result.current.posts).toHaveLength(290));
    rerender({ ...defaultFilters, limit: 550 });
    await waitFor(() => expect(client.listPosts).toHaveBeenCalledTimes(3));
    expect(client.listPosts.mock.calls[2][1]).toMatchObject({ limit: 250, cursor: 'c2' });
    await waitFor(() => expect(result.current.posts).toHaveLength(540));
  });

  it('stops appending after the last page', async () => {
    const client = cursorClient([{ posts: ids('a', 10), total: 10, nextCursor: null }]);
    const { result, rerender } = renderHook((props: PostFilters) => usePosts(props), {
      initialProps: { ...defaultFilters, limit: 50 },
      wrapper: providerOf(client),
    });
    await waitFor(() => expect(result.current.posts).toHaveLength(10));
    rerender({ ...defaultFilters, limit: 300 });
    await act(async () => {
      await Promise.resolve();
    });
    expect(client.listPosts).toHaveBeenCalledTimes(1);
    expect(result.current.loading).toBe(false);
  });

  it('cancels a superseded request through its signal', async () => {
    let firstSignal: AbortSignal | undefined;
    const client = cursorClient([]);
    client.listPosts
      .mockImplementationOnce((_query: unknown, page: { signal?: AbortSignal }) => {
        firstSignal = page.signal;
        return new Promise(() => {});
      })
      .mockResolvedValueOnce({ posts: ids('b', 3), total: 3, nextCursor: null });
    const { result, rerender } = renderHook((props: PostFilters) => usePosts(props), {
      initialProps: { ...defaultFilters, limit: 50 },
      wrapper: providerOf(client),
    });
    rerender({ ...defaultFilters, platform: 'twitter', limit: 50 });
    await waitFor(() => expect(result.current.posts).toHaveLength(3));
    expect(firstSignal?.aborted).toBe(true);
  });
});

// ─── 12. The web stream's events and errors ───────────────────────────────────

describe('usePosts — web stream and API errors', () => {
  // A client that keeps its subscribers, so a test can push events.
  function liveClient() {
    const listeners = new Map<string, ((event: unknown) => void)[]>();
    const client: ShelfyClient & { listPosts: Mock } = {
      capabilities: desktopCapabilities('darwin'),
      media: { file: (r) => r ?? null, tile: (r) => r ?? null, isStored: () => false },
      listPosts: vi.fn().mockResolvedValue({ posts: [], total: 0, nextCursor: null }),
      getPostsByIds: vi.fn().mockResolvedValue([]),
      getStats: vi.fn(),
      listCollections: vi.fn().mockResolvedValue([]),
      updatePost: vi.fn(),
      createCollection: vi.fn(),
      updateCollection: vi.fn(),
      deleteCollection: vi.fn(),
      addPostsToCollections: vi.fn(),
      removePostFromCollection: vi.fn(),
      openExternal: vi.fn(),
      on: vi.fn((type: string, listener: (event: unknown) => void) => {
        listeners.set(type, [...(listeners.get(type) ?? []), listener]);
        return () => {};
      }) as ShelfyClient['on'],
      reportError: vi.fn(),
    };
    const emit = (type: string, event: unknown = { type }) =>
      (listeners.get(type) ?? []).forEach((listener) => listener(event));
    return { client, emit };
  }
  const wrapperOf = (client: ShelfyClient) =>
    function Provider({ children }: { children: React.ReactNode }) {
      return <ShelfyProvider client={client}>{children}</ShelfyProvider>;
    };

  it('reloads after a resync, through the coalesced reload', async () => {
    vi.useFakeTimers();
    const { client, emit } = liveClient();
    renderHook(() => usePosts(defaultFilters), { wrapper: wrapperOf(client) });
    await act(async () => {
      await Promise.resolve();
    });
    const callsBefore = client.listPosts.mock.calls.length;

    act(() => emit('resync'));
    act(() => {
      vi.advanceTimersByTime(399);
    });
    expect(client.listPosts.mock.calls.length).toBe(callsBefore);
    await act(async () => {
      vi.advanceTimersByTime(1);
      await Promise.resolve();
    });
    expect(client.listPosts.mock.calls.length).toBe(callsBefore + 1);

    vi.runAllTimers();
    vi.useRealTimers();
  });

  it("shows an API failure's message from its problem code", async () => {
    const { client } = liveClient();
    client.listPosts.mockRejectedValue(
      Object.assign(new Error('unavailable: database busy'), { code: 'unavailable' }),
    );
    const { result } = renderHook(() => usePosts(defaultFilters), { wrapper: wrapperOf(client) });
    await waitFor(() =>
      expect(result.current.error).toBe('Il server è occupato. Riprova tra poco.'),
    );
  });

  it('keeps the message of an error with no known code', async () => {
    const { client } = liveClient();
    client.listPosts.mockRejectedValue(
      Object.assign(new Error('SQLITE_BUSY: locked'), { code: 'SQLITE_BUSY' }),
    );
    const { result } = renderHook(() => usePosts(defaultFilters), { wrapper: wrapperOf(client) });
    await waitFor(() => expect(result.current.error).toBe('SQLITE_BUSY: locked'));
  });
});

// ─── 13. Windowing (plan §2.19 "the loaded list is windowed: ±1,000 around
// the viewport") ────────────────────────────────────────────────────────────

describe('windowPosts', () => {
  const post = (id: string) => ({ id }) as Shelfy.Post;
  const ids = (n: number) => Array.from({ length: n }, (_, i) => post(`p${i}`));

  it('leaves a list within budget untouched (same reference)', () => {
    const list = ids(500);
    expect(windowPosts(list, 2000, 6)).toBe(list);
  });

  it('trims the front, row-aligned to cols, down to at most max', () => {
    const list = ids(2010); // 10 over budget
    const out = windowPosts(list, 2000, 6);
    // ceil(10/6)*6 = 12 dropped, not 10: a partial row would reshuffle which
    // posts share it.
    expect(out).toHaveLength(1998);
    expect(out[0].id).toBe('p12');
    // The tail (what a forward scroll's viewport sits near) is untouched.
    expect(out.at(-1)?.id).toBe('p2009');
  });

  it('never keeps more than max even when the whole list must go', () => {
    expect(windowPosts(ids(3), 2, 10)).toEqual([]);
  });

  it('tolerates a non-positive or fractional column count', () => {
    const list = ids(2005);
    expect(() => windowPosts(list, 2000, 0)).not.toThrow();
    expect(windowPosts(list, 2000, 0).length).toBeLessThanOrEqual(2000);
  });
});

describe('usePosts — windowing keeps memory near the viewport', () => {
  // A keyset-style backend over `total` posts, serving whatever `limit`
  // (minus what's already fetched, via `cursor`) asks for — the same shape a
  // real 20k-post library's `GET /posts` gives Gallery's infinite scroll.
  function pagingClient(total: number): ShelfyClient & { listPosts: Mock } {
    const listPosts = vi.fn(
      async (_query: unknown, page: { limit: number; cursor?: string | null }) => {
        const start = page.cursor ? Number(page.cursor) : 0;
        const n = Math.max(0, Math.min(page.limit, total - start));
        const posts = Array.from({ length: n }, (_, i) => ({ id: `p${start + i}` }) as Shelfy.Post);
        const end = start + posts.length;
        return { posts, total, nextCursor: end < total ? String(end) : null };
      },
    );
    return {
      capabilities: desktopCapabilities('darwin'),
      media: { file: (r) => r ?? null, tile: (r) => r ?? null, isStored: () => false },
      listPosts,
      getPostsByIds: vi.fn().mockResolvedValue([]),
      getStats: vi.fn(),
      listCollections: vi.fn().mockResolvedValue([]),
      updatePost: vi.fn(),
      createCollection: vi.fn(),
      updateCollection: vi.fn(),
      deleteCollection: vi.fn(),
      addPostsToCollections: vi.fn(),
      removePostFromCollection: vi.fn(),
      openExternal: vi.fn(),
      on: vi.fn(() => () => {}),
      reportError: vi.fn(),
    };
  }
  function providerOf(client: ShelfyClient) {
    return function Provider({ children }: { children: React.ReactNode }) {
      return <ShelfyProvider client={client}>{children}</ShelfyProvider>;
    };
  }

  it('never holds more than MAX_LOADED_POSTS scrolling through a 20k-post library', async () => {
    const client = pagingClient(20_000);
    const { result, rerender } = renderHook((props: PostFilters) => usePosts(props), {
      initialProps: { ...defaultFilters, limit: 250 },
      wrapper: providerOf(client),
    });
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.posts.length).toBeGreaterThan(0);

    for (let limit = 500; limit <= 16_000; limit += 250) {
      rerender({ ...defaultFilters, limit });
      await waitFor(() => expect(result.current.loading).toBe(false));
      expect(result.current.posts.length).toBeLessThanOrEqual(MAX_LOADED_POSTS);
    }
    // The viewport — the tail, on this forward-only infinite scroll — is
    // exactly what the backend last served, never trimmed away.
    expect(result.current.posts.at(-1)?.id).toBe('p15999');
  });
});
