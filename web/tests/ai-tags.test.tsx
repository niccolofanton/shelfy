import React from 'react';
import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent, waitFor, renderHook, act } from '@testing-library/react';
import AiTags from '../../src/views/AiTags';
import { useAiTags } from '../../src/hooks/useAiTags';
import { ShelfyProvider } from '../../src/api/ShelfyProvider';
import { createTagsApi } from '../src/api/ai/tags';
import { createHttpClient, WEB_CAPABILITIES } from '../src/api/httpClient';
import { ApiError, type Http } from '../src/api/http';
import type { EventStream } from '../src/api/events';
import { apiPost } from './fixtures';

const overview = {
  total: 301,
  analyzed: 240,
  unanalyzed: 61,
  uniqueTags: 2,
  taggedPosts: 240,
  byCategory: [],
  byContentType: [],
  languages: [],
};
function harness() {
  const get = vi.fn(async (path: string, _query?: URLSearchParams) => {
    if (path.endsWith('/overview')) return overview;
    if (path.endsWith('/health'))
      return { orphanTags: [], rareTags: 0, unanalyzedPosts: 61, untaggedPosts: 0 };
    if (path.endsWith('/tags'))
      return { items: [{ tag: 'Lamp', count: 301, lastUsed: null, categories: [] }] };
    if (path.endsWith('/posts'))
      return { items: [apiPost({ coverUrl: null })], total: 301, nextCursor: null };
    return { items: [] };
  });
  const send = vi.fn(
    async (_method: string, _path: string, _body?: unknown) =>
      new Response(JSON.stringify({ accepted: 4, rewritten: 10 })),
  );
  const on = vi.fn((_event: string, _callback: unknown) => () => {});
  const http = { get, send } as unknown as Http;
  const baseClient = createHttpClient(http, {
    events: { on } as unknown as EventStream,
    capabilities: { ...WEB_CAPABILITIES, ai: true, aiTags: true },
  });
  const client = { ...baseClient, ai: { tags: createTagsApi(http) } };
  const wrapper = ({ children }: { children: React.ReactNode }) => (
    <ShelfyProvider client={client}>{children}</ShelfyProvider>
  );
  return { get, send, client, wrapper, on };
}
describe('web Tag Explorer', () => {
  it('renders tags by tier without exposing unavailable queue/job actions', async () => {
    const { get, wrapper } = harness();
    render(<AiTags />, { wrapper });
    await screen.findByTestId('aitags-tag-index');
    expect(screen.queryByTestId('analyze-missing-btn')).not.toBeInTheDocument();
    expect(screen.queryByText('Rigenera')).not.toBeInTheDocument();
    fireEvent.change(screen.getByTestId('aitags-tier'), { target: { value: 'manual' } });
    await waitFor(() =>
      expect(
        get.mock.calls.some(
          (call) =>
            call[0] === '/api/v1/tags' && (call[1] as URLSearchParams)?.get('tier') === 'manual',
        ),
      ).toBe(true),
    );
  });
  it('creates a folder from the full server filter rather than the preview page', async () => {
    const { client, wrapper } = harness();
    client.createCollection = vi.fn().mockResolvedValue({ id: 8 });
    client.bulkAction = vi.fn().mockResolvedValue({ changed: 301, selected: 301, job: null });
    vi.spyOn(window, 'prompt').mockReturnValue('Lamps');
    render(<AiTags />, { wrapper });
    fireEvent.click(await screen.findByText('Lamp'));
    await screen.findByTestId('aitags-grid');
    fireEvent.click(screen.getByText('Crea collection da questi'));
    await waitFor(() =>
      expect(client.bulkAction).toHaveBeenCalledWith(
        { filter: { tags: ['Lamp'], tagMode: 'or' } },
        'addToCollections',
        { collectionIds: [8] },
      ),
    );
    expect(window.electronAPI.getPostIdsByTags).not.toHaveBeenCalled();
  });
  it('keeps the newest tier data when an older request finishes later', async () => {
    const { client, wrapper } = harness();
    const api = client.ai.tags;
    let finishGeneral!: (tags: Shelfy.Tag[]) => void;
    const general = new Promise<Shelfy.Tag[]>((resolve) => {
      finishGeneral = resolve;
    });
    api.getTagStats = vi.fn(({ tier } = {}) =>
      tier === 'general'
        ? general
        : Promise.resolve([{ tag: 'Manual', count: 1, lastUsed: null, categories: [] }]),
    );
    const { result, rerender } = renderHook(
      ({ tier }: { tier: Shelfy.TagTier }) => useAiTags({ tier }),
      { wrapper, initialProps: { tier: 'general' } },
    );
    rerender({ tier: 'manual' });
    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () =>
      finishGeneral([{ tag: 'Old', count: 2, lastUsed: null, categories: [] }]),
    );
    expect(result.current.tagStats[0].tag).toBe('Manual');
    expect(result.current.loading).toBe(false);
  });
  it('uses atomic alias acceptance and reports authoritative accepted count', async () => {
    const { wrapper, send } = harness();
    const { result } = renderHook(() => useAiTags(), { wrapper });
    await waitFor(() => expect(result.current.loading).toBe(false));
    let accepted = 0;
    await act(async () => {
      accepted = (await result.current.acceptAllAliases()).accepted;
    });
    expect(accepted).toBe(4);
    expect(send).toHaveBeenCalledTimes(1);
    expect(send.mock.calls[0][1]).toBe('/api/v1/tag-aliases/accept-all');
  });
  it('localizes API failure without exposing backend detail and subscribes to library changes', async () => {
    const { get, wrapper, on } = harness();
    get.mockRejectedValue(new ApiError(403, 'forbidden', 'private internal detail'));
    const { result } = renderHook(() => useAiTags(), { wrapper });
    await waitFor(() => expect(result.current.error).toBeTruthy());
    expect(result.current.error).not.toContain('private internal detail');
    expect(on.mock.calls.map((call) => call[0])).toContain('posts.changed');
    expect(on.mock.calls.map((call) => call[0])).toContain('resync');
  });
});
