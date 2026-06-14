import { describe, it, expect, vi } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import { useAiTags, type UseAiTagsResult } from '../../src/hooks/useAiTags';
import type { PostSearchResult } from '../../types/electron-api';

type MergeTagsResult = Awaited<ReturnType<UseAiTagsResult['renameTag']>>;
type QueuedResult = Awaited<ReturnType<UseAiTagsResult['analyzeMissing']>>;

// ─── 1. Initial parallel load ─────────────────────────────────────────────────

describe('useAiTags — initial load', () => {
  it('loads overview/tagStats/clusters/entityStats/health/mergeSuggestions in parallel on mount', async () => {
    vi.mocked(window.electronAPI.getAiOverview).mockResolvedValue({
      total: 10,
      analyzed: 4,
    } as unknown as Shelfy.AiOverview);
    vi.mocked(window.electronAPI.getTagStats).mockResolvedValue([
      { tag: 'a', count: 3 },
    ] as unknown as Shelfy.Tag[]);
    vi.mocked(window.electronAPI.getTagClusters).mockResolvedValue([
      { id: 'c1' },
    ] as unknown as Shelfy.TagCluster[]);
    vi.mocked(window.electronAPI.getEntityStats).mockResolvedValue([
      { entity: 'e1' },
    ] as unknown as Shelfy.Entity[]);
    vi.mocked(window.electronAPI.getTagHealth).mockResolvedValue({
      orphanTags: [],
      rareTags: 2,
    } as unknown as Shelfy.TagHealth);
    vi.mocked(window.electronAPI.getTagMergeSuggestions).mockResolvedValue([
      { from: 'x', to: 'y' },
    ] as unknown as Shelfy.TagMergeSuggestion[]);

    const { result } = renderHook(() => useAiTags());

    await waitFor(() => expect(result.current.loading).toBe(false));

    expect(window.electronAPI.getAiOverview).toHaveBeenCalled();
    expect(window.electronAPI.getTagStats).toHaveBeenCalled();
    expect(window.electronAPI.getTagClusters).toHaveBeenCalled();
    expect(window.electronAPI.getEntityStats).toHaveBeenCalled();
    expect(window.electronAPI.getTagHealth).toHaveBeenCalled();
    expect(window.electronAPI.getTagMergeSuggestions).toHaveBeenCalled();

    expect(result.current.overview).toEqual({ total: 10, analyzed: 4 });
    expect(result.current.tagStats).toEqual([{ tag: 'a', count: 3 }]);
    expect(result.current.clusters).toEqual([{ id: 'c1' }]);
    expect(result.current.entityStats).toEqual([{ entity: 'e1' }]);
    expect(result.current.health).toEqual({ orphanTags: [], rareTags: 2 });
    expect(result.current.mergeSuggestions).toEqual([{ from: 'x', to: 'y' }]);
    expect(result.current.error).toBeNull();
  });

  it('transitions loading from true to false', async () => {
    const { result } = renderHook(() => useAiTags());

    expect(result.current.loading).toBe(true);
    await waitFor(() => expect(result.current.loading).toBe(false));
  });

  it('defaults array fields to [] and object fields to null when responses are non-array/falsy', async () => {
    vi.mocked(window.electronAPI.getAiOverview).mockResolvedValue(
      null as unknown as Shelfy.AiOverview,
    );
    vi.mocked(window.electronAPI.getTagStats).mockResolvedValue(null as unknown as Shelfy.Tag[]);
    vi.mocked(window.electronAPI.getTagClusters).mockResolvedValue(
      undefined as unknown as Shelfy.TagCluster[],
    );
    vi.mocked(window.electronAPI.getEntityStats).mockResolvedValue(
      null as unknown as Shelfy.Entity[],
    );
    vi.mocked(window.electronAPI.getTagHealth).mockResolvedValue(
      null as unknown as Shelfy.TagHealth,
    );
    vi.mocked(window.electronAPI.getTagMergeSuggestions).mockResolvedValue(
      null as unknown as Shelfy.TagMergeSuggestion[],
    );

    const { result } = renderHook(() => useAiTags());
    await waitFor(() => expect(result.current.loading).toBe(false));

    expect(result.current.overview).toBeNull();
    expect(result.current.tagStats).toEqual([]);
    expect(result.current.clusters).toEqual([]);
    expect(result.current.entityStats).toEqual([]);
    expect(result.current.health).toBeNull();
    expect(result.current.mergeSuggestions).toEqual([]);
  });
});

// ─── 2. Error handling ────────────────────────────────────────────────────────

describe('useAiTags — error handling', () => {
  it('sets error to the message when a load call rejects', async () => {
    vi.mocked(window.electronAPI.getAiOverview).mockRejectedValue(new Error('boom'));

    const { result } = renderHook(() => useAiTags());

    await waitFor(() => expect(result.current.error).not.toBeNull());
    expect(result.current.error).toBe('boom');
    expect(result.current.loading).toBe(false);
  });

  it('sets a fallback error message when rejection has no message', async () => {
    vi.mocked(window.electronAPI.getAiOverview).mockRejectedValue(null);

    const { result } = renderHook(() => useAiTags());

    await waitFor(() => expect(result.current.error).not.toBeNull());
    expect(result.current.error).toBe('Impossibile caricare i dati AI');
  });
});

// ─── 3. refresh() ─────────────────────────────────────────────────────────────

describe('useAiTags — refresh()', () => {
  it('reloads all aggregate data', async () => {
    const { result } = renderHook(() => useAiTags());
    await waitFor(() => expect(result.current.loading).toBe(false));

    const before = vi.mocked(window.electronAPI.getAiOverview).mock.calls.length;

    await act(async () => {
      await result.current.refresh();
    });

    expect(vi.mocked(window.electronAPI.getAiOverview).mock.calls.length).toBeGreaterThan(before);
  });
});

// ─── 4. Mutations refresh afterwards ─────────────────────────────────────────

describe('useAiTags — renameTag', () => {
  it('calls electronAPI.renameTag then reloads', async () => {
    vi.mocked(window.electronAPI.renameTag).mockResolvedValue({ updated: 3 });

    const { result } = renderHook(() => useAiTags());
    await waitFor(() => expect(result.current.loading).toBe(false));

    const before = vi.mocked(window.electronAPI.getAiOverview).mock.calls.length;

    let res: MergeTagsResult | undefined;
    await act(async () => {
      res = await result.current.renameTag('old', 'new');
    });

    expect(window.electronAPI.renameTag).toHaveBeenCalledWith('old', 'new');
    expect(res).toEqual({ updated: 3 });
    expect(vi.mocked(window.electronAPI.getAiOverview).mock.calls.length).toBeGreaterThan(before);
  });
});

describe('useAiTags — mergeTags', () => {
  it('calls electronAPI.mergeTags then reloads', async () => {
    vi.mocked(window.electronAPI.mergeTags).mockResolvedValue({ updated: 5 });

    const { result } = renderHook(() => useAiTags());
    await waitFor(() => expect(result.current.loading).toBe(false));

    const before = vi.mocked(window.electronAPI.getAiOverview).mock.calls.length;

    let res: MergeTagsResult | undefined;
    await act(async () => {
      res = await result.current.mergeTags(['a', 'b'], 'c');
    });

    expect(window.electronAPI.mergeTags).toHaveBeenCalledWith(['a', 'b'], 'c');
    expect(res).toEqual({ updated: 5 });
    expect(vi.mocked(window.electronAPI.getAiOverview).mock.calls.length).toBeGreaterThan(before);
  });
});

describe('useAiTags — analyzeMissing', () => {
  it('calls electronAPI.analyzeMissing and does NOT reload', async () => {
    vi.mocked(window.electronAPI.analyzeMissing).mockResolvedValue({ queued: 7 });

    const { result } = renderHook(() => useAiTags());
    await waitFor(() => expect(result.current.loading).toBe(false));

    const before = vi.mocked(window.electronAPI.getAiOverview).mock.calls.length;

    let res: QueuedResult | undefined;
    await act(async () => {
      res = await result.current.analyzeMissing();
    });

    expect(window.electronAPI.analyzeMissing).toHaveBeenCalled();
    expect(res).toEqual({ queued: 7 });
    expect(vi.mocked(window.electronAPI.getAiOverview).mock.calls.length).toBe(before);
  });
});

// ─── 5. Read helpers ──────────────────────────────────────────────────────────

describe('useAiTags — read helpers', () => {
  it('getPostIdsByTags delegates to electronAPI with tags and mode', async () => {
    vi.mocked(window.electronAPI.getPostIdsByTags).mockResolvedValue(['p1', 'p2']);

    const { result } = renderHook(() => useAiTags());
    await waitFor(() => expect(result.current.loading).toBe(false));

    let ids: string[] | undefined;
    await act(async () => {
      ids = await result.current.getPostIdsByTags(['a'], 'and');
    });

    expect(window.electronAPI.getPostIdsByTags).toHaveBeenCalledWith(['a'], 'and');
    expect(ids).toEqual(['p1', 'p2']);
  });

  it('getTagCooccurrence delegates to electronAPI with tag and limit', async () => {
    vi.mocked(window.electronAPI.getTagCooccurrence).mockResolvedValue([{ tag: 'b', count: 2 }]);

    const { result } = renderHook(() => useAiTags());
    await waitFor(() => expect(result.current.loading).toBe(false));

    let res: Shelfy.TagCount[] | undefined;
    await act(async () => {
      res = await result.current.getTagCooccurrence('a', 5);
    });

    expect(window.electronAPI.getTagCooccurrence).toHaveBeenCalledWith('a', 5);
    expect(res).toEqual([{ tag: 'b', count: 2 }]);
  });

  it('fetchPosts calls getPosts with the filters and returns the posts payload', async () => {
    const payload = { posts: [{ id: '1' }], total: 1 };
    vi.mocked(window.electronAPI.getPosts).mockResolvedValue(
      payload as unknown as PostSearchResult,
    );

    const { result } = renderHook(() => useAiTags());
    await waitFor(() => expect(result.current.loading).toBe(false));

    const filters = { tag: 'cats', limit: 20 };
    let res: PostSearchResult | undefined;
    await act(async () => {
      res = await result.current.fetchPosts(filters);
    });

    expect(window.electronAPI.getPosts).toHaveBeenCalledWith(filters);
    expect(res).toEqual(payload);
  });
});
