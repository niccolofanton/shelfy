import React from 'react';
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen, fireEvent, waitFor, cleanup, act } from '@testing-library/react';
import AiSearch from '@ui/views/AiSearch';
import { ShelfyProvider } from '@ui/api/ShelfyProvider';
import { getElectronClient } from '@ui/api/electronClient';
import type { ChatSearchResult } from '../../../types/electron-api';
import type { AiSearchApi } from '@ui/api/ai';
import { WEB_CAPABILITIES } from '../../src/api/httpClient';
const callbacks = new Map<AiSearchApi, (v: unknown) => void>();
function api() {
  const value = {
    byTags: vi.fn(async () => ({ posts: [], total: 2 })),
    hybrid: vi.fn(async () => ({ posts: [], total: 2 })),
    byText: vi.fn(async () => ({ posts: [], total: 2 })),
    chat: vi.fn(
      async (..._args: Parameters<AiSearchApi['chat']>): Promise<ChatSearchResult> => ({
        reply: '',
        replyCode: 'suggestions' as const,
        tagsToAdd: ['lamp'],
        tagsToRemove: [],
        keywordsToAdd: [],
        tagGroups: { broad: [], specific: ['lamp'], keywords: [] },
        modelUsed: false,
      }),
    ),
    cancelChat: vi.fn(async () => ({ ok: true as const })),
    onToken: vi.fn((cb: (v: unknown) => void) => {
      callbacks.set(value, cb);
      return () => callbacks.delete(value);
    }),
    onResultsStale: vi.fn(() => () => {}),
    getProviders: vi.fn(async () => []),
    selectProvider: vi.fn(async () => []),
    getModelStatus: vi.fn(async () => ({ ready: false })),
    onModelProgress: vi.fn(() => () => {}),
  };
  return value;
}
const client = (search: AiSearchApi) => ({
  ...getElectronClient(),
  capabilities: { ...WEB_CAPABILITIES, ai: true, aiChat: true },
  ai: { search },
});
function send(text = 'lamp') {
  fireEvent.change(screen.getByTestId('chat-input'), { target: { value: text } });
  fireEvent.click(screen.getByTestId('chat-send-btn'));
}
afterEach(cleanup);
describe('web conversation and filter state', () => {
  it('localizes fallback, scopes the chat and search, replaces filters, toggles and clears', async () => {
    const search = api();
    render(
      <ShelfyProvider client={client(search)}>
        <AiSearch />
      </ShelfyProvider>,
    );
    expect(screen.queryByTestId('model-download-btn')).toBeNull();
    expect(screen.queryByTestId('stt-download-btn')).toBeNull();
    expect(screen.getByTestId('chat-mic-btn')).toBeDisabled();
    fireEvent.click(screen.getByTestId('source-web'));
    send();
    await screen.findByTestId('chat-message-assistant');
    expect(search.chat).toHaveBeenCalledWith([{ role: 'user', content: 'lamp' }], [], {
      source: 'web',
    });
    expect(screen.getByTestId('chat-message-assistant')).toHaveTextContent(
      'Ecco i filtri suggeriti dal tuo archivio.',
    );
    await waitFor(() => expect(search.byTags).toHaveBeenCalledWith(['lamp'], 'or', 60, 0, 'web'));
    fireEvent.click(screen.getByTestId('proposed-tag-chip').querySelector('button')!);
    expect(screen.queryByTestId('active-tag-chip')).toBeNull();
    fireEvent.click(screen.getByTestId('apply-message-tags-btn'));
    await waitFor(() => expect(screen.getByTestId('active-tag-chip')).toHaveTextContent('lamp'));
    search.chat.mockResolvedValueOnce({
      reply: '',
      replyCode: 'suggestions',
      tagsToAdd: ['glass'],
      tagsToRemove: [],
      keywordsToAdd: [],
      tagGroups: { broad: [], specific: ['glass'], keywords: [] },
      modelUsed: false,
    });
    send('glass');
    await waitFor(() => expect(screen.getByTestId('active-tag-chip')).toHaveTextContent('glass'));
    expect(screen.getAllByTestId('active-tag-chip')).toHaveLength(1);
    fireEvent.click(screen.getByTestId('chat-reset-btn'));
    expect(screen.queryByTestId('chat-message-assistant')).toBeNull();
    expect(search.cancelChat).toHaveBeenCalled();
  });
  it('supports string run ids, ignores stale tokens and stops before a late result', async () => {
    const search = api();
    let finish!: (v: Awaited<ReturnType<typeof search.chat>>) => void;
    search.chat.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    render(
      <ShelfyProvider client={client(search)}>
        <AiSearch />
      </ShelfyProvider>,
    );
    send();
    act(() => {
      callbacks.get(search)?.({ start: true, runId: 'server-ulid' });
      callbacks.get(search)?.({ runId: 'old', token: 'OLD PRIVATE' });
      callbacks.get(search)?.({ runId: 'server-ulid', token: 'Live answer' });
    });
    expect(screen.getByTestId('chat-message-streaming')).toHaveTextContent('Live answer');
    expect(screen.queryByText('OLD PRIVATE')).toBeNull();
    fireEvent.click(screen.getByTestId('chat-stop-btn'));
    await waitFor(() => expect(search.cancelChat).toHaveBeenCalled());
    await act(async () =>
      finish({
        reply: 'late answer',
        tagsToAdd: ['late'],
        tagsToRemove: [],
        keywordsToAdd: [],
        tagGroups: { broad: [], specific: [], keywords: [] },
        modelUsed: true,
      }),
    );
    expect(screen.queryByText('late answer')).toBeNull();
    expect(screen.queryByTestId('active-tag-chip')).toBeNull();
  });
  it('retries explicitly without duplicating the failed turn and clears private history on account change', async () => {
    const first = api();
    first.chat.mockRejectedValueOnce(new Error('PRIVATE DETAILS'));
    const a = client(first);
    const rendered = render(
      <ShelfyProvider client={a}>
        <AiSearch />
      </ShelfyProvider>,
    );
    send('private question');
    await screen.findByTestId('chat-retry-btn');
    expect(screen.queryByText('PRIVATE DETAILS')).toBeNull();
    expect(first.chat).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByTestId('chat-retry-btn'));
    await waitFor(() => expect(first.chat).toHaveBeenCalledTimes(2));
    expect(first.chat.mock.calls[1][0]).toEqual([{ role: 'user', content: 'private question' }]);
    const second = api();
    await act(async () =>
      rendered.rerender(
        <ShelfyProvider client={client(second)}>
          <AiSearch />
        </ShelfyProvider>,
      ),
    );
    expect(screen.queryByText('private question')).toBeNull();
    expect(screen.queryByTestId('active-tag-chip')).toBeNull();
    expect(first.cancelChat).toHaveBeenCalled();
    expect(callbacks.has(first)).toBe(false);
  });
});
