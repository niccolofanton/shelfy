import Gallery from '../../src/views/Gallery';
import { ShelfyProvider } from '../../src/api/ShelfyProvider';
import type { ShelfyClient } from '../../src/api/ShelfyClient';
import { WEB_CAPABILITIES } from '../src/api/httpClient';
import { toPost, webMedia } from '../src/api/mapping';
import { apiPost } from './fixtures';
import React from 'react';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { I18nProvider } from '../../src/i18n';
import AnalyzeDialog from '../../src/components/ai/AnalyzeDialog';
import WebAiQueue from '../../src/components/ai/WebAiQueue';
import type { AnalyzeResult, WebAiQueueApi, WebAiQueuePage } from '../../src/api/ai/webQueue';
import { createWebAiQueueApi } from '../src/api/ai/queue';
import { createHttpClient } from '../src/api/httpClient';
import type { Http } from '../src/api/http';
import type { EventStream } from '../src/api/events';
import { OWNER } from './authFakes';

const preview: AnalyzeResult = {
  counts: { analyzable: 1, waitingForMedia: 2, alreadyQueued: 3 },
  estimate: { inputTokens: 1200, outputTokens: 500, costUsd: null, etaMs: null },
  queued: false,
  enqueued: 0,
  confirmToken: 'preview',
};
const page: WebAiQueuePage = {
  counts: { unanalyzed: 10, pending: 1, analyzing: 1, done: 20, error: 1 },
  items: [
    { postKey: 'x_1', status: 'pending', attempts: 0, nextAt: null, error: null },
    { postKey: 'x_2', status: 'error', attempts: 1, nextAt: null, error: 'ai_invalid_key' },
  ],
  cursor: null,
  etaMs: null,
  providerState: 'ok',
  paused: false,
};
function fixture() {
  const changed = new Set<() => void>();
  const streams = new Set<(value: { postKey: string; text: string }) => void>();
  const api: WebAiQueueApi = {
    get: vi.fn(async () => structuredClone(page)),
    estimate: vi.fn(async () => preview),
    confirm: vi.fn(async () => ({ ...preview, queued: true, enqueued: 1, confirmToken: null })),
    pause: vi.fn(async () => {}),
    resume: vi.fn(async () => {}),
    cancel: vi.fn(async () => 1),
    retry: vi.fn(async () => 1),
    onChanged(listener) {
      changed.add(listener);
      return () => changed.delete(listener);
    },
    onStream(listener) {
      streams.add(listener);
      return () => streams.delete(listener);
    },
  };
  return { api, streams, changed };
}
function ui(node: React.ReactNode) {
  localStorage.setItem('app:language', 'en');
  return render(<I18nProvider>{node}</I18nProvider>);
}
afterEach(() => localStorage.removeItem('app:language'));

describe('web AI queue', () => {
  it('requires explicit confirmation for one post and keeps failures reviewable', async () => {
    const h = fixture();
    const close = vi.fn();
    const queued = vi.fn();
    vi.mocked(h.api.confirm).mockRejectedValueOnce({ code: 'network' });
    const request = { selector: { keys: ['x_1'] }, mode: 'selected' as const };
    ui(<AnalyzeDialog api={h.api} request={request} onClose={close} onQueued={queued} />);
    await screen.findByTestId('analyze-estimate');
    expect(h.api.confirm).not.toHaveBeenCalled();
    expect(screen.getAllByText('Unavailable')).toHaveLength(2);
    fireEvent.click(screen.getByRole('button', { name: 'Confirm analysis' }));
    await screen.findByRole('alert');
    expect(close).not.toHaveBeenCalled();
    expect(queued).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Confirm analysis' }));
    await waitFor(() => expect(queued).toHaveBeenCalledWith(1));
    expect(close).toHaveBeenCalledOnce();
  });
  it('closing a preview never queues and Italian copy is available', async () => {
    const h = fixture();
    localStorage.setItem('app:language', 'it');
    const close = vi.fn();
    render(
      <I18nProvider>
        <AnalyzeDialog
          api={h.api}
          request={{ selector: { filter: {} }, mode: 'missing' }}
          onClose={close}
          onQueued={vi.fn()}
        />
      </I18nProvider>,
    );
    await screen.findByTestId('analyze-estimate');
    fireEvent.click(screen.getByRole('button', { name: 'Chiudi' }));
    expect(close).toHaveBeenCalled();
    expect(h.api.confirm).not.toHaveBeenCalled();
  });
  it('uses authoritative counts and item actions; stream subscriptions leave with the view', async () => {
    const h = fixture();
    const view = ui(<WebAiQueue api={h.api} active />);
    expect(await screen.findByTestId('queue-counts')).toHaveTextContent('Completed: 20');
    expect(h.streams.size).toBe(1);
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(h.api.cancel).toHaveBeenCalledWith(['x_1']));
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    await waitFor(() => expect(h.api.retry).toHaveBeenCalledWith(['x_2']));
    view.rerender(
      <I18nProvider>
        <WebAiQueue api={h.api} active={false} />
      </I18nProvider>,
    );
    expect(h.streams.size).toBe(0);
    expect(h.changed.size).toBe(0);
  });
  it('reports offline and no-provider states without local-model download controls', async () => {
    const h = fixture();
    vi.mocked(h.api.get).mockResolvedValue({ ...page, providerState: 'offline' });
    ui(<WebAiQueue api={h.api} active />);
    await screen.findByText(/Waiting for the provider/);
    expect(screen.queryByText(/Download.*model/)).toBeNull();
    vi.mocked(h.api.get).mockResolvedValue({ ...page, providerState: null });
    act(() => {
      for (const listener of h.changed) listener();
    });
    await screen.findByText(/No AI provider available/);
    expect(screen.getByRole('button', { name: 'Analyze missing' })).toBeDisabled();
  });
});

it('maps selectors, retries confirms with the same idempotency key and gates the API', async () => {
  const send = vi.fn().mockResolvedValue(new Response(JSON.stringify(preview)));
  const http = {
    get: vi.fn(async () => page),
    send,
    onUnauthorized: () => () => {},
  } as unknown as Http;
  const events = { on: vi.fn(() => () => {}), close: vi.fn() } as unknown as EventStream;
  const api = createWebAiQueueApi(http, events);
  const request = {
    selector: { filter: { platform: 'twitter' as const }, exceptKeys: ['x_2'] },
    mode: 'selected' as const,
  };
  await api.estimate(request);
  expect(send).toHaveBeenLastCalledWith('POST', '/api/v1/ai/analyze', {
    selector: { filter: { platform: 'twitter' }, exceptKeys: ['x_2'] },
    mode: 'all',
    deep: false,
    estimateOnly: true,
  });
  send
    .mockRejectedValueOnce({ code: 'network' })
    .mockImplementation(
      async () => new Response(JSON.stringify({ ...preview, queued: true, enqueued: 1 })),
    );
  await expect(api.confirm(request, 'confirm')).rejects.toEqual({ code: 'network' });
  const first = send.mock.calls.at(-1)![3];
  await api.confirm(request, 'confirm');
  expect(send.mock.calls.at(-1)![3]).toEqual(first);
  expect(send.mock.calls.at(-1)![2]).not.toHaveProperty('estimateOnly');
  const capable = { ...OWNER, capabilities: { ...OWNER.capabilities, 'ai.tasks': true } };
  expect(createHttpClient(http, { me: capable, events }).ai?.webQueue).toBeDefined();
  expect(createHttpClient(http, { me: capable, events }).capabilities.aiQueue).toBe(true);
  expect(
    createHttpClient(http, {
      me: { ...capable, capabilities: { ...capable.capabilities, 'ai.tasks': false } },
      events,
    }).ai,
  ).toBeUndefined();
});

it('selects every matching gallery post by filter with an excluded key, without enumerating the library', async () => {
  const h = fixture();
  const client = {
    capabilities: { ...WEB_CAPABILITIES, ai: true, aiQueue: true },
    ai: { webQueue: h.api },
    media: webMedia,
    listPosts: vi.fn(async () => ({
      posts: [toPost(apiPost({ key: 'ig_1' }))],
      total: 200,
      nextCursor: 'next',
    })),
    resolveAllIds: vi.fn(async () => null),
    countPosts: vi.fn(async () => 200),
    on: () => () => {},
    reportError: vi.fn(),
  } as unknown as ShelfyClient;
  ui(
    <ShelfyProvider client={client}>
      <Gallery />
    </ShelfyProvider>,
  );
  const card = await screen.findByTestId('post-card');
  fireEvent.click(screen.getByTestId('select-toggle'));
  fireEvent.click(screen.getByTestId('select-all-matching'));
  await waitFor(() => expect(screen.getByTestId('selection-count')).toHaveTextContent('200'));
  fireEvent.click(card.querySelector('[data-testid="select-checkbox"]')!);
  expect(screen.getByTestId('selection-count')).toHaveTextContent('199');
  fireEvent.click(screen.getByTestId('bulk-actions'));
  fireEvent.click(await screen.findByTestId('bulk-analyze'));
  await waitFor(() =>
    expect(h.api.estimate).toHaveBeenCalledWith(
      expect.objectContaining({
        selector: { filter: expect.any(Object), exceptKeys: ['ig_1'] },
        mode: 'selected',
      }),
    ),
  );
  expect(h.api.confirm).not.toHaveBeenCalled();
});

it('recomputes an expired preview explicitly and requires a fresh confirmation', async () => {
  const h = fixture();
  vi.mocked(h.api.confirm).mockRejectedValueOnce({ code: 'confirm_token_invalid' });
  vi.mocked(h.api.estimate)
    .mockResolvedValueOnce(preview)
    .mockResolvedValueOnce({ ...preview, confirmToken: 'fresh' });
  const queued = vi.fn();
  ui(
    <AnalyzeDialog
      api={h.api}
      request={{ selector: { keys: ['x_1'] }, mode: 'selected' }}
      onClose={vi.fn()}
      onQueued={queued}
    />,
  );
  await screen.findByTestId('analyze-estimate');
  fireEvent.click(screen.getByRole('button', { name: 'Confirm analysis' }));
  const recompute = await screen.findByRole('button', { name: 'Recompute estimate' });
  expect(h.api.estimate).toHaveBeenCalledOnce();
  expect(h.api.confirm).toHaveBeenCalledOnce();
  expect(queued).not.toHaveBeenCalled();
  fireEvent.click(recompute);
  await screen.findByTestId('analyze-estimate');
  expect(h.api.estimate).toHaveBeenCalledTimes(2);
  expect(h.api.confirm).toHaveBeenCalledOnce();
  fireEvent.click(screen.getByRole('button', { name: 'Confirm analysis' }));
  await waitFor(() => expect(queued).toHaveBeenCalledWith(1));
  expect(h.api.confirm).toHaveBeenLastCalledWith(expect.any(Object), 'fresh');
});

it('requests catalog provider setup and refreshes the queue after routing changes', async () => {
  const h = fixture();
  vi.mocked(h.api.get).mockResolvedValue({ ...page, providerState: null });
  const connection = vi.fn();
  window.addEventListener('shelfy:provider-connection', connection);
  const view = ui(<WebAiQueue api={h.api} active canConnect />);
  try {
    fireEvent.click(await screen.findByRole('button', { name: 'Connect provider' }));
    expect(connection).toHaveBeenCalledOnce();
    expect((connection.mock.calls[0][0] as CustomEvent).detail).toEqual({
      task: 'catalog',
      provider: undefined,
    });
    vi.mocked(h.api.get).mockResolvedValue(page);
    act(() => window.dispatchEvent(new CustomEvent('shelfy:provider-settings-changed')));
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Analyze missing' })).toBeEnabled(),
    );
  } finally {
    window.removeEventListener('shelfy:provider-connection', connection);
    view.unmount();
  }
});
